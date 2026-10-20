//! API local (axum): autenticação por token de sessão e rotas. Não serve UI:
//! a interface é o app desktop (`will-desktop`), que chama estes handlers
//! diretamente; a API HTTP atende o modo headless, automação e a segunda instância.

pub mod engine;
pub mod integrations;
pub mod kb;
pub mod mcp;
pub mod system;
pub mod threads;
pub mod ws;

use crate::app::AppState;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use std::sync::Arc;

pub type App = Arc<AppState>;

/// Erro de API: `{ "error": "..." }`.
pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<String> for ApiError {
    fn from(s: String) -> Self {
        ApiError(StatusCode::BAD_REQUEST, s)
    }
}
impl From<will_storage::StorageError> for ApiError {
    fn from(e: will_storage::StorageError) -> Self {
        match e {
            will_storage::StorageError::NotFound => {
                ApiError(StatusCode::NOT_FOUND, "não encontrado".into())
            }
            other => ApiError(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        }
    }
}

pub fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
pub fn internal(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
}

pub type ApiResult<T> = Result<Json<T>, ApiError>;

fn token_from(req: &Request) -> Option<String> {
    if let Some(v) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return Some(t.trim().to_string());
        }
    }
    req.uri().query().and_then(|q| {
        q.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == "token").then(|| v.to_string())
        })
    })
}

async fn auth(State(state): State<App>, req: Request, next: Next) -> Response {
    // Dentro do `nest("/api")` o caminho chega sem o prefixo.
    let path = req.uri().path();
    if path == "/health" || path == "/api/health" {
        return next.run(req).await;
    }
    match token_from(&req) {
        Some(t) if t == state.ui_token => next.run(req).await,
        _ => ApiError(StatusCode::UNAUTHORIZED, "token de sessão inválido".into()).into_response(),
    }
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "não encontrado; a UI é o app desktop (will-ia)" })),
    )
        .into_response()
}

pub fn router(state: App) -> Router {
    let api = Router::new()
        .route("/health", get(system::health))
        .route("/status", get(system::status))
        .route(
            "/settings",
            get(system::get_settings).put(system::put_settings),
        )
        .route("/secrets", post(system::set_secret))
        .route("/secrets/{key}", delete(system::delete_secret))
        .route("/logs", get(system::logs))
        .route("/costs", get(system::costs))
        .route("/costs/summary", get(system::costs_summary))
        .route("/diagnostics", get(system::diagnostics))
        .route("/ui/show", post(system::ui_show))
        .route("/ui/screenshot", post(system::ui_screenshot))
        .route("/events", get(ws::events_ws))
        .route("/threads", get(threads::list).post(threads::create))
        .route(
            "/threads/{id}",
            get(threads::get_one)
                .patch(threads::rename)
                .delete(threads::delete_one),
        )
        .route("/threads/{id}/turns", get(threads::turns))
        .route("/threads/{id}/messages", post(threads::send))
        .route("/threads/{id}/restart", post(threads::restart))
        .route("/threads/{id}/reset", post(threads::reset))
        .route("/threads/{id}/interrupt", post(threads::interrupt))
        .route("/turns/{id}/events", get(threads::turn_events))
        .route(
            "/engine/tokens",
            get(engine::list_tokens).post(engine::add_token),
        )
        .route("/engine/tokens/{id}/test", post(engine::test_token))
        .route("/engine/tokens/{id}/activate", post(engine::activate_token))
        .route("/engine/tokens/{id}", delete(engine::delete_token))
        .route("/engine/check", get(engine::check))
        .route("/kb/status", get(kb::status))
        .route("/kb/scaffold", post(kb::scaffold))
        .route("/kb/open", post(kb::open))
        .route("/kb/mcp-json", get(kb::mcp_json))
        .route("/mcp/tools", get(mcp::tools))
        .route("/mcp/tools/{name}/policy", put(mcp::set_policy))
        .route("/mcp/calls", get(mcp::calls))
        .route("/approvals", get(mcp::approvals))
        .route("/approvals/history", get(mcp::approvals_history))
        .route("/approvals/{id}", post(mcp::decide))
        .route("/mcp/external", get(mcp::external_list))
        .route(
            "/mcp/external/import-claude",
            post(mcp::external_import_claude),
        )
        .route("/mcp/external/import-json", post(mcp::external_import_json))
        .route(
            "/mcp/external/{name}",
            put(mcp::external_put).delete(mcp::external_delete),
        )
        .route("/mcp/external/{name}/test", post(mcp::external_test))
        .route("/telegram/test", post(integrations::telegram_test))
        .route("/telegram/status", get(integrations::telegram_status))
        .route("/voice/start", post(integrations::voice_start))
        .route("/voice/stop", post(integrations::voice_stop))
        .route("/voice/toggle", post(integrations::voice_toggle))
        .route("/voice/status", get(integrations::voice_status))
        .route("/voice/transcripts", get(integrations::voice_transcripts))
        .route("/voice/audio", get(ws::voice_ws))
        .route("/whatsapp/status", get(integrations::wa_status))
        .route("/whatsapp/pair", post(integrations::wa_pair))
        .route("/whatsapp/unlink", post(integrations::wa_unlink))
        .route("/whatsapp/reconnect", post(integrations::wa_reconnect))
        .route(
            "/whatsapp/accounts/{id}",
            patch(integrations::wa_account_patch),
        )
        .route("/google/accounts", get(integrations::google_accounts))
        .route("/google/connect", post(integrations::google_connect))
        .route(
            "/google/accounts/{id}/reauthorize",
            post(integrations::google_reauthorize),
        )
        .route(
            "/google/accounts/{id}",
            delete(integrations::google_disconnect),
        )
        .route("/google/calendars", get(integrations::google_calendars))
        .route(
            "/google/calendars/refresh",
            post(integrations::google_calendars_refresh),
        )
        .route(
            "/google/calendars/{account}/{calendar_id}",
            put(integrations::google_calendar_flags),
        )
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth))
        .layer(tower_http::cors::CorsLayer::very_permissive())
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024));
    Router::new()
        .nest("/api", api)
        .fallback(not_found)
        .with_state(state)
}
