//! will-browser — proxy local do Chrome DevTools Protocol (CDP) para o
//! agent-browser, com lançamento do navegador sob demanda (`docs/25`).
//!
//! O agente recebe `AGENT_BROWSER_CDP=http://127.0.0.1:<listen_port>`. O
//! agent-browser faz `GET /json/version`, lê `webSocketDebuggerUrl` e abre o
//! WebSocket. O proxy:
//! - repassa o HTTP de descoberta (`/json*`) ao navegador com `Host` em
//!   loopback (o Chrome rejeita outros) e reescreve `host:porta` nas URLs
//!   devolvidas para apontar para si;
//! - repassa cada WebSocket nos dois sentidos, registrando as mensagens no
//!   log de tráfego da página Browser CDP;
//! - antes de qualquer repasse, garante o upstream (`ensure_upstream`):
//!   1. `GET /json/version` em cada host candidato (`localhost` = `127.0.0.1` **e** `[::1]`);
//!   2. porta aberta mas sem `/json/*` (Chrome sem `--remote-allow-origins`, caso do perfil
//!      principal do usuário com depuração ligada por outro app): lê `DevToolsActivePort`
//!      do perfil dedicado e, com `attach_existing`, dos perfis do usuário, e valida com um
//!      handshake WebSocket + `Browser.getVersion`. Nesse modo o proxy **sintetiza**
//!      `/json/version`, `/json/list`, `/json/new`, `/json/activate`, `/json/close` por CDP;
//!   3. porta ocupada por algo que não reconhece → erro acionável (**não** lança um segundo
//!      navegador na mesma porta);
//!   4. porta livre e `auto_launch` → sobe o navegador com `--remote-debugging-port` + perfil
//!      dedicado e espera ele responder.
//!
//! Erros em `/json/*` voltam como `503` **JSON** (`{"error", "hint", "upstream"}`), que o
//! agent-browser consegue mostrar. Ver `docs/25` e `docs/diagnosticos/2026-10-28-…`.

pub mod detect;
pub mod discover;
pub mod install;
pub mod traffic;

use axum::body::Body;
use axum::extract::ws::{Message as AxMsg, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use futures::{SinkExt, StreamExt};
use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::tungstenite::Message as TgMsg;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use traffic::Traffic;
use will_core::settings::BrowserSettings;
use will_core::{Event, EventBus, Module, ModuleStatus};

pub use install::{InstallStatus, Installer, Job};
pub use traffic::{Counters, TrafficEntry};

pub const FAMILY: &str = "browser";

#[derive(Debug, Clone, serde::Serialize)]
pub struct LaunchedBrowser {
    pub pid: Option<u32>,
    pub path: String,
    pub headless: bool,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

/// Navegador upstream reconhecido: como foi achado e como falar com ele.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Upstream {
    /// `host:porta` que de fato respondeu (ex.: `127.0.0.1:9222` ou `[::1]:9222`).
    pub authority: String,
    /// `ws://<authority>/devtools/browser/<token>`.
    pub browser_ws: String,
    pub browser: Option<String>,
    /// `true` = `/json/*` do navegador funciona (o proxy repassa); `false` = o proxy sintetiza por CDP.
    pub http_discovery: bool,
    /// `http` ou `DevToolsActivePort (<perfil>)`.
    pub source: String,
}

/// Conexão CDP de controle (uma só, reaproveitada) usada para sintetizar a
/// descoberta e para `Browser.close`. No perfil principal do Chrome cada
/// conexão nova dispara o diálogo "Allow remote debugging?"; por isso é única.
struct ControlConn {
    url: String,
    ws: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BrowserStatus {
    pub enabled: bool,
    pub listening: bool,
    pub listen_url: String,
    pub cdp_env: String,
    pub upstream: String,
    pub upstream_up: bool,
    pub upstream_browser: Option<String>,
    /// Autoridade que respondeu, como foi descoberta e se o `/json/*` dela funciona.
    pub upstream_authority: Option<String>,
    pub upstream_source: Option<String>,
    pub upstream_http_discovery: bool,
    pub launched: Option<LaunchedBrowser>,
    pub launching: bool,
    pub connections: u32,
    pub active_connections: u32,
    pub counters: Counters,
    pub last_seq: u64,
    pub last_error: Option<String>,
    pub resolved_browser: Option<String>,
    pub user_data_dir: String,
}

pub struct BrowserDeps {
    pub bus: EventBus,
    pub data_dir: PathBuf,
    /// Pasta da base de conhecimento (para a skill); pode mudar com as settings.
    pub kb_path: RwLock<Option<PathBuf>>,
}

pub struct BrowserModule {
    deps: BrowserDeps,
    cfg: RwLock<BrowserSettings>,
    pub traffic: Traffic,
    pub installer: Installer,
    http: reqwest::Client,
    server: RwLock<Option<CancellationToken>>,
    listening: RwLock<bool>,
    child: AsyncMutex<Option<tokio::process::Child>>,
    launched: RwLock<Option<LaunchedBrowser>>,
    launch_lock: AsyncMutex<()>,
    launching: RwLock<bool>,
    conn_seq: AtomicU32,
    active: AtomicU32,
    last_error: RwLock<Option<String>>,
    upstream_browser: RwLock<Option<String>>,
    upstream: RwLock<Option<Upstream>>,
    control: AsyncMutex<Option<ControlConn>>,
    cdp_seq: AtomicU32,
}

impl BrowserModule {
    pub fn new(deps: BrowserDeps, cfg: BrowserSettings) -> Arc<Self> {
        let installer = Installer::new(deps.bus.clone());
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Arc::new(Self {
            deps,
            traffic: Traffic::new(cfg.traffic_capacity as usize),
            installer,
            cfg: RwLock::new(cfg),
            http,
            server: RwLock::new(None),
            listening: RwLock::new(false),
            child: AsyncMutex::new(None),
            launched: RwLock::new(None),
            launch_lock: AsyncMutex::new(()),
            launching: RwLock::new(false),
            conn_seq: AtomicU32::new(0),
            active: AtomicU32::new(0),
            last_error: RwLock::new(None),
            upstream_browser: RwLock::new(None),
            upstream: RwLock::new(None),
            control: AsyncMutex::new(None),
            cdp_seq: AtomicU32::new(0),
        })
    }

    pub fn config(&self) -> BrowserSettings {
        self.cfg.read().clone()
    }
    pub fn enabled(&self) -> bool {
        self.cfg.read().enabled
    }
    pub fn set_kb_path(&self, kb: Option<PathBuf>) {
        *self.deps.kb_path.write() = kb;
    }
    pub fn kb_path(&self) -> Option<PathBuf> {
        self.deps.kb_path.read().clone()
    }

    /// URL que vai no ambiente do claude (`AGENT_BROWSER_CDP`).
    pub fn cdp_env_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.cfg.read().listen_port)
    }

    fn upstream_authority(&self) -> String {
        let c = self.cfg.read();
        format!("{}:{}", c.upstream_host, c.upstream_port)
    }

    fn upstream_is_local(&self) -> bool {
        let h = self.cfg.read().upstream_host.to_lowercase();
        matches!(h.as_str(), "127.0.0.1" | "localhost" | "::1" | "[::1]")
    }

    pub fn user_data_dir(&self) -> PathBuf {
        match self.cfg.read().user_data_dir.as_deref().map(str::trim) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => self.deps.data_dir.join("browser-profile"),
        }
    }

    fn log(&self, level: &str, msg: impl Into<String>) {
        self.deps.bus.log(level, FAMILY, msg);
    }

    fn emit(&self, state: &str, detail: Option<String>) {
        self.deps.bus.emit(Event::Browser {
            state: state.to_string(),
            detail,
        });
    }

    fn set_status(&self, s: ModuleStatus) {
        self.deps.bus.emit(Event::ModuleStatus {
            module: Module::Browser,
            status: s,
        });
    }

    fn set_error(&self, e: Option<String>) {
        *self.last_error.write() = e;
    }

    // ----- ciclo de vida -----

    /// Aplica configuração nova; reinicia o listener se porta/enabled mudaram.
    pub async fn update_config(self: &Arc<Self>, cfg: BrowserSettings) {
        let (old_port, old_enabled) = {
            let c = self.cfg.read();
            (c.listen_port, c.enabled)
        };
        *self.cfg.write() = cfg.clone();
        if old_port != cfg.listen_port || old_enabled != cfg.enabled {
            self.stop().await;
            self.start().await;
        }
    }

    pub async fn start(self: &Arc<Self>) {
        if !self.enabled() {
            self.set_status(ModuleStatus::Disabled);
            return;
        }
        if self.server.read().is_some() {
            return;
        }
        let port = self.cfg.read().listen_port;
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                let msg = format!("não consegui escutar em {addr}: {e}");
                self.set_error(Some(msg.clone()));
                self.log("error", msg.clone());
                self.set_status(ModuleStatus::Error(msg));
                return;
            }
        };
        let token = CancellationToken::new();
        *self.server.write() = Some(token.clone());
        *self.listening.write() = true;
        let app = Router::new()
            .fallback(proxy_handler)
            .with_state(self.clone());
        let me = self.clone();
        tokio::spawn(async move {
            let shutdown = token.clone();
            let r = axum::serve(listener, app)
                .with_graceful_shutdown(async move { shutdown.cancelled().await })
                .await;
            *me.listening.write() = false;
            if let Err(e) = r {
                me.log("error", format!("proxy CDP encerrou com erro: {e}"));
            }
        });
        self.set_error(None);
        self.log(
            "info",
            format!(
                "proxy CDP escutando em http://127.0.0.1:{port} → {}",
                self.upstream_authority()
            ),
        );
        self.emit("listening", Some(format!("127.0.0.1:{port}")));
        self.ensure_skill().await;
        self.refresh_readiness();
    }

    /// O que ainda falta para o agente navegar (barato: só olha o disco).
    /// `None` = pronto. Vira o motivo do `Degraded` na bolinha do módulo.
    pub fn readiness(&self) -> Option<String> {
        if detect::find_node_tool("agent-browser").is_none() {
            return Some(if detect::find_node_tool("npm").is_some() {
                "agent-browser não instalado: Browser CDP → Instalação → Instalar agent-browser"
                    .into()
            } else {
                "Node.js/npm não encontrados: instale o Node.js e depois o agent-browser em Browser CDP → Instalação".into()
            });
        }
        if let Some(kb) = self.kb_path() {
            if kb.is_dir() && !install::skill_path_in_kb(&kb).is_file() {
                return Some(
                    "skill agent-browser ausente na base de conhecimento: Browser CDP → Instalação"
                        .into(),
                );
            }
        }
        None
    }

    /// Recalcula a bolinha: `Running` quando pronto, `Degraded` com o que falta.
    /// Não sobrescreve um erro de proxy/navegador em curso.
    pub fn refresh_readiness(&self) {
        if !self.enabled() || !*self.listening.read() || self.last_error.read().is_some() {
            return;
        }
        match self.readiness() {
            None => self.set_status(ModuleStatus::Running),
            Some(reason) => self.set_status(ModuleStatus::Degraded(reason)),
        }
    }

    /// Copia a skill para a KB quando o CLI existe e ela ainda não está lá
    /// (instalação do zero sem cliques). Silencioso se não há KB ou CLI.
    pub async fn ensure_skill(&self) {
        let Some(kb) = self.kb_path().filter(|p| p.is_dir()) else {
            return;
        };
        if install::skill_path_in_kb(&kb).is_file()
            || detect::find_node_tool("agent-browser").is_none()
        {
            return;
        }
        match self.installer.install_skill(&kb).await {
            Ok(p) => self.log(
                "info",
                format!("skill agent-browser instalada automaticamente em {p}"),
            ),
            Err(e) => self.log(
                "warn",
                format!("não consegui instalar a skill agent-browser na KB: {e}"),
            ),
        }
    }

    pub async fn stop(&self) {
        if let Some(t) = self.server.write().take() {
            t.cancel();
        }
        *self.listening.write() = false;
        *self.control.lock().await = None;
        if self.cfg.read().close_browser_on_stop {
            self.close_browser().await;
        }
        self.set_status(if self.enabled() {
            ModuleStatus::Stopped
        } else {
            ModuleStatus::Disabled
        });
    }

    // ----- upstream / lançamento -----

    fn set_upstream(&self, up: Upstream) {
        *self.upstream_browser.write() = up.browser.clone();
        *self.upstream.write() = Some(up);
    }

    fn clear_upstream(&self) {
        *self.upstream.write() = None;
    }

    /// Último upstream reconhecido (sem sondar).
    pub fn upstream(&self) -> Option<Upstream> {
        self.upstream.read().clone()
    }

    /// `GET /json/version` numa autoridade concreta (sem passar pelo proxy).
    async fn http_version(
        &self,
        authority: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let url = format!("http://{authority}/json/version");
        let r = tokio::time::timeout(timeout, self.http.get(&url).send())
            .await
            .map_err(|_| "tempo esgotado".to_string())?
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("HTTP {}", r.status()));
        }
        r.json().await.map_err(|e| e.to_string())
    }

    /// Sondagem completa do upstream (ver cabeçalho do módulo). Guarda o resultado
    /// em `upstream` para o repasse/síntese e o `Browser` para o status.
    pub async fn probe_upstream(&self, timeout: Duration) -> Result<Upstream, String> {
        let (host_cfg, port, attach_existing) = {
            let c = self.cfg.read();
            (c.upstream_host.clone(), c.upstream_port, c.attach_existing)
        };
        let hosts = discover::candidate_hosts(&host_cfg);
        let mut errors: Vec<String> = vec![];
        let mut open: Vec<String> = vec![];
        for h in &hosts {
            let authority = format!("{h}:{port}");
            if !tcp_open(&authority, timeout).await {
                errors.push(format!("{authority}: nada escutando"));
                continue;
            }
            open.push(authority.clone());
            match self.http_version(&authority, timeout).await {
                Ok(v) => {
                    let ws_path = v
                        .get("webSocketDebuggerUrl")
                        .and_then(|u| u.as_str())
                        .and_then(ws_path_of)
                        .unwrap_or_else(|| "/devtools/browser".to_string());
                    let up = Upstream {
                        browser_ws: format!("ws://{authority}{ws_path}"),
                        authority,
                        browser: v.get("Browser").and_then(|b| b.as_str()).map(String::from),
                        http_discovery: true,
                        source: "http".into(),
                    };
                    self.set_upstream(up.clone());
                    return Ok(up);
                }
                Err(e) => errors.push(format!("{authority}: /json/version → {e}")),
            }
        }
        if open.is_empty() {
            self.clear_upstream();
            return Err(errors.join("; "));
        }
        // Porta aberta, mas sem HTTP de descoberta: DevToolsActivePort + handshake.
        let mut profiles = vec![discover::ProfileDir {
            name: "perfil dedicado".into(),
            dir: self.user_data_dir(),
        }];
        if attach_existing {
            profiles.extend(discover::user_profile_dirs());
        }
        let mut tried = 0usize;
        for prof in &profiles {
            let Some(dap) = discover::read_devtools_active_port(&prof.dir) else {
                continue;
            };
            let dedicated = prof.name == "perfil dedicado";
            // Perfis do usuário só valem na porta configurada; o dedicado pode ter mudado.
            if dap.port != port && !dedicated {
                continue;
            }
            for h in &hosts {
                let authority = format!("{h}:{}", dap.port);
                let ws = format!("ws://{authority}{}", dap.path);
                tried += 1;
                // Já validado antes: não refaz o handshake (cada conexão nova pode
                // disparar "Allow remote debugging?" no perfil principal do Chrome).
                let cached = self.upstream.read().clone();
                if let Some(cur) = cached {
                    if cur.browser_ws == ws && !cur.http_discovery {
                        return Ok(cur);
                    }
                }
                match self
                    .cdp_call(
                        &ws,
                        "Browser.getVersion",
                        serde_json::json!({}),
                        Duration::from_secs(4),
                    )
                    .await
                {
                    Ok(v) => {
                        let up = Upstream {
                            authority,
                            browser_ws: ws,
                            browser: v.get("product").and_then(|b| b.as_str()).map(String::from),
                            http_discovery: false,
                            source: format!(
                                "DevToolsActivePort ({}: {})",
                                prof.name,
                                prof.dir.display()
                            ),
                        };
                        self.log(
                            "info",
                            format!(
                                "navegador reconhecido por DevToolsActivePort em {} ({}; {}); /json/* será sintetizado pelo proxy",
                                up.authority,
                                up.browser.clone().unwrap_or_default(),
                                prof.name
                            ),
                        );
                        self.set_upstream(up.clone());
                        return Ok(up);
                    }
                    Err(e) => errors.push(format!("{ws}: {e}")),
                }
            }
        }
        self.clear_upstream();
        let busy = open.join(", ");
        let hint = if tried == 0 {
            format!(
                "porta ocupada ({busy}) por um processo sem /json/version e sem DevToolsActivePort compatível{}",
                if attach_existing {
                    ""
                } else {
                    " (attach_existing desligado: perfis do usuário não foram olhados)"
                }
            )
        } else {
            format!("porta ocupada ({busy}) e o handshake CDP falhou")
        };
        Err(format!("{hint}; detalhes: {}", errors.join("; ")))
    }

    /// Alguém escuta em `upstream_port` (em qualquer host candidato)?
    async fn port_busy(&self) -> bool {
        let (host_cfg, port) = {
            let c = self.cfg.read();
            (c.upstream_host.clone(), c.upstream_port)
        };
        for h in discover::candidate_hosts(&host_cfg) {
            if tcp_open(&format!("{h}:{port}"), Duration::from_millis(500)).await {
                return true;
            }
        }
        false
    }

    /// Chamada CDP pela conexão de controle (única, reaproveitada; reconecta uma vez).
    async fn cdp_call(
        &self,
        ws: &str,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let mut guard = self.control.lock().await;
        let mut last = String::from("sem conexão");
        for attempt in 0..2 {
            if guard.as_ref().map(|c| c.url != ws).unwrap_or(true) {
                let (stream, _) =
                    tokio::time::timeout(timeout, tokio_tungstenite::connect_async(ws))
                        .await
                        .map_err(|_| "tempo esgotado no handshake WebSocket".to_string())?
                        .map_err(|e| e.to_string())?;
                *guard = Some(ControlConn {
                    url: ws.to_string(),
                    ws: stream,
                });
            }
            let Some(conn) = guard.as_mut() else {
                return Err(last);
            };
            let id = i64::from(self.cdp_seq.fetch_add(1, Ordering::Relaxed)) + 1;
            let msg = serde_json::json!({"id": id, "method": method, "params": params}).to_string();
            let r: Result<serde_json::Value, String> = async {
                conn.ws
                    .send(TgMsg::Text(msg.into()))
                    .await
                    .map_err(|e| e.to_string())?;
                loop {
                    let next = tokio::time::timeout(timeout, conn.ws.next())
                        .await
                        .map_err(|_| format!("tempo esgotado esperando {method}"))?;
                    let Some(m) = next else {
                        return Err("conexão CDP fechada".to_string());
                    };
                    let m = m.map_err(|e| e.to_string())?;
                    if let TgMsg::Text(t) = m {
                        let v: serde_json::Value =
                            serde_json::from_str(t.as_str()).map_err(|e| e.to_string())?;
                        if v.get("id").and_then(|i| i.as_i64()) == Some(id) {
                            if let Some(err) = v.get("error") {
                                return Err(err
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("erro CDP")
                                    .to_string());
                            }
                            return Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null));
                        }
                    }
                }
            }
            .await;
            match r {
                Ok(v) => return Ok(v),
                Err(e) => {
                    *guard = None;
                    last = e;
                    if attempt == 1 {
                        break;
                    }
                }
            }
        }
        Err(last)
    }

    /// Garante que há um navegador reconhecido no upstream (lançando se preciso).
    pub async fn ensure_upstream(&self) -> Result<(), String> {
        let probe_err = match self.probe_upstream(Duration::from_millis(800)).await {
            Ok(_) => {
                if self.last_error.read().is_some() {
                    self.set_error(None);
                    self.set_status(ModuleStatus::Running);
                }
                return Ok(());
            }
            Err(e) => e,
        };
        let (auto, wait, port) = {
            let c = self.cfg.read();
            (c.auto_launch, c.launch_wait_secs.max(3), c.upstream_port)
        };
        // Porta ocupada por algo que não reconhecemos: lançar outro navegador na
        // mesma porta só produz um segundo Chrome preso em `[::1]` (ou nem sobe).
        if self.port_busy().await {
            let msg = format!(
                "porta {port} ocupada por um navegador que não aceita a descoberta CDP ({probe_err}). \
                 Opções: mude `upstream_port` (ex.: 9224) para o will-ia lançar o próprio navegador, \
                 ou abra esse navegador com --remote-allow-origins=*"
            );
            self.fail(msg.clone());
            return Err(msg);
        }
        if !auto {
            let msg = format!(
                "nenhum navegador respondendo em {} e lançamento automático desligado",
                self.upstream_authority()
            );
            self.fail(msg.clone());
            return Err(msg);
        }
        if !self.upstream_is_local() {
            let msg = format!(
                "upstream {} não é local; não dá para lançar o navegador daqui",
                self.upstream_authority()
            );
            self.fail(msg.clone());
            return Err(msg);
        }
        let _g = self.launch_lock.lock().await;
        // outro pedido pode ter lançado enquanto esperávamos o trinco
        if self
            .probe_upstream(Duration::from_millis(800))
            .await
            .is_ok()
        {
            return Ok(());
        }
        if let Err(e) = self.launch_browser().await {
            self.fail(e.clone());
            return Err(e);
        }
        let deadline = Instant::now() + Duration::from_secs(wait as u64);
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if self
                .probe_upstream(Duration::from_millis(800))
                .await
                .is_ok()
            {
                self.set_error(None);
                self.set_status(ModuleStatus::Running);
                let browser = self.upstream_browser.read().clone();
                self.emit("browser_up", browser);
                return Ok(());
            }
            if let Some(c) = self.child.lock().await.as_mut() {
                if let Ok(Some(st)) = c.try_wait() {
                    // wrappers (google-chrome) saem logo; só é erro se o upstream não subir
                    tracing::debug!(status = ?st, "processo lançador do navegador terminou");
                }
            }
        }
        let msg = format!(
            "navegador lançado mas {} não respondeu em {wait} s",
            self.upstream_authority()
        );
        self.fail(msg.clone());
        Err(msg)
    }

    /// Registra a falha, loga e põe o módulo em `Degraded` com o motivo (regra 7).
    fn fail(&self, msg: String) {
        self.set_error(Some(msg.clone()));
        self.log("warn", msg.clone());
        if *self.listening.read() {
            self.set_status(ModuleStatus::Degraded(msg));
        }
    }

    /// Caminho do navegador: configurado ou o primeiro detectado.
    pub async fn resolve_browser(&self) -> Option<String> {
        let configured = self.cfg.read().browser_path.clone();
        if let Some(p) = configured.filter(|p| !p.trim().is_empty()) {
            return Some(p);
        }
        let ab_chrome = match detect::find_node_tool("agent-browser") {
            Some(ab) => install::agent_browser_chrome(&ab).await,
            None => None,
        };
        detect::detect_browsers(ab_chrome.as_deref())
            .first()
            .map(|c| c.path.clone())
    }

    /// Sobe o navegador com CDP no `upstream_port` e perfil dedicado.
    pub async fn launch_browser(&self) -> Result<LaunchedBrowser, String> {
        let path = self
            .resolve_browser()
            .await
            .ok_or_else(|| "nenhum navegador encontrado (Chrome, Chromium, Brave, Edge). Instale um ou use \"Baixar Chromium do agent-browser\".".to_string())?;
        let (port, headless, extra) = {
            let c = self.cfg.read();
            (c.upstream_port, c.headless, c.extra_args.clone())
        };
        let profile = self.user_data_dir();
        std::fs::create_dir_all(&profile)
            .map_err(|e| format!("perfil {}: {e}", profile.display()))?;
        *self.launching.write() = true;
        let mut cmd = tokio::process::Command::new(&path);
        cmd.arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--remote-allow-origins=*");
        // Servidor sem sessão gráfica: headless obrigatório.
        let headless = headless || !will_platform::has_desktop();
        if headless {
            cmd.arg("--headless=new");
        }
        for a in extra.iter().filter(|a| !a.trim().is_empty()) {
            cmd.arg(a);
        }
        cmd.arg("about:blank")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false);
        will_core::process::hide_console(&mut cmd);
        will_platform::process::die_with_parent(&mut cmd);
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let child = cmd.spawn().map_err(|e| {
            *self.launching.write() = false;
            format!("não consegui executar {path}: {e}")
        })?;
        will_platform::process::attach_to_job(&child);
        let info = LaunchedBrowser {
            pid: child.id(),
            path: path.clone(),
            headless,
            started_at: chrono::Utc::now(),
        };
        *self.child.lock().await = Some(child);
        *self.launched.write() = Some(info.clone());
        *self.launching.write() = false;
        self.log(
            "info",
            format!(
                "navegador lançado: {path} (porta {port}, {}; perfil {})",
                if headless { "headless" } else { "visível" },
                profile.display()
            ),
        );
        self.emit("launched", Some(path));
        Ok(info)
    }

    /// Fecha o navegador que nós lançamos (não mexe em navegadores de terceiros).
    pub async fn close_browser(&self) {
        let mut g = self.child.lock().await;
        if let Some(mut c) = g.take() {
            #[cfg(unix)]
            if let Some(pid) = c.id() {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &format!("-{pid}")])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            let _ = c.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), c.wait()).await;
            self.log("info", "navegador lançado por nós foi fechado");
        }
        // Chrome real pode ter ficado (wrapper saiu): tenta Browser.close pelo CDP.
        if self.launched.read().is_some() {
            if let Ok(up) = self.probe_upstream(Duration::from_millis(500)).await {
                let _ = self
                    .cdp_call(
                        &up.browser_ws,
                        "Browser.close",
                        serde_json::json!({}),
                        Duration::from_secs(2),
                    )
                    .await;
            }
        }
        *self.control.lock().await = None;
        self.clear_upstream();
        *self.launched.write() = None;
        self.emit("closed", None);
    }

    // ----- testes da página -----

    /// Abre uma aba nova passando pelo próprio proxy (valida reescrita + fallback).
    pub async fn test_new_tab(&self, url: &str) -> Result<serde_json::Value, String> {
        let port = self.cfg.read().listen_port;
        let u = format!("http://127.0.0.1:{port}/json/new?{url}");
        let r = self.http.put(&u).send().await.map_err(|e| e.to_string())?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "HTTP {status}: {}",
                will_core::tools::truncate(&text, 300)
            ));
        }
        serde_json::from_str(&text).map_err(|e| format!("resposta inesperada: {e}"))
    }

    /// Roda `agent-browser --cdp <proxy> open about:blank` (e `close`) de ponta a ponta.
    pub async fn test_agent_browser(&self) -> Result<String, String> {
        let ab = detect::find_node_tool("agent-browser").ok_or("agent-browser não instalado")?;
        let port = self.cfg.read().listen_port;
        let mut cmd = tokio::process::Command::new(&ab);
        cmd.args([
            "--cdp",
            &format!("http://127.0.0.1:{port}"),
            "--session",
            "will-ia-test",
            "open",
            "about:blank",
        ])
        .env("PATH", detect::path_with(&ab))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
        will_core::process::hide_console(&mut cmd);
        let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
            .await
            .map_err(|_| "tempo esgotado (60 s)".to_string())?
            .map_err(|e| e.to_string())?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // encerra a sessão de teste sem fechar o navegador
        let mut close = tokio::process::Command::new(&ab);
        close
            .args([
                "--cdp",
                &format!("http://127.0.0.1:{port}"),
                "--session",
                "will-ia-test",
                "close",
            ])
            .env("PATH", detect::path_with(&ab))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        will_core::process::hide_console(&mut close);
        let _ = tokio::time::timeout(Duration::from_secs(20), close.output()).await;
        if out.status.success() {
            Ok(will_core::tools::truncate(text.trim(), 600))
        } else {
            Err(will_core::tools::truncate(text.trim(), 600))
        }
    }

    pub async fn status(&self) -> BrowserStatus {
        self.refresh_readiness();
        let c = self.cfg.read().clone();
        let probe = self.probe_upstream(Duration::from_millis(600)).await.ok();
        let up = probe.is_some();
        // Guardas de `RwLock` não podem atravessar `.await`: resolve antes do literal.
        let resolved_browser = self.resolve_browser().await;
        let upstream_browser = probe.as_ref().and_then(|u| u.browser.clone());
        let upstream_authority = probe.as_ref().map(|u| u.authority.clone());
        let upstream_source = probe.as_ref().map(|u| u.source.clone());
        let upstream_http_discovery = probe.as_ref().is_some_and(|u| u.http_discovery);
        let launched = self.launched.read().clone();
        let launching = *self.launching.read();
        let listening = *self.listening.read();
        let last_error = self.last_error.read().clone();
        BrowserStatus {
            enabled: c.enabled,
            listening,
            listen_url: format!("http://127.0.0.1:{}", c.listen_port),
            cdp_env: self.cdp_env_url(),
            upstream: self.upstream_authority(),
            upstream_up: up,
            upstream_browser,
            upstream_authority,
            upstream_source,
            upstream_http_discovery,
            launched,
            launching,
            connections: self.conn_seq.load(Ordering::Relaxed),
            active_connections: self.active.load(Ordering::Relaxed),
            counters: self.traffic.counters(),
            last_seq: self.traffic.last_seq(),
            last_error,
            resolved_browser,
            user_data_dir: self.user_data_dir().to_string_lossy().to_string(),
        }
    }

    pub async fn detect(&self) -> Vec<detect::BrowserCandidate> {
        let ab_chrome = match detect::find_node_tool("agent-browser") {
            Some(ab) => install::agent_browser_chrome(&ab).await,
            None => None,
        };
        detect::detect_browsers(ab_chrome.as_deref())
    }

    /// Reescreve `host:porta` do upstream (qualquer grafia de loopback, o host
    /// configurado e a autoridade que respondeu) para o proxy nas respostas de descoberta.
    fn rewrite_discovery(&self, body: &str) -> String {
        let (host, port, listen) = {
            let c = self.cfg.read();
            (c.upstream_host.clone(), c.upstream_port, c.listen_port)
        };
        let responding = self
            .upstream
            .read()
            .as_ref()
            .and_then(|u| u.authority.rsplit_once(':').map(|(h, _)| h.to_string()));
        discover::rewrite_authorities(body, &host, responding.as_deref(), port, listen)
    }

    /// `/json/*` sintetizado por CDP quando o navegador não tem HTTP de descoberta.
    async fn synthesize_json(&self, up: &Upstream, path_q: &str) -> Response<Body> {
        let listen = self.cfg.read().listen_port;
        let me = format!("127.0.0.1:{listen}");
        let (path, query) = match path_q.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (path_q, None),
        };
        let path = path.trim_end_matches('/');
        let t = Duration::from_secs(5);
        let page = |t: &serde_json::Value| {
            let id = t.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
            serde_json::json!({
                "id": id,
                "type": t.get("type").cloned().unwrap_or_else(|| "page".into()),
                "title": t.get("title").cloned().unwrap_or_else(|| "".into()),
                "url": t.get("url").cloned().unwrap_or_else(|| "".into()),
                "description": "",
                "devtoolsFrontendUrl": format!("/devtools/inspector.html?ws={me}/devtools/page/{id}"),
                "webSocketDebuggerUrl": format!("ws://{me}/devtools/page/{id}"),
            })
        };
        let json = |v: serde_json::Value| -> Response<Body> {
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json; charset=UTF-8")
                .body(Body::from(v.to_string()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        };
        let text = |s: &str| -> Response<Body> {
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/plain; charset=UTF-8")
                .body(Body::from(s.to_string()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        };
        let ws = up.browser_ws.as_str();
        let out: Result<Response<Body>, String> = match path {
            "/json/version" => self
                .cdp_call(ws, "Browser.getVersion", serde_json::json!({}), t)
                .await
                .map(|v| {
                    json(serde_json::json!({
                        "Browser": v.get("product").cloned().unwrap_or_default(),
                        "Protocol-Version": v.get("protocolVersion").cloned().unwrap_or_else(|| "1.3".into()),
                        "User-Agent": v.get("userAgent").cloned().unwrap_or_default(),
                        "V8-Version": v.get("jsVersion").cloned().unwrap_or_default(),
                        "WebKit-Version": v.get("revision").cloned().unwrap_or_default(),
                        "webSocketDebuggerUrl": format!("ws://{me}{}", ws_path_of(ws).unwrap_or_default()),
                    }))
                }),
            "/json" | "/json/list" => self
                .cdp_call(ws, "Target.getTargets", serde_json::json!({}), t)
                .await
                .map(|v| {
                    let items: Vec<serde_json::Value> = v
                        .get("targetInfos")
                        .and_then(|a| a.as_array())
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .filter(|ti| ti.get("type").and_then(|x| x.as_str()) != Some("browser"))
                        .map(page)
                        .collect();
                    json(serde_json::Value::Array(items))
                }),
            "/json/new" => {
                let url = query.filter(|q| !q.is_empty()).unwrap_or("about:blank");
                self.cdp_call(ws, "Target.createTarget", serde_json::json!({"url": url}), t)
                    .await
                    .map(|v| {
                        json(page(&serde_json::json!({
                            "targetId": v.get("targetId").cloned().unwrap_or_default(),
                            "type": "page", "title": "", "url": url,
                        })))
                    })
            }
            p if p.starts_with("/json/activate/") => {
                let id = p.trim_start_matches("/json/activate/");
                self.cdp_call(ws, "Target.activateTarget", serde_json::json!({"targetId": id}), t)
                    .await
                    .map(|_| text("Target activated"))
            }
            p if p.starts_with("/json/close/") => {
                let id = p.trim_start_matches("/json/close/");
                self.cdp_call(ws, "Target.closeTarget", serde_json::json!({"targetId": id}), t)
                    .await
                    .map(|_| text("Target is closing"))
            }
            _ => {
                return json_error(
                    StatusCode::NOT_IMPLEMENTED,
                    format!("{path} não é suportado quando o navegador não tem /json/* (descoberta por DevToolsActivePort)"),
                    Some(up.authority.clone()),
                    None,
                )
            }
        };
        match out {
            Ok(r) => r,
            Err(e) => {
                self.set_error(Some(format!("CDP {}: {e}", up.authority)));
                json_error(
                    StatusCode::BAD_GATEWAY,
                    format!("CDP {}: {e}", up.authority),
                    Some(up.authority.clone()),
                    None,
                )
            }
        }
    }
}

/// `host:porta` aceita conexão TCP?
async fn tcp_open(authority: &str, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, tokio::net::TcpStream::connect(authority)).await,
        Ok(Ok(_))
    )
}

/// Caminho (`/devtools/...`) de uma URL `ws://host:porta/devtools/...`.
fn ws_path_of(url: &str) -> Option<String> {
    url.find("/devtools/").map(|i| url[i..].to_string())
}

/// Erro do proxy em JSON (o agent-browser lê `/json/*` como JSON e mostra `error`).
fn json_error(
    status: StatusCode,
    error: String,
    upstream: Option<String>,
    hint: Option<&str>,
) -> Response<Body> {
    let body = serde_json::json!({
        "error": format!("will-ia Browser CDP: {error}"),
        "upstream": upstream,
        "hint": hint.unwrap_or("veja Browser CDP na UI do will-ia (status do módulo) ou GET /api/browser/status"),
    });
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=UTF-8")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| status.into_response())
}

// ----- handler do proxy -----

fn is_websocket(req: &Request<Body>) -> bool {
    req.headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

async fn proxy_handler(State(m): State<Arc<BrowserModule>>, req: Request<Body>) -> Response<Body> {
    if let Err(e) = m.ensure_upstream().await {
        let upstream = Some(m.upstream_authority());
        if is_websocket(&req) {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("will-ia Browser CDP: {e}"),
            )
                .into_response();
        }
        return json_error(StatusCode::SERVICE_UNAVAILABLE, e, upstream, None);
    }
    if is_websocket(&req) {
        let (mut parts, _body) = req.into_parts();
        let path_q = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".into());
        let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(w) => w,
            Err(e) => return e.into_response(),
        };
        let m2 = m.clone();
        return ws
            .on_upgrade(move |socket| async move { m2.pump(socket, path_q).await })
            .into_response();
    }
    m.forward_http(req).await
}

impl BrowserModule {
    async fn forward_http(&self, req: Request<Body>) -> Response<Body> {
        let (parts, body) = req.into_parts();
        let path_q = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".into());
        let Some(up) = self.upstream() else {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "navegador ainda não reconhecido".into(),
                Some(self.upstream_authority()),
                None,
            );
        };
        if !up.http_discovery && path_q.starts_with("/json") {
            return self.synthesize_json(&up, &path_q).await;
        }
        let url = format!("http://{}{}", up.authority, path_q);
        let bytes = match axum::body::to_bytes(body, 4 * 1024 * 1024).await {
            Ok(b) => b,
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        };
        let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
            .unwrap_or(reqwest::Method::GET);
        let mut rb = self.http.request(method, &url);
        if let Some(ct) = parts
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
        {
            rb = rb.header("content-type", ct);
        }
        if !bytes.is_empty() {
            rb = rb.body(bytes);
        }
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("upstream {}: {e}", up.authority);
                self.set_error(Some(msg.clone()));
                return json_error(
                    StatusCode::BAD_GATEWAY,
                    msg,
                    Some(up.authority.clone()),
                    None,
                );
            }
        };
        let status =
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json; charset=UTF-8")
            .to_string();
        let text = resp.text().await.unwrap_or_default();
        let out = if path_q.starts_with("/json") {
            self.rewrite_discovery(&text)
        } else {
            text
        };
        tracing::debug!(path = %path_q, status = %status, "cdp http repassado");
        Response::builder()
            .status(status)
            .header("content-type", ct)
            .body(Body::from(out))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }

    async fn pump(&self, mut client: WebSocket, path_q: String) {
        let conn = self.conn_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let authority = self
            .upstream()
            .map(|u| u.authority)
            .unwrap_or_else(|| self.upstream_authority());
        let url = format!("ws://{authority}{path_q}");
        let (upstream, _) = match tokio_tungstenite::connect_async(&url).await {
            Ok(x) => x,
            Err(e) => {
                let msg = format!("WebSocket upstream {url}: {e}");
                self.set_error(Some(msg.clone()));
                self.log("warn", msg);
                let _ = client.send(AxMsg::Close(None)).await;
                return;
            }
        };
        self.active.fetch_add(1, Ordering::Relaxed);
        self.emit("ws_open", Some(format!("conexão {conn}: {path_q}")));
        tracing::info!(conn, path = %path_q, "cdp websocket aberto");
        let (mut up_tx, mut up_rx) = upstream.split();
        let (mut cl_tx, mut cl_rx) = client.split();
        let traffic = &self.traffic;
        let to_browser = async {
            while let Some(Ok(msg)) = cl_rx.next().await {
                let out = match msg {
                    AxMsg::Text(t) => {
                        traffic.record(conn, true, t.as_str());
                        TgMsg::Text(t.as_str().into())
                    }
                    AxMsg::Binary(b) => TgMsg::Binary(b),
                    AxMsg::Ping(b) => TgMsg::Ping(b),
                    AxMsg::Pong(b) => TgMsg::Pong(b),
                    AxMsg::Close(_) => break,
                };
                if up_tx.send(out).await.is_err() {
                    break;
                }
            }
        };
        let from_browser = async {
            while let Some(Ok(msg)) = up_rx.next().await {
                let out = match msg {
                    TgMsg::Text(t) => {
                        traffic.record(conn, false, t.as_str());
                        AxMsg::Text(t.as_str().into())
                    }
                    TgMsg::Binary(b) => AxMsg::Binary(b),
                    TgMsg::Ping(b) => AxMsg::Ping(b),
                    TgMsg::Pong(b) => AxMsg::Pong(b),
                    TgMsg::Close(_) => break,
                    TgMsg::Frame(_) => continue,
                };
                if cl_tx.send(out).await.is_err() {
                    break;
                }
            }
        };
        tokio::select! {
            _ = to_browser => {}
            _ = from_browser => {}
        }
        self.active.fetch_sub(1, Ordering::Relaxed);
        self.traffic.forget_conn(conn);
        self.emit("ws_closed", Some(format!("conexão {conn}")));
        tracing::info!(conn, "cdp websocket fechado");
    }
}
