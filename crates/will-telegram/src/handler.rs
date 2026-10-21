//! Roteamento de updates: mensagens (texto, anexos, voice notes), allowlist e dispatch
//! imediato para o cérebro.

use crate::text::{command_name, escape_html, sanitize_filename, thread_key};
use crate::TelegramModule;
use std::sync::Arc;
use std::time::{Duration, Instant};
use teloxide::net::Download;
use teloxide::payloads::setters::*;
use teloxide::prelude::Requester;
use teloxide::types::{
    ChatAction, ChatId, FileId, Message, MessageId, MessageKind, ReactionType, Update, UpdateKind,
};
use will_core::ThreadStatus;

/// Mensagem já decodificada e autorizada.
#[derive(Debug, Clone)]
pub(crate) struct Incoming {
    pub key: String,
    pub chat_id: i64,
    pub topic_id: Option<i64>,
    pub message_id: i32,
    pub user_id: Option<i64>,
    pub chat_label: String,
    pub topic_name: Option<String>,
    pub text: String,
}

impl Incoming {
    /// Título para a thread: nome do tópico, senão rótulo do chat.
    pub fn title(&self) -> String {
        self.topic_name
            .clone()
            .unwrap_or_else(|| self.chat_label.clone())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FileInfo {
    pub file_id: FileId,
    pub name: String,
    pub mime: String,
    pub size: u32,
    pub is_voice: bool,
}

pub(crate) fn is_forum_service(msg: &Message) -> bool {
    matches!(
        msg.kind,
        MessageKind::ForumTopicCreated(_)
            | MessageKind::ForumTopicEdited(_)
            | MessageKind::ForumTopicClosed(_)
            | MessageKind::ForumTopicReopened(_)
            | MessageKind::GeneralForumTopicHidden(_)
            | MessageKind::GeneralForumTopicUnhidden(_)
    )
}

pub(crate) fn extract_file(msg: &Message) -> Option<FileInfo> {
    let id = msg.id.0;
    if let Some(d) = msg.document() {
        return Some(FileInfo {
            file_id: d.file.id.clone(),
            name: d.file_name.clone().unwrap_or_else(|| format!("doc-{id}")),
            mime: d
                .mime_type
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_default(),
            size: d.file.size,
            is_voice: false,
        });
    }
    if let Some(photos) = msg.photo() {
        if let Some(p) = photos.last() {
            return Some(FileInfo {
                file_id: p.file.id.clone(),
                name: format!("foto-{id}.jpg"),
                mime: "image/jpeg".into(),
                size: p.file.size,
                is_voice: false,
            });
        }
    }
    if let Some(v) = msg.voice() {
        return Some(FileInfo {
            file_id: v.file.id.clone(),
            name: format!("audio-{id}.ogg"),
            mime: v
                .mime_type
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "audio/ogg".into()),
            size: v.file.size,
            is_voice: true,
        });
    }
    if let Some(a) = msg.audio() {
        return Some(FileInfo {
            file_id: a.file.id.clone(),
            name: a.file_name.clone().unwrap_or_else(|| format!("audio-{id}")),
            mime: a
                .mime_type
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_default(),
            size: a.file.size,
            is_voice: false,
        });
    }
    if let Some(v) = msg.video() {
        return Some(FileInfo {
            file_id: v.file.id.clone(),
            name: v
                .file_name
                .clone()
                .unwrap_or_else(|| format!("video-{id}.mp4")),
            mime: v
                .mime_type
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "video/mp4".into()),
            size: v.file.size,
            is_voice: false,
        });
    }
    None
}

fn chat_label(msg: &Message) -> String {
    msg.chat
        .title()
        .map(str::to_string)
        .or_else(|| msg.chat.username().map(|u| format!("@{u}")))
        .or_else(|| msg.chat.first_name().map(str::to_string))
        .unwrap_or_else(|| msg.chat.id.0.to_string())
}

impl TelegramModule {
    pub(crate) async fn handle_update(self: &Arc<Self>, update: Update) {
        match update.kind {
            UpdateKind::Message(msg) => self.handle_message(msg).await,
            UpdateKind::CallbackQuery(cq) => self.handle_callback(cq).await,
            _ => {}
        }
    }

    async fn handle_message(self: &Arc<Self>, msg: Message) {
        let chat_id = msg.chat.id.0;
        let topic_id = if msg.is_topic_message {
            msg.thread_id.map(|t| i64::from(t.0 .0))
        } else {
            None
        };
        let key = thread_key(chat_id, topic_id);

        if is_forum_service(&msg) {
            if let (Some(created), Some(tid)) = (msg.forum_topic_created(), msg.thread_id) {
                let k = thread_key(chat_id, Some(i64::from(tid.0 .0)));
                self.topic_names.lock().insert(k, created.name.clone());
            }
            return;
        }

        let text = msg
            .text()
            .or_else(|| msg.caption())
            .unwrap_or("")
            .to_string();
        let file = extract_file(&msg);
        if text.trim().is_empty() && file.is_none() {
            return;
        }

        let user_id = msg.from.as_ref().map(|u| u.id.0 as i64);
        let user_label = msg
            .from
            .as_ref()
            .map(|u| {
                u.username
                    .clone()
                    .map(|n| format!("@{n}"))
                    .unwrap_or_else(|| u.first_name.clone())
            })
            .unwrap_or_else(|| "?".into());
        let topic_name = msg
            .reply_to_message()
            .and_then(|r| r.forum_topic_created())
            .map(|t| t.name.clone())
            .or_else(|| self.topic_names.lock().get(&key).cloned());
        if let Some(n) = &topic_name {
            self.topic_names.lock().insert(key.clone(), n.clone());
        }

        let inc = Incoming {
            key: key.clone(),
            chat_id,
            topic_id,
            message_id: msg.id.0,
            user_id,
            chat_label: chat_label(&msg),
            topic_name,
            text: text.clone(),
        };

        let allowed = self.sender.allowed_chat(chat_id)
            && user_id
                .map(|u| self.sender.allowed_user(u))
                .unwrap_or(false);
        if !allowed {
            tracing::info!(
                chat_id,
                ?user_id,
                "ignorado (fora da allowlist): {user_label}"
            );
            self.bus.log(
                "warn",
                "telegram",
                format!(
                    "ignorado (fora da allowlist): chat {chat_id}, user {user_id:?} ({user_label})"
                ),
            );
            if command_name(&text) == "start" {
                self.cmd_start(&inc, false).await;
            } else if self.settings.unauthorized_behavior == "reply" {
                self.unauthorized_notice(&inc).await;
            }
            return;
        }

        if let Some(file) = file {
            let me = self.clone();
            tokio::spawn(async move { me.handle_attachment(inc, file).await });
            return;
        }

        let mut text = text;
        if text.starts_with('/') {
            let cmd = command_name(&text);
            if self.handle_builtin(&inc, &cmd).await {
                return;
            }
            // comando da KB digitado com underscore → traduz para hífen
            let real = self.repo_alias.lock().get(&cmd).cloned();
            if let Some(real) = real {
                let first_len = text
                    .split_whitespace()
                    .next()
                    .map(|s| s.len())
                    .unwrap_or(text.len());
                text = format!("/{real}{}", &text[first_len..]);
            }
        }

        tracing::info!(key = %key, "[{user_label}] {}", crate::text::clip(&text, 120));
        self.dispatch(&inc, &text).await;
    }

    /// Aviso curto a não autorizados (no máximo uma vez por chat por hora).
    async fn unauthorized_notice(&self, inc: &Incoming) {
        let now = Instant::now();
        let should = {
            let mut m = self.unauthorized_notice.lock();
            match m.get(&inc.chat_id) {
                Some(t) if now.duration_since(*t) < Duration::from_secs(3600) => false,
                _ => {
                    m.insert(inc.chat_id, now);
                    true
                }
            }
        };
        if should {
            self.reply(
                inc,
                "⛔ Você não está autorizado a usar este bot. Mande /start para ver seus IDs.",
                false,
            )
            .await;
        }
    }

    pub(crate) async fn reply(&self, inc: &Incoming, text: &str, html: bool) {
        if let Err(e) = self
            .sender
            .send_text(inc.chat_id, inc.topic_id, text, html, None)
            .await
        {
            tracing::warn!(key = %inc.key, "responder: {e}");
        }
    }

    /// Empurra 🧑 na pilha, `typing`, e escreve NA HORA no cérebro (mesmo mid-turn).
    pub(crate) async fn dispatch(&self, inc: &Incoming, text: &str) {
        self.dispatch_with(inc, text, &[]).await
    }

    /// [`dispatch`](Self::dispatch) com os anexos que acompanham a mensagem.
    pub(crate) async fn dispatch_with(
        &self,
        inc: &Incoming,
        text: &str,
        attachments: &[will_core::Attachment],
    ) {
        let conv = self.conv(&inc.key, inc.chat_id, inc.topic_id).await;
        let mut busy = conv.in_progress();
        conv.push_user(text, inc.message_id).await;
        self.sender
            .chat_action(inc.chat_id, inc.topic_id, ChatAction::Typing)
            .await;

        let title = inc.title();
        let tid = match self
            .thread_for(&inc.key, inc.chat_id, inc.topic_id, Some(&title))
            .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(key = %inc.key, "ensure_thread: {e}");
                conv.finalize(&format!("não consegui abrir a thread: {e}"), true, false)
                    .await;
                return;
            }
        };
        if !busy {
            if let Ok(t) = self.host.get_thread(tid).await {
                busy = t.pending > 0 || t.status == ThreadStatus::Running;
            }
        }

        let mut result = self.host.send_message_with(tid, text, attachments).await;
        if result.is_err() {
            // a thread pode ter sido apagada pela UI: refaz o vínculo uma vez
            self.unbind_key(&inc.key);
            if let Ok(t2) = self
                .thread_for(&inc.key, inc.chat_id, inc.topic_id, Some(&title))
                .await
            {
                result = self.host.send_message_with(t2, text, attachments).await;
            }
        }
        if let Err(e) = result {
            tracing::error!(key = %inc.key, "send_message: {e}");
            self.bus.log(
                "error",
                "telegram",
                format!("[{}] enviar ao cérebro: {e}", inc.key),
            );
            conv.finalize(
                &format!("não consegui falar com o cérebro: {e}"),
                true,
                false,
            )
            .await;
            return;
        }

        if busy {
            let res = self
                .bot
                .set_message_reaction(ChatId(inc.chat_id), MessageId(inc.message_id))
                .reaction(vec![ReactionType::Emoji {
                    emoji: "👀".into()
                }])
                .await;
            if let Err(e) = res {
                tracing::debug!(key = %inc.key, "reação 👀: {e}");
            }
        }
    }

    // ---------- anexos ----------

    async fn download(
        &self,
        file: &FileInfo,
        inc: &Incoming,
    ) -> Result<std::path::PathBuf, String> {
        let dir = self.kb_path.join(&self.settings.attachments_dir);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("criar pasta de anexos: {e}"))?;
        let mut safe = sanitize_filename(&file.name);
        if safe.is_empty() {
            safe = format!("arq-{}", inc.message_id);
        }
        let dest = dir.join(format!("{}-{}-{}", inc.chat_id, inc.message_id, safe));
        let f = self
            .bot
            .get_file(file.file_id.clone())
            .await
            .map_err(|e| e.to_string())?;
        let mut out = tokio::fs::File::create(&dest)
            .await
            .map_err(|e| e.to_string())?;
        self.bot
            .download_file(&f.path, &mut out)
            .await
            .map_err(|e| e.to_string())?;
        Ok(dest)
    }

    async fn handle_attachment(self: Arc<Self>, inc: Incoming, file: FileInfo) {
        let kind = if file.is_voice { "voice note" } else { "anexo" };
        tracing::info!(key = %inc.key, "{kind} {} ({}, {}B)", file.name, file.mime, file.size);
        self.sender
            .chat_action(inc.chat_id, inc.topic_id, ChatAction::Typing)
            .await;

        let dest = match self.download(&file, &inc).await {
            Ok(d) => d,
            Err(e) => {
                self.reply(
                    &inc,
                    &format!("⚠️ não consegui baixar o arquivo: {e}"),
                    false,
                )
                .await;
                return;
            }
        };
        let caption = inc.text.trim().to_string();

        if file.is_voice {
            if self.settings.transcribe_voice_notes {
                if let Some(openai) = &self.openai {
                    let bytes = match tokio::fs::read(&dest).await {
                        Ok(b) => b,
                        Err(e) => {
                            self.reply(&inc, &format!("⚠️ não consegui ler o áudio: {e}"), false)
                                .await;
                            return;
                        }
                    };
                    let name = dest
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("voz.ogg")
                        .to_string();
                    match openai.transcribe(bytes, &name, &file.mime).await {
                        Ok(t) if t.is_empty() => {
                            self.reply(&inc, "⚠️ não entendi nada no áudio. Tenta de novo?", false)
                                .await;
                        }
                        Ok(t) => {
                            self.reply(&inc, &format!("🎙️ <i>{}</i>", escape_html(&t)), true)
                                .await;
                            let text = if caption.is_empty() {
                                t
                            } else {
                                format!("{caption}\n\n{t}")
                            };
                            self.dispatch(&inc, &text).await;
                        }
                        Err(e) => {
                            tracing::warn!(key = %inc.key, "transcrição: {e}");
                            self.reply(
                                &inc,
                                &format!("⚠️ não consegui transcrever o áudio: {e}"),
                                false,
                            )
                            .await;
                        }
                    }
                    return;
                }
            }
            self.reply(
                &inc,
                "ℹ️ A transcrição de áudio está desligada (sem chave da OpenAI ou desabilitada). Encaminhei o arquivo ao cérebro.",
                false,
            )
            .await;
        }

        let att = will_core::Attachment {
            name: file.name.clone(),
            mime: file.mime.clone(),
            path: dest.display().to_string(),
            size: u64::from(file.size),
        };
        let mut prompt = String::new();
        if !caption.is_empty() {
            prompt.push_str(&caption);
            prompt.push_str("\n\n");
        }
        prompt.push_str(&will_core::attachment_note(&att));
        self.dispatch_with(&inc, &prompt, &[att]).await;
    }
}
