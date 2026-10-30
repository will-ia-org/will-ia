//! Ferramenta MCP `telegram_send_message`.

use crate::sender::TelegramSender;
use std::sync::Arc;
use will_core::tools::truncate as truncate_chars;
use will_core::tools::{arg_opt_i64, arg_opt_str, arg_str};
use will_core::{
    Module, Origin, ToolContext, ToolError, ToolOutput, ToolPolicy, ToolSpec, WillTool,
};

pub struct SendMessageTool {
    pub(crate) sender: Arc<TelegramSender>,
}

impl SendMessageTool {
    /// Resolve o destino: argumentos → origem da thread → destino padrão.
    fn destination(
        &self,
        ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<(i64, Option<i64>), ToolError> {
        let arg_chat = arg_opt_i64(args, "chat_id");
        let arg_topic = arg_opt_i64(args, "topic_id");
        if let Some(c) = arg_chat {
            return Ok((c, arg_topic));
        }
        if let Some(Origin::Telegram { chat_id, topic_id }) = &ctx.origin {
            return Ok((*chat_id, arg_topic.or(*topic_id)));
        }
        match self.sender.default_destination() {
            Some((c, t)) => Ok((c, arg_topic.or(t))),
            None => Err(ToolError::Unavailable(
                "nenhum chat do Telegram configurado na allowlist".into(),
            )),
        }
    }
}

#[async_trait::async_trait]
impl WillTool for SendMessageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "telegram_send_message".into(),
            description: "Envia uma mensagem pelo bot do Telegram. Sem chat_id/topic_id, vai para o tópico de origem da thread (ou o destino padrão configurado). Só chats da allowlist são permitidos.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Texto da mensagem (texto puro; HTML básico se parse_mode=html)." },
                    "chat_id": { "type": "string", "description": "Opcional. Chat de destino. Padrão: chat de origem da thread, ou o chat padrão configurado." },
                    "topic_id": { "type": "integer", "description": "Opcional. Tópico do fórum. Padrão: tópico de origem da thread." },
                    "parse_mode": { "type": "string", "enum": ["text", "html"], "default": "text" },
                    "reply_to_message_id": { "type": "integer" }
                },
                "required": ["text"]
            }),
            requires: vec![Module::Telegram],
            default_policy: ToolPolicy::Confirm,
            family: "telegram".into(),
        }
    }

    fn approval_summary(&self, args: &serde_json::Value) -> String {
        let text = arg_opt_str(args, "text").unwrap_or("");
        let dest = match (arg_opt_i64(args, "chat_id"), arg_opt_i64(args, "topic_id")) {
            (Some(c), Some(t)) => format!("chat {c} / tópico {t}"),
            (Some(c), None) => format!("chat {c}"),
            (None, Some(t)) => format!("tópico {t} (chat de origem)"),
            (None, None) => "o tópico de origem (ou destino padrão)".to_string(),
        };
        format!(
            "Enviar no Telegram para {dest}: {}",
            truncate_chars(text, 200)
        )
    }

    async fn call(
        &self,
        ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let text = arg_str(&args, "text")?;
        let html = matches!(
            arg_opt_str(&args, "parse_mode"),
            Some("html") | Some("HTML")
        );
        let reply_to = arg_opt_i64(&args, "reply_to_message_id").map(|v| v as i32);
        let (chat_id, topic_id) = self.destination(&ctx, &args)?;
        if !self.sender.allowed_chat(chat_id) {
            tracing::warn!(
                channel = "telegram",
                chat_id,
                thread_id = ?ctx.thread_id,
                "telegram_send_message recusado: chat fora da allowlist"
            );
            return Err(ToolError::Forbidden(format!(
                "chat {chat_id} não está na allowlist do Telegram"
            )));
        }
        tracing::debug!(
            channel = "telegram",
            chat_id,
            ?topic_id,
            thread_id = ?ctx.thread_id,
            text_len = text.chars().count(),
            html,
            "telegram_send_message"
        );
        let ids = self
            .sender
            .send_text(chat_id, topic_id, ctx.thread_id, text, html, reply_to)
            .await
            .map_err(ToolError::Failed)?;
        Ok(ToolOutput::json(&serde_json::json!({
            "message_ids": ids,
            "chat_id": chat_id,
            "topic_id": topic_id,
        })))
    }
}
