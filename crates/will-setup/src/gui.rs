//! Assistente gráfico (egui/eframe) com a identidade do app: barra lateral com
//! as etapas, conteúdo com quebra de linha, verificações em linhas (título +
//! detalhe), botão principal destacado. A instalação roda numa thread; a UI só
//! consome o progresso.

use crate::install::{self, Options, Outcome};
use crate::plan::{human_bytes, Plan};
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Stroke};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

// ------------------------------------------------------------------ tema

struct Palette {
    window: Color32,
    panel: Color32,
    card: Color32,
    border: Color32,
    text: Color32,
    text_weak: Color32,
    accent: Color32,
    side: Color32,
    side_text: Color32,
    side_weak: Color32,
}

const LIGHT: Palette = Palette {
    window: Color32::from_rgb(244, 246, 250),
    panel: Color32::from_rgb(250, 251, 253),
    card: Color32::from_rgb(255, 255, 255),
    border: Color32::from_rgb(222, 227, 236),
    text: Color32::from_rgb(31, 36, 48),
    text_weak: Color32::from_rgb(106, 114, 130),
    accent: Color32::from_rgb(47, 112, 230),
    side: Color32::from_rgb(21, 25, 33),
    side_text: Color32::from_rgb(226, 230, 237),
    side_weak: Color32::from_rgb(142, 150, 166),
};
const DARK: Palette = Palette {
    window: Color32::from_rgb(17, 20, 26),
    panel: Color32::from_rgb(21, 25, 33),
    card: Color32::from_rgb(27, 32, 41),
    border: Color32::from_rgb(40, 46, 58),
    text: Color32::from_rgb(226, 230, 237),
    text_weak: Color32::from_rgb(142, 150, 166),
    accent: Color32::from_rgb(79, 140, 255),
    side: Color32::from_rgb(12, 15, 20),
    side_text: Color32::from_rgb(226, 230, 237),
    side_weak: Color32::from_rgb(142, 150, 166),
};
fn pal(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}
const OK: Color32 = Color32::from_rgb(52, 168, 83);
const WARN: Color32 = Color32::from_rgb(232, 148, 20);
const BAD: Color32 = Color32::from_rgb(217, 48, 37);

const FONT_MEDIUM: &str = "inter-medium";
const FONT_SEMIBOLD: &str = "inter-semibold";

fn setup_theme(ctx: &egui::Context) {
    use egui::epaint::text::{FontData, FontDefinitions, FontFamily};
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        (
            "inter",
            &include_bytes!("../../will-app/assets/fonts/Inter-Regular.ttf")[..],
        ),
        (
            FONT_MEDIUM,
            &include_bytes!("../../will-app/assets/fonts/Inter-Medium.ttf")[..],
        ),
        (
            FONT_SEMIBOLD,
            &include_bytes!("../../will-app/assets/fonts/Inter-SemiBold.ttf")[..],
        ),
        (
            "noto-emoji",
            &include_bytes!("../../will-app/assets/fonts/NotoEmoji.ttf")[..],
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    let mut fallback: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fallback.insert(0, "noto-emoji".into());
    let mut prop = vec!["inter".to_string()];
    prop.extend(fallback.iter().cloned());
    fonts.families.insert(FontFamily::Proportional, prop);
    for name in [FONT_MEDIUM, FONT_SEMIBOLD] {
        let mut fam = vec![name.to_string()];
        fam.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), fam);
    }
    if let Some(m) = fonts.families.get_mut(&FontFamily::Monospace) {
        m.push("noto-emoji".into());
    }
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|style| {
        use egui::{FontId, TextStyle};
        style.text_styles = [
            (
                TextStyle::Small,
                FontId::new(12.5, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(14.5, FontFamily::Name(FONT_MEDIUM.into())),
            ),
            (
                TextStyle::Heading,
                FontId::new(22.0, FontFamily::Name(FONT_SEMIBOLD.into())),
            ),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(16.0, 8.0);
        style.spacing.interact_size = egui::vec2(36.0, 32.0);
        style.spacing.scroll = egui::style::ScrollStyle::thin();
        for w in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            w.corner_radius = 8.0.into();
        }
    });
    ctx.set_visuals_of(egui::Theme::Light, visuals(false));
    ctx.set_visuals_of(egui::Theme::Dark, visuals(true));
}

fn visuals(dark: bool) -> egui::Visuals {
    let p = if dark { &DARK } else { &LIGHT };
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.window;
    v.window_fill = p.card;
    v.extreme_bg_color = if dark { p.window } else { Color32::WHITE };
    v.faint_bg_color = p.panel;
    v.code_bg_color = if dark {
        Color32::from_rgb(12, 15, 20)
    } else {
        Color32::from_rgb(238, 241, 246)
    };
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.linear_multiply(if dark { 0.45 } else { 0.30 });
    v.selection.stroke = Stroke::new(1.0, p.accent);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text_weak);
    w.inactive.bg_fill = if dark {
        Color32::from_rgb(38, 44, 56)
    } else {
        Color32::from_rgb(234, 238, 245)
    };
    w.inactive.weak_bg_fill = w.inactive.bg_fill;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_fill = if dark {
        Color32::from_rgb(48, 55, 70)
    } else {
        Color32::from_rgb(226, 231, 240)
    };
    w.hovered.weak_bg_fill = w.hovered.bg_fill;
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.linear_multiply(0.7));
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.active.bg_fill = p.accent.linear_multiply(0.6);
    w.active.weak_bg_fill = w.active.bg_fill;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v
}

// ------------------------------------------------------------------ widgets

/// Texto que quebra linha na largura disponível (nunca corre para fora da janela).
fn wrapped(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).wrap());
}

fn medium(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FONT_MEDIUM.into()))
}
fn semibold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FONT_SEMIBOLD.into()))
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 20.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(egui::pos2(rect.center().x, rect.top() + 9.0), 4.5, color);
}

/// Linha de verificação: bolinha + título + detalhe (com quebra).
fn check_row(ui: &mut egui::Ui, color: Color32, title: &str, detail: &str) {
    let p = pal(ui);
    ui.horizontal_top(|ui| {
        dot(ui, color);
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).font(medium(14.5)));
            wrapped(ui, RichText::new(detail).color(p.text_weak).size(13.5));
        });
    });
    ui.add_space(4.0);
}

/// Opção com interruptor + descrição.
fn option_row(ui: &mut egui::Ui, on: &mut bool, enabled: bool, title: &str, detail: &str) {
    let p = pal(ui);
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal_top(|ui| {
            ui.add(egui::Checkbox::without_text(on));
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(title).font(medium(14.5)));
                wrapped(ui, RichText::new(detail).color(p.text_weak).size(13.5));
            });
        });
    });
    ui.add_space(6.0);
}

fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let p = pal(ui);
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).color(Color32::WHITE).font(medium(14.5)))
            .fill(p.accent)
            .corner_radius(CornerRadius::same(8))
            .min_size(egui::vec2(132.0, 36.0)),
    )
}

fn secondary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(text).min_size(egui::vec2(100.0, 36.0)))
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let p = pal(ui);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

// ------------------------------------------------------------------ app

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Checks,
    Options,
    Working,
    Done,
}

impl Step {
    fn index(self) -> usize {
        match self {
            Step::Welcome => 0,
            Step::Checks => 1,
            Step::Options => 2,
            Step::Working => 3,
            Step::Done => 4,
        }
    }
}

const STEP_NAMES: [&str; 5] = [
    "Boas-vindas",
    "Verificações",
    "Opções",
    "Instalação",
    "Concluído",
];

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

    fn action_label(&self) -> &'static str {
        if self.uninstall {
            "Remover"
        } else if self.plan.is_update() {
            "Atualizar"
        } else {
            "Instalar"
        }
    }

    fn redetect(&mut self) {
        let dir = Some(std::path::PathBuf::from(self.custom_dir.trim()));
        self.plan = Plan::detect(dir);
    }
}

impl eframe::App for SetupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        let ctx = ui.ctx().clone();
        let p = pal(ui);

        // ---- barra lateral: produto + etapas
        egui::Panel::left("side")
            .exact_size(232.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.side)
                    .inner_margin(Margin::symmetric(22, 26)),
            )
            .show(ui, |ui| {
                ui.label(
                    RichText::new("will-ia")
                        .font(semibold(26.0))
                        .color(p.side_text),
                );
                ui.label(
                    RichText::new(format!("versão {}", self.plan.new_version))
                        .color(p.side_weak)
                        .size(13.0),
                );
                ui.add_space(28.0);
                let current = self.step.index();
                for (i, name) in STEP_NAMES.iter().enumerate() {
                    let name = if self.uninstall && i == 3 {
                        "Remoção"
                    } else {
                        name
                    };
                    let (fill, txt, weight) = if i == current {
                        (p.accent, Color32::WHITE, p.side_text)
                    } else if i < current {
                        (OK, Color32::WHITE, p.side_text)
                    } else {
                        (Color32::from_rgb(45, 52, 66), p.side_weak, p.side_weak)
                    };
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 11.0, fill);
                        let glyph = if i < current {
                            "✓".to_string()
                        } else {
                            (i + 1).to_string()
                        };
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            glyph,
                            egui::FontId::new(12.0, egui::FontFamily::Name(FONT_SEMIBOLD.into())),
                            txt,
                        );
                        ui.add_space(4.0);
                        ui.label(RichText::new(name).color(weight).font(if i == current {
                            medium(14.5)
                        } else {
                            egui::FontId::proportional(14.5)
                        }));
                    });
                    ui.add_space(10.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    wrapped(
                        ui,
                        RichText::new("Instalação por usuário.\nSem senha de administrador.")
                            .color(p.side_weak)
                            .size(12.5),
                    );
                });
            });

        // ---- rodapé: botões
        egui::Panel::bottom("bottom")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.border))
                    .inner_margin(Margin::symmetric(24, 14)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| match self.step {
                    Step::Welcome => {
                        if secondary_button(ui, "Cancelar").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if primary_button(ui, "Avançar  ›", true).clicked() {
                                self.step = Step::Checks;
                            }
                        });
                    }
                    Step::Checks => {
                        if secondary_button(ui, "‹  Voltar").clicked() {
                            self.step = Step::Welcome;
                        }
                        if secondary_button(ui, "Verificar de novo").clicked() {
                            self.redetect();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let can = self.uninstall || self.plan.can_install();
                            if primary_button(ui, "Avançar  ›", can).clicked() {
                                self.step = Step::Options;
                            }
                        });
                    }
                    Step::Options => {
                        if secondary_button(ui, "‹  Voltar").clicked() {
                            self.step = Step::Checks;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if primary_button(ui, self.action_label(), true).clicked() {
                                self.start(&ctx);
                            }
                        });
                    }
                    Step::Working => {
                        ui.spinner();
                        ui.label(
                            RichText::new("Aguarde, não feche esta janela…").color(p.text_weak),
                        );
                    }
                    Step::Done => {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if primary_button(ui, "Concluir", true).clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                    }
                });
            });

        // ---- conteúdo
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(p.window)
                    .inner_margin(Margin::symmetric(28, 24)),
            )
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match self.step {
                            Step::Welcome => self.ui_welcome(ui),
                            Step::Checks => self.ui_checks(ui),
                            Step::Options => self.ui_options(ui),
                            Step::Working | Step::Done => self.ui_progress(ui),
                        }
                    });
            });
    }
}

impl SetupApp {
    fn title(ui: &mut egui::Ui, title: &str, subtitle: &str) {
        let p = pal(ui);
        ui.label(RichText::new(title).font(semibold(24.0)));
        ui.add_space(2.0);
        wrapped(ui, RichText::new(subtitle).color(p.text_weak).size(14.5));
        ui.add_space(18.0);
    }

    fn ui_welcome(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        let plan = self.plan.clone();
        if self.uninstall {
            Self::title(
                ui,
                "Remover o will-ia",
                "Este assistente remove o servidor e o app desta conta de usuário.",
            );
            card(ui, |ui| {
                wrapped(ui, format!(
                    "Os dados (banco, segredos, WhatsApp, logs) em {} são preservados, a menos que você peça para apagar na próxima etapa.",
                    plan.data_dir.display()
                ));
            });
            return;
        }
        let (t, s) = if plan.is_update() {
            ("Atualizar o will-ia", "Uma versão já está instalada. O assistente troca os binários e religa o serviço, sem mexer nos seus dados.")
        } else {
            ("Bem-vindo ao will-ia", "O assistente instala o servidor e o app para o seu usuário, sem senha de administrador.")
        };
        Self::title(ui, t, s);
        if let Some(v) = plan.installed_version() {
            card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Instalado").color(p.text_weak));
                    ui.label(RichText::new(v).font(medium(15.0)));
                    ui.label(RichText::new("→").color(p.text_weak));
                    ui.label(RichText::new("Novo").color(p.text_weak));
                    ui.label(
                        RichText::new(&plan.new_version)
                            .font(medium(15.0))
                            .color(p.accent),
                    );
                });
            });
            ui.add_space(12.0);
        }
        ui.label(RichText::new("O que vai acontecer").font(medium(15.0)));
        ui.add_space(4.0);
        for (icon, text) in [
            (
                "1",
                format!(
                    "Gravar o servidor (will-ia) e o app (will-ia-app) em {}.",
                    plan.install_dir.display()
                ),
            ),
            (
                "2",
                format!(
                    "Registrar o servidor para iniciar com o sistema: {}.",
                    plan.service_kind.label()
                ),
            ),
            ("3", "Criar o atalho do app.".to_string()),
            (
                "4",
                "Se já houver uma versão rodando: parar o serviço, trocar os binários e religar."
                    .to_string(),
            ),
        ] {
            ui.horizontal_top(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), 10.0, p.accent.linear_multiply(0.18));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    icon,
                    medium(12.0),
                    p.accent,
                );
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    wrapped(ui, text);
                });
            });
        }
        ui.add_space(14.0);
        card(ui, |ui| {
            wrapped(ui, RichText::new("Nada que você configura dentro do will-ia (Telegram, WhatsApp, Google, Claude) é alterado. A pasta de dados fica onde está.").color(p.text_weak));
        });
    }

    fn ui_checks(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        let plan = self.plan.clone();
        Self::title(
            ui,
            "Verificações",
            "O que foi encontrado nesta máquina. Nada foi alterado ainda.",
        );
        card(ui, |ui| {
            check_row(
                ui,
                OK,
                "Sistema",
                &format!(
                    "{} {}{}",
                    plan.os,
                    plan.arch,
                    if plan.has_desktop {
                        ""
                    } else {
                        " — sem sessão gráfica"
                    }
                ),
            );
            check_row(
                ui,
                if plan.payload_ok { OK } else { BAD },
                "Binários no instalador",
                &if plan.payload_ok {
                    format!(
                        "will-ia e will-ia-app {} ({})",
                        plan.new_version,
                        human_bytes(plan.payload_bytes)
                    )
                } else {
                    "Ausentes: este instalador foi compilado sem os binários.".into()
                },
            );
            check_row(
                ui,
                if plan.writable.is_ok() { OK } else { BAD },
                "Pasta de instalação",
                &match &plan.writable {
                    Ok(()) => format!("{} (com permissão de escrita)", plan.install_dir.display()),
                    Err(e) => format!("{} — {e}", plan.install_dir.display()),
                },
            );
            check_row(
                ui,
                OK,
                "Instalação existente",
                &match plan.installed_version() {
                    Some(v) => format!("Versão {v} em {}", plan.install_dir.display()),
                    None => {
                        if plan.existing.is_some() {
                            "Binários presentes, versão desconhecida.".into()
                        } else {
                            "Nenhuma: instalação nova.".into()
                        }
                    }
                },
            );
            check_row(
                ui,
                OK,
                "Dados",
                &if plan.data_exists {
                    format!(
                        "{} — serão preservados (banco {})",
                        plan.data_dir.display(),
                        human_bytes(plan.db_bytes)
                    )
                } else {
                    format!("{} (será criada)", plan.data_dir.display())
                },
            );
            check_row(
                ui,
                if plan.service_installed { OK } else { WARN },
                "Início com o sistema",
                &format!(
                    "{} — {}{}",
                    plan.service_kind.label(),
                    if plan.service_installed {
                        "registrado"
                    } else {
                        "ainda não registrado"
                    },
                    if plan.service_running {
                        ", em execução"
                    } else {
                        ""
                    }
                ),
            );
            check_row(
                ui,
                if plan.running.is_empty() { OK } else { WARN },
                "Processos",
                &if plan.running.is_empty() {
                    "Nenhum will-ia em execução.".into()
                } else {
                    format!(
                        "{} em execução, serão encerrados: {}",
                        plan.running.len(),
                        plan.running
                            .iter()
                            .map(|(n, pid)| format!("{n} ({pid})"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
            );
            if !plan.legacy.is_empty() {
                check_row(
                    ui,
                    WARN,
                    "Instalação manual anterior",
                    &format!("{}. Será substituída, com backup.", plan.legacy.join("; ")),
                );
            }
        });
        if !plan.warnings.is_empty() {
            ui.add_space(12.0);
            Frame::new()
                .fill(WARN.linear_multiply(0.12))
                .stroke(Stroke::new(1.0, WARN.linear_multiply(0.5)))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for w in &plan.warnings {
                        wrapped(
                            ui,
                            RichText::new(format!("⚠  {w}")).color(if ui.visuals().dark_mode {
                                Color32::from_rgb(255, 196, 110)
                            } else {
                                Color32::from_rgb(140, 80, 0)
                            }),
                        );
                    }
                });
        }
        if !self.uninstall {
            ui.add_space(14.0);
            egui::CollapsingHeader::new(RichText::new("Avançado: pasta de instalação").color(p.text_weak))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.custom_dir).desired_width(ui.available_width() - 110.0));
                        if ui.button("Aplicar").clicked() {
                            self.redetect();
                        }
                    });
                    wrapped(ui, RichText::new("No macOS a pasta é o próprio .app; no Linux e no Windows é a pasta onde ficam os dois binários.").color(p.text_weak).size(13.0));
                });
        }
    }

    fn ui_options(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        if self.uninstall {
            Self::title(ui, "Opções da remoção", "O que fazer com os dados.");
            card(ui, |ui| {
                option_row(
                    ui,
                    &mut self.purge,
                    true,
                    "Apagar também os dados",
                    &format!(
                        "Banco, segredos, WhatsApp e logs em {}. Isto é irreversível.",
                        self.plan.data_dir.display()
                    ),
                );
            });
            return;
        }
        Self::title(
            ui,
            "Opções",
            "Pode deixar como está: são os padrões recomendados.",
        );
        let has_desktop = self.plan.has_desktop;
        let service = self.plan.service_kind.label().to_string();
        let start_after = self.opts.start_after;
        card(ui, |ui| {
            option_row(
                ui,
                &mut self.opts.autostart,
                true,
                "Iniciar com o sistema",
                &format!("Registra o servidor no login do usuário: {service}."),
            );
            option_row(
                ui,
                &mut self.opts.desktop_shortcut,
                has_desktop,
                "Atalho na área de trabalho",
                "Além da entrada no menu/Launchpad.",
            );
            option_row(
                ui,
                &mut self.opts.start_after,
                true,
                "Iniciar o servidor ao terminar",
                "Sobe o serviço assim que os arquivos estiverem no lugar.",
            );
            option_row(
                ui,
                &mut self.opts.open_app_after,
                has_desktop && start_after,
                "Abrir o app ao terminar",
                "Abre a janela do will-ia já conectada ao servidor local.",
            );
        });
        ui.add_space(14.0);
        wrapped(
            ui,
            RichText::new(format!(
                "Resumo: {} em {}  ·  dados em {}",
                self.action_label().to_lowercase(),
                self.plan.install_dir.display(),
                self.plan.data_dir.display()
            ))
            .color(p.text_weak)
            .size(13.0),
        );
    }

    fn ui_progress(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        if let Some(o) = &self.outcome {
            let (t, c) = if o.ok {
                ("Tudo pronto", OK)
            } else {
                ("Não deu certo", BAD)
            };
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 14.0, c);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if o.ok { "✓" } else { "!" },
                    semibold(16.0),
                    Color32::WHITE,
                );
                ui.label(RichText::new(t).font(semibold(24.0)));
            });
            ui.add_space(6.0);
            wrapped(ui, RichText::new(&o.summary).size(15.0));
            if let Some(pt) = o.port {
                wrapped(
                    ui,
                    RichText::new(format!("API local: http://127.0.0.1:{pt}")).color(p.text_weak),
                );
            }
        } else {
            Self::title(
                ui,
                if self.uninstall {
                    "Removendo…"
                } else if self.plan.is_update() {
                    "Atualizando…"
                } else {
                    "Instalando…"
                },
                &self.status,
            );
        }
        ui.add_space(10.0);
        ui.add(
            egui::ProgressBar::new(self.progress)
                .desired_height(10.0)
                .corner_radius(CornerRadius::same(5))
                .fill(p.accent),
        );
        ui.add_space(14.0);
        ui.label(
            RichText::new("Registro")
                .font(medium(14.0))
                .color(p.text_weak),
        );
        ui.add_space(4.0);
        Frame::new()
            .fill(ui.visuals().code_bg_color)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .id_salt("log")
                    .max_height(260.0)
                    .stick_to_bottom(true)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for l in &self.log {
                            wrapped(ui, RichText::new(l).monospace().size(12.5));
                        }
                    });
            });
    }
}

pub fn run(plan: Plan, uninstall: bool) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 640.0])
            .with_min_inner_size([820.0, 580.0])
            .with_title(if uninstall {
                "will-ia — remoção"
            } else {
                "will-ia — instalação"
            })
            .with_icon(window_icon()),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "will-ia-setup",
        options,
        Box::new(move |cc| {
            setup_theme(&cc.egui_ctx);
            Ok(Box::new(SetupApp::new(plan, uninstall)))
        }),
    )
}

fn window_icon() -> Arc<egui::IconData> {
    Arc::new(match will_platform::icon::app_rgba(256) {
        Ok(i) => egui::IconData {
            rgba: i.data,
            width: i.width,
            height: i.height,
        },
        Err(_) => egui::IconData::default(),
    })
}
