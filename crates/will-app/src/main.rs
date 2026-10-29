//! will-ia-app — app desktop (egui), cliente do servidor `will-ia` (ADR-0019).
//!
//! - `will-ia-app` (sem argumentos): abre a janela conectada ao último perfil
//!   usado (ou a "Esta máquina", se há um servidor local no ar). Se o app já
//!   está aberto, só pede para ele mostrar a janela e sai.
//! - `will-ia-app --url http://host:porta --token …`: conecta num servidor explícito.
//! - `will-ia-app --profile nome`: conecta num perfil salvo.
//! - `will-ia-app screenshot --nav <pagina>`: captura da janela do app em execução.
//! - `will-ia-app audio-devices`: diagnóstico de áudio.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod app_ipc;
mod help;
mod hotkey;
mod i18n;
mod icon;
mod oauth_cb;
mod pages;
mod profiles;
mod service;
mod ui;
mod voice;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "will-ia-app",
    version,
    about = "will-ia-app — app desktop do will-ia (cliente do servidor)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Abre a janela já visível (padrão)
    #[arg(long, global = true)]
    show: bool,
    /// Inicia com a janela oculta (só o atalho global e o ipc)
    #[arg(long, global = true, conflicts_with = "show")]
    hidden: bool,
    /// URL do servidor (`http://host:porta`); com `--token`
    #[arg(long, global = true)]
    url: Option<String>,
    /// Token de sessão do servidor (com `--url`)
    #[arg(long, global = true)]
    token: Option<String>,
    /// Nome de um perfil salvo (ou `local`)
    #[arg(long, global = true)]
    profile: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Pede ao app em execução que mostre a janela
    Show,
    /// Lista os dispositivos de áudio vistos pelo app (diagnóstico da voz)
    AudioDevices,
    /// Pede ao app em execução uma captura da janela (<config>/screenshot.png)
    Screenshot {
        /// Página a mostrar antes: dashboard, threads, schedules, browser, approvals, logs, costs, users, connect, general, engine, kb, voice, telegram, mcp, external, whatsapp, google, scheduler, browser-settings
        #[arg(long)]
        nav: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Show) => app_ipc::send("show").map_err(|e| anyhow::anyhow!("{e}")),
        Some(Cmd::Screenshot { nav }) => {
            let cmd = match nav {
                Some(n) => format!("screenshot:{n}"),
                None => "screenshot".into(),
            };
            app_ipc::send(&cmd).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", app_ipc::screenshot_path().display());
            Ok(())
        }
        Some(Cmd::AudioDevices) => {
            let d = voice::audio::list_devices();
            println!("host: {}", d.host);
            println!("entrada:");
            for x in &d.inputs {
                println!("  {x}");
            }
            println!("saída:");
            for x in &d.outputs {
                println!("  {x}");
            }
            let m = voice::audio::InputMonitor::start(None);
            std::thread::sleep(std::time::Duration::from_millis(400));
            match &m.error {
                Some(e) => println!("teste do microfone: ERRO {e}"),
                None => println!(
                    "teste do microfone: ok ({}) nível={:.4}",
                    m.device_name,
                    m.level()
                ),
            }
            Ok(())
        }
        None => {
            let Some(_lock) = app_ipc::acquire_or_forward("show")? else {
                return Ok(());
            };
            let profile = profiles::pick(
                cli.url.as_deref(),
                cli.token.as_deref(),
                cli.profile.as_deref(),
            );
            app::run(profile, !cli.hidden)
        }
    }
}
