//! will-ia — servidor residente (ADR-0019): serviço + API local + módulos.
//! Com sessão gráfica, fica na bandeja ("Abrir will-ia" lança o app
//! `will-ia-app`); sem sessão gráfica (servidor Linux), roda headless.
//!
//! - `will-ia` (sem argumentos): sobe o serviço; bandeja se houver desktop.
//!   Residente: SIGINT/SIGTERM/SIGHUP ignorados (só kill forçado derruba).
//! - `will-ia serve`: headless explícito; encerra limpo com Ctrl+C/SIGTERM
//!   (o que systemd espera).
//! - `will-ia info`: caminhos e token da API local.
//! - `will-ia show` / `screenshot --nav`: delega ao app em execução (ou o lança).
//!
//! Instância única por diretório de dados (`instance.lock`).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod icon;
mod ipc;
mod launcher;
mod tray;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "will-ia",
    version,
    about = "will-ia — servidor: cérebro Claude Code + voz + Telegram + MCP + agendamentos"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Abre o app (will-ia-app) depois de subir o serviço
    #[arg(long, global = true)]
    show: bool,
    /// Não tentar bandeja (mesmo com desktop)
    #[arg(long, global = true)]
    no_tray: bool,
    /// Diretório de dados (padrão: do SO)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Guarda segredos em arquivo em vez do keyring
    #[arg(long, global = true)]
    file_secrets: bool,
    /// Porta da API local (padrão: configurada, 47300)
    #[arg(long, global = true)]
    port: Option<u16>,
    /// Endereço de escuta (padrão: configurado, 127.0.0.1). Fora do loopback exige TLS.
    #[arg(long, global = true)]
    bind: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sobe só o serviço (API local, módulos), sem bandeja; Ctrl+C encerra
    Serve,
    /// Mostra o token da API local e os caminhos usados
    Info,
    /// Abre (ou traz para frente) o app will-ia-app
    Show,
    /// Pede ao app uma captura da janela (diagnóstico)
    Screenshot {
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
        bind: cli.bind.clone(),
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
            match std::fs::read_to_string(paths.data_dir.join("api-port")) {
                Ok(p) => println!("api url:  http://127.0.0.1:{}", p.trim()),
                Err(_) => println!("api url:  (servidor fora do ar)"),
            }
            if let Ok(t) =
                std::fs::read_to_string(paths.data_dir.join("admin-initial-password.txt"))
            {
                let mut it = t.lines();
                let (u, p) = (it.next().unwrap_or("admin"), it.next().unwrap_or(""));
                println!("admin inicial: usuário `{u}` senha `{p}` (troque no primeiro login)");
            }
            Ok(())
        }
        Some(Cmd::Show) => launcher::open_app(&[]).map(|_| ()),
        Some(Cmd::Screenshot { nav }) => {
            let mut args = vec!["screenshot".to_string()];
            if let Some(n) = nav {
                args.push("--nav".into());
                args.push(n);
            }
            let status = launcher::run_app(&args)?;
            if !status.success() {
                anyhow::bail!("will-ia-app screenshot saiu com {status}");
            }
            Ok(())
        }
        Some(Cmd::Serve) => {
            let _lock = acquire_instance_lock(cli.port, false)?;
            serve(opts, false, false, false)
        }
        None => {
            let Some(_lock) = acquire_instance_lock(cli.port, true)? else {
                return Ok(());
            };
            serve(opts, true, !cli.no_tray, cli.show)
        }
    }
}

/// Instância única por diretório de dados (`instance.lock`). Se o trinco está
/// ocupado e `interactive`, a instância ativa já está no ar: só abre o app e sai.
fn acquire_instance_lock(
    _port: Option<u16>,
    interactive: bool,
) -> anyhow::Result<Option<will_platform::instance::InstanceLock>> {
    let paths = will_core::AppPaths::resolve();
    match will_platform::instance::try_lock(&paths.data_dir) {
        Ok(l) => Ok(Some(l)),
        Err(will_platform::instance::InstanceLockError::Busy) => {
            if interactive {
                // Já há um servidor: o duplo clique vira "abrir o app".
                let _ = launcher::open_app(&[]);
                Ok(None)
            } else {
                anyhow::bail!(
                    "já existe uma instância do will-ia usando {}",
                    paths.data_dir.display()
                )
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Sobe o serviço. `resident`: ignora sinais (modo bandeja); senão encerra
/// limpo com Ctrl+C/SIGTERM. `with_tray`: tenta a bandeja quando há desktop.
fn serve(
    opts: will_server::RunOptions,
    resident: bool,
    with_tray: bool,
    open_app: bool,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("will-rt")
        .build()?;
    let handle = rt.block_on(will_server::run(opts))?;
    ipc::write_port_file(&handle.state.paths, handle.addr.port());
    // `POST /api/ui/show` e `/api/ui/screenshot` (segunda execução, automação):
    // delegam ao app.
    *handle.state.ui_hook.write() = Some(std::sync::Arc::new(|cmd: &str| {
        if cmd == "show" {
            let _ = launcher::open_app(&[]);
        } else if let Some(rest) = cmd.strip_prefix("screenshot") {
            let mut args = vec!["screenshot".to_string()];
            if let Some(n) = rest.strip_prefix(':') {
                args.push("--nav".into());
                args.push(n.to_string());
            }
            std::thread::spawn(move || {
                let _ = launcher::run_app(&args);
            });
        }
    }));
    tracing::info!(url = %handle.url(), "will-ia (servidor) no ar");
    // macOS: a KB costuma morar em Documentos/Mesa/iCloud, pastas sob TCC. Tocar
    // nela já no start faz o pedido "Arquivos e Pastas" sair de uma vez; se já foi
    // negado, avisa com o caminho de Ajustes (ADR-0023, `docs/27`).
    #[cfg(target_os = "macos")]
    if let Some(kb) = handle.state.kb_path() {
        if will_platform::dir_access(&kb) == will_platform::DirAccess::Denied {
            tracing::warn!(
                kb = %kb.display(),
                "sem acesso à base de conhecimento: libere em Ajustes → Privacidade e Segurança → Arquivos e Pastas → will-ia"
            );
            will_platform::notify(
                "will-ia: sem acesso à base de conhecimento",
                "Libere em Ajustes → Privacidade e Segurança → Arquivos e Pastas → will-ia.",
            );
        }
    }
    if !resident {
        println!(
            "will-ia (headless) no ar: {}  — token em {}",
            handle.url(),
            handle.state.paths.session_token_file().display()
        );
        println!("(Ctrl+C para encerrar)");
    }

    let tray_handle = if with_tray && launcher::has_desktop() {
        tray::start(&rt)
    } else {
        None
    };
    if open_app {
        let _ = launcher::open_app(&[]);
    }

    let wait = move || {
        rt.block_on(async move {
            if resident {
                ignore_signals();
                handle.cancelled().await;
            } else {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = handle.cancelled() => {}
                }
                println!("encerrando…");
                handle.shutdown().await;
            }
        });
    };
    // macOS com bandeja: a thread principal pertence ao run loop do AppKit (sem
    // ele o NSStatusItem não desenha nem recebe cliques); a espera do serviço vai
    // para outra thread e encerra o processo quando o serviço cair (ADR-0022).
    #[cfg(target_os = "macos")]
    if tray_handle.is_some() {
        std::thread::Builder::new()
            .name("will-wait".into())
            .spawn(move || {
                wait();
                std::process::exit(0);
            })?;
        will_platform::macos::run_main_loop();
    }
    wait();
    drop(tray_handle);
    Ok(())
}

/// Serviço residente: SIGINT/SIGTERM/SIGHUP não derrubam (só kill forçado).
/// Os filhos `claude` morrem junto mesmo no kill -9 (PDEATHSIG / Job Object).
fn ignore_signals() {
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                break;
            }
            tracing::warn!(
                "SIGINT ignorado: o will-ia é residente; para encerrar use kill -9 (ou o Gerenciador de Tarefas)"
            );
        }
    });
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        for (kind, name) in [
            (SignalKind::terminate(), "SIGTERM"),
            (SignalKind::hangup(), "SIGHUP"),
        ] {
            tokio::spawn(async move {
                let Ok(mut sig) = signal(kind) else { return };
                while sig.recv().await.is_some() {
                    tracing::warn!(
                        "{name} ignorado: o will-ia é residente; para encerrar use kill -9"
                    );
                }
            });
        }
    }
}
