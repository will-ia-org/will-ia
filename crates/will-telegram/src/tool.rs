//! Ferramentas MCP do Telegram: `telegram_send_message` (texto),
//! `telegram_send_file` (foto/vídeo/áudio/animação/documento) e
//! `telegram_send_voice` (mensagem de voz por arquivo ou por TTS).

use crate::media::{check_caption, check_size, mime_for_path, resolve_kb_path, FileKind};
use crate::sender::TelegramSender;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use will_core::tools::truncate as truncate_chars;
use will_core::tools::{arg_opt_i64, arg_opt_str, arg_str};
use will_core::{
    Module, Origin, ToolContext, ToolError, ToolOutput, ToolPolicy, ToolSpec, WillTool,
};
use will_realtime::openai::OpenAi;

/// Resolve o destino: argumentos → origem da thread → destino padrão.
fn destination(
    sender: &TelegramSender,
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
    match sender.default_destination() {
        Some((c, t)) => Ok((c, arg_topic.or(t))),
        None => Err(ToolError::Unavailable(
            "nenhum chat do Telegram configurado na allowlist".into(),
        )),
    }
}

/// Destino resolvido **e** dentro da allowlist (senão `Forbidden`).
fn allowed_destination(
    sender: &TelegramSender,
    ctx: &ToolContext,
    args: &serde_json::Value,
    tool: &str,
) -> Result<(i64, Option<i64>), ToolError> {
    let (chat_id, topic_id) = destination(sender, ctx, args)?;
    if !sender.allowed_chat(chat_id) {
        tracing::warn!(
            channel = "telegram",
            chat_id,
            thread_id = ?ctx.thread_id,
            "{tool} recusado: chat fora da allowlist"
        );
        return Err(ToolError::Forbidden(format!(
            "chat {chat_id} não está na allowlist do Telegram"
        )));
    }
    Ok((chat_id, topic_id))
}

/// Texto legível do destino para o resumo de aprovação.
fn describe_destination(args: &serde_json::Value) -> String {
    match (arg_opt_i64(args, "chat_id"), arg_opt_i64(args, "topic_id")) {
        (Some(c), Some(t)) => format!("chat {c} / tópico {t}"),
        (Some(c), None) => format!("chat {c}"),
        (None, Some(t)) => format!("tópico {t} (chat de origem)"),
        (None, None) => "o tópico de origem (ou destino padrão)".to_string(),
    }
}

fn wants_html(args: &serde_json::Value) -> bool {
    matches!(arg_opt_str(args, "parse_mode"), Some("html") | Some("HTML"))
}

fn reply_to(args: &serde_json::Value) -> Option<i32> {
    arg_opt_i64(args, "reply_to_message_id").map(|v| v as i32)
}

fn destination_props() -> serde_json::Value {
    serde_json::json!({
        "chat_id": { "type": "string", "description": "Opcional. Chat de destino. Padrão: chat de origem da thread, ou o chat padrão configurado." },
        "topic_id": { "type": "integer", "description": "Opcional. Tópico do fórum. Padrão: tópico de origem da thread." },
        "reply_to_message_id": { "type": "integer", "description": "Opcional. Responde a esta mensagem." }
    })
}

fn merge(mut base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    base
}

// ---------------------------------------------------------------------------
// telegram_send_message
// ---------------------------------------------------------------------------

pub struct SendMessageTool {
    pub(crate) sender: Arc<TelegramSender>,
}

#[async_trait::async_trait]
impl WillTool for SendMessageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "telegram_send_message".into(),
            description: "Envia uma mensagem de TEXTO pelo bot do Telegram. Sem chat_id/topic_id, vai para o tópico de origem da thread (ou o destino padrão configurado). Só chats da allowlist são permitidos. Para arquivos use telegram_send_file; para voz, telegram_send_voice.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": merge(serde_json::json!({
                    "text": { "type": "string", "description": "Texto da mensagem (texto puro; HTML básico se parse_mode=html)." },
                    "parse_mode": { "type": "string", "enum": ["text", "html"], "default": "text" }
                }), destination_props()),
                "required": ["text"]
            }),
            requires: vec![Module::Telegram],
            default_policy: ToolPolicy::Confirm,
            family: "telegram".into(),
        }
    }

    fn approval_summary(&self, args: &serde_json::Value) -> String {
        let text = arg_opt_str(args, "text").unwrap_or("");
        format!(
            "Enviar no Telegram para {}: {}",
            describe_destination(args),
            truncate_chars(text, 200)
        )
    }

    async fn call(
        &self,
        ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let text = arg_str(&args, "text")?;
        let html = wants_html(&args);
        let reply_to = reply_to(&args);
        let (chat_id, topic_id) =
            allowed_destination(&self.sender, &ctx, &args, "telegram_send_message")?;
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

// ---------------------------------------------------------------------------
// telegram_send_file
// ---------------------------------------------------------------------------

pub struct SendFileTool {
    pub(crate) sender: Arc<TelegramSender>,
    /// Raiz da base de conhecimento: só arquivos dentro dela podem ser enviados.
    pub(crate) kb_path: PathBuf,
}

#[async_trait::async_trait]
impl WillTool for SendFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "telegram_send_file".into(),
            description: "Envia um ARQUIVO pelo bot do Telegram (foto, vídeo, áudio, GIF/animação ou documento como PDF/CSV/ZIP) que esteja dentro da base de conhecimento, com legenda opcional. `kind` auto escolhe pelo tipo do arquivo (imagem → foto; > 10 MB → documento). Limite do Bot API: 50 MB. Sem chat_id/topic_id, vai para o tópico de origem da thread (ou o destino padrão). Exige aprovação.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": merge(serde_json::json!({
                    "path": { "type": "string", "description": "Caminho do arquivo dentro da KB (absoluto ou relativo à KB)." },
                    "caption": { "type": "string", "description": "Legenda (opcional, até 1024 caracteres; HTML básico se parse_mode=html)." },
                    "kind": { "type": "string", "enum": ["auto", "photo", "video", "audio", "animation", "document"], "default": "auto", "description": "Como o Telegram deve tratar o arquivo. `document` preserva o arquivo original (sem recompressão)." },
                    "parse_mode": { "type": "string", "enum": ["text", "html"], "default": "text", "description": "Formato da legenda." },
                    "duration_secs": { "type": "integer", "minimum": 1, "description": "Duração (vídeo/áudio), opcional; melhora a exibição." }
                }), destination_props()),
                "required": ["path"]
            }),
            requires: vec![Module::Telegram],
            default_policy: ToolPolicy::Confirm,
            family: "telegram".into(),
        }
    }

    fn approval_summary(&self, args: &serde_json::Value) -> String {
        let path = arg_opt_str(args, "path").unwrap_or("?");
        let caption = arg_opt_str(args, "caption")
            .filter(|c| !c.trim().is_empty())
            .map(|c| format!(" — {}", truncate_chars(c, 120)))
            .unwrap_or_default();
        format!(
            "Enviar arquivo {path} no Telegram para {}{caption}",
            describe_destination(args)
        )
    }

    async fn call(
        &self,
        ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let raw_path = arg_str(&args, "path")?;
        let path = resolve_kb_path(&self.kb_path, raw_path)?;
        let size = std::fs::metadata(&path)
            .map_err(|e| ToolError::Failed(format!("ler {}: {e}", path.display())))?
            .len();
        check_size(size)?;
        let caption = check_caption(arg_opt_str(&args, "caption"))?;
        let html = wants_html(&args);
        let kind = match FileKind::parse(arg_opt_str(&args, "kind"))? {
            Some(FileKind::Photo) if size > crate::media::PHOTO_MAX_BYTES => {
                return Err(ToolError::InvalidArgs(
                    "foto acima de 10 MB: envie com kind=document".into(),
                ))
            }
            Some(k) => k,
            None => FileKind::detect(&path, size),
        };
        let duration = arg_opt_i64(&args, "duration_secs").map(|d| d.max(1) as u32);
        let (chat_id, topic_id) =
            allowed_destination(&self.sender, &ctx, &args, "telegram_send_file")?;
        let mime = mime_for_path(&path);
        tracing::debug!(
            channel = "telegram",
            chat_id,
            ?topic_id,
            thread_id = ?ctx.thread_id,
            kind = kind.as_str(),
            mime,
            size,
            "telegram_send_file"
        );
        let id = self
            .sender
            .send_file(
                chat_id,
                topic_id,
                ctx.thread_id,
                &path,
                kind,
                caption,
                html,
                reply_to(&args),
                duration,
            )
            .await
            .map_err(ToolError::Failed)?;
        Ok(ToolOutput::json(&serde_json::json!({
            "message_id": id,
            "chat_id": chat_id,
            "topic_id": topic_id,
            "kind": kind.as_str(),
            "mime": mime,
            "size_bytes": size,
            "file_name": path.file_name().map(|f| f.to_string_lossy().to_string()),
        })))
    }
}

// ---------------------------------------------------------------------------
// telegram_send_voice
// ---------------------------------------------------------------------------

pub struct SendVoiceTool {
    pub(crate) sender: Arc<TelegramSender>,
    pub(crate) kb_path: PathBuf,
    /// Síntese de voz (chave da OpenAI do módulo de voz). `None` = só `path`.
    pub(crate) openai: Option<OpenAi>,
    /// Voz e velocidade do TTS (as mesmas do "resumo em áudio" do Telegram).
    pub(crate) voice: String,
    pub(crate) speed: f32,
    /// Pasta (dentro da KB) onde a fala sintetizada é gravada antes do envio.
    pub(crate) voice_dir: PathBuf,
}

/// Teto de caracteres aceito pela API de TTS da OpenAI.
const TTS_MAX_CHARS: usize = 4000;

#[async_trait::async_trait]
impl WillTool for SendVoiceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "telegram_send_voice".into(),
            description: "Envia uma MENSAGEM DE VOZ (balão com onda sonora) pelo bot do Telegram. `text` = fala sintetizada por TTS (se houver chave da OpenAI) OU `path` = arquivo de áudio dentro da KB (ogg/opus de preferência; mp3 e m4a também). Sem chat_id/topic_id, vai para o tópico de origem da thread (ou o destino padrão). Exige aprovação. Use só quando o usuário pedir áudio.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": merge(serde_json::json!({
                    "text": { "type": "string", "description": "Texto a ser falado (até 4000 caracteres); alternativa a path. Escreva como fala, sem Markdown, emojis ou listas." },
                    "path": { "type": "string", "description": "Arquivo de áudio dentro da KB (absoluto ou relativo à KB); alternativa a text." },
                    "caption": { "type": "string", "description": "Legenda curta (opcional, até 1024 caracteres)." },
                    "duration_secs": { "type": "integer", "minimum": 1, "description": "Duração em segundos (opcional; melhora a exibição)." }
                }), destination_props()),
                "required": []
            }),
            requires: vec![Module::Telegram],
            default_policy: ToolPolicy::Confirm,
            family: "telegram".into(),
        }
    }

    fn approval_summary(&self, args: &serde_json::Value) -> String {
        let what = arg_opt_str(args, "text")
            .map(|t| truncate_chars(t, 160))
            .or_else(|| arg_opt_str(args, "path").map(str::to_string))
            .unwrap_or_default();
        format!(
            "Enviar mensagem de voz no Telegram para {}: {what}",
            describe_destination(args)
        )
    }

    async fn call(
        &self,
        ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let (chat_id, topic_id) =
            allowed_destination(&self.sender, &ctx, &args, "telegram_send_voice")?;
        let caption = check_caption(arg_opt_str(&args, "caption"))?;
        let duration = arg_opt_i64(&args, "duration_secs").map(|d| d.max(1) as u32);
        let (path, synthesized) = match (arg_opt_str(&args, "path"), arg_opt_str(&args, "text")) {
            (Some(p), _) => (resolve_kb_path(&self.kb_path, p)?, false),
            (None, Some(text)) => (self.synthesize(text).await?, true),
            (None, None) => {
                return Err(ToolError::InvalidArgs(
                    "informe text (fala sintetizada) ou path (áudio na KB)".into(),
                ))
            }
        };
        let mime = mime_for_path(&path);
        if !mime.starts_with("audio/") {
            return Err(ToolError::InvalidArgs(format!(
                "o arquivo precisa ser áudio (ogg/opus, mp3, m4a); {} é {mime}",
                path.display()
            )));
        }
        let size = std::fs::metadata(&path)
            .map_err(|e| ToolError::Failed(format!("ler {}: {e}", path.display())))?
            .len();
        check_size(size)?;
        tracing::debug!(
            channel = "telegram",
            chat_id,
            ?topic_id,
            thread_id = ?ctx.thread_id,
            synthesized,
            mime,
            size,
            "telegram_send_voice"
        );
        let id = self
            .sender
            .send_file(
                chat_id,
                topic_id,
                ctx.thread_id,
                &path,
                FileKind::Voice,
                caption,
                false,
                reply_to(&args),
                duration,
            )
            .await
            .map_err(ToolError::Failed)?;
        Ok(ToolOutput::json(&serde_json::json!({
            "message_id": id,
            "chat_id": chat_id,
            "topic_id": topic_id,
            "path": path,
            "synthesized": synthesized,
            "mime": mime,
            "size_bytes": size,
        })))
    }
}

impl SendVoiceTool {
    /// Texto → Ogg/Opus gravado em `voice_dir` (dentro da KB).
    async fn synthesize(&self, text: &str) -> Result<PathBuf, ToolError> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > TTS_MAX_CHARS {
            return Err(ToolError::InvalidArgs(format!(
                "text vazio ou maior que {TTS_MAX_CHARS} caracteres"
            )));
        }
        let openai = self.openai.as_ref().ok_or_else(|| {
            ToolError::Unavailable(
                "síntese de voz indisponível (sem chave da OpenAI em Configurações → Voz); envie um arquivo em `path`".into(),
            )
        })?;
        let started = std::time::Instant::now();
        let ogg = openai
            .synthesize(text, &self.voice, self.speed)
            .await
            .map_err(|e| ToolError::Failed(format!("síntese de voz: {e}")))?;
        if ogg.is_empty() {
            return Err(ToolError::Failed(
                "síntese de voz devolveu áudio vazio".into(),
            ));
        }
        tracing::debug!(
            channel = "telegram",
            chars = text.chars().count(),
            bytes = ogg.len(),
            duration_ms = started.elapsed().as_millis() as u64,
            "voz sintetizada para telegram_send_voice"
        );
        std::fs::create_dir_all(&self.voice_dir)
            .map_err(|e| ToolError::Failed(format!("criar {}: {e}", self.voice_dir.display())))?;
        let p = self
            .voice_dir
            .join(format!("voz-{}.ogg", uuid::Uuid::now_v7()));
        std::fs::write(&p, &ogg)
            .map_err(|e| ToolError::Failed(format!("gravar {}: {e}", p.display())))?;
        Ok(p)
    }
}

/// Pasta padrão da fala sintetizada: `<kb>/<attachments_dir>/voz-enviada`.
pub(crate) fn default_voice_dir(kb: &Path, attachments_dir: &str) -> PathBuf {
    kb.join(attachments_dir.trim_start_matches('/'))
        .join("voz-enviada")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_description() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(
            describe_destination(&j(r#"{"chat_id":"-100","topic_id":7}"#)),
            "chat -100 / tópico 7"
        );
        assert_eq!(
            describe_destination(&j(r#"{"chat_id":"-100"}"#)),
            "chat -100"
        );
        assert_eq!(
            describe_destination(&j(r#"{"topic_id":7}"#)),
            "tópico 7 (chat de origem)"
        );
        assert_eq!(
            describe_destination(&j("{}")),
            "o tópico de origem (ou destino padrão)"
        );
    }

    #[test]
    fn html_and_reply_args() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert!(wants_html(&j(r#"{"parse_mode":"html"}"#)));
        assert!(wants_html(&j(r#"{"parse_mode":"HTML"}"#)));
        assert!(!wants_html(&j(r#"{"parse_mode":"text"}"#)));
        assert!(!wants_html(&j("{}")));
        assert_eq!(reply_to(&j(r#"{"reply_to_message_id":42}"#)), Some(42));
        assert_eq!(reply_to(&j("{}")), None);
    }

    #[test]
    fn voice_dir_under_attachments() {
        let p = default_voice_dir(Path::new("/kb"), "inbox/telegram");
        assert_eq!(p, PathBuf::from("/kb/inbox/telegram/voz-enviada"));
        let p = default_voice_dir(Path::new("/kb"), "/inbox/telegram");
        assert_eq!(p, PathBuf::from("/kb/inbox/telegram/voz-enviada"));
    }

    #[test]
    fn schemas_are_objects_with_destination() {
        // Só a spec (sem rede): confere que os três schemas trazem chat_id/topic_id.
        let props = destination_props();
        for k in ["chat_id", "topic_id", "reply_to_message_id"] {
            assert!(props.get(k).is_some(), "{k} ausente");
        }
        let merged = merge(serde_json::json!({"text": {"type": "string"}}), props);
        assert!(merged.get("text").is_some());
        assert!(merged.get("chat_id").is_some());
    }
}
