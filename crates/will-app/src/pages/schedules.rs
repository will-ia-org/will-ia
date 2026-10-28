//! Agendamentos: lista em cartões (um por agendamento, com próximo disparo,
//! thread dona, último resultado e ações) e formulário de criação/edição com
//! prévia da expressão cron (descrição + próximas ocorrências) ao digitar.

use super::{Cx, Nav, SettingsSection};
use crate::help;
use crate::i18n::{tr, trf};
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use egui::RichText;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use will_core::{Event, ThreadSummary};

/// Acima disso a mensagem do cartão é cortada, com "mais…" para expandir.
const MESSAGE_PREVIEW_CHARS: usize = 300;
/// Espera depois da última tecla antes de pedir a prévia do cron ao serviço.
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(600);
/// Quantas ocorrências a prévia mostra.
const PREVIEW_COUNT: usize = 3;

#[derive(Default)]
pub struct SchedulesPage {
    list: Loader<Value>,
    /// Lista de threads para o seletor de thread dona (e rótulos).
    threads: Loader<Value>,
    thread_list: Vec<ThreadSummary>,
    form: Option<Form>,
    /// Ações em voo: (texto do toast de sucesso, tarefa).
    actions: Vec<(String, Task<Value>)>,
    /// (id, nome) do agendamento aguardando confirmação de exclusão.
    confirm_delete: Option<(String, String)>,
    /// Cartões com a mensagem expandida.
    expanded: HashSet<String>,
}

/// Formulário de criação (`editing == None`) ou edição (`Some(id)`).
#[derive(Default)]
struct Form {
    editing: Option<String>,
    name: String,
    message: String,
    /// `"cron"` (recorrente) ou `"once"` (único).
    kind: String,
    cron: String,
    run_at: String,
    timezone: String,
    thread_id: String,
    preview: Loader<Value>,
    /// Texto (cron + fuso) da última prévia pedida ao serviço.
    preview_key: String,
    /// Texto atual (cron + fuso) e instante da última mudança (debounce).
    input_key: String,
    changed_at: Option<Instant>,
    save: Option<Task<Value>>,
}

impl Form {
    fn new(default_thread: Option<String>) -> Self {
        Self {
            kind: "cron".into(),
            thread_id: default_thread.unwrap_or_default(),
            ..Default::default()
        }
    }

    /// Preenche a partir de um `ScheduleView` (JSON achatado) para edição.
    fn from_view(v: &Value) -> Self {
        let mut f = Self::new(Some(ui::s(v, "thread_id")));
        f.editing = Some(ui::s(v, "id"));
        f.name = ui::s(v, "name");
        f.message = ui::s(v, "message");
        f.timezone = ui::s(v, "timezone");
        let trigger = &v["trigger"];
        if ui::s(trigger, "kind") == "cron" {
            f.kind = "cron".into();
            f.cron = ui::s(trigger, "expr");
        } else {
            f.kind = "once".into();
            // ISO-8601 com o fuso local da máquina: inequívoco e aceito pelo serviço.
            f.run_at = trigger
                .get("at")
                .and_then(|a| a.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%Y-%m-%dT%H:%M:%S%:z")
                        .to_string()
                })
                .unwrap_or_default();
        }
        f
    }

    fn is_cron(&self) -> bool {
        self.kind == "cron"
    }

    fn saving(&self) -> bool {
        self.save.as_ref().is_some_and(|t| t.pending())
    }

    /// Valida o formulário e monta o corpo da chamada (criação ou edição):
    /// JSON com os campos de `ScheduleInput` (`docs/22`), sem os ausentes.
    fn input(&self) -> Result<Value, String> {
        if self.name.trim().is_empty() {
            return Err(tr("Dê um nome ao agendamento.").into());
        }
        if self.message.trim().is_empty() {
            return Err(tr("Escreva a mensagem que o agente vai receber.").into());
        }
        if self.thread_id.trim().is_empty() {
            return Err(tr("Escolha a thread dona.").into());
        }
        let (cron, run_at) = if self.is_cron() {
            if self.cron.trim().is_empty() {
                return Err(tr("Informe a expressão cron.").into());
            }
            (Some(self.cron.trim().to_string()), None)
        } else {
            if self.run_at.trim().is_empty() {
                return Err(tr("Informe a data e hora do disparo.").into());
            }
            (None, Some(self.run_at.trim().to_string()))
        };
        let mut body = json!({
            "name": self.name.trim(),
            "message": self.message.trim(),
            // `""` limpa o fuso na edição (volta ao padrão).
            "timezone": self.timezone.trim(),
            "thread_id": self.thread_id.trim(),
        });
        if let Some(c) = cron {
            body["cron"] = Value::String(c);
        }
        if let Some(at) = run_at {
            body["run_at"] = Value::String(at);
        }
        Ok(body)
    }

    /// Debounce da prévia do cron: pede ao serviço ~600 ms depois da última
    /// mudança no texto (cron ou fuso). Sem cron, limpa a prévia.
    fn tick_preview(&mut self, ctx: &egui::Context, cx: &Cx) {
        self.preview.poll();
        if !self.is_cron() {
            return;
        }
        let key = format!("{}\n{}", self.cron.trim(), self.timezone.trim());
        if key != self.input_key {
            self.input_key = key.clone();
            self.changed_at = Some(Instant::now());
        }
        let Some(changed) = self.changed_at else {
            return;
        };
        let elapsed = changed.elapsed();
        if elapsed < PREVIEW_DEBOUNCE {
            // Acorda o quadro quando o prazo vencer, mesmo sem o usuário mexer.
            ctx.request_repaint_after(PREVIEW_DEBOUNCE - elapsed);
            return;
        }
        if self.cron.trim().is_empty() {
            self.preview = Loader::default();
            self.preview_key = key;
            self.changed_at = None;
        } else if key == self.preview_key {
            self.changed_at = None;
        } else if !self.preview.busy() {
            self.preview_key = key;
            self.changed_at = None;
            let cron = self.cron.trim().to_string();
            let timezone = Some(self.timezone.trim().to_string()).filter(|t| !t.is_empty());
            self.preview.start(cx.svc.call(move |c| async move {
                c.schedule_preview(&cron, timezone.as_deref(), PREVIEW_COUNT)
                    .await
            }));
        }
        // Ocupado com uma prévia anterior: tenta de novo no próximo quadro.
    }
}

impl SchedulesPage {
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::SchedulesChanged | Event::ScheduleFired { .. } => self.list.loaded_at = None,
            // Thread nova/apagada muda o seletor e o "thread removida" dos cartões.
            Event::ThreadCreated { .. } | Event::ThreadDeleted { .. } => {
                self.threads.loaded_at = None;
                self.list.loaded_at = None;
            }
            _ => {}
        }
    }

    fn reload(&mut self, cx: &Cx) {
        self.list
            .start(cx.svc.call(|c| async move { c.schedules(None).await }));
    }

    fn push_action(&mut self, done_text: String, task: Task<Value>) {
        self.actions.push((done_text, task));
    }

    fn update_enabled(&mut self, cx: &Cx, id: String, enabled: bool) {
        let task = cx.svc.call(move |c| async move {
            c.update_schedule(&id, json!({ "enabled": enabled })).await
        });
        let text = if enabled {
            tr("Agendamento retomado.")
        } else {
            tr("Agendamento pausado.")
        };
        self.push_action(text.into(), task);
    }

    fn run_now(&mut self, cx: &Cx, id: String, name: &str) {
        let task = cx
            .svc
            .call(move |c| async move { c.run_schedule(&id).await });
        self.push_action(trf("\"{}\" disparado agora.", &[&name]), task);
    }

    fn delete(&mut self, cx: &Cx, id: String, name: &str) {
        let task = cx
            .svc
            .call(move |c| async move { c.delete_schedule(&id).await });
        self.push_action(trf("\"{}\" excluído.", &[&name]), task);
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.list.stale(Duration::from_secs(30)) {
            self.reload(cx);
        }
        self.list.poll();

        if self.threads.stale(Duration::from_secs(120)) {
            self.threads
                .start(cx.svc.call(|c| async move { c.threads().await }));
        }
        if self.threads.poll() {
            if let Some(v) = &self.threads.data {
                if let Ok(list) = serde_json::from_value::<Vec<ThreadSummary>>(v.clone()) {
                    self.thread_list = list;
                }
            }
        }

        // Ações concluídas: toast + recarga.
        let mut results = Vec::new();
        self.actions.retain(|(text, t)| match t.take() {
            Some(r) => {
                results.push((text.clone(), r));
                false
            }
            None => true,
        });
        for (text, r) in results {
            match r {
                Ok(_) => cx.toasts.success(text),
                Err(e) => cx.toasts.error(e),
            }
            self.list.loaded_at = None;
        }

        // Salvamento do formulário.
        if let Some(f) = &mut self.form {
            if let Some(t) = &f.save {
                if let Some(r) = t.take() {
                    f.save = None;
                    match r {
                        Ok(_) => {
                            cx.toasts.success(if f.editing.is_some() {
                                tr("Agendamento salvo.")
                            } else {
                                tr("Agendamento criado.")
                            });
                            self.form = None;
                            self.list.loaded_at = None;
                        }
                        Err(e) => cx.toasts.error(trf("Não salvou: {}", &[&e])),
                    }
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);

        ui::heading_help(
            ui,
            tr("Agendamentos"),
            Some(tr(
                "Lembretes e tarefas programados pelo agente ou por você; na hora, a mensagem é entregue na thread dona.",
            )),
            &help::SCHEDULER,
        );

        let enabled = self
            .list
            .data
            .as_ref()
            .map(|v| ui::b(v, "enabled"))
            .unwrap_or(cx.snap.settings.scheduler.enabled);
        if !enabled {
            let dark = ui.visuals().dark_mode;
            let warn = Tone::Warning.color(dark);
            egui::Frame::new()
                .fill(warn.linear_multiply(if dark { 0.18 } else { 0.12 }))
                .stroke(egui::Stroke::new(1.0, warn))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(tr(
                            "Agendamentos desligados nas configurações: nada dispara e o agente não consegue criar novos.",
                        )).color(warn));
                        if ui.button(tr("Configurar")).clicked() {
                            *cx.go = Some(Nav::Settings(SettingsSection::Scheduler));
                        }
                    });
                });
            ui.add_space(6.0);
        }

        // ---- barra de ações ----
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.form.is_none(),
                    egui::Button::new(
                        RichText::new(tr("Novo agendamento")).color(egui::Color32::WHITE),
                    )
                    .fill(ui::palette(ui.visuals().dark_mode).accent),
                )
                .clicked()
            {
                let default_thread = self.thread_list.first().map(|t| t.id.to_string());
                self.form = Some(Form::new(default_thread));
            }
            if ui.button(tr("Atualizar")).clicked() {
                self.list.loaded_at = None;
                self.threads.loaded_at = None;
            }
            if self.list.loading() {
                ui.spinner();
            }
        });

        // ---- formulário ----
        if self.form.is_some() {
            self.form_ui(ui, cx);
        }

        // ---- lista ----
        let rows: Vec<Value> = self
            .list
            .data
            .as_ref()
            .and_then(|v| v.get("schedules"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let title = if rows.is_empty() {
            tr("Agendamentos").to_string()
        } else {
            trf("Agendamentos ({})", &[&rows.len()])
        };
        ui::section(ui, &title, |ui| {
            ui.set_width(ui.available_width());
            if let Some(e) = &self.list.error {
                ui::error_text(ui, e);
            }
            if rows.is_empty() {
                if self.list.loading() {
                    ui::busy(ui, tr("carregando agendamentos…"));
                } else {
                    ui.label(RichText::new(tr(
                        "Nenhum agendamento. Crie um aqui ou peça ao agente numa thread (\"todo dia às 8h me mande…\").",
                    )).weak());
                }
                return;
            }
            // "em X min" / "atrasado": mantém o relógio dos cartões vivo.
            ui.ctx().request_repaint_after(Duration::from_secs(30));
            for v in &rows {
                self.card(ui, cx, v);
                ui.add_space(6.0);
            }
        });

        self.confirm_ui(ui, cx);
    }

    // ---------- formulário ----------

    fn form_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let threads = &self.thread_list;
        let Some(f) = &mut self.form else {
            return;
        };
        f.tick_preview(ui.ctx(), cx);
        let editing = f.editing.is_some();
        let saving = f.saving();
        let mut close = false;
        let mut submit = false;

        ui::section(
            ui,
            if editing {
                tr("Editar agendamento")
            } else {
                tr("Novo agendamento")
            },
            |ui| {
                ui::field(ui, tr("Nome"), None, |ui| {
                    ui::text_input(ui, &mut f.name, tr("ex.: resumo da agenda"));
                });
                ui::field(
                    ui,
                    tr("Mensagem"),
                    Some(tr(
                        "O que o agente deve fazer ao disparar, como se você escrevesse na thread.",
                    )),
                    |ui| {
                        ui::text_area(ui, &mut f.message, 4);
                    },
                );
                ui::field(ui, tr("Tipo"), None, |ui| {
                    ui::select(
                        ui,
                        "schedule-kind",
                        &mut f.kind,
                        &[
                            ("cron", tr("Recorrente (cron)")),
                            ("once", tr("Único (data/hora)")),
                        ],
                    );
                });
                if f.is_cron() {
                    ui::field(
                        ui,
                        tr("Expressão cron"),
                        Some(tr(
                            "5 campos: minuto hora dia mês dia-da-semana. Ex.: 0 8 * * *",
                        )),
                        |ui| {
                            ui.horizontal_top(|ui| {
                                let w = (ui.available_width() * 0.4).clamp(140.0, 260.0);
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut f.cron)
                                        .hint_text("0 8 * * *")
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(w),
                                );
                                ui::text_context_menu(&r);
                                ui.add_space(8.0);
                                ui.vertical(|ui| {
                                    ui.set_width(ui.available_width());
                                    preview_ui(ui, &f.preview, f.changed_at.is_some());
                                });
                            });
                        },
                    );
                } else {
                    ui::field(
                        ui,
                        tr("Data e hora"),
                        Some(tr(
                            "AAAA-MM-DDTHH:MM no fuso do agendamento, ou ISO-8601 com fuso.",
                        )),
                        |ui| {
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut f.run_at)
                                    .hint_text("2026-12-31T09:00")
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(260.0),
                            );
                            ui::text_context_menu(&r);
                        },
                    );
                }
                ui::field(
                    ui,
                    tr("Fuso"),
                    Some(tr(
                        "Opcional. Nome IANA; vazio usa o padrão das configurações.",
                    )),
                    |ui| {
                        ui::text_input(
                            ui,
                            &mut f.timezone,
                            tr("ex.: America/Sao_Paulo; vazio = padrão"),
                        );
                    },
                );
                ui::field(
                    ui,
                    tr("Thread dona"),
                    Some(tr("Recebe a mensagem no disparo; se for apagada, é recriada com a sessão salva.")),
                    |ui| {
                        if threads.is_empty() {
                            ui.label(RichText::new(tr("Nenhuma thread ainda: crie uma em Threads.")).weak());
                        } else {
                            let labels: Vec<(String, String)> = threads
                                .iter()
                                .map(|t| (t.id.to_string(), thread_option_label(t)))
                                .collect();
                            let options: Vec<(&str, &str)> = labels
                                .iter()
                                .map(|(k, l)| (k.as_str(), l.as_str()))
                                .collect();
                            ui::select(ui, "schedule-thread", &mut f.thread_id, &options);
                        }
                    },
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !saving,
                            egui::Button::new(
                                RichText::new(tr("Salvar")).color(egui::Color32::WHITE),
                            )
                            .fill(ui::palette(ui.visuals().dark_mode).accent),
                        )
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui
                        .add_enabled(!saving, egui::Button::new(tr("Cancelar")))
                        .clicked()
                    {
                        close = true;
                    }
                    if saving {
                        ui.spinner();
                    }
                });
            },
        );

        if close {
            self.form = None;
            return;
        }
        if submit {
            match f.input() {
                Ok(input) => {
                    let task = match f.editing.clone() {
                        Some(id) => cx
                            .svc
                            .call(move |c| async move { c.update_schedule(&id, input).await }),
                        None => cx
                            .svc
                            .call(move |c| async move { c.create_schedule(input).await }),
                    };
                    f.save = Some(task);
                }
                Err(e) => cx.toasts.error(e),
            }
        }
    }

    // ---------- cartão ----------

    fn card(&mut self, ui: &mut egui::Ui, cx: &Cx, v: &Value) {
        let dark = ui.visuals().dark_mode;
        let p = ui::palette(dark);
        let id = ui::s(v, "id");
        let name = ui::s(v, "name");
        let enabled = ui::b(v, "enabled");
        let busy = self.actions.iter().any(|(_, t)| t.pending());
        let trigger = &v["trigger"];
        let is_cron = ui::s(trigger, "kind") == "cron";

        egui::Frame::new()
            .fill(p.card_alt)
            .stroke(egui::Stroke::new(
                1.0,
                if enabled {
                    p.border
                } else {
                    p.border.linear_multiply(0.6)
                },
            ))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                // Linha 1: interruptor, nome, badges; à direita o gatilho.
                ui.horizontal(|ui| {
                    let mut on = enabled;
                    if ui
                        .add_enabled_ui(!busy, |ui| ui::toggle(ui, &mut on))
                        .inner
                        .changed()
                    {
                        self.update_enabled(cx, id.clone(), on);
                    }
                    ui.label(RichText::new(&name).font(ui::medium(15.5)));
                    if ui::s(v, "created_by") == "agent" {
                        ui::badge(ui, tr("agente"), Tone::Info);
                    } else {
                        ui::badge(ui, tr("você"), Tone::Neutral);
                    }
                    if is_cron {
                        ui::badge(ui, tr("recorrente"), Tone::Success);
                    } else {
                        ui::badge(ui, tr("único"), Tone::Warning);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if is_cron {
                            let desc = ui::s(v, "cron_description");
                            if !desc.is_empty() {
                                ui.label(RichText::new(desc).small().weak());
                            }
                            ui.label(RichText::new(ui::s(trigger, "expr")).monospace())
                                .on_hover_text(ui::s(v, "timezone_label"));
                        } else {
                            ui.label(RichText::new(once_local(trigger)).monospace())
                                .on_hover_text(ui::s(v, "timezone_label"));
                        }
                    });
                });

                // Linha 2: próximo disparo (ou pausado / motivo).
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(tr("Próximo:")).small().weak());
                    let paused_reason = ui::s(v, "paused_reason");
                    if !enabled {
                        if paused_reason.is_empty() {
                            ui.label(
                                RichText::new(tr("pausado"))
                                    .small()
                                    .color(Tone::Neutral.color(dark)),
                            );
                        } else {
                            ui.label(
                                RichText::new(trf("pausado · {}", &[&paused_reason]))
                                    .small()
                                    .color(Tone::Warning.color(dark)),
                            );
                        }
                    } else {
                        let local = ui::s(v, "next_run_local");
                        let in_ = ui::s(v, "next_run_in");
                        if local.is_empty() {
                            ui.label(RichText::new("—").small().weak());
                        } else {
                            ui.label(RichText::new(local).small().font(ui::medium(12.5)));
                            if !in_.is_empty() {
                                ui.label(RichText::new(format!("· {in_}")).small().weak());
                            }
                        }
                        let tz = ui::s(v, "timezone_label");
                        if !tz.is_empty() {
                            ui.label(RichText::new(format!("· {tz}")).small().weak());
                        }
                    }
                });

                // Linha 3: thread dona.
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(tr("Thread:")).small().weak());
                    if ui::b(v, "thread_exists") {
                        let title = v
                            .get("thread_title")
                            .and_then(|t| t.as_str())
                            .filter(|t| !t.trim().is_empty())
                            .map(String::from)
                            .unwrap_or_else(|| {
                                format!("Thread {}", ui::ellipsize(&ui::s(v, "thread_id"), 9))
                            });
                        ui.label(RichText::new(title).small())
                            .on_hover_text(ui::s(v, "thread_id"));
                        ui::badge(ui, &origin_kind_label(&v["origin"]), Tone::Info);
                    } else {
                        ui.label(
                            RichText::new(tr("thread removida; será recriada no disparo"))
                                .small()
                                .color(Tone::Warning.color(dark)),
                        )
                        .on_hover_text(ui::s(v, "thread_id"));
                        ui::badge(ui, &origin_kind_label(&v["origin"]), Tone::Neutral);
                    }
                });

                // Linha 4: último disparo e contagem.
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(tr("Último:")).small().weak());
                    let last = ui::fmt_dt(&v["last_run_at"]);
                    if last.is_empty() {
                        ui.label(RichText::new(tr("nunca")).small().weak());
                    } else {
                        ui.label(RichText::new(last).small());
                        let result = ui::s(v, "last_result");
                        if result == "ok" {
                            ui.label(
                                RichText::new("· ok")
                                    .small()
                                    .color(Tone::Success.color(dark)),
                            );
                        } else if !result.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("· {result}"))
                                        .small()
                                        .color(Tone::Danger.color(dark)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(result);
                        }
                    }
                    ui.label(
                        RichText::new(trf("· {} disparo(s)", &[&ui::i(v, "run_count")]))
                            .small()
                            .weak(),
                    );
                });

                // Mensagem (cortada, com "mais…").
                ui.add_space(4.0);
                let message = ui::s(v, "message");
                let expanded = self.expanded.contains(&id);
                let long = message.chars().count() > MESSAGE_PREVIEW_CHARS;
                let shown = if long && !expanded {
                    ui::ellipsize(&message, MESSAGE_PREVIEW_CHARS)
                } else {
                    message.clone()
                };
                egui::Frame::new()
                    .fill(p.card)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui::wrapped_text(ui, &shown);
                        if long {
                            let label = if expanded { tr("menos") } else { tr("mais…") };
                            if ui.small_button(label).clicked() {
                                if expanded {
                                    self.expanded.remove(&id);
                                } else {
                                    self.expanded.insert(id.clone());
                                }
                            }
                        }
                    });

                // Ações.
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(ui::ellipsize(&id, 9))
                            .small()
                            .weak()
                            .monospace(),
                    )
                    .on_hover_text(&id);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!busy, egui::Button::new(tr("Excluir")))
                            .clicked()
                        {
                            self.confirm_delete = Some((id.clone(), name.clone()));
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new(tr("Disparar agora")))
                            .on_hover_text(tr(
                                "Entrega a mensagem na thread agora, fora do horário.",
                            ))
                            .clicked()
                        {
                            self.run_now(cx, id.clone(), &name);
                        }
                        if ui
                            .add_enabled(
                                !busy && self.form.is_none(),
                                egui::Button::new(tr("Editar")),
                            )
                            .clicked()
                        {
                            self.form = Some(Form::from_view(v));
                        }
                    });
                });
            });
    }

    // ---------- confirmação ----------

    fn confirm_ui(&mut self, ui: &mut egui::Ui, cx: &Cx) {
        if let Some((id, name)) = self.confirm_delete.clone() {
            let c = ui::Confirm {
                title: tr("Excluir agendamento").into(),
                body: trf("Excluir \"{}\"? Isso não tem volta.", &[&name]),
                confirm_label: tr("Excluir").into(),
                danger: true,
                tag: "del-schedule".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_delete = None;
                if ok {
                    self.delete(cx, id, &name);
                }
            }
        }
    }
}

// ---------- helpers privados ----------

/// Prévia do cron ao lado do campo: descrição + próximas ocorrências, ou o erro.
fn preview_ui(ui: &mut egui::Ui, preview: &Loader<Value>, waiting: bool) {
    let dark = ui.visuals().dark_mode;
    if preview.loading() || waiting {
        ui.label(RichText::new(tr("calculando…")).small().weak());
        return;
    }
    if let Some(e) = &preview.error {
        ui.add(egui::Label::new(RichText::new(e).small().color(Tone::Danger.color(dark))).wrap());
        return;
    }
    let Some(v) = &preview.data else {
        return;
    };
    let desc = ui::s(v, "description");
    if !desc.is_empty() {
        ui.add(egui::Label::new(RichText::new(desc).small().font(ui::medium(12.5))).wrap());
    }
    let tz = ui::s(v, "timezone");
    if !tz.is_empty() {
        ui.label(RichText::new(tz).small().weak());
    }
    if let Some(next) = v.get("next").and_then(|n| n.as_array()) {
        for n in next.iter().take(PREVIEW_COUNT) {
            ui.label(
                RichText::new(format!("{} · {}", ui::s(n, "local"), ui::s(n, "in")))
                    .small()
                    .monospace(),
            );
        }
    }
    let err = ui::s(v, "error");
    if !err.is_empty() {
        ui.add(egui::Label::new(RichText::new(err).small().color(Tone::Danger.color(dark))).wrap());
    }
}

/// Data/hora local (desta máquina) de um gatilho único.
fn once_local(trigger: &Value) -> String {
    trigger
        .get("at")
        .and_then(|a| a.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d/%m/%Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| ui::s(trigger, "at"))
}

/// Rótulo curto do canal de origem (`origin.kind` do JSON).
fn origin_kind_label(origin: &Value) -> String {
    match ui::s(origin, "kind").as_str() {
        "ui" => "app".into(),
        "telegram" => {
            let chat = ui::i(origin, "chat_id");
            match origin.get("topic_id").and_then(|t| t.as_i64()) {
                Some(tp) => format!("telegram {chat}:{tp}"),
                None => format!("telegram {chat}"),
            }
        }
        "realtime" => tr("voz").into(),
        other => other.to_string(),
    }
}

/// Opção do seletor de thread: título (ou id curto) + origem.
fn thread_option_label(t: &ThreadSummary) -> String {
    let title = t
        .title
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("Thread {}", &t.id.to_string()[..8]));
    let origin = match &t.origin {
        will_core::Origin::Ui => "app".to_string(),
        will_core::Origin::Telegram { chat_id, topic_id } => match topic_id {
            Some(tp) => format!("telegram {chat_id}:{tp}"),
            None => format!("telegram {chat_id}"),
        },
        will_core::Origin::Realtime => tr("voz").into(),
    };
    format!("{} · {origin}", ui::ellipsize(&title, 40))
}
