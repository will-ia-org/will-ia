//! Seção WhatsApp: várias contas pareadas (QR desenhado na própria janela ou
//! código), desvincular, reconectar, configuração das ferramentas.

use super::SettingsState;
use crate::help;
use crate::pages::Cx;
use crate::service::{j, Loader, Task};
use crate::ui::{self, Tone};
use axum::extract::{Path, State};
use axum::Json;
use serde_json::Value;
use std::collections::HashMap;
use will_core::Event;
use will_server::api::integrations as api;

#[derive(Default)]
pub struct WhatsAppPage {
    status: Loader<Value>,
    pair_task: Option<Task<Value>>,
    action_task: Option<(String, Task<Value>)>,
    show_code_pair: Option<String>,
    phone: String,
    new_label: String,
    confirm_unlink: Option<(String, String)>,
    clear_index: bool,
    remove_account: bool,
    rename: Option<(String, String)>,
    /// QRs renderizados por conta (cache por `at`).
    qr_cache: HashMap<String, (String, Vec<bool>, usize)>,
}

impl WhatsAppPage {
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::WhatsApp { .. } => self.status.loaded_at = None,
            Event::SettingsChanged { section } if section == "whatsapp" => {
                self.status.loaded_at = None
            }
            _ => {}
        }
    }

    fn reload(&mut self, cx: &Cx) {
        self.status.start(
            cx.svc
                .call(|s| async move { j(api::wa_status(State(s)).await) }),
        );
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.status.stale(std::time::Duration::from_secs(10)) {
            self.reload(cx);
        }
        self.status.poll();
        if let Some(t) = &self.pair_task {
            if let Some(r) = t.take() {
                self.pair_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts
                            .info("Pareamento iniciado; o QR/código aparece abaixo em instantes.");
                        self.show_code_pair = None;
                        self.status.loaded_at = None;
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
                        self.status.loaded_at = None;
                    }
                    Err(e) => cx.toasts.error(format!("{what}: {e}")),
                }
            }
        }
    }

    fn qr_modules(&mut self, account: &str, qr: &Value) -> Option<(Vec<bool>, usize)> {
        let data = qr.get("data")?.as_str()?;
        let at = ui::s(qr, "at");
        if let Some((cached_at, m, w)) = self.qr_cache.get(account) {
            if cached_at == &at {
                return Some((m.clone(), *w));
            }
        }
        let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
        let w = code.width();
        let modules: Vec<bool> = code
            .to_colors()
            .into_iter()
            .map(|c| c == qrcode::Color::Dark)
            .collect();
        self.qr_cache
            .insert(account.to_string(), (at, modules.clone(), w));
        Some((modules, w))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx);
        ui::heading_help(ui, "WhatsApp", Some("Contas pessoais pareadas como aparelho conectado; expostas aos agentes só como ferramentas whatsapp_* (não é canal de entrada)."), &help::WHATSAPP);

        let enabled_saved = st.saved.whatsapp.enabled;
        let status = self.status.data.clone();
        let accounts: Vec<Value> = status
            .as_ref()
            .and_then(|s| s.get("accounts"))
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();

        ui::section(ui, "Contas", |ui| {
            if let Some(e) = &self.status.error {
                ui::error_text(ui, e);
            }
            if let Some(s) = &status {
                ui.horizontal(|ui| {
                    let m = cx
                        .snap
                        .modules
                        .get(&will_core::Module::WhatsApp)
                        .cloned()
                        .unwrap_or(will_core::ModuleStatus::Stopped);
                    ui::badge(ui, &ui::module_label(&m), ui::tone_for_module(&m));
                    ui.label(
                        egui::RichText::new(format!(
                            "backend {} · índice total: {} chats, {} mensagens",
                            ui::s(s, "backend"),
                            ui::i(&s["index_total"], "chats"),
                            ui::i(&s["index_total"], "messages")
                        ))
                        .small()
                        .weak(),
                    );
                    if ui.button("atualizar").clicked() {
                        self.status.loaded_at = None;
                    }
                });
            }
            if !enabled_saved {
                ui.label(
                    egui::RichText::new(
                        "Habilite o módulo na configuração abaixo e salve para parear contas.",
                    )
                    .weak(),
                );
            }
            if accounts.is_empty() {
                ui.label(egui::RichText::new("Nenhuma conta pareada.").weak());
            }
            for a in &accounts {
                let id = ui::s(a, "id");
                let label = ui::s(a, "label");
                let state = ui::s(a, "state");
                let busy = self.action_task.is_some() || self.pair_task.is_some();
                egui::Frame::group(ui.style()).corner_radius(8.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let tone = match state.as_str() {
                            "connected" => Tone::Success,
                            "qr" | "code" => Tone::Info,
                            "degraded" | "logged_out" => Tone::Warning,
                            _ => Tone::Neutral,
                        };
                        ui::dot(ui, tone);
                        match &mut self.rename {
                            Some((rid, buf)) if rid == &id => {
                                ui.add(egui::TextEdit::singleline(buf).desired_width(200.0));
                                if ui.button("OK").clicked() {
                                    let new_label = buf.clone();
                                    let id2 = id.clone();
                                    self.action_task = Some(("Renomear".into(), cx.svc.call(move |s| async move { j(api::wa_account_patch(State(s), Path(id2), Some(Json(api::AccountPatch { label: Some(new_label), enabled: None }))).await) })));
                                    self.rename = None;
                                } else if ui.button("cancelar").clicked() {
                                    self.rename = None;
                                }
                            }
                            _ => {
                                ui.label(egui::RichText::new(&label).strong());
                                if ui.button("renomear").clicked() {
                                    self.rename = Some((id.clone(), label.clone()));
                                }
                            }
                        }
                        if let Some(p) = a.get("phone").and_then(|p| p.as_str()) {
                            ui.label(egui::RichText::new(format!("+{p}")).weak());
                        }
                        ui::badge(ui, &match state.as_str() {
                            "connected" => "conectado".to_string(),
                            "qr" => "aguardando QR".to_string(),
                            "code" => "aguardando código".to_string(),
                            "logged_out" => "sessão revogada".to_string(),
                            "degraded" => format!("degradado · {}", ui::s(a, "detail")),
                            "disconnected" => if ui::b(a, "has_session") { "desconectado".to_string() } else { "não pareado".to_string() },
                            other => other.to_string(),
                        }, tone);
                        if let Some(me) = a.get("me") {
                            if let Some(n) = me.get("push_name").and_then(|n| n.as_str()) {
                                ui.label(egui::RichText::new(n).small().weak());
                            }
                        }
                        let idx = &a["index"];
                        ui.label(egui::RichText::new(format!("{} chats · {} msgs", ui::i(idx, "chats"), ui::i(idx, "messages"))).small().weak());
                    });
                    ui.horizontal(|ui| {
                        let mut en = ui::b(a, "enabled");
                        if ui.checkbox(&mut en, "usar nas ferramentas").changed() {
                            let id2 = id.clone();
                            self.action_task = Some(("Conta".into(), cx.svc.call(move |s| async move { j(api::wa_account_patch(State(s), Path(id2), Some(Json(api::AccountPatch { label: None, enabled: Some(en) }))).await) })));
                        }
                        if state != "connected" {
                            if ui.add_enabled(!busy && enabled_saved, egui::Button::new("Parear por QR")).clicked() {
                                let id2 = id.clone();
                                self.pair_task = Some(cx.svc.call(move |s| async move { j(api::wa_pair(State(s), Some(Json(api::PairBody { account_id: Some(id2), phone: None, label: None }))).await) }));
                            }
                            if ui.add_enabled(!busy && enabled_saved, egui::Button::new("Parear por código")).clicked() {
                                self.show_code_pair = Some(id.clone());
                            }
                            if ui::b(a, "has_session") && ui.add_enabled(!busy, egui::Button::new("Reconectar")).clicked() {
                                let id2 = id.clone();
                                self.action_task = Some(("Reconectar".into(), cx.svc.call(move |s| async move { j(api::wa_reconnect(State(s), Json(api::AccountIdBody { account_id: id2 })).await) })));
                            }
                        }
                        if ui.add_enabled(!busy, egui::Button::new("Desvincular / remover")).clicked() {
                            self.confirm_unlink = Some((id.clone(), label.clone()));
                            self.clear_index = false;
                            self.remove_account = !ui::b(a, "has_session");
                        }
                    });
                    // QR / código
                    if state == "qr" {
                        if let Some(qr) = a.get("last_qr").filter(|q| !q.is_null()).cloned() {
                            ui.label(egui::RichText::new("No telefone: WhatsApp › Aparelhos conectados › Conectar aparelho, e aponte para o código:").small());
                            if let Some((modules, w)) = self.qr_modules(&id, &qr) {
                                draw_qr(ui, &modules, w, 264.0);
                            }
                            ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
                        } else {
                            ui::busy(ui, "gerando QR…");
                        }
                    }
                    if state == "code" {
                        if let Some(code) = a.get("last_pair_code").and_then(|c| c.as_str()) {
                            ui.label(egui::RichText::new("No telefone: Aparelhos conectados › Conectar com número de telefone, e digite:").small());
                            ui.label(egui::RichText::new(code).monospace().size(28.0));
                        } else {
                            ui::busy(ui, "gerando código…");
                        }
                    }
                });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let busy = self.pair_task.is_some();
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_label)
                        .hint_text("rótulo da nova conta (opcional)")
                        .desired_width(220.0),
                );
                if ui
                    .add_enabled(
                        !busy && enabled_saved,
                        egui::Button::new(
                            egui::RichText::new("+ Nova conta por QR").color(egui::Color32::WHITE),
                        )
                        .fill(Tone::Success.color(true)),
                    )
                    .clicked()
                {
                    let label = if self.new_label.trim().is_empty() {
                        None
                    } else {
                        Some(self.new_label.trim().to_string())
                    };
                    self.new_label.clear();
                    self.pair_task = Some(cx.svc.call(move |s| async move {
                        j(api::wa_pair(
                            State(s),
                            Some(Json(api::PairBody {
                                account_id: None,
                                phone: None,
                                label,
                            })),
                        )
                        .await)
                    }));
                }
                if ui
                    .add_enabled(
                        !busy && enabled_saved,
                        egui::Button::new("+ Nova conta por código"),
                    )
                    .clicked()
                {
                    self.show_code_pair = Some(String::new());
                }
                if busy {
                    ui.spinner();
                }
            });
            if let Some(target) = self.show_code_pair.clone() {
                ui.horizontal(|ui| {
                    ui.label("Telefone com DDI, só dígitos:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.phone)
                            .hint_text("5511999998888")
                            .desired_width(160.0),
                    );
                    self.phone.retain(|c| c.is_ascii_digit());
                    let ok = self.phone.len() >= 8;
                    if ui
                        .add_enabled(ok, egui::Button::new("Gerar código"))
                        .clicked()
                    {
                        let phone = Some(self.phone.clone());
                        let account_id = if target.is_empty() {
                            None
                        } else {
                            Some(target.clone())
                        };
                        let label = if self.new_label.trim().is_empty() {
                            None
                        } else {
                            Some(self.new_label.trim().to_string())
                        };
                        self.pair_task = Some(cx.svc.call(move |s| async move {
                            j(api::wa_pair(
                                State(s),
                                Some(Json(api::PairBody {
                                    account_id,
                                    phone,
                                    label,
                                })),
                            )
                            .await)
                        }));
                    }
                    if ui.button("Cancelar").clicked() {
                        self.show_code_pair = None;
                    }
                });
            }
        });

        if let Some((id, label)) = self.confirm_unlink.clone() {
            let mut decided = None;
            let m = egui::Modal::new(egui::Id::unique(("wa-unlink", &id))).show(ui.ctx(), |ui| {
                ui.set_width(440.0);
                ui.heading(format!("Desvincular '{label}'"));
                ui.label("Faz logout remoto (o telefone deixa de listar este aparelho) e apaga a sessão local.");
                ui.checkbox(&mut self.clear_index, "Apagar também o índice de mensagens desta conta");
                ui.checkbox(&mut self.remove_account, "Remover a conta da lista");
                ui.horizontal(|ui| {
                    if ui.button("Cancelar").clicked() {
                        decided = Some(false);
                    }
                    if ui::danger_button(ui, "Desvincular").clicked() {
                        decided = Some(true);
                    }
                });
            });
            if m.should_close() && decided.is_none() {
                decided = Some(false);
            }
            if let Some(ok) = decided {
                self.confirm_unlink = None;
                if ok {
                    let (clear, remove) = (self.clear_index, self.remove_account);
                    self.action_task = Some((
                        "Desvincular".into(),
                        cx.svc.call(move |s| async move {
                            j(api::wa_unlink(
                                State(s),
                                Json(api::UnlinkBody {
                                    account_id: id,
                                    clear_index: Some(clear),
                                    remove: Some(remove),
                                }),
                            )
                            .await)
                        }),
                    ));
                }
            }
        }

        ui::section(ui, "Configuração", |ui| {
            let w = &mut st.draft.whatsapp;
            ui::field(
                ui,
                "Habilitar módulo",
                Some("Sem isso nenhuma conta conecta e as ferramentas ficam indisponíveis."),
                |ui| {
                    ui::toggle(ui, &mut w.enabled);
                },
            );
            ui::field(
                ui,
                "Retenção do índice (dias)",
                Some("Mensagens mais antigas são apagadas do índice local."),
                |ui| {
                    ui::number(ui, &mut w.retention_days, 1..=3650);
                },
            );
            ui::field(
                ui,
                "Limite de envio",
                Some("Mensagens por janela de minutos, por conta."),
                |ui| {
                    ui.horizontal(|ui| {
                        ui::number(ui, &mut w.send_rate_limit_count, 1..=1000);
                        ui.label("msgs /");
                        ui::number(ui, &mut w.send_rate_limit_minutes, 1..=1440);
                        ui.label("min");
                    });
                },
            );
            ui::field(
                ui,
                "Permitir números não salvos",
                Some("Envio para telefones que não estão nos contatos."),
                |ui| {
                    ui::toggle(ui, &mut w.allow_unsaved_numbers);
                },
            );
            ui::field(ui, "Recibo de leitura em mark_read", None, |ui| {
                ui::toggle(ui, &mut w.send_read_receipts);
            });
            ui::field(
                ui,
                "Pasta de mídia",
                Some("Relativa à base de conhecimento."),
                |ui| {
                    ui::text_input(ui, &mut w.media_dir, "inbox/whatsapp");
                },
            );
        });
        st.save_bar(ui, cx.svc, "whatsapp");
    }
}

/// Desenha o QR com retângulos (sem imagem/SVG), com zona de silêncio branca.
fn draw_qr(ui: &mut egui::Ui, modules: &[bool], width: usize, size_px: f32) {
    let quiet = 4.0;
    let cell = size_px / (width as f32 + quiet * 2.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size_px, size_px), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 6.0, egui::Color32::WHITE);
    let origin = rect.min + egui::vec2(quiet * cell, quiet * cell);
    for y in 0..width {
        for x in 0..width {
            if modules[y * width + x] {
                let min = origin + egui::vec2(x as f32 * cell, y as f32 * cell);
                p.rect_filled(
                    egui::Rect::from_min_size(min, egui::vec2(cell + 0.5, cell + 0.5)),
                    0.0,
                    egui::Color32::BLACK,
                );
            }
        }
    }
}
