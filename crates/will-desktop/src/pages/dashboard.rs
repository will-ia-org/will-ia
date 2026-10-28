//! Início: status dos módulos, KB, motor, primeiros passos.

use super::{Cx, Nav, SettingsSection};
use crate::i18n::{tr, trf};
use crate::service::{j, Loader, Service};
use crate::ui::{self, Tone};
use axum::extract::State;
use serde_json::Value;
use std::sync::Arc;
use will_core::{Event, Module};
use will_server::api::{integrations, system};

#[derive(Default)]
pub struct DashboardPage {
    status: Loader<Value>,
    /// Contas Google (só para o rótulo "N conta(s)" na grade de módulos).
    google_accounts: Loader<Value>,
}

impl DashboardPage {
    pub fn refresh(&mut self, svc: &Arc<Service>) {
        if self.status.busy() {
            return;
        }
        self.status
            .start(svc.call(|s| async move { j(system::status(State(s)).await) }));
        if !self.google_accounts.busy() {
            self.google_accounts.start(
                svc.call(|s| async move { j(integrations::google_accounts(State(s)).await) }),
            );
        }
    }

    /// Rótulo do módulo na grade: o genérico de `ui::module_label` mais o
    /// detalhe que só faz sentido aqui (processos vivos, sessão de voz, contas).
    fn module_detail(&self, m: Module, st: &will_core::ModuleStatus, cx: &Cx) -> String {
        let base = ui::module_label(st);
        if !matches!(st, will_core::ModuleStatus::Running) {
            return base;
        }
        match m {
            Module::Engine => {
                let live = self
                    .status
                    .data
                    .as_ref()
                    .map(|v| ui::i(&v["engine"], "live_processes"))
                    .unwrap_or(0);
                if live > 0 {
                    trf("{} · {} processo(s) vivo(s)", &[&base, &live])
                } else {
                    base
                }
            }
            Module::Realtime => {
                if cx.voice.session_active {
                    tr("em sessão").into()
                } else {
                    base
                }
            }
            Module::Google => {
                let n = self
                    .google_accounts
                    .data
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                if n > 0 {
                    trf("{} · {} conta(s)", &[&base, &n])
                } else {
                    base
                }
            }
            // Agendamentos: `Running` = "pronto" (o relógio está rodando); sem detalhe extra.
            Module::Scheduler => base,
            // Browser CDP: `Running` = proxy escutando; o detalhe fica na própria página.
            Module::Browser => base,
            _ => base,
        }
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
        self.google_accounts.poll();
        ui::heading(
            ui,
            tr("Início"),
            Some(tr("Estado do serviço nesta máquina.")),
        );

        let s = &cx.snap.settings;
        let kb_ok = cx.snap.modules.get(&Module::Engine).map(|m| !matches!(m, will_core::ModuleStatus::Degraded(d) if d.contains("base de conhecimento"))).unwrap_or(false) && s.kb.path.is_some();
        let token_ok = s.engine.tokens.iter().any(|t| t.active);
        if !s.general.onboarding_done || !kb_ok || !token_ok {
            ui::section(ui, tr("Primeiros passos"), |ui| {
                let step =
                    |ui: &mut egui::Ui, done: bool, text: &str, go: Option<Nav>, cx: &mut Cx| {
                        ui.horizontal(|ui| {
                            ui::dot(ui, if done { Tone::Success } else { Tone::Warning });
                            ui.label(text);
                            if let Some(n) = go {
                                if !done && ui.button(tr("ir")).clicked() {
                                    *cx.go = Some(n);
                                }
                            }
                        });
                    };
                let claude_found = self.status.data.as_ref().map(|_| true).unwrap_or(true);
                step(ui, claude_found, tr("Instale o Claude Code CLI (npm i -g @anthropic-ai/claude-code) e faça login (claude setup-token)."), Some(Nav::Settings(SettingsSection::Engine)), cx);
                step(
                    ui,
                    token_ok,
                    tr("Cadastre e ative um token do Claude em Motor Claude."),
                    Some(Nav::Settings(SettingsSection::Engine)),
                    cx,
                );
                step(
                    ui,
                    kb_ok,
                    tr("Escolha a pasta da base de conhecimento e crie a estrutura inicial."),
                    Some(Nav::Settings(SettingsSection::Kb)),
                    cx,
                );
                step(
                    ui,
                    s.realtime.enabled,
                    tr("Opcional: chave da OpenAI para voz."),
                    Some(Nav::Settings(SettingsSection::Voice)),
                    cx,
                );
                step(
                    ui,
                    s.telegram.enabled,
                    tr("Opcional: bot do Telegram (token do @BotFather + allowlist)."),
                    Some(Nav::Settings(SettingsSection::Telegram)),
                    cx,
                );
            });
        }

        ui::section(ui, tr("Módulos"), |ui| {
            egui::Grid::new("modules")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .striped(true)
                .show(ui, |ui| {
                    let dark = ui.visuals().dark_mode;
                    for m in Module::all() {
                        let st = cx
                            .snap
                            .modules
                            .get(m)
                            .cloned()
                            .unwrap_or(will_core::ModuleStatus::Stopped);
                        let tone = ui::tone_for_module(&st);
                        ui.horizontal(|ui| {
                            ui::dot(ui, tone);
                            ui.label(ui::module_name(*m));
                        });
                        let text = self.module_detail(*m, &st, cx);
                        // Amarelo/vermelho levam o motivo na própria cor, para
                        // o usuário entender a bolinha sem passar o mouse.
                        let rich = match st {
                            will_core::ModuleStatus::Degraded(_)
                            | will_core::ModuleStatus::Error(_) => {
                                egui::RichText::new(text).color(tone.color(dark))
                            }
                            _ => egui::RichText::new(text),
                        };
                        ui.add(egui::Label::new(rich).wrap());
                        ui.end_row();
                    }
                });
        });

        if let Some(v) = &self.status.data {
            ui::section(ui, tr("Motor"), |ui| {
                let e = &v["engine"];
                egui::Grid::new("engine-stats")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(tr("Processos vivos"));
                        ui.label(format!(
                            "{} / {}",
                            ui::i(e, "live_processes"),
                            ui::i(e, "max_live_processes")
                        ));
                        ui.end_row();
                        ui.label(tr("Threads"));
                        ui.label(ui::i(e, "threads").to_string());
                        ui.end_row();
                        ui.label(tr("Turnos em andamento"));
                        ui.label(ui::i(e, "running_turns").to_string());
                        ui.end_row();
                        ui.label(tr("Token ativo"));
                        ui.label(
                            e.get("active_token")
                                .and_then(|t| t.as_str())
                                .unwrap_or(tr("nenhum")),
                        );
                        ui.end_row();
                        ui.label(tr("Modo de permissão"));
                        ui.label(ui::s(v, "permission_mode"));
                        ui.end_row();
                        ui.label(tr("Servidor MCP"));
                        ui.monospace(if ui::s(v, "mcp_url").is_empty() {
                            tr("desligado").to_string()
                        } else {
                            ui::s(v, "mcp_url")
                        });
                        ui.end_row();
                        ui.label(tr("Segredos"));
                        ui.label(ui::s(v, "secrets_backend"));
                        ui.end_row();
                        ui.label(tr("Tempo no ar"));
                        ui.label(trf("{} min", &[&(ui::i(v, "uptime_secs") / 60)]));
                        ui.end_row();
                    });
            });
            ui::section(ui, tr("Base de conhecimento"), |ui| {
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
                            tr("não configurada")
                        } else {
                            &path
                        },
                    );
                    if ui.button(tr("configurar")).clicked() {
                        *cx.go = Some(Nav::Settings(SettingsSection::Kb));
                    }
                });
            });
        }
        if let Some(e) = &self.status.error {
            ui::error_text(ui, e);
        }

        ui::section(ui, tr("Ações rápidas"), |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr("Nova thread")).clicked() {
                    *cx.go = Some(Nav::Threads);
                }
                if ui.button(tr("Ver logs")).clicked() {
                    *cx.go = Some(Nav::Logs);
                }
                if ui
                    .add_enabled(
                        cx.snap.settings.realtime.enabled && !cx.voice.busy(),
                        egui::Button::new(if cx.voice.session_active {
                            tr("Desligar voz")
                        } else {
                            tr("Ligar voz")
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
