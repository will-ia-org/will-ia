//! will-ia-setup — instalador/atualizador do will-ia por usuário (sem sudo/UAC).
//!
//! - Sem argumentos e com sessão gráfica: assistente em janelas (egui).
//! - `--cli` (ou sem tela): o mesmo fluxo em perguntas no terminal; `--yes` aceita tudo.
//! - `--uninstall [--purge]`: remove binários, serviço e atalhos (dados só com `--purge`).
//! - `--status`: imprime o que foi detectado (JSON com `--json`) e sai.
//!
//! Os binários `will-ia` e `will-ia-app` vêm embutidos (build.rs + `WILL_SETUP_PAYLOAD_DIR`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod gui;
mod install;
mod os;
mod payload;
mod plan;
#[cfg(target_os = "macos")]
mod sign;

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "will-ia-setup",
    version,
    about = "Instala ou atualiza o will-ia (servidor + app) para o usuário atual"
)]
struct Cli {
    /// Modo texto (sem janela)
    #[arg(long)]
    cli: bool,
    /// Aceita as opções padrão sem perguntar (implica --cli)
    #[arg(long, short = 'y')]
    yes: bool,
    /// Remove a instalação (dados preservados)
    #[arg(long)]
    uninstall: bool,
    /// Com --uninstall: apaga também a pasta de dados
    #[arg(long)]
    purge: bool,
    /// Só mostra o que foi detectado e sai
    #[arg(long)]
    status: bool,
    /// Com --status: saída em JSON
    #[arg(long)]
    json: bool,
    /// Pasta de instalação (padrão por SO; no macOS é o .app)
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Não registrar início com o sistema
    #[arg(long)]
    no_autostart: bool,
    /// Não criar atalho na área de trabalho
    #[arg(long)]
    no_shortcut: bool,
    /// Não iniciar o servidor ao terminar
    #[arg(long)]
    no_start: bool,
    /// Não abrir o app ao terminar
    #[arg(long)]
    no_open: bool,
}

fn main() {
    let cli = Cli::parse();
    let text_mode = cli.cli || cli.yes || cli.status || !will_platform::has_desktop();
    if text_mode {
        will_platform::attach_parent_console();
    }
    let plan = plan::Plan::detect(cli.dir.clone());
    if cli.status {
        if cli.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&plan).unwrap_or_default()
            );
        } else {
            cli::print_plan(&plan);
        }
        return;
    }
    let opts = install::Options {
        autostart: !cli.no_autostart,
        desktop_shortcut: !cli.no_shortcut && plan.has_desktop,
        start_after: !cli.no_start,
        open_app_after: !cli.no_open && plan.has_desktop,
    };
    if text_mode {
        let code = if cli.uninstall {
            cli::run_uninstall(&plan, cli.purge, cli.yes)
        } else {
            cli::run_install(&plan, opts, cli.yes)
        };
        std::process::exit(code);
    }
    if let Err(e) = gui::run(plan, cli.uninstall) {
        eprintln!("não consegui abrir a janela ({e}); use --cli");
        std::process::exit(4);
    }
}
