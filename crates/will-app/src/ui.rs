//! Widgets e helpers compartilhados pelas páginas (formulários, badges, toasts,
//! campo de segredo, modal de confirmação, formatação).

use crate::i18n::{tr, trf};
use egui::{Color32, RichText, Ui};
use std::time::{Duration, Instant};

/// Largura máxima do conteúdo das páginas de formulário (centralizado).
pub const CONTENT_MAX_WIDTH: f32 = 1040.0;
pub const LABEL_WIDTH: f32 = 230.0;

/// Paleta do app (escuro e claro).
pub struct Palette {
    pub window: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub card_alt: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub accent: Color32,
}

pub const DARK: Palette = Palette {
    window: Color32::from_rgb(17, 20, 26),
    panel: Color32::from_rgb(21, 25, 33),
    card: Color32::from_rgb(27, 32, 41),
    card_alt: Color32::from_rgb(33, 39, 50),
    border: Color32::from_rgb(40, 46, 58),
    text: Color32::from_rgb(226, 230, 237),
    text_weak: Color32::from_rgb(142, 150, 166),
    accent: Color32::from_rgb(79, 140, 255),
};

pub const LIGHT: Palette = Palette {
    window: Color32::from_rgb(244, 246, 250),
    panel: Color32::from_rgb(250, 251, 253),
    card: Color32::from_rgb(255, 255, 255),
    card_alt: Color32::from_rgb(243, 245, 249),
    border: Color32::from_rgb(222, 227, 236),
    text: Color32::from_rgb(31, 36, 48),
    text_weak: Color32::from_rgb(106, 114, 130),
    accent: Color32::from_rgb(47, 112, 230),
};

pub fn palette(dark: bool) -> &'static Palette {
    if dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// Famílias extras registradas em `install_fonts`.
pub const FONT_MEDIUM: &str = "inter-medium";
pub const FONT_SEMIBOLD: &str = "inter-semibold";

/// Fonte Inter embutida (OFL) como proporcional padrão, Noto Emoji como reserva
/// para emoji e as fontes do egui para os demais símbolos.
pub fn install_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontData, FontDefinitions, FontFamily};
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        FONT_MEDIUM.into(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Inter-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        FONT_SEMIBOLD.into(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Inter-SemiBold.ttf"
        ))),
    );
    // Noto Emoji (OFL, monocromático): cobertura completa de emoji. O egui não
    // desenha fontes coloridas, então os emoji saem na cor do texto.
    fonts.font_data.insert(
        "noto-emoji".into(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/NotoEmoji.ttf"
        ))),
    );
    // emoji-icon-font (MIT, a mesma do egui): símbolos que nem a Inter nem a Noto
    // Emoji têm (🗐 do botão "copiar" do egui_commonmark, ✔, ↺, ⟳ …). O egui 0.37
    // só a inclui nas famílias com a feature `monochrome_emoji_fonts`.
    fonts.font_data.insert(
        "emoji-icon-font".into(),
        std::sync::Arc::new(FontData::from_static(epaint_default_fonts::EMOJI_ICON)),
    );
    let mut fallback: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fallback.insert(0, "noto-emoji".to_string());
    fallback.push("emoji-icon-font".to_string());
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        mono.push("noto-emoji".to_string());
        mono.push("emoji-icon-font".to_string());
    }
    let mut prop = vec!["inter".to_string()];
    prop.extend(fallback.iter().cloned());
    fonts.families.insert(FontFamily::Proportional, prop);
    for name in [FONT_MEDIUM, FONT_SEMIBOLD] {
        let mut fam = vec![name.to_string()];
        fam.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), fam);
    }
    ctx.set_fonts(fonts);
    // Material Symbols (ícones da UI, `crate::icons`): família própria + reserva
    // de menor prioridade na proporcional. Depois do `set_fonts`, que substitui tudo.
    egui_material_icons::initialize(ctx);
}

// ---------- ícones (Material Symbols; ver src/icons.rs) ----------

/// Ícone solto (rótulo) na cor dada, sem quebra.
pub fn icon_colored(
    ui: &mut Ui,
    ic: crate::icons::MaterialIcon,
    size: f32,
    color: Color32,
) -> egui::Response {
    ui.add(
        egui::Label::new(ic.rich_text().size(size).color(color))
            .wrap_mode(egui::TextWrapMode::Extend)
            .selectable(false),
    )
}

/// Botão só de ícone, sem moldura, com tooltip.
pub fn icon_button(ui: &mut Ui, ic: crate::icons::MaterialIcon, tooltip: &str) -> egui::Response {
    let color = ui.visuals().text_color();
    icon_button_colored(ui, ic, tooltip, color)
}

pub fn icon_button_colored(
    ui: &mut Ui,
    ic: crate::icons::MaterialIcon,
    tooltip: &str,
    color: Color32,
) -> egui::Response {
    let r = ui.add(egui::Button::new(ic.rich_text().size(16.0).color(color)).frame(false));
    if tooltip.is_empty() {
        r
    } else {
        r.on_hover_text(tooltip)
    }
}

/// Ícone + texto num único `WidgetText` (para botões e rótulos): o ícone sai
/// na família Material e o texto na proporcional, mesma cor e tamanho.
pub fn icon_text(
    ui: &Ui,
    ic: crate::icons::MaterialIcon,
    text: &str,
    size: Option<f32>,
) -> egui::WidgetText {
    let size = size.unwrap_or_else(|| egui::TextStyle::Button.resolve(ui.style()).size);
    let color = ui.visuals().text_color();
    icon_text_colored(ic, text, size, color)
}

pub fn icon_text_colored(
    ic: crate::icons::MaterialIcon,
    text: &str,
    size: f32,
    color: Color32,
) -> egui::WidgetText {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.append(
        ic.codepoint,
        0.0,
        TextFormat {
            font_id: egui::FontId::new(size + 2.0, ic.font_family()),
            color,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.append(
        text,
        6.0,
        TextFormat {
            font_id: egui::FontId::proportional(size),
            color,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.into()
}

/// Remove seletores de variação (U+FE0E/U+FE0F) e modificadores de tom de pele,
/// que virariam "caixinhas" numa fonte de emoji monocromática.
pub fn clean_emoji(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(*c, '\u{FE0E}' | '\u{FE0F}' | '\u{1F3FB}'..='\u{1F3FF}'))
        .collect()
}

pub fn medium(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FONT_MEDIUM.into()))
}

pub fn semibold(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(FONT_SEMIBOLD.into()))
}

/// Configura fontes, tamanhos e paleta (escuro e claro). Chamar uma vez na criação.
pub fn setup(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.all_styles_mut(apply_style);
    ctx.set_visuals_of(egui::Theme::Dark, visuals(true));
    ctx.set_visuals_of(egui::Theme::Light, visuals(false));
}

/// Estilo global: tipografia, controles com altura uniforme, cantos suaves.
pub fn apply_style(style: &mut egui::Style) {
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Name(FONT_MEDIUM.into())),
        ),
        (
            TextStyle::Heading,
            FontId::new(20.0, FontFamily::Name(FONT_SEMIBOLD.into())),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size = egui::vec2(36.0, 30.0);
    style.spacing.combo_width = 260.0;
    style.spacing.text_edit_width = 260.0;
    style.spacing.indent = 16.0;
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    for w in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        w.corner_radius = 6.0.into();
    }
    style.visuals.window_corner_radius = 10.0.into();
    style.visuals.menu_corner_radius = 8.0.into();
}

fn visuals(dark: bool) -> egui::Visuals {
    let p = palette(dark);
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.panel;
    v.window_fill = p.card;
    // Fundo de campos de texto: o tom mais escuro da paleta (`window`).
    v.extreme_bg_color = if dark { p.window } else { Color32::WHITE };
    v.faint_bg_color = p.card_alt;
    v.code_bg_color = if dark {
        Color32::from_rgb(12, 15, 20)
    } else {
        Color32::from_rgb(238, 241, 246)
    };
    v.window_stroke = egui::Stroke::new(1.0, p.border);
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.linear_multiply(if dark { 0.45 } else { 0.30 });
    v.selection.stroke = egui::Stroke::new(1.0, p.accent);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.text_weak);
    w.inactive.bg_fill = if dark {
        Color32::from_rgb(38, 44, 56)
    } else {
        Color32::from_rgb(234, 238, 245)
    };
    w.inactive.weak_bg_fill = if dark {
        Color32::from_rgb(33, 39, 50)
    } else {
        Color32::from_rgb(240, 243, 248)
    };
    w.inactive.bg_stroke = egui::Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = egui::Stroke::new(1.0, p.text);
    w.hovered.bg_fill = if dark {
        Color32::from_rgb(48, 55, 70)
    } else {
        Color32::from_rgb(226, 231, 240)
    };
    w.hovered.weak_bg_fill = w.hovered.bg_fill;
    w.hovered.bg_stroke = egui::Stroke::new(1.0, p.accent.linear_multiply(0.7));
    w.hovered.fg_stroke = egui::Stroke::new(1.0, p.text);
    w.active.bg_fill = p.accent.linear_multiply(0.6);
    w.active.weak_bg_fill = w.active.bg_fill;
    w.active.bg_stroke = egui::Stroke::new(1.0, p.accent);
    w.active.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);
    w.open.bg_fill = w.inactive.bg_fill;
    w.open.weak_bg_fill = w.inactive.weak_bg_fill;
    w.open.bg_stroke = egui::Stroke::new(1.0, p.accent.linear_multiply(0.7));
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 120 } else { 40 }),
    };
    v.window_shadow = v.popup_shadow;
    v
}

/// Item do menu lateral: linha inteira clicável, com destaque quando selecionado.
pub fn nav_item(
    ui: &mut Ui,
    selected: bool,
    label: &str,
    dot_tone: Option<Tone>,
) -> egui::Response {
    let h = 32.0;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click());
    let dark = ui.visuals().dark_mode;
    let p = palette(dark);
    let fill = if selected {
        p.accent.linear_multiply(if dark { 0.26 } else { 0.14 })
    } else if resp.hovered() {
        p.card_alt
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 6.0, fill);
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top() + 8.0),
                egui::vec2(3.0, rect.height() - 16.0),
            ),
            2.0,
            p.accent,
        );
    }
    let font = if selected {
        medium(14.0)
    } else {
        egui::FontId::proportional(14.0)
    };
    ui.painter().text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        p.text,
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

/// Cor da bolinha de um módulo (semântica em `will_core::ModuleStatus`):
/// verde = pronto (mesmo ocioso), azul = iniciando, amarelo = habilitado mas
/// falta/falha algo (com motivo), vermelho = erro (com motivo), cinza = desligado
/// nas configurações ou parado explicitamente.
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

/// Rótulo curto do estado (traduzido), sempre com o motivo em `Degraded`/`Error`.
/// `Running` vira "pronto"; a página Início acrescenta detalhes por módulo
/// (processos vivos, "em sessão", número de contas).
pub fn module_label(state: &will_core::ModuleStatus) -> String {
    use will_core::ModuleStatus as S;
    match state {
        S::Running => tr("pronto").into(),
        S::Starting => tr("iniciando").into(),
        S::Degraded(d) => trf("atenção · {}", &[d]),
        S::Error(d) => trf("erro · {}", &[d]),
        S::Disabled => tr("desligado").into(),
        S::Stopped => tr("parado").into(),
    }
}

pub fn module_name(m: will_core::Module) -> &'static str {
    use will_core::Module as M;
    match m {
        M::Engine => tr("Motor Claude"),
        M::Telegram => tr("Telegram"),
        M::Realtime => tr("Voz"),
        M::Mcp => tr("Servidor MCP"),
        M::WhatsApp => tr("WhatsApp"),
        M::Google => tr("Conta Google"),
        M::Calendar => tr("Agenda"),
        M::Gmail => tr("Gmail"),
        // Module::Drive chegou com o lote do Google Drive (backend); nome mínimo
        // aqui só para o match ser exaustivo.
        M::Drive => tr("Drive"),
        M::Scheduler => tr("Agendamentos"),
        M::Triggers => tr("Triggers"),
        M::Browser => tr("Browser CDP"),
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
            // sem quebra interna: num `horizontal_wrapped` o chip inteiro vai para a
            // linha de baixo em vez de virar uma coluna de letras.
            ui.add(
                egui::Label::new(RichText::new(text).color(color).small())
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
}

/// Chip de dois tons: rótulo forte + detalhe fraco ("Gmail · ler, enviar").
pub fn chip2(ui: &mut Ui, head: &str, detail: &str, tone: Tone) {
    let dark = ui.visuals().dark_mode;
    let color = tone.color(dark);
    let fill = color.linear_multiply(if dark { 0.22 } else { 0.12 });
    egui::Frame::new()
        .fill(fill)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.add(
                egui::Label::new(RichText::new(head).color(color).small().strong())
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
            if !detail.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(detail).color(color).small())
                        .wrap_mode(egui::TextWrapMode::Extend),
                );
            }
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
        palette(ui.visuals().dark_mode).accent
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

// ---------- abas ----------

/// Uma aba de [`tabs`]/[`tab_bar`]: chave estável (não traduzida; é o que o
/// deep-link `pagina/aba` usa) + rótulo + bolinha de estado opcional.
pub struct Tab {
    pub key: &'static str,
    pub label: String,
    pub tone: Option<Tone>,
}

impl Tab {
    pub fn new(key: &'static str, label: impl Into<String>) -> Self {
        Self {
            key,
            label: label.into(),
            tone: None,
        }
    }
    pub fn tone(mut self, tone: Option<Tone>) -> Self {
        self.tone = tone;
        self
    }
}

fn tabs_memory_id(page: &str) -> egui::Id {
    egui::Id::unique(("will-tabs", page))
}

/// Aba guardada para `page` (memória do egui, dura a sessão do app).
pub fn selected_tab(ctx: &egui::Context, page: &str) -> Option<String> {
    ctx.data(|d| d.get_temp::<String>(tabs_memory_id(page)))
}

/// Seleciona a aba de uma página sem desenhar nada: deep-link ("ir para
/// Configurações → Google → Gmail") e `screenshot --nav pagina/aba`.
pub fn select_tab(ctx: &egui::Context, page: &str, key: &str) {
    ctx.data_mut(|d| d.insert_temp(tabs_memory_id(page), key.to_string()));
}

/// Barra de abas com estado próprio por `page`: desenha, trata o clique e
/// devolve a chave da aba ativa (a primeira quando nenhuma foi escolhida ou a
/// guardada não existe mais). Cada página longa com seções independentes usa
/// isto logo abaixo do título e desenha só a seção da aba ativa.
pub fn tabs(ui: &mut Ui, page: &str, items: &[Tab]) -> &'static str {
    let Some(first) = items.first() else {
        return "";
    };
    let remembered = selected_tab(ui.ctx(), page);
    let current = items
        .iter()
        .find(|t| remembered.as_deref() == Some(t.key))
        .map(|t| t.key)
        .unwrap_or(first.key);
    match tab_bar(ui, items, current) {
        Some(clicked) if clicked != current => {
            select_tab(ui.ctx(), page, clicked);
            clicked
        }
        _ => current,
    }
}

/// Barra de abas sem estado: desenha com `selected` ativa e devolve a chave
/// clicada neste quadro. Quebra linha quando a janela é estreita.
pub fn tab_bar(ui: &mut Ui, items: &[Tab], selected: &str) -> Option<&'static str> {
    let dark = ui.visuals().dark_mode;
    let p = palette(dark);
    let mut clicked = None;
    ui.add_space(6.0);
    let row = ui
        .horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for t in items {
                let active = t.key == selected;
                let font = if active {
                    medium(14.0)
                } else {
                    egui::FontId::proportional(14.0)
                };
                let galley = ui.painter().layout_no_wrap(t.label.clone(), font, p.text);
                let dot_w = if t.tone.is_some() { 14.0 } else { 0.0 };
                let size = egui::vec2(galley.size().x + 28.0 + dot_w, 32.0);
                let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                if ui.is_rect_visible(rect) {
                    let fill = if active {
                        p.accent.linear_multiply(if dark { 0.26 } else { 0.14 })
                    } else if resp.hovered() {
                        p.card_alt
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, 7.0, fill);
                    if active {
                        ui.painter().rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(rect.left() + 10.0, rect.bottom() - 3.0),
                                egui::pos2(rect.right() - 10.0, rect.bottom() - 1.0),
                            ),
                            1.0,
                            p.accent,
                        );
                    }
                    let mut x = rect.left() + 14.0;
                    if let Some(tone) = t.tone {
                        ui.painter().circle_filled(
                            egui::pos2(x + 4.0, rect.center().y),
                            4.0,
                            tone.color(dark),
                        );
                        x += dot_w;
                    }
                    let color = if active || resp.hovered() {
                        p.text
                    } else {
                        p.text_weak
                    };
                    ui.painter().galley(
                        egui::pos2(x, rect.center().y - galley.size().y / 2.0),
                        galley,
                        color,
                    );
                }
                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.clicked() {
                    clicked = Some(t.key);
                }
            }
        })
        .response;
    // linha de base sob as abas
    let r = row.rect;
    ui.painter().hline(
        r.left()..=r.right(),
        r.bottom() + 2.0,
        egui::Stroke::new(1.0, p.border),
    );
    ui.add_space(2.0);
    clicked
}

pub fn heading(ui: &mut Ui, title: &str, description: Option<&str>) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).font(semibold(21.0)));
    if let Some(d) = description {
        ui.add(egui::Label::new(RichText::new(d).weak().size(13.0)).wrap());
    }
    ui.add_space(2.0);
}

pub fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    section_help(ui, title, None, add);
}

/// Linha de formulário: rótulo (+dica) à esquerda, widget à direita.
pub fn field(ui: &mut Ui, label: &str, hint: Option<&str>, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LABEL_WIDTH);
            ui.set_max_width(LABEL_WIDTH);
            ui.add_space(5.0);
            ui.label(RichText::new(label).font(medium(14.0)));
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
        if ui.button(tr("Colar")).clicked() {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.close();
        }
        if ui.button(tr("Copiar")).clicked() {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
            ui.close();
        }
        if ui.button(tr("Recortar")).clicked() {
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
            .fill(palette(ui.visuals().dark_mode).accent),
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
                badge(ui, tr("configurado"), Tone::Success);
            } else {
                badge(ui, tr("não configurado"), Tone::Warning);
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
            let (eye, eye_tip) = if self.reveal {
                (crate::icons::VISIBILITY_OFF, tr("ocultar"))
            } else {
                (crate::icons::VISIBILITY, tr("ver"))
            };
            if icon_button(ui, eye, eye_tip).clicked() {
                self.reveal = !self.reveal;
            }
            let can_save = !self.input.trim().is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new(tr("Salvar")))
                .clicked()
            {
                action = SecretAction::Save(self.input.trim().to_string());
                self.input.clear();
            }
            if present && ui.button(tr("remover")).clicked() {
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
        // Canto superior direito: fora do caminho do campo de mensagem e dos
        // botões Enviar/Falar/Anexar (embaixo). Somem em 6 s ou com um clique.
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_TOP, [-16.0, 16.0])
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
                                let ic = match t.tone {
                                    Tone::Success => crate::icons::CHECK_CIRCLE,
                                    Tone::Danger => crate::icons::ERROR,
                                    Tone::Warning => crate::icons::WARNING,
                                    _ => crate::icons::INFO,
                                };
                                icon_colored(ui, ic, 17.0, color);
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
            if ui.button(tr("Cancelar")).clicked() {
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
        tr("agora").into()
    } else if secs < 3600 {
        trf("{} min", &[&(secs / 60)])
    } else if secs < 86400 {
        trf("{} h", &[&(secs / 3600)])
    } else {
        trf("{} d", &[&(secs / 86400)])
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

/// Corta um texto em `max` caracteres com reticências (para ids e nomes longos
/// em grades, onde não há quebra de linha).
pub fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
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

// ---------- ajuda (ícone "?" + modal) ----------

/// Conteúdo de uma ajuda: passo a passo e notas, mostrados num modal aberto
/// pelo ícone "?" ao lado do título da seção. Nada disso ocupa espaço na página.
pub struct Help {
    pub title: &'static str,
    pub intro: &'static str,
    /// (título do passo, explicação)
    pub steps: &'static [(&'static str, &'static str)],
    /// (título da nota, texto) — dúvidas comuns, erros, avisos
    pub notes: &'static [(&'static str, &'static str)],
    /// bloco de código/terminal (vazio = nenhum)
    pub code: &'static str,
    /// (rótulo, URL)
    pub links: &'static [(&'static str, &'static str)],
}

fn help_open_id(help: &Help) -> egui::Id {
    egui::Id::unique(("help-open", help.title))
}

/// Ícone "?" clicável. Abre o modal com a ajuda; o estado fica na memória do egui.
pub fn help_icon(ui: &mut Ui, help: &Help) {
    let id = help_open_id(help);
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let p = palette(ui.visuals().dark_mode);
    let r = ui
        .add(
            egui::Button::new(RichText::new("?").font(semibold(12.0)).color(p.accent))
                .corner_radius(10.0)
                .min_size(egui::vec2(20.0, 20.0))
                .stroke(egui::Stroke::new(1.0, p.accent)),
        )
        .on_hover_text(tr("Ajuda e passo a passo"));
    if r.clicked() {
        open = true;
    }
    if open && help_modal(ui.ctx(), help) {
        open = false;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, open));
}

/// Modal da ajuda; devolve `true` quando o usuário fechou.
fn help_modal(ctx: &egui::Context, help: &Help) -> bool {
    let mut close = false;
    let screen = ctx.content_rect();
    let width = (screen.width() - 48.0).clamp(280.0, 620.0);
    let max_h = (screen.height() - 160.0).max(160.0);
    let m = egui::Modal::new(egui::Id::unique(("help-modal", help.title))).show(ctx, |ui| {
        ui.set_width(width);
        let dark = ui.visuals().dark_mode;
        let accent = Tone::Info.color(dark);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr(help.title)).font(semibold(18.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, crate::icons::CLOSE, tr("Fechar")).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt(("help-scroll", help.title))
            .max_height(max_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.set_width(ui.available_width() - 8.0);
                if !help.intro.is_empty() {
                    ui.add(egui::Label::new(RichText::new(tr(help.intro)).size(13.5)).wrap());
                    ui.add_space(8.0);
                }
                for (i, (title, body)) in help.steps.iter().enumerate() {
                    ui.horizontal_top(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 11.0, accent);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            (i + 1).to_string(),
                            semibold(12.0),
                            Color32::WHITE,
                        );
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(tr(title)).font(semibold(14.0)));
                            ui.add(egui::Label::new(RichText::new(tr(body)).weak()).wrap());
                        });
                    });
                    ui.add_space(8.0);
                }
                if !help.code.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tr("No terminal")).font(semibold(13.0)));
                        if ui.button(tr("Copiar")).clicked() {
                            ui.ctx().copy_text(help.code.to_string());
                        }
                    });
                    mono(ui, help.code);
                    ui.add_space(8.0);
                }
                if !help.notes.is_empty() {
                    if !help.steps.is_empty() || !help.code.is_empty() {
                        ui.separator();
                        ui.add_space(4.0);
                    }
                    for (title, body) in help.notes {
                        ui.label(RichText::new(tr(title)).font(semibold(13.5)));
                        ui.add(egui::Label::new(RichText::new(tr(body)).weak()).wrap());
                        ui.add_space(8.0);
                    }
                }
                if !help.links.is_empty() {
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(RichText::new(tr("Links")).font(semibold(13.0)));
                    for (label, url) in help.links {
                        ui.horizontal_wrapped(|ui| {
                            ui.hyperlink_to(tr(label), *url).on_hover_text(*url);
                            if ui.button(tr("copiar")).clicked() {
                                ui.ctx().copy_text(url.to_string());
                            }
                        });
                    }
                    ui.add_space(4.0);
                }
            });
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary_button(ui, tr("Fechar")).clicked() {
                close = true;
            }
        });
    });
    close || m.should_close()
}

/// Cartão de seção com ícone "?" ao lado do título.
pub fn section_help(ui: &mut Ui, title: &str, help: Option<&Help>, add: impl FnOnce(&mut Ui)) {
    ui.add_space(8.0);
    let p = palette(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(p.card)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).font(semibold(15.0)));
                if let Some(h) = help {
                    ui.add_space(2.0);
                    help_icon(ui, h);
                }
            });
            ui.add_space(8.0);
            add(ui);
        });
}

/// Título de página com descrição curta e ícone "?" para a ajuda completa.
pub fn heading_help(ui: &mut Ui, title: &str, description: Option<&str>, help: &Help) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(semibold(21.0)));
        ui.add_space(2.0);
        help_icon(ui, help);
    });
    if let Some(d) = description {
        ui.add(egui::Label::new(RichText::new(d).weak().size(13.0)).wrap());
    }
    ui.add_space(2.0);
}
