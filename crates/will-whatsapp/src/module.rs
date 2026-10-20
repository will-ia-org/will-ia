//! `WhatsAppModule`: ciclo de vida de **múltiplas contas pareadas**, indexação
//! dos eventos de cada backend e estado exposto à UI/API local.
//!
//! Cada conta (`WaAccount`) tem o próprio backend, store de sessão
//! (`<data_dir>/whatsapp/<account_id>/`), estado de conexão e backoff. O módulo
//! consome um único `mpsc` com `(account_id, BackendEvent)`.

use crate::backend::{
    is_group_jid, phone_from_jid, BackendEvent, IncomingMessage, WhatsAppBackend,
};
use crate::qr_svg;
use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use will_core::settings::{WhatsAppAccountRef, WhatsAppSettings};
use will_core::{AppPaths, Event, EventBus, ModuleStatus};
use will_storage::whatsapp::{WaContact, WaMessage};
use will_storage::Storage;

const LOG_MODULE: &str = "whatsapp";
const BACKOFF_MIN_SECS: u64 = 1;
const BACKOFF_MAX_SECS: u64 = 60;
const EVENT_CHANNEL_CAPACITY: usize = 512;

/// Dependências injetadas pelo serviço.
pub struct WhatsAppDeps {
    pub storage: Storage,
    pub paths: AppPaths,
    pub bus: EventBus,
    /// Raiz da base de conhecimento (mídia vai para `<kb>/<media_dir>`).
    pub kb_path: PathBuf,
}

/// Fábrica de backend por conta: `(account_id, diretório do store, canal de eventos)`.
pub type BackendFactory = Arc<
    dyn Fn(&str, PathBuf, mpsc::Sender<(String, BackendEvent)>) -> Arc<dyn WhatsAppBackend>
        + Send
        + Sync,
>;

/// Estado de conexão de uma conta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    /// `"qr"` ou `"code"`.
    Pairing(String),
    Connected,
    LoggedOut,
    Degraded(String),
}

impl ConnState {
    pub fn as_str(&self) -> &str {
        match self {
            ConnState::Disconnected => "disconnected",
            ConnState::Pairing(kind) => kind.as_str(),
            ConnState::Connected => "connected",
            ConnState::LoggedOut => "logged_out",
            ConnState::Degraded(_) => "degraded",
        }
    }
}

/// Rótulo gerado automaticamente ("Conta N"); substituído pelo nome do perfil ao conectar.
pub fn is_default_label(label: &str) -> bool {
    label
        .strip_prefix("Conta ")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Uma conta WhatsApp pareada (ou em pareamento) e todo o seu estado de sessão.
pub struct WaAccount {
    pub id: String,
    pub backend: Arc<dyn WhatsAppBackend>,
    created_at: Option<DateTime<Utc>>,
    label: RwLock<String>,
    phone: RwLock<Option<String>>,
    enabled: AtomicBool,
    state: RwLock<ConnState>,
    me: RwLock<Option<Value>>,
    last_qr: RwLock<Option<Value>>,
    last_pair_code: RwLock<Option<String>>,
    last_sync: RwLock<Option<DateTime<Utc>>>,
    stopping: AtomicBool,
    unlinking: AtomicBool,
    reconnecting: AtomicBool,
    backoff_attempt: AtomicU32,
}

impl WaAccount {
    fn new(r: &WhatsAppAccountRef, backend: Arc<dyn WhatsAppBackend>) -> Arc<Self> {
        Arc::new(Self {
            id: r.id.clone(),
            backend,
            created_at: r.created_at,
            label: RwLock::new(r.label.clone()),
            phone: RwLock::new(r.phone.clone()),
            enabled: AtomicBool::new(r.enabled),
            state: RwLock::new(ConnState::Disconnected),
            me: RwLock::new(None),
            last_qr: RwLock::new(None),
            last_pair_code: RwLock::new(None),
            last_sync: RwLock::new(None),
            stopping: AtomicBool::new(false),
            unlinking: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
            backoff_attempt: AtomicU32::new(0),
        })
    }

    pub fn label(&self) -> String {
        self.label.read().clone()
    }

    pub fn phone(&self) -> Option<String> {
        self.phone.read().clone()
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> ConnState {
        self.state.read().clone()
    }

    pub fn is_connected(&self) -> bool {
        *self.state.read() == ConnState::Connected
    }

    pub fn me(&self) -> Option<Value> {
        self.me.read().clone()
    }

    pub fn has_session(&self) -> bool {
        self.backend.has_session()
    }

    /// Referência persistível nas configurações.
    pub fn to_ref(&self) -> WhatsAppAccountRef {
        WhatsAppAccountRef {
            id: self.id.clone(),
            label: self.label(),
            phone: self.phone(),
            enabled: self.enabled(),
            created_at: self.created_at,
        }
    }

    /// `id — label — phone` (para mensagens de erro das ferramentas).
    pub fn describe(&self) -> String {
        format!(
            "{} — {} — {}",
            self.id,
            self.label(),
            self.phone().unwrap_or_else(|| "sem número".into())
        )
    }

    fn set_state(&self, st: ConnState) {
        *self.state.write() = st;
    }

    /// Motivo de degradação quando não conectada.
    fn degraded_reason(&self) -> Option<String> {
        match self.state() {
            ConnState::Connected | ConnState::Pairing(_) => None,
            ConnState::Disconnected => Some(if self.has_session() {
                "desconectado".into()
            } else {
                "não pareado".into()
            }),
            ConnState::LoggedOut => Some("SessionRevoked".into()),
            ConnState::Degraded(r) => Some(r),
        }
    }
}

pub struct WhatsAppModule {
    pub(crate) deps: WhatsAppDeps,
    pub(crate) settings: RwLock<WhatsAppSettings>,
    factory: BackendFactory,
    tx: mpsc::Sender<(String, BackendEvent)>,
    events: tokio::sync::Mutex<Option<mpsc::Receiver<(String, BackendEvent)>>>,
    accounts: RwLock<BTreeMap<String, Arc<WaAccount>>>,
    started: AtomicBool,
    token: Mutex<Option<CancellationToken>>,
    pub(crate) weak: Weak<WhatsAppModule>,
}

impl WhatsAppModule {
    /// Cria o módulo com as contas de `settings.accounts` (backends via `factory`).
    pub fn new(
        deps: WhatsAppDeps,
        settings: WhatsAppSettings,
        factory: BackendFactory,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let me = Arc::new_cyclic(|weak| Self {
            deps,
            settings: RwLock::new(settings.clone()),
            factory,
            tx,
            events: tokio::sync::Mutex::new(Some(rx)),
            accounts: RwLock::new(BTreeMap::new()),
            started: AtomicBool::new(false),
            token: Mutex::new(None),
            weak: weak.clone(),
        });
        for r in &settings.accounts {
            if !r.id.is_empty() {
                me.insert_account(r);
            }
        }
        me
    }

    /// Backend real quando compilado com `--features real`; caso contrário um
    /// mock que reporta "backend indisponível neste build".
    pub fn new_default(deps: WhatsAppDeps, settings: WhatsAppSettings) -> Arc<Self> {
        #[cfg(feature = "real")]
        let factory: BackendFactory =
            Arc::new(|id, dir, tx| Arc::new(crate::real::RealBackend::new(id, dir, tx)));
        #[cfg(not(feature = "real"))]
        let factory: BackendFactory =
            Arc::new(|id, _dir, tx| Arc::new(crate::mock::MockBackend::unavailable(id, tx)));
        Self::new(deps, settings, factory)
    }

    fn account_dir(&self, account_id: &str) -> PathBuf {
        self.deps.paths.whatsapp_dir().join(account_id)
    }

    fn insert_account(&self, r: &WhatsAppAccountRef) -> Arc<WaAccount> {
        let dir = self.account_dir(&r.id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("criar {}: {e}", dir.display());
        }
        let backend = (self.factory)(&r.id, dir, self.tx.clone());
        let acc = WaAccount::new(r, backend);
        self.accounts.write().insert(r.id.clone(), acc.clone());
        acc
    }

    /// Espelha `s.accounts`: cria contas novas, remove as ausentes (desconecta,
    /// sem apagar o store do disco) e aplica `enabled`/rótulo/telefone nas existentes.
    pub async fn update_settings(&self, s: WhatsAppSettings) {
        let incoming: BTreeMap<String, WhatsAppAccountRef> = s
            .accounts
            .iter()
            .filter(|r| !r.id.is_empty())
            .map(|r| (r.id.clone(), r.clone()))
            .collect();
        let (removed, to_disable): (Vec<Arc<WaAccount>>, Vec<Arc<WaAccount>>) = {
            let mut accounts = self.accounts.write();
            let removed: Vec<Arc<WaAccount>> = accounts
                .keys()
                .filter(|id| !incoming.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .filter_map(|id| accounts.remove(&id))
                .collect();
            let mut to_disable = Vec::new();
            for (id, r) in &incoming {
                if let Some(acc) = accounts.get(id) {
                    let was_enabled = acc.enabled.swap(r.enabled, Ordering::SeqCst);
                    if was_enabled && !r.enabled {
                        to_disable.push(acc.clone());
                    }
                    if !r.label.is_empty()
                        && r.label != acc.label()
                        && !(is_default_label(&r.label) && !is_default_label(&acc.label()))
                    {
                        *acc.label.write() = r.label.clone();
                    }
                    if acc.phone().is_none() && r.phone.is_some() {
                        *acc.phone.write() = r.phone.clone();
                    }
                }
            }
            (removed, to_disable)
        };
        for (id, r) in &incoming {
            if !self.accounts.read().contains_key(id) {
                self.insert_account(r);
            }
        }
        *self.settings.write() = s;
        for acc in removed {
            self.deps.bus.log(
                "info",
                LOG_MODULE,
                format!(
                    "conta {} ({}) removida das configurações",
                    acc.id,
                    acc.label()
                ),
            );
            self.disconnect_account(&acc, "conta removida").await;
        }
        for acc in to_disable {
            self.disconnect_account(&acc, "conta desativada").await;
        }
        if self.started.load(Ordering::SeqCst) {
            self.publish_module_status();
        }
    }

    pub fn settings(&self) -> WhatsAppSettings {
        self.settings.read().clone()
    }

    pub fn storage(&self) -> &Storage {
        &self.deps.storage
    }

    pub fn kb_path(&self) -> &PathBuf {
        &self.deps.kb_path
    }

    /// Diretório absoluto onde a mídia baixada é gravada (`<kb>/<media_dir>`).
    pub fn media_dir(&self) -> PathBuf {
        let s = self.settings.read();
        let rel = PathBuf::from(s.media_dir.trim_start_matches('/'));
        self.deps.kb_path.join(rel)
    }

    // ----- contas -----

    pub fn accounts(&self) -> Vec<Arc<WaAccount>> {
        self.accounts.read().values().cloned().collect()
    }

    pub fn account(&self, id: &str) -> Option<Arc<WaAccount>> {
        self.accounts.read().get(id).cloned()
    }

    pub fn connected_accounts(&self) -> Vec<Arc<WaAccount>> {
        self.accounts
            .read()
            .values()
            .filter(|a| a.is_connected())
            .cloned()
            .collect()
    }

    /// Há alguma conta conectada?
    pub fn is_connected(&self) -> bool {
        self.accounts.read().values().any(|a| a.is_connected())
    }

    /// Referências das contas (para o serviço persistir nas configurações).
    pub fn account_refs(&self) -> Vec<WhatsAppAccountRef> {
        self.accounts.read().values().map(|a| a.to_ref()).collect()
    }

    /// Resolve `account` por id exato, depois rótulo (case-insensitive), depois telefone (só dígitos).
    pub fn resolve_account(&self, query: &str) -> Option<Arc<WaAccount>> {
        let q = query.trim();
        if q.is_empty() {
            return None;
        }
        let accounts = self.accounts.read();
        if let Some(a) = accounts.get(q) {
            return Some(a.clone());
        }
        if let Some(a) = accounts
            .values()
            .find(|a| a.label().eq_ignore_ascii_case(q))
        {
            return Some(a.clone());
        }
        let digits: String = q.chars().filter(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            if let Some(a) = accounts
                .values()
                .find(|a| a.phone().as_deref() == Some(digits.as_str()))
            {
                return Some(a.clone());
            }
        }
        None
    }

    fn get_account(&self, id: &str) -> Result<Arc<WaAccount>, String> {
        self.account(id)
            .ok_or_else(|| format!("conta WhatsApp {id} não encontrada"))
    }

    fn next_default_label(&self) -> String {
        let accounts = self.accounts.read();
        let mut n = accounts.len() + 1;
        loop {
            let label = format!("Conta {n}");
            if !accounts.values().any(|a| a.label() == label) {
                return label;
            }
            n += 1;
        }
    }

    // ----- ciclo de vida -----

    /// Liga o módulo: loop de eventos + poda diária; conecta cada conta ativa com sessão.
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        let s = self.settings();
        if !s.enabled {
            self.deps
                .bus
                .log("info", LOG_MODULE, "WhatsApp desligado nas configurações");
            return Ok(());
        }
        self.ensure_loops();
        let accounts = self.accounts();
        if accounts.is_empty() {
            self.deps.bus.log(
                "info",
                LOG_MODULE,
                "nenhuma conta WhatsApp pareada; pareie nas configurações",
            );
        }
        for acc in accounts.iter().filter(|a| a.enabled()) {
            if acc.is_connected() || matches!(acc.state(), ConnState::Pairing(_)) {
                continue;
            }
            if acc.has_session() {
                if let Err(e) = acc.backend.connect(None).await {
                    acc.set_state(ConnState::Degraded(e.clone()));
                    self.deps.bus.log(
                        "warn",
                        LOG_MODULE,
                        format!("conta {}: falha ao conectar: {e}", acc.label()),
                    );
                    self.schedule_reconnect(acc);
                }
            } else {
                self.deps.bus.log(
                    "info",
                    LOG_MODULE,
                    format!("conta {} sem sessão; aguardando pareamento", acc.label()),
                );
            }
        }
        self.publish_module_status();
        Ok(())
    }

    /// Desliga: cancela tarefas e fecha as conexões (as sessões permanecem no disco).
    pub async fn stop(&self) {
        if let Some(t) = self.token.lock().take() {
            t.cancel();
        }
        for acc in self.accounts() {
            self.disconnect_account(&acc, "módulo parado").await;
        }
        self.started.store(false, Ordering::SeqCst);
        self.publish_module_status();
    }

    async fn disconnect_account(&self, acc: &Arc<WaAccount>, reason: &str) {
        acc.stopping.store(true, Ordering::SeqCst);
        let _ = acc.backend.disconnect().await;
        acc.stopping.store(false, Ordering::SeqCst);
        if matches!(acc.state(), ConnState::Connected | ConnState::Pairing(_)) {
            acc.set_state(ConnState::Disconnected);
            self.emit(
                "disconnected",
                json!({"account_id": acc.id, "reason": reason}),
            );
        }
    }

    /// Inicia pareamento por QR (`phone == None`) ou por código. Sem `account_id`
    /// cria uma conta nova e devolve o id; com `account_id` reutiliza a existente.
    pub async fn pair(
        self: &Arc<Self>,
        account_id: Option<String>,
        phone: Option<String>,
        label: Option<String>,
    ) -> Result<String, String> {
        let phone = match phone.filter(|p| !p.trim().is_empty()) {
            Some(p) => Some(
                crate::backend::phone_digits(&p)
                    .ok_or("telefone inválido: use DDI+DDD+número, só dígitos")?,
            ),
            None => None,
        };
        let label = label
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty());
        let acc = match account_id.filter(|id| !id.trim().is_empty()) {
            Some(id) => {
                let acc = self.get_account(&id)?;
                if let Some(l) = label {
                    *acc.label.write() = l;
                }
                acc
            }
            None => {
                let id = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
                let label = label.unwrap_or_else(|| self.next_default_label());
                let r = WhatsAppAccountRef {
                    id,
                    label,
                    phone: None,
                    enabled: true,
                    created_at: Some(Utc::now()),
                };
                let acc = self.insert_account(&r);
                self.deps.bus.log(
                    "info",
                    LOG_MODULE,
                    format!("nova conta WhatsApp {} ({})", acc.id, acc.label()),
                );
                acc
            }
        };
        acc.enabled.store(true, Ordering::SeqCst);
        self.ensure_loops();
        *acc.last_qr.write() = None;
        *acc.last_pair_code.write() = None;
        acc.set_state(ConnState::Pairing(if phone.is_some() {
            "code".into()
        } else {
            "qr".into()
        }));
        if let Err(e) = acc.backend.connect(phone).await {
            acc.set_state(ConnState::Degraded(e.clone()));
            self.publish_module_status();
            return Err(e);
        }
        self.publish_module_status();
        Ok(acc.id.clone())
    }

    /// Logout remoto + apaga a sessão local; `clear_index` limpa o índice da
    /// conta; `remove` tira a conta do módulo (e de `account_refs()`).
    pub async fn unlink(
        &self,
        account_id: &str,
        clear_index: bool,
        remove: bool,
    ) -> Result<(), String> {
        let acc = self.get_account(account_id)?;
        acc.stopping.store(true, Ordering::SeqCst);
        acc.unlinking.store(true, Ordering::SeqCst);
        let res = acc.backend.logout().await;
        acc.stopping.store(false, Ordering::SeqCst);
        if let Err(e) = &res {
            self.deps.bus.log(
                "warn",
                LOG_MODULE,
                format!("conta {}: logout remoto falhou: {e}", acc.label()),
            );
        }
        *acc.me.write() = None;
        *acc.last_qr.write() = None;
        *acc.last_pair_code.write() = None;
        acc.set_state(ConnState::Disconnected);
        if clear_index {
            self.deps
                .storage
                .wa_clear_index(&acc.id)
                .await
                .map_err(|e| e.to_string())?;
        }
        self.emit(
            "logged_out",
            json!({"account_id": acc.id, "reason": "desvinculado pelo usuário"}),
        );
        if remove {
            self.accounts.write().remove(&acc.id);
            self.deps.bus.log(
                "info",
                LOG_MODULE,
                format!("conta {} ({}) removida", acc.id, acc.label()),
            );
        } else {
            // Se o backend não emitiu LoggedOut, não deixe a flag armada para um logout remoto futuro.
            let weak = Arc::downgrade(&acc);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if let Some(a) = weak.upgrade() {
                    a.unlinking.store(false, Ordering::SeqCst);
                }
            });
        }
        self.publish_module_status();
        res
    }

    pub async fn reconnect(self: &Arc<Self>, account_id: &str) -> Result<(), String> {
        let acc = self.get_account(account_id)?;
        self.ensure_loops();
        acc.stopping.store(true, Ordering::SeqCst);
        let _ = acc.backend.disconnect().await;
        acc.stopping.store(false, Ordering::SeqCst);
        acc.backoff_attempt.store(0, Ordering::SeqCst);
        acc.backend.connect(None).await.inspect_err(|e| {
            acc.set_state(ConnState::Degraded(e.clone()));
        })
    }

    /// Altera rótulo e/ou ativação de uma conta. Desativar desconecta; ativar
    /// reconecta se houver sessão e o módulo estiver ligado.
    pub async fn set_account(
        self: &Arc<Self>,
        account_id: &str,
        label: Option<String>,
        enabled: Option<bool>,
    ) -> Result<(), String> {
        let acc = self.get_account(account_id)?;
        if let Some(l) = label
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
        {
            *acc.label.write() = l;
        }
        if let Some(en) = enabled {
            let was = acc.enabled.swap(en, Ordering::SeqCst);
            if was && !en {
                self.disconnect_account(&acc, "conta desativada").await;
            } else if !was
                && en
                && self.started.load(Ordering::SeqCst)
                && acc.has_session()
                && !acc.is_connected()
            {
                if let Err(e) = acc.backend.connect(None).await {
                    acc.set_state(ConnState::Degraded(e));
                }
            }
        }
        self.publish_module_status();
        Ok(())
    }

    // ----- estado -----

    pub async fn status(&self) -> Value {
        let s = self.settings();
        let accounts = self.accounts();
        let mut by_account: BTreeMap<String, Value> = self
            .deps
            .storage
            .wa_stats_all()
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut total = (0i64, 0i64, 0i64);
        for v in by_account.values() {
            total.0 += v["chats"].as_i64().unwrap_or(0);
            total.1 += v["messages"].as_i64().unwrap_or(0);
            total.2 += v["contacts"].as_i64().unwrap_or(0);
        }
        let empty = json!({"chats": 0, "messages": 0, "contacts": 0});
        let items: Vec<Value> = accounts
            .iter()
            .map(|a| {
                let st = a.state();
                json!({
                    "id": a.id,
                    "label": a.label(),
                    "phone": a.phone(),
                    "enabled": a.enabled(),
                    "state": st.as_str(),
                    "detail": match &st { ConnState::Degraded(r) => Some(r.clone()), _ => None },
                    "me": a.me(),
                    "last_qr": a.last_qr.read().clone(),
                    "last_pair_code": a.last_pair_code.read().clone(),
                    "last_sync": a.last_sync.read().map(|d| d.to_rfc3339()),
                    "has_session": a.has_session(),
                    "index": by_account.remove(&a.id).unwrap_or_else(|| empty.clone()),
                })
            })
            .collect();
        let backend =
            accounts
                .first()
                .map(|a| a.backend.kind())
                .unwrap_or(if cfg!(feature = "real") {
                    "real"
                } else {
                    "mock"
                });
        json!({
            "enabled": s.enabled,
            "started": self.started.load(Ordering::SeqCst),
            "backend": backend,
            "media_dir": self.media_dir(),
            "accounts": items,
            "index_total": {"chats": total.0, "messages": total.1, "contacts": total.2},
        })
    }

    pub fn module_status(&self) -> ModuleStatus {
        if !self.settings.read().enabled {
            return ModuleStatus::Disabled;
        }
        let accounts = self.accounts();
        if accounts.is_empty() {
            return ModuleStatus::Degraded("nenhuma conta pareada".into());
        }
        if !self.started.load(Ordering::SeqCst) {
            return ModuleStatus::Stopped;
        }
        if accounts.iter().any(|a| a.is_connected()) {
            return ModuleStatus::Running;
        }
        if accounts
            .iter()
            .any(|a| matches!(a.state(), ConnState::Pairing(_)))
        {
            return ModuleStatus::Starting;
        }
        let enabled: Vec<&Arc<WaAccount>> = accounts.iter().filter(|a| a.enabled()).collect();
        if enabled.is_empty() {
            return ModuleStatus::Degraded("todas as contas desativadas".into());
        }
        ModuleStatus::Degraded(
            enabled[0]
                .degraded_reason()
                .unwrap_or_else(|| "desconectado".into()),
        )
    }

    // ----- internos -----

    fn emit(&self, state: &str, detail: Value) {
        self.deps.bus.emit(Event::WhatsApp {
            state: state.to_string(),
            detail: Some(detail.to_string()),
        });
    }

    fn publish_module_status(&self) {
        self.deps.bus.emit(Event::ModuleStatus {
            module: will_core::Module::WhatsApp,
            status: self.module_status(),
        });
    }

    fn ensure_loops(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let token = CancellationToken::new();
        *self.token.lock() = Some(token.clone());
        let me = Arc::clone(self);
        let t = token.clone();
        tokio::spawn(async move { me.event_loop(t).await });
        let me = Arc::clone(self);
        tokio::spawn(async move { me.prune_loop(token).await });
    }

    async fn event_loop(self: Arc<Self>, token: CancellationToken) {
        let Some(mut rx) = self.events.lock().await.take() else {
            tracing::warn!("loop de eventos do WhatsApp já em execução");
            return;
        };
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                ev = rx.recv() => match ev {
                    Some((account_id, ev)) => match self.account(&account_id) {
                        Some(acc) => self.handle_event(&acc, ev).await,
                        None => tracing::debug!("evento de conta WhatsApp desconhecida/removida {account_id} ignorado"),
                    },
                    None => {
                        tracing::warn!("canal de eventos dos backends WhatsApp fechado");
                        break;
                    }
                }
            }
        }
        *self.events.lock().await = Some(rx);
    }

    async fn prune_loop(self: Arc<Self>, token: CancellationToken) {
        loop {
            let days = self.settings().retention_days;
            if days > 0 {
                match self.deps.storage.wa_prune(days as i64).await {
                    Ok(n) if n > 0 => self.deps.bus.log(
                        "info",
                        LOG_MODULE,
                        format!("poda do índice: {n} mensagens removidas (> {days} dias)"),
                    ),
                    Ok(_) => {}
                    Err(e) => {
                        self.deps
                            .bus
                            .log("warn", LOG_MODULE, format!("poda do índice falhou: {e}"))
                    }
                }
            }
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(24 * 3600)) => {}
            }
        }
    }

    fn schedule_reconnect(self: &Arc<Self>, acc: &Arc<WaAccount>) {
        if acc.stopping.load(Ordering::SeqCst)
            || !acc.enabled()
            || !self.started.load(Ordering::SeqCst)
            || !acc.has_session()
        {
            return;
        }
        if acc.reconnecting.swap(true, Ordering::SeqCst) {
            return;
        }
        let attempt = acc.backoff_attempt.fetch_add(1, Ordering::SeqCst);
        let delay = (BACKOFF_MIN_SECS << attempt.min(6)).min(BACKOFF_MAX_SECS);
        let token = self.token.lock().clone();
        let me = Arc::clone(self);
        let acc = Arc::clone(acc);
        tokio::spawn(async move {
            let sleep = tokio::time::sleep(Duration::from_secs(delay));
            match token {
                Some(t) => {
                    tokio::select! { _ = t.cancelled() => { acc.reconnecting.store(false, Ordering::SeqCst); return; }, _ = sleep => {} }
                }
                None => sleep.await,
            }
            acc.reconnecting.store(false, Ordering::SeqCst);
            if acc.backend.connected()
                || acc.stopping.load(Ordering::SeqCst)
                || !acc.enabled()
                || !me.started.load(Ordering::SeqCst)
            {
                return;
            }
            if matches!(acc.state(), ConnState::LoggedOut) || me.account(&acc.id).is_none() {
                return;
            }
            me.deps.bus.log(
                "info",
                LOG_MODULE,
                format!(
                    "reconectando WhatsApp {} (tentativa {})",
                    acc.label(),
                    attempt + 1
                ),
            );
            if let Err(e) = acc.backend.connect(None).await {
                acc.set_state(ConnState::Degraded(e.clone()));
                me.deps.bus.log(
                    "warn",
                    LOG_MODULE,
                    format!("conta {}: reconexão falhou: {e}", acc.label()),
                );
                me.schedule_reconnect(&acc);
            }
        });
    }

    async fn handle_event(self: &Arc<Self>, acc: &Arc<WaAccount>, ev: BackendEvent) {
        match ev {
            BackendEvent::Qr { code, timeout_secs } => {
                let svg = qr_svg(&code);
                let detail = json!({"account_id": acc.id, "data": code, "svg": svg, "timeout_secs": timeout_secs, "at": Utc::now().to_rfc3339()});
                *acc.last_qr.write() = Some(detail.clone());
                acc.set_state(ConnState::Pairing("qr".into()));
                self.emit("qr", detail);
            }
            BackendEvent::PairCode { code } => {
                *acc.last_pair_code.write() = Some(code.clone());
                acc.set_state(ConnState::Pairing("code".into()));
                self.emit("code", json!({"account_id": acc.id, "code": code}));
            }
            BackendEvent::Connected { me } => {
                let phone = me
                    .get("phone")
                    .and_then(|p| p.as_str())
                    .map(|p| p.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
                    .filter(|p| !p.is_empty())
                    .or_else(|| {
                        me.get("jid")
                            .and_then(|j| j.as_str())
                            .and_then(phone_from_jid)
                    });
                if phone.is_some() {
                    *acc.phone.write() = phone.clone();
                }
                if is_default_label(&acc.label()) {
                    let push_name = me
                        .get("push_name")
                        .and_then(|p| p.as_str())
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(str::to_string);
                    if let Some(new_label) = push_name.or(phone) {
                        *acc.label.write() = new_label;
                    }
                }
                *acc.me.write() = Some(me.clone());
                *acc.last_qr.write() = None;
                *acc.last_pair_code.write() = None;
                *acc.last_sync.write() = Some(Utc::now());
                acc.backoff_attempt.store(0, Ordering::SeqCst);
                acc.set_state(ConnState::Connected);
                self.emit("connected", json!({"account_id": acc.id, "me": me}));
                self.deps.bus.log(
                    "info",
                    LOG_MODULE,
                    format!("WhatsApp conectado: {}", acc.label()),
                );
                self.publish_module_status();
            }
            BackendEvent::Disconnected { reason } => {
                if !matches!(acc.state(), ConnState::LoggedOut) {
                    acc.set_state(if acc.stopping.load(Ordering::SeqCst) {
                        ConnState::Disconnected
                    } else {
                        ConnState::Degraded(reason.clone())
                    });
                }
                self.emit(
                    "disconnected",
                    json!({"account_id": acc.id, "reason": reason}),
                );
                self.deps.bus.log(
                    "warn",
                    LOG_MODULE,
                    format!("WhatsApp {} desconectado: {reason}", acc.label()),
                );
                self.publish_module_status();
                self.schedule_reconnect(acc);
            }
            BackendEvent::LoggedOut => {
                *acc.me.write() = None;
                let by_user = acc.unlinking.swap(false, Ordering::SeqCst);
                if by_user {
                    // Desvinculado pelo próprio usuário (unlink): fica "desconectado", sem alarme.
                    acc.set_state(ConnState::Disconnected);
                } else {
                    acc.set_state(ConnState::LoggedOut);
                    self.emit(
                        "logged_out",
                        json!({"account_id": acc.id, "reason": "revogado no telefone"}),
                    );
                    self.deps.bus.log(
                        "warn",
                        LOG_MODULE,
                        format!("sessão do WhatsApp {} revogada pelo telefone", acc.label()),
                    );
                    self.deps.bus.emit(Event::Notification {
                        title: "WhatsApp desvinculado".into(),
                        body: format!("A sessão da conta {} foi revogada no telefone. Pareie novamente nas configurações.", acc.label()),
                    });
                }
                self.publish_module_status();
            }
            BackendEvent::Message(m) => self.index_message(acc, m).await,
            BackendEvent::Receipt {
                chat_jid,
                message_ids,
                kind,
            } => {
                self.handle_receipt(acc, &chat_jid, &message_ids, &kind)
                    .await
            }
            BackendEvent::ContactUpdate(mut c) => {
                c.account_id = acc.id.clone();
                if let Err(e) = self.deps.storage.wa_upsert_contact(&c).await {
                    tracing::warn!("wa_upsert_contact: {e}");
                }
            }
            BackendEvent::GroupUpdate {
                jid,
                subject,
                participants,
                admins,
                description,
            } => {
                if subject.is_none() && participants.is_empty() {
                    match acc.backend.fetch_group(&jid).await {
                        Ok(meta) => self.upsert_group_json(acc, &jid, &meta).await,
                        Err(e) => tracing::debug!("fetch_group {jid}: {e}"),
                    }
                } else if let Err(e) = self
                    .deps
                    .storage
                    .wa_upsert_group(
                        &acc.id,
                        &jid,
                        subject.as_deref(),
                        description.as_deref(),
                        &json!(participants),
                        &json!(admins),
                    )
                    .await
                {
                    tracing::warn!("wa_upsert_group: {e}");
                }
            }
            BackendEvent::ChatArchived { jid, archived } => {
                let st = &self.deps.storage;
                let _ = st
                    .wa_upsert_chat(&acc.id, &jid, None, is_group_jid(&jid))
                    .await;
                if let Err(e) = st.wa_set_chat_archived(&acc.id, &jid, archived).await {
                    tracing::warn!("wa_set_chat_archived: {e}");
                }
            }
        }
    }

    pub(crate) async fn upsert_group_json(&self, acc: &WaAccount, jid: &str, meta: &Value) {
        let subject = meta.get("subject").and_then(|v| v.as_str());
        let description = meta.get("description").and_then(|v| v.as_str());
        let participants = meta.get("participants").cloned().unwrap_or(json!([]));
        let admins = meta.get("admins").cloned().unwrap_or(json!([]));
        if let Err(e) = self
            .deps
            .storage
            .wa_upsert_group(&acc.id, jid, subject, description, &participants, &admins)
            .await
        {
            tracing::warn!("wa_upsert_group: {e}");
        }
    }

    async fn index_message(&self, acc: &WaAccount, m: IncomingMessage) {
        let st = &self.deps.storage;
        let chat_name = m.chat_name.clone().or_else(|| {
            if !m.is_group && !m.from_me {
                m.sender_name.clone()
            } else {
                None
            }
        });
        if let Err(e) = st
            .wa_upsert_chat(&acc.id, &m.chat_jid, chat_name.as_deref(), m.is_group)
            .await
        {
            tracing::warn!("wa_upsert_chat: {e}");
            return;
        }
        if !m.from_me && !m.sender_jid.is_empty() {
            let c = WaContact {
                account_id: acc.id.clone(),
                jid: m.sender_jid.clone(),
                phone: phone_from_jid(&m.sender_jid),
                name: None,
                push_name: m.sender_name.clone(),
            };
            if let Err(e) = st.wa_upsert_contact(&c).await {
                tracing::debug!("wa_upsert_contact: {e}");
            }
        }
        let msg = WaMessage {
            account_id: acc.id.clone(),
            id: m.id,
            chat_jid: m.chat_jid,
            sender_jid: Some(m.sender_jid),
            sender_name: m.sender_name,
            from_me: m.from_me,
            at: m.timestamp,
            kind: m.kind,
            text: m.text,
            media_ref: m.media_ref,
            media_path: None,
            quoted_id: m.quoted_id,
            read: m.from_me,
        };
        if let Err(e) = st.wa_insert_message(&msg).await {
            tracing::warn!("wa_insert_message: {e}");
        }
        *acc.last_sync.write() = Some(Utc::now());
    }

    async fn handle_receipt(
        &self,
        acc: &WaAccount,
        chat_jid: &str,
        message_ids: &[String],
        kind: &str,
    ) {
        let st = &self.deps.storage;
        if message_ids.is_empty() {
            if kind == "read" {
                let _ = st.wa_mark_read(&acc.id, chat_jid).await;
            }
            return;
        }
        let mut mark_chat = false;
        for id in message_ids {
            match st.wa_get_message(&acc.id, id).await {
                Ok(Some(m)) if m.from_me => {
                    let status = match kind {
                        "read" | "played" => "read",
                        "delivered" => "delivered",
                        other => other,
                    };
                    let _ = st.wa_set_outbox_status(&acc.id, id, status, None).await;
                }
                Ok(Some(_)) => {
                    if kind == "read" || kind == "played" {
                        mark_chat = true;
                    }
                }
                Ok(None) => {
                    // Mensagem nossa não indexada (enviada do telefone): só atualiza outbox se existir.
                    let _ = st.wa_set_outbox_status(&acc.id, id, kind, None).await;
                }
                Err(e) => tracing::debug!("wa_get_message {id}: {e}"),
            }
        }
        if mark_chat {
            let _ = st.wa_mark_read(&acc.id, chat_jid).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_default_label;

    #[test]
    fn default_label_detection() {
        assert!(is_default_label("Conta 1"));
        assert!(is_default_label("Conta 12"));
        assert!(!is_default_label("Conta"));
        assert!(!is_default_label("Conta x"));
        assert!(!is_default_label("Will"));
    }
}
