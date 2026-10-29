//! Operações do Drive (Drive API v3): listar/buscar/ler, baixar/exportar, criar,
//! enviar, atualizar, lixeira, compartilhar e drives compartilhados.

use super::query::{
    build_query, download_filename, export_choice, is_google_type, mime_alias, normalize_file,
    normalize_permission, permission_request, str_list, FILE_FIELDS, MIME_FOLDER, PERM_FIELDS,
};
use crate::calendar::enc;
use crate::client::UploadRequest;
use crate::fs::{confined_dir, relative_to_kb, resolve_in_kb, unique_path};
use crate::GoogleModule;
use serde_json::{json, Value};
use std::path::Path;
use will_core::settings::DriveSettings;
use will_core::tools::{arg_opt_bool, arg_opt_str, arg_opt_u64, arg_str};
use will_core::{ToolError, ToolOutput};
use will_storage::google::GoogleAccount;

pub(super) const BASE: &str = "drive/v3";
pub(super) const UPLOAD_BASE: &str = "upload/drive/v3/files";
pub(super) const MAX_PAGE: u64 = 200;
/// Acima disso o upload é resumable (recomendação da Google: multipart ≤ 5 MB).
const MULTIPART_MAX: u64 = 5 * 1024 * 1024;
/// Limite da Google para `files.export`.
const EXPORT_MAX: u64 = 10 * 1024 * 1024;

fn all_drives() -> (&'static str, String) {
    ("supportsAllDrives", "true".into())
}

fn file_fields() -> (&'static str, String) {
    ("fields", FILE_FIELDS.into())
}

fn require_write(cfg: &DriveSettings) -> Result<(), ToolError> {
    if cfg.allow_write {
        Ok(())
    } else {
        Err(ToolError::Unavailable(
            "escrita no Drive desligada (Configurações → Google → Drive → Permitir escrever)"
                .into(),
        ))
    }
}

fn require_share(cfg: &DriveSettings) -> Result<(), ToolError> {
    if cfg.allow_share {
        Ok(())
    } else {
        Err(ToolError::Unavailable(
            "compartilhamento desligado (Configurações → Google → Drive → Permitir compartilhar)"
                .into(),
        ))
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

fn guess_mime(path: &Path, explicit: Option<&str>) -> String {
    match explicit.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => mime_alias(m),
        None => mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_string(),
    }
}

async fn get_meta(m: &GoogleModule, acc: &GoogleAccount, id: &str) -> Result<Value, ToolError> {
    let v = m
        .client
        .get(
            acc,
            &format!("{BASE}/files/{}", enc(id)),
            &[all_drives(), file_fields()],
        )
        .await?;
    m.remember_drive_name(&v);
    Ok(v)
}

fn out_file(m: &GoogleModule, acc: &GoogleAccount, v: &Value, extra: Value) -> ToolOutput {
    m.remember_drive_name(v);
    let mut f = normalize_file(v);
    if let Some(obj) = extra.as_object() {
        for (k, val) in obj {
            f[k] = val.clone();
        }
    }
    f["account"] = json!(acc.email);
    ToolOutput::json(&f)
}

// ----- leitura -----

async fn list_files(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
    q: String,
) -> Result<ToolOutput, ToolError> {
    let page_size = arg_opt_u64(args, "max_results")
        .unwrap_or(50)
        .clamp(1, MAX_PAGE);
    let mut query: Vec<(&str, String)> = vec![
        ("q", q.clone()),
        ("pageSize", page_size.to_string()),
        (
            "fields",
            format!("nextPageToken,incompleteSearch,files({FILE_FIELDS})"),
        ),
        all_drives(),
        ("includeItemsFromAllDrives", "true".into()),
    ];
    if let Some(t) = arg_opt_str(args, "page_token") {
        query.push(("pageToken", t.to_string()));
    }
    let order = arg_opt_str(args, "order_by")
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .unwrap_or("folder,modifiedTime desc,name");
    query.push(("orderBy", order.to_string()));
    if let Some(d) = arg_opt_str(args, "drive_id") {
        query.push(("corpora", "drive".into()));
        query.push(("driveId", d.to_string()));
    }
    let v = m.client.get(acc, &format!("{BASE}/files"), &query).await?;
    let files: Vec<Value> = v
        .get("files")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .map(|f| {
                    m.remember_drive_name(f);
                    normalize_file(f)
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ToolOutput::json(&json!({
        "count": files.len(),
        "files": files,
        "next_page_token": v.get("nextPageToken"),
        "incomplete_search": v.get("incompleteSearch").and_then(|x| x.as_bool()).unwrap_or(false),
        "query": q,
        "account": acc.email,
    })))
}

pub(super) async fn list(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let folder = arg_opt_str(args, "folder_id")
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .unwrap_or("root");
    let mut a = json!({"parent": folder});
    if let Some(t) = args.get("trashed") {
        a["trashed"] = t.clone();
    }
    if let Some(mt) = args.get("mime_type") {
        a["mime_type"] = mt.clone();
    }
    let q = build_query(&a)?;
    list_files(m, acc, args, q).await
}

pub(super) async fn search(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let q = build_query(args)?;
    list_files(m, acc, args, q).await
}

pub(super) async fn get(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let id = arg_str(args, "file_id")?;
    let v = get_meta(m, acc, id).await?;
    let mut extra = json!({});
    if arg_opt_bool(args, "include_permissions").unwrap_or(true) {
        match m
            .client
            .get(
                acc,
                &format!("{BASE}/files/{}/permissions", enc(id)),
                &[
                    all_drives(),
                    ("fields", format!("permissions({PERM_FIELDS})")),
                ],
            )
            .await
        {
            Ok(p) => {
                extra["permissions"] = json!(p
                    .get("permissions")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().map(normalize_permission).collect::<Vec<_>>())
                    .unwrap_or_default());
            }
            Err(e) => extra["permissions_error"] = json!(ToolError::from(e).to_string()),
        }
    }
    Ok(out_file(m, acc, &v, extra))
}

pub(super) async fn download(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().drive;
    let id = arg_str(args, "file_id")?;
    let max_bytes = cfg.max_download_mb.max(1) as u64 * 1024 * 1024;
    let meta = get_meta(m, acc, id).await?;
    let name = meta
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("arquivo");
    let mime = meta
        .get("mimeType")
        .and_then(|x| x.as_str())
        .unwrap_or("application/octet-stream")
        .to_string();
    if mime == MIME_FOLDER {
        return Err(ToolError::InvalidArgs(
            "é uma pasta; use drive_list para ver o conteúdo".into(),
        ));
    }
    let export = export_choice(&mime, &cfg, arg_opt_str(args, "export_format"));
    if export.is_none() && is_google_type(&mime) {
        return Err(ToolError::InvalidArgs(format!(
            "tipo {mime} não pode ser baixado nem exportado"
        )));
    }
    let size = meta
        .get("size")
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if export.is_none() && size > max_bytes {
        return Err(ToolError::Forbidden(format!(
            "arquivo de {} MB excede o limite de {} MB",
            mb(size),
            cfg.max_download_mb
        )));
    }
    let dir = confined_dir(&m.kb_path, &cfg.download_dir, "inbox/drive");
    let filename = match arg_opt_str(args, "save_as").map(str::trim) {
        Some(s) if !s.is_empty() => download_filename(s, export.as_deref()),
        _ => download_filename(name, export.as_deref()),
    };
    let dest = unique_path(&dir, &filename);
    let (path, query): (String, Vec<(&str, String)>) = match &export {
        Some(em) => (
            format!("{BASE}/files/{}/export", enc(id)),
            vec![("mimeType", em.clone())],
        ),
        None => (
            format!("{BASE}/files/{}", enc(id)),
            vec![("alt", "media".into()), all_drives()],
        ),
    };
    let limit = if export.is_some() {
        max_bytes.min(EXPORT_MAX)
    } else {
        max_bytes
    };
    let (written, ctype) = m
        .client
        .download_to_file(acc, &path, &query, &dest, limit)
        .await?;
    Ok(ToolOutput::json(&json!({
        "path": dest.to_string_lossy(),
        "relative_path": relative_to_kb(&m.kb_path, &dest),
        "filename": dest.file_name().map(|f| f.to_string_lossy().into_owned()),
        "mime": export.clone().or(ctype).unwrap_or(mime.clone()),
        "size": written,
        "file_id": id,
        "name": name,
        "exported": export.is_some(),
        "source_mime": mime,
        "account": acc.email,
    })))
}

// ----- escrita -----

pub(super) async fn create_folder(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    require_write(&m.config().drive)?;
    let name = arg_str(args, "name")?.trim();
    if name.is_empty() {
        return Err(ToolError::InvalidArgs("name vazio".into()));
    }
    let mut body = json!({"name": name, "mimeType": MIME_FOLDER});
    if let Some(p) = arg_opt_str(args, "parent_id")
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        body["parents"] = json!([p]);
    }
    if let Some(d) = arg_opt_str(args, "description") {
        body["description"] = json!(d);
    }
    let v = m
        .client
        .post(
            acc,
            &format!("{BASE}/files"),
            &[all_drives(), file_fields()],
            &body,
        )
        .await?;
    Ok(out_file(m, acc, &v, json!({})))
}

/// Metadados comuns de criação (`name`, `parents`, `description`, `mimeType` de conversão).
fn create_metadata(args: &Value, name: &str, convert_to: Option<&str>) -> Value {
    let mut meta = json!({"name": name});
    if let Some(p) = arg_opt_str(args, "parent_id")
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        meta["parents"] = json!([p]);
    }
    if let Some(d) = arg_opt_str(args, "description") {
        meta["description"] = json!(d);
    }
    if let Some(c) = convert_to.map(str::trim).filter(|c| !c.is_empty()) {
        meta["mimeType"] = json!(mime_alias(c));
    }
    meta
}

pub(super) async fn upload(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().drive;
    require_write(&cfg)?;
    let p = arg_str(args, "path")?;
    let file = resolve_in_kb(&m.kb_path, p, "arquivo")?;
    let size = tokio::fs::metadata(&file)
        .await
        .map_err(|e| ToolError::Failed(format!("ler arquivo: {e}")))?
        .len();
    let max_bytes = cfg.max_upload_mb.max(1) as u64 * 1024 * 1024;
    if size > max_bytes {
        return Err(ToolError::Forbidden(format!(
            "arquivo de {} MB excede o limite de {} MB",
            mb(size),
            cfg.max_upload_mb
        )));
    }
    let default_name = file
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arquivo".into());
    let name = arg_opt_str(args, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&default_name);
    let content_type = guess_mime(&file, arg_opt_str(args, "mime_type"));
    let meta = create_metadata(args, name, arg_opt_str(args, "convert_to"));
    let query = [
        ("uploadType", "multipart".to_string()),
        all_drives(),
        file_fields(),
    ];
    let req = UploadRequest {
        method: reqwest::Method::POST,
        path: UPLOAD_BASE,
        query: &query,
        metadata: &meta,
        content_type: &content_type,
    };
    let (v, method) = if size <= MULTIPART_MAX {
        let bytes = tokio::fs::read(&file)
            .await
            .map_err(|e| ToolError::Failed(format!("ler arquivo: {e}")))?;
        (
            m.client.upload_multipart(acc, &req, bytes).await?,
            "multipart",
        )
    } else {
        (
            m.client.upload_resumable(acc, &req, &file).await?,
            "resumable",
        )
    };
    Ok(out_file(
        m,
        acc,
        &v,
        json!({"uploaded_bytes": size, "upload_method": method, "source_path": file.to_string_lossy()}),
    ))
}

pub(super) async fn create_file(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().drive;
    require_write(&cfg)?;
    let name = arg_str(args, "name")?.trim();
    if name.is_empty() {
        return Err(ToolError::InvalidArgs("name vazio".into()));
    }
    let content = arg_str(args, "content")?;
    let max_bytes = cfg.max_upload_mb.max(1) as u64 * 1024 * 1024;
    if content.len() as u64 > max_bytes {
        return Err(ToolError::Forbidden(format!(
            "conteúdo excede o limite de {} MB",
            cfg.max_upload_mb
        )));
    }
    let content_type = match arg_opt_str(args, "mime_type").map(str::trim) {
        Some(mt) if !mt.is_empty() => mime_alias(mt),
        _ => {
            let guessed = mime_guess::from_path(name)
                .first_raw()
                .unwrap_or("text/plain");
            if guessed.starts_with("text/") || guessed.ends_with("json") {
                format!("{guessed}; charset=UTF-8")
            } else {
                "text/plain; charset=UTF-8".into()
            }
        }
    };
    let meta = create_metadata(args, name, arg_opt_str(args, "convert_to"));
    let query = [
        ("uploadType", "multipart".to_string()),
        all_drives(),
        file_fields(),
    ];
    let req = UploadRequest {
        method: reqwest::Method::POST,
        path: UPLOAD_BASE,
        query: &query,
        metadata: &meta,
        content_type: &content_type,
    };
    let v = m
        .client
        .upload_multipart(acc, &req, content.as_bytes().to_vec())
        .await?;
    Ok(out_file(
        m,
        acc,
        &v,
        json!({"uploaded_bytes": content.len()}),
    ))
}

pub(super) async fn update(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().drive;
    require_write(&cfg)?;
    let id = arg_str(args, "file_id")?;
    let mut meta = json!({});
    if let Some(n) = arg_opt_str(args, "name")
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        meta["name"] = json!(n);
    }
    if let Some(d) = arg_opt_str(args, "description") {
        meta["description"] = json!(d);
    }
    if let Some(s) = arg_opt_bool(args, "starred") {
        meta["starred"] = json!(s);
    }
    let mut query: Vec<(&str, String)> = vec![all_drives(), file_fields()];
    if let Some(p) = str_list(args, "add_parents").filter(|p| !p.is_empty()) {
        query.push(("addParents", p.join(",")));
    }
    if let Some(p) = str_list(args, "remove_parents").filter(|p| !p.is_empty()) {
        query.push(("removeParents", p.join(",")));
    }
    let content_path = arg_opt_str(args, "content_path")
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let has_meta = meta.as_object().is_some_and(|o| !o.is_empty()) || query.len() > 2;
    if !has_meta && content_path.is_none() {
        return Err(ToolError::InvalidArgs(
            "nada a alterar: informe name, description, starred, add_parents/remove_parents ou content_path"
                .into(),
        ));
    }
    let v = match content_path {
        None => {
            m.client
                .patch(acc, &format!("{BASE}/files/{}", enc(id)), &query, &meta)
                .await?
        }
        Some(p) => {
            let file = resolve_in_kb(&m.kb_path, p, "arquivo")?;
            let size = tokio::fs::metadata(&file)
                .await
                .map_err(|e| ToolError::Failed(format!("ler arquivo: {e}")))?
                .len();
            let max_bytes = cfg.max_upload_mb.max(1) as u64 * 1024 * 1024;
            if size > max_bytes {
                return Err(ToolError::Forbidden(format!(
                    "arquivo de {} MB excede o limite de {} MB",
                    mb(size),
                    cfg.max_upload_mb
                )));
            }
            let content_type = guess_mime(&file, arg_opt_str(args, "mime_type"));
            query.push(("uploadType", "multipart".into()));
            let path = format!("{UPLOAD_BASE}/{}", enc(id));
            let req = UploadRequest {
                method: reqwest::Method::PATCH,
                path: &path,
                query: &query,
                metadata: &meta,
                content_type: &content_type,
            };
            if size <= MULTIPART_MAX {
                let bytes = tokio::fs::read(&file)
                    .await
                    .map_err(|e| ToolError::Failed(format!("ler arquivo: {e}")))?;
                m.client.upload_multipart(acc, &req, bytes).await?
            } else {
                m.client.upload_resumable(acc, &req, &file).await?
            }
        }
    };
    Ok(out_file(
        m,
        acc,
        &v,
        json!({"content_replaced": content_path.is_some()}),
    ))
}

pub(super) async fn trash(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().drive;
    require_write(&cfg)?;
    let id = arg_str(args, "file_id")?;
    if arg_opt_bool(args, "permanent").unwrap_or(false) {
        if !cfg.allow_delete {
            return Err(ToolError::Unavailable(
                "exclusão definitiva desligada (Configurações → Google → Drive → Permitir excluir definitivamente)"
                    .into(),
            ));
        }
        m.client
            .delete(acc, &format!("{BASE}/files/{}", enc(id)), &[all_drives()])
            .await?;
        return Ok(ToolOutput::json(&json!({
            "ok": true, "file_id": id, "deleted": true, "account": acc.email,
        })));
    }
    let restore = arg_opt_bool(args, "restore").unwrap_or(false);
    let v = m
        .client
        .patch(
            acc,
            &format!("{BASE}/files/{}", enc(id)),
            &[all_drives(), file_fields()],
            &json!({"trashed": !restore}),
        )
        .await?;
    Ok(out_file(m, acc, &v, json!({"restored": restore})))
}

// ----- compartilhamento -----

pub(super) async fn list_permissions(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let id = arg_str(args, "file_id")?;
    let v = m
        .client
        .get_all_pages(
            acc,
            &format!("{BASE}/files/{}/permissions", enc(id)),
            &[
                all_drives(),
                (
                    "fields",
                    format!("nextPageToken,permissions({PERM_FIELDS})"),
                ),
                ("pageSize", "100".into()),
            ],
            "permissions",
            Some(500),
        )
        .await?;
    let perms: Vec<Value> = v.iter().map(normalize_permission).collect();
    Ok(ToolOutput::json(&json!({
        "file_id": id,
        "count": perms.len(),
        "permissions": perms,
        "account": acc.email,
    })))
}

pub(super) async fn share(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    require_share(&m.config().drive)?;
    let id = arg_str(args, "file_id")?;
    let (body, query) = permission_request(args)?;
    let v = m
        .client
        .post(
            acc,
            &format!("{BASE}/files/{}/permissions", enc(id)),
            &query,
            &body,
        )
        .await?;
    let link = m
        .client
        .get(
            acc,
            &format!("{BASE}/files/{}", enc(id)),
            &[all_drives(), ("fields", "webViewLink,name".into())],
        )
        .await
        .ok();
    Ok(ToolOutput::json(&json!({
        "file_id": id,
        "name": link.as_ref().and_then(|l| l.get("name")),
        "web_view_link": link.as_ref().and_then(|l| l.get("webViewLink")),
        "permission": normalize_permission(&v),
        "account": acc.email,
    })))
}

pub(super) async fn unshare(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    require_share(&m.config().drive)?;
    let id = arg_str(args, "file_id")?;
    let pid = arg_str(args, "permission_id")?;
    m.client
        .delete(
            acc,
            &format!("{BASE}/files/{}/permissions/{}", enc(id), enc(pid)),
            &[all_drives()],
        )
        .await?;
    Ok(ToolOutput::json(&json!({
        "ok": true, "file_id": id, "permission_id": pid, "account": acc.email,
    })))
}

pub(super) async fn list_shared_drives(
    m: &GoogleModule,
    acc: &GoogleAccount,
    _args: &Value,
) -> Result<ToolOutput, ToolError> {
    let v = m
        .client
        .get_all_pages(
            acc,
            &format!("{BASE}/drives"),
            &[
                ("pageSize", "100".into()),
                (
                    "fields",
                    "nextPageToken,drives(id,name,createdTime,hidden)".into(),
                ),
            ],
            "drives",
            Some(500),
        )
        .await?;
    let drives: Vec<Value> = v
        .iter()
        .map(|d| {
            json!({
                "id": d.get("id"),
                "name": d.get("name"),
                "created": d.get("createdTime"),
                "hidden": d.get("hidden").and_then(|h| h.as_bool()).unwrap_or(false),
            })
        })
        .collect();
    Ok(ToolOutput::json(&json!({
        "count": drives.len(),
        "drives": drives,
        "account": acc.email,
    })))
}
