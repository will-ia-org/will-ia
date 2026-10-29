//! will-google — Conta Google (OAuth PKCE + loopback), Agenda e Gmail.
//!
//! Ver `docs/18-conta-google.md`, `docs/19-google-agenda.md`, `docs/20-gmail.md`.
//! O módulo não cria threads: só oferece ferramentas (`calendar_*`, `gmail_*`)
//! via `ToolProvider` e uma API consumida pelo servidor local (contas, agendas).

pub mod calendar;
pub mod client;
pub mod error;
pub mod gmail;
pub mod oauth;

pub use client::{GoogleClient, STATUS_OK, STATUS_REAUTHORIZE};
pub use error::GoogleError;

use client::refresh_token_key;
use oauth2::{
    AuthorizationCode, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse,
};
use secrecy::SecretString;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use will_core::settings::{CalendarSettings, GmailSettings, GoogleSettings};
use will_core::{Event, EventBus, Module, ModuleStatus, ToolProvider};
use will_storage::google::{GoogleAccount, GoogleCalendar};
use will_storage::{SecretStore, Storage};

const MODULE: &str = "google";

/// Fluxo OAuth aguardando o `code` que o cliente vai repassar.
struct PendingFlow {
    verifier: PkceCodeVerifier,
    redirect: RedirectUrl,
    requested: Vec<String>,
    existing: Option<GoogleAccount>,
    created: Instant,
}

/// Início de um fluxo OAuth. `state` presente = fluxo dirigido pelo cliente
/// (ele escuta o callback e chama `complete_flow`); ausente = o servidor
/// conclui sozinho pelo loopback local.
pub struct ConnectStart {
    pub auth_url: String,
    pub state: Option<String>,
}

/// Dados extraídos da resposta de token, sem expor o tipo do oauth2.
struct ExchangeOut {
    access: String,
    refresh: Option<String>,
    granted: Option<Vec<String>>,
    ttl: Duration,
}

pub struct GoogleDeps {
    pub storage: Storage,
    pub secrets: Arc<dyn SecretStore>,
    pub bus: EventBus,
    /// Raiz da base de conhecimento (anexos do Gmail vão para `<kb>/<attachments_dir>`).
    pub kb_path: PathBuf,
}

#[derive(Clone, Default)]
pub struct GoogleConfig {
    pub google: GoogleSettings,
    pub calendar: CalendarSettings,
    pub gmail: GmailSettings,
}

pub struct GoogleModule {
    me: Weak<GoogleModule>,
    pub(crate) client: Arc<GoogleClient>,
    cfg: parking_lot::RwLock<GoogleConfig>,
    pub(crate) kb_path: PathBuf,
    pending_flow: parking_lot::Mutex<Option<CancellationToken>>,
    /// Fluxos OAuth iniciados pelo servidor e concluídos pelo cliente remoto
    /// (ele escuta o callback de loopback e devolve o `code`), por `state`.
    pending_flows: parking_lot::Mutex<HashMap<String, PendingFlow>>,
    pub(crate) calendars_cache: parking_lot::RwLock<Vec<GoogleCalendar>>,
    pub(crate) gmail_state: gmail::GmailState,
}

impl GoogleModule {
    pub fn new(deps: GoogleDeps, cfg: GoogleConfig) -> Arc<Self> {
        let client = Arc::new(GoogleClient::new(
            deps.storage,
            deps.secrets,
            deps.bus,
            cfg.google.clone(),
        ));
        let module = Arc::new_cyclic(|me| GoogleModule {
            me: me.clone(),
            client,
            cfg: parking_lot::RwLock::new(cfg),
            kb_path: deps.kb_path,
            pending_flow: parking_lot::Mutex::new(None),
            pending_flows: parking_lot::Mutex::new(HashMap::new()),
            calendars_cache: parking_lot::RwLock::new(Vec::new()),
            gmail_state: gmail::GmailState::new(),
        });
        // Carrega contas/agendas em segundo plano (se houver runtime).
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let m = module.clone();
            handle.spawn(async move {
                if let Err(e) = m.client.reload_accounts().await {
                    tracing::warn!(module = MODULE, error = %e, "falha ao carregar contas Google");
                }
                let _ = m.enabled_calendars().await;
                m.emit_status();
            });
        }
        module
    }

    /// Para testes: constrói o módulo em torno de um cliente já configurado.
    pub fn with_client(
        client: Arc<GoogleClient>,
        cfg: GoogleConfig,
        kb_path: PathBuf,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| GoogleModule {
            me: me.clone(),
            client,
            cfg: parking_lot::RwLock::new(cfg),
            kb_path,
            pending_flow: parking_lot::Mutex::new(None),
            pending_flows: parking_lot::Mutex::new(HashMap::new()),
            calendars_cache: parking_lot::RwLock::new(Vec::new()),
            gmail_state: gmail::GmailState::new(),
        })
    }

    pub fn update_config(&self, cfg: GoogleConfig) {
        self.client.set_settings(cfg.google.clone());
        *self.cfg.write() = cfg;
        self.emit_status();
    }

    pub fn config(&self) -> GoogleConfig {
        self.cfg.read().clone()
    }

    pub fn client(&self) -> &Arc<GoogleClient> {
        &self.client
    }

    /// Estado da conta Google: `Disabled` sem `client_id`; `Degraded` sem conta
    /// ou com conta precisando reautorizar; `Running` = pelo menos uma conta ok.
    pub fn status(&self) -> ModuleStatus {
        if self.client.client_id().is_none() {
            return ModuleStatus::Disabled;
        }
        let accounts = self.client.cached_accounts();
        if accounts.is_empty() {
            return ModuleStatus::Degraded("nenhuma conta Google conectada".into());
        }
        if let Some(a) = accounts.iter().find(|a| a.status == STATUS_REAUTHORIZE) {
            return ModuleStatus::Degraded(format!(
                "token da conta {} expirado; reautorize",
                a.email
            ));
        }
        if let Some(a) = accounts.iter().find(|a| a.status != STATUS_OK) {
            return ModuleStatus::Degraded(format!("conta {}: {}", a.email, a.status));
        }
        ModuleStatus::Running
    }

    /// Estado da Agenda: `Disabled` se desligada; senão `Running` quando há
    /// conta ok com escopo de agenda, ou `Degraded` com o motivo.
    pub fn calendar_status(&self) -> ModuleStatus {
        if !self.config().calendar.enabled {
            return ModuleStatus::Disabled;
        }
        self.feature_status(self.client.cached_accounts(), "/auth/calendar", "Agenda")
    }

    /// Estado do Gmail: `Disabled` se desligado; senão `Running` quando há conta
    /// ok (entre as permitidas em `gmail.accounts`) com escopo do Gmail.
    pub fn gmail_status(&self) -> ModuleStatus {
        if !self.config().gmail.enabled {
            return ModuleStatus::Disabled;
        }
        self.feature_status(self.gmail_accounts(), "/auth/gmail", "Gmail")
    }

    fn feature_status(
        &self,
        accounts: Vec<GoogleAccount>,
        scope_fragment: &str,
        feature: &str,
    ) -> ModuleStatus {
        if self.client.client_id().is_none() {
            return ModuleStatus::Degraded("credenciais OAuth do Google não configuradas".into());
        }
        if accounts.is_empty() {
            return ModuleStatus::Degraded("nenhuma conta Google conectada".into());
        }
        let with_scope: Vec<&GoogleAccount> = accounts
            .iter()
            .filter(|a| a.scopes.iter().any(|s| s.contains(scope_fragment)))
            .collect();
        if with_scope.is_empty() {
            return ModuleStatus::Degraded(format!(
                "nenhuma conta autorizou o {feature}; reautorize incluindo esse acesso"
            ));
        }
        if with_scope.iter().any(|a| a.status == STATUS_OK) {
            return ModuleStatus::Running;
        }
        match with_scope.iter().find(|a| a.status == STATUS_REAUTHORIZE) {
            Some(a) => {
                ModuleStatus::Degraded(format!("token da conta {} expirado; reautorize", a.email))
            }
            None => ModuleStatus::Degraded(format!(
                "conta {}: {}",
                with_scope[0].email, with_scope[0].status
            )),
        }
    }

    pub(crate) fn emit_status(&self) {
        self.client.bus.emit(Event::ModuleStatus {
            module: Module::Google,
            status: self.status(),
        });
    }

    pub(crate) fn ok_accounts(&self) -> Vec<GoogleAccount> {
        self.client
            .cached_accounts()
            .into_iter()
            .filter(|a| a.status == STATUS_OK)
            .collect()
    }

    pub(crate) fn has_ok_account(&self) -> bool {
        !self.ok_accounts().is_empty()
    }

    /// Conta pronta para uso (existe e não está marcada para reautorizar).
    pub(crate) async fn account_ready(
        &self,
        id_or_email: &str,
    ) -> Result<GoogleAccount, GoogleError> {
        let a = self.client.find_account(id_or_email).await?;
        if a.status == STATUS_REAUTHORIZE {
            return Err(GoogleError::Reauthorize(a.email));
        }
        Ok(a)
    }

    pub(crate) async fn enabled_calendars(&self) -> Result<Vec<GoogleCalendar>, GoogleError> {
        let all = self.client.storage.list_google_calendars(false).await?;
        *self.calendars_cache.write() = all.clone();
        Ok(all.into_iter().filter(|c| c.enabled).collect())
    }

    // ----- OAuth -----

    /// Inicia a conexão de uma conta nova. Retorna a URL de autorização; o
    /// restante (callback, troca, userinfo, persistência) corre em segundo plano
    /// e é sinalizado por `Event::Log` + `Event::ModuleStatus`.
    pub async fn start_connect(
        &self,
        scopes: Vec<String>,
        open_browser: bool,
        callback_port: Option<u16>,
    ) -> Result<ConnectStart, String> {
        let urls = oauth::logical_to_scopes(&scopes).map_err(|e| e.to_string())?;
        self.begin_flow(urls, None, open_browser, callback_port)
            .await
    }

    /// Reautoriza uma conta existente (consentimento incremental: inclui os
    /// escopos já concedidos antes).
    pub async fn reauthorize(
        &self,
        account_id: &str,
        open_browser: bool,
        callback_port: Option<u16>,
    ) -> Result<ConnectStart, String> {
        let account = self
            .client
            .find_account(account_id)
            .await
            .map_err(|e| e.to_string())?;
        let mut wanted = Vec::new();
        let cfg = self.config();
        if cfg.calendar.enabled {
            wanted.push(
                if cfg.calendar.allow_write {
                    "calendar_write"
                } else {
                    "calendar"
                }
                .to_string(),
            );
        }
        if cfg.gmail.enabled {
            wanted.push("gmail".into());
            if cfg.gmail.allow_send {
                wanted.push("gmail_send".into());
            }
            if cfg.gmail.allow_modify {
                wanted.push("gmail_modify".into());
            }
        }
        let urls = oauth::logical_to_scopes(&wanted).map_err(|e| e.to_string())?;
        let urls = oauth::merge_scopes(&urls, &account.scopes);
        self.begin_flow(urls, Some(account), open_browser, callback_port)
            .await
    }

    async fn begin_flow(
        &self,
        scopes: Vec<String>,
        existing: Option<GoogleAccount>,
        open_browser: bool,
        callback_port: Option<u16>,
    ) -> Result<ConnectStart, String> {
        let client = self
            .client
            .oauth_client()
            .await
            .map_err(|e| e.to_string())?;
        let fixed_port = self.config().google.redirect_port;
        // Cliente remoto: ele mesmo escuta o callback na porta `callback_port` e
        // devolve o `code` em `complete_flow`. Local: abrimos o loopback aqui.
        let server_listener = match callback_port {
            Some(_) => None,
            None => {
                let (listener, port) = oauth::bind_loopback(fixed_port).await.map_err(|e| match fixed_port {
                    Some(p) => format!("não consegui abrir a porta fixa {p} do callback ({e}); feche o que a ocupa ou troque a porta em Configurações › Google"),
                    None => e.to_string(),
                })?;
                Some((listener, port))
            }
        };
        let port = match (callback_port, &server_listener) {
            (Some(p), _) => p,
            (None, Some((_, p))) => *p,
            (None, None) => unreachable!("sem porta de callback"),
        };
        let redirect = RedirectUrl::new(format!("http://127.0.0.1:{port}{}", oauth::CALLBACK_PATH))
            .map_err(|e| e.to_string())?;
        let client = client.set_redirect_uri(redirect.clone());
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut req = client
            .authorize_url(CsrfToken::new_random)
            .set_pkce_challenge(challenge)
            .add_extra_param("access_type", "offline")
            .add_extra_param("prompt", "consent")
            .add_extra_param("include_granted_scopes", "true");
        for sc in &scopes {
            req = req.add_scope(Scope::new(sc.clone()));
        }
        if let Some(acc) = &existing {
            req = req.add_extra_param("login_hint", acc.email.clone());
        }
        let (url, state) = req.url();
        let url_s = url.to_string();

        match server_listener {
            // Modo local: o servidor escuta o loopback e conclui em segundo plano.
            Some((listener, _)) => {
                let me = self
                    .me
                    .upgrade()
                    .ok_or_else(|| "módulo encerrado".to_string())?;
                let cancel = CancellationToken::new();
                if let Some(prev) = self.pending_flow.lock().replace(cancel.clone()) {
                    prev.cancel();
                }
                let bus = self.client.bus.clone();
                tokio::spawn(async move {
                    let result = me
                        .finish_flow(listener, client, verifier, state, scopes, existing, cancel)
                        .await;
                    match result {
                        Ok(email) => {
                            bus.log("info", MODULE, format!("conta Google {email} conectada"))
                        }
                        Err(e) => bus.log(
                            "error",
                            MODULE,
                            format!("falha ao conectar conta Google: {e}"),
                        ),
                    }
                    me.emit_status();
                });
                if open_browser {
                    if let Err(e) = open::that_detached(&url_s) {
                        self.client.bus.log(
                            "warn",
                            MODULE,
                            format!("não foi possível abrir o navegador: {e}"),
                        );
                    }
                }
                Ok(ConnectStart {
                    auth_url: url_s,
                    state: None,
                })
            }
            // Modo cliente remoto: guardamos o fluxo e esperamos o `code`.
            None => {
                self.prune_pending_flows();
                self.pending_flows.lock().insert(
                    state.secret().to_string(),
                    PendingFlow {
                        verifier,
                        redirect,
                        requested: scopes,
                        existing,
                        created: Instant::now(),
                    },
                );
                if open_browser {
                    let _ = open::that_detached(&url_s);
                }
                Ok(ConnectStart {
                    auth_url: url_s,
                    state: Some(state.secret().to_string()),
                })
            }
        }
    }

    /// Remove fluxos pendentes mais velhos que o timeout.
    fn prune_pending_flows(&self) {
        let mut g = self.pending_flows.lock();
        g.retain(|_, f| f.created.elapsed() < oauth::FLOW_TIMEOUT);
    }

    /// Conclui um fluxo iniciado para cliente remoto: troca o `code` pelo token
    /// e persiste a conta. Chamado pela rota `POST /api/google/callback`.
    pub async fn complete_flow(&self, state: &str, code: &str) -> Result<String, String> {
        self.prune_pending_flows();
        let pf = self
            .pending_flows
            .lock()
            .remove(state)
            .ok_or_else(|| "fluxo de autorização expirado ou desconhecido".to_string())?;
        let client = self
            .client
            .oauth_client()
            .await
            .map_err(|e| e.to_string())?;
        let client = client.set_redirect_uri(pf.redirect);
        let tr = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(pf.verifier)
            .request_async(self.client.oauth_http())
            .await
            .map_err(|e| format!("troca de código falhou: {e}"))?;
        let out = ExchangeOut {
            access: tr.access_token().secret().to_string(),
            refresh: tr.refresh_token().map(|r| r.secret().to_string()),
            granted: tr
                .scopes()
                .map(|s| s.iter().map(|x| x.to_string()).collect()),
            ttl: tr.expires_in().unwrap_or(Duration::from_secs(3600)),
        };
        let email = self
            .persist_exchange(out, pf.requested, pf.existing)
            .await
            .map_err(|e| e.to_string())?;
        self.client
            .bus
            .log("info", MODULE, format!("conta Google {email} conectada"));
        self.emit_status();
        Ok(email)
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_flow(
        &self,
        listener: tokio::net::TcpListener,
        client: client::OauthClient,
        verifier: PkceCodeVerifier,
        state: CsrfToken,
        requested: Vec<String>,
        existing: Option<GoogleAccount>,
        cancel: CancellationToken,
    ) -> Result<String, GoogleError> {
        let cb =
            oauth::wait_callback(listener, state.secret(), oauth::FLOW_TIMEOUT, cancel).await?;
        let tr = client
            .exchange_code(AuthorizationCode::new(cb.code))
            .set_pkce_verifier(verifier)
            .request_async(self.client.oauth_http())
            .await
            .map_err(|e| GoogleError::OAuth(format!("troca de código falhou: {e}")))?;
        let out = ExchangeOut {
            access: tr.access_token().secret().to_string(),
            refresh: tr.refresh_token().map(|r| r.secret().to_string()),
            granted: tr
                .scopes()
                .map(|s| s.iter().map(|x| x.to_string()).collect()),
            ttl: tr.expires_in().unwrap_or(Duration::from_secs(3600)),
        };
        self.persist_exchange(out, requested, existing).await
    }

    /// Troca concluída (userinfo + persistência da conta e do refresh token).
    /// Compartilhado pelo fluxo local (`finish_flow`) e pelo remoto
    /// (`complete_flow`).
    async fn persist_exchange(
        &self,
        out: ExchangeOut,
        requested: Vec<String>,
        existing: Option<GoogleAccount>,
    ) -> Result<String, GoogleError> {
        let access = out.access;
        let granted: Vec<String> = out
            .granted
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or(requested);
        let info = oauth::fetch_userinfo(
            self.client.http(),
            &self.client.endpoints.userinfo_url,
            &access,
        )
        .await?;
        let email = info.email.filter(|e| !e.is_empty()).ok_or_else(|| {
            GoogleError::OAuth("Google não devolveu o e-mail da conta (escopo email)".into())
        })?;

        let all = self.client.storage.list_google_accounts().await?;
        let prior = all
            .into_iter()
            .find(|a| a.email.eq_ignore_ascii_case(&email))
            .or_else(|| existing.filter(|a| a.email.eq_ignore_ascii_case(&email)));
        let id = prior
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

        match &out.refresh {
            Some(rt) => {
                self.client
                    .store_refresh_token(&id, &SecretString::from(rt.clone()))
                    .await?;
            }
            None => {
                let has_old = self
                    .client
                    .secrets
                    .get(&refresh_token_key(&id))
                    .await
                    .map_err(GoogleError::Secret)?
                    .is_some();
                if !has_old {
                    return Err(GoogleError::OAuth(
                        "Google não devolveu refresh token; remova o acesso do app em myaccount.google.com/permissions e tente de novo".into(),
                    ));
                }
            }
        }
        let scopes = match &prior {
            Some(p) => oauth::merge_scopes(&p.scopes, &granted),
            None => granted,
        };
        let account = GoogleAccount {
            id: id.clone(),
            email: email.clone(),
            secret_ref: refresh_token_key(&id),
            scopes,
            status: STATUS_OK.into(),
            connected_at: prior
                .as_ref()
                .map(|p| p.connected_at)
                .unwrap_or_else(chrono::Utc::now),
            last_used_at: None,
        };
        self.client.storage.upsert_google_account(&account).await?;
        let ttl = out.ttl.saturating_sub(Duration::from_secs(60));
        self.client
            .seed_token(&id, &access, ttl.max(Duration::from_secs(30)));
        self.client.reload_accounts().await?;
        Ok(email)
    }

    /// Revoga no Google (best-effort), apaga o refresh token e a conta.
    pub async fn disconnect(&self, account_id: &str) -> Result<(), String> {
        let account = self
            .client
            .find_account(account_id)
            .await
            .map_err(|e| e.to_string())?;
        if let Err(e) = self.client.revoke(&account).await {
            self.client.bus.log(
                "warn",
                MODULE,
                format!("revogação no Google falhou para {}: {e}", account.email),
            );
        }
        self.client
            .delete_refresh_token(&account.id)
            .await
            .map_err(|e| e.to_string())?;
        self.client
            .storage
            .delete_google_account(&account.id)
            .await
            .map_err(|e| e.to_string())?;
        self.client
            .reload_accounts()
            .await
            .map_err(|e| e.to_string())?;
        let _ = self.enabled_calendars().await;
        self.client.bus.log(
            "info",
            MODULE,
            format!("conta Google {} desconectada", account.email),
        );
        self.emit_status();
        Ok(())
    }

    pub async fn list_accounts(&self) -> Result<Vec<GoogleAccount>, String> {
        self.client
            .reload_accounts()
            .await
            .map_err(|e| e.to_string())
    }

    /// `calendarList.list` de cada conta; faz upsert preservando flags e
    /// devolve todas as agendas conhecidas (habilitadas ou não).
    pub async fn refresh_calendars(&self) -> Result<Vec<GoogleCalendar>, String> {
        let accounts = self
            .client
            .reload_accounts()
            .await
            .map_err(|e| e.to_string())?;
        let mut errors = Vec::new();
        let mut ok = 0usize;
        for acc in accounts.iter().filter(|a| a.status == STATUS_OK) {
            match self
                .client
                .get_all_pages(
                    acc,
                    "calendar/v3/users/me/calendarList",
                    &[("maxResults", "250".into())],
                    "items",
                    None,
                )
                .await
            {
                Ok(items) => {
                    ok += 1;
                    for it in items {
                        let Some(id) = it.get("id").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        let s = |k: &str| it.get(k).and_then(|v| v.as_str()).map(|v| v.to_string());
                        let primary = it.get("primary").and_then(|v| v.as_bool()).unwrap_or(false);
                        let cal = GoogleCalendar {
                            account_id: acc.id.clone(),
                            calendar_id: id.to_string(),
                            summary: s("summaryOverride")
                                .or_else(|| s("summary"))
                                .unwrap_or_else(|| id.to_string()),
                            timezone: s("timeZone"),
                            access_role: s("accessRole"),
                            color: s("backgroundColor"),
                            // Novas agendas: só a principal nasce habilitada (upsert preserva flags).
                            enabled: primary,
                            default_for_create: false,
                        };
                        if let Err(e) = self.client.storage.upsert_google_calendar(&cal).await {
                            errors.push(format!("{}: {e}", acc.email));
                        }
                    }
                }
                Err(e) => errors.push(format!("{}: {e}", acc.email)),
            }
        }
        for e in &errors {
            self.client
                .bus
                .log("warn", MODULE, format!("atualização de agendas: {e}"));
        }
        if ok == 0 && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        let all = self
            .client
            .storage
            .list_google_calendars(false)
            .await
            .map_err(|e| e.to_string())?;
        *self.calendars_cache.write() = all.clone();
        Ok(all)
    }

    pub async fn set_calendar_flags(
        &self,
        account_id: &str,
        calendar_id: &str,
        enabled: bool,
        default_for_create: bool,
    ) -> Result<(), String> {
        self.client
            .storage
            .set_google_calendar_flags(account_id, calendar_id, enabled, default_for_create)
            .await
            .map_err(|e| e.to_string())?;
        let _ = self.enabled_calendars().await;
        Ok(())
    }

    pub fn calendar_provider(self: &Arc<Self>) -> Arc<dyn ToolProvider> {
        Arc::new(calendar::CalendarProvider::new(self.clone()))
    }

    pub fn gmail_provider(self: &Arc<Self>) -> Arc<dyn ToolProvider> {
        Arc::new(gmail::GmailProvider::new(self.clone()))
    }
}
