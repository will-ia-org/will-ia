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

/// Procura `name` no PATH e nas pastas de ferramentas conhecidas
/// (`will_platform::tool_dirs`: nvm, volta, fnm, Homebrew, npm global do Windows…).
pub fn find_node_tool(name: &str) -> Option<PathBuf> {
    will_platform::which(name)
}

/// PATH para processos filhos do Node: diretório da ferramenta na frente,
/// para `npm` achar o `node` da mesma instalação.
pub fn path_with(tool: &Path) -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = vec![];
    if let Some(d) = tool.parent() {
        parts.push(d.to_path_buf());
    }
    parts.extend(std::env::split_paths(&will_platform::augmented_path()));
    std::env::join_paths(parts).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}
