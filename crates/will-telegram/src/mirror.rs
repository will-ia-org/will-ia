//! Espelho app ↔ tópicos do fórum.
//!
//! Thread criada no app (ou pelo MCP) vira um tópico no supergrupo
//! `forum_chat_id`; renomear/apagar a thread edita/apaga o tópico. O sentido
//! inverso (tópico → thread) já existe no `handler`. Tudo é best-effort: se o
//! fórum não está configurado/validado, a thread vive só no app.

use crate::text::thread_key;
use crate::TelegramModule;
use teloxide::payloads::setters::*;
use teloxide::prelude::Requester;
use teloxide::types::{
    ChatFullInfoKind, ChatFullInfoPublicKind, ChatId, ChatMemberKind, MessageId,
    ThreadId as TgThreadId, UserId,
};
use will_core::{ModuleStatus, Origin};

/// Nome do tópico enquanto a thread do app ainda não tem assunto (o título
/// chega com a primeira mensagem e o tópico é renomeado junto).
pub const PLACEHOLDER_TOPIC: &str = "Nova thread";

/// Resultado da validação do fórum, feita na subida do módulo.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ForumState {
    /// `mirror_threads` ligado nas configurações.
    pub enabled: bool,
    pub chat_id: Option<i64>,
    pub chat_title: Option<String>,
    /// Fórum pronto: supergrupo com Tópicos e bot com "Gerenciar tópicos".
    pub ok: bool,
    pub error: Option<String>,
}

/// Nome de tópico válido (1–128 caracteres, sem quebras).
fn topic_name(title: &str) -> String {
    let t: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(128).collect();
    if t.is_empty() {
        PLACEHOLDER_TOPIC.to_string()
    } else {
        t
    }
}

fn tg_thread(topic_id: i64) -> TgThreadId {
    TgThreadId(MessageId(topic_id as i32))
}

impl TelegramModule {
    /// Valida o fórum do espelho (`getChat` + `getChatMember` do próprio bot).
    pub(crate) async fn check_forum(&self, bot_id: UserId) -> ForumState {
        let mut st = ForumState {
            enabled: self.settings.mirror_threads,
            chat_id: self.settings.forum_chat_id,
            ..Default::default()
        };
        if !st.enabled {
            return st;
        }
        let Some(chat_id) = st.chat_id else {
            st.error = Some("fórum não configurado (chat ID do supergrupo com Tópicos)".into());
            return st;
        };
        if !self.settings.allowed_chats.contains(&chat_id) {
            st.error = Some("o fórum precisa estar em Chats permitidos".into());
            return st;
        }
        match self.bot.get_chat(ChatId(chat_id)).await {
            Ok(c) => {
                st.chat_title = c.title().map(String::from);
                let is_forum = match &c.kind {
                    ChatFullInfoKind::Public(p) => {
                        matches!(&p.kind, ChatFullInfoPublicKind::Supergroup(s) if s.is_forum)
                    }
                    _ => false,
                };
                if !is_forum {
                    st.error = Some("o chat não é um supergrupo com Tópicos ligados".into());
                    return st;
                }
            }
            Err(e) => {
                st.error = Some(format!("getChat falhou: {e}"));
                return st;
            }
        }
        match self.bot.get_chat_member(ChatId(chat_id), bot_id).await {
            Ok(m) => match m.kind {
                ChatMemberKind::Owner(_) => {}
                ChatMemberKind::Administrator(a) if a.can_manage_topics => {}
                ChatMemberKind::Administrator(_) => {
                    st.error = Some(
                        "o bot é administrador, mas sem a permissão \"Gerenciar tópicos\"".into(),
                    );
                    return st;
                }
                _ => {
                    st.error = Some("o bot não é administrador do fórum".into());
                    return st;
                }
            },
            Err(e) => {
                st.error = Some(format!("getChatMember falhou: {e}"));
                return st;
            }
        }
        st.ok = true;
        st
    }

    /// `telegram.mirror_threads` da configuração com que o módulo subiu.
    pub fn settings_mirror_threads(&self) -> bool {
        self.settings.mirror_threads
    }

    pub fn forum_state(&self) -> ForumState {
        self.forum.lock().clone()
    }

    /// Espelho ligado e módulo no ar (renomear/apagar valem para qualquer
    /// tópico que o bot alcance, não só o fórum configurado).
    fn mirror_on(&self) -> bool {
        self.settings.enabled
            && self.settings.mirror_threads
            && matches!(self.status(), ModuleStatus::Running)
    }

    /// Dá para criar tópicos: espelho ligado e fórum validado.
    pub fn mirror_ready(&self) -> bool {
        self.mirror_on() && self.forum.lock().ok
    }

    /// Cria o tópico no fórum para uma thread nova e devolve a origem a gravar
    /// na thread (`Telegram { forum, tópico }`).
    pub async fn mirror_create(&self, title: Option<&str>) -> Result<Origin, String> {
        let st = self.forum_state();
        if !self.mirror_on() {
            return Err("espelho desligado ou módulo parado".into());
        }
        if !st.ok {
            return Err(st.error.unwrap_or_else(|| "fórum não validado".into()));
        }
        let chat_id = st.chat_id.ok_or("fórum não configurado")?;
        let name = topic_name(title.unwrap_or(PLACEHOLDER_TOPIC));
        let topic = self
            .bot
            .create_forum_topic(ChatId(chat_id), name.clone())
            .await
            .map_err(|e| format!("createForumTopic: {e}"))?;
        let topic_id = i64::from(topic.thread_id.0 .0);
        let key = thread_key(chat_id, Some(topic_id));
        self.topic_names.lock().insert(key, name.clone());
        tracing::info!(chat_id, topic_id, name = %name, "tópico criado no fórum (espelho do app)");
        self.bus.log(
            "info",
            "telegram",
            format!("[{chat_id}:{topic_id}] tópico \"{name}\" criado (espelho do app)"),
        );
        Ok(Origin::Telegram {
            chat_id,
            topic_id: Some(topic_id),
        })
    }

    /// Renomeia o tópico da thread. Com `only_placeholder`, só mexe se o tópico
    /// ainda se chama "Nova thread" (título automático da primeira mensagem não
    /// deve sobrescrever o nome que o usuário deu ao tópico no Telegram).
    pub async fn mirror_rename(
        &self,
        origin: &Origin,
        title: &str,
        only_placeholder: bool,
    ) -> Result<(), String> {
        if !self.mirror_on() {
            return Ok(());
        }
        let Origin::Telegram {
            chat_id,
            topic_id: Some(topic_id),
        } = origin
        else {
            return Ok(());
        };
        let key = thread_key(*chat_id, Some(*topic_id));
        let name = topic_name(title);
        let current = self.topic_names.lock().get(&key).cloned();
        if current.as_deref() == Some(name.as_str()) {
            return Ok(());
        }
        if only_placeholder && current.as_deref() != Some(PLACEHOLDER_TOPIC) {
            return Ok(());
        }
        self.bot
            .edit_forum_topic(ChatId(*chat_id), tg_thread(*topic_id))
            .name(name.clone())
            .await
            .map_err(|e| format!("editForumTopic: {e}"))?;
        self.topic_names.lock().insert(key, name.clone());
        tracing::info!(chat_id, topic_id, name = %name, "tópico renomeado (espelho do app)");
        Ok(())
    }

    /// Apaga o tópico da thread no Telegram e esquece o vínculo no bot.
    pub async fn mirror_delete(&self, origin: &Origin) -> Result<(), String> {
        if !self.mirror_on() {
            return Ok(());
        }
        let Origin::Telegram {
            chat_id,
            topic_id: Some(topic_id),
        } = origin
        else {
            return Ok(());
        };
        let key = thread_key(*chat_id, Some(*topic_id));
        self.forget_key(&key).await;
        self.topic_names.lock().remove(&key);
        self.bot
            .delete_forum_topic(ChatId(*chat_id), tg_thread(*topic_id))
            .await
            .map_err(|e| format!("deleteForumTopic: {e}"))?;
        tracing::info!(chat_id, topic_id, "tópico apagado (espelho do app)");
        self.bus.log(
            "info",
            "telegram",
            format!("[{key}] tópico apagado (espelho do app)"),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_name_limits() {
        assert_eq!(topic_name("  a   b\nc "), "a b c");
        assert_eq!(topic_name("   "), PLACEHOLDER_TOPIC);
        assert_eq!(topic_name(&"x".repeat(200)).chars().count(), 128);
    }
}
