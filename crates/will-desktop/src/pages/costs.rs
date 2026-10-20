//! Custos de IA: filtro por fonte, resumo por período (respeitando o filtro) e
//! registros recentes em linhas compactas.

use super::Cx;
use crate::service::{j, Loader};
use crate::ui::{self, Tone};
use axum::extract::{Query, State};
use serde_json::Value;
use will_core::Event;
use will_server::api::system;

#[derive(Default)]
pub struct CostsPage {
    summary: Loader<Value>,
    rows: Loader<Value>,
    source: String,
}

fn source_label(s: &str) -> &str {
    match s {
        "voice" => "Voz",
        "claude" => "Claude Code",
        "telegram_stt" => "Telegram · transcrição",
        "telegram_tts" => "Telegram · áudio",
        other => other,
    }
}

fn source_tone(s: &str) -> Tone {
    match s {
        "voice" => Tone::Info,
        "claude" => Tone::Success,
        _ => Tone::Neutral,
    }
}

fn unit_label(k: &str) -> &str {
    match k {
        "text_in" => "texto in",
        "text_out" => "texto out",
        "text_cached" => "texto cache",
        "audio_in" => "áudio in",
        "audio_out" => "áudio out",
        "audio_cached" => "áudio cache",
        "input_tokens" => "entrada",
        "output_tokens" => "saída",
        "cache_read_input_tokens" => "cache lido",
        "cache_creation_input_tokens" => "cache criado",
        other => other,
    }
}

fn fmt_units(v: &Value) -> String {
    let Some(o) = v.as_object() else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for (k, val) in o {
        if let Some(n) = val.as_u64() {
            if n > 0 && k != "responses" {
                parts.push(format!("{} {n}", unit_label(k)));
            }
        }
    }
    parts.join(" · ")
}

impl CostsPage {
    pub fn on_event(&mut self, ev: &Event) {
        if matches!(ev, Event::CostRecorded { .. }) {
            self.summary.loaded_at = None;
            self.rows.loaded_at = None;
        }
    }

    fn reload(&mut self, cx: &Cx) {
        let source = if self.source.is_empty() {
            None
        } else {
            Some(self.source.clone())
        };
        let s1 = source.clone();
        self.summary.start(cx.svc.call(move |s| async move {
            j(system::costs_summary(
                State(s),
                Query(system::CostsQuery {
                    limit: None,
                    source: s1,
                }),
            )
            .await)
        }));
        self.rows.start(cx.svc.call(move |s| async move {
            j(system::costs(
                State(s),
                Query(system::CostsQuery {
                    limit: Some(300),
                    source,
                }),
            )
            .await)
        }));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.summary.stale(std::time::Duration::from_secs(60))
            || self.rows.stale(std::time::Duration::from_secs(60))
        {
            self.reload(cx);
        }
        self.summary.poll();
        self.rows.poll();

        ui::heading(ui, "Custos de IA", Some("Voz: estimativa pela tabela de preços (Voz › Preços). Claude: valor informado pelo CLI a cada turno."));

        // ---- filtro (vale para o resumo e para os registros) ----
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Fonte").strong());
            if ui::select(
                ui,
                "cost-source",
                &mut self.source,
                &[("", "todas"), ("voice", "Voz"), ("claude", "Claude Code")],
            ) {
                self.summary.loaded_at = None;
                self.rows.loaded_at = None;
            }
            if ui.button("Atualizar").clicked() {
                self.summary.loaded_at = None;
                self.rows.loaded_at = None;
            }
            if self.summary.loading() || self.rows.loading() {
                ui.spinner();
            }
        });

        // ---- resumo ----
        ui::section(
            ui,
            if self.source.is_empty() {
                "Resumo (todas as fontes)"
            } else {
                "Resumo (fonte filtrada)"
            },
            |ui| {
                if let Some(e) = &self.summary.error {
                    ui::error_text(ui, e);
                }
                if let Some(s) = &self.summary.data {
                    let periods = [
                        ("Hoje", "today_usd", "today"),
                        ("7 dias", "last_7d_usd", "last_7d"),
                        ("30 dias", "last_30d_usd", "last_30d"),
                        ("Total", "all_time_usd", "all_time"),
                    ];
                    let per_row = if ui.available_width() >= 760.0 { 4 } else { 2 };
                    let rows: Vec<&[(&str, &str, &str)]> = periods.chunks(per_row).collect();
                    for chunk in rows {
                        ui.columns(per_row, |cols| {
                            for (col, (title, total_key, bucket_key)) in
                                cols.iter_mut().zip(chunk.iter())
                            {
                                let total = s[total_key].as_f64().unwrap_or(0.0);
                                egui::Frame::new()
                                    .fill(col.visuals().faint_bg_color)
                                    .corner_radius(8.0)
                                    .inner_margin(12.0)
                                    .show(col, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(egui::RichText::new(*title).weak().small());
                                        ui.label(
                                            egui::RichText::new(format!("US$ {total:.4}"))
                                                .strong()
                                                .size(19.0),
                                        );
                                        if let Some(b) = s[bucket_key].as_array() {
                                            for x in b {
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(format!(
                                                            "{}: US$ {:.4} · {} reg.",
                                                            source_label(&ui::s(x, "source")),
                                                            x["usd"].as_f64().unwrap_or(0.0),
                                                            ui::i(x, "count")
                                                        ))
                                                        .small()
                                                        .weak(),
                                                    )
                                                    .truncate(),
                                                );
                                            }
                                            if b.is_empty() {
                                                ui.label(
                                                    egui::RichText::new("sem registros")
                                                        .small()
                                                        .weak(),
                                                );
                                            }
                                        }
                                    });
                            }
                        });
                        ui.add_space(8.0);
                    }
                } else if self.summary.loading() {
                    ui::busy(ui, "calculando…");
                }
            },
        );

        // ---- registros ----
        ui::section(ui, "Registros", |ui| {
            if let Some(e) = &self.rows.error {
                ui::error_text(ui, e);
            }
            let rows = self
                .rows
                .data
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() && !self.rows.loading() {
                ui.label(egui::RichText::new("Nenhum custo registrado ainda. Use a voz ou mande uma mensagem a uma thread.").weak());
            }
            for r in &rows {
                let src = ui::s(r, "source");
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(ui::fmt_dt(&r["at"])).small().weak());
                            ui::badge(ui, source_label(&src), source_tone(&src));
                            ui.label(
                                egui::RichText::new(format!(
                                    "US$ {:.4}",
                                    r["usd"].as_f64().unwrap_or(0.0)
                                ))
                                .strong(),
                            );
                            ui.label(
                                egui::RichText::new(if ui::b(r, "estimated") {
                                    "estimado"
                                } else {
                                    "informado"
                                })
                                .small()
                                .weak(),
                            );
                            if let Some(d) = r.get("duration_secs").and_then(|d| d.as_i64()) {
                                ui.label(egui::RichText::new(format!("{d} s")).small().weak());
                            }
                        });
                        let model = r.get("model").and_then(|m| m.as_str()).unwrap_or("");
                        let units = fmt_units(&r["units"]);
                        if !model.is_empty() || !units.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "{model}{}{units}",
                                        if model.is_empty() || units.is_empty() {
                                            ""
                                        } else {
                                            " · "
                                        }
                                    ))
                                    .small()
                                    .weak(),
                                )
                                .wrap(),
                            );
                        }
                    });
            }
        });
    }
}
