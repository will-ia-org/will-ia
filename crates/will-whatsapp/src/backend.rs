//! Contrato entre o módulo e a implementação do protocolo (`WhatsAppBackend`).
//!
//! O backend fala o protocolo e entrega eventos crus por um `mpsc`; o módulo
//! indexa, emite eventos do serviço e expõe ferramentas. Assim a dependência
//! `whatsapp-rust` (0.x, sujeita a quebras) fica isolada em um único arquivo.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use will_storage::whatsapp::WaContact;

/// Mensagem recebida (tempo real ou history sync), já normalizada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingMessage {
    pub id: String,
    pub chat_jid: String,
    pub sender_jid: String,
    pub sender_name: Option<String>,
    pub from_me: bool,
    pub timestamp: DateTime<Utc>,
    /// `text | image | audio | video | document | sticker | location | other`
    pub kind: String,
    pub text: Option<String>,
    /// Referência opaca para download sob demanda (JSON no backend real).
    pub media_ref: Option<String>,
    pub quoted_id: Option<String>,
    pub is_group: bool,
    pub chat_name: Option<String>,
}

/// Eventos emitidos pelo backend para o módulo.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Qr {
        code: String,
        timeout_secs: u64,
    },
    PairCode {
        code: String,
    },
    Connected {
        me: serde_json::Value,
    },
    Disconnected {
        reason: String,
    },
    LoggedOut,
    Message(IncomingMessage),
    /// `kind`: `delivered | read | played | sent | ...`. `message_ids` vazio com
    /// `kind == "read"` significa "chat inteiro lido" (sincronizado do telefone).
    Receipt {
        chat_jid: String,
        message_ids: Vec<String>,
        kind: String,
    },
    ContactUpdate(WaContact),
    /// Metadados de grupo. Se `subject` e `participants` vierem vazios o módulo
    /// consulta `fetch_group` para completar.
    GroupUpdate {
        jid: String,
        subject: Option<String>,
        participants: Vec<String>,
        admins: Vec<String>,
        description: Option<String>,
    },
    ChatArchived {
        jid: String,
        archived: bool,
    },
}

#[async_trait::async_trait]
pub trait WhatsAppBackend: Send + Sync {
    /// `"real"` ou `"mock"` (aparece em `status()`).
    fn kind(&self) -> &'static str;
    /// Há sessão persistida (store local) para reconectar sem parear?
    fn has_session(&self) -> bool;
    /// Inicia a sessão. Com `pair_phone` usa pareamento por código; sem ele,
    /// por QR (se não houver sessão). Eventos saem pelo canal.
    async fn connect(&self, pair_phone: Option<String>) -> Result<(), String>;
    async fn disconnect(&self) -> Result<(), String>;
    /// Desvincula remotamente e apaga a sessão local.
    async fn logout(&self) -> Result<(), String>;
    /// Retorna o id da mensagem enviada.
    async fn send_text(
        &self,
        chat_jid: &str,
        text: &str,
        quote_message_id: Option<&str>,
    ) -> Result<String, String>;
    async fn send_media(
        &self,
        chat_jid: &str,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<String, String>;
    /// Baixa a mídia da mensagem para `dest_dir` e devolve o caminho do arquivo.
    /// `media_ref` é a referência guardada no índice (permite baixar após reinício).
    async fn download_media(
        &self,
        message_id: &str,
        media_ref: Option<&str>,
        dest_dir: &Path,
    ) -> Result<PathBuf, String>;
    async fn mark_read(&self, chat_jid: &str) -> Result<(), String>;
    /// `{jid, subject, description, participants: [jid], admins: [jid]}`
    async fn fetch_group(&self, jid: &str) -> Result<serde_json::Value, String>;
    /// Telefone (dígitos) → JID se o número estiver no WhatsApp; `None` se não estiver.
    async fn resolve_phone(&self, phone: &str) -> Result<Option<String>, String>;
    fn connected(&self) -> bool;
    /// `{jid, phone, push_name, platform}`
    fn me(&self) -> Option<serde_json::Value>;
}

pub const USER_SERVER: &str = "s.whatsapp.net";
pub const GROUP_SERVER: &str = "g.us";

pub fn is_group_jid(jid: &str) -> bool {
    jid.ends_with(&format!("@{GROUP_SERVER}"))
}

/// Dígitos do telefone a partir de um JID de usuário (`5511...@s.whatsapp.net`).
pub fn phone_from_jid(jid: &str) -> Option<String> {
    let (user, server) = jid.split_once('@')?;
    if server != USER_SERVER && server != "c.us" {
        return None;
    }
    let user = user.split(':').next().unwrap_or(user);
    let digits: String = user.chars().filter(|c| c.is_ascii_digit()).collect();
    (!digits.is_empty()).then_some(digits)
}

/// Normaliza um telefone para dígitos (sem `+`, espaços, traços).
pub fn phone_digits(phone: &str) -> Option<String> {
    let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    (digits.len() >= 8 && digits.len() <= 15).then_some(digits)
}

pub fn jid_from_phone(digits: &str) -> String {
    format!("{digits}@{USER_SERVER}")
}

/// Extensão de arquivo sugerida para um mime type de mídia.
pub fn ext_for_mime(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "video/3gpp" => "3gp",
        "audio/ogg" => "ogg",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/aac" => "m4a",
        "audio/wav" => "wav",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "application/zip" => "zip",
        _ => "bin",
    }
}

/// Mime type por extensão (subconjunto suficiente para envio de mídia).
pub fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("mp4" | "m4v") => "video/mp4",
        Some("3gp") => "video/3gpp",
        Some("ogg" | "opus") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("m4a" | "aac") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("pdf") => "application/pdf",
        Some("txt" | "md") => "text/plain",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// Classe de mídia (image/video/audio/document) para um mime.
pub fn media_class(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        "document"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jid_helpers() {
        assert!(is_group_jid("123-456@g.us"));
        assert!(!is_group_jid("5511999@s.whatsapp.net"));
        assert_eq!(
            phone_from_jid("5511999999999:3@s.whatsapp.net").as_deref(),
            Some("5511999999999")
        );
        assert_eq!(phone_from_jid("abc@lid"), None);
        assert_eq!(
            phone_digits("+55 (11) 99999-9999").as_deref(),
            Some("5511999999999")
        );
        assert_eq!(phone_digits("123"), None);
        assert_eq!(jid_from_phone("5511"), "5511@s.whatsapp.net");
        assert_eq!(ext_for_mime("image/jpeg"), "jpg");
        assert_eq!(media_class(mime_for_path(Path::new("a.PDF"))), "document");
    }
}
