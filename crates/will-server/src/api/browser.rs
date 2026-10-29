//! Browser CDP: status do proxy, tráfego, lançamento/fechamento do navegador,
//! testes e instalação do agent-browser + skill. Ver `docs/25`.

use super::{bad, ApiError, ApiResult, App};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use will_browser::BrowserModule;

fn module(state: &App) -> Result<Arc<BrowserModule>, ApiError> {
    state.browser.read().clone().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Browser CDP não iniciado".into(),
        )
    })
}

pub async fn status(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    Ok(Json(json!(m.status().await)))
}

pub async fn detect(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    Ok(Json(json!({ "browsers": m.detect().await })))
}

#[derive(serde::Deserialize, Default)]
pub struct TrafficQuery {
    pub after: Option<u64>,
    pub limit: Option<usize>,
}

pub async fn traffic(State(state): State<App>, Query(q): Query<TrafficQuery>) -> ApiResult<Value> {
    let m = module(&state)?;
    let entries = m
        .traffic
        .since(q.after.unwrap_or(0), q.limit.unwrap_or(200).clamp(1, 1000));
    Ok(Json(json!({
        "entries": entries,
        "last_seq": m.traffic.last_seq(),
        "counters": m.traffic.counters(),
    })))
}

pub async fn traffic_clear(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.traffic.clear();
    Ok(Json(json!({ "ok": true })))
}

/// Sobe o navegador agora (se já há um respondendo, só confirma).
pub async fn launch(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.ensure_upstream().await.map_err(bad)?;
    Ok(Json(json!(m.status().await)))
}

pub async fn close(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.close_browser().await;
    Ok(Json(json!(m.status().await)))
}

pub async fn test_upstream(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    let up = m
        .probe_upstream(std::time::Duration::from_secs(3))
        .await
        .map_err(bad)?;
    // `version` mantém as chaves do `/json/version` do Chrome (a UI lê `Browser` e
    // `webSocketDebuggerUrl`) e acrescenta como o navegador foi reconhecido.
    Ok(Json(json!({
        "ok": true,
        "version": {
            "Browser": up.browser.clone().unwrap_or_default(),
            "webSocketDebuggerUrl": up.browser_ws,
            "authority": up.authority,
            "source": up.source,
            "http_discovery": up.http_discovery,
        },
        "upstream": up,
    })))
}

#[derive(serde::Deserialize, Default)]
pub struct NewTabBody {
    pub url: Option<String>,
}

pub async fn test_new_tab(
    State(state): State<App>,
    body: Option<Json<NewTabBody>>,
) -> ApiResult<Value> {
    let m = module(&state)?;
    let url = body
        .and_then(|b| b.0.url)
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "about:blank".into());
    let v = m.test_new_tab(&url).await.map_err(bad)?;
    Ok(Json(json!({ "ok": true, "target": v })))
}

pub async fn test_agent_browser(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    match m.test_agent_browser().await {
        Ok(out) => Ok(Json(json!({ "ok": true, "output": out }))),
        Err(out) => Ok(Json(json!({ "ok": false, "output": out }))),
    }
}

pub async fn install_status(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    let kb = m.kb_path();
    Ok(Json(json!(m.installer.status(kb.as_deref()).await)))
}

pub async fn install_job(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    Ok(Json(json!(m.installer.job())))
}

pub async fn install_agent_browser(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.installer.install_agent_browser().map_err(bad)?;
    Ok(Json(json!(m.installer.job())))
}

pub async fn install_chromium(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    m.installer.install_chromium().map_err(bad)?;
    Ok(Json(json!(m.installer.job())))
}

pub async fn install_skill(State(state): State<App>) -> ApiResult<Value> {
    let m = module(&state)?;
    let kb = m
        .kb_path()
        .ok_or_else(|| bad("pasta da base de conhecimento não configurada"))?;
    let path = m.installer.install_skill(&kb).await.map_err(bad)?;
    Ok(Json(
        json!({ "ok": true, "path": path, "job": m.installer.job() }),
    ))
}
