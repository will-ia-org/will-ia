//! Instância única do app e comandos entre execuções (`show`, `screenshot`):
//! trinco `app.lock` na pasta de configuração + um TCP em loopback com token,
//! anotados em `ipc` (porta e token). A segunda execução só manda o comando
//! e sai. O servidor usa o mesmo caminho para "Abrir will-ia" na bandeja.

use crate::profiles::config_dir;
use crate::service::{Service, UiCommand};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

pub struct AppLock {
    _lock: will_platform::instance::InstanceLock,
}

/// `Ok(Some)` = somos a instância; `Ok(None)` = já havia uma (e o comando foi entregue).
pub fn acquire_or_forward(cmd: &str) -> anyhow::Result<Option<AppLock>> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    match will_platform::instance::try_lock(&dir) {
        Ok(l) => Ok(Some(AppLock { _lock: l })),
        Err(will_platform::instance::InstanceLockError::Busy) => {
            for _ in 0..10 {
                if send(cmd).is_ok() {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            eprintln!("will-ia-app já está aberto, mas não respondeu ao comando `{cmd}`.");
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

fn ipc_file() -> std::path::PathBuf {
    config_dir().join("ipc")
}

/// Envia um comando para o app em execução.
pub fn send(cmd: &str) -> Result<(), String> {
    let s = std::fs::read_to_string(ipc_file()).map_err(|e| e.to_string())?;
    let mut it = s.split_whitespace();
    let port: u16 = it
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or("ipc sem porta")?;
    let token = it.next().ok_or("ipc sem token")?;
    let mut st = TcpStream::connect_timeout(
        &(std::net::Ipv4Addr::LOCALHOST, port).into(),
        Duration::from_millis(500),
    )
    .map_err(|e| e.to_string())?;
    st.set_read_timeout(Some(Duration::from_secs(2))).ok();
    writeln!(st, "{token} {cmd}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(st)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.trim() == "ok" {
        Ok(())
    } else {
        Err(format!("resposta: {}", line.trim()))
    }
}

/// Abre o TCP de comandos e grava `ipc`. Comandos: `show`, `screenshot[:nav]`.
pub fn serve(svc: Arc<Service>) {
    let listener = match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "ipc do app indisponível");
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let token = uuid_like();
    let _ = std::fs::write(ipc_file(), format!("{port} {token}"));
    std::thread::Builder::new()
        .name("will-app-ipc".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let svc = svc.clone();
                let token = token.clone();
                let mut reader = BufReader::new(conn);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut stream = reader.into_inner();
                let mut it = line.trim().splitn(2, ' ');
                if it.next() != Some(token.as_str()) {
                    let _ = writeln!(stream, "token inválido");
                    continue;
                }
                let cmd = it.next().unwrap_or("").trim();
                match cmd {
                    "show" | "" => svc.send_cmd(UiCommand::Show),
                    c if c.starts_with("screenshot") => {
                        let nav = c
                            .strip_prefix("screenshot")
                            .and_then(|r| r.strip_prefix(':'))
                            .map(String::from);
                        svc.send_cmd(UiCommand::Screenshot(screenshot_path(), nav));
                    }
                    _ => {
                        let _ = writeln!(stream, "comando desconhecido");
                        continue;
                    }
                }
                let _ = writeln!(stream, "ok");
            }
        })
        .ok();
}

pub fn screenshot_path() -> std::path::PathBuf {
    config_dir().join("screenshot.png")
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", t, std::process::id())
}
