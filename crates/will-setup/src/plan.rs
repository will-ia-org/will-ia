//! Detecção do estado da máquina antes de instalar: onde instalar, o que já
//! existe (binários, versão, dados, serviço, atalhos, processos) e se temos
//! permissão. Não altera nada.

use crate::os;
use serde::Serialize;
use std::path::PathBuf;

pub const PRODUCT: &str = "will-ia";
/// Rótulo do LaunchAgent (macOS), nome da unit (Linux) e do valor na chave Run (Windows).
pub const SERVICE_LABEL: &str = "dev.will.will-ia";
/// LaunchAgent criado à mão antes do instalador existir (máquina de origem).
pub const LEGACY_LAUNCHD_LABELS: &[&str] = &["com.will.will-ia"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum ServiceKind {
    /// macOS: `~/Library/LaunchAgents/<label>.plist` (RunAtLoad + KeepAlive).
    Launchd,
    /// Linux com sessão gráfica: `~/.config/autostart/will-ia.desktop` (servidor residente + bandeja).
    AutostartDesktop,
    /// Linux sem tela: user unit do systemd (`will-ia serve`, Restart=on-failure).
    SystemdUser,
    /// Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
    RunKey,
}

impl ServiceKind {
    pub fn label(&self) -> &'static str {
        match self {
            ServiceKind::Launchd => "LaunchAgent do launchd (inicia no login, religa se cair)",
            ServiceKind::AutostartDesktop => "autostart da sessão (~/.config/autostart)",
            ServiceKind::SystemdUser => "unidade de usuário do systemd (will-ia serve)",
            ServiceKind::RunKey => "chave Run do registro (inicia no login)",
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Existing {
    pub server: Option<PathBuf>,
    pub app: Option<PathBuf>,
    pub server_version: Option<String>,
    pub app_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub os: String,
    pub arch: String,
    pub has_desktop: bool,
    pub new_version: String,
    pub payload_ok: bool,
    pub payload_bytes: u64,
    /// Pasta de instalação: macOS = o `.app`; Linux/Windows = pasta dos binários.
    pub install_dir: PathBuf,
    pub server_path: PathBuf,
    pub app_path: PathBuf,
    pub data_dir: PathBuf,
    pub data_exists: bool,
    pub db_bytes: u64,
    pub existing: Option<Existing>,
    pub service_kind: ServiceKind,
    pub service_installed: bool,
    pub service_running: bool,
    /// Resquícios de instalações manuais (LaunchAgent antigo, Login Item, autostart do app…).
    pub legacy: Vec<String>,
    pub running: Vec<(String, u32)>,
    pub writable: Result<(), String>,
    pub manifest: Option<serde_json::Value>,
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn detect(install_dir: Option<PathBuf>) -> Self {
        let install_dir = install_dir.unwrap_or_else(os::default_install_dir);
        let (server_path, app_path) = os::binary_paths(&install_dir);
        let data_dir = os::data_dir();
        let data_exists = data_dir.is_dir();
        let db_bytes = std::fs::metadata(data_dir.join("will-ia.db"))
            .map(|m| m.len())
            .unwrap_or(0);
        let manifest = std::fs::read_to_string(data_dir.join(os::MANIFEST))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let existing = {
            let server = if server_path.is_file() {
                Some(server_path.clone())
            } else if cfg!(target_os = "macos") {
                // instalação ≤ v0.1.7: servidor ao lado do app
                let legacy = os::legacy_server_path(&install_dir);
                legacy.is_file().then_some(legacy)
            } else {
                None
            };
            let app = app_path.is_file().then(|| app_path.clone());
            if server.is_none() && app.is_none() {
                None
            } else {
                Some(Existing {
                    server_version: server.as_deref().and_then(os::binary_version),
                    app_version: app.as_deref().and_then(os::binary_version),
                    server,
                    app,
                })
            }
        };
        let has_desktop = will_platform::has_desktop();
        let service_kind = os::service_kind(has_desktop);
        let (service_installed, service_running) = os::service_state(&service_kind);
        let legacy = os::legacy_artifacts();
        let running = os::running_processes();
        let writable = os::check_writable(&install_dir);
        let mut warnings = vec![];
        if !crate::payload::available() {
            warnings.push(
                "este instalador foi compilado SEM os binários (payload vazio): nada será instalado"
                    .into(),
            );
        }
        if let Some(e) = &existing {
            if let Some(v) = &e.server_version {
                match cmp_versions(v, crate::payload::VERSION) {
                    std::cmp::Ordering::Greater => warnings.push(format!(
                        "a versão instalada ({v}) é MAIS NOVA que a deste instalador ({}) — será um downgrade",
                        crate::payload::VERSION
                    )),
                    std::cmp::Ordering::Equal => warnings.push(format!(
                        "a versão {v} já está instalada — os binários serão reescritos (reparo)"
                    )),
                    std::cmp::Ordering::Less => {}
                }
            }
        }
        if let Err(e) = &writable {
            warnings.push(format!(
                "sem permissão de escrita em {}: {e}",
                install_dir.display()
            ));
        }
        Self {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            has_desktop,
            new_version: crate::payload::VERSION.into(),
            payload_ok: crate::payload::available(),
            payload_bytes: crate::payload::total_bytes(),
            install_dir,
            server_path,
            app_path,
            data_dir,
            data_exists,
            db_bytes,
            existing,
            service_kind,
            service_installed,
            service_running,
            legacy,
            running,
            writable,
            manifest,
            warnings,
        }
    }

    pub fn is_update(&self) -> bool {
        self.existing.is_some()
    }

    pub fn installed_version(&self) -> Option<&str> {
        self.existing
            .as_ref()
            .and_then(|e| e.server_version.as_deref().or(e.app_version.as_deref()))
    }

    pub fn can_install(&self) -> bool {
        self.payload_ok && self.writable.is_ok()
    }
}

/// Compara `a.b.c` numericamente (sufixos ignorados).
pub fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let num = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (x, y) = (num(a), num(b));
    let n = x.len().max(y.len());
    for i in 0..n {
        let o = x.get(i).unwrap_or(&0).cmp(y.get(i).unwrap_or(&0));
        if o != std::cmp::Ordering::Equal {
            return o;
        }
    }
    std::cmp::Ordering::Equal
}

pub fn human_bytes(b: u64) -> String {
    const K: f64 = 1024.0;
    let b = b as f64;
    if b >= K * K * K {
        format!("{:.1} GB", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.1} MB", b / (K * K))
    } else if b >= K {
        format!("{:.0} KB", b / K)
    } else {
        format!("{b:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        use std::cmp::Ordering::*;
        assert_eq!(cmp_versions("0.1.4", "0.1.3"), Greater);
        assert_eq!(cmp_versions("v0.1.4", "0.1.4"), Equal);
        assert_eq!(cmp_versions("0.1.4-rc1", "0.1.10"), Less);
        assert_eq!(cmp_versions("1.0", "1.0.0"), Equal);
    }
    #[test]
    fn bytes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(70 * 1024 * 1024), "70.0 MB");
    }
}
