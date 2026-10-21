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
            json!({
                "id": r.id, "thread_id": r.thread_id, "prompt": r.prompt, "started_at": r.started_at, "finished_at": r.finished_at,
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
}

pub async fn send(
    State(state): State<App>,
    Path(id): Path<String>,
    Json(body): Json<SendBody>,
) -> ApiResult<Value> {
    let text = body.text.trim();
    if text.is_empty() {
        return Err(bad("mensagem vazia"));
    }
    let t = tid(&id)?;
    let mid_turn = state.engine.pending(t) > 0;
    let turn = state.host().send_message(t, text).await.map_err(bad)?;
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
