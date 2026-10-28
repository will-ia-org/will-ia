//! API local (axum): autenticação por token de sessão e rotas. Não serve UI:
//! a interface é o app desktop (`will-desktop`), que chama estes handlers
//! diretamente; a API HTTP atende o modo headless, automação e a segunda instância.

pub mod auth;
pub mod browser;
pub mod engine;
pub mod files;
pub mod integrations;
pub mod kb;
pub mod mcp;
pub mod schedules;
pub mod system;
pub mod threads;
pub mod users;
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

/// Token bruto da requisição (para `logout`).
#[derive(Clone)]
pub struct BearerToken(pub String);

/// Autentica: token local da API (admin implícito desta máquina) ou sessão de
/// usuário. Põe `Principal` e `BearerToken` nas extensões da requisição.
async fn auth(State(state): State<App>, mut req: Request, next: Next) -> Response {
    // Dentro do `nest("/api")` o caminho chega sem o prefixo.
    let path = req.uri().path();
    if path == "/health"
        || path == "/api/health"
        || path == "/auth/login"
        || path == "/api/auth/login"
    {
        return next.run(req).await;
    }
    let Some(t) = token_from(&req) else {
        return ApiError(StatusCode::UNAUTHORIZED, "token de sessão ausente".into())
            .into_response();
    };
    let principal = if t == state.ui_token {
        Some(will_core::Principal::local())
    } else {
        match state
            .storage
            .session_user(&crate::auth::token_hash(&t))
            .await
        {
            Ok(Some(u)) => Some(crate::auth::principal_of(&u)),
            _ => None,
        }
    };
    let Some(p) = principal else {
        return ApiError(StatusCode::UNAUTHORIZED, "token de sessão inválido".into())
            .into_response();
    };
    req.extensions_mut().insert(p);
    req.extensions_mut().insert(BearerToken(t));
    next.run(req).await
}

/// Rotas de configuração: só administradores.
async fn require_admin(req: Request, next: Next) -> Response {
    match req.extensions().get::<will_core::Principal>() {
        Some(p) if p.is_admin() => next.run(req).await,
        _ => ApiError(StatusCode::FORBIDDEN, "só administradores".into()).into_response(),
    }
}

pub fn is_admin_path(method: &axum::http::Method, path: &str) -> bool {
    let p = path.trim_start_matches("/api");
    let m = method.as_str();
    (p == "/settings" && m == "PUT")
        || p.starts_with("/secrets")
        || p.starts_with("/engine/tokens")
        || p.starts_with("/mcp/script-token")
        || (p.starts_with("/mcp/tools/") && p.ends_with("/policy"))
        || p.starts_with("/mcp/external")
        || p.starts_with("/users")
        || p == "/kb/scaffold"
        || p.starts_with("/browser/install/")
        || p.starts_with("/google/connect")
        || p.starts_with("/google/accounts/")
        || p.starts_with("/google/calendars")
        || p.starts_with("/whatsapp/pair")
        || p.starts_with("/whatsapp/unlink")
        || p.starts_with("/whatsapp/reconnect")
        || p.starts_with("/whatsapp/accounts/")
        || p == "/kb/open"
        || p == "/kb/browse"
        || p == "/diagnostics"
        || p == "/browser/launch"
        || p == "/browser/close"
        || p == "/network/cert"
}

/// Aplica [`require_admin`] só nos caminhos de configuração.
async fn admin_gate(req: Request, next: Next) -> Response {
    if is_admin_path(req.method(), req.uri().path()) {
        require_admin(req, next).await
    } else {
        next.run(req).await
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
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/password", post(auth::change_password))
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", patch(users::patch).delete(users::delete))
        .route("/users/{id}/sessions", get(users::sessions))
        .route("/status", get(system::status))
        .route("/network/cert", get(system::network_cert))
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
        .route("/kb/browse", get(files::browse))
        .route("/files", get(files::file))
        .route(
            "/uploads",
            post(files::upload).layer(axum::extract::DefaultBodyLimit::max(files::UPLOAD_MAX)),
        )
        .route("/mcp/script-token", get(mcp::script_token))
        .route("/mcp/script-token/rotate", post(mcp::rotate_script_token))
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
        .route("/schedules", get(schedules::list).post(schedules::create))
        .route("/schedules/preview", get(schedules::preview))
        .route(
            "/schedules/{id}",
            get(schedules::get_one)
                .put(schedules::update)
                .delete(schedules::delete),
        )
        .route("/schedules/{id}/run", post(schedules::run_now))
        .route("/browser/status", get(browser::status))
        .route("/browser/detect", get(browser::detect))
        .route("/browser/traffic", get(browser::traffic))
        .route("/browser/traffic/clear", post(browser::traffic_clear))
        .route("/browser/launch", post(browser::launch))
        .route("/browser/close", post(browser::close))
        .route("/browser/test/upstream", post(browser::test_upstream))
        .route("/browser/test/new-tab", post(browser::test_new_tab))
        .route(
            "/browser/test/agent-browser",
            post(browser::test_agent_browser),
        )
        .route("/browser/install/status", get(browser::install_status))
        .route("/browser/install/job", get(browser::install_job))
        .route(
            "/browser/install/agent-browser",
            post(browser::install_agent_browser),
        )
        .route("/browser/install/chromium", post(browser::install_chromium))
        .route("/browser/install/skill", post(browser::install_skill))
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
        .layer(axum::middleware::from_fn(admin_gate))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth))
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024));
    Router::new()
        .nest("/api", api)
        .fallback(not_found)
        .with_state(state)
}
