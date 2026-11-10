//! Páginas da janela principal. Cada página guarda seu próprio estado e recebe
//! um [`Cx`] com o serviço, o snapshot global e os toasts.

pub mod approvals;
pub mod browser;
pub mod connect;
pub mod costs;
pub mod dashboard;
pub mod logs;
pub mod schedules;
pub mod settings;
pub mod tasks;
pub mod threads;
pub mod triggers;
pub mod users;

use crate::i18n::tr;
use crate::service::Service;
use crate::ui::Toasts;
use crate::voice::VoiceController;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use will_core::{ApprovalRequest, Event, Module, ModuleStatus, Principal, Settings};

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
    Scheduler,
    Browser,
    Network,
    Tasks,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 13] = [
        SettingsSection::General,
        SettingsSection::Engine,
        SettingsSection::Kb,
        SettingsSection::Voice,
        SettingsSection::Telegram,
        SettingsSection::Mcp,
        SettingsSection::External,
        SettingsSection::WhatsApp,
        SettingsSection::Google,
        SettingsSection::Scheduler,
        SettingsSection::Browser,
        SettingsSection::Network,
        SettingsSection::Tasks,
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
            SettingsSection::Scheduler => tr("Triggers"),
            SettingsSection::Browser => tr("Browser CDP"),
            SettingsSection::Network => tr("Rede"),
            SettingsSection::Tasks => tr("Tarefas"),
        }
    }
    /// Uma linha para o cartão da página Configurações.
    pub fn description(self) -> &'static str {
        match self {
            SettingsSection::General => tr("Idioma, tema, atalho, status do serviço e telemetria."),
            SettingsSection::Engine => {
                tr("Claude Code: token da assinatura, binário, limites e resumo das threads.")
            }
            SettingsSection::Kb => tr("Pasta da base de conhecimento que o agente lê e escreve."),
            SettingsSection::Voice => tr("Voz em tempo real (OpenAI) e chave da API."),
            SettingsSection::Telegram => {
                tr("Bot, chats permitidos, espelho de tópicos e resumo em áudio.")
            }
            SettingsSection::Mcp => tr("Servidor MCP local e política de cada ferramenta."),
            SettingsSection::External => tr("Servidores MCP de terceiros entregues ao agente."),
            SettingsSection::WhatsApp => tr("Conta pareada e ferramentas whatsapp_*."),
            SettingsSection::Google => tr("Contas Google: Agenda, Gmail e Drive."),
            SettingsSection::Scheduler => {
                tr("Regras por WhatsApp, e-mail e horário que acordam threads.")
            }
            SettingsSection::Browser => tr("Proxy CDP do Chrome para o agent-browser."),
            SettingsSection::Network => tr("Porta, TLS e acesso remoto ao servidor."),
            SettingsSection::Tasks => tr("Fuso, avisos no Telegram e ordenação do Gantt."),
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
            SettingsSection::Scheduler => Some(Module::Scheduler),
            SettingsSection::Browser => Some(Module::Browser),
            SettingsSection::Tasks => Some(Module::Tasks),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Nav {
    /// Painel Gantt de tarefas (primeiro item do menu).
    Tasks,
    Threads,
    Schedules,
    Browser,
    Approvals,
    Logs,
    Costs,
    /// Contas do servidor (só administradores).
    Users,
    /// Página de entrada das configurações (cartões de todas as seções).
    SettingsHome,
    Settings(SettingsSection),
}

impl Nav {
    /// `dashboard`/`status` (= Geral → Status), `tasks`, `threads`, `schedules`, `browser`, `approvals`, `logs`, `costs`,
    /// `users`, `general`, `engine`, `kb`, `voice`, `telegram`, `mcp`, `external`, `whatsapp`,
    /// `google`, `scheduler`, `browser-settings`, `network`. A tela de entrada
    /// (`connect`) não é uma página: aparece sozinha quando não há conexão.
    /// Páginas com abas aceitam `pagina/aba` no `screenshot --nav` (o app
    /// seleciona a aba com `ui::select_tab`; a chave da aba é a do `ui::Tab`).
    pub fn parse(s: &str) -> Option<Nav> {
        Some(match s {
            // "Início" virou a aba Status de Configurações → Geral (2026-10-29).
            "dashboard" | "status" => Nav::Settings(SettingsSection::General),
            "tasks" => Nav::Tasks,
            "threads" => Nav::Threads,
            "schedules" | "triggers" => Nav::Schedules,
            "browser" => Nav::Browser,
            "approvals" => Nav::Approvals,
            "logs" => Nav::Logs,
            "costs" => Nav::Costs,
            "users" => Nav::Users,
            "settings" | "config" => Nav::SettingsHome,
            "general" => Nav::Settings(SettingsSection::General),
            "engine" => Nav::Settings(SettingsSection::Engine),
            "kb" => Nav::Settings(SettingsSection::Kb),
            "voice" => Nav::Settings(SettingsSection::Voice),
            "telegram" => Nav::Settings(SettingsSection::Telegram),
            "mcp" => Nav::Settings(SettingsSection::Mcp),
            "external" => Nav::Settings(SettingsSection::External),
            "whatsapp" => Nav::Settings(SettingsSection::WhatsApp),
            "google" => Nav::Settings(SettingsSection::Google),
            "scheduler" => Nav::Settings(SettingsSection::Scheduler),
            "browser-settings" => Nav::Settings(SettingsSection::Browser),
            "network" => Nav::Settings(SettingsSection::Network),
            "tasks-settings" => Nav::Settings(SettingsSection::Tasks),
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
    pub logs_dir: PathBuf,
    pub db_path: PathBuf,
    pub api_url: String,
    /// O servidor é esta máquina (loopback): ações locais como "abrir pasta" valem.
    pub server_is_local: bool,
    /// Conexão com o servidor (WS de eventos) no ar.
    pub connected: bool,
    /// Quem está autenticado neste servidor (`GET /api/auth/me`); `None` enquanto
    /// não carregou. Decide o que o menu mostra (configurações e usuários só
    /// para administradores) e a troca obrigatória de senha.
    pub principal: Option<Principal>,
}

impl Snapshot {
    /// Administrador do servidor. Sem `principal` carregado ainda, assume que sim
    /// (comportamento de antes das contas) para não esconder o menu durante a carga.
    pub fn is_admin(&self) -> bool {
        self.principal
            .as_ref()
            .map(|p| p.is_admin())
            .unwrap_or(true)
    }
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
    /// Tela de entrada (perfis/login); desenhada pelo app no lugar das páginas
    /// quando não há conexão, e dona do modal de troca obrigatória de senha.
    pub connect: connect::ConnectPage,
    pub tasks: tasks::TasksPage,
    pub threads: threads::ThreadsPage,
    pub schedules: schedules::SchedulesPage,
    pub triggers: triggers::TriggersPage,
    pub browser: browser::BrowserPage,
    pub approvals: approvals::ApprovalsPage,
    pub logs: logs::LogsPage,
    pub costs: costs::CostsPage,
    pub users: users::UsersPage,
    pub settings: settings::SettingsPages,
}

impl Pages {
    pub fn on_event(&mut self, ev: &Event) {
        self.tasks.on_event(ev);
        self.threads.on_event(ev);
        self.schedules.on_event(ev);
        self.triggers.on_event(ev);
        self.browser.on_event(ev);
        self.approvals.on_event(ev);
        self.logs.on_event(ev);
        self.costs.on_event(ev);
        self.settings.on_event(ev);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, nav: Nav, cx: &mut Cx) {
        match nav {
            Nav::Tasks => self.tasks.ui(ui, cx),
            Nav::Threads => self.threads.ui(ui, cx),
            // Triggers: abas "WhatsApp e E-mail" (regras) e "Agendamentos" (relógio).
            Nav::Schedules => {
                crate::ui::heading_help(
                    ui,
                    tr("Triggers"),
                    Some(tr(
                        "Regras que acordam uma thread do Claude Code: por mensagem do WhatsApp, e-mail novo ou horário (agendamentos). Dispara e esquece.",
                    )),
                    &crate::help::TRIGGERS,
                );
                let tab = crate::ui::tabs(
                    ui,
                    "triggers",
                    &[
                        crate::ui::Tab::new("rules", tr("WhatsApp e E-mail")),
                        crate::ui::Tab::new("schedules", tr("Agendamentos")),
                    ],
                );
                ui.add_space(6.0);
                if tab == "schedules" {
                    self.schedules.ui(ui, cx);
                } else {
                    self.triggers.ui(ui, cx);
                }
            }
            Nav::Browser => self.browser.ui(ui, cx),
            Nav::Approvals => self.approvals.ui(ui, cx),
            Nav::Logs => self.logs.ui(ui, cx),
            Nav::Costs => self.costs.ui(ui, cx),
            Nav::Users => self.users.ui(ui, cx),
            Nav::SettingsHome => self.settings_home(ui, cx),
            Nav::Settings(sec) => {
                // Trilha de volta: as seções não ficam mais no menu lateral.
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(crate::ui::icon_text(
                                ui,
                                crate::icons::ARROW_BACK,
                                tr("Configurações"),
                                Some(13.0),
                            ))
                            .frame(false),
                        )
                        .clicked()
                    {
                        *cx.go = Some(Nav::SettingsHome);
                    }
                    ui.label(egui::RichText::new("›").weak());
                    ui.label(egui::RichText::new(sec.label()).weak().small());
                });
                ui.add_space(2.0);
                self.settings.ui(ui, sec, cx)
            }
        }
    }

    /// Configurações: um cartão por seção (nome, estado do módulo, uma linha),
    /// em grade que se adapta à largura. Clicar abre a seção.
    fn settings_home(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        use crate::ui::{self as u, Tone};
        let dark = ui.visuals().dark_mode;
        let pal = u::palette(dark);
        u::heading(
            ui,
            tr("Configurações"),
            Some(tr(
                "Tudo que não é do dia a dia: módulos, contas, chaves e limites.",
            )),
        );
        ui.add_space(8.0);
        const CARD_W: f32 = 270.0;
        const CARD_H: f32 = 104.0;
        const GAP: f32 = 12.0;
        let cols = ((ui.available_width() + GAP) / (CARD_W + GAP))
            .floor()
            .max(1.0) as usize;
        let mut go: Option<Nav> = None;
        for chunk in SettingsSection::ALL.chunks(cols) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for sec in chunk {
                    let sec = *sec;
                    let status = sec.module().map(|m| {
                        let st = cx.snap.modules.get(&m);
                        (
                            st.map(u::tone_for_module).unwrap_or(Tone::Neutral),
                            st.map(u::module_label)
                                .unwrap_or_else(|| tr("sem estado ainda").to_string()),
                        )
                    });
                    // cartão: moldura pintada + conteúdo num filho com layout vertical
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(CARD_W, CARD_H), egui::Sense::click());
                    let hovered = resp.hovered();
                    let fill = if hovered { pal.card_alt } else { pal.card };
                    let stroke =
                        egui::Stroke::new(1.0, if hovered { pal.accent } else { pal.border });
                    ui.painter()
                        .rect(rect, 10.0, fill, stroke, egui::StrokeKind::Inside);
                    if let Some((tone, _)) = &status {
                        // filete colorido à esquerda com o estado do módulo
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 1.0, rect.top() + 14.0),
                                egui::vec2(3.0, rect.height() - 28.0),
                            ),
                            2.0,
                            tone.color(dark),
                        );
                    }
                    let inner = rect.shrink2(egui::vec2(16.0, 12.0));
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(inner)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    child.set_clip_rect(inner);
                    child.spacing_mut().item_spacing.y = 6.0;
                    child.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(sec.label())
                                .font(u::semibold(15.0))
                                .color(pal.text),
                        );
                        if let Some((tone, label)) = &status {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    u::badge(ui, label, *tone);
                                },
                            );
                        }
                    });
                    child.add(
                        egui::Label::new(
                            egui::RichText::new(sec.description())
                                .size(12.5)
                                .color(pal.text_weak),
                        )
                        .wrap(),
                    );
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.clicked() {
                        go = Some(Nav::Settings(sec));
                    }
                }
            });
            ui.add_space(GAP);
        }
        if let Some(n) = go {
            *cx.go = Some(n);
        }
    }
}
