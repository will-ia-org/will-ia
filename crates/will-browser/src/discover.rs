//! Descoberta do navegador upstream quando o HTTP de descoberta não responde.
//!
//! Um Chrome com depuração remota ligada **sem** `--remote-allow-origins` (caso
//! do perfil principal do usuário, ligado por outro app) devolve `404` em todo
//! `/json/*`, mas aceita o WebSocket CDP normalmente. A porta e o caminho do
//! WebSocket ficam no arquivo `DevToolsActivePort` do perfil:
//!
//! ```text
//! 9222
//! /devtools/browser/d783d62e-6345-457d-83b5-89663bcb25a7
//! ```
//!
//! Este módulo só lê arquivos e monta candidatos; quem valida (handshake) é o
//! `BrowserModule`.

use std::path::{Path, PathBuf};

/// Conteúdo de um `DevToolsActivePort`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevToolsActivePort {
    pub port: u16,
    /// `/devtools/browser/<token>`
    pub path: String,
}

/// Interpreta o texto do arquivo (`porta\ncaminho`). Tolerante a CRLF e linhas vazias.
pub fn parse_devtools_active_port(s: &str) -> Option<DevToolsActivePort> {
    let mut lines = s.lines().map(str::trim).filter(|l| !l.is_empty());
    let port: u16 = lines.next()?.parse().ok()?;
    let path = lines.next()?;
    if !path.starts_with("/devtools/") {
        return None;
    }
    Some(DevToolsActivePort {
        port,
        path: path.to_string(),
    })
}

/// Lê `<dir>/DevToolsActivePort`. O Chrome apaga o arquivo ao sair, então
/// existir já é um bom sinal de navegador vivo.
pub fn read_devtools_active_port(dir: &Path) -> Option<DevToolsActivePort> {
    let p = dir.join("DevToolsActivePort");
    let s = std::fs::read_to_string(p).ok()?;
    parse_devtools_active_port(&s)
}

/// Perfil conhecido: nome legível + pasta `user-data-dir`.
#[derive(Debug, Clone)]
pub struct ProfileDir {
    pub name: String,
    pub dir: PathBuf,
}

/// Perfis **do usuário** (não o dedicado do serviço) onde um navegador Chromium
/// pode estar com depuração ligada, na ordem de preferência, só os que existem.
pub fn user_profile_dirs() -> Vec<ProfileDir> {
    let mut out: Vec<ProfileDir> = vec![];
    let mut push = |name: &str, dir: PathBuf| {
        if dir.is_dir() && !out.iter().any(|p| p.dir == dir) {
            out.push(ProfileDir {
                name: name.to_string(),
                dir,
            });
        }
    };
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let base = home.join("Library/Application Support");
        push("Google Chrome", base.join("Google/Chrome"));
        push("Chromium", base.join("Chromium"));
        push("Brave", base.join("BraveSoftware/Brave-Browser"));
        push("Microsoft Edge", base.join("Microsoft Edge"));
    }
    #[cfg(target_os = "linux")]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        push("Google Chrome", base.join("google-chrome"));
        push("Chromium", base.join("chromium"));
        push("Brave", base.join("BraveSoftware/Brave-Browser"));
        push("Microsoft Edge", base.join("microsoft-edge"));
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        push("Google Chrome", local.join(r"Google\Chrome\User Data"));
        push("Chromium", local.join(r"Chromium\User Data"));
        push(
            "Brave",
            local.join(r"BraveSoftware\Brave-Browser\User Data"),
        );
        push("Microsoft Edge", local.join(r"Microsoft\Edge\User Data"));
    }
    out
}

/// Hosts concretos a sondar para um `upstream_host` configurado. `localhost`
/// é dual-stack (resolve para `127.0.0.1` **e** `::1`; o Chrome pode ter se
/// ligado só num deles), então vira os dois, IPv4 primeiro.
pub fn candidate_hosts(upstream_host: &str) -> Vec<String> {
    let h = upstream_host.trim().trim_matches(|c| c == '[' || c == ']');
    match h.to_ascii_lowercase().as_str() {
        "localhost" | "" => vec!["127.0.0.1".into(), "[::1]".into()],
        "::1" => vec!["[::1]".into()],
        other if other.contains(':') => vec![format!("[{other}]")],
        other => vec![other.to_string()],
    }
}

/// Reescreve `ws://<host>:<port>` e `<host>:<port>` (qualquer grafia de loopback,
/// o host configurado e a autoridade que respondeu) para `127.0.0.1:<listen>`.
pub fn rewrite_authorities(
    body: &str,
    upstream_host: &str,
    responding_host: Option<&str>,
    upstream_port: u16,
    listen_port: u16,
) -> String {
    let me = format!("127.0.0.1:{listen_port}");
    let mut hosts: Vec<String> = vec!["127.0.0.1".into(), "localhost".into(), "[::1]".into()];
    for h in [Some(upstream_host), responding_host].into_iter().flatten() {
        let h = h.trim();
        if !h.is_empty() && !hosts.iter().any(|x| x == h) {
            hosts.push(h.to_string());
        }
    }
    let alt = hosts
        .iter()
        .map(|h| regex::escape(h))
        .collect::<Vec<_>>()
        .join("|");
    match regex::Regex::new(&format!(r"(?:{alt}):{upstream_port}\b")) {
        Ok(re) => re.replace_all(body, me.as_str()).into_owned(),
        Err(_) => body.replace(&format!("{upstream_host}:{upstream_port}"), &me),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devtools_active_port() {
        let p = parse_devtools_active_port("9222\r\n/devtools/browser/abc-123\n").unwrap();
        assert_eq!(p.port, 9222);
        assert_eq!(p.path, "/devtools/browser/abc-123");
        assert!(parse_devtools_active_port("x\n/devtools/browser/a").is_none());
        assert!(parse_devtools_active_port("9222\n/outra/coisa").is_none());
        assert!(parse_devtools_active_port("").is_none());
    }

    #[test]
    fn reads_file_from_profile_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_devtools_active_port(dir.path()).is_none());
        std::fs::write(
            dir.path().join("DevToolsActivePort"),
            "9333\n/devtools/browser/tok",
        )
        .unwrap();
        let p = read_devtools_active_port(dir.path()).unwrap();
        assert_eq!((p.port, p.path.as_str()), (9333, "/devtools/browser/tok"));
    }

    #[test]
    fn localhost_is_dual_stack() {
        assert_eq!(candidate_hosts("localhost"), vec!["127.0.0.1", "[::1]"]);
        assert_eq!(candidate_hosts("127.0.0.1"), vec!["127.0.0.1"]);
        assert_eq!(candidate_hosts("::1"), vec!["[::1]"]);
        assert_eq!(candidate_hosts("[::1]"), vec!["[::1]"]);
        assert_eq!(candidate_hosts("10.0.0.5"), vec!["10.0.0.5"]);
    }

    #[test]
    fn rewrites_every_loopback_spelling_and_responding_host() {
        let body = r#"{"a":"ws://localhost:9222/devtools/browser/x","b":"ws://[::1]:9222/p","c":"127.0.0.1:9222/q","d":"ws://127.0.0.1:92220/zz"}"#;
        let out = rewrite_authorities(body, "localhost", Some("[::1]"), 9222, 9223);
        assert_eq!(
            out,
            r#"{"a":"ws://127.0.0.1:9223/devtools/browser/x","b":"ws://127.0.0.1:9223/p","c":"127.0.0.1:9223/q","d":"ws://127.0.0.1:92220/zz"}"#
        );
    }
}
