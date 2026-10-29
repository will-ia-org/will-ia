//! Camada por sistema operacional: caminhos, processos, serviço de autostart,
//! atalhos e gravação dos binários. Tudo **por usuário** (sem sudo/UAC):
//!
//! | | macOS | Linux | Windows |
//! |---|---|---|---|
//! | binários | `~/Applications/will-ia.app/Contents/MacOS/` | `~/.local/bin/` | `%LOCALAPPDATA%\Programs\will-ia\` |
//! | serviço | LaunchAgent `dev.will.will-ia` | autostart `.desktop` (com tela) ou user unit systemd (sem tela) | valor `will-ia` em `HKCU\…\Run` |
//! | atalho | symlink do `.app` na Mesa | `.desktop` no menu e na Área de Trabalho | `.lnk` na Área de Trabalho e no Menu Iniciar |
//! | dados | `~/Library/Application Support/dev.will.will-ia` | `~/.local/share/will-ia` | `%APPDATA%\will\will-ia\data` |
//!
//! Os dados (`data_dir`: banco, segredos, WhatsApp, logs) **nunca** são tocados,
//! salvo o `install-manifest.json` e, com `--purge`, a remoção explícita.

use crate::plan::{Plan, ServiceKind, LEGACY_LAUNCHD_LABELS, PRODUCT, SERVICE_LABEL};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MANIFEST: &str = "install-manifest.json";
pub type Log<'a> = dyn Fn(&str) + Send + Sync + 'a;

const SERVER_BIN: &str = "will-ia";
const APP_BIN: &str = "will-ia-app";

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

pub fn home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Mesmo cálculo do `will_core::AppPaths` (sem modo portátil).
pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("WILL_IA_DATA_DIR") {
        return PathBuf::from(d);
    }
    directories::ProjectDirs::from("dev", "will", PRODUCT)
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| home().join(".will-ia"))
}

pub fn default_install_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Applications").join(format!("{PRODUCT}.app"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData/Local"))
            .join("Programs")
            .join(PRODUCT)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home().join(".local/bin")
    }
}

pub fn binary_paths(install_dir: &Path) -> (PathBuf, PathBuf) {
    let base = if cfg!(target_os = "macos") {
        install_dir.join("Contents/MacOS")
    } else {
        install_dir.to_path_buf()
    };
    (base.join(exe(SERVER_BIN)), base.join(exe(APP_BIN)))
}

// ---------------------------------------------------------------- utilitários

/// Roda um comando e devolve stdout (ou erro com stderr). Nunca herda a janela do terminal.
pub fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let mut c = Command::new(program);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut c);
    match c.output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(format!(
            "{program} {}: {}{}",
            args.join(" "),
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("{program}: {e}")),
    }
}

fn hide_console(_c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        _c.creation_flags(CREATE_NO_WINDOW);
    }
}

/// `--version` do binário, com limite de 5 s (um clap imprime e sai).
pub fn binary_version(path: &Path) -> Option<String> {
    let mut c = Command::new(path);
    c.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut c);
    let mut child = c.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().last().map(|v| v.trim().to_string())
}

pub fn check_writable(install_dir: &Path) -> Result<(), String> {
    let probe_dir = if cfg!(target_os = "macos") {
        install_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| install_dir.to_path_buf())
    } else {
        install_dir.to_path_buf()
    };
    std::fs::create_dir_all(&probe_dir).map_err(|e| e.to_string())?;
    let probe = probe_dir.join(".will-ia-setup-probe");
    std::fs::write(&probe, b"ok").map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

// ---------------------------------------------------------------- processos

pub fn running_processes() -> Vec<(String, u32)> {
    let mut out = vec![];
    for name in [SERVER_BIN, APP_BIN] {
        for pid in pids_of(name) {
            out.push((name.to_string(), pid));
        }
    }
    out
}

#[cfg(unix)]
fn pids_of(name: &str) -> Vec<u32> {
    run("pgrep", &["-x", name])
        .map(|s| s.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

#[cfg(windows)]
fn pids_of(name: &str) -> Vec<u32> {
    let filter = format!("IMAGENAME eq {name}.exe");
    run("tasklist", &["/FI", &filter, "/FO", "CSV", "/NH"])
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let cols: Vec<&str> = l.split("\",\"").collect();
                    cols.get(1).and_then(|p| p.trim_matches('"').parse().ok())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn kill(name: &str, force: bool) {
    #[cfg(unix)]
    {
        let _ = if force {
            run("pkill", &["-9", "-x", name])
        } else {
            run("pkill", &["-x", name])
        };
    }
    #[cfg(windows)]
    {
        let image = format!("{name}.exe");
        let _ = if force {
            run("taskkill", &["/F", "/IM", &image])
        } else {
            run("taskkill", &["/IM", &image])
        };
    }
}

/// Para o serviço e mata app + servidor (o servidor residente ignora SIGTERM:
/// depois de 5 s vai de SIGKILL). Espera os processos sumirem.
pub fn stop_everything(plan: &Plan, log: &Log<'_>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let uid = uid();
        for label in std::iter::once(SERVICE_LABEL).chain(LEGACY_LAUNCHD_LABELS.iter().copied()) {
            if run("launchctl", &["print", &format!("gui/{uid}/{label}")]).is_ok() {
                log(&format!("parando LaunchAgent {label}"));
                let _ = run("launchctl", &["bootout", &format!("gui/{uid}/{label}")]);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if plan.service_kind == ServiceKind::SystemdUser || plan.service_installed {
            let _ = run(
                "systemctl",
                &["--user", "stop", &format!("{PRODUCT}.service")],
            );
        }
    }
    let _ = plan;
    let mut any = false;
    for name in [APP_BIN, SERVER_BIN] {
        if !pids_of(name).is_empty() {
            any = true;
            log(&format!("encerrando {name}"));
            kill(name, false);
        }
    }
    if !any {
        log("nenhum processo do will-ia em execução");
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !running_processes().is_empty() {
        std::thread::sleep(Duration::from_millis(250));
    }
    if !running_processes().is_empty() {
        log("servidor residente não encerrou: forçando (kill -9 / taskkill /F)");
        for name in [APP_BIN, SERVER_BIN] {
            kill(name, true);
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && !running_processes().is_empty() {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    let left = running_processes();
    if left.is_empty() {
        log("processos encerrados");
        Ok(())
    } else {
        Err(format!("ainda em execução: {left:?}"))
    }
}

#[cfg(target_os = "macos")]
fn uid() -> String {
    run("id", &["-u"]).unwrap_or_else(|_| "501".into())
}

// ---------------------------------------------------------------- binários

fn write_atomic(dst: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let tmp = dst.with_extension("new");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    // Windows não renomeia por cima de arquivo existente (e nem de um em uso):
    // remove antes, com algumas tentativas (o processo pode estar saindo).
    let mut last = None;
    for _ in 0..20 {
        if dst.exists() {
            if let Err(e) = std::fs::remove_file(dst) {
                last = Some(e.to_string());
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
        }
        match std::fs::rename(&tmp, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e.to_string());
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    Err(format!(
        "não consegui gravar {}: {}",
        dst.display(),
        last.unwrap_or_default()
    ))
}

pub fn write_binaries(plan: &Plan, log: &Log<'_>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let contents = plan.install_dir.join("Contents");
        std::fs::create_dir_all(contents.join("MacOS")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(contents.join("Resources")).map_err(|e| e.to_string())?;
        let plist =
            crate::payload::MACOS_INFO_PLIST.replace("__VERSION__", crate::payload::VERSION);
        std::fs::write(contents.join("Info.plist"), plist).map_err(|e| e.to_string())?;
        std::fs::write(
            contents.join("Resources/will-ia.icns"),
            crate::payload::MACOS_ICNS,
        )
        .map_err(|e| e.to_string())?;
        log(&format!(
            "bundle {} (Info.plist + ícone)",
            plan.install_dir.display()
        ));
    }
    log(&format!(
        "gravando {} ({})",
        plan.server_path.display(),
        crate::plan::human_bytes(crate::payload::SERVER.len() as u64)
    ));
    write_atomic(&plan.server_path, crate::payload::SERVER)?;
    log(&format!(
        "gravando {} ({})",
        plan.app_path.display(),
        crate::plan::human_bytes(crate::payload::APP.len() as u64)
    ));
    write_atomic(&plan.app_path, crate::payload::APP)?;
    #[cfg(target_os = "macos")]
    {
        // arquivos gravados por nós não têm quarentena; garante no bundle inteiro
        let _ = run(
            "xattr",
            &[
                "-dr",
                "com.apple.quarantine",
                &plan.install_dir.to_string_lossy(),
            ],
        );
        // Finder/Dock guardam cache do ícone por mtime do bundle: "toca" pra atualizar.
        let _ = run("touch", &[&plan.install_dir.to_string_lossy()]);
    }
    Ok(())
}

pub fn remove_binaries(plan: &Plan, log: &Log<'_>) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        if plan.install_dir.is_dir() {
            std::fs::remove_dir_all(&plan.install_dir).map_err(|e| e.to_string())?;
            log(&format!("removido {}", plan.install_dir.display()));
        }
        return Ok(());
    }
    for p in [&plan.server_path, &plan.app_path] {
        if p.exists() {
            std::fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display()))?;
            log(&format!("removido {}", p.display()));
        }
    }
    if cfg!(windows) && plan.install_dir.is_dir() {
        let _ = std::fs::remove_dir(&plan.install_dir);
    }
    Ok(())
}

// ---------------------------------------------------------------- serviço

pub fn service_kind(has_desktop: bool) -> ServiceKind {
    if cfg!(target_os = "macos") {
        ServiceKind::Launchd
    } else if cfg!(windows) {
        ServiceKind::RunKey
    } else if has_desktop {
        ServiceKind::AutostartDesktop
    } else {
        ServiceKind::SystemdUser
    }
}

fn launchd_plist_path(label: &str) -> PathBuf {
    home()
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}
fn autostart_desktop_path() -> PathBuf {
    config_home()
        .join("autostart")
        .join(format!("{PRODUCT}.desktop"))
}
fn systemd_unit_path() -> PathBuf {
    config_home()
        .join("systemd/user")
        .join(format!("{PRODUCT}.service"))
}
fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// (instalado, em execução)
pub fn service_state(kind: &ServiceKind) -> (bool, bool) {
    let running = !pids_of(SERVER_BIN).is_empty();
    match kind {
        ServiceKind::Launchd => {
            let installed = launchd_plist_path(SERVICE_LABEL).is_file();
            #[cfg(target_os = "macos")]
            let loaded = run(
                "launchctl",
                &["print", &format!("gui/{}/{SERVICE_LABEL}", uid())],
            )
            .is_ok();
            #[cfg(not(target_os = "macos"))]
            let loaded = false;
            (installed, loaded && running)
        }
        ServiceKind::AutostartDesktop => (autostart_desktop_path().is_file(), running),
        ServiceKind::SystemdUser => {
            let active = run(
                "systemctl",
                &["--user", "is-active", &format!("{PRODUCT}.service")],
            )
            .map(|s| s == "active")
            .unwrap_or(false);
            (systemd_unit_path().is_file(), active)
        }
        ServiceKind::RunKey => {
            #[cfg(windows)]
            let installed = run("reg", &["query", RUN_KEY, "/v", PRODUCT]).is_ok();
            #[cfg(not(windows))]
            let installed = false;
            (installed, running)
        }
    }
}

/// Instalações manuais anteriores que o instalador vai absorver.
pub fn legacy_artifacts() -> Vec<String> {
    let mut v = vec![];
    if cfg!(target_os = "macos") {
        for label in LEGACY_LAUNCHD_LABELS {
            let p = launchd_plist_path(label);
            if p.is_file() {
                v.push(format!("LaunchAgent manual {label} ({})", p.display()));
            }
        }
    }
    v
}

fn env_path_for_service() -> String {
    let mut parts: Vec<PathBuf> = vec![
        home().join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
    ];
    parts.extend(will_platform::tool_dirs());
    for d in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        parts.push(PathBuf::from(d));
    }
    let mut seen = vec![];
    for p in parts {
        if p.is_dir() && !seen.contains(&p) {
            seen.push(p);
        }
    }
    std::env::join_paths(seen)
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/usr/bin:/bin".into())
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn install_service(plan: &Plan, enable: bool, log: &Log<'_>) -> Result<(), String> {
    match plan.service_kind {
        ServiceKind::Launchd => install_launchd(plan, enable, log),
        ServiceKind::AutostartDesktop => {
            let p = autostart_desktop_path();
            // se existir unit do systemd de uma instalação anterior, desativa
            if systemd_unit_path().is_file() {
                let _ = run(
                    "systemctl",
                    &["--user", "disable", "--now", &format!("{PRODUCT}.service")],
                );
                let _ = std::fs::remove_file(systemd_unit_path());
            }
            if !enable {
                if p.exists() {
                    std::fs::remove_file(&p).map_err(|e| e.to_string())?;
                    log("autostart removido");
                }
                return Ok(());
            }
            std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")))
                .map_err(|e| e.to_string())?;
            let body = format!(
                "[Desktop Entry]\nType=Application\nName=will-ia (servidor)\nComment=Servidor will-ia (bandeja + API local); o app é aberto pela bandeja\nExec={}\nIcon=will-ia\nTerminal=false\nX-GNOME-Autostart-enabled=true\nX-KDE-autostart-after=panel\nStartupNotify=false\n",
                plan.server_path.display()
            );
            std::fs::write(&p, body).map_err(|e| e.to_string())?;
            log(&format!("autostart: {}", p.display()));
            Ok(())
        }
        ServiceKind::SystemdUser => {
            let p = systemd_unit_path();
            if !enable {
                let _ = run(
                    "systemctl",
                    &["--user", "disable", "--now", &format!("{PRODUCT}.service")],
                );
                if p.exists() {
                    std::fs::remove_file(&p).map_err(|e| e.to_string())?;
                }
                let _ = run("systemctl", &["--user", "daemon-reload"]);
                log("unidade do systemd removida");
                return Ok(());
            }
            std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")))
                .map_err(|e| e.to_string())?;
            let body = format!(
                "[Unit]\nDescription=will-ia — servidor (Claude Code + Telegram + MCP + agendamentos)\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={} serve\nRestart=on-failure\nRestartSec=3\nEnvironment=RUST_LOG=info\nEnvironment=PATH={}\n\n[Install]\nWantedBy=default.target\n",
                plan.server_path.display(),
                env_path_for_service()
            );
            std::fs::write(&p, body).map_err(|e| e.to_string())?;
            run("systemctl", &["--user", "daemon-reload"])?;
            run(
                "systemctl",
                &["--user", "enable", &format!("{PRODUCT}.service")],
            )?;
            log(&format!("unidade do systemd: {} (habilitada)", p.display()));
            if run("loginctl", &["show-user", &whoami(), "--property=Linger"])
                .map(|s| s.contains("yes"))
                .unwrap_or(false)
            {
                log("linger já ativo: o serviço sobe sem sessão aberta");
            } else {
                log(
                    "dica: `loginctl enable-linger $USER` mantém o serviço de pé sem sessão aberta",
                );
            }
            Ok(())
        }
        ServiceKind::RunKey => {
            #[cfg(windows)]
            {
                if !enable {
                    let _ = run("reg", &["delete", RUN_KEY, "/v", PRODUCT, "/f"]);
                    log("autostart (chave Run) removido");
                    return Ok(());
                }
                let value = format!("\"{}\"", plan.server_path.display());
                run(
                    "reg",
                    &[
                        "add", RUN_KEY, "/v", PRODUCT, "/t", "REG_SZ", "/d", &value, "/f",
                    ],
                )?;
                log(&format!("autostart: {RUN_KEY}\\{PRODUCT}"));
                Ok(())
            }
            #[cfg(not(windows))]
            {
                let _ = (plan, enable, log);
                Err("chave Run só existe no Windows".into())
            }
        }
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
}

fn install_launchd(plan: &Plan, enable: bool, log: &Log<'_>) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (plan, enable, log);
        Err("LaunchAgent só existe no macOS".into())
    }
    #[cfg(target_os = "macos")]
    {
        let uid = uid();
        let plist = launchd_plist_path(SERVICE_LABEL);
        // LaunchAgents manuais antigos: descarrega e guarda como .bak
        for label in LEGACY_LAUNCHD_LABELS {
            let old = launchd_plist_path(label);
            if old.is_file() {
                let _ = run("launchctl", &["bootout", &format!("gui/{uid}/{label}")]);
                let bak = old.with_extension(format!(
                    "plist.bak.{}",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ));
                std::fs::rename(&old, &bak).map_err(|e| e.to_string())?;
                log(&format!(
                    "LaunchAgent antigo {label} desativado (backup {})",
                    bak.display()
                ));
            }
        }
        // Login Item criado pelo app (auto-launch) duplicaria o servidor no login
        let _ = run(
            "osascript",
            &["-e", "tell application \"System Events\" to delete (every login item whose name is \"will-ia\")"],
        );
        if run(
            "launchctl",
            &["print", &format!("gui/{uid}/{SERVICE_LABEL}")],
        )
        .is_ok()
        {
            let _ = run(
                "launchctl",
                &["bootout", &format!("gui/{uid}/{SERVICE_LABEL}")],
            );
        }
        if !enable {
            if plist.exists() {
                std::fs::remove_file(&plist).map_err(|e| e.to_string())?;
            }
            log("LaunchAgent removido (não inicia com o sistema)");
            return Ok(());
        }
        std::fs::create_dir_all(plist.parent().unwrap_or(Path::new(".")))
            .map_err(|e| e.to_string())?;
        let logs = plan.data_dir.join("logs");
        let _ = std::fs::create_dir_all(&logs);
        let lang = std::env::var("LANG").unwrap_or_else(|_| "pt_BR.UTF-8".into());
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <!-- gerado pelo will-ia-setup {ver}: servidor residente (bandeja + API local + MCP). O app é aberto pela bandeja. -->
  <key>ProgramArguments</key>
  <array><string>{server}</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>{path}</string>
    <key>HOME</key><string>{home}</string>
    <key>LANG</key><string>{lang}</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>{out}</string>
  <key>StandardErrorPath</key><string>{err}</string>
</dict>
</plist>
"#,
            label = SERVICE_LABEL,
            ver = crate::payload::VERSION,
            server = xml_escape(&plan.server_path.to_string_lossy()),
            path = xml_escape(&env_path_for_service()),
            home = xml_escape(&home().to_string_lossy()),
            lang = xml_escape(&lang),
            out = xml_escape(&logs.join("launchd.out.log").to_string_lossy()),
            err = xml_escape(&logs.join("launchd.err.log").to_string_lossy()),
        );
        std::fs::write(&plist, body).map_err(|e| e.to_string())?;
        run(
            "launchctl",
            &["bootstrap", &format!("gui/{uid}"), &plist.to_string_lossy()],
        )?;
        log(&format!(
            "LaunchAgent {SERVICE_LABEL} carregado ({})",
            plist.display()
        ));
        Ok(())
    }
}

// ---------------------------------------------------------------- atalhos

fn desktop_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        if let Ok(d) = run("xdg-user-dir", &["DESKTOP"]) {
            let p = PathBuf::from(d);
            if p.is_dir() {
                return p;
            }
        }
    }
    #[cfg(windows)]
    {
        if let Ok(d) = run(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "[Environment]::GetFolderPath('Desktop')",
            ],
        ) {
            let p = PathBuf::from(d.trim());
            if p.is_dir() {
                return p;
            }
        }
    }
    home().join("Desktop")
}

pub fn install_shortcuts(plan: &Plan, desktop: bool, log: &Log<'_>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let link = desktop_dir().join(format!("{PRODUCT}.app"));
        if desktop {
            if link.exists() || std::fs::symlink_metadata(&link).is_ok() {
                let _ = std::fs::remove_file(&link);
            }
            std::os::unix::fs::symlink(&plan.install_dir, &link).map_err(|e| e.to_string())?;
            log(&format!("atalho na Mesa: {}", link.display()));
        } else if std::fs::symlink_metadata(&link).is_ok() {
            let _ = std::fs::remove_file(&link);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=will-ia\nComment=Assistente pessoal: Claude Code + voz + Telegram + MCP (app desktop)\nExec={} --show\nIcon=will-ia\nTerminal=false\nCategories=Utility;Office;\nStartupWMClass=will-ia\n",
            plan.app_path.display()
        );
        let apps = home().join(".local/share/applications");
        std::fs::create_dir_all(&apps).map_err(|e| e.to_string())?;
        let menu = apps.join(format!("{PRODUCT}.desktop"));
        std::fs::write(&menu, &body).map_err(|e| e.to_string())?;
        log(&format!("entrada de menu: {}", menu.display()));
        let _ = run("update-desktop-database", &[&apps.to_string_lossy()]);
        // Ícone `will-ia` no tema hicolor do usuário (menu, dock, alt-tab).
        let hicolor = home().join(".local/share/icons/hicolor");
        let mut n = 0;
        for (size, bytes) in will_platform::icon::APP_PNGS {
            let dir = hicolor.join(format!("{size}x{size}/apps"));
            if std::fs::create_dir_all(&dir).is_ok()
                && std::fs::write(dir.join(format!("{PRODUCT}.png")), bytes).is_ok()
            {
                n += 1;
            }
        }
        let _ = run(
            "gtk-update-icon-cache",
            &["-f", "-t", &hicolor.to_string_lossy()],
        );
        log(&format!(
            "ícones instalados em {} ({n} tamanhos)",
            hicolor.display()
        ));
        let d = desktop_dir().join(format!("{PRODUCT}.desktop"));
        if desktop {
            if let Some(p) = d.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            std::fs::write(&d, &body).map_err(|e| e.to_string())?;
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));
            let _ = run(
                "gio",
                &["set", &d.to_string_lossy(), "metadata::trusted", "true"],
            );
            log(&format!("atalho na Área de Trabalho: {}", d.display()));
        } else if d.exists() {
            let _ = std::fs::remove_file(&d);
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let start = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData/Roaming"))
            .join(r"Microsoft\Windows\Start Menu\Programs");
        let mut targets = vec![start.join(format!("{PRODUCT}.lnk"))];
        let d = desktop_dir().join(format!("{PRODUCT}.lnk"));
        if desktop {
            targets.push(d.clone());
        } else if d.exists() {
            let _ = std::fs::remove_file(&d);
        }
        for lnk in targets {
            let script = format!(
                "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath='{}'; $s.Arguments='--show'; $s.WorkingDirectory='{}'; $s.IconLocation='{},0'; $s.Description='will-ia'; $s.Save()",
                lnk.display().to_string().replace('\'', "''"),
                plan.app_path.display().to_string().replace('\'', "''"),
                plan.install_dir.display().to_string().replace('\'', "''"),
                plan.app_path.display().to_string().replace('\'', "''"),
            );
            run(
                "powershell",
                &[
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &script,
                ],
            )?;
            log(&format!("atalho: {}", lnk.display()));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (plan, desktop, log);
        Ok(())
    }
}

pub fn remove_shortcuts(plan: &Plan, log: &Log<'_>) {
    let _ = plan;
    #[cfg(target_os = "macos")]
    {
        let link = desktop_dir().join(format!("{PRODUCT}.app"));
        if std::fs::symlink_metadata(&link).is_ok() && std::fs::remove_file(&link).is_ok() {
            log("atalho da Mesa removido");
        }
    }
    #[cfg(target_os = "linux")]
    {
        for p in [
            home()
                .join(".local/share/applications")
                .join(format!("{PRODUCT}.desktop")),
            desktop_dir().join(format!("{PRODUCT}.desktop")),
        ] {
            if p.exists() && std::fs::remove_file(&p).is_ok() {
                log(&format!("removido {}", p.display()));
            }
        }
        let hicolor = home().join(".local/share/icons/hicolor");
        for (size, _) in will_platform::icon::APP_PNGS {
            let _ = std::fs::remove_file(hicolor.join(format!("{size}x{size}/apps/{PRODUCT}.png")));
        }
        log("ícones do tema removidos");
    }
    #[cfg(windows)]
    {
        let start = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData/Roaming"))
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join(format!("{PRODUCT}.lnk"));
        for p in [start, desktop_dir().join(format!("{PRODUCT}.lnk"))] {
            if p.exists() && std::fs::remove_file(&p).is_ok() {
                log(&format!("removido {}", p.display()));
            }
        }
    }
}

// ---------------------------------------------------------------- subir / saúde

fn spawn_detached(path: &Path, args: &[&str]) -> Result<(), String> {
    let mut c = Command::new(path);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    c.spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Sobe o servidor pelo mecanismo do serviço (ou direto, se o autostart foi recusado).
pub fn start_server(plan: &Plan, service_enabled: bool, log: &Log<'_>) -> Result<(), String> {
    match (&plan.service_kind, service_enabled) {
        (ServiceKind::Launchd, true) => {
            // `bootstrap` com RunAtLoad já sobe o servidor; só dá kickstart se ele não apareceu.
            std::thread::sleep(Duration::from_millis(800));
            if pids_of(SERVER_BIN).is_empty() {
                #[cfg(target_os = "macos")]
                {
                    run(
                        "launchctl",
                        &["kickstart", "-k", &format!("gui/{}/{SERVICE_LABEL}", uid())],
                    )?;
                }
                log("servidor iniciado pelo launchd (kickstart)");
            } else {
                log("servidor já no ar pelo launchd");
            }
            Ok(())
        }
        (ServiceKind::SystemdUser, true) => {
            run(
                "systemctl",
                &["--user", "restart", &format!("{PRODUCT}.service")],
            )?;
            log("servidor iniciado pelo systemd");
            Ok(())
        }
        _ => {
            spawn_detached(&plan.server_path, &[])?;
            log("servidor iniciado");
            Ok(())
        }
    }
}

/// Espera `<data_dir>/api-port` responder em `/api/health`.
pub fn wait_health(data_dir: &Path, timeout: Duration) -> Result<u16, String> {
    use std::io::{Read, Write};
    let deadline = Instant::now() + timeout;
    let mut last = String::from("sem resposta");
    while Instant::now() < deadline {
        let port: u16 = std::fs::read_to_string(data_dir.join("api-port"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(47300);
        if let Ok(mut s) = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(800),
        ) {
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            let req = format!(
                "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            );
            if s.write_all(req.as_bytes()).is_ok() {
                let mut buf = String::new();
                let _ = s.read_to_string(&mut buf);
                if buf.contains("\"ok\":true") {
                    return Ok(port);
                }
                last = format!("porta {port} respondeu sem ok");
            }
        } else {
            last = format!("porta {port} fechada");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(last)
}

pub fn open_app(plan: &Plan) -> Result<(), String> {
    spawn_detached(&plan.app_path, &["--show"])
}

// ---------------------------------------------------------------- manifesto

pub fn write_manifest(
    plan: &Plan,
    service_enabled: bool,
    desktop_shortcut: bool,
) -> Result<(), String> {
    std::fs::create_dir_all(&plan.data_dir).map_err(|e| e.to_string())?;
    let m = serde_json::json!({
        "product": PRODUCT,
        "version": crate::payload::VERSION,
        "installed_at": chrono::Utc::now().to_rfc3339(),
        "installer": format!("will-ia-setup {}", crate::payload::VERSION),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "install_dir": plan.install_dir,
        "server": plan.server_path,
        "app": plan.app_path,
        "service": if service_enabled { serde_json::json!(plan.service_kind) } else { serde_json::Value::Null },
        "service_label": SERVICE_LABEL,
        "desktop_shortcut": desktop_shortcut,
    });
    std::fs::write(
        plan.data_dir.join(MANIFEST),
        serde_json::to_string_pretty(&m).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())
}

pub fn remove_manifest(plan: &Plan) {
    let _ = std::fs::remove_file(plan.data_dir.join(MANIFEST));
}

pub fn purge_data(plan: &Plan, log: &Log<'_>) -> Result<(), String> {
    if plan.data_dir.is_dir() {
        std::fs::remove_dir_all(&plan.data_dir).map_err(|e| e.to_string())?;
        log(&format!("dados removidos: {}", plan.data_dir.display()));
    }
    Ok(())
}
