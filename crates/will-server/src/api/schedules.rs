//! Agendamentos (scheduler): CRUD, prévia de cron e disparo manual. Ver `docs/24`.

use super::{bad, ApiError, ApiResult, App};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use will_core::{ScheduleId, ThreadId};
use will_scheduler::{ScheduleInput, SchedulerModule};

fn module(state: &App) -> Result<Arc<SchedulerModule>, ApiError> {
    state.scheduler.read().clone().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "scheduler não iniciado".into(),
        )
    })
}

fn sid(s: &str) -> Result<ScheduleId, ApiError> {
    ScheduleId::parse(s).map_err(|_| bad("id de agendamento inválido"))
}

#[derive(serde::Deserialize, Default)]
pub struct ListQuery {
    pub thread_id: Option<String>,
}

pub async fn list(State(state): State<App>, Query(q): Query<ListQuery>) -> ApiResult<Value> {
    let m = module(&state)?;
    let thread = match q.thread_id.as_deref().filter(|s| !s.is_empty()) {
        Some(t) => Some(ThreadId::parse(t).map_err(|_| bad("thread_id inválido"))?),
        None => None,
    };
    let list = m.list(thread).await.map_err(super::internal)?;
    Ok(Json(json!({ "schedules": list, "enabled": m.enabled() })))
}

/// `thread_id` obrigatório no corpo (a UI escolhe a thread dona).
pub async fn create(State(state): State<App>, Json(body): Json<ScheduleInput>) -> ApiResult<Value> {
    let m = module(&state)?;
    let thread = body
        .thread_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("thread_id obrigatório"))?;
    let thread = ThreadId::parse(thread).map_err(|_| bad("thread_id inválido"))?;
    let v = m.create(body, thread, "user").await.map_err(bad)?;
    Ok(Json(json!(v)))
}

pub async fn get_one(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    let id = sid(&id)?;
    let v = m
        .get(id)
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e))?;
    let runs = m.runs(id).await.unwrap_or_default();
    Ok(Json(json!({ "schedule": v, "runs": runs })))
}

pub async fn update(
    State(state): State<App>,
    Path(id): Path<String>,
    Json(body): Json<ScheduleInput>,
) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m.update(sid(&id)?, body).await.map_err(bad)?;
    Ok(Json(json!(v)))
}

pub async fn delete(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.delete(sid(&id)?).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn run_now(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.run_now(sid(&id)?).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize, Default)]
pub struct PreviewQuery {
    pub cron: String,
    pub timezone: Option<String>,
    pub n: Option<usize>,
}

/// Próximas ocorrências e descrição de uma expressão (para a UI validar ao digitar).
pub async fn preview(State(state): State<App>, Query(q): Query<PreviewQuery>) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m
        .preview(&q.cron, q.timezone.as_deref(), q.n.unwrap_or(5))
        .map_err(bad)?;
    Ok(Json(v))
}
