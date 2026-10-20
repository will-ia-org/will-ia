//! Envio de mensagens para o Telegram com split em 4096, fallback HTML→texto e
//! allowlist de destinos. Usado pelo módulo e pela ferramenta `telegram_send_message`.

use crate::text::{split_message, TELEGRAM_MAX};
use teloxide::payloads::setters::*;
use teloxide::prelude::Requester;
use teloxide::types::{
    ChatAction, ChatId, InlineKeyboardMarkup, Message, MessageId, ParseMode, ReplyParameters,
    ThreadId,
};
use teloxide::{ApiError, Bot, RequestError};
use will_core::settings::TelegramSettings;

pub struct TelegramSender {
    bot: Bot,
    settings: TelegramSettings,
}

pub(crate) fn topic(topic_id: Option<i64>) -> Option<ThreadId> {
    topic_id.map(|t| ThreadId(MessageId(t as i32)))
}

/// O Telegram não conseguiu interpretar as entidades HTML.
pub(crate) fn is_parse_error(e: &RequestError) -> bool {
    matches!(e, RequestError::Api(ApiError::CantParseEntities(_)))
}

/// A mensagem sumiu (apagada pelo usuário / id inválido): recriar.
pub(crate) fn is_missing_message(e: &RequestError) -> bool {
    matches!(
        e,
        RequestError::Api(
            ApiError::MessageToEditNotFound
                | ApiError::MessageToDeleteNotFound
                | ApiError::MessageCantBeEdited
                | ApiError::MessageIdInvalid
        )
    )
}

pub(crate) fn is_not_modified(e: &RequestError) -> bool {
    matches!(e, RequestError::Api(ApiError::MessageNotModified))
}

impl TelegramSender {
    pub(crate) fn new(bot: Bot, settings: TelegramSettings) -> Self {
        Self { bot, settings }
    }

    pub(crate) fn bot(&self) -> &Bot {
        &self.bot
    }

    /// Chat está na allowlist?
    pub fn allowed_chat(&self, chat_id: i64) -> bool {
        self.settings.allowed_chats.contains(&chat_id)
    }

    /// Usuário está na allowlist?
    pub fn allowed_user(&self, user_id: i64) -> bool {
        self.settings.allowed_users.contains(&user_id)
    }

    /// Destino padrão: `default_chat_id`/`default_topic_id` ou o primeiro chat da allowlist.
    pub fn default_destination(&self) -> Option<(i64, Option<i64>)> {
        if let Some(c) = self.settings.default_chat_id {
            return Some((c, self.settings.default_topic_id));
        }
        self.settings.allowed_chats.first().map(|c| (*c, None))
    }

    /// Envia uma única mensagem (sem split). Erro cru da API.
    pub(crate) async fn send_one(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        text: &str,
        html: bool,
        reply_to: Option<i32>,
        markup: Option<InlineKeyboardMarkup>,
    ) -> Result<Message, RequestError> {
        let mut req = self.bot.send_message(ChatId(chat_id), text);
        if let Some(t) = topic(topic_id) {
            req = req.message_thread_id(t);
        }
        if html {
            req = req.parse_mode(ParseMode::Html);
        }
        if let Some(r) = reply_to {
            req = req
                .reply_parameters(ReplyParameters::new(MessageId(r)).allow_sending_without_reply());
        }
        if let Some(m) = markup {
            req = req.reply_markup(m);
        }
        req.await
    }

    /// Envia em pedaços ≤ 4096 (reply só no primeiro). Erro cru da API.
    pub(crate) async fn send_chunks(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        text: &str,
        html: bool,
        reply_to: Option<i32>,
    ) -> Result<Vec<i32>, RequestError> {
        let text = if text.trim().is_empty() {
            "(vazio)"
        } else {
            text
        };
        let mut ids = Vec::new();
        for (i, chunk) in split_message(text, TELEGRAM_MAX).into_iter().enumerate() {
            let reply = if i == 0 { reply_to } else { None };
            let m = self
                .send_one(chat_id, topic_id, &chunk, html, reply, None)
                .await?;
            ids.push(m.id.0);
        }
        Ok(ids)
    }

    /// Envia texto (split em 4096). Em `html`, se o Telegram recusar o parse, reenvia
    /// o mesmo texto como texto puro. Devolve os ids das mensagens.
    pub async fn send_text(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        text: &str,
        html: bool,
        reply_to: Option<i32>,
    ) -> Result<Vec<i32>, String> {
        match self
            .send_chunks(chat_id, topic_id, text, html, reply_to)
            .await
        {
            Ok(ids) => Ok(ids),
            Err(e) if html && is_parse_error(&e) => {
                tracing::warn!(
                    chat_id,
                    "HTML recusado pelo Telegram, reenviando como texto: {e}"
                );
                self.send_chunks(chat_id, topic_id, text, false, reply_to)
                    .await
                    .map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Tenta a versão HTML; se o parse falhar, manda a versão texto puro.
    pub(crate) async fn send_html_or_plain(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        html_text: &str,
        plain_text: &str,
        reply_to: Option<i32>,
    ) -> Result<Vec<i32>, String> {
        match self
            .send_chunks(chat_id, topic_id, html_text, true, reply_to)
            .await
        {
            Ok(ids) => Ok(ids),
            Err(e) if is_parse_error(&e) => {
                tracing::warn!(
                    chat_id,
                    "HTML da resposta final recusado, enviando texto puro: {e}"
                );
                self.send_chunks(chat_id, topic_id, plain_text, false, reply_to)
                    .await
                    .map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub(crate) async fn chat_action(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        action: ChatAction,
    ) {
        let mut req = self.bot.send_chat_action(ChatId(chat_id), action);
        if let Some(t) = topic(topic_id) {
            req = req.message_thread_id(t);
        }
        if let Err(e) = req.await {
            tracing::debug!(chat_id, "sendChatAction: {e}");
        }
    }

    pub(crate) async fn delete(&self, chat_id: i64, message_id: i32) {
        if let Err(e) = self
            .bot
            .delete_message(ChatId(chat_id), MessageId(message_id))
            .await
        {
            if !is_missing_message(&e) {
                tracing::debug!(chat_id, message_id, "deleteMessage: {e}");
            }
        }
    }
}
