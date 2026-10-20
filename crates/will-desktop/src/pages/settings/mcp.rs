//! Seção Servidor MCP: status/URL do servidor local, configuração, políticas
//! por ferramenta (auto / confirmar / desligada) e chamadas recentes.

use super::SettingsState;
use crate::pages::Cx;
use crate::service::{j, Loader, Task};
use crate::ui::{self, Tone};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::Value;
use std::collections::BTreeMap;
use will_core::settings::ToolPolicySetting;
use will_core::{Event, Module};
use will_server::api::mcp as api;

#[derive(Default)]
pub struct McpPage {
    tools: Loader<Value>,
    calls: Loader<Value>,
    policy_task: Option<(String, Task<Value>)>,
}

fn policy_label(p: &str) -> &'static str {
    match p {
        "auto" => "automático",
        "confirm" => "confirmar",
        "off" => "desligada",
        _ => "padrão",
    }
}

fn family_label(f: &str) -> String {
    match f {
        "telegram" => "Telegram".into(),
        "whatsapp" => "WhatsApp".into(),
        "calendar" => "Agenda".into(),
        "gmail" => "Gmail".into(),
        "core" => "Sistema".into(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

impl McpPage {
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::McpToolCalled { .. } => self.calls.loaded_at = None,
            Event::SettingsChanged { section } if section == "mcp" => self.tools.loaded_at = None,
            _ => {}
        }
    }

    fn reload_tools(&mut self, cx: &Cx) {
        self.tools.start(
            cx.svc
                .call(|s| async move { j(api::tools(State(s)).await) }),
        );
    }

    fn reload_calls(&mut self, cx: &Cx) {
        self.calls.start(cx.svc.call(|s| async move {
            j(api::calls(State(s), Query(api::Limit { limit: Some(100) })).await)
        }));
    }

    fn poll(&mut self, cx: &mut Cx, st: &mut SettingsState) {
        if self.tools.stale(std::time::Duration::from_secs(120)) {
            self.reload_tools(cx);
        }
        if self.calls.stale(std::time::Duration::from_secs(30)) {
            self.reload_calls(cx);
        }
        self.tools.poll();
        self.calls.poll();
        if let Some((name, t)) = &self.policy_task {
            if let Some(r) = t.take() {
                let name = name.clone();
                self.policy_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts
                            .success(format!("Política de '{name}' atualizada."));
                        // O handler grava direto nas settings; realinha rascunho e salvo.
                        st.load(cx.svc);
                        self.reload_tools(cx);
                    }
                    Err(e) => cx.toasts.error(format!("Política de '{name}': {e}")),
                }
            }
        }
    }

    fn set_policy(&mut self, cx: &Cx, st: &mut SettingsState, name: &str, value: &str) {
        // Espelha no rascunho para a barra de salvar não reverter a mudança.
        let setting = match value {
            "auto" => Some(ToolPolicySetting::Auto),
            "confirm" => Some(ToolPolicySetting::Confirm),
            "off" => Some(ToolPolicySetting::Off),
            _ => None,
        };
        match setting {
            Some(p) => {
                st.draft.mcp.policies.insert(name.to_string(), p);
            }
            None => {
                st.draft.mcp.policies.remove(name);
            }
        }
        let n = name.to_string();
        let policy = Some(value.to_string());
        let t = cx.svc.call(move |s| async move {
            j(api::set_policy(State(s), Path(n), Json(api::PolicyBody { policy })).await)
        });
        self.policy_task = Some((name.to_string(), t));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx, st);
        ui::heading(ui, "Servidor MCP", Some("Servidor local (Streamable HTTP em 127.0.0.1) que expõe as ferramentas do will-ia aos agentes. Bearer efêmero por processo; cada chamada sabe de qual thread veio."));

        // ---- servidor ----
        ui::section(ui, "Servidor", |ui| {
            let status = cx
                .snap
                .modules
                .get(&Module::Mcp)
                .cloned()
                .unwrap_or(will_core::ModuleStatus::Stopped);
            ui.horizontal(|ui| {
                ui.label("Módulo:");
                ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
            });
            let url = cx.svc.state.mcp_url.read().clone();
            ui.horizontal(|ui| {
                ui.label("URL:");
                if url.is_empty() {
                    ui.label(egui::RichText::new("(servidor não está no ar)").weak());
                } else {
                    ui::mono(ui, &url);
                }
            });
            ui.add_space(6.0);
            let m = &mut st.draft.mcp;
            ui::field(
                ui,
                "Habilitar",
                Some("Desligado, os agentes ficam sem as ferramentas will_ia."),
                |ui| {
                    ui::toggle(ui, &mut m.enabled);
                },
            );
            ui::field(ui, "Porta", Some("0 = aleatória a cada execução (recomendado); o .mcp.json da KB é regravado com a URL atual."), |ui| {
                ui::number(ui, &mut m.port, 0..=65535);
            });
            ui::field(
                ui,
                "Timeout de aprovação (s)",
                Some("Pedido de confirmação sem resposta é negado após esse tempo."),
                |ui| {
                    ui::number(ui, &mut m.approval_timeout_secs, 10..=3600);
                },
            );
            st.save_bar(ui, cx.svc, "mcp");
        });

        // ---- ferramentas ----
        ui::section(ui, "Ferramentas e políticas", |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("auto = executa direto · confirmar = pede aprovação (UI, notificação ou Telegram) · desligada = não é exposta ao agente.").weak().small());
            });
            ui.horizontal(|ui| {
                if ui.button("Atualizar").clicked() {
                    self.tools.loaded_at = None;
                }
                if self.tools.loading() {
                    ui.spinner();
                }
                if let Some((n, _)) = &self.policy_task {
                    ui.label(
                        egui::RichText::new(format!("aplicando política de {n}…"))
                            .weak()
                            .small(),
                    );
                }
            });
            if let Some(e) = &self.tools.error {
                ui::error_text(ui, e);
            }
            let list = self
                .tools
                .data
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if list.is_empty() && !self.tools.loading() {
                ui.label(egui::RichText::new("Nenhuma ferramenta registrada.").weak());
            }
            let mut by_family: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
            for t in &list {
                by_family.entry(ui::s(t, "family")).or_default().push(t);
            }
            let mut change: Option<(String, String)> = None;
            for (family, tools) in &by_family {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(family_label(family)).strong());
                egui::Grid::new(format!("mcp-tools-{family}"))
                    .num_columns(4)
                    .striped(true)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        for t in tools {
                            let name = ui::s(t, "name");
                            let available = ui::b(t, "available");
                            let default_policy = ui::s(t, "default_policy");
                            let overridden = ui::b(t, "overridden");
                            let policy = ui::s(t, "policy");
                            let desc = ui::s(t, "description");
                            let default_label =
                                format!("padrão ({})", policy_label(&default_policy));
                            let opts: [(&str, &str); 4] = [
                                ("default", default_label.as_str()),
                                ("auto", "automático"),
                                ("confirm", "confirmar"),
                                ("off", "desligada"),
                            ];
                            let mut v = if overridden {
                                policy.clone()
                            } else {
                                "default".to_string()
                            };
                            let busy = self.policy_task.is_some();
                            ui.monospace(&name);
                            if available {
                                ui::badge(ui, "disponível", Tone::Success);
                            } else {
                                ui::badge(ui, "indisponível", Tone::Neutral);
                            }
                            ui.add_enabled_ui(!busy, |ui| {
                                if ui::select(ui, &format!("pol-{name}"), &mut v, &opts) {
                                    change = Some((name.clone(), v.clone()));
                                }
                            });
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(truncate(&desc, 160)).weak().small(),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&desc);
                            ui.end_row();
                        }
                    });
            }
            if let Some((name, v)) = change {
                self.set_policy(cx, st, &name, &v);
            }
        });

        // ---- chamadas ----
        ui::section(ui, "Chamadas recentes", |ui| {
            ui.horizontal(|ui| {
                if ui.button("Atualizar").clicked() {
                    self.calls.loaded_at = None;
                }
                if self.calls.loading() {
                    ui.spinner();
                }
            });
            if let Some(e) = &self.calls.error {
                ui::error_text(ui, e);
            }
            let calls = self
                .calls
                .data
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if calls.is_empty() && !self.calls.loading() {
                ui.label(egui::RichText::new("Nenhuma chamada registrada ainda.").weak());
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("mcp-calls")
                .max_height(360.0)
                .show(ui, |ui| {
                    egui::Grid::new("mcp-calls-grid")
                        .num_columns(6)
                        .striped(true)
                        .spacing([14.0, 4.0])
                        .show(ui, |ui| {
                            for h in [
                                "Hora",
                                "Ferramenta",
                                "Thread",
                                "Resultado",
                                "Duração",
                                "Erro",
                            ] {
                                ui.label(egui::RichText::new(h).weak().small());
                            }
                            ui.end_row();
                            for c in &calls {
                                ui.label(egui::RichText::new(ui::fmt_dt(&c["at"])).small());
                                ui.monospace(ui::s(c, "tool"));
                                let tid = ui::s(c, "thread_id");
                                ui.monospace(if tid.is_empty() {
                                    "—".to_string()
                                } else {
                                    tid.chars().take(8).collect()
                                });
                                if ui::b(c, "ok") {
                                    ui::badge(ui, "ok", Tone::Success);
                                } else {
                                    ui::badge(ui, "erro", Tone::Danger);
                                }
                                let ms = c
                                    .get("duration_ms")
                                    .and_then(|d| d.as_i64())
                                    .unwrap_or(0)
                                    .max(0) as u64;
                                ui.label(egui::RichText::new(ui::fmt_ms(ms)).small());
                                let err = ui::s(c, "error");
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(truncate(&err, 60))
                                            .small()
                                            .color(Tone::Danger.color(ui.visuals().dark_mode)),
                                    )
                                    .wrap(),
                                )
                                .on_hover_text(&err);
                                ui.end_row();
                            }
                        });
                });
        });
    }
}
