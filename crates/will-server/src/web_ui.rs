//! App web embutido (`docs/33`): o servidor entrega em `/` a mesma interface
//! do app de desktop, compilada para WASM. Fica na mesma porta da API, então a
//! página fala com `/api` sem CORS, e quem expõe para fora é o que já expõe a
//! API (TLS próprio, `tailscale serve`…).
//!
//! Os arquivos são embutidos no binário pelo `build.rs` (`WILL_APP_WEB_DIR`).
//! Compilado sem eles, `/` continua respondendo 404.

use crate::api::App;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;

/// O binário leva o app web.
pub const EMBEDDED: bool = cfg!(app_web);

struct Asset {
    bytes: &'static [u8],
    content_type: &'static str,
    gzip: bool,
}

macro_rules! asset {
    ($name:literal, $ct:literal, $gzip:literal) => {
        Asset {
            bytes: include_bytes!(concat!(env!("OUT_DIR"), "/app-web-", $name)),
            content_type: $ct,
            gzip: $gzip,
        }
    };
}

const INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app-web-index.html"));

fn asset(path: &str) -> Option<Asset> {
    Some(match path {
        "/manifest.webmanifest" => {
            asset!("manifest.webmanifest", "application/manifest+json", false)
        }
        "/icon-180.png" => asset!("icon-180.png", "image/png", false),
        "/icon-512.png" => asset!("icon-512.png", "image/png", false),
        "/will-ia-app.js" => asset!("will-ia-app.js.gz", "text/javascript; charset=utf-8", true),
        "/will-ia-app_bg.wasm" => asset!("will-ia-app_bg.wasm.gz", "application/wasm", true),
        _ => return None,
    })
}

/// A página foi pedida por `localhost` nesta máquina, sem passar por um proxy
/// (`tailscale serve` e afins mandam outro `Host` e cabeçalhos de
/// encaminhamento). Só nesse caso ela recebe o token local e entra direto em
/// "Esta máquina", como o app de desktop; de fora, pede usuário e senha.
///
/// O `Host` também barra DNS rebinding: um site de fora apontado para
/// `127.0.0.1` chega aqui com o nome dele, não `localhost`.
fn is_local_visit(peer: Option<SocketAddr>, headers: &HeaderMap) -> bool {
    let loopback = peer.is_some_and(|a| a.ip().to_canonical().is_loopback());
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    let forwarded = headers.keys().any(|k| {
        let k = k.as_str();
        k == "forwarded" || k.starts_with("x-forwarded-") || k.starts_with("tailscale-")
    });
    loopback && !forwarded && matches!(host, "localhost" | "127.0.0.1")
}

/// `index.html`, com o token local injetado quando a visita é local.
fn index(local_token: Option<&str>) -> Response {
    let mut html = String::from_utf8_lossy(INDEX).into_owned();
    if let Some(token) = local_token.filter(|t| !t.is_empty()) {
        // `serde_json` escapa aspas e barras: o token entra como literal JS.
        let lit = serde_json::to_string(token).unwrap_or_else(|_| "\"\"".into());
        html = html.replacen(
            "<script type=\"module\">",
            &format!(
                "<script>window.__WILL_IA_LOCAL_TOKEN__ = {lit};</script>\n<script type=\"module\">"
            ),
            1,
        );
    }
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // Leva o token local: nunca fica em cache.
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response()
}

fn not_found() -> Response {
    let msg = if EMBEDDED {
        "não encontrado"
    } else {
        "não encontrado; este servidor foi compilado sem o app web (use o app desktop)"
    };
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

/// `fallback` do roteador: tudo que não é `/api`.
pub async fn serve(State(state): State<App>, req: Request) -> Response {
    if !EMBEDDED || !matches!(*req.method(), Method::GET | Method::HEAD) {
        return not_found();
    }
    let path = req.uri().path();
    if path == "/" || path == "/index.html" {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);
        let local = is_local_visit(peer, req.headers());
        return index(local.then_some(state.ui_token.as_str()));
    }
    let Some(a) = asset(path) else {
        return not_found();
    };
    // Os arquivos só mudam com o binário: o ETag evita baixar o WASM de novo.
    let etag = concat!("\"", env!("WILL_APP_WEB_TAG"), "\"");
    if req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains(etag))
    {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let mut res = (
        [
            (header::CONTENT_TYPE, a.content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::ETAG, etag),
        ],
        a.bytes,
    )
        .into_response();
    if a.gzip {
        res.headers_mut().insert(
            header::CONTENT_ENCODING,
            header::HeaderValue::from_static("gzip"),
        );
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn local_visit_needs_loopback_localhost_and_no_proxy() {
        let lo: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let lan: SocketAddr = "192.168.0.7:5000".parse().unwrap();
        assert!(is_local_visit(
            Some(lo),
            &headers(&[("host", "localhost:47300")])
        ));
        assert!(is_local_visit(
            Some(lo),
            &headers(&[("host", "127.0.0.1:47300")])
        ));
        // de outra máquina
        assert!(!is_local_visit(
            Some(lan),
            &headers(&[("host", "localhost:47300")])
        ));
        // sem saber quem conectou
        assert!(!is_local_visit(None, &headers(&[("host", "localhost")])));
        // por trás do `tailscale serve`: chega de loopback, mas com outro Host
        assert!(!is_local_visit(
            Some(lo),
            &headers(&[("host", "mac.rede.ts.net")])
        ));
        assert!(!is_local_visit(
            Some(lo),
            &headers(&[
                ("host", "localhost:47300"),
                ("x-forwarded-for", "100.1.2.3")
            ])
        ));
        assert!(!is_local_visit(
            Some(lo),
            &headers(&[("host", "localhost"), ("tailscale-user-login", "a@b")])
        ));
        // DNS rebinding: site de fora resolvendo para 127.0.0.1
        assert!(!is_local_visit(
            Some(lo),
            &headers(&[("host", "evil.example:47300")])
        ));
    }
}
