//! Custos de IA: filtro por fonte, resumo por período (respeitando o filtro) e
//! registros recentes em linhas compactas.

use super::Cx;
use crate::i18n::{tr, trf};
use crate::service::Loader;
use crate::ui::{self, Tone};
use serde_json::Value;
use will_core::Event;

#[derive(Default)]
pub struct CostsPage {
    summary: Loader<Value>,
    rows: Loader<Value>,
    source: String,
    /// Fontes já vistas nos dados (resumo "todas" e registros), para o filtro
    /// oferecer também o que o código ainda não conhece por nome.
    seen_sources: Vec<String>,
}

/// Fontes gravadas em `ai_costs` pelo código (valor → rótulo), na ordem do filtro.
/// Quem grava: `will-realtime` (`voice`), `orchestrator` (`claude`,
/// `thread_summary`), `api/integrations` (`voice_stt`, `voice_summary`, `voice_tts`).
/// `telegram_*` ficam por compatibilidade com registros antigos.
const KNOWN_SOURCES: [(&str, &str); 8] = [
    ("voice", "Voz (tempo real)"),
    ("voice_stt", "Voz · transcrição"),
    ("voice_tts", "Voz · áudio (TTS)"),
    ("voice_summary", "Voz · resumo"),
    ("claude", "Claude Code"),
    ("thread_summary", "Resumo de thread"),
    ("telegram_stt", "Telegram · transcrição"),
    ("telegram_tts", "Telegram · áudio"),
];

fn source_label(s: &str) -> &str {
    KNOWN_SOURCES
        .iter()
        .find(|(k, _)| *k == s)
        .map(|(_, l)| tr(l))
        .unwrap_or(s)
}

fn source_tone(s: &str) -> Tone {
    match s {
        "voice" | "voice_stt" | "voice_tts" | "voice_summary" => Tone::Info,
        "claude" | "thread_summary" => Tone::Success,
        _ => Tone::Neutral,
    }
}

/// Opções do filtro: "todas", as fontes conhecidas (ordem fixa) e, no fim, as
/// vistas nos dados que o código não conhece (ordem alfabética, rótulo = valor
/// bruto). Sem duplicatas.
fn source_options(seen: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![(String::new(), tr("todas").to_string())];
    for (k, l) in KNOWN_SOURCES {
        out.push((k.to_string(), tr(l).to_string()));
    }
    let mut extra: Vec<&String> = seen
        .iter()
        .filter(|s| !s.is_empty() && !KNOWN_SOURCES.iter().any(|(k, _)| k == s))
        .collect();
    extra.sort();
    extra.dedup();
    for s in extra {
        out.push((s.clone(), s.clone()));
    }
    out
}

/// Fontes presentes num resumo (`today`/`last_7d`/`last_30d`/`all_time`) ou
/// numa lista de registros.
fn sources_in(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        if !s.is_empty() && !out.iter().any(|x: &String| x == s) {
            out.push(s.to_string());
        }
    };
    if let Some(rows) = v.as_array() {
        for r in rows {
            push(&ui::s(r, "source"));
        }
    } else if let Some(o) = v.as_object() {
        for b in ["today", "last_7d", "last_30d", "all_time"] {
            if let Some(a) = o.get(b).and_then(|x| x.as_array()) {
                for x in a {
                    push(&ui::s(x, "source"));
                }
            }
        }
    }
    out
}

fn unit_label(k: &str) -> &str {
    match k {
        "text_in" => tr("texto in"),
        "text_out" => tr("texto out"),
        "text_cached" => tr("texto cache"),
        "audio_in" => tr("áudio in"),
        "audio_out" => tr("áudio out"),
        "audio_cached" => tr("áudio cache"),
        "input_tokens" => tr("entrada"),
        "output_tokens" => tr("saída"),
        "cache_read_input_tokens" => tr("cache lido"),
        "cache_creation_input_tokens" => tr("cache criado"),
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
        self.summary.start(
            cx.svc
                .call(move |c| async move { c.costs_summary(s1.as_deref()).await }),
        );
        self.rows.start(
            cx.svc
                .call(move |c| async move { c.costs(Some(300), source.as_deref()).await }),
        );
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.summary.stale(std::time::Duration::from_secs(60))
            || self.rows.stale(std::time::Duration::from_secs(60))
        {
            self.reload(cx);
        }
        let got_summary = self.summary.poll();
        let got_rows = self.rows.poll();
        if got_summary || got_rows {
            for v in [&self.summary.data, &self.rows.data].into_iter().flatten() {
                for src in sources_in(v) {
                    if !self.seen_sources.contains(&src) {
                        self.seen_sources.push(src);
                    }
                }
            }
        }

        ui::heading(ui, tr("Custos de IA"), Some(tr("Voz: estimativa pela tabela de preços (Voz › Preços). Claude: valor informado pelo CLI a cada turno.")));

        // ---- filtro (vale para o resumo e para os registros) ----
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr("Fonte")).strong());
            let options = source_options(&self.seen_sources);
            let opts: Vec<(&str, &str)> = options
                .iter()
                .map(|(k, l)| (k.as_str(), l.as_str()))
                .collect();
            if ui::select(ui, "cost-source", &mut self.source, &opts) {
                self.summary.loaded_at = None;
                self.rows.loaded_at = None;
            }
            if ui.button(tr("Atualizar")).clicked() {
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
                tr("Resumo (todas as fontes)")
            } else {
                tr("Resumo (fonte filtrada)")
            },
            |ui| {
                if let Some(e) = &self.summary.error {
                    ui::error_text(ui, e);
                }
                if let Some(s) = &self.summary.data {
                    let periods = [
                        (tr("Hoje"), "today_usd", "today"),
                        (tr("7 dias"), "last_7d_usd", "last_7d"),
                        (tr("30 dias"), "last_30d_usd", "last_30d"),
                        (tr("Total"), "all_time_usd", "all_time"),
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
                                                        egui::RichText::new(trf(
                                                            "{}: US$ {} · {} reg.",
                                                            &[
                                                                &source_label(&ui::s(x, "source")),
                                                                &format!(
                                                                    "{:.4}",
                                                                    x["usd"]
                                                                        .as_f64()
                                                                        .unwrap_or(0.0)
                                                                ),
                                                                &ui::i(x, "count"),
                                                            ],
                                                        ))
                                                        .small()
                                                        .weak(),
                                                    )
                                                    .truncate(),
                                                );
                                            }
                                            if b.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(tr("sem registros"))
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
                    ui::busy(ui, tr("calculando…"));
                }
            },
        );

        // ---- registros ----
        ui::section(ui, tr("Registros"), |ui| {
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
                ui.label(egui::RichText::new(tr("Nenhum custo registrado ainda. Use a voz ou mande uma mensagem a uma thread.")).weak());
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
                                    tr("estimado")
                                } else {
                                    tr("informado")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_known_first_then_unknown_sorted_dedup() {
        let seen = vec![
            "zeta".to_string(),
            "claude".to_string(),
            "alpha".to_string(),
            "zeta".to_string(),
            String::new(),
        ];
        let o = source_options(&seen);
        assert_eq!(o[0].0, "");
        let known: Vec<&str> = o[1..=KNOWN_SOURCES.len()]
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(known[0], "voice");
        assert!(known.contains(&"thread_summary"));
        let tail: Vec<&str> = o[KNOWN_SOURCES.len() + 1..]
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(tail, vec!["alpha", "zeta"]);
        assert_eq!(o.iter().filter(|(k, _)| k == "claude").count(), 1);
        // desconhecida: rótulo = valor bruto
        assert_eq!(o.last().map(|(_, l)| l.as_str()), Some("zeta"));
    }

    #[test]
    fn sources_in_summary_and_rows() {
        let summary = serde_json::json!({
            "today": [{"source": "voice"}],
            "last_7d": [{"source": "claude"}, {"source": "voice"}],
            "last_30d": [],
            "all_time": [{"source": "voice_tts"}]
        });
        assert_eq!(sources_in(&summary), vec!["voice", "claude", "voice_tts"]);
        let rows = serde_json::json!([{"source": "thread_summary"}, {"source": "thread_summary"}, {"source": ""}]);
        assert_eq!(sources_in(&rows), vec!["thread_summary"]);
        assert_eq!(source_label("voice_tts"), "Voz · áudio (TTS)");
        assert_eq!(source_label("misterio"), "misterio");
    }
}
