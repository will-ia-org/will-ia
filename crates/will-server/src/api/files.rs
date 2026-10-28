//! Arquivos entre app e servidor (`docs/26`, etapa 2): upload de anexos para
//! `<kb>/inbox/ui/`, leitura de arquivos da KB (miniaturas/downloads) e
//! navegação de pastas do servidor (escolher a KB sem diálogo local).

use super::{bad, ApiError, ApiResult, App};
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Tamanho máximo de um upload (bytes); o `DefaultBodyLimit` desta rota é maior que o global.
pub const UPLOAD_MAX: usize = 64 * 1024 * 1024;

#[derive(serde::Deserialize, Default)]
pub struct UploadQuery {
    pub name: String,
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.trim_matches('.').chars().take(120).collect()
}

/// `POST /api/uploads?name=<arquivo>` com o corpo bruto do arquivo. Grava em
/// `<kb>/inbox/ui/<carimbo>-<nome>` e devolve o anexo (caminho do servidor),
/// pronto para `POST /api/threads/{id}/messages`.
pub async fn upload(
    State(state): State<App>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Value> {
    let kb = state.kb_path().filter(|p| p.is_dir()).ok_or_else(|| {
        bad("Configure a pasta da base de conhecimento antes de anexar arquivos.")
    })?;
    let mut safe = sanitize(&q.name);
    if safe.is_empty() {
        safe = "arquivo".into();
    }
    let dir = kb.join("inbox").join("ui");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| super::internal(format!("criar pasta de anexos: {e}")))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut dest = dir.join(format!("{stamp}-{safe}"));
    let mut n = 1;
    while dest.exists() {
        n += 1;
        dest = dir.join(format!("{stamp}-{n}-{safe}"));
    }
    tokio::fs::write(&dest, &body)
        .await
        .map_err(|e| super::internal(format!("gravar anexo: {e}")))?;
    let mime = mime_guess::from_path(&dest)
        .first_or_octet_stream()
        .to_string();
    Ok(Json(json!({
        "name": safe,
        "mime": mime,
        "path": dest.to_string_lossy(),
        "size": body.len(),
    })))
}

#[derive(serde::Deserialize, Default)]
pub struct FileQuery {
    pub path: String,
}

/// Caminho canônico dentro da KB, ou erro. Bloqueia `..`, links para fora e
/// qualquer coisa fora da base (o app só precisa de anexos e capturas).
async fn inside_kb(state: &App, path: &str) -> Result<PathBuf, ApiError> {
    let kb = state
        .kb_path()
        .filter(|p| p.is_dir())
        .ok_or_else(|| bad("pasta da base de conhecimento não configurada"))?;
    let kb_c = tokio::fs::canonicalize(&kb)
        .await
        .map_err(|e| super::internal(e.to_string()))?;
    let p = tokio::fs::canonicalize(Path::new(path))
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "arquivo não encontrado".into()))?;
    if !p.starts_with(&kb_c) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "fora da base de conhecimento".into(),
        ));
    }
    Ok(p)
}

/// `GET /api/files?path=<caminho no servidor>`: conteúdo de um arquivo da KB.
pub async fn file(
    State(state): State<App>,
    Query(q): Query<FileQuery>,
) -> Result<Response, ApiError> {
    let p = inside_kb(&state, &q.path).await?;
    let meta = tokio::fs::metadata(&p)
        .await
        .map_err(|e| super::internal(e.to_string()))?;
    if !meta.is_file() {
        return Err(bad("não é um arquivo"));
    }
    if meta.len() as usize > UPLOAD_MAX {
        return Err(bad("arquivo grande demais para baixar pelo app"));
    }
    let bytes = tokio::fs::read(&p)
        .await
        .map_err(|e| super::internal(e.to_string()))?;
    let mime = mime_guess::from_path(&p)
        .first_or_octet_stream()
        .to_string();
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("arquivo")
        .to_string();
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", sanitize(&name)),
        )
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

#[derive(serde::Deserialize, Default)]
pub struct BrowseQuery {
    pub path: Option<String>,
}

/// `GET /api/kb/browse?path=`: subpastas de um diretório do **servidor**, para
/// o app escolher a pasta da KB sem diálogo local. Sem `path`: a pasta home.
pub async fn browse(State(state): State<App>, Query(q): Query<BrowseQuery>) -> ApiResult<Value> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| state.paths.data_dir.clone());
    let dir = match q.path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => home.clone(),
    };
    let dir = tokio::fs::canonicalize(&dir)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "pasta não encontrada".into()))?;
    if !dir.is_dir() {
        return Err(bad("não é uma pasta"));
    }
    let mut dirs = vec![];
    let mut rd = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| super::internal(e.to_string()))?;
    while let Ok(Some(e)) = rd.next_entry().await {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if e.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            dirs.push(json!({ "name": name, "path": e.path().to_string_lossy() }));
        }
    }
    dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let writable = tokio::fs::metadata(&dir)
        .await
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false);
    Ok(Json(json!({
        "path": dir.to_string_lossy(),
        "parent": dir.parent().map(|p| p.to_string_lossy().to_string()),
        "home": home.to_string_lossy(),
        "writable": writable,
        "dangerous": will_platform::is_dangerous_kb(&dir),
        "dirs": dirs,
    })))
}
