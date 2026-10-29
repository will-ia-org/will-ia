//! Assistente gráfico (egui/eframe): Boas-vindas → Verificações → Opções →
//! Instalação (log ao vivo) → Concluído. A instalação roda numa thread; a UI só
//! consome o progresso.

use crate::install::{self, Options, Outcome};
use crate::plan::{human_bytes, Plan};
use eframe::egui::{self, Color32, RichText};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Checks,
    Options,
    Working,
    Done,
}

enum Msg {
    Progress(f32, String),
    Finished(Outcome),
}

pub struct SetupApp {
    step: Step,
    plan: Plan,
    opts: Options,
    uninstall: bool,
    purge: bool,
    custom_dir: String,
    log: Vec<String>,
    progress: f32,
    status: String,
    rx: Option<Receiver<Msg>>,
    outcome: Option<Outcome>,
}

impl SetupApp {
    pub fn new(plan: Plan, uninstall: bool) -> Self {
        let custom_dir = plan.install_dir.display().to_string();
        Self {
            step: Step::Welcome,
            plan,
            opts: Options::default(),
            uninstall,
            purge: false,
            custom_dir,
            log: vec![],
            progress: 0.0,
            status: String::new(),
            rx: None,
            outcome: None,
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel::<Msg>();
        self.rx = Some(rx);
        self.step = Step::Working;
        self.log.clear();
        let plan = self.plan.clone();
        let opts = self.opts.clone();
        let uninstall = self.uninstall;
        let purge = self.purge;
        let ctx2 = ctx.clone();
        let tx2 = tx.clone();
        let progress: install::Progress = Arc::new(move |f, m| {
            let _ = tx2.send(Msg::Progress(f, m.to_string()));
            ctx2.request_repaint();
        });
        let ctx3 = ctx.clone();
        std::thread::spawn(move || {
            let out = if uninstall {
                install::uninstall(&plan, purge, progress)
            } else {
                install::install(&plan, &opts, progress)
            };
            let _ = tx.send(Msg::Finished(out));
            ctx3.request_repaint();
        });
    }

    fn pump(&mut self) {
        let mut done = None;
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Progress(f, text) => {
                        if f >= 0.0 {
                            self.progress = f;
                            self.status = text.clone();
                        }
                        self.log.push(text);
                    }
                    Msg::Finished(o) => done = Some(o),
                }
            }
        }
        if let Some(o) = done {
            self.progress = 1.0;
            self.outcome = Some(o);
            self.step = Step::Done;
            self.rx = None;
        }
    }
}

fn ok_dot(ui: &mut egui::Ui, ok: Option<bool>) {
    let (c, t) = match ok {
        Some(true) => (Color32::from_rgb(52, 168, 83), "●"),
        Some(false) => (Color32::from_rgb(217, 48, 37), "●"),
        None => (Color32::from_rgb(242, 153, 0), "●"),
    };
    ui.label(RichText::new(t).color(c).size(13.0));
}

impl eframe::App for SetupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        let ctx = ui.ctx().clone();
        let title = if self.uninstall {
            "Remover o will-ia"
        } else if self.plan.is_update() {
            "Atualizar o will-ia"
        } else {
            "Instalar o will-ia"
        };
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.heading(title);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("versão {}", self.plan.new_version)).weak());
                });
            });
            ui.add_space(6.0);
        });
        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| match self.step {
                Step::Welcome => {
                    if ui.button("Cancelar").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Avançar ›").clicked() {
                            self.step = Step::Checks;
                        }
                    });
                }
                Step::Checks => {
                    if ui.button("‹ Voltar").clicked() {
                        self.step = Step::Welcome;
                    }
                    if ui.button("Verificar de novo").clicked() {
                        let dir = Some(std::path::PathBuf::from(self.custom_dir.trim()));
                        self.plan = Plan::detect(dir);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can = self.uninstall || self.plan.can_install();
                        if ui
                            .add_enabled(can, egui::Button::new("Avançar ›"))
                            .clicked()
                        {
                            self.step = Step::Options;
                        }
                    });
                }
                Step::Options => {
                    if ui.button("‹ Voltar").clicked() {
                        self.step = Step::Checks;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.uninstall {
                            "Remover"
                        } else if self.plan.is_update() {
                            "Atualizar"
                        } else {
                            "Instalar"
                        };
                        if ui.button(RichText::new(label).strong()).clicked() {
                            self.start(&ctx);
                        }
                    });
                }
                Step::Working => {
                    ui.label(RichText::new("Aguarde…").weak());
                }
                Step::Done => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Concluir").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                }
            });
            ui.add_space(8.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(6.0);
            match self.step {
                Step::Welcome => self.ui_welcome(ui),
                Step::Checks => self.ui_checks(ui),
                Step::Options => self.ui_options(ui),
                Step::Working | Step::Done => self.ui_progress(ui),
            }
        });
    }
}

impl SetupApp {
    fn ui_welcome(&mut self, ui: &mut egui::Ui) {
        let p = &self.plan;
        if self.uninstall {
            ui.label("Este assistente remove o servidor will-ia e o app will-ia-app desta conta de usuário.");
            ui.add_space(8.0);
            ui.label(format!("Os dados (banco, segredos, WhatsApp, logs) em {} são preservados, a menos que você peça para apagar.", p.data_dir.display()));
            return;
        }
        ui.label(format!(
            "Este assistente instala o will-ia {} para o seu usuário — sem senha de administrador.",
            p.new_version
        ));
        ui.add_space(8.0);
        ui.label("O que ele faz:");
        ui.label("  •  grava o servidor (will-ia) e o app (will-ia-app) na pasta do usuário;");
        ui.label(format!(
            "  •  registra o servidor para iniciar com o sistema ({});",
            p.service_kind.label()
        ));
        ui.label("  •  cria o atalho do app;");
        ui.label(
            "  •  se já houver uma versão rodando, para o serviço, troca os binários e religa.",
        );
        ui.add_space(8.0);
        ui.label(RichText::new("Nada que você configura dentro do will-ia (Telegram, WhatsApp, Google, Claude) é tocado: os dados ficam onde estão.").weak());
        if let Some(v) = p.installed_version() {
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("Versão instalada: {v}  →  nova: {}", p.new_version))
                    .strong(),
            );
        }
    }

    fn ui_checks(&mut self, ui: &mut egui::Ui) {
        let p = &self.plan;
        egui::Grid::new("checks")
            .num_columns(3)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ok_dot(ui, Some(true));
                ui.label("Sistema");
                ui.label(format!(
                    "{} {}{}",
                    p.os,
                    p.arch,
                    if p.has_desktop {
                        ""
                    } else {
                        " (sem sessão gráfica)"
                    }
                ));
                ui.end_row();

                ok_dot(ui, Some(p.payload_ok));
                ui.label("Binários no instalador");
                ui.label(if p.payload_ok {
                    format!(
                        "will-ia + will-ia-app {} ({})",
                        p.new_version,
                        human_bytes(p.payload_bytes)
                    )
                } else {
                    "AUSENTES — este instalador foi compilado sem os binários".into()
                });
                ui.end_row();

                ok_dot(ui, Some(p.writable.is_ok()));
                ui.label("Pasta de instalação");
                ui.label(match &p.writable {
                    Ok(()) => format!("{} (gravável)", p.install_dir.display()),
                    Err(e) => format!("{} — {e}", p.install_dir.display()),
                });
                ui.end_row();

                ok_dot(ui, Some(true));
                ui.label("Instalação existente");
                ui.label(match p.installed_version() {
                    Some(v) => format!("versão {v} em {}", p.install_dir.display()),
                    None => {
                        if p.existing.is_some() {
                            "binários presentes (versão desconhecida)".into()
                        } else {
                            "nenhuma — instalação nova".into()
                        }
                    }
                });
                ui.end_row();

                ok_dot(ui, Some(true));
                ui.label("Dados");
                ui.label(if p.data_exists {
                    format!(
                        "{} — serão preservados (banco {})",
                        p.data_dir.display(),
                        human_bytes(p.db_bytes)
                    )
                } else {
                    format!("{} (será criada)", p.data_dir.display())
                });
                ui.end_row();

                ok_dot(
                    ui,
                    if p.service_installed {
                        Some(true)
                    } else {
                        None
                    },
                );
                ui.label("Início com o sistema");
                ui.label(format!(
                    "{} — {}{}",
                    p.service_kind.label(),
                    if p.service_installed {
                        "registrado"
                    } else {
                        "não registrado"
                    },
                    if p.service_running {
                        ", em execução"
                    } else {
                        ""
                    }
                ));
                ui.end_row();

                ok_dot(
                    ui,
                    if p.running.is_empty() {
                        Some(true)
                    } else {
                        None
                    },
                );
                ui.label("Processos");
                ui.label(if p.running.is_empty() {
                    "nenhum will-ia em execução".into()
                } else {
                    format!(
                        "{} em execução — serão encerrados: {}",
                        p.running.len(),
                        p.running
                            .iter()
                            .map(|(n, pid)| format!("{n} ({pid})"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                });
                ui.end_row();

                if !p.legacy.is_empty() {
                    ok_dot(ui, None);
                    ui.label("Instalação manual anterior");
                    ui.label(format!(
                        "{} — será substituída (com backup)",
                        p.legacy.join("; ")
                    ));
                    ui.end_row();
                }
            });
        if !p.warnings.is_empty() {
            ui.add_space(10.0);
            for w in &p.warnings {
                ui.label(RichText::new(format!("⚠ {w}")).color(Color32::from_rgb(200, 120, 0)));
            }
        }
        if !self.uninstall {
            ui.add_space(12.0);
            ui.collapsing("Avançado: pasta de instalação", |ui| {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.custom_dir);
                    if ui.button("Aplicar").clicked() {
                        let dir = Some(std::path::PathBuf::from(self.custom_dir.trim()));
                        self.plan = Plan::detect(dir);
                    }
                });
                ui.label(RichText::new("No macOS a pasta é o próprio .app; no Linux/Windows é a pasta dos dois binários.").weak());
            });
        }
    }

    fn ui_options(&mut self, ui: &mut egui::Ui) {
        if self.uninstall {
            ui.checkbox(
                &mut self.purge,
                "Apagar também os dados (banco, segredos, WhatsApp, logs)",
            );
            ui.label(
                RichText::new(format!("Pasta de dados: {}", self.plan.data_dir.display())).weak(),
            );
            if self.purge {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Isto é irreversível.")
                        .color(Color32::from_rgb(217, 48, 37))
                        .strong(),
                );
            }
            return;
        }
        let has_desktop = self.plan.has_desktop;
        ui.checkbox(
            &mut self.opts.autostart,
            format!("Iniciar com o sistema ({})", self.plan.service_kind.label()),
        );
        ui.add_enabled_ui(has_desktop, |ui| {
            ui.checkbox(
                &mut self.opts.desktop_shortcut,
                "Atalho do app na área de trabalho",
            );
        });
        ui.checkbox(&mut self.opts.start_after, "Iniciar o servidor ao terminar");
        ui.add_enabled_ui(has_desktop && self.opts.start_after, |ui| {
            ui.checkbox(&mut self.opts.open_app_after, "Abrir o app ao terminar");
        });
        ui.add_space(12.0);
        ui.label(
            RichText::new(format!(
                "Resumo: {} em {} · dados em {}",
                if self.plan.is_update() {
                    "atualizar"
                } else {
                    "instalar"
                },
                self.plan.install_dir.display(),
                self.plan.data_dir.display()
            ))
            .weak(),
        );
    }

    fn ui_progress(&mut self, ui: &mut egui::Ui) {
        if let Some(o) = &self.outcome {
            ui.horizontal(|ui| {
                ok_dot(ui, Some(o.ok));
                ui.label(RichText::new(&o.summary).strong());
            });
            if let Some(pt) = o.port {
                ui.label(format!("API local: http://127.0.0.1:{pt}"));
            }
        } else {
            ui.label(RichText::new(&self.status).strong());
        }
        ui.add(egui::ProgressBar::new(self.progress).show_percentage());
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for l in &self.log {
                    ui.label(RichText::new(l).monospace().size(12.0));
                }
            });
    }
}

pub fn run(plan: Plan, uninstall: bool) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([700.0, 520.0])
            .with_min_inner_size([600.0, 440.0])
            .with_title("will-ia — instalação"),
        ..Default::default()
    };
    eframe::run_native(
        "will-ia-setup",
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            cc.egui_ctx
                .set_pixels_per_point(cc.egui_ctx.pixels_per_point().max(1.0));
            Ok(Box::new(SetupApp::new(plan, uninstall)))
        }),
    )
}

/// Inter + Noto Emoji (os mesmos do app), para acentos e símbolos saírem certos.
fn install_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontData, FontDefinitions, FontFamily};
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../will-app/assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "noto-emoji".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../will-app/assets/fonts/NotoEmoji.ttf"
        ))),
    );
    if let Some(p) = fonts.families.get_mut(&FontFamily::Proportional) {
        p.insert(0, "inter".into());
        p.insert(1, "noto-emoji".into());
    }
    if let Some(m) = fonts.families.get_mut(&FontFamily::Monospace) {
        m.push("noto-emoji".into());
    }
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|style| {
        for f in style.text_styles.values_mut() {
            f.size *= 1.1;
        }
    });
}
