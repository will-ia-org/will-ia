//! Modo texto: o mesmo assistente, em perguntas no terminal (servidor sem tela,
//! automação com `--yes`).

use crate::install::{self, Options};
use crate::plan::{human_bytes, Plan};
use std::io::Write;
use std::sync::Arc;

pub fn print_plan(plan: &Plan) {
    println!(
        "will-ia-setup {} — {} {}",
        plan.new_version, plan.os, plan.arch
    );
    println!("  instalar em : {}", plan.install_dir.display());
    println!(
        "  dados em    : {} {}",
        plan.data_dir.display(),
        if plan.data_exists {
            format!("(existem; banco {})", human_bytes(plan.db_bytes))
        } else {
            "(novo)".into()
        }
    );
    match plan.installed_version() {
        Some(v) => println!("  instalado   : {v}"),
        None => println!("  instalado   : nenhum"),
    }
    println!(
        "  serviço     : {} — {}{}",
        plan.service_kind.label(),
        if plan.service_installed {
            "registrado"
        } else {
            "não registrado"
        },
        if plan.service_running {
            ", em execução"
        } else {
            ""
        }
    );
    if !plan.running.is_empty() {
        println!("  processos   : {:?}", plan.running);
    }
    for l in &plan.legacy {
        println!("  legado      : {l}");
    }
    for w in &plan.warnings {
        println!("  ⚠ {w}");
    }
}

fn ask(q: &str, default: bool) -> bool {
    print!("{q} [{}] ", if default { "S/n" } else { "s/N" });
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return default;
    }
    match s.trim().to_lowercase().as_str() {
        "" => default,
        "s" | "sim" | "y" | "yes" => true,
        _ => false,
    }
}

pub fn run_install(plan: &Plan, mut opts: Options, yes: bool) -> i32 {
    print_plan(plan);
    if !plan.can_install() {
        eprintln!("não dá para instalar (veja os avisos acima)");
        return 2;
    }
    if !yes {
        opts.autostart = ask("Iniciar com o sistema?", opts.autostart);
        if plan.has_desktop {
            opts.desktop_shortcut = ask("Criar atalho na área de trabalho?", opts.desktop_shortcut);
        } else {
            opts.desktop_shortcut = false;
            opts.open_app_after = false;
        }
        opts.start_after = ask("Iniciar o servidor ao terminar?", opts.start_after);
        if !ask(
            &format!(
                "{} agora?",
                if plan.is_update() {
                    "Atualizar"
                } else {
                    "Instalar"
                }
            ),
            true,
        ) {
            println!("cancelado");
            return 1;
        }
    }
    let data_dir = plan.data_dir.clone();
    let progress: install::Progress = Arc::new(move |f, msg| {
        install::append_setup_log(&data_dir, msg);
        if f >= 0.0 {
            println!("[{:>3}%] {msg}", (f * 100.0) as u32);
        } else {
            println!("       {msg}");
        }
    });
    let out = install::install(plan, &opts, progress);
    println!("{}", out.summary);
    if out.ok {
        0
    } else {
        3
    }
}

pub fn run_uninstall(plan: &Plan, purge: bool, yes: bool) -> i32 {
    print_plan(plan);
    if plan.existing.is_none() && !plan.service_installed {
        println!("nada instalado em {}", plan.install_dir.display());
    }
    if !yes
        && !ask(
            &format!(
                "Remover o will-ia{}?",
                if purge {
                    " E TODOS OS DADOS"
                } else {
                    " (dados preservados)"
                }
            ),
            false,
        )
    {
        println!("cancelado");
        return 1;
    }
    let data_dir = plan.data_dir.clone();
    let progress: install::Progress = Arc::new(move |f, msg| {
        install::append_setup_log(&data_dir, msg);
        if f >= 0.0 {
            println!("[{:>3}%] {msg}", (f * 100.0) as u32);
        } else {
            println!("       {msg}");
        }
    });
    let out = install::uninstall(plan, purge, progress);
    println!("{}", out.summary);
    if out.ok {
        0
    } else {
        3
    }
}
