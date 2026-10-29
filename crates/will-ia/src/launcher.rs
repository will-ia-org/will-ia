//! Lança o app desktop (`will-ia-app`) a partir do servidor: bandeja,
//! `will-ia show`, `POST /api/ui/show`. Procura o binário ao lado do próprio
//! executável e, depois, no PATH.

use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

fn app_binary() -> Option<PathBuf> {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    // Nome simples (instalação normal) e nome com plataforma (pacote portátil).
    let names = [
        format!("will-ia-app{ext}"),
        format!("will-ia-app-{os}-{arch}{ext}"),
    ];
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            for n in &names {
                let p = dir.join(n);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        // macOS instalado: o servidor roda do bundle auxiliar
        // `will-ia.app/Contents/Library/LoginItems/will-ia Server.app/Contents/MacOS/will-ia`;
        // o app está no bundle externo, em `Contents/MacOS/will-ia-app`.
        if cfg!(target_os = "macos") {
            for anc in me.ancestors() {
                if anc.extension().is_some_and(|e| e == "app") {
                    let p = anc.join("Contents/MacOS").join(&names[0]);
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    will_platform::which("will-ia-app")
}

/// Há sessão gráfica para bandeja e app?
pub fn has_desktop() -> bool {
    will_platform::has_desktop()
}

/// Abre o app (se já estiver aberto, ele mesmo só mostra a janela).
///
/// macOS dentro do bundle: pede a janela por ipc (`will-ia-app show`, sem
/// NSApplication, logo sem "pulo" no Dock) e deixa o **LaunchServices** ativar ou
/// lançar o `.app` (`open -a`). Lançar o binário direto fazia o app tentar se
/// ativar sozinho, o que o macOS 14+ nega e transforma em ícone pulando (ADR-0022).
pub fn open_app(extra: &[String]) -> anyhow::Result<u32> {
    let bin = app_binary().ok_or_else(|| {
        anyhow::anyhow!("will-ia-app não encontrado ao lado do will-ia nem no PATH")
    })?;
    #[cfg(target_os = "macos")]
    if let Some(bundle) = will_platform::macos::bundle_of(&bin) {
        if extra.is_empty() {
            // Já aberto: mostra a janela (ipc). Fechado: falha rápido e segue.
            let _ = Command::new(&bin)
                .arg("show")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        will_platform::macos::activate_bundle(&bundle, extra)?;
        tracing::info!(bundle = %bundle.display(), "app ativado pelo LaunchServices");
        return Ok(0);
    }
    let mut cmd = Command::new(&bin);
    cmd.args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = cmd.spawn()?;
    tracing::info!(bin = %bin.display(), pid = child.id(), "app aberto");
    Ok(child.id())
}

/// Roda o app com argumentos e espera (ex.: `screenshot --nav x`).
pub fn run_app(args: &[String]) -> anyhow::Result<ExitStatus> {
    let bin = app_binary().ok_or_else(|| {
        anyhow::anyhow!("will-ia-app não encontrado ao lado do will-ia nem no PATH")
    })?;
    let status = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(status)
}
