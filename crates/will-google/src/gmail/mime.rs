//! Utilitários de MIME/Gmail: base64url, extração de corpo/anexos do payload
//! `users.messages.get(format=full)`, derivação de `unread`/`archived`,
//! montagem de mensagens de saída (incl. resposta na thread).

use base64::Engine;
use std::io::Read;

pub const LABEL_INBOX: &str = "INBOX";
pub const LABEL_UNREAD: &str = "UNREAD";
pub const LABEL_TRASH: &str = "TRASH";
pub const LABEL_SPAM: &str = "SPAM";

pub fn decode_b64url(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let trimmed = cleaned.trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(trimmed))
        .map_err(|e| format!("base64 inválido: {e}"))
}

pub fn encode_b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `unread` = tem `UNREAD`; `archived` = sem `INBOX` e fora de `TRASH`/`SPAM`.
pub fn flags_from_labels(labels: &[String]) -> (bool, bool) {
    let has = |l: &str| labels.iter().any(|x| x == l);
    let unread = has(LABEL_UNREAD);
    let archived = !has(LABEL_INBOX) && !has(LABEL_TRASH) && !has(LABEL_SPAM);
    (unread, archived)
}

pub fn labels_of(msg: &serde_json::Value) -> Vec<String> {
    msg.get("labelIds")
        .and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Header (case-insensitive) de um `payload`.
pub fn header(payload: &serde_json::Value, name: &str) -> Option<String> {
    payload
        .get("headers")?
        .as_array()?
        .iter()
        .find(|h| {
            h.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .and_then(|h| h.get("value").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

pub fn all_headers(payload: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    if let Some(hs) = payload.get("headers").and_then(|h| h.as_array()) {
        for h in hs {
            if let (Some(n), Some(v)) = (
                h.get("name").and_then(|n| n.as_str()),
                h.get("value").and_then(|v| v.as_str()),
            ) {
                m.insert(n.to_string(), serde_json::Value::String(v.to_string()));
            }
        }
    }
    m
}

/// Extrai endereços (minúsculos) de um header `From`/`To`/`Cc`.
pub fn parse_addresses(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in value.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let addr = if let (Some(a), Some(b)) = (p.rfind('<'), p.rfind('>')) {
            if a < b {
                &p[a + 1..b]
            } else {
                p
            }
        } else {
            p
        };
        let addr = addr.trim().trim_matches('"').to_ascii_lowercase();
        if addr.contains('@') && !out.contains(&addr) {
            out.push(addr);
        }
    }
    out
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AttachmentInfo {
    pub attachment_id: Option<String>,
    pub filename: String,
    pub mime: String,
    pub size: u64,
    pub inline: bool,
}

#[derive(Debug, Default)]
pub struct ExtractedBody {
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

/// Percorre as partes do payload (recursivo) coletando texto, HTML e anexos.
pub fn extract_body(payload: &serde_json::Value) -> ExtractedBody {
    let mut out = ExtractedBody::default();
    walk(payload, &mut out);
    out
}

fn walk(part: &serde_json::Value, out: &mut ExtractedBody) {
    let mime = part
        .get("mimeType")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let filename = part.get("filename").and_then(|f| f.as_str()).unwrap_or("");
    let body = part.get("body");
    let data = body.and_then(|b| b.get("data")).and_then(|d| d.as_str());
    let attachment_id = body
        .and_then(|b| b.get("attachmentId"))
        .and_then(|a| a.as_str());
    let size = body
        .and_then(|b| b.get("size"))
        .and_then(|s| s.as_u64())
        .unwrap_or(0);
    let disposition = header(part, "Content-Disposition")
        .unwrap_or_default()
        .to_ascii_lowercase();

    if !filename.is_empty() || attachment_id.is_some() {
        out.attachments.push(AttachmentInfo {
            attachment_id: attachment_id.map(|s| s.to_string()),
            filename: if filename.is_empty() {
                "anexo".into()
            } else {
                filename.to_string()
            },
            mime: if mime.is_empty() {
                "application/octet-stream".into()
            } else {
                mime.clone()
            },
            size,
            inline: disposition.starts_with("inline"),
        });
        return;
    }
    if let Some(parts) = part.get("parts").and_then(|p| p.as_array()) {
        for p in parts {
            walk(p, out);
        }
        return;
    }
    if let Some(d) = data {
        let decoded = decode_b64url(d)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        if mime == "text/plain" || (mime.is_empty() && out.text.is_none()) {
            if out.text.is_none() {
                out.text = Some(decoded);
            }
        } else if mime == "text/html" && out.html.is_none() {
            out.html = Some(decoded);
        }
    }
}

pub fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 100).unwrap_or_else(|_| strip_tags(html))
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Remove citações (`>`) e tudo após marcadores típicos de resposta.
pub fn strip_quotes(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('>') {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        let is_marker = (lower.starts_with("on ") && lower.ends_with("wrote:"))
            || (lower.starts_with("em ") && lower.ends_with("escreveu:"))
            || lower.starts_with("-----original message-----")
            || lower.starts_with("-----mensagem original-----")
            || lower.starts_with("from: ")
            || lower.starts_with("de: ") && lower.contains('@');
        if is_marker {
            break;
        }
        out.push(line);
    }
    let s = out.join("\n");
    s.trim_end().to_string()
}

/// Corta `s` em `max_bytes` sem quebrar UTF-8. Retorna (texto, truncado?).
pub fn cap_bytes(s: &str, max_bytes: usize) -> (String, bool) {
    if s.len() <= max_bytes {
        return (s.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}\n[... truncado ...]", &s[..end]), true)
}

pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut s: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s = s.trim().trim_matches('.').to_string();
    if s.is_empty() {
        s = "anexo".into();
    }
    if s.chars().count() > 120 {
        s = s.chars().take(120).collect();
    }
    s
}

pub fn reply_subject(original: &str) -> String {
    let t = original.trim();
    if t.to_ascii_lowercase().starts_with("re:") {
        t.to_string()
    } else {
        format!("Re: {t}")
    }
}

/// Mensagem de saída (envio ou rascunho).
#[derive(Debug, Default)]
pub struct OutgoingMail {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// (content-type, nome, bytes)
    pub attachments: Vec<(String, String, Vec<u8>)>,
}

/// Monta o MIME (RFC 5322) pronto para `raw` (base64url).
pub fn build_mime(mail: &OutgoingMail) -> Result<Vec<u8>, String> {
    use mail_builder::MessageBuilder;
    let mut b = MessageBuilder::new()
        .from(mail.from.as_str())
        .subject(mail.subject.as_str());
    if !mail.to.is_empty() {
        b = b.to(mail.to.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }
    if !mail.cc.is_empty() {
        b = b.cc(mail.cc.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }
    if !mail.bcc.is_empty() {
        b = b.bcc(mail.bcc.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }
    // mail-builder acrescenta os `<>`; aceitamos ids com ou sem.
    let bare = |s: &str| {
        s.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_string()
    };
    if let Some(irt) = &mail.in_reply_to {
        b = b.in_reply_to(bare(irt));
    }
    if !mail.references.is_empty() {
        b = b.references(mail.references.iter().map(|s| bare(s)).collect::<Vec<_>>());
    }
    b = b.text_body(mail.text.as_str());
    if let Some(h) = &mail.html {
        b = b.html_body(h.as_str());
    }
    for (ctype, name, bytes) in &mail.attachments {
        b = b.attachment(ctype.as_str(), name.as_str(), bytes.as_slice());
    }
    b.write_to_vec().map_err(|e| format!("montagem MIME: {e}"))
}

/// Lê um `Read` inteiro (helper para testes/fixtures).
pub fn read_all(mut r: impl Read) -> Vec<u8> {
    let mut v = Vec::new();
    let _ = r.read_to_end(&mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_from_labels_work() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(flags_from_labels(&l(&["INBOX", "UNREAD"])), (true, false));
        assert_eq!(flags_from_labels(&l(&["IMPORTANT"])), (false, true));
        assert_eq!(flags_from_labels(&l(&["TRASH"])), (false, false));
        assert_eq!(flags_from_labels(&l(&["SPAM", "UNREAD"])), (true, false));
        assert_eq!(flags_from_labels(&l(&[])), (false, true));
    }

    #[test]
    fn b64url_roundtrip_and_padding_tolerance() {
        let data = b"ol\xc3\xa1 mundo? sim/n\xc3\xa3o";
        let enc = encode_b64url(data);
        assert!(!enc.contains('+') && !enc.contains('/') && !enc.contains('='));
        assert_eq!(decode_b64url(&enc).unwrap(), data);
        assert_eq!(decode_b64url("aGk=").unwrap(), b"hi");
        assert_eq!(decode_b64url("aGk").unwrap(), b"hi");
        assert_eq!(decode_b64url("a\nGk\r\n").unwrap(), b"hi");
        assert!(decode_b64url("!!!").is_err());
    }

    #[test]
    fn extracts_multipart_body_and_attachments() {
        let payload = serde_json::json!({
            "mimeType": "multipart/mixed",
            "headers": [{"name": "Subject", "value": "Oi"}, {"name": "from", "value": "Ana <ana@x.com>"}],
            "parts": [
                {"mimeType": "multipart/alternative", "parts": [
                    {"mimeType": "text/plain", "filename": "", "body": {"data": encode_b64url(b"texto puro")}},
                    {"mimeType": "text/html", "filename": "", "body": {"data": encode_b64url(b"<p>html <b>rico</b></p>")}}
                ]},
                {"mimeType": "application/pdf", "filename": "doc.pdf", "body": {"attachmentId": "ATT1", "size": 1234}}
            ]
        });
        let b = extract_body(&payload);
        assert_eq!(b.text.as_deref(), Some("texto puro"));
        assert!(b.html.as_deref().unwrap().contains("<b>rico</b>"));
        assert_eq!(b.attachments.len(), 1);
        assert_eq!(b.attachments[0].filename, "doc.pdf");
        assert_eq!(b.attachments[0].attachment_id.as_deref(), Some("ATT1"));
        assert_eq!(header(&payload, "FROM").as_deref(), Some("Ana <ana@x.com>"));
        assert_eq!(
            parse_addresses("Ana <ana@x.com>, bob@y.org, \"C\" <c@z.io>"),
            vec!["ana@x.com", "bob@y.org", "c@z.io"]
        );
    }

    #[test]
    fn html_only_is_converted() {
        let payload = serde_json::json!({
            "mimeType": "text/html",
            "body": {"data": encode_b64url(b"<html><body><h1>Titulo</h1><p>Linha</p></body></html>")}
        });
        let b = extract_body(&payload);
        assert!(b.text.is_none());
        let txt = html_to_text(b.html.as_deref().unwrap());
        assert!(txt.contains("Titulo"));
        assert!(txt.contains("Linha"));
        assert!(!txt.contains("<p>"));
    }

    #[test]
    fn builds_reply_with_threading_headers() {
        let mail = OutgoingMail {
            from: "eu@gmail.com".into(),
            to: vec!["ana@x.com".into()],
            cc: vec![],
            bcc: vec!["oculto@x.com".into()],
            subject: reply_subject("Reuniao"),
            text: "Fechado.".into(),
            html: None,
            in_reply_to: Some("<abc@mail.x.com>".into()),
            references: vec!["<root@mail.x.com>".into(), "<abc@mail.x.com>".into()],
            attachments: vec![("text/plain".into(), "nota.txt".into(), b"ok".to_vec())],
        };
        let raw = String::from_utf8(build_mime(&mail).unwrap()).unwrap();
        assert!(raw.contains("Subject: Re: Reuniao"), "{raw}");
        assert!(raw.contains("In-Reply-To: <abc@mail.x.com>"), "{raw}");
        assert!(
            raw.contains("References: <root@mail.x.com> <abc@mail.x.com>"),
            "{raw}"
        );
        assert!(
            raw.contains("To: <ana@x.com>") || raw.contains("To: ana@x.com"),
            "{raw}"
        );
        assert!(raw.contains("nota.txt"));
        assert_eq!(reply_subject("RE: x"), "RE: x");
    }

    #[test]
    fn strip_quotes_and_cap() {
        let t = "Oi\n\n> citado\nEm seg., Ana escreveu:\n> mais";
        assert_eq!(strip_quotes(t), "Oi");
        let (c, trunc) = cap_bytes("áéíóú", 3);
        assert!(trunc);
        assert!(c.starts_with('á'));
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("rel: a/b?.pdf"), "b_.pdf");
    }
}
