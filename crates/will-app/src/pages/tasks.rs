//! Tarefas: painel Gantt (lista à esquerda, linha do tempo à direita) com zoom,
//! navegação por arraste, linha do "agora", cor por estado (futura cinza, em
//! andamento azul, atrasada vermelha, concluída verde), botão de concluir ao
//! passar o mouse na barra e formulário de criação/edição. Ver `docs/31`.

use super::Cx;
use crate::help;
use crate::i18n::{tr, trf};
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use chrono::{DateTime, Datelike, NaiveDateTime, Timelike, Utc};
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use serde_json::{json, Value};
use std::time::Duration;

const ROW_H: f32 = 30.0;
const HEADER_H: f32 = 44.0;
/// Largura inicial da lista de títulos (ajustável pelo divisor arrastável).
const LIST_W: f32 = 280.0;
const LIST_W_MIN: f32 = 160.0;
const LIST_W_MAX: f32 = 640.0;
const MIN_DAYS_VISIBLE: f32 = 0.5;
/// Até 2 anos na tela: o eixo troca de escala (dias → semanas → meses).
const MAX_DAYS_VISIBLE: f32 = 730.0;
const OPEN_END_PX: f32 = 26.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Upcoming,
    Running,
    Overdue,
    Done,
    /// Fechada como não realizada (some como a concluída, mas não foi feita).
    Skipped,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            State::Upcoming => tr("futura"),
            State::Running => tr("em andamento"),
            State::Overdue => tr("atrasada"),
            State::Done => tr("concluída"),
            State::Skipped => tr("não realizada"),
        }
    }
}

#[derive(Debug, Clone)]
struct Row {
    id: String,
    title: String,
    description: String,
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    done: bool,
    skipped: bool,
    priority: u8,
    start_local: String,
    end_local: String,
    /// Espelhada num evento do Google Calendar.
    linked: bool,
    /// O evento é especial do Google (aniversário, Gmail…): não aceita escrita.
    read_only: bool,
    /// Etiquetas normalizadas.
    tags: Vec<String>,
}

impl Row {
    fn from_json(v: &Value) -> Option<Row> {
        let start = v
            .get("start_at")
            .and_then(|s| s.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))?;
        let end = v
            .get("end_at")
            .and_then(|s| s.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        Some(Row {
            id: ui::s(v, "id"),
            title: ui::s(v, "title"),
            description: ui::s(v, "description"),
            start,
            end,
            done: v.get("done_at").is_some_and(|d| !d.is_null()),
            skipped: ui::b(v, "skipped"),
            priority: ui::i(v, "priority_rank").clamp(1, 3) as u8,
            start_local: ui::s(v, "start_local"),
            end_local: ui::s(v, "end_local"),
            linked: v.get("event_id").is_some_and(|e| e.is_string()),
            read_only: ui::b(v, "event_read_only"),
            tags: v
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    fn state(&self, now: DateTime<Utc>) -> State {
        if self.done {
            if self.skipped {
                State::Skipped
            } else {
                State::Done
            }
        } else if now < self.start {
            State::Upcoming
        } else if self.end.is_some_and(|e| now >= e) {
            State::Overdue
        } else {
            State::Running
        }
    }
}

/// Formulário de criação (`editing == None`) ou edição. Datas pelo
/// calendário (`DatePickerButton`) e hora/minuto por seletores.
struct Form {
    editing: Option<String>,
    title: String,
    description: String,
    start_date: jiff::civil::Date,
    start_h: u32,
    start_m: u32,
    has_end: bool,
    end_date: jiff::civil::Date,
    end_h: u32,
    end_m: u32,
    priority: String,
    /// Etiquetas já normalizadas (chips) e o que está sendo digitado.
    tags: Vec<String>,
    tag_input: String,
    done: bool,
    skipped: bool,
    /// Espelhada no Google Calendar (só informativo no cabeçalho).
    linked: bool,
    read_only: bool,
    save: Option<Task<Value>>,
    error: Option<String>,
    /// Aba Descrição: editor (true) ou Markdown renderizado (false).
    desc_edit: bool,
}

/// Próximo quarto de hora a partir do relógio local.
fn next_quarter(offset_minutes: i32) -> NaiveDateTime {
    let now = local_naive(Utc::now(), offset_minutes);
    let base = now.date().and_hms_opt(now.hour(), 0, 0).unwrap_or(now);
    let q = (now.minute() / 15 + 1) as i64 * 15;
    base + chrono::Duration::minutes(q)
}

pub(crate) fn jdate(n: chrono::NaiveDate) -> jiff::civil::Date {
    jiff::civil::Date::new(n.year() as i16, n.month() as i8, n.day() as i8).unwrap_or_default()
}

fn ndate(d: jiff::civil::Date) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(i32::from(d.year()), d.month() as u32, d.day() as u32)
        .unwrap_or_default()
}

/// `YYYY-MM-DDTHH:MM` (fuso configurado) → (data, hora, minuto).
fn parse_local(s: &str) -> Option<(jiff::civil::Date, u32, u32)> {
    let n = NaiveDateTime::parse_from_str(s.get(..16)?, "%Y-%m-%dT%H:%M").ok()?;
    Some((jdate(n.date()), n.hour(), n.minute()))
}

fn fmt_local(d: jiff::civil::Date, h: u32, m: u32) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        d.year(),
        d.month(),
        d.day(),
        h,
        m
    )
}

impl Form {
    fn blank() -> Self {
        let d = jiff::civil::Date::default();
        Self {
            editing: None,
            title: String::new(),
            description: String::new(),
            start_date: d,
            start_h: 9,
            start_m: 0,
            has_end: false,
            end_date: d,
            end_h: 10,
            end_m: 0,
            priority: "medium".into(),
            tags: Vec::new(),
            tag_input: String::new(),
            done: false,
            skipped: false,
            linked: false,
            read_only: false,
            save: None,
            error: None,
            desc_edit: false,
        }
    }

    fn new(offset_minutes: i32) -> Self {
        let n = next_quarter(offset_minutes);
        let mut f = Self::blank();
        f.start_date = jdate(n.date());
        f.start_h = n.hour();
        f.start_m = n.minute();
        f.set_end_from_start(1);
        f.has_end = false;
        f
    }

    fn from_row(r: &Row) -> Self {
        let mut f = Self::blank();
        f.editing = Some(r.id.clone());
        f.title = r.title.clone();
        f.description = r.description.clone();
        f.linked = r.linked;
        f.read_only = r.read_only;
        if let Some((d, h, m)) = parse_local(&r.start_local) {
            f.start_date = d;
            f.start_h = h;
            f.start_m = m;
        }
        match parse_local(&r.end_local) {
            Some((d, h, m)) => {
                f.has_end = true;
                f.end_date = d;
                f.end_h = h;
                f.end_m = m;
            }
            None => {
                f.set_end_from_start(1);
                f.has_end = false;
            }
        }
        f.priority = match r.priority {
            1 => "high",
            3 => "low",
            _ => "medium",
        }
        .into();
        f.done = r.done;
        f.skipped = r.skipped;
        f.tags = r.tags.clone();
        f
    }

    /// Transforma o que está na caixa de tags em chips (normalizado, sem
    /// repetir). Aceita várias separadas por vírgula.
    fn commit_tag_input(&mut self) {
        let raw = std::mem::take(&mut self.tag_input);
        for t in will_core::task::normalize_tags(raw.split(',')) {
            if !self.tags.contains(&t) {
                self.tags.push(t);
            }
        }
    }

    fn start_naive(&self) -> NaiveDateTime {
        ndate(self.start_date)
            .and_hms_opt(self.start_h, self.start_m, 0)
            .unwrap_or_default()
    }

    /// Fim = início + `hours` (atalhos "+1h", "+1 dia"…).
    fn set_end_from_start(&mut self, hours: i64) {
        let e = self.start_naive() + chrono::Duration::hours(hours);
        self.has_end = true;
        self.end_date = jdate(e.date());
        self.end_h = e.hour();
        self.end_m = e.minute();
    }

    fn saving(&self) -> bool {
        self.save.as_ref().is_some_and(|t| t.pending())
    }

    fn body(&self) -> Result<Value, String> {
        if self.title.trim().is_empty() {
            return Err(tr("Dê um título à tarefa.").into());
        }
        let start = fmt_local(self.start_date, self.start_h, self.start_m);
        let end = if self.has_end {
            let e = ndate(self.end_date)
                .and_hms_opt(self.end_h, self.end_m, 0)
                .unwrap_or_default();
            if e <= self.start_naive() {
                return Err(tr("O fim precisa ser depois do início.").into());
            }
            fmt_local(self.end_date, self.end_h, self.end_m)
        } else {
            String::new()
        };
        // o que ficou na caixa sem Enter também entra
        let mut tags = self.tags.clone();
        for t in will_core::task::normalize_tags(self.tag_input.split(',')) {
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
        Ok(json!({
            "title": self.title.trim(),
            "description": self.description.trim(),
            "start_at": start,
            "end_at": end,
            "priority": self.priority,
            "tags": tags,
        }))
    }
}

/// Seletores de hora e minuto (minuto de 5 em 5, mais o valor atual).
fn time_picker(ui: &mut egui::Ui, id: &str, h: &mut u32, m: &mut u32) {
    egui::ComboBox::from_id_salt((id, "h"))
        .width(58.0)
        .selected_text(format!("{h:02}"))
        .show_ui(ui, |ui| {
            for x in 0..24u32 {
                ui.selectable_value(h, x, format!("{x:02}"));
            }
        });
    ui.label(":");
    let mut mins: Vec<u32> = (0..60).step_by(5).collect();
    if !mins.contains(m) {
        mins.push(*m);
        mins.sort_unstable();
    }
    egui::ComboBox::from_id_salt((id, "m"))
        .width(58.0)
        .selected_text(format!("{m:02}"))
        .show_ui(ui, |ui| {
            for x in mins {
                ui.selectable_value(m, x, format!("{x:02}"));
            }
        });
}

pub(crate) fn date_picker<'a>(
    d: &'a mut jiff::civil::Date,
    id: &'a str,
) -> egui_extras::DatePickerButton<'a> {
    egui_extras::DatePickerButton::new(d)
        .id_salt(id)
        .format("%d/%m/%Y")
        .highlight_weekends(true)
        .show_icon(true)
}

/// Corta o texto com "…" para caber em `max_w` pixels na fonte dada (mede de
/// verdade, em vez de contar caracteres), para o título acompanhar a borda.
fn fit_text(p: &egui::Painter, text: &str, font: egui::FontId, max_w: f32) -> String {
    let width = |t: &str| {
        p.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if width(&candidate) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

/// Distância humana e curta entre `now` e `at`, sem segundos: "agora",
/// "em 5 min", "há 2 h", "em 3 d", "há 2 sem", "em 4 meses". Uma unidade só,
/// sempre a maior que cabe (arredonda para baixo).
pub(crate) fn relative_short(now: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let secs = (at - now).num_seconds();
    let abs = secs.abs();
    if abs < 60 {
        return tr("agora").to_string();
    }
    let amount = if abs < 3600 {
        trf("{} min", &[&(abs / 60)])
    } else if abs < 86_400 {
        trf("{} h", &[&(abs / 3600)])
    } else if abs < 7 * 86_400 {
        trf("{} d", &[&(abs / 86_400)])
    } else if abs < 60 * 86_400 {
        trf("{} sem", &[&(abs / (7 * 86_400))])
    } else if abs < 60 * 86_400 * 2 {
        tr("1 mês").to_string()
    } else {
        trf("{} meses", &[&(abs / (30 * 86_400))])
    };
    if secs > 0 {
        trf("em {}", &[&amount])
    } else {
        trf("há {}", &[&amount])
    }
}

/// Rótulo relativo da barra e de que lado fica: ainda não começou → "em X"
/// à **esquerda** (quanto falta); em andamento → "há X" à esquerda (desde o
/// início); já acabou (ou concluída) → "há X" à **direita**, contado do fim.
fn relative_label(r: &Row, now: DateTime<Utc>) -> (String, bool) {
    let ended = r.end.filter(|e| now >= *e);
    if r.done {
        (relative_short(now, r.end.unwrap_or(r.start)), false)
    } else if now < r.start {
        (relative_short(now, r.start), true)
    } else if let Some(e) = ended {
        (relative_short(now, e), false)
    } else {
        (relative_short(now, r.start), true)
    }
}

/// Tarefa aberta que ainda não começou e começa em até `soon_secs` (o corte
/// vem de `settings.tasks.soon_minutes`; `0` = destaque desligado).
fn starting_soon(r: &Row, now: DateTime<Utc>, soon_secs: i64) -> bool {
    soon_secs > 0 && !r.done && now < r.start && (r.start - now).num_seconds() <= soon_secs
}

/// Amarelo do rótulo "está chegando" (mais escuro no tema claro, para ler).
fn soon_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(250, 204, 21)
    } else {
        Color32::from_rgb(176, 122, 0)
    }
}

/// Pulso discreto atrás do rótulo "está chegando": pílula amarela translúcida
/// que respira (~1,6 s por ciclo). `time` = relógio do egui em segundos.
fn paint_soon_pulse(p: &egui::Painter, text: Rect, time: f64, dark: bool) {
    let wave = ((time * std::f64::consts::TAU / 1.6).sin() * 0.5 + 0.5) as f32;
    let alpha = 0.10 + 0.20 * wave;
    p.rect_filled(
        text.expand2(Vec2::new(4.0, 2.0)),
        6.0,
        soon_color(dark).gamma_multiply(alpha),
    );
}

/// Instante UTC → relógio local (fuso das configurações, pelo deslocamento).
fn local_naive(dt: DateTime<Utc>, offset_minutes: i32) -> NaiveDateTime {
    (dt + chrono::Duration::minutes(i64::from(offset_minutes))).naive_utc()
}

/// Horas locais desde a época (eixo x do Gantt).
fn local_hours(dt: DateTime<Utc>, offset_minutes: i32) -> f64 {
    dt.timestamp() as f64 / 3600.0 + f64::from(offset_minutes) / 60.0
}

/// Inverso de [`local_hours`].
fn utc_of_local_hours(h: f64, offset_minutes: i32) -> DateTime<Utc> {
    let secs = ((h - f64::from(offset_minutes) / 60.0) * 3600.0).round() as i64;
    DateTime::from_timestamp(secs, 0).unwrap_or_default()
}

fn month_pt(m: u32) -> &'static str {
    match m {
        1 => tr("jan"),
        2 => tr("fev"),
        3 => tr("mar"),
        4 => tr("abr"),
        5 => tr("mai"),
        6 => tr("jun"),
        7 => tr("jul"),
        8 => tr("ago"),
        9 => tr("set"),
        10 => tr("out"),
        11 => tr("nov"),
        _ => tr("dez"),
    }
}

fn weekday_pt(n: NaiveDateTime) -> &'static str {
    match n.weekday() {
        chrono::Weekday::Mon => tr("seg"),
        chrono::Weekday::Tue => tr("ter"),
        chrono::Weekday::Wed => tr("qua"),
        chrono::Weekday::Thu => tr("qui"),
        chrono::Weekday::Fri => tr("sex"),
        chrono::Weekday::Sat => tr("sáb"),
        chrono::Weekday::Sun => tr("dom"),
    }
}

/// Recorte da visão: tudo (zoom/rolagem livres) ou o dia, a semana ou o mês
/// atuais — a lista mostra só o que cai no período e a linha do tempo fica
/// travada com o começo do período na borda esquerda e o fim na direita.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Scope {
    #[default]
    All,
    Day,
    Week,
    Month,
}

impl Scope {
    const ALL: [Scope; 4] = [Scope::All, Scope::Day, Scope::Week, Scope::Month];

    fn as_str(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Day => "day",
            Scope::Week => "week",
            Scope::Month => "month",
        }
    }

    fn parse(s: &str) -> Scope {
        match s {
            "day" => Scope::Day,
            "week" => Scope::Week,
            "month" => Scope::Month,
            _ => Scope::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Scope::All => tr("Tudo"),
            Scope::Day => tr("Dia"),
            Scope::Week => tr("Semana"),
            Scope::Month => tr("Mês"),
        }
    }

    fn hover(self) -> &'static str {
        match self {
            Scope::All => tr("Todas as tarefas; zoom e rolagem livres"),
            Scope::Day => tr("Só o dia de hoje: 00:00 na borda esquerda, 24:00 na direita"),
            Scope::Week => tr("Só a semana atual (segunda a domingo), de ponta a ponta"),
            Scope::Month => tr("Só o mês atual, do dia 1 ao último dia"),
        }
    }

    /// `[início, fim)` do período em horas locais desde a época, para o "agora".
    fn range(self, now_h: f64) -> Option<(f64, f64)> {
        let day_start = (now_h / 24.0).floor() * 24.0;
        let local = DateTime::from_timestamp((day_start * 3600.0).round() as i64, 0)?.naive_utc();
        match self {
            Scope::All => None,
            Scope::Day => Some((day_start, day_start + 24.0)),
            Scope::Week => {
                let start = day_start - 24.0 * f64::from(local.weekday().num_days_from_monday());
                Some((start, start + 24.0 * 7.0))
            }
            Scope::Month => {
                let start = day_start - 24.0 * f64::from(local.day() - 1);
                let first = chrono::NaiveDate::from_ymd_opt(local.year(), local.month(), 1)?;
                let next = first.checked_add_months(chrono::Months::new(1))?;
                let days = next.signed_duration_since(first).num_days();
                Some((start, start + 24.0 * days as f64))
            }
        }
    }
}

pub struct TasksPage {
    list: Loader<Value>,
    rows: Vec<Row>,
    offset_minutes: i32,
    show_done: bool,
    /// Dia / semana / mês atuais ou tudo (persistido nas preferências locais).
    scope: Scope,
    /// Todas as tags em uso (vem com a lista) e as marcadas no filtro
    /// (vazio = todas; com marcas, mostra quem tem QUALQUER uma delas).
    all_tags: Vec<String>,
    tag_filter: Vec<String>,
    /// Busca digitada dentro do dropdown de tags (limpa ao fechar).
    tag_search: String,
    /// `start` | `alpha` | `priority` (persistido em `settings.tasks.sort`).
    sort: String,
    sort_loaded: bool,
    sort_save: Option<Task<Value>>,
    form: Option<Form>,
    actions: Vec<(String, Task<Value>)>,
    /// Confirmação pendente na tela: (id, título, ação = delete|complete|reopen).
    confirm: Option<(String, String, &'static str)>,
    /// Zoom: pixels por hora. `None` = ajustar a 7 dias na primeira vez.
    px_per_hour: Option<f32>,
    /// Hora local (desde a época) na borda esquerda da linha do tempo.
    view_start: Option<f64>,
    hovered: Option<String>,
    /// Ponteiro estava sobre as barras no quadro anterior: a roda do mouse vira
    /// zoom e a rolagem vertical da lista fica desligada.
    wheel_over_tl: bool,
    /// Largura atual da lista de títulos (divisor arrastável).
    list_w: f32,
    /// Preferências locais (largura, zoom, posição, concluídas) carregadas?
    prefs_loaded: bool,
    last_saved: Option<crate::ui_prefs::GanttPrefs>,
    dirty_since: Option<std::time::Instant>,
    md_cache: egui_commonmark::CommonMarkCache,
}

impl Default for TasksPage {
    fn default() -> Self {
        Self {
            list: Loader::default(),
            rows: Vec::new(),
            offset_minutes: -180,
            show_done: false,
            scope: Scope::All,
            all_tags: Vec::new(),
            tag_filter: Vec::new(),
            tag_search: String::new(),
            sort: "start".into(),
            sort_loaded: false,
            sort_save: None,
            form: None,
            actions: Vec::new(),
            confirm: None,
            px_per_hour: None,
            view_start: None,
            hovered: None,
            wheel_over_tl: false,
            list_w: LIST_W,
            prefs_loaded: false,
            last_saved: None,
            dirty_since: None,
            md_cache: egui_commonmark::CommonMarkCache::default(),
        }
    }
}

impl TasksPage {
    pub fn on_event(&mut self, ev: &will_core::Event) {
        if matches!(ev, will_core::Event::TasksChanged) {
            self.list.loaded_at = None;
        }
    }

    /// Carrega/atualiza a lista mesmo com a página fechada (badges do menu).
    /// Chamado pelo app a cada quadro; pede a lista quando está velha (60 s)
    /// ou quando `TasksChanged` zerou o carimbo.
    pub fn poll_list(&mut self, svc: &std::sync::Arc<crate::service::Service>) {
        self.load_prefs();
        if self.list.stale(Duration::from_secs(60)) {
            let filter = if self.show_done { "all" } else { "pending" };
            self.list
                .start(svc.call(move |c| async move { c.tasks(filter).await }));
        }
        if self.list.poll() {
            if let Some(v) = &self.list.data {
                self.offset_minutes = v
                    .get("offset_minutes")
                    .and_then(|o| o.as_i64())
                    .unwrap_or(-180) as i32;
                self.rows = v
                    .get("tasks")
                    .and_then(|t| t.as_array())
                    .map(|a| a.iter().filter_map(Row::from_json).collect())
                    .unwrap_or_default();
                self.all_tags = v
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                // tag marcada que sumiu do uso sai do filtro
                self.tag_filter.retain(|t| self.all_tags.contains(t));
            }
        }
    }

    /// (atrasadas, em andamento) agora, pelo relógio.
    pub fn counts(&self) -> (usize, usize) {
        let now = Utc::now();
        let mut overdue = 0;
        let mut running = 0;
        for r in &self.rows {
            match r.state(now) {
                State::Overdue => overdue += 1,
                State::Running => running += 1,
                _ => {}
            }
        }
        (overdue, running)
    }

    /// Tarefas abertas que começam em até `soon_minutes` (badge amarelo do
    /// menu; mesmo corte do rótulo pulsante do Gantt). `0` = desligado.
    pub fn soon_count(&self, soon_minutes: u32) -> usize {
        let now = Utc::now();
        let soon_secs = i64::from(soon_minutes) * 60;
        self.rows
            .iter()
            .filter(|r| starting_soon(r, now, soon_secs))
            .count()
    }

    fn load_prefs(&mut self) {
        if !self.prefs_loaded {
            self.prefs_loaded = true;
            let g = crate::ui_prefs::load().gantt;
            if let Some(w) = g.list_w {
                self.list_w = w.clamp(LIST_W_MIN, LIST_W_MAX);
            }
            if let Some(z) = g.px_per_hour {
                if z.is_finite() && z > 0.0 {
                    self.px_per_hour = Some(z);
                }
            }
            if let Some(v) = g.view_start {
                if v.is_finite() {
                    self.view_start = Some(v);
                }
            }
            self.show_done = g.show_done;
            self.scope = Scope::parse(&g.scope);
            self.last_saved = Some(g);
        }
    }

    fn poll(&mut self, cx: &mut Cx) {
        self.load_prefs();
        if !self.sort_loaded {
            self.sort = cx.snap.settings.tasks.sort.clone();
            if !matches!(self.sort.as_str(), "start" | "alpha" | "priority") {
                self.sort = "start".into();
            }
            self.sort_loaded = true;
        }
        self.poll_list(cx.svc);
        let mut i = 0;
        while i < self.actions.len() {
            if let Some(r) = self.actions[i].1.take() {
                let (msg, _) = self.actions.remove(i);
                match r {
                    Ok(_) => {
                        cx.toasts.success(msg);
                        self.list.loaded_at = None;
                    }
                    Err(e) => cx.toasts.error(trf("Tarefas: {}", &[&e])),
                }
            } else {
                i += 1;
            }
        }
        if let Some(t) = &self.sort_save {
            if let Some(r) = t.take() {
                self.sort_save = None;
                if let Err(e) = r {
                    cx.toasts.error(trf("Ordenação não salva: {}", &[&e]));
                }
            }
        }
        let mut close = false;
        if let Some(f) = &mut self.form {
            if let Some(t) = &f.save {
                if let Some(r) = t.take() {
                    f.save = None;
                    match r {
                        Ok(_) => {
                            cx.toasts.success(if f.editing.is_some() {
                                tr("Tarefa salva.")
                            } else {
                                tr("Tarefa criada.")
                            });
                            close = true;
                            self.list.loaded_at = None;
                        }
                        Err(e) => f.error = Some(e),
                    }
                }
            }
        }
        if close {
            self.form = None;
        }
    }

    fn act(&mut self, cx: &Cx, msg: &str, id: String, kind: &'static str) {
        let t = cx.svc.call(move |c| async move {
            match kind {
                "complete" => c.complete_task(&id).await,
                "skip" => c.skip_task(&id).await,
                "reopen" => c.reopen_task(&id).await,
                _ => c.delete_task(&id).await,
            }
        });
        self.actions.push((msg.to_string(), t));
    }

    fn save_sort(&mut self, cx: &Cx) {
        let mut tasks = cx.snap.settings.tasks.clone();
        tasks.sort = self.sort.clone();
        let value = serde_json::to_value(tasks).unwrap_or(Value::Null);
        self.sort_save = Some(
            cx.svc
                .call(move |c| async move { c.put_settings("tasks", value).await }),
        );
    }

    /// Período do recorte atual em UTC (`[início, fim)`), se houver.
    fn scope_range_utc(&self, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let (s, e) = self.scope.range(local_hours(now, self.offset_minutes))?;
        Some((
            utc_of_local_hours(s, self.offset_minutes),
            utc_of_local_hours(e, self.offset_minutes),
        ))
    }

    fn sorted_rows(&self, now: DateTime<Utc>) -> Vec<Row> {
        let range = self.scope_range_utc(now);
        let mut rows: Vec<Row> = self
            .rows
            .iter()
            .filter(|r| self.show_done || !r.done)
            .filter(|r| match range {
                // no recorte, só o que toca o período (tarefa sem fim = instante)
                Some((s, e)) => r.start < e && r.end.unwrap_or(r.start) >= s,
                None => true,
            })
            // filtro de tags: nenhuma marcada = todas; senão, qualquer uma delas
            .filter(|r| {
                self.tag_filter.is_empty() || r.tags.iter().any(|t| self.tag_filter.contains(t))
            })
            .cloned()
            .collect();
        match self.sort.as_str() {
            "alpha" => rows.sort_by_key(|r| (r.title.to_lowercase(), r.start)),
            "priority" => rows.sort_by_key(|r| (r.priority, r.start)),
            _ => rows.sort_by_key(|r| r.start),
        }
        let _ = now;
        rows
    }

    /// Combo de múltipla escolha com todas as tags em uso. Nenhuma marcada =
    /// todas as tarefas; marcadas = tarefas com qualquer uma delas.
    fn tag_filter_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(tr("Tags:")).weak().small());
        let label = if self.tag_filter.is_empty() {
            tr("todas").to_string()
        } else {
            self.tag_filter.join(", ")
        };
        let label = ui::ellipsize(&label, 28);
        let opened = egui::ComboBox::from_id_salt("tasks-tag-filter")
            .selected_text(label)
            .width(200.0)
            .show_ui(ui, |ui| {
                if self.all_tags.is_empty() {
                    ui.label(
                        egui::RichText::new(tr("Nenhuma tag ainda. Adicione tags nas tarefas."))
                            .small()
                            .weak(),
                    );
                    return;
                }
                // busca: filtra a lista enquanto digita (foco ao abrir)
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.tag_search)
                        .hint_text(tr("buscar tag…"))
                        .desired_width(f32::INFINITY),
                );
                if self.tag_search.is_empty() && !search.has_focus() {
                    search.request_focus();
                }
                let needle = self.tag_search.trim().to_lowercase();
                let mut clear = false;
                if !self.tag_filter.is_empty() {
                    let n = self.tag_filter.len();
                    if ui
                        .selectable_label(false, trf("limpar filtro ({})", &[&n]))
                        .clicked()
                    {
                        clear = true;
                    }
                }
                let visible: Vec<String> = self
                    .all_tags
                    .iter()
                    .filter(|t| needle.is_empty() || t.contains(&needle))
                    .cloned()
                    .collect();
                if visible.is_empty() {
                    ui.label(
                        egui::RichText::new(tr("nenhuma tag com esse texto"))
                            .small()
                            .weak(),
                    );
                }
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for t in visible {
                            let mut on = self.tag_filter.contains(&t);
                            if ui.checkbox(&mut on, &t).changed() {
                                if on {
                                    self.tag_filter.push(t);
                                } else {
                                    self.tag_filter.retain(|x| *x != t);
                                }
                            }
                        }
                    });
                if clear {
                    self.tag_filter.clear();
                }
            });
        opened.response.on_hover_text(tr(
            "Filtrar por tag: nenhuma marcada mostra todas; com marcas, só tarefas que têm qualquer uma delas.",
        ));
        if opened.inner.is_none() {
            // fechado: a próxima abertura começa sem busca
            self.tag_search.clear();
        }
    }

    fn color_for(state: State, pal: &ui::Palette, dark: bool) -> Color32 {
        match state {
            State::Upcoming => pal.text_weak.gamma_multiply(0.55),
            State::Running => pal.accent,
            State::Overdue => Tone::Danger.color(dark),
            State::Done => Tone::Success.color(dark),
            State::Skipped => pal.text_weak.gamma_multiply(0.35),
        }
    }

    fn priority_color(p: u8, dark: bool) -> Color32 {
        match p {
            1 => Tone::Danger.color(dark),
            3 => Tone::Success.color(dark),
            _ => Tone::Warning.color(dark),
        }
    }

    /// Bandeirinha de prioridade (mastro + triângulo).
    fn paint_flag(p: &egui::Painter, at: Pos2, color: Color32) {
        let pole = Stroke::new(1.4, color);
        p.line_segment([at, Pos2::new(at.x, at.y + 11.0)], pole);
        p.add(egui::Shape::convex_polygon(
            vec![
                at,
                Pos2::new(at.x + 8.0, at.y + 2.5),
                Pos2::new(at.x, at.y + 5.5),
            ],
            color,
            Stroke::NONE,
        ));
    }

    /// Círculo com ✓ (concluir) ou ↺ (reabrir), desenhado na barra ao passar o mouse.
    fn paint_check(p: &egui::Painter, rect: Rect, bg: Color32, fg: Color32, reopen: bool) {
        let c = rect.center();
        let r = rect.width().min(rect.height()) / 2.0;
        p.circle_filled(c, r, bg);
        let st = Stroke::new(1.8, fg);
        if reopen {
            p.circle_stroke(c, r * 0.5, st);
        } else {
            p.line_segment(
                [
                    Pos2::new(c.x - r * 0.45, c.y),
                    Pos2::new(c.x - r * 0.1, c.y + r * 0.4),
                ],
                st,
            );
            p.line_segment(
                [
                    Pos2::new(c.x - r * 0.1, c.y + r * 0.4),
                    Pos2::new(c.x + r * 0.5, c.y - r * 0.4),
                ],
                st,
            );
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);
        ui.ctx().request_repaint_after(Duration::from_secs(30));
        let dark = ui.visuals().dark_mode;
        let pal = ui::palette(dark);
        let now = Utc::now();

        // ---- barra superior ----
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr("Tarefas")).font(ui::semibold(18.0)));
            ui::help_icon(ui, &help::TASKS);
            ui.add_space(8.0);
            if ui::primary_button(ui, tr("+ Nova tarefa")).clicked() {
                let mut f = Form::new(self.offset_minutes);
                f.desc_edit = true;
                ui::select_tab(ui.ctx(), "task-form", "basic");
                self.form = Some(f);
            }
            ui.add_space(10.0);
            ui.label(egui::RichText::new(tr("Ordenar:")).weak().small());
            let before = self.sort.clone();
            ui::select(
                ui,
                "tasks-sort",
                &mut self.sort,
                &[
                    ("start", tr("Data de início")),
                    ("alpha", tr("Ordem alfabética")),
                    ("priority", tr("Prioridade")),
                ],
            );
            if before != self.sort {
                self.save_sort(cx);
            }
            ui.add_space(10.0);
            // recorte: tudo · dia · semana · mês
            ui.label(egui::RichText::new(tr("Ver:")).weak().small());
            for sc in Scope::ALL {
                let selected = self.scope == sc;
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new(sc.label()).small())
                            .selected(selected),
                    )
                    .on_hover_text(sc.hover())
                    .clicked()
                {
                    self.scope = sc;
                }
            }
            ui.add_space(10.0);
            if ui::toggle(ui, &mut self.show_done).changed() {
                self.list.loaded_at = None;
            }
            ui.label(egui::RichText::new(tr("mostrar concluídas")).small());
            ui.add_space(10.0);
            // filtro de tags (múltipla escolha; vazio = todas)
            self.tag_filter_ui(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::icon_button(ui, crate::icons::REFRESH, tr("Atualizar")).clicked() {
                    self.list.loaded_at = None;
                }
                if self.list.loading() {
                    ui.spinner();
                }
                if ui
                    .button(tr("Hoje"))
                    .on_hover_text(tr("Volta ao agora (atalho: barra de espaço)"))
                    .clicked()
                {
                    self.view_start = None;
                }
                if ui
                    .button("+")
                    .on_hover_text(tr("Aproximar (roda do mouse sobre as barras)"))
                    .clicked()
                {
                    if let Some(z) = self.px_per_hour.as_mut() {
                        *z *= 1.5;
                    }
                }
                if ui.button("−").on_hover_text(tr("Afastar")).clicked() {
                    if let Some(z) = self.px_per_hour.as_mut() {
                        *z /= 1.5;
                    }
                }
                ui.label(egui::RichText::new(tr("Zoom:")).weak().small());
            });
        });
        // legenda
        ui.horizontal(|ui| {
            for (st, label) in [
                (State::Upcoming, tr("futura")),
                (State::Running, tr("em andamento")),
                (State::Overdue, tr("atrasada")),
                (State::Done, tr("concluída")),
                (State::Skipped, tr("não realizada")),
            ] {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter()
                    .rect_filled(r.shrink(1.0), 2.0, Self::color_for(st, pal, dark));
                ui.label(egui::RichText::new(label).small().weak());
                ui.add_space(6.0);
            }
            ui.label(
                egui::RichText::new(tr(
                    "· roda do mouse sobre as barras = zoom · arraste para navegar · clique na barra para editar",
                ))
                .small()
                .weak(),
            );
        });
        if let Some(e) = &self.list.error {
            ui::error_text(ui, e);
        }
        ui.add_space(4.0);

        // barra de espaço = "Hoje" (fora de campos de texto e com o formulário fechado)
        if self.form.is_none()
            && self.confirm.is_none()
            && ui.ctx().memory(|m| m.focused().is_none())
            && ui.input(|i| i.key_pressed(egui::Key::Space))
        {
            self.view_start = None;
        }
        let rows = self.sorted_rows(now);
        let avail = ui.available_rect_before_wrap();
        let list_w = self
            .list_w
            .clamp(LIST_W_MIN, (avail.width() - 240.0).max(LIST_W_MIN));
        self.list_w = list_w;
        let tl_w = (avail.width() - list_w).max(200.0);
        let zoom = *self.px_per_hour.get_or_insert(tl_w / (7.0 * 24.0));
        let min_zoom = tl_w / (MAX_DAYS_VISIBLE * 24.0);
        let max_zoom = tl_w / (MIN_DAYS_VISIBLE * 24.0);
        let zoom = zoom.clamp(min_zoom, max_zoom);
        self.px_per_hour = Some(zoom);
        let now_h = local_hours(now, self.offset_minutes);
        let view_start = *self.view_start.get_or_insert_with(|| {
            // "agora" a 30% da largura, alinhado à meia-noite local do dia
            let hours_left = (tl_w * 0.3) / zoom;
            let day_start = (now_h / 24.0).floor() * 24.0;
            let candidate = now_h - f64::from(hours_left);
            candidate.min(day_start)
        });
        // Recorte (dia/semana/mês): o período ocupa a largura toda; zoom e
        // rolagem salvos ficam intactos para quando voltar a "Tudo".
        let scoped = self.scope.range(now_h);
        let (zoom, view_start) = match scoped {
            Some((s, e)) => ((tl_w / ((e - s) as f32)).max(f32::EPSILON), s),
            None => (zoom, view_start),
        };

        // ---- cabeçalho (fixo) ----
        let (header, _) =
            ui.allocate_exact_size(Vec2::new(avail.width(), HEADER_H), Sense::hover());
        let tl_header =
            Rect::from_min_max(Pos2::new(header.left() + list_w, header.top()), header.max);
        let x_of = |h: f64| -> f32 { tl_header.left() + ((h - view_start) as f32) * zoom };
        {
            let p = ui.painter_at(header);
            p.rect_filled(header, 0.0, pal.panel);
            p.text(
                Pos2::new(header.left() + 8.0, header.center().y),
                Align2::LEFT_CENTER,
                trf("{} tarefa(s)", &[&rows.len()]),
                ui::medium(12.0),
                pal.text_weak,
            );
            self.paint_time_axis(&p, tl_header, view_start, zoom, now_h, pal, dark, true);
        }

        // ---- linhas (rolagem vertical) ----
        let body_h = ui.available_height().max(100.0);
        let content_h = (rows.len() as f32 * ROW_H).max(body_h);
        let mut open_form: Option<Form> = None;
        let mut ask: Option<(String, String, &'static str)> = None;
        let mut hovered_now: Option<String> = None;
        let mut pan_delta = 0.0f32;
        let mut wheel_now = false;
        let mut zoom_at: Option<(f32, f32)> = None;
        // algum rótulo "está chegando" na tela: mantém a animação do pulso
        let mut any_soon = false;
        let soon_secs = i64::from(cx.snap.settings.tasks.soon_minutes) * 60;
        let time = ui.input(|i| i.time);
        egui::ScrollArea::vertical()
            .id_salt("tasks-gantt")
            .scroll_source(egui::scroll_area::ScrollSource {
                mouse_wheel: !self.wheel_over_tl,
                ..Default::default()
            })
            .auto_shrink([false, false])
            .max_height(body_h)
            .show(ui, |ui| {
                let (area, _) = ui.allocate_exact_size(
                    Vec2::new(avail.width(), content_h),
                    Sense::hover(),
                );
                let tl = Rect::from_min_max(Pos2::new(area.left() + list_w, area.top()), area.max);
                let p = ui.painter_at(area);
                p.rect_filled(area, 0.0, pal.window);
                self.paint_time_axis(&p, tl, view_start, zoom, now_h, pal, dark, false);

                // fundo da linha do tempo: arrastar navega; Ctrl+rolagem dá zoom
                let bg = ui.interact(tl, ui.scope_id().with("tl-bg"), Sense::drag());
                if bg.dragged() {
                    pan_delta = bg.drag_delta().x;
                }
                // A roda só vale para o Gantt se o ponteiro está nele E a camada
                // no topo sob o ponteiro é esta (não um dropdown, popup ou
                // modal aberto por cima): senão a rolagem da lista do combo
                // virava zoom aqui atrás.
                let hover_pos = ui.input(|i| i.pointer.hover_pos());
                let on_top = hover_pos.is_some_and(|hp| {
                    ui.ctx().layer_id_at(hp) == Some(ui.layer_id())
                }) && !egui::Popup::is_any_open(ui.ctx())
                    && self.form.is_none()
                    && self.confirm.is_none();
                let over_tl = on_top && hover_pos.is_some_and(|hp| tl.contains(hp));
                let over_header = on_top && hover_pos.is_some_and(|hp| tl_header.contains(hp));
                wheel_now = over_tl;
                if over_tl || over_header {
                    let (zd, sd) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
                    let mx = hover_pos.map(|h| h.x).unwrap_or(tl.left());
                    if (zd - 1.0).abs() > 0.001 {
                        // pinça no trackpad
                        zoom_at = Some((mx, zd));
                    } else if sd.y.abs() > 0.0 {
                        // roda do mouse sobre as barras: para cima aproxima,
                        // para baixo afasta (convenção de mapas/linhas do tempo),
                        // ancorado no ponteiro
                        let f = (1.0 + sd.y * 0.004).clamp(0.7, 1.4);
                        zoom_at = Some((mx, f));
                    }
                    if sd.x.abs() > 0.0 {
                        pan_delta += sd.x;
                    }
                }

                let x_of = |h: f64| -> f32 { tl.left() + ((h - view_start) as f32) * zoom };
                let font = ui::medium(12.5);
                let small = egui::FontId::proportional(11.0);
                for (i, r) in rows.iter().enumerate() {
                    let y = area.top() + i as f32 * ROW_H;
                    let row_rect = Rect::from_min_size(Pos2::new(area.left(), y), Vec2::new(area.width(), ROW_H));
                    let list_rect = Rect::from_min_size(Pos2::new(area.left(), y), Vec2::new(list_w, ROW_H));
                    let state = r.state(now);
                    let color = Self::color_for(state, pal, dark);
                    let is_hover = self.hovered.as_deref() == Some(r.id.as_str());
                    if is_hover {
                        p.rect_filled(row_rect, 0.0, pal.card_alt.gamma_multiply(0.7));
                    }
                    p.line_segment(
                        [Pos2::new(area.left(), y + ROW_H), Pos2::new(area.right(), y + ROW_H)],
                        Stroke::new(1.0, pal.border.gamma_multiply(0.6)),
                    );
                    // lista: bandeira, título, estado
                    Self::paint_flag(
                        &p,
                        Pos2::new(area.left() + 10.0, y + 9.0),
                        Self::priority_color(r.priority, dark),
                    );
                    p.circle_filled(Pos2::new(area.left() + 30.0, y + ROW_H / 2.0), 3.5, color);
                    let title_color = if r.done { pal.text_weak } else { pal.text };
                    // título cortado só quando encosta na borda da lista (acompanha o divisor)
                    let title_max_w = (list_w - 40.0 - 10.0).max(20.0);
                    let title = fit_text(&p, &r.title, font.clone(), title_max_w);
                    let title_color = if r.done {
                        pal.text_weak.gamma_multiply(0.75)
                    } else {
                        title_color
                    };
                    let tr_rect = p.with_clip_rect(list_rect).text(
                        Pos2::new(area.left() + 40.0, y + ROW_H / 2.0),
                        Align2::LEFT_CENTER,
                        title,
                        font.clone(),
                        title_color,
                    );
                    if r.done {
                        // concluída: apagada e riscada
                        p.with_clip_rect(list_rect).line_segment(
                            [
                                Pos2::new(tr_rect.left(), tr_rect.center().y),
                                Pos2::new(tr_rect.right(), tr_rect.center().y),
                            ],
                            Stroke::new(1.2, title_color),
                        );
                    }
                    // tags depois do título, no que sobrar de largura
                    if !r.tags.is_empty() {
                        let tags_x = tr_rect.right() + 8.0;
                        let room = list_rect.right() - 6.0 - tags_x;
                        if room > 24.0 {
                            let text = r.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ");
                            let text = fit_text(&p, &text, small.clone(), room);
                            p.with_clip_rect(list_rect).text(
                                Pos2::new(tags_x, y + ROW_H / 2.0),
                                Align2::LEFT_CENTER,
                                text,
                                small.clone(),
                                pal.text_weak,
                            );
                        }
                    }
                    let list_resp = ui.interact(list_rect, ui.scope_id().with(("task-row", &r.id)), Sense::click());
                    if list_resp.hovered() {
                        hovered_now = Some(r.id.clone());
                    }
                    if list_resp.clicked() {
                        open_form = Some(Form::from_row(r));
                    }

                    // barra
                    let sh = local_hours(r.start, self.offset_minutes);
                    let x0 = x_of(sh);
                    let (x1, open_end) = match r.end {
                        Some(e) => (x_of(local_hours(e, self.offset_minutes)), false),
                        None => {
                            let until = if r.done || now < r.start { r.start } else { now };
                            (x_of(local_hours(until, self.offset_minutes)) + OPEN_END_PX, true)
                        }
                    };
                    let x1 = x1.max(x0 + 6.0);
                    let bar = Rect::from_min_max(
                        Pos2::new(x0.max(tl.left() - 2.0), y + 6.0),
                        Pos2::new(x1.min(tl.right() + 2.0), y + ROW_H - 6.0),
                    );
                    if x1 < tl.left() || x0 > tl.right() {
                        // fora da janela: só uma seta na borda
                        let (ax, dir) = if x1 < tl.left() { (tl.left() + 6.0, -1.0) } else { (tl.right() - 6.0, 1.0) };
                        let cy = y + ROW_H / 2.0;
                        p.add(egui::Shape::convex_polygon(
                            vec![
                                Pos2::new(ax + 5.0 * dir, cy),
                                Pos2::new(ax - 3.0 * dir, cy - 5.0),
                                Pos2::new(ax - 3.0 * dir, cy + 5.0),
                            ],
                            color,
                            Stroke::NONE,
                        ));
                        // quanto falta / há quanto tempo, ao lado da seta
                        let (rel, _) = relative_label(r, now);
                        let (anchor, align) = if dir < 0.0 {
                            (Pos2::new(ax + 9.0, cy), Align2::LEFT_CENTER)
                        } else {
                            (Pos2::new(ax - 9.0, cy), Align2::RIGHT_CENTER)
                        };
                        let lp = p.with_clip_rect(tl);
                        if starting_soon(r, now, soon_secs) {
                            any_soon = true;
                            let g = lp.layout_no_wrap(rel, small.clone(), soon_color(dark));
                            let rect = align.anchor_size(anchor, g.size());
                            paint_soon_pulse(&lp, rect, time, dark);
                            lp.galley(rect.min, g, soon_color(dark));
                        } else {
                            lp.text(anchor, align, rel, small.clone(), pal.text_weak);
                        }
                        continue;
                    }
                    let fill = if is_hover { color.gamma_multiply(1.15) } else { color };
                    p.rect_filled(bar, 4.0, fill);
                    if open_end {
                        // extremidade aberta: degradê simples (barra sem fim)
                        let fade = Rect::from_min_max(Pos2::new(bar.right() - OPEN_END_PX, bar.top()), bar.max);
                        p.rect_filled(fade, 4.0, pal.window.gamma_multiply(0.55));
                    }
                    if bar.width() > 60.0 {
                        let text = fit_text(&p, &r.title, small.clone(), bar.width() - 34.0);
                        p.text(
                            Pos2::new(bar.left() + 8.0, bar.center().y),
                            Align2::LEFT_CENTER,
                            text,
                            small.clone(),
                            if state == State::Upcoming { pal.text } else { Color32::WHITE },
                        );
                    }
                    let bar_resp = ui.interact(bar, ui.scope_id().with(("task-bar", &r.id)), Sense::click());
                    if bar_resp.hovered() {
                        hovered_now = Some(r.id.clone());
                    }
                    let mut tip = format!(
                        "{}\n{} {}",
                        r.title,
                        tr("início"),
                        local_naive(r.start, self.offset_minutes).format("%d/%m %H:%M")
                    );
                    if let Some(e) = r.end {
                        tip.push_str(&format!(
                            " · {} {}",
                            tr("fim"),
                            local_naive(e, self.offset_minutes).format("%d/%m %H:%M")
                        ));
                    }
                    let (rel, rel_left) = relative_label(r, now);
                    tip.push_str(&format!("\n{} · {} · {} {}", state.label(), rel, tr("prioridade"), match r.priority { 1 => tr("alta"), 3 => tr("baixa"), _ => tr("média") }));
                    if !r.tags.is_empty() {
                        tip.push_str(&format!("\n{}", r.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")));
                    }
                    if !r.description.trim().is_empty() {
                        tip.push_str("\n\n");
                        tip.push_str(&ui::ellipsize(r.description.trim(), 240));
                    }
                    let bar_resp = bar_resp.on_hover_text(tip);
                    // mouse na barra (não na linha): início à esquerda e fim à direita
                    if bar_resp.hovered() {
                        let sl = local_naive(r.start, self.offset_minutes);
                        let start_txt = sl.format("%d/%m %H:%M").to_string();
                        let end_txt = match r.end {
                            Some(e) => {
                                let el = local_naive(e, self.offset_minutes);
                                if el.date() == sl.date() {
                                    el.format("%H:%M").to_string()
                                } else {
                                    el.format("%d/%m %H:%M").to_string()
                                }
                            }
                            None => tr("sem fim").to_string(),
                        };
                        let lp = p.with_clip_rect(tl);
                        let lab = |text: &str, anchor_x: f32, left_of: bool| {
                            let g = lp.layout_no_wrap(text.to_string(), small.clone(), pal.text);
                            let w = g.size().x + 10.0;
                            let h = g.size().y + 6.0;
                            let mut x0 = if left_of { anchor_x - 4.0 - w } else { anchor_x + 4.0 };
                            // se não couber fora da barra, encosta por dentro
                            if left_of && x0 < tl.left() {
                                x0 = anchor_x + 4.0;
                            }
                            if !left_of && x0 + w > tl.right() {
                                x0 = anchor_x - 4.0 - w;
                            }
                            let rr = Rect::from_min_size(
                                Pos2::new(x0, bar.center().y - h / 2.0),
                                Vec2::new(w, h),
                            );
                            lp.rect(rr, 4.0, pal.card, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
                            lp.galley(Pos2::new(rr.left() + 5.0, rr.top() + 3.0), g, pal.text);
                        };
                        lab(&start_txt, bar.left(), true);
                        lab(&end_txt, bar.right(), false);
                    } else {
                        // quanto falta / há quanto tempo, fora da barra: do lado
                        // preferido; se não couber na janela, do outro; senão nada
                        let lp = p.with_clip_rect(tl);
                        let g = lp.layout_no_wrap(rel.clone(), small.clone(), pal.text_weak);
                        let w = g.size().x;
                        let left_x = bar.left() - 6.0 - w;
                        let right_x = bar.right() + 6.0;
                        let fits_left = left_x >= tl.left();
                        let fits_right = right_x + w <= tl.right();
                        let (first, second) = if rel_left {
                            ((fits_left, left_x), (fits_right, right_x))
                        } else {
                            ((fits_right, right_x), (fits_left, left_x))
                        };
                        let x = [first, second].into_iter().find(|(fits, _)| *fits).map(|(_, x)| x);
                        if let Some(x) = x {
                            let pos = Pos2::new(x, bar.center().y - g.size().y / 2.0);
                            let color = if starting_soon(r, now, soon_secs) {
                                // faltam 30 min ou menos: amarelo com pulso atrás
                                any_soon = true;
                                paint_soon_pulse(&lp, Rect::from_min_size(pos, g.size()), time, dark);
                                soon_color(dark)
                            } else if r.done {
                                pal.text_weak.gamma_multiply(0.75)
                            } else {
                                pal.text_weak
                            };
                            lp.galley(pos, g, color);
                        }
                    }
                    // botão ✓ (concluir) / ↺ (reabrir), sempre visível na ponta da
                    // barra; registrado depois da barra para ficar por cima no hit-test
                    if bar.width() > 28.0 {
                        let btn = Rect::from_center_size(
                            Pos2::new(bar.right() - 11.0, bar.center().y),
                            Vec2::splat(16.0),
                        );
                        let btn_resp = ui
                            .interact(btn, ui.scope_id().with(("task-done", &r.id)), Sense::click())
                            .on_hover_text(if r.done { tr("Reabrir") } else { tr("Concluir") });
                        if btn_resp.hovered() {
                            hovered_now = Some(r.id.clone());
                        }
                        let bg = if btn_resp.hovered() {
                            Color32::WHITE
                        } else {
                            Color32::WHITE.gamma_multiply(if is_hover { 0.95 } else { 0.75 })
                        };
                        Self::paint_check(&p, btn, bg, fill, r.done);
                        if btn_resp.clicked() {
                            ask = Some((
                                r.id.clone(),
                                r.title.clone(),
                                if r.done { "reopen" } else { "complete" },
                            ));
                            continue;
                        }
                    }
                    if bar_resp.clicked() {
                        open_form = Some(Form::from_row(r));
                    }
                }
                if rows.is_empty() {
                    p.text(
                        Pos2::new(area.left() + 12.0, area.top() + 16.0),
                        Align2::LEFT_CENTER,
                        if self.scope != Scope::All { tr("Nada neste período. Clique em \"Tudo\" para ver todas as tarefas.") } else if self.show_done { tr("Nenhuma tarefa ainda. Clique em + Nova tarefa ou peça ao agente (tasks_create).") } else { tr("Nada pendente. Ligue \"mostrar concluídas\" para ver o histórico.") },
                        font,
                        pal.text_weak,
                    );
                }
            });
        // divisor lista | linha do tempo: arrastar muda a largura da lista
        {
            let x = header.left() + list_w;
            let grip = Rect::from_min_max(
                Pos2::new(x - 4.0, header.top()),
                Pos2::new(x + 4.0, header.top() + HEADER_H + body_h),
            );
            let grip_resp = ui.interact(
                grip,
                ui.scope_id().with("tasks-splitter"),
                Sense::click_and_drag(),
            );
            if grip_resp.hovered() || grip_resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if grip_resp.dragged() {
                self.list_w =
                    (self.list_w + grip_resp.drag_delta().x).clamp(LIST_W_MIN, LIST_W_MAX);
            }
            if grip_resp.double_clicked() {
                self.list_w = LIST_W;
            }
            let color = if grip_resp.hovered() || grip_resp.dragged() {
                pal.accent
            } else {
                pal.border
            };
            ui.painter().line_segment(
                [Pos2::new(x, grip.top()), Pos2::new(x, grip.bottom())],
                Stroke::new(if grip_resp.dragged() { 2.0 } else { 1.0 }, color),
            );
        }
        self.wheel_over_tl = wheel_now;
        if any_soon {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        if scoped.is_some() {
            // visão travada no período: nem arrastar nem zoom
            pan_delta = 0.0;
            zoom_at = None;
        }
        if pan_delta != 0.0 {
            self.view_start = Some(view_start - f64::from(pan_delta / zoom));
        }
        if let Some((mx, f)) = zoom_at {
            let h_at_mouse = view_start + f64::from((mx - tl_header.left()) / zoom);
            let new_zoom = (zoom * f).clamp(min_zoom, max_zoom);
            self.px_per_hour = Some(new_zoom);
            self.view_start = Some(h_at_mouse - f64::from((mx - tl_header.left()) / new_zoom));
        }
        self.hovered = hovered_now;
        let _ = x_of;
        if let Some(a) = ask {
            self.confirm = Some(a);
        }
        if let Some(mut f) = open_form {
            f.desc_edit = f.description.trim().is_empty();
            ui::select_tab(ui.ctx(), "task-form", "basic");
            self.form = Some(f);
        }
        self.form_ui(ui, cx);
        self.confirm_ui(ui, cx);
        self.persist_prefs(ui.ctx());
    }

    /// Eixo do tempo: faixas por dia, linhas de dia/hora e a linha do agora.
    /// `labels` = desenhar os rótulos (cabeçalho).
    #[allow(clippy::too_many_arguments)]
    fn paint_time_axis(
        &self,
        p: &egui::Painter,
        tl: Rect,
        view_start: f64,
        zoom: f32,
        now_h: f64,
        pal: &ui::Palette,
        dark: bool,
        labels: bool,
    ) {
        // tudo do eixo fica recortado à linha do tempo (nada vaza para a lista)
        let p = &p.with_clip_rect(tl);
        let hours_visible = f64::from(tl.width() / zoom);
        let first_day = (view_start / 24.0).floor() * 24.0;
        let x_of = |h: f64| -> f32 { tl.left() + ((h - view_start) as f32) * zoom };
        // escala do eixo conforme os pixels por dia: dias (com ticks de hora),
        // semanas (segunda a domingo) ou meses
        let px_day = zoom * 24.0;
        #[derive(PartialEq, Clone, Copy)]
        enum Scale {
            Day,
            Week,
            Month,
        }
        let scale = if px_day >= 30.0 {
            Scale::Day
        } else if px_day >= 7.0 {
            Scale::Week
        } else {
            Scale::Month
        };
        // passo dos ticks de hora conforme o zoom (só na escala de dias)
        let hour_step = if scale != Scale::Day {
            0
        } else if zoom >= 40.0 {
            1
        } else if zoom >= 18.0 {
            3
        } else if zoom >= 8.0 {
            6
        } else if zoom >= 4.0 {
            12
        } else {
            0
        };
        let mut day = first_day;
        let mut k = 0; // índice do período (dia/semana/mês) para alternar o fundo
                       // os rótulos são desenhados depois do laço: senão as faixas dos dias
                       // seguintes cobrem o texto que ultrapassa o primeiro dia do período
        let mut texts: Vec<(Pos2, Align2, String, egui::FontId, Color32)> = Vec::new();
        while day < view_start + hours_visible {
            let x0 = x_of(day);
            let x1 = x_of(day + 24.0);
            let Some(n) = DateTime::from_timestamp((day * 3600.0) as i64, 0).map(|d| d.naive_utc())
            else {
                day += 24.0;
                continue;
            };
            let weekend = matches!(n.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun);
            let period_start = match scale {
                Scale::Day => true,
                Scale::Week => n.weekday() == chrono::Weekday::Mon,
                Scale::Month => n.day() == 1,
            };
            if period_start {
                k += 1;
            }
            let band = Rect::from_min_max(
                Pos2::new(x0.max(tl.left()), tl.top()),
                Pos2::new(x1.min(tl.right()), tl.bottom()),
            );
            if band.width() > 0.0 {
                let shade = if weekend && px_day >= 7.0 {
                    pal.card_alt.gamma_multiply(if dark { 0.9 } else { 1.0 })
                } else if k % 2 == 0 {
                    pal.card
                } else {
                    pal.panel
                };
                p.rect_filled(band, 0.0, shade);
            }
            // separador: forte no início do período, fraco entre dias (semanas)
            if period_start {
                p.line_segment(
                    [Pos2::new(x0, tl.top()), Pos2::new(x0, tl.bottom())],
                    Stroke::new(1.0, pal.border),
                );
            } else if scale == Scale::Week {
                p.line_segment(
                    [Pos2::new(x0, tl.top()), Pos2::new(x0, tl.bottom())],
                    Stroke::new(1.0, pal.border.gamma_multiply(0.35)),
                );
            }
            if hour_step > 0 {
                let mut h = hour_step;
                while h < 24 {
                    let x = x_of(day + f64::from(h));
                    if x > tl.left() && x < tl.right() {
                        p.line_segment(
                            [
                                Pos2::new(x, if labels { tl.bottom() - 10.0 } else { tl.top() }),
                                Pos2::new(x, tl.bottom()),
                            ],
                            Stroke::new(1.0, pal.border.gamma_multiply(0.5)),
                        );
                        if labels && zoom >= 8.0 {
                            texts.push((
                                Pos2::new(x + 2.0, tl.bottom() - 11.0),
                                Align2::LEFT_BOTTOM,
                                format!("{h:02}h"),
                                egui::FontId::proportional(9.5),
                                pal.text_weak,
                            ));
                        }
                    }
                    h += hour_step;
                }
            }
            if labels {
                let is_today = (now_h / 24.0).floor() * 24.0 == day;
                match scale {
                    Scale::Day => {
                        let text = if px_day > 90.0 {
                            format!("{} {}", weekday_pt(n), n.format("%d/%m"))
                        } else {
                            n.format("%d/%m").to_string()
                        };
                        let lx = (x0 + 6.0).max(tl.left() + 6.0);
                        if x1 - lx > 24.0 {
                            texts.push((
                                Pos2::new(lx, tl.top() + 6.0),
                                Align2::LEFT_TOP,
                                text,
                                if is_today {
                                    ui::semibold(12.0)
                                } else {
                                    ui::medium(12.0)
                                },
                                if is_today { pal.accent } else { pal.text_weak },
                            ));
                        }
                    }
                    Scale::Week if period_start => {
                        // rótulo da semana: data da segunda + número ISO
                        let text = format!("{} · S{}", n.format("%d/%m"), n.iso_week().week());
                        let lx = (x0 + 6.0).max(tl.left() + 6.0);
                        if x_of(day + 7.0 * 24.0) - lx > 30.0 {
                            texts.push((
                                Pos2::new(lx, tl.top() + 6.0),
                                Align2::LEFT_TOP,
                                text,
                                ui::medium(12.0),
                                pal.text_weak,
                            ));
                        }
                        // dia da semana abaixo, quando cabe
                        if px_day >= 14.0 {
                            let mut d = day;
                            for _ in 0..7 {
                                let dx = x_of(d) + 2.0;
                                if let Some(dn) = DateTime::from_timestamp((d * 3600.0) as i64, 0) {
                                    let dn = dn.naive_utc();
                                    let today = (now_h / 24.0).floor() * 24.0 == d;
                                    texts.push((
                                        Pos2::new(dx, tl.bottom() - 2.0),
                                        Align2::LEFT_BOTTOM,
                                        dn.format("%d").to_string(),
                                        egui::FontId::proportional(9.5),
                                        if today { pal.accent } else { pal.text_weak },
                                    ));
                                }
                                d += 24.0;
                            }
                        }
                    }
                    Scale::Month if period_start => {
                        // rótulo conforme a largura do mês: "nov 2026" → "nov" → só o ano em janeiro
                        let days_in_month = {
                            let (y, m) = (n.year(), n.month());
                            let next = if m == 12 {
                                chrono::NaiveDate::from_ymd_opt(y + 1, 1, 1)
                            } else {
                                chrono::NaiveDate::from_ymd_opt(y, m + 1, 1)
                            };
                            next.zip(chrono::NaiveDate::from_ymd_opt(y, m, 1))
                                .map(|(a, b)| (a - b).num_days())
                                .unwrap_or(30) as f32
                        };
                        let month_w = days_in_month * px_day;
                        let text = if month_w >= 78.0 {
                            Some(format!("{} {}", month_pt(n.month()), n.format("%Y")))
                        } else if month_w >= 34.0 {
                            Some(if n.month() == 1 {
                                format!("{} {}", month_pt(1), n.format("%y"))
                            } else {
                                month_pt(n.month()).to_string()
                            })
                        } else if n.month() == 1 {
                            Some(n.format("%Y").to_string())
                        } else {
                            None
                        };
                        let Some(text) = text else {
                            day += 24.0;
                            continue;
                        };
                        let lx = (x0 + 6.0).max(tl.left() + 6.0);
                        texts.push((
                            Pos2::new(lx, tl.top() + 6.0),
                            Align2::LEFT_TOP,
                            text,
                            ui::medium(12.0),
                            pal.text_weak,
                        ));
                    }
                    _ => {}
                }
                // na escala de semanas/meses, marca o dia de hoje como uma faixa fina
                if is_today && scale != Scale::Day && band.width() > 0.0 {
                    p.rect_filled(band, 0.0, pal.accent.gamma_multiply(0.12));
                }
            }
            day += 24.0;
        }
        for (pos, align, text, font, color) in texts {
            p.text(pos, align, text, font, color);
        }
        // linha do agora
        let nx = x_of(now_h);
        if nx >= tl.left() && nx <= tl.right() {
            p.line_segment(
                [Pos2::new(nx, tl.top()), Pos2::new(nx, tl.bottom())],
                Stroke::new(2.0, pal.accent),
            );
            if labels {
                let n = local_naive(Utc::now(), self.offset_minutes);
                let r = p.text(
                    Pos2::new(nx + 4.0, tl.bottom() - 2.0),
                    Align2::LEFT_BOTTOM,
                    trf("agora {}", &[&n.format("%H:%M")]),
                    egui::FontId::proportional(10.0),
                    Color32::WHITE,
                );
                p.rect_filled(r.expand(2.0), 3.0, pal.accent);
                p.text(
                    Pos2::new(nx + 4.0, tl.bottom() - 2.0),
                    Align2::LEFT_BOTTOM,
                    trf("agora {}", &[&n.format("%H:%M")]),
                    egui::FontId::proportional(10.0),
                    Color32::WHITE,
                );
            }
        }
    }

    /// Guarda largura, zoom, posição e o toggle de concluídas em `ui-prefs.json`
    /// (com debounce de ~0,8 s, para não escrever a cada quadro de arraste).
    fn persist_prefs(&mut self, ctx: &egui::Context) {
        let cur = crate::ui_prefs::GanttPrefs {
            list_w: Some(self.list_w),
            px_per_hour: self.px_per_hour,
            view_start: self.view_start,
            show_done: self.show_done,
            scope: self.scope.as_str().into(),
        };
        if self.last_saved.as_ref() == Some(&cur) {
            self.dirty_since = None;
            return;
        }
        let since = *self.dirty_since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() >= Duration::from_millis(800) {
            if let Err(e) = crate::ui_prefs::save_gantt(&cur) {
                tracing::warn!(error = %e, "preferências do Gantt não salvas");
            }
            self.last_saved = Some(cur);
            self.dirty_since = None;
        } else {
            ctx.request_repaint_after(Duration::from_millis(850));
        }
    }

    fn form_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let offset = self.offset_minutes;
        let mut md_cache = std::mem::take(&mut self.md_cache);
        let Some(f) = &mut self.form else {
            self.md_cache = md_cache;
            return;
        };
        let mut close = false;
        let mut ask: Option<(String, String, &'static str)> = None;
        let title = if f.editing.is_some() {
            tr("Editar tarefa")
        } else {
            tr("Nova tarefa")
        };
        let dark = ui.visuals().dark_mode;
        let pal = ui::palette(dark);
        let frame = egui::Frame::window(ui.style())
            .inner_margin(egui::Margin::symmetric(28, 22))
            .corner_radius(12.0);
        egui::Window::new(title)
            .id(egui::Id::unique("task-form"))
            .title_bar(false)
            .frame(frame)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.set_width(880.0);
                ui.spacing_mut().item_spacing.y = 12.0;
                // cabeçalho: título do modal + estado (na edição)
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(title).font(ui::semibold(18.0)));
                    if f.editing.is_some() {
                        ui.add_space(8.0);
                        if f.done && f.skipped {
                            ui::badge(ui, tr("não realizada"), Tone::Neutral);
                        } else if f.done {
                            ui::badge(ui, tr("concluída"), Tone::Success);
                        }
                        if f.linked && f.read_only {
                            ui::badge(ui, tr("Google Agenda · só leitura"), Tone::Info).on_hover_text(tr(
                                "Evento especial do Google (aniversário, Gmail, fora do escritório, foco): a agenda não aceita alteração, então editar ou apagar esta tarefa não mexe nele.",
                            ));
                        } else if f.linked {
                            ui::badge(ui, tr("Google Agenda"), Tone::Info);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui::icon_button(ui, crate::icons::CLOSE, tr("Fechar")).clicked() {
                            close = true;
                        }
                    });
                });
                let tab = ui::tabs(
                    ui,
                    "task-form",
                    &[
                        ui::Tab::new("basic", tr("Básico")),
                        ui::Tab::new("details", tr("Detalhes")),
                    ],
                );
                ui.add_space(8.0);
                // altura fixa para as abas não mudarem o tamanho do modal
                let (_, body_rect) = ui.allocate_space(Vec2::new(ui.available_width(), 0.0));
                let body_h = 430.0;
                let body =
                    Rect::from_min_size(body_rect.min, Vec2::new(ui.available_width(), body_h));
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(body)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.spacing_mut().item_spacing.y = 12.0;
                if tab == "basic" {
                    let ui = &mut child;
                    ui::field(ui, tr("Título"), None, |ui| {
                        ui::text_input(ui, &mut f.title, tr("ex.: Enviar proposta"));
                    });
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(tr(
                                "O que a tarefa envolve; o agente lê e escreve em Markdown.",
                            ))
                            .weak()
                            .small(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label = if f.desc_edit {
                                tr("Visualizar")
                            } else {
                                tr("Editar")
                            };
                            let ic = if f.desc_edit {
                                crate::icons::VISIBILITY
                            } else {
                                crate::icons::EDIT
                            };
                            if ui
                                .add(egui::Button::new(ui::icon_text(ui, ic, label, Some(13.0))))
                                .clicked()
                            {
                                f.desc_edit = !f.desc_edit;
                            }
                        });
                    });
                    let area_h = body_h - 96.0;
                    if f.desc_edit {
                        egui::ScrollArea::vertical()
                            .id_salt("task-desc-edit")
                            .max_height(area_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.add_sized(
                                    [ui.available_width(), area_h - 4.0],
                                    egui::TextEdit::multiline(&mut f.description)
                                        .hint_text(tr("Escreva em Markdown…"))
                                        .font(egui::TextStyle::Monospace),
                                );
                            });
                    } else {
                        egui::Frame::new()
                            .fill(pal.card)
                            .stroke(Stroke::new(1.0, pal.border))
                            .corner_radius(8.0)
                            .inner_margin(14.0)
                            .show(ui, |ui| {
                                ui.set_min_height(area_h - 30.0);
                                ui.set_width(ui.available_width());
                                egui::ScrollArea::vertical()
                                    .id_salt("task-desc-view")
                                    .max_height(area_h - 30.0)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        if f.description.trim().is_empty() {
                                            ui.label(
                                                egui::RichText::new(tr(
                                                    "Sem descrição. Clique em Editar.",
                                                ))
                                                .weak(),
                                            );
                                        } else {
                                            let text = ui::clean_emoji(&f.description);
                                            egui_commonmark::CommonMarkViewer::new().show(
                                                ui,
                                                &mut md_cache,
                                                &text,
                                            );
                                        }
                                    });
                            });
                    }
                } else {
                    let ui = &mut child;
                    ui::field(
                        ui,
                        tr("Início"),
                        Some(tr("Data e hora obrigatórias, no fuso das configurações.")),
                        |ui| {
                            ui.horizontal(|ui| {
                                ui.add(date_picker(&mut f.start_date, "task-start-date"));
                                ui.add_space(6.0);
                                time_picker(ui, "task-start-time", &mut f.start_h, &mut f.start_m);
                                ui.add_space(6.0);
                                if ui.small_button(tr("agora")).clicked() {
                                    let n = next_quarter(offset);
                                    f.start_date = jdate(n.date());
                                    f.start_h = n.hour();
                                    f.start_m = n.minute();
                                }
                            });
                        },
                    );
                    ui::field(
                        ui,
                        tr("Fim"),
                        Some(tr(
                            "Opcional. Depois dele, sem concluir, a tarefa fica atrasada.",
                        )),
                        |ui| {
                            ui.horizontal(|ui| {
                                if ui::toggle(ui, &mut f.has_end).changed() && f.has_end {
                                    f.set_end_from_start(1);
                                }
                                ui.label(
                                    egui::RichText::new(if f.has_end {
                                        tr("com fim")
                                    } else {
                                        tr("sem fim (nunca atrasa)")
                                    })
                                    .small(),
                                );
                            });
                            if f.has_end {
                                ui.horizontal(|ui| {
                                    ui.add(date_picker(&mut f.end_date, "task-end-date"));
                                    ui.add_space(6.0);
                                    time_picker(ui, "task-end-time", &mut f.end_h, &mut f.end_m);
                                    ui.add_space(6.0);
                                    if ui.small_button("+1h").clicked() {
                                        f.set_end_from_start(1);
                                    }
                                    if ui.small_button("+4h").clicked() {
                                        f.set_end_from_start(4);
                                    }
                                    if ui.small_button(tr("+1 dia")).clicked() {
                                        f.set_end_from_start(24);
                                    }
                                    if ui.small_button(tr("+1 semana")).clicked() {
                                        f.set_end_from_start(24 * 7);
                                    }
                                });
                            }
                        },
                    );
                    ui::field(ui, tr("Prioridade"), None, |ui| {
                        ui::select(
                            ui,
                            "task-priority",
                            &mut f.priority,
                            &[
                                ("high", tr("Alta")),
                                ("medium", tr("Média")),
                                ("low", tr("Baixa")),
                            ],
                        );
                    });
                    ui::field(
                        ui,
                        tr("Tags"),
                        Some(tr("Digite e Enter (ou vírgula) para adicionar. Vira minúsculas sem acento; espaço vira traço.")),
                        |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let mut remove: Option<usize> = None;
                                for (i, t) in f.tags.iter().enumerate() {
                                    if ui
                                        .add(
                                            egui::Button::new(egui::RichText::new(format!("{t} ×")).small())
                                                .corner_radius(10.0),
                                        )
                                        .on_hover_text(tr("remover"))
                                        .clicked()
                                    {
                                        remove = Some(i);
                                    }
                                }
                                if let Some(i) = remove {
                                    f.tags.remove(i);
                                }
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut f.tag_input)
                                        .hint_text(tr("nova tag"))
                                        .desired_width(140.0),
                                );
                                let enter = r.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if enter || f.tag_input.contains(',') {
                                    f.commit_tag_input();
                                    if enter {
                                        r.request_focus();
                                    }
                                }
                            });
                            // prévia do que vai ser gravado enquanto digita
                            if let Some(p) = will_core::task::normalize_tag(&f.tag_input) {
                                if p != f.tag_input.trim() {
                                    ui.label(egui::RichText::new(format!("→ {p}")).small().weak());
                                }
                            }
                        },
                    );
                }
                ui.advance_cursor_after_rect(body);
                if let Some(e) = &f.error {
                    ui::error_text(ui, e);
                }
                ui.add_space(6.0);
                ui.painter().line_segment(
                    [
                        Pos2::new(ui.min_rect().left(), ui.cursor().top()),
                        Pos2::new(ui.min_rect().right(), ui.cursor().top()),
                    ],
                    Stroke::new(1.0, pal.border),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let saving = f.saving();
                    // esquerda: ações secundárias/destrutivas (só na edição)
                    if let Some(id) = f.editing.clone() {
                        if ui::danger_button(ui, tr("Excluir")).clicked() {
                            ask = Some((id.clone(), f.title.clone(), "delete"));
                        }
                        ui.add_space(6.0);
                        if f.done {
                            if ui.button(tr("Reabrir")).clicked() {
                                ask = Some((id, f.title.clone(), "reopen"));
                            }
                        } else {
                            if ui.button(tr("Concluir")).clicked() {
                                ask = Some((id.clone(), f.title.clone(), "complete"));
                            }
                            if ui
                                .button(tr("Não realizada"))
                                .on_hover_text(tr(
                                    "Fecha sem dizer que foi feita (faltou, desistiu): some do painel como a concluída, mas fica registrado.",
                                ))
                                .clicked()
                            {
                                ask = Some((id, f.title.clone(), "skip"));
                            }
                        }
                    }
                    // direita: Cancelar e a ação primária (Salvar) na ponta
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let save = ui.add_enabled(
                            !saving,
                            egui::Button::new(
                                egui::RichText::new(if f.editing.is_some() {
                                    tr("Salvar")
                                } else {
                                    tr("Criar tarefa")
                                })
                                .color(Color32::WHITE),
                            )
                            .fill(pal.accent)
                            .min_size(Vec2::new(120.0, 32.0)),
                        );
                        if save.clicked() {
                            match f.body() {
                                Ok(body) => {
                                    f.error = None;
                                    let editing = f.editing.clone();
                                    f.save = Some(cx.svc.call(move |c| async move {
                                        match editing {
                                            Some(id) => c.update_task(&id, body).await,
                                            None => c.create_task(body).await,
                                        }
                                    }));
                                }
                                Err(e) => f.error = Some(e),
                            }
                        }
                        if saving {
                            ui.spinner();
                        }
                    });
                });
            });
        // Esc fecha o formulário (sem salvar), como o X do cabeçalho
        if self.confirm.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if close {
            self.form = None;
        }
        if let Some(a) = ask {
            self.confirm = Some(a);
        }
        self.md_cache = md_cache;
    }

    fn confirm_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let Some((id, title, kind)) = self.confirm.clone() else {
            return;
        };
        let (mtitle, body, label, danger, done_msg) = match kind {
            "delete" => (
                tr("Excluir tarefa?"),
                trf(
                    "\"{}\" será removida definitivamente. Para encerrar sem apagar, use Concluir.",
                    &[&title],
                ),
                tr("Excluir"),
                true,
                tr("Tarefa excluída."),
            ),
            "skip" => (
                tr("Marcar como não realizada?"),
                trf(
                    "\"{}\" some do painel como a concluída, mas fica registrado que não foi feita (não conta como realizada).",
                    &[&title],
                ),
                tr("Não realizada"),
                false,
                tr("Tarefa marcada como não realizada."),
            ),
            "reopen" => (
                tr("Reabrir tarefa?"),
                trf(
                    "\"{}\" volta a pendente e o estado passa a seguir o relógio.",
                    &[&title],
                ),
                tr("Reabrir"),
                false,
                tr("Tarefa reaberta."),
            ),
            _ => (
                tr("Concluir tarefa?"),
                trf(
                    "\"{}\" fica verde e some do painel (ligue \"mostrar concluídas\" para ver).",
                    &[&title],
                ),
                tr("Concluir"),
                false,
                tr("Tarefa concluída."),
            ),
        };
        let c = ui::Confirm {
            title: mtitle.into(),
            body,
            confirm_label: label.into(),
            danger,
            tag: format!("task-{kind}-{id}"),
        };
        if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
            self.confirm = None;
            if ok {
                self.form = None;
                self.act(cx, done_msg, id, kind);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn relative_short_picks_one_unit_without_seconds() {
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        let at = |secs: i64| now + chrono::Duration::seconds(secs);
        assert_eq!(relative_short(now, at(30)), "agora");
        assert_eq!(relative_short(now, at(-30)), "agora");
        assert_eq!(relative_short(now, at(60)), "em 1 min");
        assert_eq!(relative_short(now, at(-119)), "há 1 min");
        assert_eq!(relative_short(now, at(3 * 3600 + 1800)), "em 3 h");
        assert_eq!(relative_short(now, at(-2 * 86_400)), "há 2 d");
        assert_eq!(relative_short(now, at(20 * 86_400)), "em 2 sem");
        assert_eq!(relative_short(now, at(-70 * 86_400)), "há 1 mês");
        assert_eq!(relative_short(now, at(200 * 86_400)), "em 6 meses");
    }

    #[test]
    fn starting_soon_only_for_open_tasks_within_30_min() {
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        let m = |n: i64| now + chrono::Duration::minutes(n);
        let row = |start, done| Row {
            id: "x".into(),
            title: "t".into(),
            description: String::new(),
            start,
            end: None,
            done,
            skipped: false,
            priority: 2,
            start_local: String::new(),
            end_local: String::new(),
            linked: false,
            read_only: false,
            tags: vec![],
        };
        let s = 30 * 60;
        assert!(starting_soon(&row(m(30), false), now, s));
        assert!(starting_soon(&row(m(1), false), now, s));
        assert!(!starting_soon(&row(m(31), false), now, s));
        assert!(!starting_soon(&row(m(-1), false), now, s));
        assert!(!starting_soon(&row(m(10), true), now, s));
        // corte configurável; 0 desliga
        assert!(starting_soon(&row(m(50), false), now, 60 * 60));
        assert!(!starting_soon(&row(m(1), false), now, 0));
    }

    #[test]
    fn relative_label_side_follows_state() {
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        let h = |n: i64| now + chrono::Duration::hours(n);
        let row = |start, end, done| Row {
            id: "x".into(),
            title: "t".into(),
            description: String::new(),
            start,
            end,
            done,
            skipped: false,
            priority: 2,
            start_local: String::new(),
            end_local: String::new(),
            linked: false,
            read_only: false,
            tags: vec![],
        };
        assert_eq!(
            relative_label(&row(h(2), Some(h(3)), false), now),
            ("em 2 h".to_string(), true)
        );
        assert_eq!(
            relative_label(&row(h(-1), Some(h(3)), false), now),
            ("há 1 h".to_string(), true)
        );
        assert_eq!(
            relative_label(&row(h(-5), None, false), now),
            ("há 5 h".to_string(), true)
        );
        assert_eq!(
            relative_label(&row(h(-5), Some(h(-2)), false), now),
            ("há 2 h".to_string(), false)
        );
        assert_eq!(
            relative_label(&row(h(-5), Some(h(-2)), true), now),
            ("há 2 h".to_string(), false)
        );
    }
}
