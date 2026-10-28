//! Configurações: estado compartilhado (rascunho × salvo, segredos presentes,
//! salvamento por seção) e despacho para cada seção.

pub mod browser;
pub mod engine;
pub mod external;
pub mod general;
pub mod google;
pub mod kb;
pub mod mcp;
pub mod scheduler;
pub mod telegram;
pub mod voice;
pub mod whatsapp;

use super::{Cx, SettingsSection};
use crate::i18n::{tr, trf};
use crate::service::{j, Loader, Service, Task};
use crate::ui;
use axum::extract::{Path, State};
use axum::Json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use will_core::Settings;
use will_server::api::system;

/// Estado compartilhado por todas as seções.
#[derive(Default)]
pub struct SettingsState {
    loader: Loader<Value>,
    pub draft: Settings,
    pub saved: Settings,
    pub secrets_present: BTreeMap<String, bool>,
    pub loaded: bool,
    save_task: Option<(String, Task<Value>)>,
    secret_task: Option<(String, Task<Value>)>,
    pub last_error: Option<String>,
}

impl SettingsState {
    pub fn load(&mut self, svc: &Arc<Service>) {
        if self.loader.busy() {
            return;
        }
        self.loader
            .start(svc.call(|st| async move { j(system::get_settings(State(st)).await) }));
    }

    /// Consome tarefas pendentes. Deve rodar a cada quadro antes de desenhar.
    pub fn poll(&mut self, cx: &mut Cx) {
        if self.loader.poll() {
            if let Some(v) = &self.loader.data {
                match serde_json::from_value::<Settings>(v.clone()) {
                    Ok(s) => {
                        // Preserva edições não salvas da seção em curso: só substitui o rascunho
                        // nas seções iguais ao que estava salvo.
                        let old_saved = serde_json::to_value(&self.saved).unwrap_or_default();
                        let old_draft = serde_json::to_value(&self.draft).unwrap_or_default();
                        let mut new_draft = serde_json::to_value(&s).unwrap_or_default();
                        if self.loaded {
                            if let (Some(nd), Some(os), Some(od)) = (
                                new_draft.as_object_mut(),
                                old_saved.as_object(),
                                old_draft.as_object(),
                            ) {
                                for (k, dv) in od {
                                    if os.get(k) != Some(dv) {
                                        nd.insert(k.clone(), dv.clone());
                                    }
                                }
                            }
                        }
                        self.draft =
                            serde_json::from_value(new_draft).unwrap_or_else(|_| s.clone());
                        self.saved = s;
                        self.loaded = true;
                        self.last_error = None;
                    }
                    Err(e) => self.last_error = Some(trf("settings inválidas: {}", &[&e])),
                }
                if let Some(m) = v.get("secrets_present").and_then(|x| x.as_object()) {
                    self.secrets_present = m
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_bool().unwrap_or(false)))
                        .collect();
                }
            } else if let Some(e) = &self.loader.error {
                self.last_error = Some(e.clone());
            }
        }
        if let Some((section, t)) = &self.save_task {
            if let Some(r) = t.take() {
                let section = section.clone();
                self.save_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts
                            .success(trf("Seção '{}' salva.", &[&section_label(&section)]));
                        self.load(cx.svc);
                    }
                    Err(e) => cx.toasts.error(trf("Não salvou: {}", &[&e])),
                }
            }
        }
        if let Some((key, t)) = &self.secret_task {
            if let Some(r) = t.take() {
                let key = key.clone();
                self.secret_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts.success(trf("Segredo '{}' atualizado.", &[&key]));
                        self.load(cx.svc);
                    }
                    Err(e) => cx.toasts.error(trf("Segredo: {}", &[&e])),
                }
            }
        }
    }

    pub fn saving(&self) -> bool {
        self.save_task.as_ref().is_some_and(|(_, t)| t.pending())
    }

    fn section_value(s: &Settings, section: &str) -> Value {
        serde_json::to_value(s)
            .ok()
            .and_then(|v| v.get(section).cloned())
            .unwrap_or(Value::Null)
    }

    pub fn dirty(&self, section: &str) -> bool {
        Self::section_value(&self.draft, section) != Self::section_value(&self.saved, section)
    }

    pub fn reset(&mut self, section: &str) {
        let mut d = serde_json::to_value(&self.draft).unwrap_or_default();
        let sv = Self::section_value(&self.saved, section);
        if let Some(o) = d.as_object_mut() {
            o.insert(section.to_string(), sv);
        }
        if let Ok(s) = serde_json::from_value(d) {
            self.draft = s;
        }
    }

    pub fn save(&mut self, svc: &Arc<Service>, section: &str) {
        if self.saving() {
            return;
        }
        let value = Self::section_value(&self.draft, section);
        let sec = section.to_string();
        let sec2 = sec.clone();
        let t = svc.call(move |st| async move {
            j(system::put_settings(
                State(st),
                Json(system::PutSettings {
                    section: sec2,
                    value,
                }),
            )
            .await)
        });
        self.save_task = Some((sec, t));
    }

    pub fn set_secret(&mut self, svc: &Arc<Service>, key: &str, value: String) {
        let k = key.to_string();
        let k2 = k.clone();
        let t = svc.call(move |st| async move {
            j(system::set_secret(State(st), Json(system::SetSecret { key: k2, value })).await)
        });
        self.secret_task = Some((k, t));
    }

    pub fn delete_secret(&mut self, svc: &Arc<Service>, key: &str) {
        let k = key.to_string();
        let k2 = k.clone();
        let t =
            svc.call(move |st| async move { j(system::delete_secret(State(st), Path(k2)).await) });
        self.secret_task = Some((k, t));
    }

    pub fn secret_present(&self, key: &str) -> bool {
        self.secrets_present.get(key).copied().unwrap_or(false)
    }

    /// Barra Salvar/Descartar da seção.
    pub fn save_bar(&mut self, ui: &mut egui::Ui, svc: &Arc<Service>, section: &str) {
        let dirty = self.dirty(section);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    dirty && !self.saving(),
                    egui::Button::new(
                        egui::RichText::new(tr("Salvar")).color(egui::Color32::WHITE),
                    )
                    .fill(ui::palette(ui.visuals().dark_mode).accent),
                )
                .clicked()
            {
                self.save(svc, section);
            }
            if ui
                .add_enabled(dirty && !self.saving(), egui::Button::new(tr("Descartar")))
                .clicked()
            {
                self.reset(section);
            }
            if self.saving() {
                ui.spinner();
            } else if dirty {
                ui.label(egui::RichText::new(tr("alterações não salvas")).weak());
            }
        });
    }
}

fn section_label(s: &str) -> &str {
    match s {
        "general" => tr("Geral"),
        "engine" => tr("Motor Claude"),
        "kb" => tr("Base de conhecimento"),
        "realtime" => tr("Voz"),
        "telegram" => tr("Telegram"),
        "mcp" => tr("Servidor MCP"),
        "whatsapp" => tr("WhatsApp"),
        "google" => tr("Google"),
        "calendar" => tr("Agenda"),
        "gmail" => tr("Gmail"),
        "external_mcps" => tr("MCPs externos"),
        "scheduler" => tr("Agendamentos"),
        "browser" => tr("Browser CDP"),
        other => other,
    }
}

#[derive(Default)]
pub struct SettingsPages {
    pub state: SettingsState,
    pub general: general::GeneralPage,
    pub engine: engine::EnginePage,
    pub kb: kb::KbPage,
    pub voice: voice::VoicePage,
    pub telegram: telegram::TelegramPage,
    pub mcp: mcp::McpPage,
    pub external: external::ExternalPage,
    pub whatsapp: whatsapp::WhatsAppPage,
    pub google: google::GooglePage,
    pub scheduler: scheduler::SchedulerPage,
    pub browser: browser::BrowserPage,
}

impl SettingsPages {
    pub fn on_event(&mut self, ev: &will_core::Event) {
        self.engine.on_event(ev);
        self.voice.on_event(ev);
        self.telegram.on_event(ev);
        self.mcp.on_event(ev);
        self.external.on_event(ev);
        self.whatsapp.on_event(ev);
        self.google.on_event(ev);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, sec: SettingsSection, cx: &mut Cx) {
        self.state.poll(cx);
        if !self.state.loaded {
            if let Some(e) = &self.state.last_error {
                ui::error_text(ui, e);
                if ui.button(tr("Tentar de novo")).clicked() {
                    self.state.load(cx.svc);
                }
            } else {
                ui::busy(ui, tr("carregando configurações…"));
            }
            return;
        }
        let st = &mut self.state;
        match sec {
            SettingsSection::General => self.general.ui(ui, cx, st),
            SettingsSection::Engine => self.engine.ui(ui, cx, st),
            SettingsSection::Kb => self.kb.ui(ui, cx, st),
            SettingsSection::Voice => self.voice.ui(ui, cx, st),
            SettingsSection::Telegram => self.telegram.ui(ui, cx, st),
            SettingsSection::Mcp => self.mcp.ui(ui, cx, st),
            SettingsSection::External => self.external.ui(ui, cx, st),
            SettingsSection::WhatsApp => self.whatsapp.ui(ui, cx, st),
            SettingsSection::Google => self.google.ui(ui, cx, st),
            SettingsSection::Scheduler => self.scheduler.ui(ui, cx, st),
            SettingsSection::Browser => self.browser.ui(ui, cx, st),
        }
    }
}
