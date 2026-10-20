//! Comunicação com uma instância já em execução, pela API local em loopback.
//! Implementação mínima de HTTP/1.1 sobre `TcpStream` (sem dependências).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn port_from_settings() -> u16 {
    // A porta vive nas settings (SQLite); sem abrir o banco aqui, usamos o padrão
    // e um arquivo auxiliar gravado pela instância no ar.
    let paths = will_core::AppPaths::resolve();
    std::fs::read_to_string(paths.data_dir.join("api-port"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(47300)
}

fn request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> Result<(u16, String), String> {
    let mut s = TcpStream::connect_timeout(
        &(std::net::Ipv4Addr::LOCALHOST, port).into(),
        Duration::from_millis(400),
    )
    .map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n{auth}\r\n");
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Ok((status, body))
}

/// `Ok(true)` se havia uma instância com GUI e ela foi avisada; `Ok(false)` se
/// não há instância (ou é headless); `Err` em falha de leitura do token.
pub fn show_running_instance(port: Option<u16>) -> Result<bool, String> {
    post_ui(port, "/api/ui/show")
}

/// Pede à instância em execução uma captura da janela (`<data_dir>/screenshot.png`).
pub fn screenshot_running_instance(port: Option<u16>, nav: Option<&str>) -> Result<bool, String> {
    match nav {
        Some(n) => post_ui(port, &format!("/api/ui/screenshot?nav={n}")),
        None => post_ui(port, "/api/ui/screenshot"),
    }
}

fn post_ui(port: Option<u16>, path: &str) -> Result<bool, String> {
    let port = port.unwrap_or_else(port_from_settings);
    let Ok((200, _)) = request(port, "GET", "/api/health", None) else {
        return Ok(false);
    };
    let paths = will_core::AppPaths::resolve();
    let token = std::fs::read_to_string(paths.session_token_file())
        .map_err(|e| format!("token da API local: {e}"))?;
    let (status, body) = request(port, "POST", path, Some(token.trim()))?;
    Ok(status == 200 && body.contains("\"ok\":true"))
}

/// Grava a porta efetiva para a próxima execução encontrar a instância.
pub fn write_port_file(paths: &will_core::AppPaths, port: u16) {
    let _ = std::fs::write(paths.data_dir.join("api-port"), port.to_string());
}
