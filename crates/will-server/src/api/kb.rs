use super::{bad, ApiResult, App};
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

pub async fn status(State(state): State<App>) -> ApiResult<Value> {
    Ok(Json(json!(
        crate::kb::status(state.kb_path().as_deref()).await
    )))
}

pub async fn scaffold(State(state): State<App>) -> ApiResult<Value> {
    let kb = state
        .kb_path()
        .ok_or_else(|| bad("pasta da base de conhecimento não configurada"))?;
    let created = crate::kb::scaffold(&kb).map_err(bad)?;
    let _ = crate::mcp_json::write(&state).await;
    crate::modules::refresh_engine(&state, false).await;
    Ok(Json(json!({ "created": created })))
}

pub async fn open(State(state): State<App>) -> ApiResult<Value> {
    let kb = state
        .kb_path()
        .ok_or_else(|| bad("pasta da base de conhecimento não configurada"))?;
    will_platform::open_path(&kb).map_err(bad)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn mcp_json(State(state): State<App>) -> ApiResult<Value> {
    let path = crate::mcp_json::write(&state).await.map_err(bad)?;
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            Ok(Json(json!({ "path": p, "json": v, "text": text })))
        }
        None => Ok(Json(
            json!({ "path": null, "json": crate::mcp_json::render(&state.settings(), &state.mcp_url.read(), state.settings().mcp.enabled) }),
        )),
    }
}
