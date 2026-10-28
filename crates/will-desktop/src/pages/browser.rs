//! Browser CDP: status do proxy local do Chrome DevTools Protocol, instalação
//! do agent-browser (CLI, Chromium e skill na base de conhecimento), testes de
//! ponta a ponta e o tráfego CDP ao vivo num visor estilo terminal.
//!
//! Dados: handlers de `will_server::api::browser` (chamados em processo). O
//! tráfego é buscado por polling a cada 1 s (`after` = última `seq` vista) e
//! mantido localmente até [`TRAFFIC_MAX`] mensagens; o status recarrega sozinho
//! a cada [`Event::Browser`].

use super::{Cx, Nav, SettingsSection};
use crate::help;
use crate::i18n::{tr, trf};
use crate::service::{j, Loader, Task};
use crate::ui::{self, Tone};
use axum::extract::{Query, State};
use axum::Json;
use egui::{Color32, FontId, RichText, TextFormat};
use serde_json::Value;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use will_core::Event;
use will_server::api::browser as api;

/// Teto de mensagens CDP guardadas localmente.
const TRAFFIC_MAX: usize = 1000;
/// Quantas mensagens pedir por polling.
const TRAFFIC_PAGE: usize = 500;
/// Intervalo do polling de tráfego e do job de instalação.
const POLL_EVERY: Duration = Duration::from_secs(1);
/// Recarga do status quando nenhum evento chega.
const STATUS_EVERY: Duration = Duration::from_secs(15);
const FONT_SIZE: f32 = 12.5;
/// Altura do visor de tráfego (a página inteira rola; o visor tem altura fixa).
const TRAFFIC_HEIGHT: f32 = 380.0;

// ---------- cores do visor (iguais nos dois temas: é um terminal) ----------

const PANEL_BG: Color32 = Color32::from_rgb(14, 16, 20);
const PANEL_BORDER: Color32 = Color32::from_rgb(36, 40, 48);
const TEXT: Color32 = Color32::from_rgb(208, 213, 222);
const TEXT_DIM: Color32 = Color32::from_rgb(118, 126, 140);
const ROW_HOVER: Color32 = Color32::from_rgb(26, 30, 38);
const KIND_COMMAND: Color32 = Color32::from_rgb(110, 170, 255);
const KIND_EVENT: Color32 = Color32::from_rgb(150, 158, 172);
const KIND_ERROR: Color32 = Color32::from_rgb(255, 96, 96);
const OK_COLOR: Color32 = Color32::from_rgb(104, 196, 140);

/// Tipo de mensagem CDP (campo `kind` do serviço).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Command,
    Response,
    Event,
    Error,
    Other,
}

impl Kind {
    fn parse(s: &str) -> Kind {
        match s {
            "command" => Kind::Command,
            "response" => Kind::Response,
            "event" => Kind::Event,
            "error" => Kind::Error,
            _ => Kind::Other,
        }
    }

    /// Valor do filtro de tipo (chave estável, independe do idioma).
    fn key(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Response => "response",
            Kind::Event => "event",
            Kind::Error => "error",
            Kind::Other => "other",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Kind::Command => KIND_COMMAND,
            Kind::Response => TEXT,
            Kind::Event => KIND_EVENT,
            Kind::Error => KIND_ERROR,
            Kind::Other => TEXT_DIM,
        }
    }

    fn row_fill(self) -> Option<Color32> {
        match self {
            Kind::Error => Some(Color32::from_rgb(58, 22, 26)),
            _ => None,
        }
    }
}

/// Uma mensagem CDP já convertida do JSON do serviço.
struct TrafficEntry {
    seq: u64,
    time: String,
    to_browser: bool,
    conn: String,
    id: Option<i64>,
    method: String,
    session_id: String,
    kind: Kind,
    preview: String,
    bytes: i64,
    /// `método + preview + sessão` em minúsculas, para o filtro de texto.
    haystack: String,
}

impl TrafficEntry {
    fn from_json(v: &Value) -> Self {
        let method = ui::s(v, "method");
        let preview = ui::s(v, "preview");
        let session_id = ui::s(v, "session_id");
        let haystack = format!("{method} {preview} {session_id}").to_lowercase();
        let time = v
            .get("at")
            .and_then(|a| a.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%H:%M:%S%.3f")
                    .to_string()
            })
            .unwrap_or_else(|| ui::ellipsize(&ui::s(v, "at"), 12));
        let conn = match v.get("conn") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        Self {
            seq: v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0),
            time,
            to_browser: ui::s(v, "dir") == "to_browser",
            conn,
            id: v.get("id").and_then(|i| i.as_i64()),
            method,
            session_id,
            kind: Kind::parse(&ui::s(v, "kind")),
            preview,
            bytes: ui::i(v, "bytes"),
            haystack,
        }
    }

    /// Linha em texto puro (para copiar).
    fn plain(&self) -> String {
        format!(
            "{} {} {} {} {} {}",
            self.time,
            if self.to_browser { "→" } else { "←" },
            self.id.map(|i| format!("#{i}")).unwrap_or_default(),
            self.method,
            self.session_id,
            self.preview
        )
    }
}

/// Etapa em curso de "Instalar tudo".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallAll {
    /// Esperando o job do `npm i -g agent-browser` terminar com sucesso.
    AgentBrowser,
    /// Skill pedida; ao terminar, recarrega o status.
    Skill,
}

#[derive(Default)]
pub struct BrowserPage {
    status: Loader<Value>,
    install: Loader<Value>,
    job: Loader<Value>,
    last_job_poll: Option<Instant>,
    /// `running` do último job visto (detecta a transição rodando → terminado).
    job_was_running: bool,
    /// Ações rápidas em voo: (texto do toast de sucesso, tarefa). Devolvem status.
    actions: Vec<(String, Task<Value>)>,
    launch: Option<Task<Value>>,
    /// Pedido de instalação em voo (devolve o job, ou `{ok, path, job}` na skill).
    install_task: Option<(&'static str, Task<Value>)>,
    install_all: Option<InstallAll>,
    // ---- testes ----
    test_upstream: Loader<Value>,
    test_tab: Loader<Value>,
    test_tab_url: String,
    test_agent: Loader<Value>,
    // ---- tráfego ----
    traffic: Loader<Value>,
    last_traffic_poll: Option<Instant>,
    entries: Vec<TrafficEntry>,
    last_seq: u64,
    traffic_filter: String,
    /// `""` (todos) ou a chave de [`Kind::key`].
    traffic_kind: String,
    paused: bool,
    expanded: HashSet<u64>,
    clear_task: Option<Task<Value>>,
}

impl BrowserPage {
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::Browser { .. } => self.status.loaded_at = None,
            Event::SettingsChanged { .. } => {
                self.status.loaded_at = None;
                self.install.loaded_at = None;
            }
            _ => {}
        }
    }

    // ---------- chamadas ----------

    fn reload_status(&mut self, cx: &Cx) {
        self.status.start(
            cx.svc
                .call(|s| async move { j(api::status(State(s)).await) }),
        );
    }

    fn reload_install(&mut self, cx: &Cx) {
        self.install.start(
            cx.svc
                .call(|s| async move { j(api::install_status(State(s)).await) }),
        );
    }

    fn poll_job(&mut self, cx: &Cx) {
        self.last_job_poll = Some(Instant::now());
        self.job.start(
            cx.svc
                .call(|s| async move { j(api::install_job(State(s)).await) }),
        );
    }

    fn poll_traffic(&mut self, cx: &Cx) {
        self.last_traffic_poll = Some(Instant::now());
        let after = self.last_seq;
        self.traffic.start(cx.svc.call(move |s| async move {
            j(api::traffic(
                State(s),
                Query(api::TrafficQuery {
                    after: Some(after),
                    limit: Some(TRAFFIC_PAGE),
                }),
            )
            .await)
        }));
    }

    fn start_install(&mut self, cx: &Cx, step: &'static str) {
        let task = match step {
            "agent-browser" => cx
                .svc
                .call(|s| async move { j(api::install_agent_browser(State(s)).await) }),
            "chromium" => cx
                .svc
                .call(|s| async move { j(api::install_chromium(State(s)).await) }),
            _ => cx
                .svc
                .call(|s| async move { j(api::install_skill(State(s)).await) }),
        };
        self.install_task = Some((step, task));
    }

    /// Job atual (`None` quando o serviço nunca rodou nenhum).
    fn job_value(&self) -> Option<&Value> {
        self.job.data.as_ref().filter(|j| {
            j.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| !n.is_empty())
        })
    }

    fn job_running(&self) -> bool {
        self.job_value().is_some_and(|j| ui::b(j, "running"))
    }

    /// Absorve uma resposta de job (dos handlers de instalação ou do polling).
    fn absorb_job(&mut self, cx: &mut Cx, job: Value) {
        let running = ui::b(&job, "running");
        let name = ui::s(&job, "name");
        if self.job_was_running && !running && !name.is_empty() {
            // Transição rodando → terminado: toast, recarga e próxima etapa.
            match job.get("ok").and_then(|o| o.as_bool()) {
                Some(true) => {
                    cx.toasts.success(trf("\"{}\" concluído.", &[&name]));
                    self.install.loaded_at = None;
                    self.status.loaded_at = None;
                    if self.install_all == Some(InstallAll::AgentBrowser) {
                        self.install_all = Some(InstallAll::Skill);
                        self.start_install(cx, "skill");
                    } else if self.install_all == Some(InstallAll::Skill) {
                        self.install_all = None;
                    }
                }
                _ => {
                    cx.toasts
                        .error(trf("\"{}\" falhou; veja a saída abaixo.", &[&name]));
                    self.install.loaded_at = None;
                    self.install_all = None;
                }
            }
        }
        self.job_was_running = running;
        self.job.data = Some(job);
        self.job.error = None;
        self.job.loaded_at = Some(Instant::now());
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.status.stale(STATUS_EVERY) {
            self.reload_status(cx);
        }
        self.status.poll();
        if self.install.stale(Duration::from_secs(60)) {
            self.reload_install(cx);
        }
        self.install.poll();

        // Lançar/fechar: devolvem o status novo.
        if let Some(t) = &self.launch {
            if let Some(r) = t.take() {
                self.launch = None;
                match r {
                    Ok(v) => {
                        cx.toasts.success(tr("Navegador respondendo."));
                        self.status.data = Some(v);
                        self.status.error = None;
                        self.status.loaded_at = Some(Instant::now());
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }
        let mut results = Vec::new();
        self.actions.retain(|(text, t)| match t.take() {
            Some(r) => {
                results.push((text.clone(), r));
                false
            }
            None => true,
        });
        for (text, r) in results {
            match r {
                Ok(v) => {
                    cx.toasts.success(text);
                    if v.is_object() && v.get("listening").is_some() {
                        self.status.data = Some(v);
                        self.status.error = None;
                        self.status.loaded_at = Some(Instant::now());
                    } else {
                        self.status.loaded_at = None;
                    }
                }
                Err(e) => cx.toasts.error(e),
            }
        }

        // Pedido de instalação: a resposta já traz o job.
        if let Some((step, t)) = &self.install_task {
            if let Some(r) = t.take() {
                let step = *step;
                self.install_task = None;
                match r {
                    Ok(v) => {
                        let job = if step == "skill" {
                            if let Some(p) = v.get("path").and_then(|p| p.as_str()) {
                                cx.toasts.success(trf("Skill instalada em {}", &[&p]));
                            }
                            self.install.loaded_at = None;
                            v.get("job").cloned().unwrap_or(Value::Null)
                        } else {
                            v
                        };
                        if job.is_object() {
                            // Marca como "estava rodando" só se o job devolvido roda:
                            // assim a transição é detectada no próximo polling.
                            self.job_was_running = ui::b(&job, "running");
                            self.job.data = Some(job);
                            self.job.error = None;
                            self.job.loaded_at = Some(Instant::now());
                        }
                        // Encadeamento de "Instalar tudo" quando o job devolvido já
                        // terminou (sem transição a detectar no polling).
                        if !self.job_running() {
                            match (step, self.install_all) {
                                ("agent-browser", Some(InstallAll::AgentBrowser)) => {
                                    let ok = self
                                        .job
                                        .data
                                        .as_ref()
                                        .and_then(|j| j.get("ok"))
                                        .and_then(|o| o.as_bool())
                                        .unwrap_or(false);
                                    if ok {
                                        self.install_all = Some(InstallAll::Skill);
                                        self.start_install(cx, "skill");
                                    } else {
                                        self.install_all = None;
                                    }
                                }
                                ("skill", Some(InstallAll::Skill)) => self.install_all = None,
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        cx.toasts.error(e);
                        self.install_all = None;
                    }
                }
            }
        }

        // Job: polling a cada 1 s enquanto roda (ou até a primeira leitura).
        let need_job =
            self.job.loaded_at.is_none() || self.job_running() || self.install_all.is_some();
        if need_job
            && !self.job.busy()
            && self
                .last_job_poll
                .map(|t| t.elapsed() >= POLL_EVERY)
                .unwrap_or(true)
        {
            self.poll_job(cx);
        }
        if self.job.poll() {
            if let Some(job) = self.job.data.take() {
                self.absorb_job(cx, job);
            }
        }

        // Testes.
        self.test_upstream.poll();
        self.test_tab.poll();
        self.test_agent.poll();

        // Tráfego.
        if !self.paused
            && !self.traffic.busy()
            && self
                .last_traffic_poll
                .map(|t| t.elapsed() >= POLL_EVERY)
                .unwrap_or(true)
        {
            self.poll_traffic(cx);
        }
        if self.traffic.poll() {
            if let Some(v) = self.traffic.data.take() {
                self.absorb_traffic(&v);
            }
        }
        if let Some(t) = &self.clear_task {
            if let Some(r) = t.take() {
                self.clear_task = None;
                if let Err(e) = r {
                    cx.toasts.error(e);
                }
            }
        }
    }

    fn absorb_traffic(&mut self, v: &Value) {
        if let Some(list) = v.get("entries").and_then(|e| e.as_array()) {
            for e in list {
                let entry = TrafficEntry::from_json(e);
                if entry.seq > self.last_seq {
                    self.last_seq = entry.seq;
                }
                self.entries.push(entry);
            }
        }
        // O serviço pode ter limpado o log (seq volta): acompanha.
        if let Some(seq) = v.get("last_seq").and_then(|s| s.as_u64()) {
            if seq < self.last_seq {
                self.last_seq = seq;
            }
        }
        if self.entries.len() > TRAFFIC_MAX {
            let drop = self.entries.len() - TRAFFIC_MAX;
            self.entries.drain(..drop);
        }
    }

    // ---------- UI ----------

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);
        // Polling de tráfego/job mesmo sem o usuário mexer.
        if !self.paused || self.job_running() || self.install_all.is_some() {
            ui.ctx().request_repaint_after(POLL_EVERY);
        }

        ui::heading_help(
            ui,
            tr("Browser CDP"),
            Some(tr(
                "Proxy local do Chrome DevTools Protocol para o agent-browser: status, instalação, testes e tráfego ao vivo.",
            )),
            &help::BROWSER,
        );

        if !cx.snap.settings.browser.enabled {
            let dark = ui.visuals().dark_mode;
            let warn = Tone::Warning.color(dark);
            egui::Frame::new()
                .fill(warn.linear_multiply(if dark { 0.18 } else { 0.12 }))
                .stroke(egui::Stroke::new(1.0, warn))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(tr(
                            "Browser CDP desligado nas configurações: o proxy não escuta e o agente não recebe AGENT_BROWSER_CDP.",
                        )).color(warn));
                        if ui.button(tr("Configurar")).clicked() {
                            *cx.go = Some(Nav::Settings(SettingsSection::Browser));
                        }
                    });
                });
            ui.add_space(6.0);
        }

        self.status_section(ui, cx);
        self.install_section(ui, cx);
        self.tests_section(ui, cx);
        self.traffic_section(ui, cx);
    }

    // ---------- Status ----------

    fn status_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let mut do_launch = false;
        let mut do_close = false;
        let mut do_refresh = false;
        let launching = self.launch.as_ref().is_some_and(|t| t.pending());
        let status = self.status.data.clone();
        let error = self.status.error.clone();
        let loading = self.status.loading();

        ui::section(ui, tr("Status"), |ui| {
            let dark = ui.visuals().dark_mode;
            if let Some(e) = &error {
                ui::error_text(ui, e);
            }
            let Some(s) = &status else {
                if loading {
                    ui::busy(ui, tr("consultando o proxy…"));
                }
                return;
            };

            // Proxy.
            ui.horizontal(|ui| {
                let listening = ui::b(s, "listening");
                ui::dot(
                    ui,
                    if listening {
                        Tone::Success
                    } else {
                        Tone::Neutral
                    },
                );
                if listening {
                    ui.label(trf("Proxy escutando em {}", &[&ui::s(s, "listen_url")]));
                } else {
                    ui.label(tr("Proxy desligado"));
                }
                let env = ui::s(s, "cdp_env");
                if !env.is_empty() {
                    ui.label(RichText::new(env).monospace().small().weak())
                        .on_hover_text(tr(
                            "Variável AGENT_BROWSER_CDP injetada nos processos do claude",
                        ));
                }
            });

            // Navegador real.
            ui.horizontal(|ui| {
                let up = ui::b(s, "upstream_up");
                let launching_now = ui::b(s, "launching") || launching;
                ui::dot(
                    ui,
                    if up {
                        Tone::Success
                    } else if launching_now {
                        Tone::Info
                    } else {
                        Tone::Warning
                    },
                );
                let who = s
                    .get("upstream_browser")
                    .and_then(|b| b.as_str())
                    .filter(|b| !b.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| {
                        if launching_now {
                            tr("lançando…").to_string()
                        } else {
                            tr("ninguém respondendo").to_string()
                        }
                    });
                let text = trf("Navegador: {} → {}", &[&ui::s(s, "upstream"), &who]);
                if up {
                    ui.label(text);
                } else {
                    ui.label(RichText::new(text).color(Tone::Warning.color(dark)));
                }
                if launching_now {
                    ui.spinner();
                }
            });

            // Lançado por nós.
            if let Some(l) = s.get("launched").filter(|l| l.is_object()) {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(18.0);
                    ui.label(RichText::new(tr("lançado por nós:")).small().weak());
                    ui.label(
                        RichText::new(trf("pid {}", &[&ui::i(l, "pid")]))
                            .small()
                            .monospace(),
                    );
                    ui.label(RichText::new(ui::s(l, "path")).small().monospace())
                        .on_hover_text(ui::s(l, "path"));
                    ui::badge(
                        ui,
                        if ui::b(l, "headless") {
                            tr("headless")
                        } else {
                            tr("visível")
                        },
                        Tone::Info,
                    );
                    let since = ui::fmt_dt(&l["started_at"]);
                    if !since.is_empty() {
                        ui.label(RichText::new(trf("desde {}", &[&since])).small().weak());
                    }
                });
            }

            let resolved = ui::s(s, "resolved_browser");
            let profile = ui::s(s, "user_data_dir");
            if !resolved.is_empty() || !profile.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(18.0);
                    if !resolved.is_empty() {
                        ui.label(RichText::new(tr("executável:")).small().weak());
                        ui.label(RichText::new(&resolved).small().monospace());
                    }
                    if !profile.is_empty() {
                        ui.label(RichText::new(tr("perfil:")).small().weak());
                        ui.label(RichText::new(&profile).small().monospace());
                    }
                });
            }

            if let Some(e) = s
                .get("last_error")
                .and_then(|e| e.as_str())
                .filter(|e| !e.is_empty())
            {
                ui.add(
                    egui::Label::new(
                        RichText::new(trf("Último erro: {}", &[&e]))
                            .color(Tone::Danger.color(dark)),
                    )
                    .wrap(),
                );
            }

            // Contadores.
            ui.add_space(4.0);
            let c = &s["counters"];
            ui.horizontal_wrapped(|ui| {
                ui::badge(
                    ui,
                    &trf("{} comandos", &[&ui::i(c, "commands")]),
                    Tone::Info,
                );
                ui::badge(
                    ui,
                    &trf("{} respostas", &[&ui::i(c, "responses")]),
                    Tone::Neutral,
                );
                ui::badge(
                    ui,
                    &trf("{} eventos", &[&ui::i(c, "events")]),
                    Tone::Neutral,
                );
                let errors = ui::i(c, "errors");
                ui::badge(
                    ui,
                    &trf("{} erros", &[&errors]),
                    if errors > 0 {
                        Tone::Danger
                    } else {
                        Tone::Neutral
                    },
                );
                ui::badge(
                    ui,
                    &trf(
                        "{} conexões ativas / {} no total",
                        &[&ui::i(s, "active_connections"), &ui::i(s, "connections")],
                    ),
                    Tone::Neutral,
                );
                ui.label(
                    RichText::new(trf(
                        "↑ {} · ↓ {}",
                        &[
                            &fmt_bytes(ui::i(c, "bytes_to_browser")),
                            &fmt_bytes(ui::i(c, "bytes_from_browser")),
                        ],
                    ))
                    .small()
                    .weak(),
                )
                .on_hover_text(tr("bytes enviados ao navegador · bytes recebidos dele"));
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let enabled = ui::b(s, "enabled");
                if ui
                    .add_enabled(
                        enabled && !launching,
                        egui::Button::new(tr("Subir navegador")),
                    )
                    .on_hover_text(tr(
                        "Garante um navegador respondendo (lança um se preciso; pode demorar).",
                    ))
                    .clicked()
                {
                    do_launch = true;
                }
                if launching {
                    ui.spinner();
                }
                let launched = s.get("launched").is_some_and(|l| l.is_object());
                if ui
                    .add_enabled(
                        launched && !launching,
                        egui::Button::new(tr("Fechar navegador")),
                    )
                    .on_hover_text(tr("Fecha só o navegador que nós lançamos."))
                    .clicked()
                {
                    do_close = true;
                }
                if ui.button(tr("Atualizar")).clicked() {
                    do_refresh = true;
                }
                if loading {
                    ui.spinner();
                }
            });
        });

        if do_launch {
            self.launch = Some(
                cx.svc
                    .call(|s| async move { j(api::launch(State(s)).await) }),
            );
        }
        if do_close {
            let task = cx
                .svc
                .call(|s| async move { j(api::close(State(s)).await) });
            self.actions
                .push((tr("Navegador fechado.").to_string(), task));
        }
        if do_refresh {
            self.status.loaded_at = None;
            self.install.loaded_at = None;
        }
    }

    // ---------- Instalação ----------

    fn install_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let mut action: Option<&'static str> = None;
        let mut install_all = false;
        let install = self.install.data.clone();
        let install_err = self.install.error.clone();
        let loading = self.install.loading();
        let job_running = self.job_running();
        let requesting = self.install_task.as_ref().is_some_and(|(_, t)| t.pending());
        let chaining = self.install_all.is_some();
        let busy = job_running || requesting || chaining;
        let job = self.job_value().cloned();

        ui::section(ui, tr("Instalação"), |ui| {
            let dark = ui.visuals().dark_mode;
            if let Some(e) = &install_err {
                ui::error_text(ui, e);
            }
            let Some(s) = &install else {
                if loading {
                    ui::busy(ui, tr("verificando…"));
                }
                return;
            };
            let row = |ui: &mut egui::Ui, label: &str, ok: bool, text: &str| {
                ui.horizontal(|ui| {
                    ui::dot(ui, if ok { Tone::Success } else { Tone::Warning });
                    ui.label(label);
                    ui.add(egui::Label::new(RichText::new(text).weak()).truncate())
                        .on_hover_text(text);
                });
            };
            let opt = |s: &Value, key: &str| -> Option<String> {
                s.get(key)
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
                    .map(String::from)
            };
            let with_version = |path: Option<String>, version: Option<String>| -> String {
                match (path, version) {
                    (Some(p), Some(v)) => format!("{v} · {p}"),
                    (Some(p), None) => p,
                    (None, Some(v)) => v,
                    (None, None) => String::new(),
                }
            };

            let node = opt(s, "node");
            let npm = opt(s, "npm");
            let agent = opt(s, "agent_browser");
            let chrome = opt(s, "agent_browser_chrome");
            let kb = opt(s, "kb_path");
            let skill_ok = ui::b(s, "skill_in_kb");

            let node_text = with_version(node.clone(), opt(s, "node_version"));
            row(
                ui,
                "Node.js",
                node.is_some(),
                if node.is_some() {
                    &node_text
                } else {
                    tr("não encontrado (instale o Node.js 18+)")
                },
            );
            let npm_text = with_version(npm.clone(), opt(s, "npm_version"));
            row(
                ui,
                "npm",
                npm.is_some(),
                if npm.is_some() {
                    &npm_text
                } else {
                    tr("não encontrado (vem com o Node.js)")
                },
            );
            let agent_text = with_version(agent.clone(), opt(s, "agent_browser_version"));
            row(
                ui,
                "agent-browser",
                agent.is_some(),
                if agent.is_some() {
                    &agent_text
                } else {
                    tr("não instalado")
                },
            );
            row(
                ui,
                tr("Chromium do agent-browser"),
                chrome.is_some(),
                chrome
                    .as_deref()
                    .unwrap_or(tr("não baixado (opcional se já há um Chrome)")),
            );
            let skill_path = opt(s, "skill_in_kb_path");
            let skill_text = match (&kb, skill_ok, &skill_path) {
                (None, _, _) => tr("base de conhecimento não configurada").to_string(),
                (Some(_), true, Some(p)) => p.clone(),
                (Some(_), true, None) => tr("ok").to_string(),
                (Some(_), false, Some(p)) => trf("falta · {}", &[&p]),
                (Some(_), false, None) => tr("falta").to_string(),
            };
            row(
                ui,
                tr("Skill na base de conhecimento"),
                kb.is_some() && skill_ok,
                &skill_text,
            );
            if let Some(w) = s.get("warnings").and_then(|w| w.as_array()) {
                for w in w {
                    if let Some(t) = w.as_str() {
                        ui.colored_label(Tone::Warning.color(dark), format!("! {t}"));
                    }
                }
            }

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        npm.is_some() && !busy,
                        egui::Button::new(RichText::new(tr("Instalar tudo")).color(Color32::WHITE))
                            .fill(ui::palette(dark).accent),
                    )
                    .on_hover_text(tr(
                        "agent-browser (npm i -g) e, ao terminar, a skill na base de conhecimento.",
                    ))
                    .clicked()
                {
                    install_all = true;
                }
                if ui
                    .add_enabled(
                        npm.is_some() && !busy,
                        egui::Button::new(tr("Instalar/atualizar agent-browser")),
                    )
                    .on_hover_text("npm i -g agent-browser")
                    .clicked()
                {
                    action = Some("agent-browser");
                }
                if ui
                    .add_enabled(
                        kb.is_some() && !busy,
                        egui::Button::new(tr("Instalar skill na base de conhecimento")),
                    )
                    .on_hover_text(tr(
                        "Copia a skill para <kb>/.claude/skills/agent-browser/SKILL.md",
                    ))
                    .clicked()
                {
                    action = Some("skill");
                }
                if ui
                    .add_enabled(
                        agent.is_some() && !busy,
                        egui::Button::new(tr("Baixar Chromium do agent-browser")),
                    )
                    .on_hover_text("agent-browser install")
                    .clicked()
                {
                    action = Some("chromium");
                }
                if ui.button(tr("Atualizar")).clicked() {
                    action = Some("refresh");
                }
                if requesting || loading {
                    ui.spinner();
                }
            });

            // Painel do job (estilo terminal).
            if let Some(j) = &job {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ui::s(j, "name")).font(ui::medium(13.5)));
                    if ui::b(j, "running") {
                        ui.spinner();
                        ui::badge(ui, tr("rodando"), Tone::Info);
                    } else {
                        match j.get("ok").and_then(|o| o.as_bool()) {
                            Some(true) => ui::badge(ui, tr("ok"), Tone::Success),
                            Some(false) => ui::badge(ui, tr("falhou"), Tone::Danger),
                            None => {}
                        }
                    }
                    if chaining {
                        ui::badge(ui, tr("instalando tudo…"), Tone::Info);
                    }
                    let started = ui::fmt_dt(&j["started_at"]);
                    if !started.is_empty() {
                        ui.label(RichText::new(started).small().weak());
                    }
                });
                let output = ui::s(j, "output");
                terminal_box(ui, "browser-job-output", &output, 220.0, true);
            }
        });

        if install_all {
            self.install_all = Some(InstallAll::AgentBrowser);
            self.start_install(cx, "agent-browser");
        } else if let Some(a) = action {
            if a == "refresh" {
                self.install.loaded_at = None;
                self.job.loaded_at = None;
            } else {
                self.start_install(cx, a);
            }
        }
    }

    // ---------- Testes ----------

    fn tests_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let mut run_upstream = false;
        let mut run_tab = false;
        let mut run_agent = false;

        ui::section(ui, tr("Testes"), |ui| {
            let dark = ui.visuals().dark_mode;

            // 1. /json/version direto no navegador.
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.test_upstream.loading(),
                        egui::Button::new(tr("Testar navegador (/json/version)")),
                    )
                    .clicked()
                {
                    run_upstream = true;
                }
                if self.test_upstream.loading() {
                    ui.spinner();
                }
            });
            if let Some(e) = &self.test_upstream.error {
                ui.add(egui::Label::new(RichText::new(e).color(Tone::Danger.color(dark))).wrap());
            } else if let Some(v) = &self.test_upstream.data {
                let ver = &v["version"];
                ui.horizontal_wrapped(|ui| {
                    ui::dot(ui, Tone::Success);
                    ui.label(RichText::new(ui::s(ver, "Browser")).font(ui::medium(13.5)));
                    let ws = ui::s(ver, "webSocketDebuggerUrl");
                    if !ws.is_empty() {
                        ui.label(RichText::new(ws).monospace().small().weak());
                    }
                });
            }
            ui.add_space(6.0);

            // 2. Aba nova pelo proxy.
            ui.horizontal(|ui| {
                let w = (ui.available_width() * 0.4).clamp(160.0, 360.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.test_tab_url)
                        .hint_text("about:blank")
                        .font(egui::TextStyle::Monospace)
                        .desired_width(w),
                );
                ui::text_context_menu(&r);
                if ui
                    .add_enabled(
                        !self.test_tab.loading(),
                        egui::Button::new(tr("Abrir aba de teste pelo proxy")),
                    )
                    .on_hover_text(tr(
                        "Passa pelo proxy: valida a reescrita das URLs e o fallback de lançamento.",
                    ))
                    .clicked()
                {
                    run_tab = true;
                }
                if self.test_tab.loading() {
                    ui.spinner();
                }
            });
            if let Some(e) = &self.test_tab.error {
                ui.add(egui::Label::new(RichText::new(e).color(Tone::Danger.color(dark))).wrap());
            } else if let Some(v) = &self.test_tab.data {
                let t = &v["target"];
                ui.horizontal_wrapped(|ui| {
                    ui::dot(ui, Tone::Success);
                    ui.label(trf("Aba aberta: {}", &[&ui::s(t, "url")]));
                    let id = ui::s(t, "id");
                    if !id.is_empty() {
                        ui.label(RichText::new(id).monospace().small().weak());
                    }
                });
            }
            ui.add_space(6.0);

            // 3. agent-browser de ponta a ponta.
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.test_agent.loading(),
                        egui::Button::new(tr("Testar agent-browser de ponta a ponta")),
                    )
                    .on_hover_text(tr(
                        "Roda o CLI contra o proxy, como o agente faria (até 60 s).",
                    ))
                    .clicked()
                {
                    run_agent = true;
                }
                if self.test_agent.loading() {
                    ui.spinner();
                    ui.label(
                        RichText::new(tr("rodando o CLI… (até 60 s)"))
                            .weak()
                            .small(),
                    );
                }
            });
            if let Some(e) = &self.test_agent.error {
                ui.add(egui::Label::new(RichText::new(e).color(Tone::Danger.color(dark))).wrap());
            } else if let Some(v) = &self.test_agent.data {
                let ok = ui::b(v, "ok");
                ui.horizontal(|ui| {
                    ui::dot(ui, if ok { Tone::Success } else { Tone::Danger });
                    ui.label(if ok {
                        tr("agent-browser funcionou de ponta a ponta.")
                    } else {
                        tr("agent-browser falhou; veja a saída.")
                    });
                });
                terminal_box(ui, "browser-agent-test", &ui::s(v, "output"), 200.0, ok);
            }
        });

        if run_upstream {
            self.test_upstream.start(
                cx.svc
                    .call(|s| async move { j(api::test_upstream(State(s)).await) }),
            );
        }
        if run_tab {
            let url = self.test_tab_url.trim().to_string();
            let body = Some(Json(api::NewTabBody {
                url: Some(url).filter(|u| !u.is_empty()),
            }));
            self.test_tab.start(
                cx.svc
                    .call(move |s| async move { j(api::test_new_tab(State(s), body).await) }),
            );
        }
        if run_agent {
            self.test_agent.start(
                cx.svc
                    .call(|s| async move { j(api::test_agent_browser(State(s)).await) }),
            );
        }
    }

    // ---------- Tráfego ----------

    fn traffic_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let mut do_clear = false;
        let mut copy_visible = false;

        ui::section(ui, tr("Tráfego CDP (ao vivo)"), |ui| {
            // Barra.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.traffic_filter)
                        .desired_width(240.0)
                        .hint_text(tr("filtrar (método ou conteúdo)")),
                );
                ui::text_context_menu(&r);
                if !self.traffic_filter.is_empty()
                    && ui
                        .small_button("×")
                        .on_hover_text(tr("limpar filtro"))
                        .clicked()
                {
                    self.traffic_filter.clear();
                }
                ui.label(RichText::new(tr("Tipo")).weak());
                let options: [(&str, &str); 5] = [
                    ("", tr("todos")),
                    (Kind::Command.key(), tr("comandos")),
                    (Kind::Response.key(), tr("respostas")),
                    (Kind::Event.key(), tr("eventos")),
                    (Kind::Error.key(), tr("erros")),
                ];
                let current = options
                    .iter()
                    .find(|(k, _)| *k == self.traffic_kind.as_str())
                    .map(|(_, l)| *l)
                    .unwrap_or(tr("todos"));
                egui::ComboBox::from_id_salt("browser-traffic-kind")
                    .selected_text(current)
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for (k, l) in options {
                            ui.selectable_value(&mut self.traffic_kind, k.to_string(), l);
                        }
                    });
                ui.separator();
                ui.checkbox(&mut self.paused, tr("Pausar"));
                if ui
                    .button(tr("Limpar"))
                    .on_hover_text(tr("Esvazia o log no serviço e na tela"))
                    .clicked()
                {
                    do_clear = true;
                }
                if ui.button(tr("Copiar visíveis")).clicked() {
                    copy_visible = true;
                }
                if self.traffic.loading() && !self.paused {
                    ui.label(RichText::new("·").weak());
                }
            });
            if let Some(e) = &self.traffic.error {
                ui::error_text(ui, e);
            }
            ui.add_space(4.0);
            self.traffic_panel(ui);

            // Rodapé.
            let shown = self.visible_indices().len();
            ui.horizontal(|ui| {
                let counter = if shown == self.entries.len() {
                    trf(
                        "{} mensagens · última seq {}",
                        &[&self.entries.len(), &self.last_seq],
                    )
                } else {
                    trf(
                        "{} mensagens ({} filtradas) · última seq {}",
                        &[&self.entries.len(), &shown, &self.last_seq],
                    )
                };
                ui.label(RichText::new(counter).weak().small());
                if self.paused {
                    ui::badge(ui, tr("pausado"), Tone::Neutral);
                }
            });
        });

        if copy_visible {
            let text: String = self
                .visible_indices()
                .iter()
                .map(|&i| self.entries[i].plain())
                .collect::<Vec<_>>()
                .join("\n");
            let n = self.visible_indices().len();
            ui.ctx().copy_text(text);
            cx.toasts.success(trf("{} linhas copiadas.", &[&n]));
        }
        if do_clear {
            self.entries.clear();
            self.expanded.clear();
            self.last_seq = 0;
            self.clear_task = Some(
                cx.svc
                    .call(|s| async move { j(api::traffic_clear(State(s)).await) }),
            );
        }
    }

    /// Índices das mensagens que passam pelo filtro de texto e de tipo.
    fn visible_indices(&self) -> Vec<usize> {
        let needle = self.traffic_filter.trim().to_lowercase();
        let kind = self.traffic_kind.as_str();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| kind.is_empty() || e.kind.key() == kind)
            .filter(|(_, e)| needle.is_empty() || e.haystack.contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    fn traffic_panel(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(FONT_SIZE);
        let line_h = ui
            .painter()
            .layout_no_wrap("X".to_owned(), font.clone(), TEXT)
            .size()
            .y;
        let row_h = (line_h + 3.0).round();
        let visible = self.visible_indices();
        let uniform = self.expanded.is_empty();

        egui::Frame::new()
            .fill(PANEL_BG)
            .stroke(egui::Stroke::new(1.0, PANEL_BORDER))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(0, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(TRAFFIC_HEIGHT);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                ui.style_mut().interaction.selectable_labels = true;
                ui.visuals_mut().selection.bg_fill = Color32::from_rgb(52, 84, 140);

                let scroll = egui::ScrollArea::vertical()
                    .id_salt("browser-traffic")
                    .auto_shrink([false, false])
                    .stick_to_bottom(!self.paused);

                if visible.is_empty() {
                    scroll.show(ui, |ui| {
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            let msg = if self.entries.is_empty() {
                                tr("nenhuma mensagem ainda; peça ao agente para abrir uma página")
                            } else {
                                tr("nenhuma mensagem corresponde ao filtro")
                            };
                            ui.label(RichText::new(msg).font(font.clone()).color(TEXT_DIM));
                        });
                    });
                    return;
                }

                let mut toggled: Option<u64> = None;
                if uniform {
                    scroll.show_rows(ui, row_h, visible.len(), |ui, range| {
                        for vi in range {
                            let e = &self.entries[visible[vi]];
                            if traffic_row(ui, e, &font, row_h, false) {
                                toggled = Some(e.seq);
                            }
                        }
                    });
                } else {
                    scroll.show(ui, |ui| {
                        for &idx in &visible {
                            let e = &self.entries[idx];
                            let wrap = self.expanded.contains(&e.seq);
                            if traffic_row(ui, e, &font, row_h, wrap) {
                                toggled = Some(e.seq);
                            }
                        }
                    });
                }
                if let Some(k) = toggled {
                    if !self.expanded.remove(&k) {
                        self.expanded.insert(k);
                    }
                }
            });
    }
}

// ---------- helpers privados ----------

/// Desenha uma mensagem CDP; devolve `true` quando a linha foi clicada
/// (para expandir/recolher o preview).
fn traffic_row(ui: &mut egui::Ui, e: &TrafficEntry, font: &FontId, row_h: f32, wrap: bool) -> bool {
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(&format!(" {} ", e.time), 0.0, fmt(TEXT_DIM));
    job.append(
        if e.to_browser { " → " } else { " ← " },
        0.0,
        fmt(if e.to_browser { KIND_COMMAND } else { OK_COLOR }),
    );
    let id = e.id.map(|i| format!("#{i}")).unwrap_or_default();
    job.append(&format!("{id:>6} "), 0.0, fmt(TEXT_DIM));
    if !e.method.is_empty() {
        job.append(&format!("{} ", e.method), 0.0, fmt(e.kind.color()));
    }
    if !e.session_id.is_empty() {
        job.append(
            &format!("({}) ", ui::ellipsize(&e.session_id, 8)),
            0.0,
            fmt(TEXT_DIM),
        );
    }
    if !e.conn.is_empty() {
        job.append(&format!("c{} ", e.conn), 0.0, fmt(TEXT_DIM));
    }
    job.append(
        &e.preview,
        0.0,
        fmt(if e.kind == Kind::Error {
            Color32::from_rgb(255, 206, 206)
        } else {
            TEXT
        }),
    );
    if !wrap {
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
    }

    let bg = ui.painter().add(egui::Shape::Noop);
    let width = ui.available_width();
    let label = if wrap {
        egui::Label::new(job).wrap().selectable(true)
    } else {
        egui::Label::new(job).truncate().selectable(true)
    };
    let inner = ui.allocate_ui_with_layout(
        egui::vec2(width, row_h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| ui.add(label),
    );
    let resp = inner.inner;
    let rect = egui::Rect::from_min_max(
        egui::pos2(inner.response.rect.left(), inner.response.rect.top()),
        egui::pos2(
            inner.response.rect.left() + width,
            inner.response.rect.bottom(),
        ),
    );
    let hovered = ui.rect_contains_pointer(rect);
    let fill = match (e.kind.row_fill(), hovered) {
        (Some(c), true) => Some(c.linear_multiply(1.35)),
        (Some(c), false) => Some(c),
        (None, true) => Some(ROW_HOVER),
        (None, false) => None,
    };
    if let Some(c) = fill {
        ui.painter().set(bg, egui::Shape::rect_filled(rect, 0.0, c));
    }
    let resp = resp.on_hover_text(format!("seq {} · {} bytes", e.seq, e.bytes));
    resp.clicked() && !resp.dragged()
}

/// Caixa de saída de processo (fundo escuro, monoespaçado, rolável), com a cor
/// do texto conforme o resultado.
fn terminal_box(ui: &mut egui::Ui, id: &str, text: &str, max_height: f32, ok: bool) {
    egui::Frame::new()
        .fill(PANEL_BG)
        .stroke(egui::Stroke::new(1.0, PANEL_BORDER))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.style_mut().interaction.selectable_labels = true;
            egui::ScrollArea::vertical()
                .id_salt(id)
                .max_height(max_height)
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let shown = if text.trim().is_empty() {
                        tr("(sem saída ainda)")
                    } else {
                        text
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(shown)
                                .font(FontId::monospace(FONT_SIZE))
                                .color(if text.trim().is_empty() {
                                    TEXT_DIM
                                } else if ok {
                                    TEXT
                                } else {
                                    Color32::from_rgb(255, 206, 206)
                                }),
                        )
                        .wrap()
                        .selectable(true),
                    );
                });
        });
}

/// Bytes em forma legível (B, KB, MB).
fn fmt_bytes(n: i64) -> String {
    let n = n.max(0) as f64;
    if n < 1024.0 {
        format!("{n:.0} B")
    } else if n < 1024.0 * 1024.0 {
        format!("{:.1} KB", n / 1024.0)
    } else {
        format!("{:.1} MB", n / (1024.0 * 1024.0))
    }
}
