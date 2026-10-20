//! Fluxo OAuth: mapeamento de escopos lógicos, listener de loopback de uso
//! único (porta aleatória, timeout, validação de `state`) e `userinfo`.

use crate::error::GoogleError;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub const SCOPE_OPENID: &str = "openid";
pub const SCOPE_EMAIL: &str = "email";
pub const SCOPE_CALENDAR_RO: &str = "https://www.googleapis.com/auth/calendar.readonly";
pub const SCOPE_CALENDAR_EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";
pub const SCOPE_GMAIL_RO: &str = "https://www.googleapis.com/auth/gmail.readonly";
pub const SCOPE_GMAIL_SEND: &str = "https://www.googleapis.com/auth/gmail.send";
pub const SCOPE_GMAIL_MODIFY: &str = "https://www.googleapis.com/auth/gmail.modify";

pub const CALLBACK_PATH: &str = "/oauth/google/callback";
pub const FLOW_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Converte escopos lógicos (`calendar`, `calendar_write`, `gmail`, `gmail_send`,
/// `gmail_modify`) em URLs. URLs completas passam direto. `openid`+`email` sempre.
pub fn logical_to_scopes(logical: &[String]) -> Result<Vec<String>, GoogleError> {
    let mut out: Vec<String> = vec![SCOPE_OPENID.to_string(), SCOPE_EMAIL.to_string()];
    for l in logical {
        let l = l.trim();
        let mapped: &[&str] = match l {
            "" => &[],
            "openid" | "email" | "profile" => &[],
            "calendar" | "calendar_read" => &[SCOPE_CALENDAR_RO],
            "calendar_write" => &[SCOPE_CALENDAR_RO, SCOPE_CALENDAR_EVENTS],
            "gmail" | "gmail_read" => &[SCOPE_GMAIL_RO],
            "gmail_send" => &[SCOPE_GMAIL_RO, SCOPE_GMAIL_SEND],
            "gmail_modify" => &[SCOPE_GMAIL_RO, SCOPE_GMAIL_MODIFY],
            other if other.starts_with("https://") => {
                push_unique(&mut out, other);
                continue;
            }
            other => {
                return Err(GoogleError::Invalid(format!(
                    "escopo desconhecido: {other}"
                )))
            }
        };
        for m in mapped {
            push_unique(&mut out, m);
        }
    }
    Ok(out)
}

pub fn merge_scopes(base: &[String], extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in base.iter().chain(extra.iter()) {
        push_unique(&mut out, s);
    }
    out
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !s.is_empty() && !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

pub fn has_scope(scopes: &[String], wanted: &str) -> bool {
    scopes.iter().any(|s| s == wanted)
}

/// Resultado do callback de loopback.
#[derive(Debug)]
pub struct CallbackResult {
    pub code: String,
}

/// Abre um listener em 127.0.0.1 na porta pedida (ou aleatória com `None`).
pub async fn bind_loopback(port: Option<u16>) -> Result<(TcpListener, u16), GoogleError> {
    let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0))).await?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Espera o redirect do Google (um único `code` válido), com timeout e `state`.
pub async fn wait_callback(
    listener: TcpListener,
    expected_state: &str,
    timeout: Duration,
    cancel: CancellationToken,
) -> Result<CallbackResult, GoogleError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let accept = tokio::select! {
            _ = cancel.cancelled() => return Err(GoogleError::OAuth("fluxo cancelado".into())),
            r = tokio::time::timeout_at(deadline, listener.accept()) => r,
        };
        let (mut stream, _) = match accept {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(GoogleError::Io(e.to_string())),
            Err(_) => {
                return Err(GoogleError::OAuth(
                    "tempo esgotado aguardando autorização (5 min)".into(),
                ))
            }
        };
        let head = match tokio::time::timeout(Duration::from_secs(5), read_head(&mut stream)).await
        {
            Ok(Ok(h)) => h,
            _ => continue,
        };
        let Some((path, query)) = parse_request_target(&head) else {
            let _ = respond(&mut stream, 400, "Requisição inválida.").await;
            continue;
        };
        if path != CALLBACK_PATH {
            let _ = respond(&mut stream, 404, "Não encontrado.").await;
            continue;
        }
        let get = |k: &str| query.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        if let Some(err) = get("error") {
            let _ = respond(
                &mut stream,
                200,
                &format!("Autorização não concluída ({err}). Pode fechar esta aba."),
            )
            .await;
            return Err(GoogleError::OAuth(format!("autorização negada: {err}")));
        }
        if get("state").as_deref() != Some(expected_state) {
            let _ = respond(
                &mut stream,
                400,
                "Estado inválido (anti-CSRF). Tente conectar de novo.",
            )
            .await;
            continue;
        }
        let Some(code) = get("code").filter(|c| !c.is_empty()) else {
            let _ = respond(&mut stream, 400, "Código ausente.").await;
            continue;
        };
        let _ = respond(
            &mut stream,
            200,
            "Autorização concluída. Pode fechar esta aba e voltar ao will-ia.",
        )
        .await;
        return Ok(CallbackResult { code });
    }
}

async fn read_head(stream: &mut tokio::net::TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Extrai (path, query pairs) da request line `GET /x?y=z HTTP/1.1`.
pub fn parse_request_target(head: &str) -> Option<(String, Vec<(String, String)>)> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    if method != "GET" {
        return None;
    }
    let target = parts.next()?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    let pairs = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    Some((url.path().to_string(), pairs))
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    message: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let body = format!(
        "<!doctype html><html lang=\"pt-BR\"><head><meta charset=\"utf-8\"><title>will-ia</title>\
         <style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#111;color:#eee}}\
         main{{text-align:center;max-width:32rem;padding:2rem}}</style></head>\
         <body><main><h1>will-ia</h1><p>{}</p></main></body></html>",
        html_escape(message)
    );
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.shutdown().await
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Debug, serde::Deserialize)]
pub struct UserInfo {
    pub sub: String,
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: bool,
}

pub async fn fetch_userinfo(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
) -> Result<UserInfo, GoogleError> {
    let resp = http.get(url).bearer_auth(access_token).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(crate::client::parse_api_error(status.as_u16(), &text));
    }
    serde_json::from_str(&text).map_err(|e| GoogleError::Http(format!("userinfo inválido: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_logical_scopes_with_identity() {
        let s = logical_to_scopes(&["calendar".into(), "gmail_send".into()]).unwrap();
        assert_eq!(s[0], "openid");
        assert_eq!(s[1], "email");
        assert!(s.contains(&SCOPE_CALENDAR_RO.to_string()));
        assert!(s.contains(&SCOPE_GMAIL_RO.to_string()));
        assert!(s.contains(&SCOPE_GMAIL_SEND.to_string()));
        assert!(!s.contains(&SCOPE_CALENDAR_EVENTS.to_string()));
        // sem duplicatas
        let mut d = s.clone();
        d.dedup();
        assert_eq!(d.len(), s.len());
    }

    #[test]
    fn calendar_write_implies_readonly_and_passthrough_urls() {
        let s = logical_to_scopes(&["calendar_write".into(), SCOPE_GMAIL_MODIFY.into()]).unwrap();
        assert!(s.contains(&SCOPE_CALENDAR_RO.to_string()));
        assert!(s.contains(&SCOPE_CALENDAR_EVENTS.to_string()));
        assert!(s.contains(&SCOPE_GMAIL_MODIFY.to_string()));
        assert!(logical_to_scopes(&["drive".into()]).is_err());
    }

    #[test]
    fn merge_dedups_preserving_order() {
        let m = merge_scopes(&["a".into(), "b".into()], &["b".into(), "c".into()]);
        assert_eq!(m, vec!["a", "b", "c"]);
    }

    #[test]
    fn parses_callback_request_line() {
        let (p, q) = parse_request_target(
            "GET /oauth/google/callback?state=abc&code=4%2Fxyz HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .unwrap();
        assert_eq!(p, CALLBACK_PATH);
        assert!(q.contains(&("state".to_string(), "abc".to_string())));
        assert!(q.contains(&("code".to_string(), "4/xyz".to_string())));
        assert!(parse_request_target("POST / HTTP/1.1").is_none());
    }

    #[tokio::test]
    async fn loopback_rejects_bad_state_and_accepts_good_code() {
        let (listener, port) = bind_loopback(None).await.unwrap();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(wait_callback(
            listener,
            "good",
            Duration::from_secs(10),
            cancel,
        ));
        let http = reqwest::Client::new();
        let bad = http
            .get(format!(
                "http://127.0.0.1:{port}{CALLBACK_PATH}?state=bad&code=1"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status().as_u16(), 400);
        let nf = http
            .get(format!("http://127.0.0.1:{port}/favicon.ico"))
            .send()
            .await
            .unwrap();
        assert_eq!(nf.status().as_u16(), 404);
        let ok = http
            .get(format!(
                "http://127.0.0.1:{port}{CALLBACK_PATH}?state=good&code=the-code"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status().as_u16(), 200);
        assert!(ok.text().await.unwrap().contains("Pode fechar"));
        let r = task.await.unwrap().unwrap();
        assert_eq!(r.code, "the-code");
    }
}
