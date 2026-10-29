//! Listener de loopback do lado do cliente para o callback OAuth do Google.
//!
//! No modelo cliente/servidor, quem tem navegador é o app. Ele sobe uma porta
//! em `127.0.0.1`, o Google redireciona para ela com o `code`, o app pega o
//! código e repassa ao servidor (`POST /api/google/callback`), que faz a troca
//! pelo token. Assim a autorização funciona mesmo com o servidor remoto.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CALLBACK_PATH: &str = "/oauth/google/callback";

/// Abre o listener em `127.0.0.1` (porta fixa, ou aleatória com `None`).
pub async fn bind(fixed_port: Option<u16>) -> Result<(TcpListener, u16), String> {
    let listener = TcpListener::bind(("127.0.0.1", fixed_port.unwrap_or(0)))
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    Ok((listener, port))
}

/// Espera o redirect do Google e devolve o `code` (valida `state`).
pub async fn wait_code(
    listener: TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let accept = tokio::time::timeout_at(deadline, listener.accept()).await;
        let (mut stream, _) = match accept {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => return Err("tempo esgotado aguardando autorização (5 min)".into()),
        };
        let head = match tokio::time::timeout(Duration::from_secs(5), read_head(&mut stream)).await
        {
            Ok(Ok(h)) => h,
            _ => continue,
        };
        let Some((path, query)) = parse_target(&head) else {
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
            return Err(format!("autorização negada: {err}"));
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
        return Ok(code);
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

/// Extrai (path, pares de query) da request line `GET /x?y=z HTTP/1.1`.
fn parse_target(head: &str) -> Option<(String, Vec<(String, String)>)> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let pairs = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (urldecode(k), urldecode(v))
        })
        .collect();
    Some((path.to_string(), pairs))
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = (hex(b[i + 1]), hex(b[i + 2]));
                if let (Some(a), Some(c)) = h {
                    out.push(a * 16 + c);
                    i += 3;
                    continue;
                }
                out.push(b[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

async fn respond(stream: &mut tokio::net::TcpStream, code: u16, body: &str) -> std::io::Result<()> {
    let reason = if code == 200 { "OK" } else { "Error" };
    let resp = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n<!doctype html><meta charset=utf-8><body style=\"font-family:sans-serif;padding:2rem\">{}</body>",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await
}
