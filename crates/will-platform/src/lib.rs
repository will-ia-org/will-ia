//! will-platform — integrações de SO sem dependências de GUI: notificações,
//! abrir pasta/URL, detecção de binários no PATH.

pub mod icon;
pub mod instance;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod process;

use std::path::{Path, PathBuf};

/// Notificações nativas em voo. No macOS o backend (`mac-notification-sys`)
/// segura a thread até a notificação ser dispensada; o teto evita acumular
/// threads numa rajada de eventos (as excedentes só vão ao log).
static NOTIFY_IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const NOTIFY_MAX_IN_FLIGHT: usize = 6;

/// Notificação nativa (best-effort; nunca falha nem bloqueia o chamador).
pub fn notify(title: &str, body: &str) {
    use std::sync::atomic::Ordering;
    let title = title.to_string();
    let body = body.to_string();
    let title_len = title.chars().count();
    let body_len = body.chars().count();
    if NOTIFY_IN_FLIGHT.fetch_add(1, Ordering::SeqCst) >= NOTIFY_MAX_IN_FLIGHT {
        NOTIFY_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        tracing::warn!(name: "platform", op = "notify", ok = false, title_len, body_len, in_flight = NOTIFY_MAX_IN_FLIGHT, "notificação descartada: muitas em voo");
        return;
    }
    std::thread::spawn(move || {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                NOTIFY_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _done = Done;
        #[cfg(any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "macos",
            target_os = "windows"
        ))]
        {
            let mut n = notify_rust::Notification::new();
            n.appname("will-ia").summary(&title).body(&body);
            match n.show() {
                Ok(_) => {
                    tracing::debug!(name: "platform", op = "notify", ok = true, title_len, body_len, "notificação nativa exibida")
                }
                Err(e) => {
                    tracing::warn!(name: "platform", op = "notify", ok = false, title_len, body_len, error = %e, "notificação nativa falhou")
                }
            }
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "macos",
            target_os = "windows"
        )))]
        {
            tracing::info!(name: "platform", op = "notify", ok = false, title_len, body_len, "notificação sem backend nativo neste SO (só no log)");
        }
    });
}

/// Resultado de tentar listar uma pasta (TCC do macOS nega com EPERM
/// "Operation not permitted" pastas como Documentos/Mesa/Downloads/iCloud).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirAccess {
    Ok,
    /// Permissão negada pelo SO (no macOS: Ajustes → Privacidade → Arquivos e Pastas).
    Denied,
    Missing,
    Other(String),
}

pub fn dir_access(path: &Path) -> DirAccess {
    let access = match std::fs::read_dir(path) {
        Ok(mut it) => match it.next() {
            Some(Err(e)) if e.kind() == std::io::ErrorKind::PermissionDenied => DirAccess::Denied,
            _ => DirAccess::Ok,
        },
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => DirAccess::Denied,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirAccess::Missing,
        Err(e) => DirAccess::Other(e.to_string()),
    };
    if access == DirAccess::Denied {
        tracing::warn!(name: "platform", permission = "files_and_folders", granted = false, path = %path.display(), "acesso à pasta negado pelo SO");
    }
    access
}

/// Há sessão gráfica (bandeja, app, Chrome visível)? Windows/macOS: sempre;
/// Linux/BSD: `DISPLAY` ou `WAYLAND_DISPLAY`.
pub fn has_desktop() -> bool {
    if cfg!(windows) || cfg!(target_os = "macos") {
        return true;
    }
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn open_path(path: &Path) -> Result<(), String> {
    open::that_detached(path).map_err(|e| {
        tracing::warn!(name: "platform", op = "open_path", ok = false, error = %e, "abrir pasta falhou");
        e.to_string()
    })
}

pub fn open_url(url: &str) -> Result<(), String> {
    open::that_detached(url).map_err(|e| {
        tracing::warn!(name: "platform", op = "open_url", ok = false, error = %e, "abrir URL falhou");
        e.to_string()
    })
}

/// Procura um executável no PATH (com extensões do Windows).
/// Pastas onde ferramentas de desenvolvedor costumam morar e que ficam fora do
/// `PATH` de um app aberto pelo Finder/launchd (macOS), pelo autostart ou por
/// um serviço: Homebrew, MacPorts, nvm/volta/fnm, npm global, pyenv/asdf,
/// cargo, instalação nativa do Claude Code, npm global do Windows.
pub fn tool_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec![];
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if let Some(h) = &home {
        // nvm: versões mais novas primeiro
        if let Ok(rd) = std::fs::read_dir(h.join(".nvm/versions/node")) {
            let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
            v.sort();
            v.reverse();
            dirs.extend(v);
        }
        for rel in [
            ".volta/bin",
            ".local/share/fnm/aliases/default/bin",
            ".fnm/aliases/default/bin",
            ".npm-global/bin",
            ".npm/bin",
            ".local/bin",
            ".cargo/bin",
            ".pyenv/shims",
            ".asdf/shims",
            ".claude/local",
            ".bun/bin",
            ".deno/bin",
        ] {
            dirs.push(h.join(rel));
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/opt/node/bin"));
        dirs.push(PathBuf::from("/opt/local/bin"));
        dirs.push(PathBuf::from(
            "/Library/Frameworks/Python.framework/Versions/Current/bin",
        ));
    }
    #[cfg(windows)]
    {
        if let Some(a) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(a).join("npm"));
        }
        if let Some(p) = std::env::var_os("ProgramFiles") {
            dirs.push(PathBuf::from(&p).join("nodejs"));
            dirs.push(PathBuf::from(&p).join("Git").join("cmd"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            let l = PathBuf::from(l);
            dirs.push(l.join("Programs").join("Python").join("Launcher"));
            if let Ok(rd) = std::fs::read_dir(l.join("Programs").join("Python")) {
                for e in rd.flatten() {
                    dirs.push(e.path());
                }
            }
        }
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/snap/bin"));
    dirs.retain(|d| d.is_dir());
    dirs
}

/// `PATH` do processo + [`tool_dirs`] (sem duplicatas), para processos filhos
/// (`claude`, `node`, `npm`) acharem as ferramentas mesmo quando o serviço foi
/// aberto com `PATH` mínimo.
pub fn augmented_path() -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in tool_dirs() {
        if !parts.iter().any(|p| p == &d) {
            parts.push(d);
        }
    }
    std::env::join_paths(parts).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

/// Procura um executável no `PATH` e, depois, nas pastas de [`tool_dirs`]
/// (Homebrew, nvm, npm global…). Aceita caminho absoluto.
pub fn which(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.components().count() > 1 {
        return p.exists().then(|| p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or(".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(tool_dirs());
    for dir in dirs {
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
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version");
    will_core::process::hide_console(&mut cmd);
    let out = cmd.output().await.ok()?;
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

/// Windows: um binário com `windows_subsystem = "windows"` não tem console; quando
/// é chamado de um terminal (modo CLI), religa a saída ao console do pai para
/// `println!` aparecer. Nos outros SOs não faz nada.
pub fn attach_parent_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        // SAFETY: chamada simples da API do Windows, sem ponteiros; falha (sem pai com
        // console) é ignorada.
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[cfg(test)]
mod which_tests {
    use super::*;

    /// Simula o macOS: `PATH` mínimo e o Node instalado via nvm no HOME.
    #[test]
    #[cfg(unix)]
    fn finds_tools_outside_path_via_tool_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("will-ia-which-{}", std::process::id()));
        let bin = home.join(".nvm/versions/node/v24.0.0/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("node-fake-xyz");
        std::fs::write(&exe, "#!/bin/sh\necho v24\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let old_home = std::env::var_os("HOME");
        let old_path = std::env::var_os("PATH");
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", "/usr/bin:/bin");
        let found = which("node-fake-xyz");
        let aug = augmented_path();
        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        }
        if let Some(p) = old_path {
            std::env::set_var("PATH", p);
        }
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(found, Some(exe));
        assert!(aug
            .to_string_lossy()
            .contains(".nvm/versions/node/v24.0.0/bin"));
    }
}
