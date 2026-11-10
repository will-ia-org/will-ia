//! Seção Tarefas, em duas abas: **Configuração** (liga/desliga o módulo, fuso
//! das datas, avisos do sistema e do Telegram) e **Google Calendar** (agenda +
//! janela de datas + botão "Sincronizar agora", com o relatório da última rodada).

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::pages::tasks::{date_picker, jdate};
use crate::pages::{Cx, Nav};
use crate::service::{Loader, Task};
use crate::ui;
use serde_json::{json, Value};
use std::time::Duration;
use will_core::{Module, ModuleStatus};

pub struct TasksSettingsPage {
    calendars: Loader<Value>,
    from: jiff::civil::Date,
    to: jiff::civil::Date,
    sync_task: Option<Task<Value>>,
    last_sync: Option<Value>,
}

impl Default for TasksSettingsPage {
    fn default() -> Self {
        let (from, to) = default_window();
        Self {
            calendars: Loader::default(),
            from,
            to,
            sync_task: None,
            last_sync: None,
        }
    }
}

/// Janela padrão: hoje → daqui a 2 meses.
fn default_window() -> (jiff::civil::Date, jiff::civil::Date) {
    let today = chrono::Local::now().date_naive();
    let to = today + chrono::Months::new(2);
    (jdate(today), jdate(to))
}

fn ymd(d: jiff::civil::Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

impl TasksSettingsPage {
    fn poll(&mut self, cx: &mut Cx) {
        if self.calendars.stale(Duration::from_secs(120)) {
            self.calendars.start(
                cx.svc
                    .call(|c| async move { c.google_calendars(&[]).await }),
            );
        }
        self.calendars.poll();
        if let Some(t) = &self.sync_task {
            if let Some(r) = t.take() {
                self.sync_task = None;
                match r {
                    Ok(v) => {
                        let warnings = v
                            .get("warnings")
                            .and_then(|w| w.as_array())
                            .map(|w| w.len())
                            .unwrap_or(0);
                        let total = ui::i(&v, "pulled_created")
                            + ui::i(&v, "pulled_updated")
                            + ui::i(&v, "pulled_deleted")
                            + ui::i(&v, "pushed_created")
                            + ui::i(&v, "pushed_updated")
                            + ui::i(&v, "pushed_deleted");
                        if warnings > 0 {
                            cx.toasts.error(trf(
                                "Sincronização: {} mudança(s), {} aviso(s)",
                                &[&total, &warnings],
                            ));
                        } else {
                            cx.toasts.success(trf(
                                "Sincronização: {} mudança(s), {} sem alteração",
                                &[&total, &ui::i(&v, "unchanged")],
                            ));
                        }
                        self.last_sync = Some(v);
                    }
                    Err(e) => {
                        cx.toasts.error(trf("Sincronização falhou: {}", &[&e]));
                        self.last_sync = None;
                    }
                }
            }
        }
    }

    /// Bloco "Sincronizar com o Google Calendar".
    fn sync_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        let cal_status = cx
            .snap
            .modules
            .get(&Module::Calendar)
            .cloned()
            .unwrap_or(ModuleStatus::Stopped);
        let cal_ok = cal_status.is_running();
        ui.horizontal(|ui| {
            ui.label(tr("Agenda do Google:"));
            ui::badge(
                ui,
                &ui::module_label(&cal_status),
                ui::tone_for_module(&cal_status),
            );
            if !cal_ok {
                ui.label(
                    egui::RichText::new(tr(
                        "Conecte uma conta e ligue a Agenda em Configurações › Google.",
                    ))
                    .weak()
                    .small(),
                );
                if ui.small_button(tr("abrir Google")).clicked() {
                    *cx.go = Some(Nav::Settings(super::SettingsSection::Google));
                }
            }
        });
        ui.add_space(4.0);

        // agenda alvo: habilitadas e graváveis
        let list: Vec<Value> = self
            .calendars
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let writable: Vec<(String, String)> = list
            .iter()
            .filter(|c| ui::b(c, "enabled"))
            .filter(|c| {
                let role = ui::s(c, "access_role");
                role.is_empty() || role == "owner" || role == "writer"
            })
            .map(|c| {
                let mut label = ui::s(c, "summary");
                if ui::b(c, "default_for_create") {
                    label.push_str(" · ");
                    label.push_str(tr("padrão"));
                }
                (ui::s(c, "calendar_id"), label)
            })
            .collect();
        let t = &mut st.draft.tasks;
        ui::field(
            ui,
            tr("Agenda"),
            Some(tr(
                "Só eventos desta agenda viram tarefas e só nela as tarefas viram eventos. Salvar guarda a escolha.",
            )),
            |ui| {
                let mut options: Vec<(&str, &str)> =
                    vec![("", tr("Agenda padrão (Google › Agenda)"))];
                for (k, l) in &writable {
                    options.push((k.as_str(), l.as_str()));
                }
                ui::select(ui, "tasks-sync-calendar", &mut t.sync_calendar_id, &options);
                if self.calendars.loading() {
                    ui.spinner();
                }
            },
        );
        ui::field(
            ui,
            tr("Período"),
            Some(tr(
                "Janela de início das tarefas e dos eventos considerados (padrão: hoje até 2 meses à frente).",
            )),
            |ui| {
                ui.horizontal(|ui| {
                    ui.add(date_picker(&mut self.from, "tasks-sync-from"));
                    ui.label("→");
                    ui.add(date_picker(&mut self.to, "tasks-sync-to"));
                    if ui
                        .small_button(tr("2 meses"))
                        .on_hover_text(tr("Volta ao período padrão."))
                        .clicked()
                    {
                        let (f, to) = default_window();
                        self.from = f;
                        self.to = to;
                    }
                });
            },
        );
        let bad_range = self.to < self.from;
        if bad_range {
            ui::error_text(
                ui,
                tr("A data final precisa ser igual ou depois da inicial."),
            );
        }
        ui.horizontal(|ui| {
            let syncing = self.sync_task.is_some();
            let enabled = !syncing && cal_ok && !bad_range && t.enabled;
            let resp = ui
                .add_enabled_ui(enabled, |ui| ui::primary_button(ui, tr("Sincronizar agora")))
                .inner
                .on_hover_text(tr(
                    "O que está aqui vai para a agenda, o que está na agenda vem para cá; itens já espelhados seguem a última edição. Nada é duplicado.",
                ))
                .on_disabled_hover_text(tr(
                    "Precisa do módulo Tarefas ligado, da Agenda do Google no ar e de um período válido.",
                ));
            if resp.clicked() {
                let body = json!({
                    "from": ymd(self.from),
                    "to": ymd(self.to),
                    "calendar_id": t.sync_calendar_id,
                });
                self.sync_task = Some(
                    cx.svc
                        .call(move |c| async move { c.sync_tasks_calendar(body).await }),
                );
            }
            if syncing {
                ui.spinner();
                ui.label(egui::RichText::new(tr("sincronizando…")).weak().small());
            }
        });
        if let Some(v) = &self.last_sync {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(trf(
                    "Agenda \"{}\" · {} → {}",
                    &[&ui::s(v, "calendar"), &ui::s(v, "from"), &ui::s(v, "to")],
                ))
                .small(),
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(tr("⬇ Agenda → tarefas:"))
                        .strong()
                        .small(),
                );
                ui.label(
                    egui::RichText::new(trf(
                        "{} criada(s) · {} atualizada(s) · {} removida(s)",
                        &[
                            &ui::i(v, "pulled_created"),
                            &ui::i(v, "pulled_updated"),
                            &ui::i(v, "pulled_deleted"),
                        ],
                    ))
                    .small(),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(tr("⬆ Tarefas → agenda:"))
                        .strong()
                        .small(),
                );
                ui.label(
                    egui::RichText::new(trf(
                        "{} criado(s) · {} atualizado(s) · {} removido(s)",
                        &[
                            &ui::i(v, "pushed_created"),
                            &ui::i(v, "pushed_updated"),
                            &ui::i(v, "pushed_deleted"),
                        ],
                    ))
                    .small(),
                );
            });
            ui.label(
                egui::RichText::new(trf(
                    "{} sem alteração · {} ignorado(s) · {} ms",
                    &[
                        &ui::i(v, "unchanged"),
                        &ui::i(v, "skipped"),
                        &ui::i(v, "duration_ms"),
                    ],
                ))
                .weak()
                .small(),
            );
            if let Some(w) = v.get("warnings").and_then(|w| w.as_array()) {
                for e in w.iter().take(6) {
                    ui::error_text(ui, e.as_str().unwrap_or(""));
                }
                if w.len() > 6 {
                    ui.label(
                        egui::RichText::new(trf(
                            "… e mais {} aviso(s) (veja Logs)",
                            &[&(w.len() - 6)],
                        ))
                        .weak()
                        .small(),
                    );
                }
            }
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(tr(
                "Como funciona: a tarefa guarda o id do evento e o evento guarda o id da tarefa, então repetir a sincronização nunca duplica. Concluída vira \"✓ título\" na agenda. Apagar de um lado apaga do outro. Evento de dia inteiro vira tarefa 00:00 → 23:59; recorrente entra uma ocorrência por vez.",
            ))
            .weak()
            .small(),
        );
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx);
        ui::heading_help(
            ui,
            tr("Tarefas"),
            Some(tr(
                "Painel Gantt de tarefas com início/fim e prioridade; o agente mexe nelas pelas ferramentas tasks_*.",
            )),
            &help::TASKS,
        );
        let tab = ui::tabs(
            ui,
            "tasks-settings",
            &[
                ui::Tab::new("config", tr("Configuração")),
                ui::Tab::new("sync", tr("Google Calendar")),
            ],
        );
        match tab {
            "sync" => ui::section(ui, tr("Sincronizar com o Google Calendar"), |ui| {
                self.sync_ui(ui, cx, st);
            }),
            _ => ui::section(ui, tr("Configuração"), |ui| {
                let status = cx
                    .snap
                    .modules
                    .get(&Module::Tasks)
                    .cloned()
                    .unwrap_or(ModuleStatus::Stopped);
                ui.horizontal(|ui| {
                    ui.label(tr("Módulo:"));
                    ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
                    if ui.button(tr("ver tarefas")).clicked() {
                        *cx.go = Some(Nav::Tasks);
                    }
                });
                ui.add_space(4.0);
                let t = &mut st.draft.tasks;
                ui::field(
                ui,
                tr("Habilitar"),
                Some(tr(
                    "Desligado, o painel some do agente (tasks_* indisponíveis) e nenhum aviso é enviado; as tarefas ficam guardadas.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.enabled);
                },
            );
                ui::field(
                ui,
                tr("Fuso horário"),
                Some(tr(
                    "Nome IANA usado nas datas digitadas sem fuso e na exibição. Brasil: America/Sao_Paulo (GMT-3).",
                )),
                |ui| {
                    ui::text_input(ui, &mut t.timezone, "America/Sao_Paulo");
                },
            );
                ui::field(
                ui,
                tr("Notificação do sistema"),
                Some(tr(
                    "Notificação nativa quando uma tarefa entra em andamento ou em atraso (como nas triggers e aprovações).",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.notify_native);
                },
            );
                ui::field(
                ui,
                tr("Avisar no Telegram ao entrar em andamento"),
                Some(tr(
                    "Quando o relógio passa do início de uma tarefa pendente, manda uma mensagem ao chat padrão do Telegram.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.notify_start);
                },
            );
                ui::field(
                ui,
                tr("Avisar no Telegram ao entrar em atraso"),
                Some(tr(
                    "Quando o relógio passa do fim de uma tarefa não concluída, manda uma mensagem ao chat padrão do Telegram.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.notify_overdue);
                },
            );
                ui.label(
                egui::RichText::new(tr(
                    "Os avisos usam o destino padrão da seção Telegram (chat padrão ou primeiro chat permitido) e precisam do módulo Telegram no ar.",
                ))
                .weak()
                .small(),
            );
            }),
        }
        st.save_bar(ui, cx.svc, "tasks");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_defaults_to_two_months() {
        let (f, t) = default_window();
        assert!(t > f);
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        assert_eq!(ymd(jdate(d)), "2026-01-31");
        assert_eq!(ymd(jdate(d + chrono::Months::new(2))), "2026-03-31");
    }
}
