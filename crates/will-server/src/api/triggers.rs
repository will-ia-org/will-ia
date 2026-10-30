//! Triggers universais: CRUD, histórico, teste e varredura do Gmail. Ver `docs/30`.

use super::{bad, ApiError, ApiResult, App};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use will_core::TriggerId;
use will_triggers::{TriggerInput, TriggersModule};

fn module(state: &App) -> Result<Arc<TriggersModule>, ApiError> {
    state.triggers.read().clone().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "triggers não iniciadas".into(),
        )
    })
}

fn tid(s: &str) -> Result<TriggerId, ApiError> {
    TriggerId::parse(s).map_err(|_| bad("id de trigger inválido"))
}

pub async fn list(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    let list = m.list().await.map_err(super::internal)?;
    Ok(Json(json!({
        "triggers": list,
        "enabled": m.enabled(),
        "gmail_available": m.has_mail_source(),
        "sources": [
            { "id": "whatsapp", "label": "WhatsApp", "fields": will_core::TriggerSource::WhatsApp.fields() },
            { "id": "gmail", "label": "E-mail (Gmail)", "fields": will_core::TriggerSource::Gmail.fields() },
        ],
    })))
}

pub async fn create(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
    Json(body): Json<TriggerInput>,
) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m.create(body, &me.username).await.map_err(bad)?;
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
    Json(body): Json<TriggerInput>,
) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m.update(tid(&id)?, body).await.map_err(bad)?;
    Ok(Json(json!(v)))
}

pub async fn delete(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.delete(tid(&id)?)
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn runs(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    let r = m.runs(tid(&id)?).await.map_err(super::internal)?;
    Ok(Json(json!({ "runs": r })))
}

/// Dispara com um evento de exemplo (ignora filtro/cooldown).
pub async fn test(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let m = module(&state)?;
    let v = m.test(tid(&id)?).await.map_err(bad)?;
    Ok(Json(json!(v)))
}

/// Varredura do Gmail agora (fora do intervalo).
pub async fn gmail_poll(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    if !m.has_mail_source() {
        return Err(bad("fonte Gmail indisponível"));
    }
    #[cfg(feature = "google")]
    {
        let src: Arc<dyn will_triggers::MailSource> =
            Arc::new(crate::modules::gmail_source(&state));
        let n = m.gmail_poll_once(&src).await;
        Ok(Json(json!({ "fired": n })))
    }
    #[cfg(not(feature = "google"))]
    {
        Err(bad("fonte Gmail indisponível"))
    }
}
