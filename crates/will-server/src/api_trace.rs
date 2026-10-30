//! Span `api` por requisição HTTP (método, caminho, status, usuário, duração).
//!
//! Middleware simples (sem `TraceLayer`) para ser aplicado em `api::router`
//! como a camada **mais externa** da API (último `.layer(...)` do `Router` em
//! `api/mod.rs`): assim cobre 401/403 do `auth`/`admin_gate` também. O campo
//! `user` fica vazio até o middleware `auth` chamar
//! `tracing::Span::current().record("user", username)`.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use std::time::Instant;
use tracing::Instrument;

/// Middleware axum: abre o span `api` e emite um evento no fim com o status.
pub async fn api_span(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let user = req
        .extensions()
        .get::<will_core::Principal>()
        .map(|p| p.username.clone())
        .unwrap_or_default();
    let span = tracing::info_span!(
        "api",
        method = %method,
        path = %path,
        status = tracing::field::Empty,
        user = %user,
        duration_ms = tracing::field::Empty
    );
    let start = Instant::now();
    async move {
        let res = next.run(req).await;
        let status = res.status();
        let ms = start.elapsed().as_millis() as u64;
        let span = tracing::Span::current();
        span.record("status", status.as_u16());
        span.record("duration_ms", ms);
        let code = status.as_u16();
        if code >= 500 {
            tracing::error!(
                status = code,
                duration_ms = ms,
                "requisição falhou no servidor"
            );
        } else if code >= 400 {
            tracing::warn!(status = code, duration_ms = ms, "requisição recusada");
        } else if method == axum::http::Method::GET {
            // Leituras (polling do app) ficam em debug; mutações em info.
            tracing::debug!(status = code, duration_ms = ms, "requisição atendida");
        } else {
            tracing::info!(status = code, duration_ms = ms, "requisição atendida");
        }
        res
    }
    .instrument(span)
    .await
}
