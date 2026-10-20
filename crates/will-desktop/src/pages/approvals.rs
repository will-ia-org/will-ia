//! Aprovações: pedidos pendentes (aprovar/negar) e histórico.

use super::Cx;
use crate::service::{j, Loader, Task};
use crate::ui::{self, Tone};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::Value;
use will_core::Event;
use will_server::api::mcp as api;

#[derive(Default)]
pub struct ApprovalsPage {
    history: Loader<Value>,
    decide: Vec<Task<Value>>,
}

impl ApprovalsPage {
    pub fn on_event(&mut self, ev: &Event) {
        if matches!(ev, Event::ApprovalDecided { .. }) {
            self.history.loaded_at = None;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.history.stale(std::time::Duration::from_secs(60)) {
            self.history.start(cx.svc.call(|s| async move {
                j(api::approvals_history(State(s), Query(api::Limit { limit: Some(100) })).await)
            }));
        }
        self.history.poll();
        self.decide.retain(|t| match t.take() {
            Some(Err(e)) => {
                cx.toasts.error(e);
                false
            }
            Some(Ok(_)) => false,
            None => true,
        });

        ui::heading(ui, "Aprovações", Some("Ferramentas com efeito externo pedem confirmação. Sem decisão até o tempo limite, o pedido é negado."));

        ui::section(ui, "Pendentes", |ui| {
            if cx.snap.pending_approvals.is_empty() {
                ui.label(egui::RichText::new("Nenhum pedido pendente.").weak());
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
            for a in &cx.snap.pending_approvals {
                let elapsed = (chrono::Utc::now() - a.requested_at).num_seconds().max(0) as u64;
                let left = a.timeout_secs.saturating_sub(elapsed);
                egui::Frame::group(ui.style())
                    .corner_radius(8.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui::badge(ui, &a.tool, Tone::Warning);
                            ui.label(egui::RichText::new(&a.summary).strong());
                        });
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "pedido {} · expira em {}s",
                                    ui::fmt_dt_chrono(&a.requested_at),
                                    left
                                ))
                                .small()
                                .weak(),
                            );
                            if let Some(t) = a.thread_id {
                                ui.label(
                                    egui::RichText::new(format!("thread {}", &t.to_string()[..8]))
                                        .small()
                                        .weak(),
                                );
                            }
                        });
                        ui.collapsing("argumentos", |ui| {
                            ui::mono(
                                ui,
                                &serde_json::to_string_pretty(&a.args_redacted).unwrap_or_default(),
                            );
                        });
                        ui.horizontal(|ui| {
                            let id = a.id.to_string();
                            if ui::primary_button(ui, "Aprovar").clicked() {
                                let id2 = id.clone();
                                self.decide.push(cx.svc.call(move |s| async move {
                                    j(api::decide(
                                        State(s),
                                        Path(id2),
                                        Json(api::DecideBody {
                                            decision: "approved".into(),
                                        }),
                                    )
                                    .await)
                                }));
                            }
                            if ui::danger_button(ui, "Negar").clicked() {
                                self.decide.push(cx.svc.call(move |s| async move {
                                    j(api::decide(
                                        State(s),
                                        Path(id),
                                        Json(api::DecideBody {
                                            decision: "denied".into(),
                                        }),
                                    )
                                    .await)
                                }));
                            }
                        });
                    });
            }
        });

        ui::section(ui, "Histórico", |ui| {
            if let Some(e) = &self.history.error {
                ui::error_text(ui, e);
            }
            let rows = self
                .history
                .data
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() {
                ui.label(egui::RichText::new("Sem decisões registradas.").weak());
            }
            egui::Grid::new("approvals-history")
                .num_columns(5)
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for r in &rows {
                        ui.label(egui::RichText::new(ui::fmt_dt(&r["requested_at"])).small());
                        ui.label(ui::s(r, "tool"));
                        ui.add(egui::Label::new(ui::s(r, "summary")).truncate());
                        let d = ui::s(r, "decision");
                        ui::badge(
                            ui,
                            if d.is_empty() { "pendente" } else { &d },
                            match d.as_str() {
                                "approved" => Tone::Success,
                                "denied" => Tone::Danger,
                                "timeout" => Tone::Warning,
                                _ => Tone::Neutral,
                            },
                        );
                        ui.label(egui::RichText::new(ui::s(r, "via")).small().weak());
                        ui.end_row();
                    }
                });
        });
    }
}
