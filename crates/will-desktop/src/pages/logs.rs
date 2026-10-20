//! Logs: histórico do banco + cauda ao vivo pelos eventos `Log`.

use super::Cx;
use crate::service::{j, Loader};
use crate::ui::{self, Tone};
use axum::extract::{Query, State};
use serde_json::Value;
use will_core::Event;
use will_server::api::system;

#[derive(Default)]
pub struct LogsPage {
    history: Loader<Value>,
    live: Vec<(chrono::DateTime<chrono::Utc>, String, String, String)>,
    module: String,
    level: String,
    filter: String,
    paused: bool,
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
                .push((*at, level.clone(), module.clone(), message.clone()));
            if self.live.len() > 2000 {
                self.live.drain(..500);
            }
        }
    }

    fn reload(&mut self, cx: &Cx) {
        let module = if self.module.is_empty() {
            None
        } else {
            Some(self.module.clone())
        };
        let level = if self.level.is_empty() {
            None
        } else {
            Some(self.level.clone())
        };
        self.live.clear();
        self.history.start(cx.svc.call(move |s| async move {
            j(system::logs(
                State(s),
                Query(system::LogsQuery {
                    limit: Some(400),
                    module,
                    level,
                }),
            )
            .await)
        }));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.history.loaded_at.is_none() && !self.history.busy() {
            self.reload(cx);
        }
        self.history.poll();
        ui::heading(ui, "Logs", Some("Registros do serviço (segredos redigidos). Arquivos completos ficam na pasta de logs."));
        ui.horizontal(|ui| {
            ui.label("Módulo");
            if ui::select(
                ui,
                "log-module",
                &mut self.module,
                &[
                    ("", "todos"),
                    ("engine", "engine"),
                    ("telegram", "telegram"),
                    ("realtime", "realtime"),
                    ("mcp", "mcp"),
                    ("whatsapp", "whatsapp"),
                    ("google", "google"),
                    ("server", "server"),
                ],
            ) {
                self.reload(cx);
            }
            ui.label("Nível");
            if ui::select(
                ui,
                "log-level",
                &mut self.level,
                &[
                    ("", "todos"),
                    ("error", "error"),
                    ("warn", "warn"),
                    ("info", "info"),
                ],
            ) {
                self.reload(cx);
            }
            ui.label("Filtro");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .desired_width(200.0)
                    .hint_text("texto"),
            );
            if ui.button("Atualizar").clicked() {
                self.reload(cx);
            }
            ui.checkbox(&mut self.paused, "pausar ao vivo");
            if ui.button("Abrir pasta de logs").clicked() {
                let dir = cx.svc.state.paths.logs_dir.clone();
                if let Err(e) = will_platform::open_path(&dir) {
                    cx.toasts.error(e);
                }
            }
        });
        if let Some(e) = &self.history.error {
            ui::error_text(ui, e);
        }
        let filter = self.filter.to_lowercase();
        let level_tone = |l: &str| match l.to_lowercase().as_str() {
            "error" => Tone::Danger,
            "warn" | "warning" => Tone::Warning,
            "info" => Tone::Info,
            _ => Tone::Neutral,
        };
        let max_h = (ui.ctx().content_rect().height() - 150.0).max(200.0);
        egui::ScrollArea::vertical()
            .id_salt("logs-scroll")
            .auto_shrink([false, false])
            .max_height(max_h)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let mut rows: Vec<(String, String, String, String)> = Vec::new();
                if let Some(list) = self.history.data.as_ref().and_then(|v| v.as_array()) {
                    for r in list.iter().rev() {
                        rows.push((
                            ui::fmt_dt(&r["at"]),
                            ui::s(r, "level"),
                            ui::s(r, "module"),
                            ui::s(r, "message"),
                        ));
                    }
                }
                for (at, level, module, msg) in &self.live {
                    if !self.module.is_empty() && &self.module != module {
                        continue;
                    }
                    if !self.level.is_empty() && !level.eq_ignore_ascii_case(&self.level) {
                        continue;
                    }
                    rows.push((
                        at.with_timezone(&chrono::Local)
                            .format("%d/%m %H:%M:%S")
                            .to_string(),
                        level.clone(),
                        module.clone(),
                        msg.clone(),
                    ));
                }
                for (at, level, module, msg) in rows {
                    if !filter.is_empty()
                        && !msg.to_lowercase().contains(&filter)
                        && !module.to_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(at).monospace().small().weak());
                        ui.label(
                            egui::RichText::new(level.to_uppercase())
                                .monospace()
                                .small()
                                .color(level_tone(&level).color(ui.visuals().dark_mode)),
                        );
                        ui.label(egui::RichText::new(module).monospace().small().strong());
                        ui.add(
                            egui::Label::new(egui::RichText::new(msg).monospace().small())
                                .wrap()
                                .selectable(true),
                        );
                    });
                }
            });
    }
}
