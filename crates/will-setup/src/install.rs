//! Orquestração: a sequência de passos de instalação/atualização e de remoção,
//! comum ao assistente gráfico e à CLI. Cada passo registra no log e avança a
//! barra de progresso.

use crate::os;
use crate::plan::Plan;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Options {
    pub autostart: bool,
    pub desktop_shortcut: bool,
    pub start_after: bool,
    pub open_app_after: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            autostart: true,
            desktop_shortcut: true,
            start_after: true,
            open_app_after: true,
        }
    }
}

/// Relato de progresso: fração 0..1 + linha de log.
pub type Progress = Arc<dyn Fn(f32, &str) + Send + Sync>;

/// Acrescenta uma linha a `<data_dir>/logs/setup.log` (o servidor envia esse
/// arquivo pela telemetria no próximo start, se ela estiver ligada).
pub fn append_setup_log(data_dir: &std::path::Path, msg: &str) {
    let dir = data_dir.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("setup.log"))
    {
        let _ = writeln!(
            f,
            "{} setup {} {}: {msg}",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%z"),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        );
    }
}

pub struct Outcome {
    pub ok: bool,
    pub summary: String,
    pub port: Option<u16>,
}

pub fn install(plan: &Plan, opts: &Options, progress: Progress) -> Outcome {
    let steps: &[(&str, f32)] = &[
        ("verificar", 0.05),
        ("parar", 0.25),
        ("gravar", 0.55),
        ("serviço", 0.70),
        ("atalhos", 0.80),
        ("manifesto", 0.85),
        ("iniciar", 1.0),
    ];
    let p = |i: usize, msg: &str| progress(steps[i].1, msg);
    let log = |msg: &str| progress(-1.0, msg);
    let log_ref: &os::Log<'_> = &log;

    p(
        0,
        &format!(
            "{} will-ia {} em {}",
            if plan.is_update() {
                "Atualizando para"
            } else {
                "Instalando"
            },
            plan.new_version,
            plan.install_dir.display()
        ),
    );
    if !plan.payload_ok {
        return fail("este instalador não contém os binários (compilado sem payload)");
    }
    if let Err(e) = &plan.writable {
        return fail(&format!(
            "sem permissão de escrita em {}: {e}",
            plan.install_dir.display()
        ));
    }
    if plan.data_exists {
        log(&format!(
            "dados existentes preservados em {} (banco {})",
            plan.data_dir.display(),
            crate::plan::human_bytes(plan.db_bytes)
        ));
    }

    p(1, "Parando serviço e processos em execução…");
    if let Err(e) = os::stop_everything(plan, log_ref) {
        return fail(&format!("não consegui parar o will-ia: {e}"));
    }

    p(2, "Gravando binários…");
    if let Err(e) = os::write_binaries(plan, log_ref) {
        return fail(&e);
    }
    // macOS: assinatura com identidade estável, para o TCC/Keychain não pedirem
    // permissão de novo a cada atualização (ADR-0023).
    #[cfg(target_os = "macos")]
    match crate::sign::sign_install(plan, log_ref) {
        Ok(s) => log(&match s.identity {
            Some(id) => format!("assinado com \"{id}\" — {}", s.designated),
            None => format!("assinado ad-hoc — {}", s.designated),
        }),
        Err(e) => log(&format!("aviso: assinatura: {e}")),
    }

    p(
        3,
        if opts.autostart {
            "Registrando início com o sistema…"
        } else {
            "Removendo início com o sistema…"
        },
    );
    if let Err(e) = os::install_service(plan, opts.autostart, log_ref) {
        return fail(&format!("serviço: {e}"));
    }

    p(4, "Atalhos…");
    if let Err(e) = os::install_shortcuts(plan, opts.desktop_shortcut, log_ref) {
        log(&format!("aviso: atalhos: {e}"));
    }

    p(5, "Gravando manifesto…");
    if let Err(e) = os::write_manifest(plan, opts.autostart, opts.desktop_shortcut) {
        log(&format!("aviso: manifesto: {e}"));
    }

    let mut port = None;
    if opts.start_after {
        p(6, "Iniciando o servidor…");
        match os::start_server(plan, opts.autostart, log_ref) {
            Ok(()) => match os::wait_health(&plan.data_dir, Duration::from_secs(40)) {
                Ok(pt) => {
                    port = Some(pt);
                    log(&format!("API local no ar em http://127.0.0.1:{pt}"));
                    if opts.open_app_after && plan.has_desktop {
                        match os::open_app(plan) {
                            Ok(()) => log("app aberto"),
                            Err(e) => log(&format!("aviso: não consegui abrir o app: {e}")),
                        }
                    }
                }
                Err(e) => {
                    log(&format!(
                        "aviso: servidor não respondeu a tempo ({e}); veja os logs em {}",
                        plan.data_dir.join("logs").display()
                    ));
                    let tail = os::server_log_tail(&plan.data_dir, 12);
                    if !tail.is_empty() {
                        log(&format!("últimas linhas dos logs do servidor:\n{tail}"));
                    }
                }
            },
            Err(e) => log(&format!("aviso: não consegui iniciar o servidor: {e}")),
        }
    } else {
        p(6, "Concluído (servidor não iniciado por opção)");
    }
    Outcome {
        ok: true,
        summary: format!(
            "will-ia {} {} em {}",
            plan.new_version,
            if plan.is_update() {
                "atualizado"
            } else {
                "instalado"
            },
            plan.install_dir.display()
        ),
        port,
    }
}

pub fn uninstall(plan: &Plan, purge: bool, progress: Progress) -> Outcome {
    let log = |msg: &str| progress(-1.0, msg);
    let log_ref: &os::Log<'_> = &log;
    progress(0.1, "Parando serviço e processos…");
    if let Err(e) = os::stop_everything(plan, log_ref) {
        return fail(&format!("não consegui parar o will-ia: {e}"));
    }
    progress(0.4, "Removendo início com o sistema…");
    if let Err(e) = os::install_service(plan, false, log_ref) {
        log(&format!("aviso: {e}"));
    }
    progress(0.6, "Removendo atalhos…");
    os::remove_shortcuts(plan, log_ref);
    progress(0.8, "Removendo binários…");
    if let Err(e) = os::remove_binaries(plan, log_ref) {
        return fail(&e);
    }
    os::remove_manifest(plan);
    if purge {
        progress(0.9, "Removendo dados…");
        if let Err(e) = os::purge_data(plan, log_ref) {
            return fail(&e);
        }
    } else {
        log(&format!("dados mantidos em {}", plan.data_dir.display()));
    }
    progress(1.0, "Remoção concluída");
    Outcome {
        ok: true,
        summary: if purge {
            "will-ia removido, inclusive os dados".into()
        } else {
            format!(
                "will-ia removido; dados preservados em {}",
                plan.data_dir.display()
            )
        },
        port: None,
    }
}

fn fail(msg: &str) -> Outcome {
    Outcome {
        ok: false,
        summary: msg.to_string(),
        port: None,
    }
}
