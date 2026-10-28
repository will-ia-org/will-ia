use crate::orchestrator::Host;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use will_core::{AppPaths, ApprovalManager, EventBus, Module, ModuleStatus, Settings, ThreadId};
use will_engine::EnginePool;
use will_mcp::{McpServer, TokenRegistry, ToolRegistry};
use will_storage::{SecretStore, Storage};

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub port: Option<u16>,
    /// Mantido por compatibilidade; a UI é o app desktop, nada é aberto no navegador.
    pub open_browser: bool,
    pub data_dir: Option<PathBuf>,
    /// Força segredos em arquivo (sem keyring).
    pub file_secrets: bool,
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    pub ui_token: String,
    pub state: Arc<AppState>,
    cancel: CancellationToken,
}

impl ServerHandle {
    /// Base da API local (`http://127.0.0.1:<porta>`).
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        crate::modules::stop_all(&self.state).await;
        self.state.engine.shutdown().await;
    }
    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.cancel.cancelled()
    }
}

pub struct AppState {
    pub paths: AppPaths,
    pub storage: Storage,
    pub secrets: Arc<dyn SecretStore>,
    pub settings: RwLock<Settings>,
    pub bus: EventBus,
    pub approvals: ApprovalManager,
    pub engine: Arc<EnginePool>,
    pub registry: Arc<ToolRegistry>,
    pub tokens: Arc<TokenRegistry>,
    pub mcp: tokio::sync::Mutex<Option<McpServer>>,
    pub mcp_url: Arc<RwLock<String>>,
    pub module_status: RwLock<HashMap<Module, ModuleStatus>>,
    pub realtime: RwLock<Option<Arc<will_realtime::RealtimeManager>>>,
    pub scheduler: RwLock<Option<Arc<will_scheduler::SchedulerModule>>>,
    #[cfg(feature = "telegram")]
    pub telegram: tokio::sync::RwLock<Option<Arc<will_telegram::TelegramModule>>>,
    #[cfg(feature = "google")]
    pub google: RwLock<Option<Arc<will_google::GoogleModule>>>,
    #[cfg(feature = "whatsapp")]
    pub whatsapp: tokio::sync::RwLock<Option<Arc<will_whatsapp::WhatsAppModule>>>,
    pub ui_token: String,
    pub started_at: Instant,
    pub voice_thread: RwLock<Option<ThreadId>>,
    pub cancel: CancellationToken,
    pub host: RwLock<Option<Arc<Host>>>,
    /// Gancho da GUI (app desktop): recebe comandos como `show`. `None` no modo headless.
    pub ui_hook: RwLock<Option<UiHook>>,
}

pub type UiHook = Arc<dyn Fn(&str) + Send + Sync>;

impl AppState {
    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }
    /// Pasta da base de conhecimento (caminho relativo = relativo à raiz portátil).
    pub fn kb_path(&self) -> Option<PathBuf> {
        self.settings
            .read()
            .kb
            .path
            .as_ref()
            .map(|p| self.paths.resolve_user_path(p))
    }
    pub fn set_module_status(&self, m: Module, s: ModuleStatus) {
        let changed = self.module_status.read().get(&m) != Some(&s);
        self.module_status.write().insert(m, s.clone());
        if changed {
            self.bus.emit(will_core::Event::ModuleStatus {
                module: m,
                status: s,
            });
        }
    }
    pub fn host(&self) -> Arc<Host> {
        self.host.read().clone().expect("host inicializado")
    }
    pub async fn save_settings(&self) -> Result<(), String> {
        let s = self.settings();
        self.storage
            .save_settings(&s)
            .await
            .map_err(|e| e.to_string())
    }
}

fn load_or_create_ui_token(paths: &AppPaths) -> String {
    let f = paths.session_token_file();
    if let Ok(t) = std::fs::read_to_string(&f) {
        let t = t.trim().to_string();
        if t.len() >= 16 {
            return t;
        }
    }
    use rand::Rng;
    let mut b = [0u8; 24];
    rand::rng().fill_bytes(&mut b);
    let t: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let _ = std::fs::write(&f, &t);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600));
    }
    t
}

pub async fn start(opts: RunOptions) -> anyhow::Result<ServerHandle> {
    // Há mais de um provedor de criptografia do rustls no grafo de dependências
    // (ring e aws-lc-rs); sem isto o rustls entra em pânico ao abrir TLS.
    let _ = rustls::crypto::ring::default_provider().install_default();
    if let Some(d) = &opts.data_dir {
        std::env::set_var("WILL_IA_DATA_DIR", d);
    }
    let paths = AppPaths::resolve();
    paths.ensure()?;
    let bus = EventBus::new(2048);
    crate::logging::init(&paths, bus.clone());
    tracing::info!(data_dir = %paths.data_dir.display(), "will-ia iniciando");

    let storage = Storage::open(&paths.db_path()).await?;
    storage.reset_running_threads().await?;
    let settings = storage.load_settings().await?;
    // Em modo portátil o keyring da máquina não serve (o pendrive muda de máquina):
    // segredos vão para `data/secrets.json`.
    let force_file = opts.file_secrets || paths.is_portable();
    if paths.is_portable() {
        tracing::info!(root = %paths.portable_root.as_deref().map(|p| p.display().to_string()).unwrap_or_default(), "modo portátil");
    }
    let secrets: Arc<dyn SecretStore> =
        Arc::new(will_storage::AutoSecretStore::new(paths.secrets_file(), force_file).await);
    tracing::info!(backend = secrets.backend(), "segredos");

    let approvals = ApprovalManager::new(bus.clone());
    let registry = Arc::new(ToolRegistry::new());
    registry.set_overrides(storage.tool_policies().await.unwrap_or_default());
    let tokens = Arc::new(TokenRegistry::new());
    let mcp_url = Arc::new(RwLock::new(String::new()));

    // Engine com configuração inicial (token/kb podem estar vazios; o pool só spawna sob demanda).
    let initial_cfg = crate::engine_cfg::build(
        &settings,
        &paths,
        None,
        &mcp_url.read(),
        &std::collections::BTreeMap::new(),
    );
    let engine = EnginePool::new(initial_cfg);
    let ui_token = load_or_create_ui_token(&paths);
    let cancel = CancellationToken::new();

    let state = Arc::new(AppState {
        paths,
        storage,
        secrets,
        settings: RwLock::new(settings),
        bus,
        approvals,
        engine,
        registry,
        tokens,
        mcp: tokio::sync::Mutex::new(None),
        mcp_url,
        module_status: RwLock::new(HashMap::new()),
        realtime: RwLock::new(None),
        scheduler: RwLock::new(None),
        #[cfg(feature = "telegram")]
        telegram: tokio::sync::RwLock::new(None),
        #[cfg(feature = "google")]
        google: RwLock::new(None),
        #[cfg(feature = "whatsapp")]
        whatsapp: tokio::sync::RwLock::new(None),
        ui_token: ui_token.clone(),
        started_at: Instant::now(),
        voice_thread: RwLock::new(None),
        cancel: cancel.clone(),
        host: RwLock::new(None),
        ui_hook: RwLock::new(None),
    });
    let host = Arc::new(Host::new(state.clone()));
    *state.host.write() = Some(host.clone());

    crate::orchestrator::spawn_engine_loop(state.clone());
    crate::orchestrator::spawn_bus_persistence(state.clone());
    crate::modules::start_all(&state).await;

    let port = opts
        .port
        .unwrap_or_else(|| state.settings.read().general.ui_port);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let app = crate::api::router(state.clone());
    let c2 = cancel.clone();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { c2.cancelled().await })
            .await
        {
            tracing::error!(error = %e, "servidor HTTP encerrou com erro");
        }
    });
    tracing::info!(%addr, "API local no ar");
    let _ = opts.open_browser;
    Ok(ServerHandle {
        addr,
        ui_token,
        state,
        cancel,
    })
}
