//! Detecção de navegadores com CDP e de ferramentas do Node (node/npm/npx/agent-browser).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct BrowserCandidate {
    pub name: String,
    pub path: String,
}

/// Candidatos instalados, na ordem de preferência. Inclui o Chromium baixado
/// pelo agent-browser quando `agent_browser_chrome` é conhecido.
pub fn detect_browsers(agent_browser_chrome: Option<&str>) -> Vec<BrowserCandidate> {
    let mut out = vec![];
    let mut push = |name: &str, p: PathBuf| {
        if p.is_file()
            && !out
                .iter()
                .any(|c: &BrowserCandidate| c.path == p.to_string_lossy())
        {
            out.push(BrowserCandidate {
                name: name.to_string(),
                path: p.to_string_lossy().to_string(),
            });
        }
    };
    #[cfg(target_os = "linux")]
    {
        for (name, bins) in [
            (
                "Google Chrome",
                &[
                    "google-chrome",
                    "google-chrome-stable",
                    "google-chrome-beta",
                ][..],
            ),
            ("Chromium", &["chromium", "chromium-browser"][..]),
            ("Brave", &["brave-browser", "brave"][..]),
            (
                "Microsoft Edge",
                &["microsoft-edge", "microsoft-edge-stable"][..],
            ),
        ] {
            for b in bins {
                if let Some(p) = will_platform::which(b) {
                    push(name, p);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        for (name, p) in [
            (
                "Google Chrome",
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            ),
            (
                "Chromium",
                "/Applications/Chromium.app/Contents/MacOS/Chromium",
            ),
            (
                "Brave",
                "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            ),
            (
                "Microsoft Edge",
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            ),
        ] {
            push(name, PathBuf::from(p));
        }
    }
    #[cfg(windows)]
    {
        let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(|k| std::env::var_os(k).map(PathBuf::from))
            .collect();
        for (name, rel) in [
            ("Google Chrome", r"Google\Chrome\Application\chrome.exe"),
            ("Chromium", r"Chromium\Application\chrome.exe"),
            (
                "Brave",
                r"BraveSoftware\Brave-Browser\Application\brave.exe",
            ),
            ("Microsoft Edge", r"Microsoft\Edge\Application\msedge.exe"),
        ] {
            for r in &roots {
                push(name, r.join(rel));
            }
        }
    }
    if let Some(p) = agent_browser_chrome {
        push("Chromium do agent-browser", PathBuf::from(p));
    }
    out
}

/// Diretórios extras onde ferramentas do Node costumam morar quando o app é
/// iniciado fora do shell do usuário (nvm, volta, fnm, homebrew, npm global).
fn node_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![];
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        if let Ok(rd) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
            versions.sort();
            versions.reverse(); // mais nova primeiro
            dirs.extend(versions);
        }
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".local/share/fnm/aliases/default/bin"));
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(".local/bin"));
    }
    #[cfg(windows)]
    {
        if let Some(a) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(a).join("npm"));
        }
        if let Some(p) = std::env::var_os("ProgramFiles") {
            dirs.push(PathBuf::from(p).join("nodejs"));
        }
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs
}

/// Procura `name` no PATH e nos diretórios do Node. No Windows tenta `.cmd`/`.exe`.
pub fn find_node_tool(name: &str) -> Option<PathBuf> {
    if let Some(p) = will_platform::which(name) {
        return Some(p);
    }
    let exts: &[&str] = if cfg!(windows) {
        &[".cmd", ".exe", ""]
    } else {
        &[""]
    };
    for d in node_dirs() {
        for ext in exts {
            let p = d.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// PATH para processos filhos do Node: diretório da ferramenta na frente,
/// para `npm` achar o `node` da mesma instalação.
pub fn path_with(tool: &Path) -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = vec![];
    if let Some(d) = tool.parent() {
        parts.push(d.to_path_buf());
    }
    if let Some(p) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(parts).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}
