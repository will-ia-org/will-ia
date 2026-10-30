//! `/api/telemetry/*`: status do envio OTLP, evento de teste e a configuração
//! que o app usa para enviar os próprios logs (admin).

use super::{ApiResult, App};
use axum::extract::State;
use axum::Json;
use secrecy::ExposeSecret;
use serde_json::{json, Value};

pub async fn status(State(state): State<App>) -> ApiResult<Value> {
    let s = state.settings().telemetry;
    let Some(h) = crate::logging::telemetry() else {
        return Ok(Json(json!({ "enabled": false, "configured": false })));
    };
    let st = h.stats();
    Ok(Json(json!({
        "enabled": s.enabled,
        "configured": h.enabled(),
        "provider": s.provider,
        "endpoint": s.endpoint,
        "dataset": s.dataset,
        "instance_id": h.instance.instance_id,
        "label": crate::telemetry::effective_label(&s),
        "public_ip": crate::telemetry::public_ip(),
        "host": h.instance.host,
        "sent": st.sent,
        "failed": st.failed,
        "spooled": st.spooled,
        "spool_bytes": st.spool_bytes,
        "last_sent_at": st.last_sent_at.map(|t| t.to_rfc3339()),
        "last_error": st.last_error,
    })))
}

pub async fn test(State(_state): State<App>) -> ApiResult<Value> {
    let Some(h) = crate::logging::telemetry() else {
        return Ok(Json(
            json!({ "ok": false, "message": "telemetria indisponível" }),
        ));
    };
    Ok(Json(match h.send_test().await {
        Ok(m) => json!({ "ok": true, "message": m }),
        Err(e) => json!({ "ok": false, "message": e }),
    }))
}

/// O que o app precisa para enviar os próprios logs ao mesmo destino.
pub async fn client_config(State(state): State<App>) -> ApiResult<Value> {
    let s = state.settings().telemetry;
    let token = state
        .secrets
        .get(crate::telemetry::TOKEN_KEY)
        .await
        .ok()
        .flatten();
    let cfg = if s.ship_app_logs {
        crate::telemetry::ship_config(&s, token)
    } else {
        None
    };
    let instance_id = crate::logging::telemetry()
        .map(|h| h.instance.instance_id.clone())
        .unwrap_or_default();
    Ok(Json(match cfg {
        Some(c) => json!({
            "enabled": true,
            "logs_url": c.logs_url,
            "traces_url": c.traces_url,
            "headers": c.headers,
            "bearer": c.bearer.as_ref().map(|b| b.expose_secret().to_string()),
            "level": s.level,
            "label": crate::telemetry::effective_label(&s),
            "instance_id": instance_id,
        }),
        None => json!({ "enabled": false, "instance_id": instance_id }),
    }))
}
