//! Eventos do cérebro → Telegram: narração na pilha, finalize no fim do turno,
//! sessão expirada, aprovações com botões.

use crate::conversation::Conversation;
use crate::text::{escape_html, parse_thread_key, thread_key};
use crate::{ApprovalMsg, TelegramModule};
use std::sync::Arc;
use teloxide::payloads::setters::*;
use teloxide::prelude::Requester;
use teloxide::types::{ChatAction, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use will_core::{ApprovalDecision, ApprovalRequest, EngineDelta, Event, Origin, ThreadId};

impl TelegramModule {
    pub(crate) async fn event_loop(self: Arc<Self>, cancel: CancellationToken) {
        let mut rx = self.host.subscribe();
        loop {
            let ev = tokio::select! {
                _ = cancel.cancelled() => break,
                r = rx.recv() => r,
            };
            match ev {
                Ok(ev) => self.on_event(ev).await,
                Err(RecvError::Lagged(n)) => tracing::warn!("eventos perdidos (lag): {n}"),
                Err(RecvError::Closed) => break,
            }
        }
        tracing::info!("loop de eventos encerrado");
    }

    async fn bound_conv(&self, thread_id: ThreadId) -> Option<Arc<Conversation>> {
        let key = self.key_of(thread_id)?;
        self.conv_by_key(&key).await
    }

    async fn on_event(self: &Arc<Self>, ev: Event) {
        match ev {
            Event::ThreadCreated { thread } | Event::ThreadUpdated { thread } => {
                if let Origin::Telegram { chat_id, topic_id } = thread.origin {
                    let key = thread_key(chat_id, topic_id);
                    if self.thread_of(&key) != Some(thread.id) {
                        self.bind(&key, thread.id);
                    }
                }
            }
            Event::ThreadDeleted { thread_id } => {
                if let Some(key) = self.unbind_thread(thread_id) {
                    self.convs.lock().remove(&key);
                }
            }
            Event::TurnDelta {
                thread_id,
                delta: EngineDelta::Narration { text },
                ..
            } => {
                if let Some(conv) = self.bound_conv(thread_id).await {
                    conv.push_assistant(&text).await;
                }
            }
            Event::TurnFinished {
                thread_id, result, ..
            } => {
                if let Some(conv) = self.bound_conv(thread_id).await {
                    let me = self.clone();
                    tokio::spawn(
                        async move { me.finalize(conv, result.text, result.is_error).await },
                    );
                }
            }
            Event::TurnFailed {
                thread_id, error, ..
            } => {
                if let Some(conv) = self.bound_conv(thread_id).await {
                    let me = self.clone();
                    tokio::spawn(async move { me.finalize(conv, error, true).await });
                }
            }
            Event::StaleSession { thread_id } => {
                if let Some(conv) = self.bound_conv(thread_id).await {
                    conv.push_assistant("♻️ a sessão antiga deste tópico expirou — comecei uma nova e reenviei sua mensagem").await;
                }
            }
            Event::ApprovalRequested { approval } => self.on_approval_requested(approval).await,
            Event::ApprovalDecided {
                approval_id,
                decision,
            } => {
                let entry = self.approval_msgs.lock().remove(&approval_id);
                if let Some(m) = entry {
                    let suffix = match decision {
                        ApprovalDecision::Approved => "✅ <b>Aprovado</b>",
                        ApprovalDecision::Denied => "❌ <b>Negado</b>",
                        ApprovalDecision::Timeout => "⏰ <b>Expirou</b> sem decisão",
                    };
                    self.mark_approval_message(m.chat_id, m.message_id, Some(&m.text), suffix)
                        .await;
                }
            }
            _ => {}
        }
    }

    async fn finalize(&self, conv: Arc<Conversation>, text: String, is_error: bool) {
        let html = self.settings.reply_format == "html";
        let clean = conv.finalize(&text, is_error, html).await;
        if let Some(clean) = clean {
            self.maybe_send_audio(conv, clean).await;
        }
    }

    /// Resumo em áudio (best-effort).
    async fn maybe_send_audio(&self, conv: Arc<Conversation>, clean: String) {
        if !self.settings.audio_summary {
            return;
        }
        let Some(openai) = self.openai.clone() else {
            return;
        };
        if clean.chars().count() < self.settings.audio_min_chars {
            return;
        }
        self.sender
            .chat_action(conv.chat_id, conv.topic_id, ChatAction::RecordVoice)
            .await;
        match openai
            .voice_summary(
                &clean,
                &self.settings.audio_voice,
                self.settings.audio_speed,
            )
            .await
        {
            Ok(Some(ogg)) => {
                let mut req = self.bot.send_voice(
                    ChatId(conv.chat_id),
                    InputFile::memory(ogg).file_name("voz.ogg"),
                );
                if let Some(t) = crate::sender::topic(conv.topic_id) {
                    req = req.message_thread_id(t);
                }
                if let Err(e) = req.await {
                    tracing::warn!(key = %conv.key, "sendVoice: {e}");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(key = %conv.key, "áudio: {e}"),
        }
    }

    async fn on_approval_requested(&self, approval: ApprovalRequest) {
        let dest = approval
            .thread_id
            .and_then(|t| self.key_of(t))
            .and_then(|k| parse_thread_key(&k))
            .or_else(|| self.sender.default_destination());
        let Some((chat_id, topic_id)) = dest else {
            return;
        };
        let text = format!(
            "🔐 <b>Aprovação</b>\n{}\n<i>ferramenta: {}</i>",
            escape_html(&approval.summary),
            escape_html(&approval.tool)
        );
        let keyboard = InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback("✅ Aprovar", format!("ap:{}:y", approval.id)),
            InlineKeyboardButton::callback("❌ Negar", format!("ap:{}:n", approval.id)),
        ]]);
        match self
            .sender
            .send_one(chat_id, topic_id, &text, true, None, Some(keyboard))
            .await
        {
            Ok(m) => {
                self.approval_msgs.lock().insert(
                    approval.id,
                    ApprovalMsg {
                        chat_id,
                        message_id: m.id.0,
                        text,
                    },
                );
            }
            Err(e) => tracing::warn!("enviar pedido de aprovação: {e}"),
        }
    }
}
