//! Tarefas (`docs/31`): CRUD, concluir/reabrir. O estado vem calculado no `TaskView`.

use super::{bad, ApiError, ApiResult, App};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use will_core::TaskId;
use will_tasks::{TaskInput, TasksModule};

fn module(state: &App) -> Result<Arc<TasksModule>, ApiError> {
    state.tasks.read().clone().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "módulo de tarefas não iniciado".into(),
        )
    })
}

fn tid(s: &str) -> Result<TaskId, ApiError> {
    TaskId::parse(s).map_err(|_| bad("id de tarefa inválido"))
}

#[derive(serde::Deserialize, Default)]
pub struct ListQuery {
    /// `pending` (padrão) | `running` | `overdue` | `upcoming` | `done` | `all`.
    pub filter: Option<String>,
}

pub async fn list(State(state): State<App>, Query(q): Query<ListQuery>) -> ApiResult<Value> {
    let m = module(&state)?;
    let filter = q.filter.as_deref().unwrap_or("pending");
    let list = m.list(filter).await.map_err(bad)?;
    let zone = m.zone();
    Ok(Json(json!({
        "tasks": list,
        "enabled": m.enabled(),
        "timezone": zone.name(),
        "offset_minutes": zone.offset_minutes(),
        "now": chrono::Utc::now(),
    })))
}

pub async fn create(State(state): State<App>, Json(body): Json<TaskInput>) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m.create(body, "user").await.map_err(bad)?;
    Ok(Json(json!(v)))
}

pub async fn get_one(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m
        .get(tid(&id)?)
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e))?;
    Ok(Json(json!(v)))
}

pub async fn update(
    State(state): State<App>,
    Path(id): Path<String>,
    Json(body): Json<TaskInput>,
) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m.update(tid(&id)?, body).await.map_err(bad)?;
    Ok(Json(json!(v)))
}

pub async fn delete(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.delete(tid(&id)?).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn complete(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m.set_done(tid(&id)?, true).await.map_err(bad)?;
    Ok(Json(json!(v)))
}

pub async fn reopen(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m.set_done(tid(&id)?, false).await.map_err(bad)?;
    Ok(Json(json!(v)))
}
