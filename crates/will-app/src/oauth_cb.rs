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
            let _ = respond(&mut stream, 400, false, "Requisição inválida", "").await;
            continue;
        };
        if path != CALLBACK_PATH {
            let _ = respond(&mut stream, 404, false, "Não encontrado", "").await;
            continue;
        }
        let get = |k: &str| query.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        if let Some(err) = get("error") {
            let _ = respond(
                &mut stream,
                200,
                false,
                "Autorização não concluída",
                &format!("O Google recusou o acesso ({err}). Você pode fechar esta aba e tentar de novo no will-ia."),
            )
            .await;
            return Err(format!("autorização negada: {err}"));
        }
        if get("state").as_deref() != Some(expected_state) {
            let _ = respond(
                &mut stream,
                400,
                false,
                "Estado inválido",
                "Verificação anti-CSRF falhou. Tente conectar de novo pelo will-ia.",
            )
            .await;
            continue;
        }
        let Some(code) = get("code").filter(|c| !c.is_empty()) else {
            let _ = respond(&mut stream, 400, false, "Código ausente", "").await;
            continue;
        };
        let _ = respond(
            &mut stream,
            200,
            true,
            "Conta Google conectada",
            "Pronto! Pode fechar esta aba e voltar ao will-ia.",
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

async fn respond(
    stream: &mut tokio::net::TcpStream,
    code: u16,
    ok: bool,
    heading: &str,
    text: &str,
) -> std::io::Result<()> {
    let reason = if code == 200 { "OK" } else { "Error" };
    let html = page(ok, heading, text);
    let resp = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await
}

/// Página HTML do callback: cartão centralizado, clara/escura, com ícone.
fn page(ok: bool, heading: &str, text: &str) -> String {
    let (cls, icon) = if ok { ("ok", "✓") } else { ("err", "✕") };
    format!(
        r#"<!doctype html><html lang="pt-BR"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>will-ia</title>
<style>
:root{{color-scheme:light dark}}
*{{box-sizing:border-box}}
body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;padding:24px;
font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;
background:#f5f6f8;color:#1a1d21}}
.card{{max-width:420px;width:100%;padding:40px 32px;text-align:center;background:#fff;
border-radius:18px;box-shadow:0 12px 48px rgba(0,0,0,.12)}}
.icon{{width:68px;height:68px;margin:0 auto 22px;border-radius:50%;display:flex;
align-items:center;justify-content:center;font-size:34px;font-weight:700}}
.ok{{background:#e6f6ec;color:#1a7f3c}}
.err{{background:#fde8e8;color:#c0392b}}
h1{{font-size:21px;margin:0 0 10px;font-weight:650}}
p{{margin:0;color:#6b7280;font-size:15px;line-height:1.55}}
.brand{{margin-top:26px;font-size:12px;letter-spacing:.08em;text-transform:uppercase;color:#9aa3ad}}
@media(prefers-color-scheme:dark){{
body{{background:#15171a;color:#e6e8eb}}
.card{{background:#1e2126;box-shadow:0 12px 48px rgba(0,0,0,.5)}}
.ok{{background:#16351f;color:#4ade80}}
.err{{background:#3a1d1d;color:#f87171}}
p{{color:#9aa3ad}}
}}
</style></head>
<body><div class="card">
<div class="icon {cls}">{icon}</div>
<h1>{heading}</h1>
<p>{text}</p>
<div class="brand">will-ia</div>
</div>
<script>setTimeout(function(){{try{{window.close()}}catch(e){{}}}},1200)</script>
</body></html>"#
    )
}
