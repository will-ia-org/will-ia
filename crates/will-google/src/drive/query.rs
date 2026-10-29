//! Helpers puros do Drive: montagem da consulta `q`, normalização de arquivo e
//! permissão, escolha do formato de exportação e tipos MIME do Google.

use serde_json::{json, Value};
use will_core::settings::DriveSettings;
use will_core::ToolError;

pub const MIME_FOLDER: &str = "application/vnd.google-apps.folder";
pub const MIME_DOC: &str = "application/vnd.google-apps.document";
pub const MIME_SHEET: &str = "application/vnd.google-apps.spreadsheet";
pub const MIME_SLIDES: &str = "application/vnd.google-apps.presentation";
pub const MIME_DRAWING: &str = "application/vnd.google-apps.drawing";
pub const MIME_SHORTCUT: &str = "application/vnd.google-apps.shortcut";
pub const MIME_FORM: &str = "application/vnd.google-apps.form";
const GOOGLE_PREFIX: &str = "application/vnd.google-apps.";

/// Campos pedidos em toda leitura de arquivo (`fields=files(...)`).
pub const FILE_FIELDS: &str = "id,name,mimeType,size,createdTime,modifiedTime,parents,\
owners(displayName,emailAddress),lastModifyingUser(displayName,emailAddress),shared,trashed,\
starred,description,webViewLink,webContentLink,iconLink,driveId,md5Checksum,\
shortcutDetails(targetId,targetMimeType),capabilities(canEdit,canShare,canDownload,canTrash,canDelete)";
pub const PERM_FIELDS: &str =
    "id,type,role,emailAddress,domain,displayName,allowFileDiscovery,expirationTime,pendingOwner";

/// Escapa um literal para a sintaxe `q` (`\` e `'`).
pub fn escape_q(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Aliases aceitos em `mime_type` / `convert_to`.
pub fn mime_alias(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "folder" | "pasta" => MIME_FOLDER.into(),
        "doc" | "docs" | "document" | "documento" | "gdoc" => MIME_DOC.into(),
        "sheet" | "sheets" | "spreadsheet" | "planilha" | "gsheet" => MIME_SHEET.into(),
        "slides" | "presentation" | "apresentacao" | "apresentação" | "gslides" => {
            MIME_SLIDES.into()
        }
        "drawing" | "desenho" => MIME_DRAWING.into(),
        "form" | "formulario" | "formulário" => MIME_FORM.into(),
        "shortcut" | "atalho" => MIME_SHORTCUT.into(),
        "pdf" => "application/pdf".into(),
        "image" | "imagem" => "image/".into(),
        "video" | "vídeo" => "video/".into(),
        "audio" | "áudio" => "audio/".into(),
        other => other.to_string(),
    }
}

/// `YYYY-MM-DD` → `YYYY-MM-DDT00:00:00`; o resto passa como está (RFC 3339).
pub fn date_or_iso(s: &str) -> String {
    let t = s.trim();
    if t.len() == 10 && t.as_bytes()[4] == b'-' && t.as_bytes()[7] == b'-' {
        format!("{t}T00:00:00")
    } else {
        t.to_string()
    }
}

/// Lista de strings a partir de array JSON ou CSV.
pub fn str_list(args: &Value, key: &str) -> Option<Vec<String>> {
    match args.get(key)? {
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        Value::String(s) => Some(
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

/// Monta a consulta `q` a partir de `q` explícito + atalhos. Sem `trashed`
/// explícito, filtra `trashed = false`.
pub fn build_query(args: &Value) -> Result<String, ToolError> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(q) = args.get("q").and_then(|v| v.as_str()) {
        let q = q.trim();
        if !q.is_empty() {
            parts.push(format!("({q})"));
        }
    }
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    if let Some(n) = s("name") {
        parts.push(format!("name contains '{}'", escape_q(n)));
    }
    if let Some(t) = s("full_text") {
        parts.push(format!("fullText contains '{}'", escape_q(t)));
    }
    if let Some(mt) = s("mime_type") {
        let m = mime_alias(mt);
        if m.ends_with('/') {
            parts.push(format!("mimeType contains '{}'", escape_q(&m)));
        } else {
            parts.push(format!("mimeType = '{}'", escape_q(&m)));
        }
    }
    if let Some(d) = s("modified_after") {
        parts.push(format!("modifiedTime > '{}'", escape_q(&date_or_iso(d))));
    }
    if let Some(d) = s("modified_before") {
        parts.push(format!("modifiedTime < '{}'", escape_q(&date_or_iso(d))));
    }
    if let Some(p) = s("parent") {
        let p = if p.eq_ignore_ascii_case("root") {
            "root"
        } else {
            p
        };
        parts.push(format!("'{}' in parents", escape_q(p)));
    }
    if let Some(o) = s("owner") {
        parts.push(format!("'{}' in owners", escape_q(o)));
    }
    if args.get("shared_with_me").and_then(|v| v.as_bool()) == Some(true) {
        parts.push("sharedWithMe = true".into());
    }
    if args.get("starred").and_then(|v| v.as_bool()) == Some(true) {
        parts.push("starred = true".into());
    }
    match args.get("trashed").and_then(|v| v.as_bool()) {
        Some(true) => parts.push("trashed = true".into()),
        Some(false) | None => parts.push("trashed = false".into()),
    }
    let mut q = parts.join(" and ");
    if q.is_empty() {
        q = "trashed = false".into();
    }
    if q.len() > 4000 {
        return Err(ToolError::InvalidArgs("consulta longa demais".into()));
    }
    Ok(q)
}

fn parse_size(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

fn person(v: &Value) -> Option<String> {
    let name = v.get("displayName").and_then(|x| x.as_str()).unwrap_or("");
    let email = v.get("emailAddress").and_then(|x| x.as_str()).unwrap_or("");
    match (name.is_empty(), email.is_empty()) {
        (true, true) => None,
        (false, true) => Some(name.to_string()),
        (true, false) => Some(email.to_string()),
        (false, false) => Some(format!("{name} <{email}>")),
    }
}

/// Tipo legível a partir do MIME.
pub fn kind_of(mime: &str) -> &'static str {
    match mime {
        MIME_FOLDER => "folder",
        MIME_DOC => "google_doc",
        MIME_SHEET => "google_sheet",
        MIME_SLIDES => "google_slides",
        MIME_DRAWING => "google_drawing",
        MIME_FORM => "google_form",
        MIME_SHORTCUT => "shortcut",
        m if m.starts_with(GOOGLE_PREFIX) => "google_app",
        _ => "file",
    }
}

pub fn is_google_type(mime: &str) -> bool {
    mime.starts_with(GOOGLE_PREFIX) && mime != MIME_FOLDER && mime != MIME_SHORTCUT
}

/// Arquivo do Drive no formato devolvido ao agente.
pub fn normalize_file(v: &Value) -> Value {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    let mime = s("mimeType").unwrap_or_default();
    let owners: Vec<String> = v
        .get("owners")
        .and_then(|o| o.as_array())
        .map(|a| a.iter().filter_map(person).collect())
        .unwrap_or_default();
    let caps = v.get("capabilities").cloned().unwrap_or(Value::Null);
    let mut out = json!({
        "id": s("id"),
        "name": s("name"),
        "mime_type": mime,
        "kind": kind_of(&mime),
        "size": v.get("size").and_then(parse_size),
        "created": s("createdTime"),
        "modified": s("modifiedTime"),
        "modified_by": v.get("lastModifyingUser").and_then(person),
        "owner": owners.first().cloned(),
        "parents": v.get("parents").cloned().unwrap_or(json!([])),
        "shared": b("shared"),
        "trashed": b("trashed"),
        "starred": b("starred"),
        "web_view_link": s("webViewLink"),
        "can": {
            "edit": caps.get("canEdit").and_then(|x| x.as_bool()).unwrap_or(false),
            "share": caps.get("canShare").and_then(|x| x.as_bool()).unwrap_or(false),
            "download": caps.get("canDownload").and_then(|x| x.as_bool()).unwrap_or(true),
        },
    });
    if owners.len() > 1 {
        out["owners"] = json!(owners);
    }
    if let Some(d) = s("description").filter(|d| !d.is_empty()) {
        out["description"] = json!(d);
    }
    if let Some(d) = s("driveId") {
        out["drive_id"] = json!(d);
    }
    if let Some(m) = s("md5Checksum") {
        out["md5"] = json!(m);
    }
    if let Some(l) = s("webContentLink") {
        out["web_content_link"] = json!(l);
    }
    if let Some(sc) = v.get("shortcutDetails") {
        out["shortcut_target"] = json!({
            "id": sc.get("targetId"),
            "mime_type": sc.get("targetMimeType"),
        });
    }
    out
}

pub fn normalize_permission(v: &Value) -> Value {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let mut out = json!({
        "id": s("id"),
        "type": s("type"),
        "role": s("role"),
    });
    if let Some(e) = s("emailAddress") {
        out["email"] = json!(e);
    }
    if let Some(d) = s("domain") {
        out["domain"] = json!(d);
    }
    if let Some(n) = s("displayName") {
        out["name"] = json!(n);
    }
    if let Some(a) = v.get("allowFileDiscovery").and_then(|x| x.as_bool()) {
        out["allow_discovery"] = json!(a);
    }
    if let Some(e) = s("expirationTime") {
        out["expires"] = json!(e);
    }
    out
}

/// Alias de formato de exportação → MIME.
pub fn export_mime_alias(s: &str) -> String {
    match s
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => "text/markdown".into(),
        "txt" | "text" => "text/plain".into(),
        "html" => "text/html".into(),
        "pdf" => "application/pdf".into(),
        "docx" | "word" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into()
        }
        "xlsx" | "excel" => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into()
        }
        "pptx" | "powerpoint" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".into()
        }
        "csv" => "text/csv".into(),
        "tsv" => "text/tab-separated-values".into(),
        "odt" => "application/vnd.oasis.opendocument.text".into(),
        "ods" => "application/vnd.oasis.opendocument.spreadsheet".into(),
        "odp" => "application/vnd.oasis.opendocument.presentation".into(),
        "rtf" => "application/rtf".into(),
        "epub" => "application/epub+zip".into(),
        "png" => "image/png".into(),
        "jpg" | "jpeg" => "image/jpeg".into(),
        "svg" => "image/svg+xml".into(),
        "zip" => "application/zip".into(),
        "json" => "application/vnd.google-apps.script+json".into(),
        other => other.to_string(),
    }
}

/// Extensão de arquivo para um MIME de exportação/download.
pub fn extension_for(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "text/markdown" => "md",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/csv" => "csv",
        "text/tab-separated-values" => "tsv",
        "application/pdf" => "pdf",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/vnd.oasis.opendocument.text" => "odt",
        "application/vnd.oasis.opendocument.spreadsheet" => "ods",
        "application/vnd.oasis.opendocument.presentation" => "odp",
        "application/rtf" => "rtf",
        "application/epub+zip" => "epub",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/svg+xml" => "svg",
        "application/zip" => "zip",
        "application/vnd.google-apps.script+json" => "json",
        _ => "",
    }
}

/// Para arquivos nativos do Google (Docs/Sheets/Slides/Drawings) devolve o MIME de
/// exportação (`override` > configuração); `None` = download binário normal.
pub fn export_choice(mime: &str, cfg: &DriveSettings, over: Option<&str>) -> Option<String> {
    if !is_google_type(mime) {
        return None;
    }
    if let Some(o) = over.map(str::trim).filter(|o| !o.is_empty()) {
        return Some(export_mime_alias(o));
    }
    let def = match mime {
        MIME_DOC => cfg.export_docs.as_str(),
        MIME_SHEET => cfg.export_sheets.as_str(),
        MIME_SLIDES => cfg.export_slides.as_str(),
        MIME_DRAWING => "image/png",
        _ => "application/pdf",
    };
    let def = if def.trim().is_empty() {
        "application/pdf"
    } else {
        def
    };
    Some(export_mime_alias(def))
}

/// Nome de arquivo para o download: nome do Drive + extensão do export quando
/// o nome ainda não termina com ela.
pub fn download_filename(name: &str, export_mime: Option<&str>) -> String {
    let base = crate::gmail::mime::sanitize_filename(name);
    match export_mime.map(extension_for).filter(|e| !e.is_empty()) {
        Some(ext) if !base.to_ascii_lowercase().ends_with(&format!(".{ext}")) => {
            format!("{base}.{ext}")
        }
        _ => base,
    }
}

/// Corpo JSON + query de `permissions.create`.
pub type PermissionRequest = (Value, Vec<(&'static str, String)>);

/// Corpo e query de `permissions.create` a partir dos argumentos.
pub fn permission_request(args: &Value) -> Result<PermissionRequest, ToolError> {
    let email = args
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|e| !e.is_empty());
    let domain = args
        .get("domain")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|e| !e.is_empty());
    let ptype = match args
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
    {
        Some(t) if !t.is_empty() => t,
        _ => {
            if email.is_some() {
                "user".into()
            } else if domain.is_some() {
                "domain".into()
            } else {
                return Err(ToolError::InvalidArgs(
                    "informe `email` (pessoa/grupo), `domain` ou `type: anyone`".into(),
                ));
            }
        }
    };
    if !["user", "group", "domain", "anyone"].contains(&ptype.as_str()) {
        return Err(ToolError::InvalidArgs(
            "type deve ser user, group, domain ou anyone".into(),
        ));
    }
    let role = args
        .get("role")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "reader".into());
    if !["reader", "commenter", "writer"].contains(&role.as_str()) {
        return Err(ToolError::InvalidArgs(
            "role deve ser reader, commenter ou writer (transferir propriedade não é suportado)"
                .into(),
        ));
    }
    let mut body = json!({"type": ptype, "role": role});
    match ptype.as_str() {
        "user" | "group" => {
            let e = email.ok_or_else(|| {
                ToolError::InvalidArgs("`email` é obrigatório para type user/group".into())
            })?;
            if !e.contains('@') {
                return Err(ToolError::InvalidArgs(format!("e-mail inválido: {e}")));
            }
            body["emailAddress"] = json!(e);
        }
        "domain" => {
            body["domain"] = json!(domain.ok_or_else(|| {
                ToolError::InvalidArgs("`domain` é obrigatório para type domain".into())
            })?);
        }
        _ => {
            body["allowFileDiscovery"] = json!(args
                .get("allow_discovery")
                .and_then(|v| v.as_bool())
                .unwrap_or(false));
        }
    }
    let mut query: Vec<(&'static str, String)> = vec![
        ("supportsAllDrives", "true".into()),
        ("fields", PERM_FIELDS.into()),
    ];
    if matches!(ptype.as_str(), "user" | "group") {
        let notify = args.get("notify").and_then(|v| v.as_bool()).unwrap_or(true);
        query.push(("sendNotificationEmail", notify.to_string()));
        if notify {
            if let Some(msg) = args.get("message").and_then(|v| v.as_str()) {
                if !msg.trim().is_empty() {
                    query.push(("emailMessage", msg.trim().to_string()));
                }
            }
        }
    }
    Ok((body, query))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_shortcuts_and_escaping() {
        let q = build_query(&json!({
            "name": "Relat'orio \\ 2026", "mime_type": "pdf", "parent": "root",
            "modified_after": "2026-01-15", "shared_with_me": true
        }))
        .unwrap();
        assert_eq!(
            q,
            "name contains 'Relat\\'orio \\\\ 2026' and mimeType = 'application/pdf' and \
modifiedTime > '2026-01-15T00:00:00' and 'root' in parents and sharedWithMe = true and trashed = false"
        );
        assert_eq!(build_query(&json!({})).unwrap(), "trashed = false");
        assert_eq!(
            build_query(&json!({"q": "name = 'x'", "trashed": true})).unwrap(),
            "(name = 'x') and trashed = true"
        );
        assert_eq!(
            build_query(&json!({"mime_type": "image"})).unwrap(),
            "mimeType contains 'image/' and trashed = false"
        );
        assert_eq!(
            build_query(&json!({"mime_type": "folder", "full_text": "ata"})).unwrap(),
            format!("fullText contains 'ata' and mimeType = '{MIME_FOLDER}' and trashed = false")
        );
    }

    #[test]
    fn normalizes_file_and_permission() {
        let f = normalize_file(&json!({
            "id": "1", "name": "Plano.gdoc", "mimeType": MIME_DOC, "size": "1234",
            "owners": [{"displayName": "Ana", "emailAddress": "ana@x.com"}],
            "capabilities": {"canEdit": true, "canShare": false},
            "shared": true, "webViewLink": "https://docs.google.com/x"
        }));
        assert_eq!(f["kind"], "google_doc");
        assert_eq!(f["size"], 1234);
        assert_eq!(f["owner"], "Ana <ana@x.com>");
        assert_eq!(f["can"]["edit"], true);
        assert_eq!(f["can"]["share"], false);
        assert_eq!(f["can"]["download"], true);
        assert!(f.get("owners").is_none());
        let p = normalize_permission(
            &json!({"id": "p1", "type": "user", "role": "writer", "emailAddress": "b@x.com"}),
        );
        assert_eq!(p["email"], "b@x.com");
        assert_eq!(kind_of(MIME_FOLDER), "folder");
        assert_eq!(kind_of("image/png"), "file");
    }

    #[test]
    fn export_choice_and_filename() {
        let cfg = DriveSettings::default();
        assert_eq!(
            export_choice(MIME_DOC, &cfg, None).as_deref(),
            Some("text/markdown")
        );
        assert_eq!(
            export_choice(MIME_SHEET, &cfg, None).as_deref(),
            Some("text/csv")
        );
        assert_eq!(
            export_choice(MIME_SLIDES, &cfg, None).as_deref(),
            Some("application/pdf")
        );
        assert_eq!(
            export_choice(MIME_DRAWING, &cfg, None).as_deref(),
            Some("image/png")
        );
        assert_eq!(
            export_choice(MIME_DOC, &cfg, Some("docx")).as_deref(),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        );
        assert_eq!(export_choice("application/pdf", &cfg, Some("md")), None);
        assert_eq!(export_choice(MIME_FOLDER, &cfg, None), None);
        assert_eq!(
            download_filename("Plano 2026", Some("text/markdown")),
            "Plano 2026.md"
        );
        assert_eq!(
            download_filename("plano.md", Some("text/markdown")),
            "plano.md"
        );
        assert_eq!(download_filename("../x/foto.png", None), "foto.png");
        assert_eq!(extension_for("text/csv; charset=utf-8"), "csv");
    }

    #[test]
    fn permission_request_rules() {
        let (b, q) =
            permission_request(&json!({"email": "a@x.com", "role": "writer", "message": "oi"}))
                .unwrap();
        assert_eq!(
            b,
            json!({"type": "user", "role": "writer", "emailAddress": "a@x.com"})
        );
        assert!(q.contains(&("sendNotificationEmail", "true".to_string())));
        assert!(q.contains(&("emailMessage", "oi".to_string())));
        let (b, q) = permission_request(&json!({"type": "anyone"})).unwrap();
        assert_eq!(b["allowFileDiscovery"], false);
        assert!(!q.iter().any(|(k, _)| *k == "sendNotificationEmail"));
        let (b, _) = permission_request(&json!({"domain": "x.com"})).unwrap();
        assert_eq!(b["type"], "domain");
        assert!(permission_request(&json!({})).is_err());
        assert!(permission_request(&json!({"email": "a@x.com", "role": "owner"})).is_err());
        assert!(permission_request(&json!({"email": "semarroba"})).is_err());
    }

    #[test]
    fn aliases() {
        assert_eq!(mime_alias("Planilha"), MIME_SHEET);
        assert_eq!(mime_alias("text/plain"), "text/plain");
        assert_eq!(date_or_iso("2026-02-03"), "2026-02-03T00:00:00");
        assert_eq!(date_or_iso("2026-02-03T10:00:00Z"), "2026-02-03T10:00:00Z");
        assert_eq!(
            str_list(&json!({"a": "x, y"}), "a").unwrap(),
            vec!["x", "y"]
        );
        assert_eq!(str_list(&json!({"a": ["x", " "]}), "a").unwrap(), vec!["x"]);
    }
}
