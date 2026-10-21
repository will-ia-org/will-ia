//! Janela principal (eframe::App): navegação lateral, páginas, eventos do
//! serviço, bandeja, atalho global, voz e indicador flutuante.

use crate::hotkey::Hotkeys;
use crate::i18n::{tr, trf};
use crate::pages::{Cx, Nav, Pages, SettingsSection, Snapshot};
use crate::service::{Service, UiCommand};
use crate::tray::Tray;
use crate::ui::{self, Toasts, Tone};
use crate::voice::VoiceController;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};
use will_core::{Event, Module};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartWindow {
    Auto,
    Shown,
    Hidden,
}

pub fn run(opts: will_server::RunOptions, start: StartWindow) -> anyhow::Result<()> {
    let (svc, ev_rx, cmd_rx) = Service::start(opts)?;
    let onboarding_done = svc.state.settings().general.onboarding_done;
    let visible = match start {
        StartWindow::Shown => true,
        StartWindow::Hidden => false,
        StartWindow::Auto => !onboarding_done,
    };
    let viewport = egui::ViewportBuilder::default()
        .with_title("will-ia")
        .with_app_id("will-ia")
        .with_inner_size([1180.0, 780.0])
        .with_min_inner_size([900.0, 600.0])
        .with_icon(crate::icon::egui_icon())
        // Barra de título desenhada pelo app (integrada ao fundo); ver `title_bar`.
        .with_decorations(false)
        .with_visible(visible);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let svc2 = svc.clone();
    let result = eframe::run_native(
        "will-ia",
        options,
        Box::new(move |cc| Ok(Box::new(WillApp::new(cc, svc2, ev_rx, cmd_rx, visible)))),
    );
    tracing::info!("janela encerrada; desligando o serviço");
    svc.shutdown();
    result.map_err(|e| anyhow::anyhow!("GUI: {e}"))
}

pub struct WillApp {
    svc: Arc<Service>,
    ev_rx: Receiver<Event>,
    cmd_rx: Receiver<UiCommand>,
    pub nav: Nav,
    pub snap: Snapshot,
    pub pages: Pages,
    pub toasts: Toasts,
    pub voice: VoiceController,
    tray: Tray,
    hotkeys: Hotkeys,
    window_visible: bool,
    quitting: bool,
    last_status_refresh: Instant,
    theme_applied: Option<String>,
    hotkey_applied: Option<String>,
    confirm_quit: bool,
    pending_hide: bool,
    /// Captura agendada (caminho, quadros restantes) para dar tempo à navegação.
    pending_screenshot: Option<(std::path::PathBuf, u8)>,
}

impl WillApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        svc: Arc<Service>,
        ev_rx: Receiver<Event>,
        cmd_rx: Receiver<UiCommand>,
        visible: bool,
    ) -> Self {
        svc.set_ctx(cc.egui_ctx.clone());
        egui_extras::install_image_loaders(&cc.egui_ctx);
        crate::ui::setup(&cc.egui_ctx);
        let tray = Tray::new(&svc);
        let hotkeys = Hotkeys::new(&svc);
        let settings = svc.state.settings();
        crate::i18n::set_language(&settings.general.language);
        let snap = Snapshot {
            settings: settings.clone(),
            modules: svc.state.module_status.read().clone(),
            pending_approvals: svc.state.approvals.pending(),
            portable_root: svc.state.paths.portable_root.clone(),
            data_dir: svc.state.paths.data_dir.clone(),
            db_path: svc.state.paths.db_path(),
            api_url: svc.handle.url(),
        };
        let mut voice = VoiceController::new();
        voice.apply_settings(&settings.realtime);
        let mut pages = Pages::default();
        pages.settings.state.load(&svc);
        let nav = if settings.general.onboarding_done {
            Nav::Dashboard
        } else {
            Nav::Settings(SettingsSection::Engine)
        };
        let mut app = Self {
            svc,
            ev_rx,
            cmd_rx,
            nav,
            snap,
            pages,
            toasts: Toasts::default(),
            voice,
            tray,
            hotkeys,
            window_visible: visible,
            quitting: false,
            last_status_refresh: Instant::now() - Duration::from_secs(60),
            theme_applied: None,
            hotkey_applied: None,
            confirm_quit: false,
            pending_hide: false,
            pending_screenshot: None,
        };
        if !app.tray.available() {
            app.toasts.info(tr(
                "Bandeja indisponível neste desktop: fechar a janela encerra o will-ia.",
            ));
        }
        app.apply_theme(&cc.egui_ctx);
        app
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let scale = self.snap.settings.general.ui_scale.clamp(0.6, 2.0);
        if (ctx.zoom_factor() - scale).abs() > 0.01 {
            ctx.set_zoom_factor(scale);
        }
        let theme = self.snap.settings.general.theme.clone();
        if self.theme_applied.as_deref() == Some(theme.as_str()) {
            return;
        }
        ctx.set_theme(match theme.as_str() {
            "dark" => egui::ThemePreference::Dark,
            "light" => egui::ThemePreference::Light,
            _ => egui::ThemePreference::System,
        });
        self.theme_applied = Some(theme);
    }

    fn apply_hotkey(&mut self) {
        let hk = if self.snap.settings.realtime.enabled {
            self.snap.settings.realtime.hotkey.clone()
        } else {
            String::new()
        };
        if self.hotkey_applied.as_deref() == Some(hk.as_str()) {
            return;
        }
        self.hotkeys.apply(&hk);
        if let Some(e) = &self.hotkeys.error {
            self.toasts.error(trf("Atalho global: {}", &[e]));
        }
        self.hotkey_applied = Some(hk);
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.window_visible = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.window_visible = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn handle_event(&mut self, ev: Event) {
        match &ev {
            Event::ModuleStatus { module, status } => {
                // `Running` em Voz significa "pronta", não "em sessão": a bandeja
                // acompanha os eventos `Voice` (abaixo).
                self.snap.modules.insert(*module, status.clone());
            }
            Event::ApprovalRequested { approval } => {
                self.snap.pending_approvals.retain(|a| a.id != approval.id);
                self.snap.pending_approvals.push(approval.clone());
                self.toasts.push(
                    Tone::Warning,
                    trf("Aprovação pedida: {}", &[&approval.summary]),
                );
            }
            Event::ApprovalDecided { approval_id, .. } => {
                self.snap.pending_approvals.retain(|a| a.id != *approval_id);
            }
            Event::SettingsChanged { .. } => {
                self.snap.settings = self.svc.state.settings();
                self.voice.apply_settings(&self.snap.settings.realtime);
                self.pages.settings.state.load(&self.svc);
            }
            Event::Voice { state, detail } => {
                self.voice.on_voice_event(state, detail.as_deref());
                let active = !matches!(state.as_str(), "stopped" | "error");
                self.tray.set_voice_active(active, &self.svc);
            }
            Event::Notification { title, body } => {
                self.toasts.info(format!("{title}: {body}"));
            }
            Event::CostRecorded {
                source, summary, ..
            } => {
                if source == "voice" {
                    self.toasts.info(summary.clone());
                }
            }
            Event::StaleSession { .. } => {
                self.toasts.info(tr(
                    "Sessão do Claude expirou; a thread recomeçou e reenviou as pendências.",
                ));
            }
            Event::TurnFailed { error, .. } => {
                self.toasts.error(error.clone());
            }
            _ => {}
        }
        self.pages.on_event(&ev);
    }

    fn handle_cmd(&mut self, ctx: &egui::Context, cmd: UiCommand) {
        match cmd {
            UiCommand::Show => self.show_window(ctx),
            UiCommand::ToggleVoice => self.voice.toggle(&self.svc),
            UiCommand::HotkeyDown => {
                if self.voice.ptt_mode {
                    if !self.voice.session_active && !self.voice.busy() {
                        self.voice.start(&self.svc);
                    } else {
                        self.voice.set_ptt(&self.svc, true);
                    }
                } else {
                    self.voice.toggle(&self.svc);
                }
            }
            UiCommand::HotkeyUp => {
                if self.voice.ptt_mode {
                    self.voice.set_ptt(&self.svc, false);
                }
            }
            UiCommand::Screenshot(path, nav) => {
                if let Some(n) = nav.as_deref().and_then(Nav::parse) {
                    self.nav = n;
                } else if let Some(id) = nav.as_deref().and_then(|n| n.strip_prefix("threads/")) {
                    // diagnóstico: abre a página de threads já com a thread selecionada
                    if let Ok(t) = will_core::ThreadId::parse(id) {
                        self.nav = Nav::Threads;
                        self.pages.threads.select(t);
                    }
                }
                self.pending_screenshot = Some((path, 3));
            }
            UiCommand::ScreenshotNow(path) => {
                let cb =
                    egui::ScreenshotCallback::new(move |img: std::sync::Arc<egui::ColorImage>| {
                        let [w, h] = img.size;
                        let mut rgba = Vec::with_capacity(w * h * 4);
                        for px in &img.pixels {
                            rgba.extend_from_slice(&px.to_srgba_unmultiplied());
                        }
                        match image::RgbaImage::from_raw(w as u32, h as u32, rgba) {
                            Some(im) => {
                                if let Err(e) = im.save(&path) {
                                    tracing::warn!(error = %e, "screenshot: falha ao salvar");
                                } else {
                                    tracing::info!(path = %path.display(), "screenshot salvo");
                                }
                            }
                            None => tracing::warn!("screenshot: imagem inválida"),
                        }
                    });
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(cb));
            }
            UiCommand::Quit => self.quit(ctx),
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let mut events = Vec::new();
        while let Ok(ev) = self.ev_rx.try_recv() {
            events.push(ev);
        }
        for ev in events {
            self.handle_event(ev);
        }
        let mut cmds = Vec::new();
        while let Ok(c) = self.cmd_rx.try_recv() {
            cmds.push(c);
        }
        for c in cmds {
            self.handle_cmd(ctx, c);
        }
        if let Some((path, n)) = self.pending_screenshot.take() {
            if n == 0 {
                self.handle_cmd(ctx, UiCommand::ScreenshotNow(path));
            } else {
                self.pending_screenshot = Some((path, n - 1));
                ctx.request_repaint();
            }
        }
        if let Some(r) = self.voice.tick(&self.svc) {
            match r {
                Ok(m) => self.toasts.success(m),
                Err(e) => self.toasts.error(e),
            }
        }
        if self.last_status_refresh.elapsed() > Duration::from_secs(15) {
            self.last_status_refresh = Instant::now();
            self.pages.dashboard.refresh(&self.svc);
        }
        self.apply_hotkey();
    }

    fn nav_panel(&mut self, ui: &mut egui::Ui) {
        let voice_active = self.voice.session_active;
        let pending = self.snap.pending_approvals.len();
        // Rodapé fixo: voz, ocultar/sair, versão.
        egui::Panel::bottom("nav-bottom").show_separator_line(false).show(ui, |ui| {
            ui.add_space(6.0);
            let (label, tone) = if voice_active { (tr("Desligar voz"), Tone::Danger) } else { (tr("Ligar voz"), Tone::Success) };
            let btn = egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE)).fill(tone.color(ui.visuals().dark_mode)).min_size(egui::vec2(ui.available_width(), 34.0));
            if ui.add_enabled(!self.voice.busy() && self.snap.settings.realtime.enabled, btn).clicked() {
                self.voice.toggle(&self.svc);
            }
            if self.voice.session_active {
                let (label, color) = crate::voice::overlay::label_for(&self.voice.state);
                ui.label(egui::RichText::new(label).color(color).small());
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Ocultar")).on_hover_text(tr("O serviço continua em segundo plano; rode `will-ia` de novo para reabrir.")).clicked() {
                    self.pending_hide = true;
                }
                if ui.button(tr("Sair")).on_hover_text(tr("Encerra o serviço, os processos claude e a voz.")).clicked() {
                    self.confirm_quit = true;
                }
            });
            let mut info = format!("v{}", env!("CARGO_PKG_VERSION"));
            if self.snap.portable_root.is_some() {
                info.push_str(tr(" · portátil"));
            }
            if !self.tray.available() {
                info.push_str(tr(" · sem bandeja"));
            }
            ui.label(egui::RichText::new(info).small().weak());
            ui.add_space(4.0);
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("will-ia").font(ui::semibold(18.0)));
        });
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .id_salt("nav-scroll")
            .auto_shrink([false, false])
            .max_height((ui.available_height() - 12.0).max(80.0))
            .show(ui, |ui| {
                let items: Vec<(Nav, String)> = vec![
                    (Nav::Dashboard, tr("Início").into()),
                    (Nav::Threads, tr("Threads").into()),
                    (
                        Nav::Approvals,
                        if pending > 0 {
                            trf("Aprovações ({})", &[&pending])
                        } else {
                            tr("Aprovações").into()
                        },
                    ),
                    (Nav::Logs, tr("Logs").into()),
                    (Nav::Costs, tr("Custos").into()),
                ];
                for (n, label) in items {
                    if ui::nav_item(ui, self.nav == n, &label, None).clicked() {
                        self.nav = n;
                    }
                }
                ui.add_space(14.0);
                ui.label(egui::RichText::new(tr("CONFIGURAÇÕES")).small().weak());
                ui.add_space(2.0);
                for (sec, label) in SettingsSection::ALL.iter().map(|s| (*s, s.label())) {
                    let selected = self.nav == Nav::Settings(sec);
                    let status: Option<(Module, Option<&will_core::ModuleStatus>)> =
                        sec.module().map(|m| (m, self.snap.modules.get(&m)));
                    let dot_tone =
                        status.map(|(_, st)| st.map(ui::tone_for_module).unwrap_or(Tone::Neutral));
                    let mut resp = ui::nav_item(ui, selected, label, dot_tone);
                    if let Some((m, st)) = status {
                        // A bolinha explica a cor ao passar o mouse.
                        let text = match st {
                            Some(st) => ui::module_label(st),
                            None => tr("sem estado ainda").to_string(),
                        };
                        resp = resp.on_hover_text(format!("{}: {text}", ui::module_name(m)));
                    }
                    if resp.clicked() {
                        self.nav = Nav::Settings(sec);
                    }
                }
            });
    }

    /// Barra de título integrada: arrastar move a janela, duplo clique maximiza,
    /// botões minimizar / maximizar / fechar (fechar = ocultar para a bandeja).
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let h = 38.0;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
        let dark = ui.visuals().dark_mode;
        let bg = ui::palette(dark).titlebar;
        ui.painter().rect_filled(rect, 0.0, bg);
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));

        // área de arrastar (tudo menos os botões)
        let btn_w = 46.0;
        let drag_rect =
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - btn_w * 3.0, rect.max.y));
        let drag = ui.interact(
            drag_rect,
            egui::Id::unique("titlebar-drag"),
            egui::Sense::click_and_drag(),
        );
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
        // título
        let icon_c = egui::pos2(rect.left() + 20.0, rect.center().y);
        ui.painter()
            .circle_filled(icon_c, 7.0, egui::Color32::from_rgb(31, 42, 68));
        ui.painter().circle_stroke(
            icon_c,
            4.2,
            egui::Stroke::new(1.4, egui::Color32::from_rgb(150, 170, 220)),
        );
        ui.painter()
            .circle_filled(icon_c, 1.6, egui::Color32::WHITE);
        let title_color = if dark {
            egui::Color32::from_rgb(200, 205, 215)
        } else {
            egui::Color32::from_rgb(40, 44, 54)
        };
        ui.painter().text(
            egui::pos2(rect.left() + 34.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "will-ia",
            egui::FontId::proportional(14.0),
            title_color,
        );
        if self.voice.session_active {
            let (label, color) = crate::voice::overlay::label_for(&self.voice.state);
            ui.painter().circle_filled(
                egui::pos2(rect.left() + 100.0, rect.center().y),
                4.0,
                color,
            );
            ui.painter().text(
                egui::pos2(rect.left() + 110.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                trf("voz · {}", &[&label]),
                egui::FontId::proportional(12.5),
                color,
            );
        }

        // botões
        let mut x = rect.max.x - btn_w;
        let stroke_color = title_color;
        for (i, kind) in ["close", "max", "min"].iter().enumerate() {
            let r = egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(btn_w, h));
            let resp = ui.interact(
                r,
                egui::Id::unique(("titlebar-btn", i)),
                egui::Sense::click(),
            );
            if resp.hovered() {
                let fill = if *kind == "close" {
                    egui::Color32::from_rgb(200, 60, 60)
                } else if dark {
                    egui::Color32::from_rgb(45, 48, 58)
                } else {
                    egui::Color32::from_rgb(220, 223, 230)
                };
                ui.painter().rect_filled(r, 0.0, fill);
            }
            let c = r.center();
            let sc = if resp.hovered() && *kind == "close" {
                egui::Color32::WHITE
            } else {
                stroke_color
            };
            let st = egui::Stroke::new(1.3, sc);
            match *kind {
                "min" => {
                    ui.painter().line_segment(
                        [
                            egui::pos2(c.x - 5.0, c.y + 0.5),
                            egui::pos2(c.x + 5.0, c.y + 0.5),
                        ],
                        st,
                    );
                }
                "max" => {
                    if maximized {
                        ui.painter().rect_stroke(
                            egui::Rect::from_center_size(
                                egui::pos2(c.x - 1.0, c.y + 1.0),
                                egui::vec2(8.0, 8.0),
                            ),
                            1.0,
                            st,
                            egui::StrokeKind::Middle,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(c.x - 2.0, c.y - 3.0),
                                egui::pos2(c.x - 2.0, c.y - 4.0),
                            ],
                            st,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(c.x - 2.0, c.y - 4.0),
                                egui::pos2(c.x + 5.0, c.y - 4.0),
                            ],
                            st,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(c.x + 5.0, c.y - 4.0),
                                egui::pos2(c.x + 5.0, c.y + 3.0),
                            ],
                            st,
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(c.x + 5.0, c.y + 3.0),
                                egui::pos2(c.x + 4.0, c.y + 3.0),
                            ],
                            st,
                        );
                    } else {
                        ui.painter().rect_stroke(
                            egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0)),
                            1.0,
                            st,
                            egui::StrokeKind::Middle,
                        );
                    }
                }
                _ => {
                    ui.painter().line_segment(
                        [
                            egui::pos2(c.x - 5.0, c.y - 5.0),
                            egui::pos2(c.x + 5.0, c.y + 5.0),
                        ],
                        st,
                    );
                    ui.painter().line_segment(
                        [
                            egui::pos2(c.x - 5.0, c.y + 5.0),
                            egui::pos2(c.x + 5.0, c.y - 5.0),
                        ],
                        st,
                    );
                }
            }
            if resp.clicked() {
                match *kind {
                    "min" => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                    "max" => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                    _ => self.pending_hide = true,
                }
            }
            let tip = match *kind {
                "min" => tr("Minimizar"),
                "max" => {
                    if maximized {
                        tr("Restaurar")
                    } else {
                        tr("Maximizar")
                    }
                }
                _ => tr("Fechar (continua na bandeja)"),
            };
            resp.on_hover_text(tip);
            x -= btn_w;
        }
    }

    /// Redimensionar pelas bordas (a janela não tem moldura do sistema).
    fn resize_edges(&self, ctx: &egui::Context) {
        use egui::{CursorIcon, ResizeDirection as D, ViewportCommand as VC};
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else {
            return;
        };
        let r = ctx.content_rect();
        let m = 6.0;
        let left = pos.x <= r.left() + m;
        let right = pos.x >= r.right() - m;
        let top = pos.y <= r.top() + m;
        let bottom = pos.y >= r.bottom() - m;
        let dir = match (left, right, top, bottom) {
            (true, _, true, _) => Some((D::NorthWest, CursorIcon::ResizeNwSe)),
            (_, true, true, _) => Some((D::NorthEast, CursorIcon::ResizeNeSw)),
            (true, _, _, true) => Some((D::SouthWest, CursorIcon::ResizeNeSw)),
            (_, true, _, true) => Some((D::SouthEast, CursorIcon::ResizeNwSe)),
            (true, _, _, _) => Some((D::West, CursorIcon::ResizeHorizontal)),
            (_, true, _, _) => Some((D::East, CursorIcon::ResizeHorizontal)),
            (_, _, true, _) => Some((D::North, CursorIcon::ResizeVertical)),
            (_, _, _, true) => Some((D::South, CursorIcon::ResizeVertical)),
            _ => None,
        };
        if let Some((d, cursor)) = dir {
            ctx.set_cursor_icon(cursor);
            let pressed = ctx.input(|i| i.pointer.primary_pressed());
            if pressed {
                ctx.send_viewport_cmd(VC::BeginResize(d));
            }
        }
    }
}

impl eframe::App for WillApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump(ctx);
        // Idioma da UI vem do snapshot (atualizado por `SettingsChanged`): salvar
        // a seção Geral troca todos os textos no quadro seguinte.
        crate::i18n::set_language(&self.snap.settings.general.language);
        if self.voice.overlay_visible() {
            crate::voice::overlay::show(ctx, &self.voice.overlay);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);

        // Fechar a janela nunca encerra o serviço: ele continua na bandeja (ou, sem
        // bandeja, em segundo plano; rodar `will-ia` de novo reabre a janela).
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(&ctx);
            if !self.tray.available() {
                tracing::info!("janela oculta; sem bandeja neste desktop. Rode `will-ia` de novo para reabrir ou use Sair na janela.");
            }
        }

        egui::Panel::top("titlebar")
            .exact_size(38.0)
            .show_separator_line(false)
            .show(ui, |ui| self.title_bar(ui));

        egui::Panel::left("nav")
            .exact_size(232.0)
            .resizable(false)
            .show(ui, |ui| self.nav_panel(ui));

        // Faixa de aprovações pendentes.
        if !self.snap.pending_approvals.is_empty() && self.nav != Nav::Approvals {
            let n = self.snap.pending_approvals.len();
            egui::Panel::top("approvals-banner").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui::dot(ui, Tone::Warning);
                    ui.label(trf("{} aprovação(ões) pendente(s).", &[&n]));
                    if ui.button(tr("Ver")).clicked() {
                        self.nav = Nav::Approvals;
                    }
                });
            });
        }

        let mut requested_nav: Option<Nav> = None;
        egui::CentralPanel::default().show(ui, |ui| {
            if matches!(self.nav, Nav::Threads | Nav::Logs) {
                // Threads e Logs ocupam a janela exatamente (cabeçalho, caixa de
                // entrada e barra de filtros fixos); só o histórico do chat ou o
                // painel de log rola, dentro da página.
                ui.set_clip_rect(ui.max_rect());
                ui.add_space(4.0);
                let mut cx = Cx {
                    svc: &self.svc,
                    snap: &self.snap,
                    toasts: &mut self.toasts,
                    voice: &mut self.voice,
                    go: &mut requested_nav,
                };
                self.pages.ui(ui, self.nav, &mut cx);
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt(("page", self.nav))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let full = ui.available_width();
                    let max_w = full.min(ui::CONTENT_MAX_WIDTH);
                    let pad = ((full - max_w) / 2.0).max(0.0);
                    ui.add_space(4.0);
                    ui.horizontal_top(|ui| {
                        ui.add_space(pad);
                        ui.vertical(|ui| {
                            ui.set_width(max_w);
                            ui.set_min_width(max_w);
                            let mut cx = Cx {
                                svc: &self.svc,
                                snap: &self.snap,
                                toasts: &mut self.toasts,
                                voice: &mut self.voice,
                                go: &mut requested_nav,
                            };
                            self.pages.ui(ui, self.nav, &mut cx);
                            ui.add_space(24.0);
                        });
                    });
                });
        });
        if let Some(n) = requested_nav {
            self.nav = n;
        }
        if self.pending_hide {
            self.pending_hide = false;
            self.hide_window(&ctx);
        }
        if self.confirm_quit {
            let c = ui::Confirm {
                title: tr("Sair do will-ia").into(),
                body: tr("Encerra o serviço: processos claude, sessão de voz, Telegram e servidor MCP param. Para só fechar a janela, use Ocultar.").into(),
                confirm_label: tr("Sair").into(),
                danger: true,
                tag: "quit".into(),
            };
            if let Some(ok) = ui::confirm_modal(&ctx, &c) {
                self.confirm_quit = false;
                if ok {
                    self.quit(&ctx);
                }
            }
        }
        self.toasts.show(&ctx);
        self.resize_edges(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        tracing::info!("saindo do will-ia");
    }
}
