//! A PILHA DE STATUS de uma thread: uma única mensagem em itálico por thread key,
//! editada ao vivo (com throttle) enquanto o cérebro trabalha. No fim do turno a
//! resposta final sai como mensagem nova (reply à última mensagem do usuário) e a
//! mensagem de status é apagada depois de um atraso.

use crate::sender::{is_missing_message, is_not_modified, TelegramSender};
use crate::text::{clip, escape_html, markdown_to_html};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use teloxide::payloads::setters::*;
use teloxide::prelude::Requester;
use will_core::ThreadId;
use will_storage::telegram::{StackEntry, TelegramStatusState};
use will_storage::Storage;

pub const THROTTLE_MS: u64 = 1200;
pub const PREVIEW_MAX: usize = 3500;
pub const ENTRY_MAX: usize = 280;

struct Sched {
    timer_pending: bool,
    last_edit_at: Option<Instant>,
}

pub struct Conversation {
    pub key: String,
    pub chat_id: i64,
    pub topic_id: Option<i64>,
    sender: Arc<TelegramSender>,
    storage: Storage,
    state: Mutex<TelegramStatusState>,
    sched: Mutex<Sched>,
    /// Serializa criação/edição do status (o finalize espera a edição em voo).
    flush_lock: tokio::sync::Mutex<()>,
    /// Invalida flushes agendados antes de um finalize/clear.
    epoch: AtomicU64,
    delete_delay: Duration,
}

/// Monta o corpo em itálico: uma linha por entrada, com emoji; descarta as mais
/// antigas quando estoura `PREVIEW_MAX`.
pub fn render_stack(stack: &[StackEntry]) -> String {
    let mut lines: Vec<String> = stack
        .iter()
        .map(|e| {
            let emoji = match e.role.as_str() {
                "user" => "🧑",
                "assistant" => "🤖",
                _ => "•",
            };
            format!("{emoji} <i>{}</i>", escape_html(&e.text))
        })
        .collect();
    if lines.is_empty() {
        lines.push("🤔 <i>pensando…</i>".to_string());
    }
    let mut body = lines.join("\n");
    while lines.len() > 1 && body.chars().count() > PREVIEW_MAX {
        lines.remove(0);
        body = lines.join("\n");
    }
    if body.chars().count() > PREVIEW_MAX {
        body = body.chars().take(PREVIEW_MAX).collect();
    }
    body
}

impl Conversation {
    pub fn new(
        key: &str,
        chat_id: i64,
        topic_id: Option<i64>,
        sender: Arc<TelegramSender>,
        storage: Storage,
        mut state: TelegramStatusState,
        delete_delay: Duration,
    ) -> Arc<Self> {
        state.thread_key = key.to_string();
        Arc::new(Self {
            key: key.to_string(),
            chat_id,
            topic_id,
            sender,
            storage,
            state: Mutex::new(state),
            sched: Mutex::new(Sched {
                timer_pending: false,
                last_edit_at: None,
            }),
            flush_lock: tokio::sync::Mutex::new(()),
            epoch: AtomicU64::new(0),
            delete_delay,
        })
    }

    pub fn snapshot(&self) -> TelegramStatusState {
        self.state.lock().clone()
    }

    pub fn thread_id(&self) -> Option<ThreadId> {
        self.state.lock().thread_id
    }

    pub async fn set_thread_id(&self, id: Option<ThreadId>) {
        self.state.lock().thread_id = id;
        self.save().await;
    }

    /// Há turno em andamento (algo na pilha ou status na tela)?
    pub fn in_progress(&self) -> bool {
        let s = self.state.lock();
        !s.stack.is_empty() || s.status_message_id.is_some()
    }

    pub fn last_user_message_id(&self) -> Option<i32> {
        self.state.lock().last_user_message_id
    }

    async fn save(&self) {
        let snap = self.snapshot();
        if let Err(e) = self.storage.save_telegram_status(&snap).await {
            tracing::warn!(key = %self.key, "salvar telegram_status: {e}");
        }
    }

    /// Entrou uma mensagem do usuário → pilha (guardando o id para o reply).
    pub async fn push_user(self: &Arc<Self>, text: &str, message_id: i32) {
        {
            let mut s = self.state.lock();
            s.stack.push(StackEntry {
                role: "user".into(),
                text: clip(text, ENTRY_MAX),
                message_id: Some(message_id),
            });
            s.last_user_message_id = Some(message_id);
        }
        self.save().await;
        self.schedule();
    }

    /// O cérebro narrou algo → pilha.
    pub async fn push_assistant(self: &Arc<Self>, text: &str) {
        let t = clip(text, ENTRY_MAX);
        if t.is_empty() {
            return;
        }
        self.state.lock().stack.push(StackEntry {
            role: "assistant".into(),
            text: t,
            message_id: None,
        });
        self.save().await;
        self.schedule();
    }

    /// Agenda a atualização do status respeitando o throttle.
    fn schedule(self: &Arc<Self>) {
        let wait = {
            let mut sc = self.sched.lock();
            if sc.timer_pending {
                return;
            }
            sc.timer_pending = true;
            match sc.last_edit_at {
                Some(t) => Duration::from_millis(THROTTLE_MS).saturating_sub(t.elapsed()),
                None => Duration::ZERO,
            }
        };
        let me = self.clone();
        let epoch = self.epoch.load(Ordering::SeqCst);
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            me.sched.lock().timer_pending = false;
            if me.epoch.load(Ordering::SeqCst) != epoch {
                return; // finalize/clear aconteceu no meio
            }
            me.flush().await;
        });
    }

    /// Garante a mensagem de status e a atualiza. Cria se não existir; recria se sumiu.
    async fn flush(&self) {
        let _g = self.flush_lock.lock().await;
        let (body, status_id) = {
            let s = self.state.lock();
            (render_stack(&s.stack), s.status_message_id)
        };
        self.sched.lock().last_edit_at = Some(Instant::now());

        let Some(status_id) = status_id else {
            self.create_status(&body).await;
            return;
        };

        let bot = self.sender.bot();
        let res = bot
            .edit_message_text(
                teloxide::types::ChatId(self.chat_id),
                teloxide::types::MessageId(status_id),
                body.clone(),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await;
        match res {
            Ok(_) => {}
            Err(e) if is_not_modified(&e) => {}
            Err(e) if is_missing_message(&e) => self.create_status(&body).await,
            Err(e) => tracing::warn!(key = %self.key, "editar status: {e}"),
        }
    }

    async fn create_status(&self, body: &str) {
        match self
            .sender
            .send_one(self.chat_id, self.topic_id, body, true, None, None)
            .await
        {
            Ok(m) => {
                self.state.lock().status_message_id = Some(m.id.0);
                self.save().await;
            }
            Err(e) => tracing::warn!(key = %self.key, "criar status: {e}"),
        }
    }

    /// Fim do turno: resposta final (reply à última mensagem do usuário na pilha) →
    /// agenda remoção do status → limpa a pilha. Devolve o texto limpo para o áudio
    /// (`None` em erro).
    pub async fn finalize(&self, text: &str, is_error: bool, html: bool) -> Option<String> {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        // espera a última edição em voo (evita corrida com o delete)
        let _g = self.flush_lock.lock().await;

        let clean = text.trim();
        let clean = if clean.is_empty() {
            "(sem resposta)"
        } else {
            clean
        };
        let final_text = if is_error {
            format!("⚠️ {clean}")
        } else {
            clean.to_string()
        };

        let (reply_to, status_id) = {
            let s = self.state.lock();
            let last_user = s
                .stack
                .iter()
                .rev()
                .find(|e| e.role == "user")
                .and_then(|e| e.message_id);
            (last_user.or(s.last_user_message_id), s.status_message_id)
        };

        let sent = if html {
            let html_text = markdown_to_html(&final_text);
            self.sender
                .send_html_or_plain(
                    self.chat_id,
                    self.topic_id,
                    &html_text,
                    &final_text,
                    reply_to,
                )
                .await
        } else {
            self.sender
                .send_text(self.chat_id, self.topic_id, &final_text, false, reply_to)
                .await
        };
        if let Err(e) = sent {
            tracing::error!(key = %self.key, "enviar resposta final: {e}");
        }

        if let Some(id) = status_id {
            let sender = self.sender.clone();
            let chat_id = self.chat_id;
            let delay = self.delete_delay;
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                sender.delete(chat_id, id).await;
            });
        }

        {
            let mut s = self.state.lock();
            s.stack.clear();
            s.status_message_id = None;
        }
        self.save().await;

        if is_error {
            None
        } else {
            Some(clean.to_string())
        }
    }

    /// Descarta a pilha e a mensagem de status (/novo e limpeza de boot).
    pub async fn clear(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let _g = self.flush_lock.lock().await;
        let status_id = self.state.lock().status_message_id;
        if let Some(id) = status_id {
            self.sender.delete(self.chat_id, id).await;
        }
        {
            let mut s = self.state.lock();
            s.stack.clear();
            s.status_message_id = None;
        }
        self.save().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: &str, text: &str) -> StackEntry {
        StackEntry {
            role: role.into(),
            text: text.into(),
            message_id: None,
        }
    }

    #[test]
    fn render_empty_is_thinking() {
        assert_eq!(render_stack(&[]), "🤔 <i>pensando…</i>");
    }

    #[test]
    fn render_lines_with_emoji_and_escape() {
        let out = render_stack(&[entry("user", "a <b>"), entry("assistant", "ok & tal")]);
        assert_eq!(out, "🧑 <i>a &lt;b&gt;</i>\n🤖 <i>ok &amp; tal</i>");
    }

    #[test]
    fn render_drops_oldest_when_too_long() {
        let big = "x".repeat(ENTRY_MAX);
        let stack: Vec<StackEntry> = (0..30)
            .map(|i| entry("assistant", &format!("{i}-{big}")))
            .collect();
        let out = render_stack(&stack);
        assert!(out.chars().count() <= PREVIEW_MAX);
        assert!(!out.contains("<i>0-"));
        assert!(out.contains("<i>29-"));
    }

    #[test]
    fn render_single_huge_entry_is_cut() {
        let huge = "y".repeat(PREVIEW_MAX + 100);
        let out = render_stack(&[entry("user", &huge)]);
        assert_eq!(out.chars().count(), PREVIEW_MAX);
    }
}
