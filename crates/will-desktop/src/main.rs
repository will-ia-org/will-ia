//! will-ia — binário único (ADR-0016).
//!
//! - `will-ia` (sem argumentos): serviço em processo + bandeja + janela nativa.
//! - `will-ia serve`: modo headless (só serviço + API local) para servidores,
//!   testes (`scripts/smoke.sh`) e systemd sem sessão gráfica.
//! - `will-ia info`: caminhos e token da API local.
//!
//! Se já houver uma instância com GUI no ar, uma nova execução sem argumentos
//! apenas pede a ela que mostre a janela e sai.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod help;
mod hotkey;
mod i18n;
mod icon;
mod ipc;
mod pages;
mod service;
mod tray;
mod ui;
mod voice;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "will-ia",
    version,
    about = "will-ia — assistente pessoal: cérebro Claude Code + voz + Telegram + MCP"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Abre a janela principal ao iniciar (padrão: só se o onboarding não terminou)
    #[arg(long, global = true)]
    show: bool,
    /// Inicia só na bandeja, sem janela
    #[arg(long, global = true, conflicts_with = "show")]
    hidden: bool,
    /// Diretório de dados (padrão: do SO)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Guarda segredos em arquivo em vez do keyring
    #[arg(long, global = true)]
    file_secrets: bool,
    /// Porta da API local (padrão: configurada, 47300)
    #[arg(long, global = true)]
    port: Option<u16>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sobe só o serviço (API local, módulos), sem janela nem bandeja
    Serve,
    /// Mostra o token da API local e os caminhos usados
    Info,
    /// Pede à instância em execução que mostre a janela
    Show,
    /// Lista os dispositivos de áudio vistos pelo app (diagnóstico da voz)
    AudioDevices,
    /// Pede à instância em execução uma captura da janela (data_dir/screenshot.png)
    Screenshot {
        /// Página a mostrar antes: dashboard, threads, approvals, logs, general, engine, kb, voice, telegram, mcp, external, whatsapp, google
        #[arg(long)]
        nav: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(d) = &cli.data_dir {
        std::env::set_var("WILL_IA_DATA_DIR", d);
    }
    let opts = will_server::RunOptions {
        port: cli.port,
        open_browser: false,
        data_dir: cli.data_dir.clone(),
        file_secrets: cli.file_secrets,
    };
    match cli.cmd {
        Some(Cmd::Info) => {
            let paths = will_core::AppPaths::resolve();
            println!("data_dir: {}", paths.data_dir.display());
            println!("logs_dir: {}", paths.logs_dir.display());
            println!("db:       {}", paths.db_path().display());
            match std::fs::read_to_string(paths.session_token_file()) {
                Ok(t) => println!("api token: {}", t.trim()),
                Err(_) => println!("api token: (ainda não gerado; rode `will-ia`)"),
            }
            Ok(())
        }
        Some(Cmd::Show) => match ipc::show_running_instance(cli.port) {
            Ok(true) => Ok(()),
            Ok(false) => anyhow::bail!("nenhuma instância com GUI em execução"),
            Err(e) => anyhow::bail!("{e}"),
        },
        Some(Cmd::Screenshot { nav }) => {
            match ipc::screenshot_running_instance(cli.port, nav.as_deref()) {
                Ok(true) => Ok(()),
                Ok(false) => anyhow::bail!("nenhuma instância com GUI em execução"),
                Err(e) => anyhow::bail!("{e}"),
            }
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
            // Abre o microfone padrão por 400 ms com buffer curto e mostra o nível.
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
        Some(Cmd::Serve) => serve_headless(opts),
        None => {
            if let Ok(true) = ipc::show_running_instance(cli.port) {
                return Ok(());
            }
            app::run(
                opts,
                if cli.hidden {
                    app::StartWindow::Hidden
                } else if cli.show {
                    app::StartWindow::Shown
                } else {
                    app::StartWindow::Auto
                },
            )
        }
    }
}

fn serve_headless(opts: will_server::RunOptions) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let handle = will_server::run(opts).await?;
        println!(
            "will-ia (headless) no ar: {}  — token em {}",
            handle.url(),
            handle.state.paths.session_token_file().display()
        );
        println!("(Ctrl+C para encerrar)");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = handle.cancelled() => {}
        }
        println!("encerrando…");
        handle.shutdown().await;
        Ok(())
    })
}
