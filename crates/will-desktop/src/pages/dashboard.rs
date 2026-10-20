//! Início: status dos módulos, KB, motor, primeiros passos.

use super::{Cx, Nav, SettingsSection};
use crate::service::{j, Loader, Service};
use crate::ui::{self, Tone};
use axum::extract::State;
use serde_json::Value;
use std::sync::Arc;
use will_core::{Event, Module};
use will_server::api::system;

#[derive(Default)]
pub struct DashboardPage {
    status: Loader<Value>,
}

impl DashboardPage {
    pub fn refresh(&mut self, svc: &Arc<Service>) {
        if self.status.busy() {
            return;
        }
        self.status
            .start(svc.call(|s| async move { j(system::status(State(s)).await) }));
    }

    pub fn on_event(&mut self, ev: &Event) {
        if matches!(
            ev,
            Event::ThreadCreated { .. }
                | Event::ThreadDeleted { .. }
                | Event::SettingsChanged { .. }
                | Event::ModuleStatus { .. }
        ) {
            self.status.loaded_at = None;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if self.status.stale(std::time::Duration::from_secs(10)) {
            self.refresh(cx.svc);
        }
        self.status.poll();
        ui::heading(ui, "Início", Some("Estado do serviço nesta máquina."));

        let s = &cx.snap.settings;
        let kb_ok = cx.snap.modules.get(&Module::Engine).map(|m| !matches!(m, will_core::ModuleStatus::Degraded(d) if d.contains("base de conhecimento"))).unwrap_or(false) && s.kb.path.is_some();
        let token_ok = s.engine.tokens.iter().any(|t| t.active);
        if !s.general.onboarding_done || !kb_ok || !token_ok {
            ui::section(ui, "Primeiros passos", |ui| {
                let step =
                    |ui: &mut egui::Ui, done: bool, text: &str, go: Option<Nav>, cx: &mut Cx| {
                        ui.horizontal(|ui| {
                            ui::dot(ui, if done { Tone::Success } else { Tone::Warning });
                            ui.label(text);
                            if let Some(n) = go {
                                if !done && ui.button("ir").clicked() {
                                    *cx.go = Some(n);
                                }
                            }
                        });
                    };
                let claude_found = self.status.data.as_ref().map(|_| true).unwrap_or(true);
                step(ui, claude_found, "Instale o Claude Code CLI (npm i -g @anthropic-ai/claude-code) e faça login (claude setup-token).", Some(Nav::Settings(SettingsSection::Engine)), cx);
                step(
                    ui,
                    token_ok,
                    "Cadastre e ative um token do Claude em Motor Claude.",
                    Some(Nav::Settings(SettingsSection::Engine)),
                    cx,
                );
                step(
                    ui,
                    kb_ok,
                    "Escolha a pasta da base de conhecimento e crie a estrutura inicial.",
                    Some(Nav::Settings(SettingsSection::Kb)),
                    cx,
                );
                step(
                    ui,
                    s.realtime.enabled,
                    "Opcional: chave da OpenAI para voz.",
                    Some(Nav::Settings(SettingsSection::Voice)),
                    cx,
                );
                step(
                    ui,
                    s.telegram.enabled,
                    "Opcional: bot do Telegram (token do @BotFather + allowlist).",
                    Some(Nav::Settings(SettingsSection::Telegram)),
                    cx,
                );
            });
        }

        ui::section(ui, "Módulos", |ui| {
            egui::Grid::new("modules")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .striped(true)
                .show(ui, |ui| {
                    for m in Module::all() {
                        let st = cx
                            .snap
                            .modules
                            .get(m)
                            .cloned()
                            .unwrap_or(will_core::ModuleStatus::Stopped);
                        ui.horizontal(|ui| {
                            ui::dot(ui, ui::tone_for_module(&st));
                            ui.label(ui::module_name(*m));
                        });
                        ui.label(ui::module_label(&st));
                        ui.end_row();
                    }
                });
        });

        if let Some(v) = &self.status.data {
            ui::section(ui, "Motor", |ui| {
                let e = &v["engine"];
                egui::Grid::new("engine-stats")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Processos vivos");
                        ui.label(format!(
                            "{} / {}",
                            ui::i(e, "live_processes"),
                            ui::i(e, "max_live_processes")
                        ));
                        ui.end_row();
                        ui.label("Threads");
                        ui.label(ui::i(e, "threads").to_string());
                        ui.end_row();
                        ui.label("Turnos em andamento");
                        ui.label(ui::i(e, "running_turns").to_string());
                        ui.end_row();
                        ui.label("Token ativo");
                        ui.label(
                            e.get("active_token")
                                .and_then(|t| t.as_str())
                                .unwrap_or("nenhum"),
                        );
                        ui.end_row();
                        ui.label("Modo de permissão");
                        ui.label(ui::s(v, "permission_mode"));
                        ui.end_row();
                        ui.label("Servidor MCP");
                        ui.monospace(if ui::s(v, "mcp_url").is_empty() {
                            "desligado".to_string()
                        } else {
                            ui::s(v, "mcp_url")
                        });
                        ui.end_row();
                        ui.label("Segredos");
                        ui.label(ui::s(v, "secrets_backend"));
                        ui.end_row();
                        ui.label("Tempo no ar");
                        ui.label(format!("{} min", ui::i(v, "uptime_secs") / 60));
                        ui.end_row();
                    });
            });
            ui::section(ui, "Base de conhecimento", |ui| {
                let k = &v["kb"];
                let path = ui::s(k, "path");
                ui.horizontal(|ui| {
                    ui::dot(
                        ui,
                        if ui::b(k, "exists") {
                            Tone::Success
                        } else {
                            Tone::Warning
                        },
                    );
                    ui::mono(
                        ui,
                        if path.is_empty() {
                            "não configurada"
                        } else {
                            &path
                        },
                    );
                    if ui.button("configurar").clicked() {
                        *cx.go = Some(Nav::Settings(SettingsSection::Kb));
                    }
                });
            });
        }
        if let Some(e) = &self.status.error {
            ui::error_text(ui, e);
        }

        ui::section(ui, "Ações rápidas", |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Nova thread").clicked() {
                    *cx.go = Some(Nav::Threads);
                }
                if ui.button("Ver logs").clicked() {
                    *cx.go = Some(Nav::Logs);
                }
                if ui
                    .add_enabled(
                        cx.snap.settings.realtime.enabled && !cx.voice.busy(),
                        egui::Button::new(if cx.voice.session_active {
                            "Desligar voz"
                        } else {
                            "Ligar voz"
                        }),
                    )
                    .clicked()
                {
                    cx.voice.toggle(cx.svc);
                }
            });
        });
    }
}
