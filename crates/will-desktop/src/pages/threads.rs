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
    md_cache: egui_commonmark::CommonMarkCache,
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
                ui.set_width(228.0);
                ui.set_min_height(avail_h);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Threads").font(ui::semibold(16.0)));
                    if ui.add_enabled(self.create_task.is_none(), egui::Button::new("+ Nova").small()).clicked() {
                        self.create_task = Some(cx.svc.call(|s| async move { j(api::create(State(s), None).await) }));
                    }
                    if ui.add(egui::Button::new("↺").small()).on_hover_text("Atualizar").clicked() {
                        self.list.loaded_at = None;
                    }
                });
                ui.checkbox(&mut self.show_archived, egui::RichText::new("arquivadas").small());
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
                        let r = egui::Frame::new().fill(fill).corner_radius(6.0).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 2.0);
                            ui.horizontal(|ui| {
                                ui::dot(ui, status_tone(t));
                                ui.add(egui::Label::new(egui::RichText::new(title_of(t)).font(ui::medium(13.0))).truncate());
                            });
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                let mut meta = format!("{} · {}", origin_label(t), ui::fmt_relative(&t.last_activity_at));
                                if t.pending > 0 {
                                    meta.push_str(&format!(" · {} pendente(s)", t.pending));
                                } else if t.alive {
                                    meta.push_str(" · vivo");
                                }
                                if t.session_expired {
                                    meta.push_str(" · sessão expirada");
                                }
                                ui.add(egui::Label::new(egui::RichText::new(meta).size(11.5).weak()).truncate());
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
        let input_h = 78.0;
        let hist_h = (avail_h - input_h - 96.0).max(120.0);
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
        egui::ScrollArea::vertical()
            .id_salt(("chat", t.id))
            .auto_shrink([false, false])
            .max_height(hist_h)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(6.0);
                if loading {
                    ui::busy(ui, "carregando…");
                }
                if let Some(e) = &turns_err {
                    ui::error_text(ui, e);
                }
                for turn in &turns {
                    let prompt = ui::s(turn, "prompt");
                    let when = turn.get("started_at").map(ui::fmt_dt).unwrap_or_default();
                    if !prompt.is_empty() {
                        user_bubble(ui, pal, &prompt, &when);
                    }
                    let text = ui::s(turn, "text");
                    let is_err = ui::b(turn, "is_error");
                    let res = turn
                        .get("result")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    if is_err {
                        let msg = if text.is_empty() {
                            ui::s(turn, "error_kind")
                        } else {
                            text.clone()
                        };
                        error_card(ui, pal, &msg);
                    } else if !text.is_empty() {
                        assistant_card(
                            ui,
                            pal,
                            &mut self.md_cache,
                            &text,
                            &res,
                            turn.get("finished_at").map(ui::fmt_dt).unwrap_or_default(),
                        );
                    } else if turn.get("finished_at").map(|f| f.is_null()).unwrap_or(true)
                        && !has_live
                    {
                        ui.label(egui::RichText::new("aguardando resposta…").weak().small());
                    }
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
                    live_card(ui, pal, &mut self.md_cache, items, text, *started);
                }
                ui.add_space(8.0);
            });

        // entrada: caixa de texto com o botão Enviar na mesma linha
        ui.add_space(6.0);
        let mut send_now = false;
        let busy = self.send_task.is_some();
        let pal2 = ui::palette(ui.visuals().dark_mode);
        egui::Frame::new()
            .fill(pal2.card)
            .stroke(egui::Stroke::new(1.0, pal2.border))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let btn_w = 92.0;
                    let text_w = (ui.available_width() - btn_w - 12.0).max(120.0);
                    let r = ui.add_sized(
                        [text_w, 54.0],
                        egui::TextEdit::multiline(&mut self.input)
                            .frame(egui::Frame::NONE)
                            .desired_rows(2)
                            .hint_text(if t.pending > 0 {
                                "Turno em andamento: a mensagem entra no meio. Ctrl+Enter envia."
                            } else {
                                "Mensagem para o Claude. Ctrl+Enter envia."
                            }),
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
                        let can = !busy && !self.input.trim().is_empty();
                        let b = egui::Button::new(
                            egui::RichText::new(if busy { "…" } else { "Enviar" })
                                .color(egui::Color32::WHITE),
                        )
                        .fill(if can {
                            pal2.accent
                        } else {
                            pal2.accent.linear_multiply(0.45)
                        })
                        .min_size(egui::vec2(btn_w - 8.0, 34.0));
                        if ui.add_enabled(can, b).clicked() {
                            send_now = true;
                        }
                    });
                });
            });
        if send_now {
            self.send(cx);
        }
    }
}

/// Renderiza Markdown (a IA responde em Markdown).
fn md(ui: &mut egui::Ui, cache: &mut egui_commonmark::CommonMarkCache, text: &str) {
    let text = ui::clean_emoji(text);
    egui_commonmark::CommonMarkViewer::new()
        .max_image_width(Some(640))
        .show(ui, cache, &text);
}

fn user_bubble(ui: &mut egui::Ui, pal: &ui::Palette, text: &str, when: &str) {
    let text = &ui::clean_emoji(text);
    let dark = ui.visuals().dark_mode;
    let fill = pal.accent.linear_multiply(if dark { 0.28 } else { 0.14 });
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, pal.accent.linear_multiply(0.45)))
            .corner_radius(egui::CornerRadius {
                nw: 12,
                ne: 12,
                sw: 12,
                se: 3,
            })
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.72);
                ui.vertical(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).color(pal.text))
                            .wrap()
                            .selectable(true),
                    );
                    if !when.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("você · {when}"))
                                .small()
                                .color(pal.text_weak),
                        );
                    }
                });
            });
    });
    ui.add_space(6.0);
}

fn assistant_card(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    cache: &mut egui_commonmark::CommonMarkCache,
    text: &str,
    res: &serde_json::Value,
    when: String,
) {
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
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new("Claude")
                                .font(ui::medium(12.0))
                                .color(pal.text_weak),
                        );
                        if !when.is_empty() {
                            ui.label(egui::RichText::new(when).small().color(pal.text_weak));
                        }
                        if let Some(m) = res.get("model").and_then(|c| c.as_str()) {
                            ui::badge(ui, m, Tone::Neutral);
                        }
                        if let Some(d) = res.get("duration_ms").and_then(|c| c.as_u64()) {
                            ui::badge(ui, &ui::fmt_ms(d), Tone::Neutral);
                        }
                        if let Some(c) = res.get("cost_usd").and_then(|c| c.as_f64()) {
                            ui::badge(ui, &ui::fmt_usd(c), Tone::Neutral);
                        }
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new("copiar").small())
                                    .frame(false),
                            )
                            .clicked()
                        {
                            ui.ctx().copy_text(text.to_string());
                        }
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
                    egui::RichText::new("Erro")
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
                    egui::RichText::new(format!("Claude está trabalhando{dots}"))
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
                                        if *is_error { "falhou:" } else { "ok:" }
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
                                egui::RichText::new("raciocínio")
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
