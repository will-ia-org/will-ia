//! Widgets e helpers compartilhados pelas páginas (formulários, badges, toasts,
//! campo de segredo, modal de confirmação, formatação).

use egui::{Color32, RichText, Ui};
use std::time::{Duration, Instant};

/// Largura máxima do conteúdo das páginas de formulário (centralizado).
pub const CONTENT_MAX_WIDTH: f32 = 1040.0;
pub const LABEL_WIDTH: f32 = 230.0;

/// Estilo global: tipografia maior, controles com altura uniforme, cantos suaves.
pub fn apply_style(style: &mut egui::Style) {
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(12.5, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(14.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(22.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.5, FontFamily::Monospace),
        ),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 7.0);
    style.spacing.interact_size = egui::vec2(40.0, 30.0);
    style.spacing.combo_width = 280.0;
    style.spacing.text_edit_width = 280.0;
    style.spacing.indent = 18.0;
    for w in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        w.corner_radius = 7.0.into();
    }
    style.visuals.window_corner_radius = 10.0.into();
    style.visuals.menu_corner_radius = 8.0.into();
}

/// Item do menu lateral: linha inteira clicável, com destaque quando selecionado.
pub fn nav_item(
    ui: &mut Ui,
    selected: bool,
    label: &str,
    dot_tone: Option<Tone>,
) -> egui::Response {
    let h = 34.0;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click());
    let dark = ui.visuals().dark_mode;
    let fill = if selected {
        Color32::from_rgb(60, 120, 220).linear_multiply(if dark { 0.35 } else { 0.18 })
    } else if resp.hovered() {
        ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.6)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 7.0, fill);
    let text_color = if selected {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    ui.painter().text(
        egui::pos2(rect.left() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(15.0),
        text_color,
    );
    if let Some(t) = dot_tone {
        ui.painter().circle_filled(
            egui::pos2(rect.right() - 14.0, rect.center().y),
            4.0,
            t.color(dark),
        );
    }
    resp
}

// ---------- cores / tons ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Success,
    Warning,
    Danger,
    Info,
}

impl Tone {
    pub fn color(self, dark: bool) -> Color32 {
        match (self, dark) {
            (Tone::Neutral, true) => Color32::from_rgb(140, 150, 170),
            (Tone::Neutral, false) => Color32::from_rgb(90, 100, 120),
            (Tone::Success, _) => Color32::from_rgb(34, 160, 100),
            (Tone::Warning, _) => Color32::from_rgb(210, 140, 20),
            (Tone::Danger, _) => Color32::from_rgb(210, 60, 60),
            (Tone::Info, _) => Color32::from_rgb(60, 120, 220),
        }
    }
}

pub fn tone_for_module(state: &will_core::ModuleStatus) -> Tone {
    use will_core::ModuleStatus as S;
    match state {
        S::Running => Tone::Success,
        S::Starting => Tone::Info,
        S::Degraded(_) => Tone::Warning,
        S::Error(_) => Tone::Danger,
        S::Disabled | S::Stopped => Tone::Neutral,
    }
}

pub fn module_label(state: &will_core::ModuleStatus) -> String {
    use will_core::ModuleStatus as S;
    match state {
        S::Running => "ativo".into(),
        S::Starting => "iniciando".into(),
        S::Degraded(d) => format!("degradado · {d}"),
        S::Error(d) => format!("erro · {d}"),
        S::Disabled => "desligado".into(),
        S::Stopped => "parado".into(),
    }
}

pub fn module_name(m: will_core::Module) -> &'static str {
    use will_core::Module as M;
    match m {
        M::Engine => "Motor Claude",
        M::Telegram => "Telegram",
        M::Realtime => "Voz",
        M::Mcp => "Servidor MCP",
        M::WhatsApp => "WhatsApp",
        M::Google => "Conta Google",
        M::Calendar => "Agenda",
        M::Gmail => "Gmail",
    }
}

// ---------- pequenos widgets ----------

pub fn badge(ui: &mut Ui, text: &str, tone: Tone) {
    let dark = ui.visuals().dark_mode;
    let color = tone.color(dark);
    let fill = color.linear_multiply(if dark { 0.25 } else { 0.15 });
    egui::Frame::new()
        .fill(fill)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).small());
        });
}

pub fn dot(ui: &mut Ui, tone: Tone) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 4.0, tone.color(ui.visuals().dark_mode));
}

/// Interruptor estilo "switch".
pub fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(44.0, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let visuals = ui.style().interact_selectable(&resp, *on);
    let bg = if *on {
        Color32::from_rgb(60, 120, 220)
    } else {
        visuals.bg_fill
    };
    let r = rect.height() / 2.0;
    ui.painter().rect_filled(rect, r, bg);
    let cx = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
    ui.painter()
        .circle_filled(egui::pos2(cx, rect.center().y), r - 3.0, Color32::WHITE);
    resp
}

pub fn heading(ui: &mut Ui, title: &str, description: Option<&str>) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong().size(24.0));
    if let Some(d) = description {
        ui.add(egui::Label::new(RichText::new(d).weak()).wrap());
    }
    ui.add_space(4.0);
}

pub fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(10.0);
    let dark = ui.visuals().dark_mode;
    let fill = if dark {
        Color32::from_rgb(32, 35, 44)
    } else {
        Color32::from_rgb(250, 250, 252)
    };
    let stroke = egui::Stroke::new(
        1.0,
        if dark {
            Color32::from_rgb(52, 56, 68)
        } else {
            Color32::from_rgb(222, 225, 232)
        },
    );
    egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(16.5));
            ui.add_space(10.0);
            add(ui);
        });
}

/// Linha de formulário: rótulo (+dica) à esquerda, widget à direita.
pub fn field(ui: &mut Ui, label: &str, hint: Option<&str>, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LABEL_WIDTH);
            ui.set_max_width(LABEL_WIDTH);
            ui.add_space(5.0);
            ui.label(RichText::new(label).strong());
            if let Some(h) = hint {
                ui.add(egui::Label::new(RichText::new(h).weak().small()).wrap());
            }
        });
        ui.add_space(12.0);
        ui.vertical(|ui| {
            let w = ui.available_width();
            ui.set_width(w);
            ui.set_max_width(w);
            add(ui);
        });
    });
    ui.add_space(8.0);
}

/// Menu de contexto (botão direito) com Recortar / Copiar / Colar para campos de texto.
pub fn text_context_menu(resp: &egui::Response) {
    let id = resp.id;
    resp.context_menu(|ui| {
        let ctx = ui.ctx().clone();
        if ui.button("Colar").clicked() {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.close();
        }
        if ui.button("Copiar").clicked() {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
            ui.close();
        }
        if ui.button("Recortar").clicked() {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut);
            ui.close();
        }
    });
}

pub fn text_input(ui: &mut Ui, value: &mut String, hint: &str) -> egui::Response {
    let r = ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
    text_context_menu(&r);
    r
}

pub fn text_area(ui: &mut Ui, value: &mut String, rows: usize) -> egui::Response {
    let r = ui.add(
        egui::TextEdit::multiline(value)
            .desired_rows(rows)
            .desired_width(f32::INFINITY),
    );
    text_context_menu(&r);
    r
}

pub fn number<N: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
) -> egui::Response {
    ui.add(egui::DragValue::new(value).range(range).speed(1.0))
}

pub fn optional_number(ui: &mut Ui, value: &mut Option<f64>, hint: &str) {
    let mut text = value.map(|v| v.to_string()).unwrap_or_default();
    let r = ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text(hint)
            .desired_width(120.0),
    );
    if r.changed() {
        *value = text.trim().replace(',', ".").parse().ok();
    }
}

pub fn optional_u32(ui: &mut Ui, value: &mut Option<u32>, hint: &str) {
    let mut text = value.map(|v| v.to_string()).unwrap_or_default();
    let r = ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text(hint)
            .desired_width(120.0),
    );
    if r.changed() {
        *value = text.trim().parse().ok();
    }
}

pub fn optional_text(ui: &mut Ui, value: &mut Option<String>, hint: &str) {
    let mut text = value.clone().unwrap_or_default();
    if text_input(ui, &mut text, hint).changed() {
        *value = if text.trim().is_empty() {
            None
        } else {
            Some(text)
        };
    }
}

/// Combo de strings (`value` recebe a chave escolhida).
pub fn select(ui: &mut Ui, id: &str, value: &mut String, options: &[(&str, &str)]) -> bool {
    let current = options
        .iter()
        .find(|(k, _)| *k == value.as_str())
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| value.clone());
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(260.0)
        .show_ui(ui, |ui| {
            for (k, l) in options {
                if ui.selectable_label(value == k, *l).clicked() {
                    *value = k.to_string();
                    changed = true;
                }
            }
        });
    changed
}

/// Lista de linhas (uma por linha num textarea) ↔ `Vec<String>`.
pub fn lines_editor(ui: &mut Ui, buf: &mut String, rows: usize) -> egui::Response {
    let r = ui.add(
        egui::TextEdit::multiline(buf)
            .desired_rows(rows)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    );
    text_context_menu(&r);
    r
}

pub fn parse_lines(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

pub fn parse_ids(s: &str) -> Vec<i64> {
    s.split(|c: char| c == ',' || c == '\n' || c == ';' || c.is_whitespace())
        .filter_map(|t| t.trim().parse().ok())
        .collect()
}

pub fn primary_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(Color32::from_rgb(60, 120, 220)),
    )
}

pub fn danger_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(Color32::from_rgb(200, 60, 60)),
    )
}

pub fn busy(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(RichText::new(text).weak());
    });
}

pub fn error_text(ui: &mut Ui, e: &str) {
    ui.colored_label(Tone::Danger.color(ui.visuals().dark_mode), e);
}

// ---------- campo de segredo ----------

#[derive(Default)]
pub struct SecretField {
    pub input: String,
    pub reveal: bool,
}

pub enum SecretAction {
    None,
    Save(String),
    Delete,
}

impl SecretField {
    pub fn ui(&mut self, ui: &mut Ui, present: bool, hint: &str) -> SecretAction {
        let mut action = SecretAction::None;
        ui.horizontal(|ui| {
            if present {
                badge(ui, "configurado", Tone::Success);
            } else {
                badge(ui, "não configurado", Tone::Warning);
            }
        });
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 270.0).max(160.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .password(!self.reveal)
                    .hint_text(hint)
                    .desired_width(w),
            );
            text_context_menu(&r);
            if ui
                .button(if self.reveal { "ocultar" } else { "ver" })
                .clicked()
            {
                self.reveal = !self.reveal;
            }
            let can_save = !self.input.trim().is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new("Salvar"))
                .clicked()
            {
                action = SecretAction::Save(self.input.trim().to_string());
                self.input.clear();
            }
            if present && ui.button("remover").clicked() {
                action = SecretAction::Delete;
            }
        });
        action
    }
}

// ---------- toasts ----------

pub struct Toast {
    pub text: String,
    pub tone: Tone,
    pub at: Instant,
}

#[derive(Default)]
pub struct Toasts {
    pub items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, tone: Tone, text: impl Into<String>) {
        self.items.push(Toast {
            text: text.into(),
            tone,
            at: Instant::now(),
        });
    }
    pub fn success(&mut self, text: impl Into<String>) {
        self.push(Tone::Success, text);
    }
    pub fn error(&mut self, text: impl Into<String>) {
        self.push(Tone::Danger, text);
    }
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Tone::Info, text);
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        let ttl = Duration::from_secs(6);
        self.items.retain(|t| t.at.elapsed() < ttl);
        if self.items.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -16.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let dark = ui.visuals().dark_mode;
                let mut remove = None;
                for (i, t) in self.items.iter().enumerate() {
                    let color = t.tone.color(dark);
                    let r = egui::Frame::popup(ui.style())
                        .fill(if dark {
                            Color32::from_rgb(30, 34, 44)
                        } else {
                            Color32::WHITE
                        })
                        .stroke(egui::Stroke::new(1.0, color))
                        .corner_radius(8.0)
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.horizontal(|ui| {
                                dot(ui, t.tone);
                                ui.label(&t.text);
                            });
                        });
                    if r.response.interact(egui::Sense::click()).clicked() {
                        remove = Some(i);
                    }
                    ui.add_space(4.0);
                }
                if let Some(i) = remove {
                    self.items.remove(i);
                }
            });
    }
}

// ---------- confirmação ----------

pub struct Confirm {
    pub title: String,
    pub body: String,
    pub confirm_label: String,
    pub danger: bool,
    pub tag: String,
}

/// Mostra o modal; devolve `Some(true)` se confirmou, `Some(false)` se cancelou.
pub fn confirm_modal(ctx: &egui::Context, c: &Confirm) -> Option<bool> {
    let mut out = None;
    let m = egui::Modal::new(egui::Id::unique(("confirm", &c.tag))).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading(&c.title);
        ui.add_space(6.0);
        ui.label(&c.body);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Cancelar").clicked() {
                out = Some(false);
            }
            let r = if c.danger {
                danger_button(ui, &c.confirm_label)
            } else {
                primary_button(ui, &c.confirm_label)
            };
            if r.clicked() {
                out = Some(true);
            }
        });
    });
    if m.should_close() && out.is_none() {
        out = Some(false);
    }
    out
}

// ---------- formatação ----------

pub fn fmt_dt(v: &serde_json::Value) -> String {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d/%m %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn fmt_dt_chrono(d: &chrono::DateTime<chrono::Utc>) -> String {
    d.with_timezone(&chrono::Local)
        .format("%d/%m %H:%M")
        .to_string()
}

pub fn fmt_usd(v: f64) -> String {
    format!("US$ {v:.4}")
}

pub fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

pub fn fmt_relative(d: &chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - *d).num_seconds().max(0);
    if secs < 60 {
        "agora".into()
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else if secs < 86400 {
        format!("{} h", secs / 3600)
    } else {
        format!("{} d", secs / 86400)
    }
}

pub fn s(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

pub fn b(v: &serde_json::Value, key: &str) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}

pub fn i(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

/// Texto selecionável com quebra de linha (para respostas e logs).
pub fn wrapped_text(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(text).wrap().selectable(true));
}

pub fn mono(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(RichText::new(text).monospace().small())
            .wrap()
            .selectable(true),
    );
}

/// Campo numérico opcional para inteiros com sinal (ex.: ids de chat do Telegram).
pub fn optional_i64(ui: &mut Ui, value: &mut Option<i64>, hint: &str) {
    let mut text = value.map(|v| v.to_string()).unwrap_or_default();
    let r = ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text(hint)
            .desired_width(160.0),
    );
    if r.changed() {
        *value = text.trim().parse().ok();
    }
}
