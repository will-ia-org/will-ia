//! Threads: lista à esquerda, conversa à direita (histórico + turno ao vivo),
//! envio imediato (inclusive no meio de um turno), reiniciar/reset/interromper,
//! renomear e apagar.

use super::Cx;
use crate::service::{j, Loader, Task};
use crate::ui::{self, Tone};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::Value;
use std::collections::HashMap;
use will_core::{EngineDelta, Event, ThreadId, ThreadStatus, ThreadSummary, TurnResult};
use will_server::api::threads as api;

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
    send_task: Option<Task<Value>>,
    action_task: Option<(String, Task<Value>)>,
    create_task: Option<Task<Value>>,
    rename_buf: Option<String>,
    confirm_delete: Option<(ThreadId, bool)>,
    confirm_reset: Option<ThreadId>,
    show_archived: bool,
    focus_input: bool,
}

fn origin_label(t: &ThreadSummary) -> String {
    match &t.origin {
        will_core::Origin::Ui => "app".into(),
        will_core::Origin::Telegram { chat_id, topic_id } => match topic_id {
            Some(tp) => format!("telegram {chat_id}:{tp}"),
            None => format!("telegram {chat_id}"),
        },
        will_core::Origin::Realtime => "voz".into(),
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

impl ThreadsPage {
    fn reload_list(&mut self, cx: &Cx) {
        self.list
            .start(cx.svc.call(|s| async move { j(api::list(State(s)).await) }));
    }

    fn reload_turns(&mut self, cx: &Cx, id: ThreadId) {
        self.turns_for = Some(id);
        let sid = id.to_string();
        self.turns.start(cx.svc.call(move |s| async move {
            j(api::turns(State(s), Path(sid), Query(api::Limit { limit: Some(200) })).await)
        }));
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
                        .push("mensagem entregue no meio do turno (steering)".into());
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
                    "sessão expirada: a thread recomeçou do zero e reenviou as pendências".into(),
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
                            cx.toasts.info("Entregue no meio do turno.");
                        }
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }
        if let Some((what, t)) = &self.action_task {
            if let Some(r) = t.take() {
                let what = what.clone();
                self.action_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts.success(format!("{what}: ok"));
                        self.list.loaded_at = None;
                        self.turns.loaded_at = None;
                    }
                    Err(e) => cx.toasts.error(format!("{what}: {e}")),
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
        if text.is_empty() || self.send_task.is_some() {
            return;
        }
        self.input.clear();
        let sid = id.to_string();
        self.send_task = Some(cx.svc.call(move |s| async move {
            j(api::send(State(s), Path(sid), Json(api::SendBody { text })).await)
        }));
        self.focus_input = true;
    }

    fn action(&mut self, cx: &Cx, id: ThreadId, what: &'static str) {
        let sid = id.to_string();
        let t: Task<Value> = match what {
            "Reiniciar" => cx
                .svc
                .call(move |s| async move { j(api::restart(State(s), Path(sid)).await) }),
            "Reset" => cx
                .svc
                .call(move |s| async move { j(api::reset(State(s), Path(sid)).await) }),
            "Interromper" => cx
                .svc
                .call(move |s| async move { j(api::interrupt(State(s), Path(sid)).await) }),
            _ => return,
        };
        self.action_task = Some((what.to_string(), t));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);
        let avail_h = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.set_min_height(avail_h);
            // ----- lista -----
            ui.vertical(|ui| {
                ui.set_width(300.0);
                ui.set_min_height(avail_h);
                ui.horizontal(|ui| {
                    ui.heading("Threads");
                    if ui.add_enabled(self.create_task.is_none(), egui::Button::new("+ Nova")).clicked() {
                        self.create_task = Some(cx.svc.call(|s| async move { j(api::create(State(s), None).await) }));
                    }
                    if ui.button("Atualizar").clicked() {
                        self.list.loaded_at = None;
                    }
                });
                ui.checkbox(&mut self.show_archived, "mostrar arquivadas");
                if let Some(e) = &self.list.error {
                    ui::error_text(ui, e);
                }
                egui::ScrollArea::vertical().id_salt("threads-list").auto_shrink([false, false]).max_height(avail_h - 70.0).show(ui, |ui| {
                    let mut sorted: Vec<&ThreadSummary> = self.threads.iter().filter(|t| self.show_archived || t.status != ThreadStatus::Archived).collect();
                    sorted.sort_by_key(|t| std::cmp::Reverse(t.last_activity_at));
                    if sorted.is_empty() {
                        ui.label(egui::RichText::new("Nenhuma thread. Crie uma com “+ Nova” ou mande mensagem pelo Telegram/voz.").weak());
                    }
                    let mut click = None;
                    for t in sorted {
                        let selected = self.selected == Some(t.id);
                        let fill = if selected { ui.visuals().selection.bg_fill.linear_multiply(0.35) } else { egui::Color32::TRANSPARENT };
                        let r = egui::Frame::new().fill(fill).corner_radius(6.0).inner_margin(6.0).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui::dot(ui, status_tone(t));
                                ui.add(egui::Label::new(egui::RichText::new(title_of(t)).strong()).truncate());
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(origin_label(t)).small().weak());
                                ui.label(egui::RichText::new(ui::fmt_relative(&t.last_activity_at)).small().weak());
                                if t.pending > 0 {
                                    ui::badge(ui, &format!("{} pendente(s)", t.pending), Tone::Info);
                                }
                                if t.alive {
                                    ui::badge(ui, "vivo", Tone::Success);
                                }
                                if t.session_expired {
                                    ui::badge(ui, "sessão expirada", Tone::Warning);
                                }
                            });
                        });
                        if r.response.interact(egui::Sense::click()).clicked() {
                            click = Some(t.id);
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
                            ui.label(egui::RichText::new("Selecione uma thread ou crie uma nova.").weak().size(16.0));
                        });
                    }
                    Some(t) => self.chat_ui(ui, cx, &t, avail_h),
                }
            });
        });

        if let Some((id, purge)) = self.confirm_delete {
            let c = ui::Confirm {
                title: "Apagar thread".into(),
                body: if purge {
                    "Apagar a thread e a transcrição salva do Claude? Isso não tem volta.".into()
                } else {
                    "Apagar a thread do will-ia? A transcrição do Claude Code fica no disco.".into()
                },
                confirm_label: "Apagar".into(),
                danger: true,
                tag: "del-thread".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_delete = None;
                if ok {
                    let sid = id.to_string();
                    self.action_task = Some((
                        "Apagar".into(),
                        cx.svc.call(move |s| async move {
                            j(api::delete_one(
                                State(s),
                                Path(sid),
                                Query(api::DeleteQuery { purge: Some(purge) }),
                            )
                            .await)
                        }),
                    ));
                    self.selected = None;
                }
            }
        }
        if let Some(id) = self.confirm_reset {
            let c = ui::Confirm { title: "Reset da thread".into(), body: "Mata o processo e esquece a sessão: a próxima mensagem começa uma conversa nova com o Claude.".into(), confirm_label: "Reset".into(), danger: true, tag: "reset-thread".into() };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_reset = None;
                if ok {
                    self.action(cx, id, "Reset");
                }
            }
        }
    }

    fn chat_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, t: &ThreadSummary, avail_h: f32) {
        // cabeçalho
        ui.horizontal(|ui| {
            match &mut self.rename_buf {
                Some(buf) => {
                    let r = ui.add(egui::TextEdit::singleline(buf).desired_width(260.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("OK").clicked() || enter {
                        let title = buf.clone();
                        let sid = t.id.to_string();
                        self.action_task =
                            Some((
                                "Renomear".into(),
                                cx.svc.call(move |s| async move {
                                    j(api::rename(
                                        State(s),
                                        Path(sid),
                                        Json(api::RenameBody { title }),
                                    )
                                    .await)
                                }),
                            ));
                        self.rename_buf = None;
                    } else if ui.button("Cancelar").clicked() {
                        self.rename_buf = None;
                    }
                }
                None => {
                    ui.heading(title_of(t));
                    if ui.button("renomear").clicked() {
                        self.rename_buf = Some(title_of(t));
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("apagar").clicked() {
                    self.confirm_delete = Some((t.id, false));
                }
                if ui
                    .button("reset")
                    .on_hover_text("mata o processo e esquece a sessão")
                    .clicked()
                {
                    self.confirm_reset = Some(t.id);
                }
                if ui
                    .button("reiniciar")
                    .on_hover_text("mata o processo; volta com --resume")
                    .clicked()
                {
                    self.action(cx, t.id, "Reiniciar");
                }
                if ui
                    .add_enabled(t.pending > 0, egui::Button::new("interromper").small())
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
                    egui::RichText::new(format!("sessão {}", &sid[..sid.len().min(8)]))
                        .small()
                        .weak(),
                );
            }
            if t.session_expired {
                ui::badge(ui, "sessão expirada no disco; vai recomeçar", Tone::Warning);
            }
        });
        ui.separator();

        // histórico + ao vivo
        let input_h = 110.0;
        let hist_h = (avail_h - input_h - 90.0).max(120.0);
        egui::ScrollArea::vertical()
            .id_salt(("chat", t.id))
            .auto_shrink([false, false])
            .max_height(hist_h)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if self.turns.loading() && self.turns.data.is_none() {
                    ui::busy(ui, "carregando…");
                }
                if let Some(e) = &self.turns.error {
                    ui::error_text(ui, e);
                }
                let turns = self
                    .turns
                    .data
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for turn in &turns {
                    let prompt = ui::s(turn, "prompt");
                    if !prompt.is_empty() {
                        bubble(ui, true, &prompt, None);
                    }
                    let text = ui::s(turn, "text");
                    let is_err = ui::b(turn, "is_error");
                    let meta = turn.get("result").map(|r| {
                        let mut parts = vec![];
                        if let Some(c) = r.get("cost_usd").and_then(|c| c.as_f64()) {
                            parts.push(ui::fmt_usd(c));
                        }
                        if let Some(d) = r.get("duration_ms").and_then(|c| c.as_u64()) {
                            parts.push(ui::fmt_ms(d));
                        }
                        if let Some(m) = r.get("model").and_then(|c| c.as_str()) {
                            parts.push(m.to_string());
                        }
                        parts.join(" · ")
                    });
                    if !text.is_empty() || is_err {
                        if is_err {
                            ui.colored_label(
                                Tone::Danger.color(ui.visuals().dark_mode),
                                format!(
                                    "erro: {}",
                                    if text.is_empty() {
                                        ui::s(turn, "error_kind")
                                    } else {
                                        text.clone()
                                    }
                                ),
                            );
                        } else {
                            bubble(ui, false, &text, meta.as_deref());
                        }
                    } else if turn.get("finished_at").map(|f| f.is_null()).unwrap_or(true)
                        && !self.live.contains_key(&t.id)
                    {
                        ui.label(egui::RichText::new("(aguardando resposta…)").weak().small());
                    }
                }
                if let Some(notes) = self.notes.get(&t.id) {
                    for n in notes {
                        ui.label(egui::RichText::new(format!("i {n}")).small().weak());
                    }
                }
                if let Some(l) = self.live.get(&t.id) {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(250));
                    egui::Frame::group(ui.style())
                        .corner_radius(8.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(egui::RichText::new("processando…").weak());
                                if let Some(s) = l.started {
                                    ui.label(
                                        egui::RichText::new(format!("{} s", s.elapsed().as_secs()))
                                            .weak()
                                            .small(),
                                    );
                                }
                            });
                            for d in &l.items {
                                match d {
                                    EngineDelta::Narration { text } => ui::wrapped_text(ui, text),
                                    EngineDelta::ToolUse {
                                        name,
                                        input_preview,
                                    } => {
                                        ui.horizontal_wrapped(|ui| {
                                            ui::badge(ui, name, Tone::Info);
                                            ui.label(
                                                egui::RichText::new(input_preview)
                                                    .small()
                                                    .monospace()
                                                    .weak(),
                                            );
                                        });
                                    }
                                    EngineDelta::ToolResult { preview, is_error } => {
                                        ui.label(
                                            egui::RichText::new(format!("└ {preview}"))
                                                .small()
                                                .monospace()
                                                .color(if *is_error {
                                                    Tone::Danger.color(true)
                                                } else {
                                                    Tone::Neutral.color(true)
                                                }),
                                        );
                                    }
                                    EngineDelta::Status { text } => {
                                        ui.label(egui::RichText::new(text).small().weak());
                                    }
                                    EngineDelta::Thinking { text } => {
                                        ui.label(
                                            egui::RichText::new(format!("pensando: {text}"))
                                                .small()
                                                .italics()
                                                .weak(),
                                        );
                                    }
                                    EngineDelta::TextDelta { .. } => {}
                                }
                            }
                            if !l.text.is_empty() {
                                ui::wrapped_text(ui, &l.text);
                            }
                        });
                }
            });

        // entrada
        ui.separator();
        let mut send_now = false;
        let r = ui.add(egui::TextEdit::multiline(&mut self.input).desired_rows(3).desired_width(f32::INFINITY).hint_text("Mensagem para o Claude (Ctrl+Enter envia). Pode mandar mesmo com um turno em andamento."));
        if self.focus_input {
            r.request_focus();
            self.focus_input = false;
        }
        if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command) {
            send_now = true;
        }
        ui.horizontal(|ui| {
            let busy = self.send_task.is_some();
            if ui
                .add_enabled(
                    !busy && !self.input.trim().is_empty(),
                    egui::Button::new(egui::RichText::new("Enviar").color(egui::Color32::WHITE))
                        .fill(Tone::Info.color(true)),
                )
                .clicked()
            {
                send_now = true;
            }
            if busy {
                ui.spinner();
            }
            if t.pending > 0 {
                ui.label(
                    egui::RichText::new("turno em andamento: a mensagem entra no meio (steering)")
                        .small()
                        .weak(),
                );
            }
        });
        if send_now {
            self.send(cx);
        }
    }
}

fn bubble(ui: &mut egui::Ui, user: bool, text: &str, meta: Option<&str>) {
    let dark = ui.visuals().dark_mode;
    let fill = if user {
        if dark {
            egui::Color32::from_rgb(40, 60, 100)
        } else {
            egui::Color32::from_rgb(220, 232, 255)
        }
    } else if dark {
        egui::Color32::from_rgb(38, 42, 52)
    } else {
        egui::Color32::from_rgb(244, 245, 248)
    };
    let layout = if user {
        egui::Layout::right_to_left(egui::Align::Min)
    } else {
        egui::Layout::left_to_right(egui::Align::Min)
    };
    ui.with_layout(layout, |ui| {
        egui::Frame::new()
            .fill(fill)
            .corner_radius(10.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.85);
                ui.vertical(|ui| {
                    ui::wrapped_text(ui, text);
                    if let Some(m) = meta {
                        ui.label(egui::RichText::new(m).small().weak());
                    }
                });
            });
    });
    ui.add_space(4.0);
}

#[allow(dead_code)]
fn _assert_types(_: TurnResult) {}
