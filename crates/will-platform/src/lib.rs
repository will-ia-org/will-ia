//! will-platform — integrações de SO sem dependências de GUI: notificações,
//! abrir pasta/URL, detecção de binários no PATH.

use std::path::{Path, PathBuf};

/// Notificação nativa (best-effort; nunca falha o chamador).
pub fn notify(title: &str, body: &str) {
    let title = title.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        #[cfg(any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "macos",
            target_os = "windows"
        ))]
        {
            let mut n = notify_rust::Notification::new();
            n.appname("will-ia").summary(&title).body(&body);
            if let Err(e) = n.show() {
                tracing::debug!(error = %e, "notificação nativa falhou");
            }
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "macos",
            target_os = "windows"
        )))]
        {
            tracing::info!(%title, %body, "notificação");
        }
    });
}

pub fn open_path(path: &Path) -> Result<(), String> {
    open::that_detached(path).map_err(|e| e.to_string())
}

pub fn open_url(url: &str) -> Result<(), String> {
    open::that_detached(url).map_err(|e| e.to_string())
}

/// Procura um executável no PATH (com extensões do Windows).
pub fn which(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.components().count() > 1 {
        return p.exists().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or(".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{bin}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Versão de um binário via `--version` (primeira linha).
pub async fn version_of(bin: &Path) -> Option<String> {
    let out = tokio::process::Command::new(bin)
        .arg("--version")
        .output()
        .await
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

/// Heurística: pasta dentro de um diretório sincronizado por nuvem.
pub fn looks_cloud_synced(path: &Path) -> bool {
    let s = path.to_string_lossy().to_lowercase();
    [
        "onedrive",
        "icloud",
        "dropbox",
        "google drive",
        "googledrive",
        "mobile documents",
    ]
    .iter()
    .any(|k| s.contains(k))
}

/// Pasta perigosa para ser a KB (home ou raiz).
pub fn is_dangerous_kb(path: &Path) -> bool {
    let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if canon.parent().is_none() {
        return true;
    }
    if let Some(h) = home {
        if canon == h {
            return true;
        }
    }
    false
}
