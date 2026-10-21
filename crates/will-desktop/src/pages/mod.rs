//! Páginas da janela principal. Cada página guarda seu próprio estado e recebe
//! um [`Cx`] com o serviço, o snapshot global e os toasts.

pub mod approvals;
pub mod costs;
pub mod dashboard;
pub mod logs;
pub mod settings;
pub mod threads;

use crate::i18n::tr;
use crate::service::Service;
use crate::ui::Toasts;
use crate::voice::VoiceController;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use will_core::{ApprovalRequest, Event, Module, ModuleStatus, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsSection {
    General,
    Engine,
    Kb,
    Voice,
    Telegram,
    Mcp,
    External,
    WhatsApp,
    Google,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 9] = [
        SettingsSection::General,
        SettingsSection::Engine,
        SettingsSection::Kb,
        SettingsSection::Voice,
        SettingsSection::Telegram,
        SettingsSection::Mcp,
        SettingsSection::External,
        SettingsSection::WhatsApp,
        SettingsSection::Google,
    ];
    /// Rótulo no idioma corrente da UI.
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => tr("Geral"),
            SettingsSection::Engine => tr("Motor Claude"),
            SettingsSection::Kb => tr("Base de conhecimento"),
            SettingsSection::Voice => tr("Voz"),
            SettingsSection::Telegram => tr("Telegram"),
            SettingsSection::Mcp => tr("Servidor MCP"),
            SettingsSection::External => tr("MCPs externos"),
            SettingsSection::WhatsApp => tr("WhatsApp"),
            SettingsSection::Google => tr("Google"),
        }
    }
    /// Módulo cujo status aparece como ponto colorido no menu.
    pub fn module(self) -> Option<Module> {
        match self {
            SettingsSection::Engine => Some(Module::Engine),
            SettingsSection::Voice => Some(Module::Realtime),
            SettingsSection::Telegram => Some(Module::Telegram),
            SettingsSection::Mcp => Some(Module::Mcp),
            SettingsSection::WhatsApp => Some(Module::WhatsApp),
            SettingsSection::Google => Some(Module::Google),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Nav {
    Dashboard,
    Threads,
    Approvals,
    Logs,
    Costs,
    Settings(SettingsSection),
}

impl Nav {
    /// `dashboard`, `threads`, `approvals`, `logs`, `general`, `engine`, `kb`, `voice`,
    /// `telegram`, `mcp`, `external`, `whatsapp`, `google`.
    pub fn parse(s: &str) -> Option<Nav> {
        Some(match s {
            "dashboard" => Nav::Dashboard,
            "threads" => Nav::Threads,
            "approvals" => Nav::Approvals,
            "logs" => Nav::Logs,
            "costs" => Nav::Costs,
            "general" => Nav::Settings(SettingsSection::General),
            "engine" => Nav::Settings(SettingsSection::Engine),
            "kb" => Nav::Settings(SettingsSection::Kb),
            "voice" => Nav::Settings(SettingsSection::Voice),
            "telegram" => Nav::Settings(SettingsSection::Telegram),
            "mcp" => Nav::Settings(SettingsSection::Mcp),
            "external" => Nav::Settings(SettingsSection::External),
            "whatsapp" => Nav::Settings(SettingsSection::WhatsApp),
            "google" => Nav::Settings(SettingsSection::Google),
            _ => return None,
        })
    }
}

/// Estado global derivado de eventos (barato de clonar por partes).
#[derive(Default)]
pub struct Snapshot {
    pub settings: Settings,
    pub modules: HashMap<Module, ModuleStatus>,
    pub pending_approvals: Vec<ApprovalRequest>,
    pub portable_root: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub api_url: String,
}

pub struct Cx<'a> {
    pub svc: &'a Arc<Service>,
    pub snap: &'a Snapshot,
    pub toasts: &'a mut Toasts,
    pub voice: &'a mut VoiceController,
    /// Navegação pedida pela página (aplicada no fim do quadro).
    pub go: &'a mut Option<Nav>,
}

#[derive(Default)]
pub struct Pages {
    pub dashboard: dashboard::DashboardPage,
    pub threads: threads::ThreadsPage,
    pub approvals: approvals::ApprovalsPage,
    pub logs: logs::LogsPage,
    pub costs: costs::CostsPage,
    pub settings: settings::SettingsPages,
}

impl Pages {
    pub fn on_event(&mut self, ev: &Event) {
        self.dashboard.on_event(ev);
        self.threads.on_event(ev);
        self.approvals.on_event(ev);
        self.logs.on_event(ev);
        self.costs.on_event(ev);
        self.settings.on_event(ev);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, nav: Nav, cx: &mut Cx) {
        match nav {
            Nav::Dashboard => self.dashboard.ui(ui, cx),
            Nav::Threads => self.threads.ui(ui, cx),
            Nav::Approvals => self.approvals.ui(ui, cx),
            Nav::Logs => self.logs.ui(ui, cx),
            Nav::Costs => self.costs.ui(ui, cx),
            Nav::Settings(sec) => self.settings.ui(ui, sec, cx),
        }
    }
}
