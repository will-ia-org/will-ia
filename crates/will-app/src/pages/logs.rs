//! Logs: histórico do banco + cauda ao vivo pelos eventos `Log`, apresentados
//! como um visor de terminal (monoespaçado, denso, uma linha por registro,
//! colunas fixas `hora · nível · módulo · mensagem`).
//!
//! Dados: `GET /api/logs` devolve `[{id, at (RFC 3339), level, module, message}]`
//! do mais novo para o mais antigo (`will-storage::logs::LogRecord`); os eventos
//! `Event::Log` trazem os mesmos campos em tempo real (inclusive `debug`, que o
//! banco não guarda).

use super::Cx;
use crate::i18n::{tr, trf};
use crate::service::Loader;
use crate::ui::{self, Tone};
use egui::{Color32, FontId, TextFormat};
use serde_json::Value;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use will_core::Event;

/// Quantas linhas pedir ao banco por recarga (o servidor limita a 2000).
const HISTORY_LIMIT: i64 = 1000;
/// Teto da cauda ao vivo em memória.
const LIVE_MAX: usize = 3000;
const LIVE_DRAIN: usize = 500;
const FONT_SIZE: f32 = 12.5;
const MODULE_WIDTH: usize = 9;

// ---------- cores do painel (iguais nos dois temas: é um terminal) ----------

const PANEL_BG: Color32 = Color32::from_rgb(14, 16, 20);
const PANEL_BORDER: Color32 = Color32::from_rgb(36, 40, 48);
const TEXT: Color32 = Color32::from_rgb(208, 213, 222);
const TEXT_DIM: Color32 = Color32::from_rgb(118, 126, 140);
const MODULE_COLOR: Color32 = Color32::from_rgb(150, 166, 196);
const ROW_HOVER: Color32 = Color32::from_rgb(26, 30, 38);
const MATCH_BG: Color32 = Color32::from_rgb(128, 102, 16);
const MATCH_FG: Color32 = Color32::from_rgb(255, 250, 230);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    const ALL: [Level; 5] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
    ];

    fn parse(s: &str) -> Level {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" | "err" | "fatal" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" => Level::Debug,
            "trace" => Level::Trace,
            _ => Level::Info,
        }
    }

    /// Etiqueta com largura fixa de 5 colunas.
    fn tag(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Level::Error => Color32::from_rgb(255, 96, 96),
            Level::Warn => Color32::from_rgb(245, 186, 60),
            Level::Info => Color32::from_rgb(104, 196, 140),
            Level::Debug => Color32::from_rgb(122, 130, 146),
            Level::Trace => Color32::from_rgb(92, 98, 110),
        }
    }

    /// Fundo da linha (só erro e aviso ganham destaque).
    fn row_fill(self) -> Option<Color32> {
        match self {
            Level::Error => Some(Color32::from_rgb(58, 22, 26)),
            Level::Warn => Some(Color32::from_rgb(44, 36, 16)),
            _ => None,
        }
    }

    fn message_color(self) -> Color32 {
        match self {
            Level::Error => Color32::from_rgb(255, 206, 206),
            Level::Warn => Color32::from_rgb(244, 230, 196),
            Level::Debug | Level::Trace => Color32::from_rgb(160, 168, 182),
            Level::Info => TEXT,
        }
    }
}

struct Entry {
    at: chrono::DateTime<chrono::Utc>,
    level: Level,
    module: String,
    message: String,
    /// `módulo + mensagem` em minúsculas, para o filtro de texto.
    haystack: String,
}

impl Entry {
    fn new(
        at: chrono::DateTime<chrono::Utc>,
        level: &str,
        module: String,
        message: String,
    ) -> Self {
        let haystack = format!("{module} {message}").to_lowercase();
        Self {
            at,
            level: Level::parse(level),
            module,
            message,
            haystack,
        }
    }

    /// Chave estável para lembrar linhas expandidas mesmo quando os índices mudam.
    fn key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.at.timestamp_micros().hash(&mut h);
        self.module.hash(&mut h);
        self.message.hash(&mut h);
        h.finish()
    }

    fn time_text(&self, with_date: bool) -> String {
        let local = self.at.with_timezone(&chrono::Local);
        if with_date {
            local.format("%d/%m %H:%M:%S%.3f").to_string()
        } else {
            local.format("%H:%M:%S%.3f").to_string()
        }
    }

    /// Linha em texto puro (para copiar).
    fn plain(&self) -> String {
        format!(
            "{} {} {:<width$} {}",
            self.at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S%.3f"),
            self.level.tag(),
            self.module,
            self.message,
            width = MODULE_WIDTH
        )
    }
}

pub struct LogsPage {
    history: Loader<Value>,
    /// Histórico já convertido, em ordem cronológica.
    hist_rows: Vec<Entry>,
    /// Cauda ao vivo (eventos `Log`), em ordem cronológica.
    live: Vec<Entry>,
    /// Filtro de módulo (aplicado no servidor e na cauda ao vivo).
    module: String,
    min_level: Level,
    filter: String,
    paused: bool,
    follow: bool,
    wrap: bool,
    show_date: bool,
    /// Linhas expandidas por clique (quando sem quebra de linha).
    expanded: HashSet<u64>,
    /// Incrementado a cada mudança nos dados; invalida `visible`.
    generation: u64,
    /// Índices visíveis em `entries()` e a chave que os produziu.
    visible: Vec<usize>,
    visible_key: Option<(u64, String, Level, String)>,
    jump_to_bottom: bool,
}

impl Default for LogsPage {
    fn default() -> Self {
        Self {
            history: Loader::default(),
            hist_rows: Vec::new(),
            live: Vec::new(),
            module: String::new(),
            min_level: Level::Trace,
            filter: String::new(),
            paused: false,
            follow: true,
            wrap: false,
            show_date: false,
            expanded: HashSet::new(),
            generation: 0,
            visible: Vec::new(),
            visible_key: None,
            jump_to_bottom: false,
        }
    }
}

impl LogsPage {
    pub fn on_event(&mut self, ev: &Event) {
        if let Event::Log {
            at,
            level,
            module,
            message,
        } = ev
        {
            if self.paused {
                return;
            }
            self.live
                .push(Entry::new(*at, level, module.clone(), message.clone()));
            if self.live.len() > LIVE_MAX {
                self.live.drain(..LIVE_DRAIN);
            }
            self.generation += 1;
        }
    }

    fn reload(&mut self, cx: &Cx) {
        // Valores vazios são omitidos da query pelo cliente.
        let module = self.module.clone();
        // `level` no servidor é igualdade exata; só vale a pena quando o mínimo
        // é ERROR (único nível sem nada acima).
        let level = if self.min_level == Level::Error {
            "error".to_string()
        } else {
            String::new()
        };
        self.live.clear();
        self.generation += 1;
        self.history.start(cx.svc.call(move |c| async move {
            c.logs(&[
                ("limit", HISTORY_LIMIT.to_string()),
                ("module", module),
                ("level", level),
            ])
            .await
        }));
    }

    /// Converte o JSON recém-chegado em `hist_rows` e descarta da cauda ao vivo
    /// o que o banco já cobre.
    fn absorb_history(&mut self) {
        let mut rows: Vec<Entry> = Vec::new();
        if let Some(list) = self.history.data.as_ref().and_then(|v| v.as_array()) {
            for r in list.iter().rev() {
                let at = r
                    .get("at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&chrono::Utc))
                    .unwrap_or_else(chrono::Utc::now);
                rows.push(Entry::new(
                    at,
                    &ui::s(r, "level"),
                    ui::s(r, "module"),
                    ui::s(r, "message"),
                ));
            }
        }
        if let Some(last) = rows.last().map(|e| e.at) {
            self.live.retain(|e| e.at > last);
        }
        self.hist_rows = rows;
        self.generation += 1;
    }

    fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.hist_rows.iter().chain(self.live.iter())
    }

    fn entry(&self, idx: usize) -> &Entry {
        if idx < self.hist_rows.len() {
            &self.hist_rows[idx]
        } else {
            &self.live[idx - self.hist_rows.len()]
        }
    }

    fn total(&self) -> usize {
        self.hist_rows.len() + self.live.len()
    }

    /// Recalcula os índices visíveis quando filtro ou dados mudaram.
    fn refresh_visible(&mut self) {
        let needle = self.filter.trim().to_lowercase();
        let key = (
            self.generation,
            needle.clone(),
            self.min_level,
            self.module.clone(),
        );
        if self.visible_key.as_ref() == Some(&key) {
            return;
        }
        let module = self.module.clone();
        let min_level = self.min_level;
        self.visible = self
            .entries()
            .enumerate()
            .filter(|(_, e)| e.level >= min_level)
            .filter(|(_, e)| module.is_empty() || e.module == module)
            .filter(|(_, e)| needle.is_empty() || e.haystack.contains(&needle))
            .map(|(i, _)| i)
            .collect();
        self.visible_key = Some(key);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.history.loaded_at.is_none() && !self.history.busy() {
            self.reload(cx);
        }
        if self.history.poll() {
            self.absorb_history();
        }
        ui::heading(
            ui,
            tr("Logs"),
            Some(tr("Registros do serviço (segredos redigidos). Arquivos completos ficam na pasta de logs.")),
        );
        self.toolbar(ui, cx);
        if let Some(e) = &self.history.error {
            ui::error_text(ui, e);
        }
        // Depois dos controles, para os contadores refletirem o filtro deste quadro.
        self.refresh_visible();
        self.counters(ui);
        ui.add_space(6.0);
        self.panel(ui);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let mut reload = false;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .desired_width(260.0)
                    .hint_text(tr("filtrar texto (mensagem ou módulo)")),
            );
            ui::text_context_menu(&resp);
            if !self.filter.is_empty()
                && ui.button("×").on_hover_text(tr("limpar filtro")).clicked()
            {
                self.filter.clear();
            }

            ui.label(egui::RichText::new(tr("Nível ≥")).weak());
            let prev_level = self.min_level;
            egui::ComboBox::from_id_salt("logs-min-level")
                .selected_text(self.min_level.label())
                .width(96.0)
                .show_ui(ui, |ui| {
                    for l in Level::ALL {
                        ui.selectable_value(&mut self.min_level, l, l.label());
                    }
                });
            // Entrar ou sair de "só erros" muda o que o servidor devolve.
            if (prev_level == Level::Error) != (self.min_level == Level::Error) {
                reload = true;
            }

            ui.label(egui::RichText::new(tr("Módulo")).weak());
            let prev_module = self.module.clone();
            egui::ComboBox::from_id_salt("logs-module")
                .selected_text(if self.module.is_empty() {
                    tr("todos").to_string()
                } else {
                    self.module.clone()
                })
                .width(110.0)
                .show_ui(ui, |ui| {
                    for (k, l) in [
                        ("", tr("todos")),
                        ("engine", "engine"),
                        ("telegram", "telegram"),
                        ("realtime", "realtime"),
                        ("mcp", "mcp"),
                        ("whatsapp", "whatsapp"),
                        ("google", "google"),
                        ("server", "server"),
                        ("storage", "storage"),
                        ("desktop", "desktop"),
                    ] {
                        ui.selectable_value(&mut self.module, k.to_string(), l);
                    }
                });
            if prev_module != self.module {
                reload = true;
            }

            ui.separator();
            if ui.checkbox(&mut self.follow, tr("seguir")).changed() && self.follow {
                self.jump_to_bottom = true;
            }
            ui.checkbox(&mut self.wrap, tr("quebrar linhas"));
            ui.checkbox(&mut self.show_date, tr("data"));
            ui.checkbox(&mut self.paused, tr("pausar ao vivo"));
            ui.separator();
            if ui.button(tr("Atualizar")).clicked() {
                reload = true;
            }
            if ui
                .button(tr("Copiar visíveis"))
                .on_hover_text(tr("Copia as linhas filtradas como texto"))
                .clicked()
            {
                let text: String = self
                    .visible
                    .iter()
                    .map(|&i| self.entry(i).plain())
                    .collect::<Vec<_>>()
                    .join("\n");
                let n = self.visible.len();
                ui.ctx().copy_text(text);
                cx.toasts.success(trf("{} linhas copiadas.", &[&n]));
            }
            if ui
                .button(tr("Limpar tela"))
                .on_hover_text(tr("Esvazia a tela (o banco e os arquivos ficam intactos)"))
                .clicked()
            {
                self.hist_rows.clear();
                self.live.clear();
                self.expanded.clear();
                self.generation += 1;
            }
            if ui.button(tr("Abrir pasta de logs")).clicked() {
                if cx.snap.server_is_local {
                    let dir = cx.snap.logs_dir.clone();
                    if let Err(e) = will_platform::open_path(&dir) {
                        cx.toasts.error(e);
                    }
                } else {
                    cx.toasts.info(tr(
                        "Abrir pasta: disponível só quando o servidor é esta máquina.",
                    ));
                }
            }
        });
        if reload {
            self.reload(cx);
        }
    }

    fn counters(&self, ui: &mut egui::Ui) {
        let total = self.total();
        let shown = self.visible.len();
        let (errors, warns) =
            self.visible
                .iter()
                .fold((0usize, 0usize), |(e, w), &i| match self.entry(i).level {
                    Level::Error => (e + 1, w),
                    Level::Warn => (e, w + 1),
                    _ => (e, w),
                });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
            let counter = if shown == total {
                trf("{} linhas", &[&total])
            } else {
                trf("{} linhas ({} filtradas)", &[&total, &shown])
            };
            ui.label(egui::RichText::new(counter).weak().small());
            if errors > 0 {
                ui::badge(ui, &trf("{} erros", &[&errors]), Tone::Danger);
            }
            if warns > 0 {
                ui::badge(ui, &trf("{} avisos", &[&warns]), Tone::Warning);
            }
            if self.history.loading() {
                ui.label(egui::RichText::new(tr("carregando…")).weak().small());
            }
            if self.paused {
                ui::badge(ui, tr("ao vivo pausado"), Tone::Neutral);
            }
        });
    }

    fn panel(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(FONT_SIZE);
        let line_h = ui
            .painter()
            .layout_no_wrap("X".to_owned(), font.clone(), TEXT)
            .size()
            .y;
        let row_h = (line_h + 3.0).round();
        // A página de Logs não rola (ver app.rs): o painel ocupa exatamente o
        // que sobra da janela abaixo da barra de filtros.
        let height = (ui.available_height() - 10.0).max(120.0);
        let needle = self.filter.trim().to_lowercase();
        let uniform = !self.wrap && self.expanded.is_empty();
        let total_rows = self.visible.len();

        egui::Frame::new()
            .fill(PANEL_BG)
            .stroke(egui::Stroke::new(1.0, PANEL_BORDER))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(0, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(height);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                ui.style_mut().interaction.selectable_labels = true;
                ui.visuals_mut().selection.bg_fill = Color32::from_rgb(52, 84, 140);

                let mut scroll = egui::ScrollArea::vertical()
                    .id_salt("logs-terminal")
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.follow);
                if std::mem::take(&mut self.jump_to_bottom) {
                    scroll = scroll.vertical_scroll_offset(1.0e9);
                }

                if total_rows == 0 {
                    scroll.show(ui, |ui| {
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            let msg = if self.total() == 0 {
                                tr("nenhum registro ainda")
                            } else {
                                tr("nenhuma linha corresponde ao filtro")
                            };
                            ui.label(egui::RichText::new(msg).font(font.clone()).color(TEXT_DIM));
                        });
                    });
                    return;
                }

                let mut toggled: Option<u64> = None;
                if uniform {
                    scroll.show_rows(ui, row_h, total_rows, |ui, range| {
                        for vi in range {
                            let idx = self.visible[vi];
                            let e = self.entry(idx);
                            if let Some(k) =
                                row(ui, e, &font, row_h, &needle, self.show_date, false)
                            {
                                toggled = Some(k);
                            }
                        }
                    });
                } else {
                    scroll.show(ui, |ui| {
                        for &idx in &self.visible {
                            let e = self.entry(idx);
                            let wrap = self.wrap || self.expanded.contains(&e.key());
                            if let Some(k) = row(ui, e, &font, row_h, &needle, self.show_date, wrap)
                            {
                                toggled = Some(k);
                            }
                        }
                    });
                }
                if let Some(k) = toggled {
                    if !self.expanded.remove(&k) {
                        self.expanded.insert(k);
                    }
                }
            });
    }
}

/// Desenha uma linha; devolve a chave da entrada quando ela foi clicada
/// (para expandir/recolher).
fn row(
    ui: &mut egui::Ui,
    e: &Entry,
    font: &FontId,
    row_h: f32,
    needle: &str,
    show_date: bool,
    wrap: bool,
) -> Option<u64> {
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(&format!(" {} ", e.time_text(show_date)), 0.0, fmt(TEXT_DIM));
    job.append(e.level.tag(), 0.0, fmt(e.level.color()));
    job.append(" ", 0.0, fmt(TEXT_DIM));
    job.append(
        &format!(
            "{:<width$} ",
            ui::ellipsize(&e.module, MODULE_WIDTH),
            width = MODULE_WIDTH
        ),
        0.0,
        fmt(MODULE_COLOR),
    );
    append_highlighted(&mut job, &e.message, fmt(e.level.message_color()), needle);
    if !wrap {
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
    }

    // Fundo reservado antes do texto para ficar por baixo dele.
    let bg = ui.painter().add(egui::Shape::Noop);
    let bar = ui.painter().add(egui::Shape::Noop);
    let width = ui.available_width();
    let label = if wrap {
        egui::Label::new(job).wrap().selectable(true)
    } else {
        egui::Label::new(job).truncate().selectable(true)
    };
    let inner = ui.allocate_ui_with_layout(
        egui::vec2(width, row_h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| ui.add(label),
    );
    let resp = inner.inner;
    let rect = egui::Rect::from_min_max(
        egui::pos2(inner.response.rect.left(), inner.response.rect.top()),
        egui::pos2(
            inner.response.rect.left() + width,
            inner.response.rect.bottom(),
        ),
    );
    let hovered = ui.rect_contains_pointer(rect);
    let fill = match (e.level.row_fill(), hovered) {
        (Some(c), true) => Some(c.linear_multiply(1.35)),
        (Some(c), false) => Some(c),
        (None, true) => Some(ROW_HOVER),
        (None, false) => None,
    };
    if let Some(c) = fill {
        ui.painter().set(bg, egui::Shape::rect_filled(rect, 0.0, c));
    }
    if e.level >= Level::Warn {
        let bar_rect = egui::Rect::from_min_max(
            rect.left_top(),
            egui::pos2(rect.left() + 3.0, rect.bottom()),
        );
        ui.painter().set(
            bar,
            egui::Shape::rect_filled(bar_rect, 0.0, e.level.color()),
        );
    }
    if resp.clicked() && !resp.dragged() {
        Some(e.key())
    } else {
        None
    }
}

/// Acrescenta `text` ao job destacando as ocorrências de `needle` (sem
/// diferenciar maiúsculas). Se a versão minúscula mudar de tamanho em bytes
/// (raro, só Unicode exótico), desiste do destaque.
fn append_highlighted(job: &mut egui::text::LayoutJob, text: &str, base: TextFormat, needle: &str) {
    if needle.is_empty() {
        job.append(text, 0.0, base);
        return;
    }
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        job.append(text, 0.0, base);
        return;
    }
    let hl = TextFormat {
        color: MATCH_FG,
        background: MATCH_BG,
        ..base.clone()
    };
    let mut pos = 0usize;
    for (start, m) in lower.match_indices(needle) {
        if start < pos {
            continue;
        }
        if !text.is_char_boundary(start) || !text.is_char_boundary(start + m.len()) {
            continue;
        }
        if start > pos {
            job.append(&text[pos..start], 0.0, base.clone());
        }
        job.append(&text[start..start + m.len()], 0.0, hl.clone());
        pos = start + m.len();
    }
    if pos < text.len() {
        job.append(&text[pos..], 0.0, base);
    }
}
