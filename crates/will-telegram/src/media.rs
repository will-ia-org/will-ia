//! Helpers puros para envio de arquivos pelo bot: classe de mídia (foto, vídeo,
//! áudio, animação, documento, voz), MIME por extensão, limites do Bot API e
//! resolução segura de caminhos dentro da base de conhecimento.

use std::path::{Path, PathBuf};
use will_core::ToolError;

/// Limite do Bot API para upload por `multipart` (50 MB).
pub const BOT_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Limite do Bot API para `sendPhoto` (10 MB); acima disso vai como documento.
pub const PHOTO_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Legenda de mídia: 0–1024 caracteres após o parse das entidades.
pub const CAPTION_MAX_CHARS: usize = 1024;

/// Método do Bot API usado para o arquivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Photo,
    Video,
    Audio,
    Animation,
    Document,
    /// Mensagem de voz (balão com onda sonora); só em [`crate::SendVoiceTool`].
    Voice,
}

impl FileKind {
    /// Nome usado nos argumentos/saída da ferramenta.
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Photo => "photo",
            FileKind::Video => "video",
            FileKind::Audio => "audio",
            FileKind::Animation => "animation",
            FileKind::Document => "document",
            FileKind::Voice => "voice",
        }
    }

    /// `kind` informado pelo agente (`auto` ou vazio = detectar pela extensão).
    pub fn parse(s: Option<&str>) -> Result<Option<Self>, ToolError> {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("auto") => Ok(None),
            Some("photo") | Some("image") | Some("foto") | Some("imagem") => {
                Ok(Some(FileKind::Photo))
            }
            Some("video") | Some("vídeo") => Ok(Some(FileKind::Video)),
            Some("audio") | Some("áudio") | Some("music") => Ok(Some(FileKind::Audio)),
            Some("animation") | Some("gif") => Ok(Some(FileKind::Animation)),
            Some("document") | Some("file") | Some("documento") | Some("arquivo") => {
                Ok(Some(FileKind::Document))
            }
            Some(other) => Err(ToolError::InvalidArgs(format!(
                "kind desconhecido: {other} (use auto, photo, video, audio, animation ou document)"
            ))),
        }
    }

    /// Classe automática pela extensão e tamanho: fotos grandes demais para
    /// `sendPhoto` viram documento (o Telegram recusaria ou recomprimiria).
    pub fn detect(path: &Path, size: u64) -> Self {
        let mime = mime_for_path(path);
        if mime == "image/gif" {
            return FileKind::Animation;
        }
        if mime.starts_with("image/") {
            return if size <= PHOTO_MAX_BYTES {
                FileKind::Photo
            } else {
                FileKind::Document
            };
        }
        if mime.starts_with("video/") {
            return FileKind::Video;
        }
        if mime.starts_with("audio/") {
            return FileKind::Audio;
        }
        FileKind::Document
    }
}

/// MIME pela extensão (mesma tabela do WhatsApp, mais alguns formatos comuns).
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
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("3gp") => "video/3gpp",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("m4a" | "aac") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("flac") => "audio/flac",
        Some("pdf") => "application/pdf",
        Some("txt" | "md") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("zip") => "application/zip",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

/// Resolve `raw` (absoluto ou relativo à KB) para um arquivo regular **dentro**
/// da base de conhecimento. Path traversal e links para fora são recusados.
pub fn resolve_kb_path(kb: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ToolError::InvalidArgs("path vazio".into()));
    }
    let candidate = {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            kb.join(p)
        }
    };
    let kb_canon = kb
        .canonicalize()
        .map_err(|e| ToolError::Failed(format!("KB inacessível ({}): {e}", kb.display())))?;
    let canon = candidate
        .canonicalize()
        .map_err(|_| ToolError::InvalidArgs(format!("arquivo não encontrado: {raw}")))?;
    if !canon.starts_with(&kb_canon) {
        return Err(ToolError::Forbidden(
            "path deve estar dentro da base de conhecimento".into(),
        ));
    }
    if !canon.is_file() {
        return Err(ToolError::InvalidArgs(format!(
            "path não é um arquivo: {raw}"
        )));
    }
    Ok(canon)
}

/// Valida o tamanho contra o limite do Bot API.
pub fn check_size(size: u64) -> Result<(), ToolError> {
    if size == 0 {
        return Err(ToolError::InvalidArgs("arquivo vazio".into()));
    }
    if size > BOT_MAX_FILE_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "arquivo com {:.1} MB; o Bot API aceita até 50 MB por upload",
            size as f64 / (1024.0 * 1024.0)
        )));
    }
    Ok(())
}

/// Valida a legenda (tamanho) e devolve `None` quando vazia.
pub fn check_caption(caption: Option<&str>) -> Result<Option<&str>, ToolError> {
    let Some(c) = caption.map(str::trim).filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    let n = c.chars().count();
    if n > CAPTION_MAX_CHARS {
        return Err(ToolError::InvalidArgs(format!(
            "legenda com {n} caracteres; o Telegram aceita até {CAPTION_MAX_CHARS}. Mande o texto longo em telegram_send_message"
        )));
    }
    Ok(Some(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_by_extension_and_size() {
        let p = Path::new("x/foto.JPG");
        assert_eq!(FileKind::detect(p, 1024), FileKind::Photo);
        assert_eq!(FileKind::detect(p, PHOTO_MAX_BYTES + 1), FileKind::Document);
        assert_eq!(
            FileKind::detect(Path::new("a.gif"), 10),
            FileKind::Animation
        );
        assert_eq!(FileKind::detect(Path::new("a.mp4"), 10), FileKind::Video);
        assert_eq!(FileKind::detect(Path::new("a.mp3"), 10), FileKind::Audio);
        assert_eq!(FileKind::detect(Path::new("a.pdf"), 10), FileKind::Document);
        assert_eq!(
            FileKind::detect(Path::new("semext"), 10),
            FileKind::Document
        );
    }

    #[test]
    fn parse_kind_aliases() {
        assert_eq!(FileKind::parse(None).unwrap(), None);
        assert_eq!(FileKind::parse(Some("auto")).unwrap(), None);
        assert_eq!(
            FileKind::parse(Some("Foto")).unwrap(),
            Some(FileKind::Photo)
        );
        assert_eq!(
            FileKind::parse(Some("gif")).unwrap(),
            Some(FileKind::Animation)
        );
        assert_eq!(
            FileKind::parse(Some("arquivo")).unwrap(),
            Some(FileKind::Document)
        );
        assert!(FileKind::parse(Some("sticker")).is_err());
    }

    #[test]
    fn mime_table() {
        assert_eq!(mime_for_path(Path::new("a.PNG")), "image/png");
        assert_eq!(mime_for_path(Path::new("a.opus")), "audio/ogg");
        assert_eq!(mime_for_path(Path::new("a.csv")), "text/csv");
        assert_eq!(
            mime_for_path(Path::new("a.bin")),
            "application/octet-stream"
        );
    }

    #[test]
    fn size_and_caption_limits() {
        assert!(check_size(0).is_err());
        assert!(check_size(BOT_MAX_FILE_BYTES).is_ok());
        assert!(check_size(BOT_MAX_FILE_BYTES + 1).is_err());
        assert_eq!(check_caption(None).unwrap(), None);
        assert_eq!(check_caption(Some("   ")).unwrap(), None);
        assert_eq!(check_caption(Some(" oi ")).unwrap(), Some("oi"));
        let long = "x".repeat(CAPTION_MAX_CHARS + 1);
        assert!(check_caption(Some(&long)).is_err());
    }

    #[test]
    fn kb_path_resolution() {
        let kb = tempdir();
        let inside = kb.join("inbox");
        std::fs::create_dir_all(&inside).unwrap();
        let file = inside.join("doc.pdf");
        std::fs::write(&file, b"%PDF").unwrap();

        let ok = resolve_kb_path(&kb, "inbox/doc.pdf").unwrap();
        assert_eq!(ok, file.canonicalize().unwrap());
        let ok_abs = resolve_kb_path(&kb, file.to_str().unwrap()).unwrap();
        assert_eq!(ok_abs, file.canonicalize().unwrap());

        assert!(matches!(
            resolve_kb_path(&kb, "inbox/nao-existe.pdf"),
            Err(ToolError::InvalidArgs(_))
        ));
        assert!(matches!(
            resolve_kb_path(&kb, "inbox"),
            Err(ToolError::InvalidArgs(_))
        ));
        let outside = std::env::temp_dir().join(format!("wt-fora-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&outside, b"x").unwrap();
        assert!(matches!(
            resolve_kb_path(&kb, outside.to_str().unwrap()),
            Err(ToolError::Forbidden(_))
        ));
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&kb);
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("will-telegram-media-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
