//! Threads: lista à esquerda, conversa à direita (histórico + turno ao vivo),
//! envio imediato (inclusive no meio de um turno), reiniciar/reset/interromper,
//! renomear e apagar.

use super::Cx;
use crate::i18n::{tr, trf};
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use serde_json::Value;
use std::collections::HashMap;
use will_core::{EngineDelta, Event, ThreadId, ThreadStatus, ThreadSummary, TurnResult};

#[derive(Default)]
struct LiveTurn {
    turn_id: Option<String>,
    items: Vec<EngineDelta>,
    text: String,
    started: Option<std::time::Instant>,
}

#[derive(Default)]
pub struct ThreadsPage {
    list: Loader<Value>,
    threads: Vec<ThreadSummary>,
    selected: Option<ThreadId>,
    turns: Loader<Value>,
    turns_for: Option<ThreadId>,
    live: HashMap<ThreadId, LiveTurn>,
    notes: HashMap<ThreadId, Vec<String>>,
    input: String,
    /// Arquivos escolhidos (botão Anexar ou arrastar-e-soltar) ainda não enviados.
    pending_files: Vec<std::path::PathBuf>,
    send_task: Option<Task<Value>>,
    action_task: Option<(&'static str, Task<Value>)>,
    create_task: Option<Task<Value>>,
    rename_buf: Option<String>,
    confirm_delete: Option<(ThreadId, bool)>,
    confirm_reset: Option<ThreadId>,
    show_archived: bool,
    focus_input: bool,
    /// Altura medida do bloco de entrada no quadro anterior (define quanto
    /// sobra para o histórico sem a página rolar).
    bottom_h: f32,
    md_cache: egui_commonmark::CommonMarkCache,
}

fn origin_label(t: &ThreadSummary) -> String {
    match &t.origin {
        will_core::Origin::Ui => "app".into(),
        will_core::Origin::Telegram { chat_id, topic_id } => match topic_id {
            Some(tp) => format!("telegram {chat_id}:{tp}"),
            None => format!("telegram {chat_id}"),
        },
        will_core::Origin::Realtime => tr("voz").into(),
    }
}

fn title_of(t: &ThreadSummary) -> String {
    t.title
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("Thread {}", &t.id.to_string()[..8]))
}

fn status_tone(t: &ThreadSummary) -> Tone {
    match t.status {
        ThreadStatus::Running => Tone::Info,
        ThreadStatus::Live => Tone::Success,
        ThreadStatus::Error => Tone::Danger,
        ThreadStatus::Archived => Tone::Neutral,
        ThreadStatus::Idle => Tone::Neutral,
    }
}

/// Tempo relativo (traduzido): "agora", "há 1 min", "há 5 min", "há 2 h",
/// "há 1 dia", "há 3 dias", "há 2 sem".
fn time_ago(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - *dt).num_seconds().max(0);
    let mins = secs / 60;
    let hours = secs / 3600;
    let days = secs / 86400;
    if secs < 60 {
        tr("agora").to_string()
    } else if mins < 60 {
        trf("há {} min", &[&mins])
    } else if hours < 24 {
        trf("há {} h", &[&hours])
    } else if days == 1 {
        tr("há 1 dia").to_string()
    } else if days < 7 {
        trf("há {} dias", &[&days])
    } else {
        trf("há {} sem", &[&(days / 7)])
    }
}

/// Data/hora exata, no fuso local, para tooltips.
fn fmt_exact(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.with_timezone(&chrono::Local)
        .format("%d/%m/%Y %H:%M:%S")
        .to_string()
}

/// Lê um campo RFC3339 do JSON do turno (`started_at`/`finished_at`).
fn turn_dt(turn: &Value, key: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    turn.get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// Rótulo discreto de tempo relativo; o hover mostra a data exata.
fn when_label(ui: &mut egui::Ui, pal: &ui::Palette, dt: Option<&chrono::DateTime<chrono::Utc>>) {
    if let Some(dt) = dt {
        ui.add(
            egui::Label::new(
                egui::RichText::new(time_ago(dt))
                    .size(11.0)
                    .color(pal.text_weak),
            )
            .sense(egui::Sense::hover()),
        )
        .on_hover_text(fmt_exact(dt));
    }
}

/// Botão pequeno "copiar" com área de clique real (moldura só no hover).
fn copy_button(ui: &mut egui::Ui, pal: &ui::Palette, hover: &str) -> bool {
    ui.add(
        egui::Button::new(
            egui::RichText::new(tr("copiar"))
                .size(11.0)
                .color(pal.text_weak),
        )
        .frame_when_inactive(false)
        .corner_radius(6.0)
        .min_size(egui::vec2(44.0, 18.0)),
    )
    .on_hover_text(hover)
    .clicked()
}

/// Ícone "i" desenhado (círculo + letra), sem depender de glifos especiais.
fn info_icon(ui: &mut egui::Ui, pal: &ui::Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let color = if resp.hovered() {
        pal.text
    } else {
        pal.text_weak
    };
    let c = rect.center();
    ui.painter()
        .circle_stroke(c, 6.5, egui::Stroke::new(1.0, color));
    ui.painter().text(
        c,
        egui::Align2::CENTER_CENTER,
        "i",
        egui::FontId::proportional(10.0),
        color,
    );
    resp
}

/// Corpo do popover de metadados do turno.
fn turn_info_body(ui: &mut egui::Ui, pal: &ui::Palette, turn: &Value) {
    ui.set_min_width(220.0);
    ui.spacing_mut().item_spacing.y = 2.0;
    let res = turn.get("result").cloned().unwrap_or(Value::Null);
    let row = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(k).small().color(pal.text_weak));
            ui.add(
                egui::Label::new(egui::RichText::new(v).small().color(pal.text))
                    .truncate()
                    .selectable(true),
            );
        });
    };
    if let Some(m) = res.get("model").and_then(|c| c.as_str()) {
        row(ui, tr("modelo"), m.to_string());
    }
    if let Some(d) = res.get("duration_ms").and_then(|c| c.as_u64()) {
        row(ui, tr("duração"), ui::fmt_ms(d));
    }
    if let Some(c) = res.get("cost_usd").and_then(|c| c.as_f64()) {
        row(ui, tr("custo"), ui::fmt_usd(c));
    }
    if let Some(dt) = turn_dt(turn, "started_at") {
        row(ui, tr("início"), fmt_exact(&dt));
    }
    if let Some(dt) = turn_dt(turn, "finished_at") {
        row(ui, tr("fim"), fmt_exact(&dt));
    }
    if let Some(sid) = res.get("session_id").and_then(|c| c.as_str()) {
        row(ui, tr("sessão"), sid.to_string());
    }
    let id = ui::s(turn, "id");
    if !id.is_empty() {
        row(ui, tr("turno"), id);
    }
}

impl ThreadsPage {
    fn reload_list(&mut self, cx: &Cx) {
        self.list
            .start(cx.svc.call(|c| async move { c.threads().await }));
    }

    fn reload_turns(&mut self, cx: &Cx, id: ThreadId) {
        self.turns_for = Some(id);
        let sid = id.to_string();
        self.turns.start(cx.svc.call(move |c| async move {
            c.get_q(&format!("threads/{sid}/turns"), &[("limit", "200".into())])
                .await
        }));
    }

    /// Seleciona uma thread (usado pela captura de diagnóstico).
    pub fn select(&mut self, id: ThreadId) {
        self.selected = Some(id);
        self.list.loaded_at = None;
        self.turns.loaded_at = None;
    }

    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::ThreadCreated { thread } | Event::ThreadUpdated { thread } => {
                if let Some(t) = self.threads.iter_mut().find(|t| t.id == thread.id) {
                    *t = thread.clone();
                } else {
                    self.threads.insert(0, thread.clone());
                }
            }
            Event::ThreadDeleted { thread_id } => {
                self.threads.retain(|t| t.id != *thread_id);
                if self.selected == Some(*thread_id) {
                    self.selected = None;
                }
            }
            Event::TurnStarted { thread_id, turn_id } => {
                let l = self.live.entry(*thread_id).or_default();
                l.turn_id = Some(turn_id.to_string());
                if l.started.is_none() {
                    l.started = Some(std::time::Instant::now());
                }
            }
            Event::MessageAccepted {
                thread_id,
                mid_turn,
                ..
            } => {
                if *mid_turn {
                    self.notes
                        .entry(*thread_id)
                        .or_default()
                        .push(tr("mensagem entregue no meio do turno (steering)").into());
                }
                let l = self.live.entry(*thread_id).or_default();
                if l.started.is_none() {
                    l.started = Some(std::time::Instant::now());
                }
            }
            Event::TurnDelta {
                thread_id, delta, ..
            } => {
                let l = self.live.entry(*thread_id).or_default();
                match delta {
                    EngineDelta::TextDelta { text } => l.text.push_str(text),
                    other => l.items.push(other.clone()),
                }
            }
            Event::TurnFinished { thread_id, .. } | Event::TurnFailed { thread_id, .. } => {
                self.live.remove(thread_id);
                if self.selected == Some(*thread_id) {
                    self.turns.loaded_at = None;
                }
                self.list.loaded_at = None;
            }
            Event::StaleSession { thread_id } => {
                self.notes.entry(*thread_id).or_default().push(
                    tr("sessão expirada: a thread recomeçou do zero e reenviou as pendências")
                        .into(),
                );
            }
            _ => {}
        }
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.list.stale(std::time::Duration::from_secs(30)) {
            self.reload_list(cx);
        }
        if self.list.poll() {
            if let Some(v) = &self.list.data {
                if let Ok(list) = serde_json::from_value::<Vec<ThreadSummary>>(v.clone()) {
                    self.threads = list;
                }
            }
        }
        if let Some(id) = self.selected {
            if self.turns_for != Some(id) || (self.turns.loaded_at.is_none() && !self.turns.busy())
            {
                self.reload_turns(cx, id);
            }
        }
        self.turns.poll();
        if let Some(t) = &self.send_task {
            if let Some(r) = t.take() {
                self.send_task = None;
                match r {
                    Ok(v) => {
                        if ui::b(&v, "mid_turn") {
                            cx.toasts.info(tr("Entregue no meio do turno."));
                        }
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }
        if let Some((what, t)) = &self.action_task {
            if let Some(r) = t.take() {
                let what = *what;
                self.action_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts.success(trf("{}: ok", &[&tr(what)]));
                        self.list.loaded_at = None;
                        self.turns.loaded_at = None;
                    }
                    Err(e) => cx.toasts.error(format!("{}: {e}", tr(what))),
                }
            }
        }
        if let Some(t) = &self.create_task {
            if let Some(r) = t.take() {
                self.create_task = None;
                match r {
                    Ok(v) => {
                        if let Ok(t) = serde_json::from_value::<ThreadSummary>(v) {
                            self.selected = Some(t.id);
                            if !self.threads.iter().any(|x| x.id == t.id) {
                                self.threads.insert(0, t);
                            }
                            self.focus_input = true;
                        }
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }
    }

    fn send(&mut self, cx: &Cx) {
        let Some(id) = self.selected else { return };
        let text = self.input.trim().to_string();
        if (text.is_empty() && self.pending_files.is_empty()) || self.send_task.is_some() {
            return;
        }
        self.input.clear();
        let attachments: Vec<String> = self
            .pending_files
            .drain(..)
            .map(|p| p.display().to_string())
            .collect();
        let sid = id.to_string();
        self.send_task = Some(
            cx.svc
                .call(move |c| async move { c.send_message(&sid, &text, attachments).await }),
        );
        self.focus_input = true;
    }

    fn action(&mut self, cx: &Cx, id: ThreadId, what: &'static str) {
        let sid = id.to_string();
        let t: Task<Value> = match what {
            "Reiniciar" => cx
                .svc
                .call(move |c| async move { c.restart_thread(&sid).await }),
            "Reset" => cx
                .svc
                .call(move |c| async move { c.reset_thread(&sid).await }),
            "Interromper" => cx
                .svc
                .call(move |c| async move { c.interrupt_thread(&sid).await }),
            _ => return,
        };
        self.action_task = Some((what, t));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);
        let avail_h = ui.available_height();
        let pal = ui::palette(ui.visuals().dark_mode);
        ui.horizontal_top(|ui| {
            ui.set_min_height(avail_h);
            // ----- lista -----
            ui.vertical(|ui| {
                ui.set_width(228.0);
                ui.set_min_height(avail_h);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tr("Threads")).font(ui::semibold(16.0)));
                    if ui.add_enabled(self.create_task.is_none(), egui::Button::new(tr("+ Nova")).small()).clicked() {
                        self.create_task = Some(cx.svc.call(|c| async move { c.create_thread(None).await }));
                    }
                    if ui.add(egui::Button::new("↺").small()).on_hover_text(tr("Atualizar")).clicked() {
                        self.list.loaded_at = None;
                    }
                });
                ui.checkbox(&mut self.show_archived, egui::RichText::new(tr("arquivadas")).small());
                if let Some(e) = &self.list.error {
                    ui::error_text(ui, e);
                }
                egui::ScrollArea::vertical().id_salt("threads-list").auto_shrink([false, false]).max_height(avail_h - 70.0).show(ui, |ui| {
                    let mut sorted: Vec<&ThreadSummary> = self.threads.iter().filter(|t| self.show_archived || t.status != ThreadStatus::Archived).collect();
                    sorted.sort_by_key(|t| std::cmp::Reverse(t.last_activity_at));
                    if sorted.is_empty() {
                        ui.label(egui::RichText::new(tr("Nenhuma thread. Crie uma com “+ Nova” ou mande mensagem pelo Telegram/voz.")).weak());
                    }
                    let mut click = None;
                    let n = sorted.len();
                    let divider = egui::Stroke::new(1.0, pal.border.linear_multiply(0.6));
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for (i, t) in sorted.into_iter().enumerate() {
                        let selected = self.selected == Some(t.id);
                        let fill = if selected { ui.visuals().selection.bg_fill.linear_multiply(0.35) } else { egui::Color32::TRANSPARENT };
                        let r = egui::Frame::new().fill(fill).corner_radius(6.0).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
                            ui.horizontal(|ui| {
                                ui::dot(ui, status_tone(t));
                                ui.add(egui::Label::new(egui::RichText::new(title_of(t)).font(ui::medium(13.0))).truncate());
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                ui.label(egui::RichText::new(time_ago(&t.last_activity_at)).size(10.5).color(pal.text_weak));
                            });
                        });
                        // Detalhes saem da linha e vão para o tooltip.
                        let mut tip = format!("{}\n{}", origin_label(t), fmt_exact(&t.last_activity_at));
                        if t.pending > 0 {
                            tip.push('\n');
                            tip.push_str(&trf("{} pendente(s)", &[&t.pending]));
                        }
                        tip.push('\n');
                        tip.push_str(if t.alive { tr("processo vivo") } else { tr("processo dormente") });
                        if t.session_expired {
                            tip.push('\n');
                            tip.push_str(tr("sessão expirada"));
                        }
                        if r.response.interact(egui::Sense::click()).on_hover_text(tip).clicked() {
                            click = Some(t.id);
                        }
                        if i + 1 < n {
                            let rect = r.response.rect;
                            ui.painter().hline(
                                egui::Rangef::new(rect.left() + 8.0, rect.right() - 8.0),
                                rect.bottom() + 1.5,
                                divider,
                            );
                        }
                    }
                    if let Some(id) = click {
                        self.selected = Some(id);
                        self.rename_buf = None;
                        self.focus_input = true;
                    }
                });
            });
            ui.separator();
            // ----- conversa -----
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(avail_h);
                match self.selected.and_then(|id| self.threads.iter().find(|t| t.id == id).cloned()) {
                    None => {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(tr("Selecione uma thread ou crie uma nova.")).weak().size(16.0));
                        });
                    }
                    Some(t) => self.chat_ui(ui, cx, &t),
                }
            });
        });

        if let Some((id, purge)) = self.confirm_delete {
            let c = ui::Confirm {
                title: tr("Apagar thread").into(),
                body: if purge {
                    tr("Apagar a thread e a transcrição salva do Claude? Isso não tem volta.")
                        .into()
                } else {
                    tr("Apagar a thread do will-ia? A transcrição do Claude Code fica no disco.")
                        .into()
                },
                confirm_label: tr("Apagar").into(),
                danger: true,
                tag: "del-thread".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_delete = None;
                if ok {
                    let sid = id.to_string();
                    self.action_task = Some((
                        "Apagar",
                        cx.svc
                            .call(move |c| async move { c.delete_thread(&sid, purge).await }),
                    ));
                    self.selected = None;
                }
            }
        }
        if let Some(id) = self.confirm_reset {
            let c = ui::Confirm { title: tr("Reset da thread").into(), body: tr("Mata o processo e esquece a sessão: a próxima mensagem começa uma conversa nova com o Claude.").into(), confirm_label: tr("Reset").into(), danger: true, tag: "reset-thread".into() };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_reset = None;
                if ok {
                    self.action(cx, id, "Reset");
                }
            }
        }
    }

    fn chat_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, t: &ThreadSummary) {
        // cabeçalho
        ui.horizontal(|ui| {
            match &mut self.rename_buf {
                Some(buf) => {
                    let r = ui.add(egui::TextEdit::singleline(buf).desired_width(260.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("OK")).clicked() || enter {
                        let title = buf.clone();
                        let sid = t.id.to_string();
                        self.action_task = Some((
                            "Renomear",
                            cx.svc
                                .call(move |c| async move { c.rename_thread(&sid, &title).await }),
                        ));
                        self.rename_buf = None;
                    } else if ui.button(tr("Cancelar")).clicked() {
                        self.rename_buf = None;
                    }
                }
                None => {
                    // Título truncado: um nome longo não pode empurrar "renomear"
                    // para cima dos botões alinhados à direita.
                    let w = (ui.available_width() - 460.0).max(160.0);
                    ui.scope(|ui| {
                        ui.set_max_width(w);
                        ui.add(
                            egui::Label::new(egui::RichText::new(title_of(t)).heading()).truncate(),
                        )
                        .on_hover_text(title_of(t));
                    });
                    if ui.button(tr("renomear")).clicked() {
                        self.rename_buf = Some(title_of(t));
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("apagar")).clicked() {
                    self.confirm_delete = Some((t.id, false));
                }
                if ui
                    .button(tr("reset"))
                    .on_hover_text(tr("mata o processo e esquece a sessão"))
                    .clicked()
                {
                    self.confirm_reset = Some(t.id);
                }
                if ui
                    .button(tr("reiniciar"))
                    .on_hover_text(tr("mata o processo; volta com --resume"))
                    .clicked()
                {
                    self.action(cx, t.id, "Reiniciar");
                }
                if ui
                    .add_enabled(t.pending > 0, egui::Button::new(tr("interromper")).small())
                    .clicked()
                {
                    self.action(cx, t.id, "Interromper");
                }
            });
        });
        ui.horizontal(|ui| {
            ui::dot(ui, status_tone(t));
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} · {}",
                    origin_label(t),
                    t.status.as_str(),
                    ui::fmt_usd(t.total_cost_usd)
                ))
                .small()
                .weak(),
            );
            if let Some(sid) = &t.claude_session_id {
                ui.label(
                    egui::RichText::new(trf("sessão {}", &[&&sid[..sid.len().min(8)]]))
                        .small()
                        .weak(),
                );
            }
            if t.session_expired {
                ui::badge(
                    ui,
                    tr("sessão expirada no disco; vai recomeçar"),
                    Tone::Warning,
                );
            }
        });
        ui.separator();

        // histórico + ao vivo: usa exatamente o que sobra acima da caixa de
        // entrada (altura medida no quadro anterior), sem estourar a janela.
        let bottom_h = if self.bottom_h > 0.0 {
            self.bottom_h
        } else {
            84.0
        };
        let hist_h = (ui.available_height() - bottom_h - 8.0).max(120.0);
        let loading = self.turns.loading() && self.turns.data.is_none();
        let turns_err = self.turns.error.clone();
        let turns = self
            .turns
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let notes = self.notes.get(&t.id).cloned().unwrap_or_default();
        let live = self
            .live
            .get(&t.id)
            .map(|l| (l.items.clone(), l.text.clone(), l.started));
        let has_live = live.is_some();
        let dark = ui.visuals().dark_mode;
        let pal = ui::palette(dark);
        // Anexos são caminhos do servidor: abrir/miniatura só em loopback.
        let local = cx.snap.server_is_local;
        let toasts = &mut *cx.toasts;
        egui::ScrollArea::vertical()
            .id_salt(("chat", t.id))
            .auto_shrink([false, false])
            .max_height(hist_h)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(6.0);
                if loading {
                    ui::busy(ui, tr("carregando…"));
                }
                if let Some(e) = &turns_err {
                    ui::error_text(ui, e);
                }
                for g in group_turns(&turns) {
                    let turn = g.main;
                    // Escopo de id por turno: respostas com o mesmo Markdown
                    // (tabelas, blocos de código) não colidem entre si.
                    ui.push_id(ui::s(turn, "id"), |ui| {
                        // Balões do usuário do mesmo turno (a mensagem inicial
                        // e as enviadas no meio dele) ficam agrupados.
                        let prompts: Vec<&Value> = std::iter::once(turn)
                            .chain(g.extra.iter().copied())
                            .filter(|t| !ui::s(t, "prompt").is_empty())
                            .collect();
                        let n = prompts.len();
                        for (i, t) in prompts.iter().enumerate() {
                            let pos = match (n, i) {
                                (1, _) => GroupPos::Single,
                                (_, 0) => GroupPos::First,
                                (_, i) if i + 1 == n => GroupPos::Last,
                                _ => GroupPos::Middle,
                            };
                            let started = turn_dt(t, "started_at");
                            ui.push_id(ui::s(t, "id"), |ui| {
                                user_bubble(ui, pal, toasts, t, started.as_ref(), pos, local);
                            });
                        }
                        let text = ui::s(turn, "text");
                        let is_err = ui::b(turn, "is_error");
                        if is_err {
                            let msg = if text.is_empty() {
                                ui::s(turn, "error_kind")
                            } else {
                                text.clone()
                            };
                            error_card(ui, pal, &msg);
                        } else if !text.is_empty() {
                            assistant_card(ui, pal, &mut self.md_cache, toasts, &text, turn);
                        } else if turn.get("finished_at").map(|f| f.is_null()).unwrap_or(true)
                            && !has_live
                        {
                            ui.label(
                                egui::RichText::new(tr("aguardando resposta…"))
                                    .weak()
                                    .small(),
                            );
                        }
                    });
                }
                for n in &notes {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(n).small().weak().italics());
                    });
                }
                if let Some((items, text, started)) = &live {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(120));
                    ui.push_id(("live", t.id), |ui| {
                        live_card(ui, pal, &mut self.md_cache, items, text, *started);
                    });
                }
                ui.add_space(8.0);
            });

        // arrastar-e-soltar: arquivos soltos na janela viram anexos pendentes
        let (dropped, hovering) = ui.ctx().input(|i| {
            (
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .collect::<Vec<_>>(),
                !i.raw.hovered_files.is_empty(),
            )
        });
        for p in dropped {
            if p.is_file() && !self.pending_files.contains(&p) {
                self.pending_files.push(p);
            }
        }

        // entrada: anexos pendentes + caixa de texto + Anexar/Enviar
        let mut send_now = false;
        let busy = self.send_task.is_some();
        let pal2 = ui::palette(ui.visuals().dark_mode);
        let bottom = ui.scope(|ui| {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(pal2.card)
                .stroke(egui::Stroke::new(
                    1.0,
                    if hovering { pal2.accent } else { pal2.border },
                ))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !self.pending_files.is_empty() {
                        let mut remove: Option<usize> = None;
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
                            for (i, p) in self.pending_files.iter().enumerate() {
                                let name =
                                    p.file_name().and_then(|n| n.to_str()).unwrap_or("arquivo");
                                egui::Frame::new()
                                    .fill(pal2.accent.linear_multiply(0.18))
                                    .stroke(egui::Stroke::new(
                                        1.0,
                                        pal2.accent.linear_multiply(0.5),
                                    ))
                                    .corner_radius(8.0)
                                    .inner_margin(egui::Margin::symmetric(8, 3))
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(name)
                                                    .size(12.0)
                                                    .color(pal2.text),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(p.display().to_string());
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    egui::RichText::new("×")
                                                        .size(13.0)
                                                        .color(pal2.text_weak),
                                                )
                                                .frame(false),
                                            )
                                            .on_hover_text(tr("remover anexo"))
                                            .clicked()
                                        {
                                            remove = Some(i);
                                        }
                                    });
                            }
                        });
                        if let Some(i) = remove {
                            self.pending_files.remove(i);
                        }
                        ui.add_space(4.0);
                    }
                    ui.horizontal(|ui| {
                        let btn_w = 92.0 + 76.0;
                        let text_w = (ui.available_width() - btn_w - 12.0).max(120.0);
                        let hint = if hovering {
                            tr("Solte os arquivos para anexar.")
                        } else if !self.pending_files.is_empty() {
                            tr("Comentário opcional sobre os anexos. Ctrl+Enter envia.")
                        } else if t.pending > 0 {
                            tr("Turno em andamento: a mensagem entra no meio. Ctrl+Enter envia.")
                        } else {
                            tr("Mensagem para o Claude. Ctrl+Enter envia.")
                        };
                        let r = ui.add_sized(
                            [text_w, 54.0],
                            egui::TextEdit::multiline(&mut self.input)
                                .frame(egui::Frame::NONE)
                                .desired_rows(2)
                                .hint_text(hint),
                        );
                        if self.focus_input {
                            r.request_focus();
                            self.focus_input = false;
                        }
                        if r.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command)
                        {
                            send_now = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let can = !busy
                                && (!self.input.trim().is_empty()
                                    || !self.pending_files.is_empty());
                            let b = egui::Button::new(
                                egui::RichText::new(if busy { "…" } else { tr("Enviar") })
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(if can {
                                pal2.accent
                            } else {
                                pal2.accent.linear_multiply(0.45)
                            })
                            .min_size(egui::vec2(84.0, 34.0));
                            if ui.add_enabled(can, b).clicked() {
                                send_now = true;
                            }
                            // Anexar: diálogo nativo de arquivos (vários de uma vez)
                            let a = egui::Button::new(
                                egui::RichText::new(tr("Anexar")).color(pal2.text),
                            )
                            .min_size(egui::vec2(68.0, 34.0));
                            if ui
                                .add_enabled(!busy, a)
                                .on_hover_text(tr("Anexar arquivos (ou arraste para a janela)"))
                                .clicked()
                            {
                                if let Some(files) = rfd::FileDialog::new()
                                    .set_title(tr("Anexar arquivos"))
                                    .pick_files()
                                {
                                    for p in files {
                                        if !self.pending_files.contains(&p) {
                                            self.pending_files.push(p);
                                        }
                                    }
                                }
                            }
                        });
                    });
                });
        });
        self.bottom_h = bottom.response.rect.height();
        if send_now {
            self.send(cx);
        }
    }
}

/// Renderiza Markdown (a IA responde em Markdown).
fn md(ui: &mut egui::Ui, cache: &mut egui_commonmark::CommonMarkCache, text: &str) {
    let text = ui::clean_emoji(text);
    // Sem `max_image_width`: o viewer usa a largura disponível como teto, então
    // blocos de código e imagens nunca alargam o balão além do chat.
    egui_commonmark::CommonMarkViewer::new().show(ui, cache, &text);
}

/// Um turno "principal" e os turnos que ele absorveu: mensagens enviadas
/// enquanto ele rodava (`merged_into`) ou, ao vivo, ainda sem `result`.
struct TurnGroup<'a> {
    main: &'a Value,
    extra: Vec<&'a Value>,
}

/// Agrupa a lista (em ordem cronológica) de turnos da API. Um turno com
/// `merged_into` entra no grupo do pai; um turno ainda aberto logo depois de
/// outro também aberto é steering ao vivo e entra no grupo anterior.
fn group_turns(turns: &[Value]) -> Vec<TurnGroup<'_>> {
    let mut groups: Vec<TurnGroup<'_>> = Vec::new();
    for t in turns {
        let merged_into = ui::s(t, "merged_into");
        if !merged_into.is_empty() {
            if let Some(g) = groups
                .iter_mut()
                .find(|g| ui::s(g.main, "id") == merged_into)
            {
                g.extra.push(t);
                continue;
            }
        }
        let open = t.get("finished_at").map(|f| f.is_null()).unwrap_or(true);
        if open && ui::s(t, "text").is_empty() {
            if let Some(g) = groups.last_mut() {
                let prev_open = g
                    .main
                    .get("finished_at")
                    .map(|f| f.is_null())
                    .unwrap_or(true);
                if prev_open && ui::s(g.main, "text").is_empty() {
                    g.extra.push(t);
                    continue;
                }
            }
        }
        groups.push(TurnGroup {
            main: t,
            extra: Vec::new(),
        });
    }
    groups
}

/// Posição do balão dentro de um grupo de mensagens do usuário.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupPos {
    Single,
    First,
    Middle,
    Last,
}

fn user_bubble(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    toasts: &mut ui::Toasts,
    turn: &Value,
    started: Option<&chrono::DateTime<chrono::Utc>>,
    pos: GroupPos,
    local: bool,
) {
    // `prompt_display` vem sem a nota de anexo que o canal acrescenta para o
    // cérebro; os anexos chegam estruturados em `attachments`.
    let text = turn
        .get("prompt_display")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| ui::s(turn, "prompt"));
    let atts: Vec<will_core::Attachment> = turn
        .get("attachments")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let raw = text.as_str();
    let shown = ui::clean_emoji(&text);
    let dark = ui.visuals().dark_mode;
    let fill = pal.accent.linear_multiply(if dark { 0.28 } else { 0.14 });
    // Cantos: num grupo, o lado direito fica "colado" entre os balões.
    let (ne, se) = match pos {
        GroupPos::Single => (12, 3),
        GroupPos::First => (12, 4),
        GroupPos::Middle => (4, 4),
        GroupPos::Last => (4, 3),
    };
    let last = matches!(pos, GroupPos::Single | GroupPos::Last);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, pal.accent.linear_multiply(0.45)))
            .corner_radius(egui::CornerRadius {
                nw: 12,
                ne,
                sw: 12,
                se,
            })
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.72);
                ui.vertical(|ui| {
                    if !shown.trim().is_empty() {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&shown).color(pal.text))
                                .wrap()
                                .selectable(true),
                        );
                    }
                    for a in &atts {
                        attachment_view(ui, pal, toasts, a, local);
                    }
                    ui.add_space(2.0);
                    // rodapé: uma linha, discreto (copiar + tempo relativo).
                    // Num grupo, só o último balão mostra a hora; os outros
                    // mostram a hora exata no hover do "copiar".
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let hover = match started {
                            Some(dt) if !last => {
                                format!("{} · {}", tr("copiar a mensagem enviada"), fmt_exact(dt))
                            }
                            _ => tr("copiar a mensagem enviada").to_string(),
                        };
                        if copy_button(ui, pal, &hover) {
                            ui.ctx().copy_text(raw.to_string());
                            toasts.success(tr("Mensagem copiada."));
                        }
                        if last {
                            when_label(ui, pal, started);
                        }
                    });
                });
            });
    });
    ui.add_space(if last { 6.0 } else { 2.0 });
}

/// Tamanho legível de um arquivo.
fn fmt_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes == 0 {
        String::new()
    } else if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{:.1} MB", b / KB / KB)
    }
}

/// Anexo dentro do balão do usuário: miniatura clicável para imagens, chip com
/// nome e tamanho para os demais. O clique abre o arquivo no app padrão.
/// `local` = o servidor é esta máquina: só então o caminho existe aqui e dá
/// para ler a miniatura ou abrir o arquivo (`docs/26`, etapa 2 traz a API).
fn attachment_view(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    toasts: &mut ui::Toasts,
    a: &will_core::Attachment,
    local: bool,
) {
    let path = std::path::Path::new(&a.path);
    // Remoto: não dá para conferir o disco do servidor; o chip fica clicável
    // e o clique explica a limitação.
    let exists = !local || path.is_file();
    let size = fmt_size(a.size);
    let mut caption = a.name.clone();
    if !size.is_empty() {
        caption.push_str(" · ");
        caption.push_str(&size);
    }
    if !exists {
        caption.push_str(" · ");
        caption.push_str(tr("arquivo não encontrado"));
    }
    ui.add_space(4.0);
    const THUMB: egui::Vec2 = egui::vec2(320.0, 240.0);
    // Miniatura só quando a imagem já decodificou: o tamanho vem do arquivo
    // (não do espaço disponível, que varia dentro do balão) e é limitado a THUMB.
    let thumb = (local && a.is_image() && exists)
        .then(|| {
            let img = egui::Image::new(format!("file://{}", a.path));
            match img.load_for_size(ui.ctx(), THUMB) {
                Ok(egui::load::TexturePoll::Ready { .. }) => Some(Some(img)),
                Ok(egui::load::TexturePoll::Pending { .. }) => Some(None),
                Err(_) => None,
            }
        })
        .flatten();
    let clicked = if let Some(img) = thumb {
        let r = match img {
            Some(img) => ui
                .add(
                    img.fit_to_original_size(1.0)
                        .max_size(THUMB)
                        .corner_radius(8.0)
                        .sense(egui::Sense::click()),
                )
                .on_hover_text(format!("{caption}\n{}", tr("clique para abrir"))),
            None => {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
                ui.add(egui::Spinner::new().size(18.0))
            }
        };
        ui.label(
            egui::RichText::new(&caption)
                .size(11.0)
                .color(pal.text_weak),
        );
        r.clicked()
    } else {
        let label = format!("{} {caption}", tr("anexo:"));
        ui.add_enabled(
            exists,
            egui::Button::new(egui::RichText::new(label).size(12.0).color(pal.text))
                .fill(pal.card.linear_multiply(0.6))
                .stroke(egui::Stroke::new(1.0, pal.border))
                .corner_radius(8.0),
        )
        .on_hover_text(&a.path)
        .clicked()
    };
    if clicked {
        if !local {
            toasts.info(tr("disponível só quando o servidor é esta máquina"));
        } else if let Err(e) = will_platform::open_path(path) {
            toasts.error(trf("não consegui abrir o arquivo: {}", &[&e]));
        }
    }
}

fn assistant_card(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    cache: &mut egui_commonmark::CommonMarkCache,
    toasts: &mut ui::Toasts,
    text: &str,
    turn: &Value,
) {
    let finished = turn_dt(turn, "finished_at").or_else(|| turn_dt(turn, "started_at"));
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(pal.card)
            .stroke(egui::Stroke::new(1.0, pal.border))
            .corner_radius(egui::CornerRadius {
                nw: 3,
                ne: 12,
                sw: 12,
                se: 12,
            })
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.88);
                ui.set_width(ui.available_width() * 0.88);
                ui.vertical(|ui| {
                    md(ui, cache, text);
                    ui.add_space(4.0);
                    // rodapé: tempo relativo à esquerda; "i" + copiar à direita
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        when_label(ui, pal, finished.as_ref());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let info =
                                info_icon(ui, pal).on_hover_cursor(egui::CursorIcon::PointingHand);
                            // clique abre um popover fixo; hover mostra o mesmo conteúdo
                            let opened = egui::containers::Popup::menu(&info)
                                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                .show(|ui| turn_info_body(ui, pal, turn));
                            if opened.is_none() {
                                info.on_hover_ui(|ui| turn_info_body(ui, pal, turn));
                            }
                            if copy_button(ui, pal, tr("copiar a resposta (Markdown)")) {
                                ui.ctx().copy_text(text.to_string());
                                toasts.success(tr("Resposta copiada."));
                            }
                        });
                    });
                });
            });
    });
    ui.add_space(8.0);
}

fn error_card(ui: &mut egui::Ui, _pal: &ui::Palette, msg: &str) {
    let red = Tone::Danger.color(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(red.linear_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, red.linear_multiply(0.6)))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width() * 0.88);
            ui.horizontal_top(|ui| {
                ui.label(
                    egui::RichText::new(tr("Erro"))
                        .font(ui::medium(13.0))
                        .color(red),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(msg).color(red))
                        .wrap()
                        .selectable(true),
                );
            });
        });
    ui.add_space(8.0);
}

/// Cartão do turno em andamento: cabeçalho animado + linha do tempo dos eventos
/// intermediários (ferramentas, raciocínio, status) + texto parcial em Markdown.
fn live_card(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    cache: &mut egui_commonmark::CommonMarkCache,
    items: &[EngineDelta],
    text: &str,
    started: Option<std::time::Instant>,
) {
    let t = ui.input(|i| i.time);
    let phase = ((t * 2.0) as usize) % 4;
    let dots = ".".repeat(phase);
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .fill(pal.card)
        .stroke(egui::Stroke::new(1.0, pal.accent.linear_multiply(0.55)))
        .corner_radius(egui::CornerRadius {
            nw: 3,
            ne: 12,
            sw: 12,
            se: 12,
        })
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width() * 0.88);
            ui.horizontal(|ui| {
                // pulso
                let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                let pulse = 0.5 + 0.5 * (t * 4.0).sin() as f32;
                ui.painter().circle_filled(
                    r.center(),
                    3.0 + 2.5 * pulse,
                    pal.accent.linear_multiply(0.35),
                );
                ui.painter().circle_filled(r.center(), 3.0, pal.accent);
                ui.label(
                    egui::RichText::new(trf("Claude está trabalhando{}", &[&dots]))
                        .font(ui::medium(13.0))
                        .color(pal.text),
                );
                if let Some(s) = started {
                    ui.label(
                        egui::RichText::new(format!("{} s", s.elapsed().as_secs()))
                            .small()
                            .color(pal.text_weak),
                    );
                }
            });
            if !items.is_empty() {
                ui.add_space(4.0);
            }
            // linha do tempo
            let line_x = 6.0;
            for d in items {
                match d {
                    EngineDelta::ToolUse {
                        name,
                        input_preview,
                    } => {
                        ui.horizontal_top(|ui| {
                            timeline_dot(ui, line_x, Tone::Info.color(dark));
                            ui.vertical(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui::badge(ui, name, Tone::Info);
                                    let prev: String = input_preview.chars().take(140).collect();
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(prev)
                                                .monospace()
                                                .size(12.0)
                                                .color(pal.text_weak),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(input_preview);
                                });
                            });
                        });
                    }
                    EngineDelta::ToolResult { preview, is_error } => {
                        let col = if *is_error {
                            Tone::Danger.color(dark)
                        } else {
                            Tone::Success.color(dark)
                        };
                        ui.horizontal_top(|ui| {
                            timeline_dot(ui, line_x, col);
                            let prev: String = preview.chars().take(160).collect();
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "{} {prev}",
                                        if *is_error { tr("falhou:") } else { tr("ok:") }
                                    ))
                                    .monospace()
                                    .size(12.0)
                                    .color(pal.text_weak),
                                )
                                .truncate(),
                            )
                            .on_hover_text(preview);
                        });
                    }
                    EngineDelta::Status { text } => {
                        ui.horizontal_top(|ui| {
                            timeline_dot(ui, line_x, pal.text_weak);
                            ui.label(egui::RichText::new(text).small().color(pal.text_weak));
                        });
                    }
                    EngineDelta::Thinking { text } => {
                        ui.horizontal_top(|ui| {
                            timeline_dot(ui, line_x, Tone::Warning.color(dark));
                            egui::CollapsingHeader::new(
                                egui::RichText::new(tr("raciocínio"))
                                    .small()
                                    .color(pal.text_weak),
                            )
                            .id_salt(("think", text.len(), text.as_ptr() as usize))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(text)
                                            .italics()
                                            .size(12.5)
                                            .color(pal.text_weak),
                                    )
                                    .wrap(),
                                );
                            });
                        });
                    }
                    EngineDelta::Narration { text } => {
                        ui.add_space(2.0);
                        md(ui, cache, text);
                    }
                    EngineDelta::TextDelta { .. } => {}
                }
            }
            if !text.is_empty() {
                ui.add_space(4.0);
                let caret = if phase.is_multiple_of(2) { "▍" } else { " " };
                md(ui, cache, &format!("{text}{caret}"));
            }
        });
    ui.add_space(8.0);
}

fn timeline_dot(ui: &mut egui::Ui, x_pad: f32, color: egui::Color32) {
    ui.add_space(x_pad);
    let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(egui::pos2(r.center().x, r.top() + 9.0), 3.5, color);
    ui.add_space(4.0);
}

#[allow(dead_code)]
fn _assert_types(_: TurnResult) {}
