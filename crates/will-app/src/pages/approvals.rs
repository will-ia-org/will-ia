//! Aprovações: pedidos pendentes (um cartão por pedido, com contexto completo
//! para decidir) e histórico em tabela de largura inteira com filtros.

use super::Cx;
use crate::i18n::{tr, trf};
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use egui::RichText;
use serde_json::Value;
use std::collections::HashMap;
use will_core::{ApprovalRequest, Event, ThreadId, ThreadSummary};

/// Largura da coluna de chaves na lista de argumentos do cartão.
const ARG_KEY_WIDTH: f32 = 150.0;
/// Acima disso o valor é cortado no cartão (o JSON completo fica em "Detalhes").
const ARG_VALUE_MAX_CHARS: usize = 600;
/// Altura máxima da tabela do histórico antes de rolar.
const HISTORY_MAX_HEIGHT: f32 = 460.0;

#[derive(Default)]
pub struct ApprovalsPage {
    history: Loader<Value>,
    /// Decisões em voo: (id do pedido, tarefa).
    decide: Vec<(String, Task<Value>)>,
    /// Lista de threads só para traduzir `thread_id` em título/origem.
    threads: Loader<Value>,
    thread_names: HashMap<ThreadId, ThreadSummary>,
    filter_text: String,
    filter_decision: String,
}

impl ApprovalsPage {
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::ApprovalDecided { .. } => self.history.loaded_at = None,
            // Um pedido novo pode vir de uma thread recém-criada: recarregar nomes.
            Event::ApprovalRequested { .. } => self.threads.loaded_at = None,
            _ => {}
        }
    }

    fn in_flight(&self, id: &str) -> bool {
        self.decide.iter().any(|(i, t)| i == id && t.pending())
    }

    fn send_decision(&mut self, cx: &Cx, id: String, decision: &'static str) {
        let id2 = id.clone();
        let task = cx.svc.call(move |c| async move {
            c.decide_approval(&id2, serde_json::json!({ "decision": decision }))
                .await
        });
        self.decide.push((id, task));
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.history.stale(std::time::Duration::from_secs(60)) {
            self.history
                .start(cx.svc.call(|c| async move {
                    c.approvals_history(&[("limit", "100".into())]).await
                }));
        }
        self.history.poll();

        if self.threads.stale(std::time::Duration::from_secs(120)) {
            self.threads
                .start(cx.svc.call(|c| async move { c.threads().await }));
        }
        if self.threads.poll() {
            if let Some(v) = &self.threads.data {
                if let Ok(list) = serde_json::from_value::<Vec<ThreadSummary>>(v.clone()) {
                    self.thread_names = list.into_iter().map(|t| (t.id, t)).collect();
                }
            }
        }

        let mut errors = Vec::new();
        self.decide.retain(|(_, t)| match t.take() {
            Some(Err(e)) => {
                errors.push(e);
                false
            }
            Some(Ok(_)) => false,
            None => true,
        });
        for e in errors {
            cx.toasts.error(e);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);

        ui::heading(
            ui,
            tr("Aprovações"),
            Some(tr("Ferramentas com efeito externo pedem confirmação. Sem decisão até o tempo limite, o pedido é negado.")),
        );

        // `snap` é `&'a Snapshot` (Copy): copiar a referência solta o empréstimo de `cx`.
        let snap = cx.snap;
        let pending_count = snap.pending_approvals.len();
        let title = if pending_count == 0 {
            tr("Pendentes").to_string()
        } else {
            trf("Pendentes ({})", &[&pending_count])
        };
        ui::section(ui, &title, |ui| {
            ui.set_width(ui.available_width());
            if snap.pending_approvals.is_empty() {
                ui.label(RichText::new(tr("Nenhuma aprovação pendente.")).weak());
                return;
            }
            // Contagem regressiva ao vivo.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
            for a in &snap.pending_approvals {
                self.pending_card(ui, cx, a);
                ui.add_space(6.0);
            }
        });

        ui::section(ui, tr("Histórico"), |ui| {
            ui.set_width(ui.available_width());
            self.history_ui(ui);
        });
    }

    // ---------- pendentes ----------

    fn pending_card(&mut self, ui: &mut egui::Ui, cx: &Cx, a: &ApprovalRequest) {
        let dark = ui.visuals().dark_mode;
        let p = ui::palette(dark);
        let id = a.id.to_string();
        let busy = self.in_flight(&id);

        let elapsed = (chrono::Utc::now() - a.requested_at).num_seconds().max(0) as u64;
        let left = a.timeout_secs.saturating_sub(elapsed);
        let urgent = left < 60;

        egui::Frame::new()
            .fill(p.card_alt)
            .stroke(egui::Stroke::new(1.5, p.accent))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                // Linha 1: ferramenta + contagem regressiva.
                ui.horizontal(|ui| {
                    ui::badge(ui, &tool_label(&a.tool), Tone::Warning);
                    ui.label(RichText::new(&a.tool).small().weak().monospace())
                        .on_hover_text(tr("Nome da ferramenta MCP"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let tone = if urgent { Tone::Danger } else { Tone::Warning };
                        let txt = if left == 0 {
                            tr("expirando…").to_string()
                        } else {
                            trf("expira em {}", &[&fmt_countdown(left)])
                        };
                        ui.label(
                            RichText::new(txt)
                                .color(tone.color(dark))
                                .font(ui::medium(13.0)),
                        )
                        .on_hover_text(trf(
                            "Tempo limite de {} · sem decisão o pedido é negado",
                            &[&fmt_countdown(a.timeout_secs)],
                        ));
                    });
                });

                // Linha 2: resumo gerado pelo serviço, em destaque.
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(RichText::new(&a.summary).font(ui::medium(15.5)))
                        .wrap()
                        .selectable(true),
                );
                ui.add_space(4.0);

                // Linha 3: origem e quando.
                ui.horizontal_wrapped(|ui| {
                    let (who, origin, hover) = self.requester(a.thread_id);
                    ui.label(RichText::new(tr("Pedido por")).small().weak());
                    ui.label(RichText::new(who).small().font(ui::medium(12.0)))
                        .on_hover_text(hover);
                    if let Some(o) = origin {
                        ui::badge(ui, &o, Tone::Info);
                    }
                    ui.label(RichText::new("·").small().weak());
                    ui.label(RichText::new(rel_label(&a.requested_at)).small().weak())
                        .on_hover_text(fmt_exact(&a.requested_at));
                });

                // Argumentos em chave/valor.
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                args_list(ui, &a.args_redacted);
                let pretty = serde_json::to_string_pretty(&a.args_redacted).unwrap_or_default();
                egui::CollapsingHeader::new(RichText::new(tr("Detalhes (JSON completo)")).small())
                    .id_salt(("approval-details", &id))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::Frame::new()
                            .fill(ui.visuals().code_bg_color)
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui::mono(ui, &pretty);
                            });
                    });

                // Botões de decisão.
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if busy {
                        ui::busy(ui, tr("enviando decisão…"));
                    } else {
                        ui.label(
                            RichText::new(trf("pedido {}", &[&&id[..8]]))
                                .small()
                                .weak()
                                .monospace(),
                        )
                        .on_hover_text(&id);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let approve = big_button(ui, tr("Aprovar"), p.accent, !busy)
                            .on_hover_text(tr("Executa a ferramenta com estes argumentos"));
                        let deny = big_button(ui, tr("Negar"), Tone::Danger.color(dark), !busy)
                            .on_hover_text(tr("O agente recebe a negativa e segue sem executar"));
                        if approve.clicked() {
                            self.send_decision(cx, id.clone(), "approved");
                        } else if deny.clicked() {
                            self.send_decision(cx, id.clone(), "denied");
                        }
                    });
                });
            });
    }

    /// (quem pediu, canal de origem, texto do hover) para o `thread_id` do pedido.
    fn requester(&self, thread_id: Option<ThreadId>) -> (String, Option<String>, String) {
        let Some(tid) = thread_id else {
            return (
                tr("serviço").to_string(),
                None,
                tr("Pedido sem thread associada").to_string(),
            );
        };
        match self.thread_names.get(&tid) {
            Some(t) => (
                thread_title(t),
                Some(origin_label(t)),
                format!("thread {tid}"),
            ),
            None => (
                format!("Thread {}", &tid.to_string()[..8]),
                None,
                format!("thread {tid}"),
            ),
        }
    }

    // ---------- histórico ----------

    fn history_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.history.error {
            ui::error_text(ui, e);
        }
        let rows: Vec<Value> = self
            .history
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Filtros + atualizar.
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.filter_text)
                    .hint_text(tr("Filtrar por resumo ou ferramenta"))
                    .desired_width(280.0),
            );
            ui::text_context_menu(&r);
            if !self.filter_text.is_empty() && ui.button("✕").on_hover_text(tr("Limpar")).clicked()
            {
                self.filter_text.clear();
            }
            ui::select(
                ui,
                "approvals-history-decision",
                &mut self.filter_decision,
                &[
                    ("", tr("Todas as decisões")),
                    ("approved", tr("Aprovadas")),
                    ("denied", tr("Negadas")),
                    ("timeout", tr("Expiradas")),
                    ("pending", tr("Sem decisão")),
                ],
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.history.loading() {
                    ui.spinner();
                } else if ui.button(tr("Atualizar")).clicked() {
                    self.history.loaded_at = None;
                }
            });
        });

        if rows.is_empty() {
            if self.history.loading() {
                ui::busy(ui, tr("carregando histórico…"));
            } else {
                ui.label(RichText::new(tr("Sem decisões registradas.")).weak());
            }
            return;
        }

        let needle = self.filter_text.trim().to_lowercase();
        let filtered: Vec<&Value> = rows
            .iter()
            .filter(|r| {
                let d = ui::s(r, "decision");
                let decision_ok = match self.filter_decision.as_str() {
                    "" => true,
                    "pending" => d.is_empty(),
                    f => d == f,
                };
                if !decision_ok {
                    return false;
                }
                if needle.is_empty() {
                    return true;
                }
                let tool = ui::s(r, "tool");
                ui::s(r, "summary").to_lowercase().contains(&needle)
                    || tool.to_lowercase().contains(&needle)
                    || tool_label(&tool).to_lowercase().contains(&needle)
            })
            .collect();

        ui.label(
            RichText::new(if filtered.len() == rows.len() {
                trf("{} registros", &[&rows.len()])
            } else {
                trf("{} de {} registros", &[&filtered.len(), &rows.len()])
            })
            .small()
            .weak(),
        );

        if filtered.is_empty() {
            ui.label(RichText::new(tr("Nenhum registro corresponde ao filtro.")).weak());
            return;
        }

        ui.set_width(ui.available_width());
        let dark = ui.visuals().dark_mode;
        let row_h = 26.0;
        egui_extras::TableBuilder::new(ui)
            .id_salt("approvals-history-table")
            .striped(true)
            .resizable(true)
            .vscroll(true)
            .max_scroll_height(HISTORY_MAX_HEIGHT)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(egui_extras::Column::auto().at_least(80.0))
            .column(egui_extras::Column::auto().at_least(140.0))
            .column(egui_extras::Column::remainder().at_least(160.0).clip(true))
            .column(egui_extras::Column::auto().at_least(84.0))
            .column(egui_extras::Column::auto().at_least(80.0))
            .header(24.0, |mut header| {
                for name in [
                    tr("Quando"),
                    tr("Ferramenta"),
                    tr("Resumo"),
                    tr("Decisão"),
                    tr("Por"),
                ] {
                    header.col(|ui| {
                        ui.label(RichText::new(name).font(ui::medium(12.5)).weak());
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, filtered.len(), |mut row| {
                    let r = filtered[row.index()];
                    let requested = parse_dt(&r["requested_at"]);
                    let decided = parse_dt(&r["decided_at"]);
                    row.col(|ui| {
                        let txt = requested
                            .as_ref()
                            .map(rel_label)
                            .unwrap_or_else(|| ui::fmt_dt(&r["requested_at"]));
                        let mut hover = requested
                            .as_ref()
                            .map(|d| trf("pedido em {}", &[&fmt_exact(d)]))
                            .unwrap_or_default();
                        if let Some(d) = &decided {
                            hover.push('\n');
                            hover.push_str(&trf("decidido em {}", &[&fmt_exact(d)]));
                        }
                        ui.label(RichText::new(txt).small()).on_hover_text(hover);
                    });
                    row.col(|ui| {
                        let tool = ui::s(r, "tool");
                        ui.add(
                            egui::Label::new(RichText::new(tool_label(&tool)).small()).truncate(),
                        )
                        .on_hover_text(&tool);
                    });
                    row.col(|ui| {
                        let summary = ui::s(r, "summary");
                        let resp = ui.add(egui::Label::new(&summary).truncate());
                        resp.on_hover_ui(|ui| {
                            ui.set_max_width(520.0);
                            ui::wrapped_text(ui, &summary);
                            let args = &r["args_redacted"];
                            if !args.is_null() {
                                ui.add_space(4.0);
                                ui::mono(
                                    ui,
                                    &serde_json::to_string_pretty(args).unwrap_or_default(),
                                );
                            }
                        });
                    });
                    row.col(|ui| {
                        let d = ui::s(r, "decision");
                        let (label, tone) = decision_badge(&d);
                        ui::badge(ui, label, tone);
                    });
                    row.col(|ui| {
                        let via = ui::s(r, "via");
                        let via = if via.is_empty() {
                            ui::s(r, "decided_via")
                        } else {
                            via
                        };
                        ui.label(
                            RichText::new(via_label(&via))
                                .small()
                                .color(if via.is_empty() {
                                    Tone::Neutral.color(dark)
                                } else {
                                    ui.visuals().text_color()
                                }),
                        )
                        .on_hover_text(if via.is_empty() {
                            "—"
                        } else {
                            via.as_str()
                        });
                    });
                });
            });
    }
}

// ---------- helpers privados ----------

/// Nome legível da ferramenta (fallback: nome bruto).
fn tool_label(tool: &str) -> String {
    let s = match tool {
        "telegram_send_message" | "telegram_send" => "Telegram · enviar mensagem",
        "telegram_send_file" | "telegram_send_document" => "Telegram · enviar arquivo",
        "whatsapp_send_message" | "whatsapp_send" => "WhatsApp · enviar mensagem",
        "whatsapp_send_media" => "WhatsApp · enviar mídia",
        "whatsapp_mark_read" => "WhatsApp · marcar como lida",
        "gmail_send" => "Gmail · enviar e-mail",
        "gmail_create_draft" => "Gmail · criar rascunho",
        "gmail_modify" => "Gmail · alterar labels / arquivar",
        "gmail_search" => "Gmail · buscar",
        "gmail_get_message" => "Gmail · ler e-mail",
        "gmail_get_thread" => "Gmail · ler conversa",
        "gmail_download_attachment" => "Gmail · baixar anexo",
        "gmail_list_labels" => "Gmail · listar labels",
        "calendar_create_event" => "Agenda · criar evento",
        "calendar_update_event" => "Agenda · alterar evento",
        "calendar_delete_event" => "Agenda · apagar evento",
        "calendar_list_events" => "Agenda · listar eventos",
        "calendar_get_event" => "Agenda · ler evento",
        "calendar_free_busy" => "Agenda · disponibilidade",
        "calendar_list_calendars" => "Agenda · listar agendas",
        "notify_user" => "Notificar você",
        other => return other.to_string(),
    };
    tr(s).to_string()
}

/// Rótulo legível (traduzido) para chaves comuns dos argumentos.
fn arg_key_label(key: &str) -> &str {
    let pt = match key {
        "to" | "recipient" | "recipients" => "para",
        "cc" => "cc",
        "bcc" => "cco",
        "subject" => "assunto",
        "body" | "text" | "message" | "content" => "texto",
        "html" => "html",
        "chat_id" | "chat" => "chat",
        "message_thread_id" | "topic_id" => "tópico",
        "reply_to" | "reply_to_message_id" => "responder a",
        "title" | "summary" => "título",
        "description" => "descrição",
        "location" => "local",
        "start" | "start_time" => "início",
        "end" | "end_time" => "fim",
        "attendees" => "convidados",
        "calendar_id" => "agenda",
        "event_id" => "evento",
        "message_id" | "id" => "id",
        "thread_id" => "conversa",
        "add_labels" => "adicionar labels",
        "remove_labels" => "remover labels",
        "archive" => "arquivar",
        "mark_read" | "read" => "marcar lida",
        "file" | "path" | "file_path" | "media" => "arquivo",
        "caption" => "legenda",
        "account" | "account_id" => "conta",
        "level" | "urgency" => "nível",
        other => return other,
    };
    tr(pt)
}

/// Valor de argumento em texto corrido (strings como estão, listas de strings
/// separadas por vírgula, o resto em JSON compacto).
fn arg_value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "—".into(),
        Value::Bool(true) => tr("sim").into(),
        Value::Bool(false) => tr("não").into(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) if items.iter().all(|x| x.is_string()) => items
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Lista chave/valor dos argumentos (valores longos cortados; JSON completo
/// fica em "Detalhes").
fn args_list(ui: &mut egui::Ui, args: &Value) {
    let Some(obj) = args.as_object() else {
        if !args.is_null() {
            ui::mono(ui, &serde_json::to_string_pretty(args).unwrap_or_default());
        } else {
            ui.label(RichText::new(tr("Sem argumentos.")).small().weak());
        }
        return;
    };
    if obj.is_empty() {
        ui.label(RichText::new(tr("Sem argumentos.")).small().weak());
        return;
    }
    for (k, v) in obj {
        let full = arg_value_text(v);
        let (text, cut) = if full.chars().count() > ARG_VALUE_MAX_CHARS {
            (ui::ellipsize(&full, ARG_VALUE_MAX_CHARS), true)
        } else {
            (full, false)
        };
        let is_block = text.contains('\n') || text.chars().count() > 120;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(ARG_KEY_WIDTH);
                ui.set_max_width(ARG_KEY_WIDTH);
                let label = arg_key_label(k);
                let r = ui.add(
                    egui::Label::new(RichText::new(label).small().weak().font(ui::medium(12.5)))
                        .truncate(),
                );
                if label != k {
                    r.on_hover_text(k);
                }
            });
            ui.vertical(|ui| {
                let w = ui.available_width();
                ui.set_width(w);
                ui.set_max_width(w);
                if is_block {
                    let dark = ui.visuals().dark_mode;
                    let p = ui::palette(dark);
                    egui::Frame::new()
                        .fill(p.card)
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui::wrapped_text(ui, &text);
                        });
                } else {
                    ui::wrapped_text(ui, &text);
                }
                if cut {
                    ui.label(
                        RichText::new(tr("valor cortado · veja o JSON completo em Detalhes"))
                            .small()
                            .weak(),
                    );
                }
            });
        });
    }
}

/// Botão grande de decisão (branco sobre `fill`), desabilitado quando `enabled` é falso.
fn big_button(ui: &mut egui::Ui, text: &str, fill: egui::Color32, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(
            RichText::new(text)
                .color(egui::Color32::WHITE)
                .font(ui::medium(14.0)),
        )
        .fill(fill)
        .min_size(egui::vec2(116.0, 34.0)),
    )
}

fn fmt_countdown(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn fmt_exact(d: &chrono::DateTime<chrono::Utc>) -> String {
    d.with_timezone(&chrono::Local)
        .format("%d/%m/%Y %H:%M:%S")
        .to_string()
}

/// "agora" ou "há 5 min" / "há 2 h" / "há 3 d".
fn rel_label(d: &chrono::DateTime<chrono::Utc>) -> String {
    let r = ui::fmt_relative(d);
    if r == tr("agora") {
        r
    } else {
        trf("há {}", &[&r])
    }
}

fn parse_dt(v: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
}

fn decision_badge(d: &str) -> (&'static str, Tone) {
    match d {
        "approved" => (tr("aprovada"), Tone::Success),
        "denied" => (tr("negada"), Tone::Danger),
        "timeout" => (tr("expirada"), Tone::Warning),
        "" => (tr("pendente"), Tone::Info),
        _ => ("?", Tone::Neutral),
    }
}

fn via_label(via: &str) -> String {
    match via {
        "" => "—".into(),
        "ui" | "app" => "app".into(),
        "telegram" => tr("Telegram").into(),
        "notification" | "notify" => tr("notificação").into(),
        "timeout" => tr("tempo limite").into(),
        other => other.to_string(),
    }
}

fn thread_title(t: &ThreadSummary) -> String {
    t.title
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("Thread {}", &t.id.to_string()[..8]))
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
