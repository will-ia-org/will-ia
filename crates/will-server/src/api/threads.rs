use super::{bad, ApiError, ApiResult, App};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use will_core::{Origin, ThreadHost, ThreadId, TurnId};

fn tid(s: &str) -> Result<ThreadId, ApiError> {
    ThreadId::parse(s).map_err(|_| bad("id de thread inválido"))
}

pub async fn list(State(state): State<App>) -> ApiResult<Value> {
    let host = state.host();
    let t = host.list_threads().await.map_err(super::internal)?;
    Ok(Json(json!(t)))
}

#[derive(serde::Deserialize, Default)]
pub struct CreateBody {
    pub title: Option<String>,
}

pub async fn create(State(state): State<App>, body: Option<Json<CreateBody>>) -> ApiResult<Value> {
    let title = body
        .and_then(|b| b.0.title)
        .filter(|t| !t.trim().is_empty());
    let t = state
        .host()
        .ensure_thread(&Origin::Ui, title.as_deref())
        .await
        .map_err(super::internal)?;
    Ok(Json(json!(t)))
}

pub async fn get_one(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let t = state
        .host()
        .get_thread(tid(&id)?)
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e))?;
    Ok(Json(json!(t)))
}

#[derive(serde::Deserialize)]
pub struct Limit {
    pub limit: Option<i64>,
}

pub async fn turns(
    State(state): State<App>,
    Path(id): Path<String>,
    Query(q): Query<Limit>,
) -> ApiResult<Value> {
    let rows = state
        .storage
        .list_turns(tid(&id)?, q.limit.unwrap_or(100).min(1000))
        .await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            // Turno absorvido por outro (mensagem enviada no meio do turno):
            // não tem resposta própria; `merged_into` aponta o turno pai.
            let merged = r.error_kind.as_deref() == Some("merged");
            let merged_into = merged
                .then(|| r.result_text.as_deref().and_then(merged_parent))
                .flatten();
            let text = if merged { None } else { r.result_text.clone() };
            // `prompt_display`: sem as notas de anexo; `attachments`: os gravados
            // no turno ou, em turnos antigos, os recuperados da própria nota.
            let (prompt_display, legacy) = will_core::split_attachment_notes(&r.prompt);
            let attachments = if r.attachments.is_empty() { legacy } else { r.attachments.clone() };
            json!({
                "id": r.id, "thread_id": r.thread_id, "author": r.author, "prompt": r.prompt, "prompt_display": prompt_display, "attachments": attachments, "started_at": r.started_at, "finished_at": r.finished_at,
                "result": text.as_ref().map(|t| json!({"text": t, "is_error": r.is_error, "cost_usd": r.cost_usd, "duration_ms": r.duration_ms, "model": r.model})),
                "text": text, "is_error": r.is_error, "error_kind": r.error_kind, "merged_into": merged_into, "cost_usd": r.cost_usd, "duration_ms": r.duration_ms, "model": r.model,
                "error": if r.is_error { text.clone() } else { None },
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

/// Extrai o id do turno pai gravado num turno mesclado. Aceita o formato atual
/// (só o uuid) e o antigo ("(incorporada ao turno <uuid>)").
fn merged_parent(text: &str) -> Option<TurnId> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|t| !t.is_empty())
        .find_map(|t| TurnId::parse(t).ok())
}

pub async fn turn_events(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let turn = TurnId::parse(&id).map_err(|_| bad("id de turno inválido"))?;
    Ok(Json(json!(state.storage.list_turn_events(turn).await?)))
}

#[derive(serde::Deserialize)]
pub struct SendBody {
    pub text: String,
    /// Caminhos locais de arquivos para anexar. Cada um é copiado para
    /// `<kb>/inbox/ui/` e entra no prompt como nota de anexo (igual ao Telegram).
    #[serde(default)]
    pub attachments: Vec<String>,
}

/// Nome de arquivo seguro para a pasta de anexos (mesma regra do Telegram).
fn sanitize_filename(name: &str) -> String {
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
    let s = s.trim_matches('.').to_string();
    let count = s.chars().count();
    if count > 80 {
        s.chars().skip(count - 80).collect()
    } else {
        s
    }
}

/// Copia um arquivo escolhido na UI para `<kb>/inbox/ui/` e descreve o anexo.
/// Arquivos que já estão em `<kb>/inbox/` (uploads do app) não são copiados.
async fn stage_attachment(
    kb: &std::path::Path,
    src: &str,
) -> Result<will_core::Attachment, String> {
    let src_path = std::path::Path::new(src);
    let meta = tokio::fs::metadata(src_path)
        .await
        .map_err(|e| format!("{src}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{src}: não é um arquivo"));
    }
    let name = src_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("arquivo")
        .to_string();
    let dir = kb.join("inbox").join("ui");
    // Já está no inbox da KB (ex.: veio por `POST /api/uploads`): usa como está.
    if let (Ok(canon), Ok(inbox)) = (
        tokio::fs::canonicalize(src_path).await,
        tokio::fs::canonicalize(kb.join("inbox")).await,
    ) {
        if canon.starts_with(&inbox) {
            return Ok(will_core::Attachment {
                mime: mime_guess::from_path(&canon)
                    .first_or_octet_stream()
                    .to_string(),
                name,
                path: canon.to_string_lossy().to_string(),
                size: meta.len(),
            });
        }
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("criar pasta de anexos: {e}"))?;
    let mut safe = sanitize_filename(&name);
    if safe.is_empty() {
        safe = "arquivo".into();
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut dest = dir.join(format!("{stamp}-{safe}"));
    let mut n = 1;
    while dest.exists() {
        n += 1;
        dest = dir.join(format!("{stamp}-{n}-{safe}"));
    }
    tokio::fs::copy(src_path, &dest)
        .await
        .map_err(|e| format!("copiar {name}: {e}"))?;
    Ok(will_core::Attachment {
        mime: mime_guess::from_path(&name)
            .first_raw()
            .unwrap_or_default()
            .to_string(),
        name,
        path: dest.display().to_string(),
        size: meta.len(),
    })
}

pub async fn send(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
    Path(id): Path<String>,
    Json(body): Json<SendBody>,
) -> ApiResult<Value> {
    let text = body.text.trim();
    if text.is_empty() && body.attachments.is_empty() {
        return Err(bad("mensagem vazia"));
    }
    let t = tid(&id)?;
    let mut atts = Vec::new();
    if !body.attachments.is_empty() {
        let kb = state.kb_path().filter(|p| p.is_dir()).ok_or_else(|| {
            bad("Configure a pasta da base de conhecimento antes de anexar arquivos.")
        })?;
        for src in &body.attachments {
            atts.push(stage_attachment(&kb, src).await.map_err(bad)?);
        }
    }
    let mut prompt = text.to_string();
    for a in &atts {
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(&will_core::attachment_note(a, "pelo app"));
    }
    let mid_turn = state.engine.pending(t) > 0;
    tracing::debug!(thread_id = %t, user = %me.username, attachments = atts.len(), mid_turn, "mensagem recebida pela API");
    let turn = state
        .host()
        .send_message_from(t, &prompt, &atts, Some(&me.author()))
        .await
        .map_err(bad)?;
    Ok(Json(json!({ "turn_id": turn, "mid_turn": mid_turn })))
}

pub async fn restart(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    state
        .host()
        .restart_thread(tid(&id)?)
        .await
        .map_err(super::internal)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn reset(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    state
        .host()
        .reset_thread(tid(&id)?)
        .await
        .map_err(super::internal)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn interrupt(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    state
        .host()
        .interrupt_thread(tid(&id)?)
        .await
        .map_err(super::internal)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
pub struct RenameBody {
    pub title: String,
}

pub async fn rename(
    State(state): State<App>,
    Path(id): Path<String>,
    Json(b): Json<RenameBody>,
) -> ApiResult<Value> {
    let t = tid(&id)?;
    state.storage.rename_thread(t, b.title.trim()).await?;
    tracing::info!(thread_id = %t, "thread renomeada pelo usuário");
    if let Ok(th) = state.host().get_thread(t).await {
        state
            .bus
            .emit(will_core::Event::ThreadUpdated { thread: th });
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
pub struct DeleteQuery {
    pub purge: Option<bool>,
}

pub async fn delete_one(
    State(state): State<App>,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<Value> {
    state
        .host()
        .delete_thread(tid(&id)?, q.purge.unwrap_or(false))
        .await
        .map_err(super::internal)?;
    Ok(Json(json!({ "ok": true })))
}
