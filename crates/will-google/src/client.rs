//! `GoogleClient`: bearer por conta (cache de access token renovado via refresh
//! token do keyring), retry com backoff em 429/5xx (respeita `Retry-After`),
//! tratamento de `invalid_grant` → conta em `reauthorize`.

use crate::error::GoogleError;
use oauth2::basic::{BasicClient, BasicErrorResponseType};
use oauth2::{
    AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet, RefreshToken, RequestTokenError,
    RevocationUrl, StandardRevocableToken, TokenResponse, TokenUrl,
};
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use will_core::settings::GoogleSettings;
use will_core::EventBus;
use will_storage::google::GoogleAccount;
use will_storage::{SecretStore, Storage};

pub type Result<T> = std::result::Result<T, GoogleError>;

/// Cliente OAuth com auth/token/revocation configurados.
pub type OauthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointSet, EndpointSet>;

pub const DEFAULT_CLIENT_SECRET_KEY: &str = "google_client_secret";
pub const STATUS_OK: &str = "ok";
pub const STATUS_REAUTHORIZE: &str = "reauthorize";
const MAX_ATTEMPTS: u32 = 4;

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api_base: String,
    pub auth_url: String,
    pub token_url: String,
    pub revoke_url: String,
    pub userinfo_url: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api_base: "https://www.googleapis.com".into(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            revoke_url: "https://oauth2.googleapis.com/revoke".into(),
            userinfo_url: "https://openidconnect.googleapis.com/v1/userinfo".into(),
        }
    }
}

struct CachedToken {
    token: String,
    expires_at: Instant,
}

pub struct GoogleClient {
    http: reqwest::Client,
    oauth_http: oauth2::reqwest::Client,
    pub storage: Storage,
    pub secrets: Arc<dyn SecretStore>,
    pub bus: EventBus,
    settings: parking_lot::RwLock<GoogleSettings>,
    pub endpoints: Endpoints,
    tokens: parking_lot::Mutex<HashMap<String, CachedToken>>,
    refresh_lock: tokio::sync::Mutex<()>,
    accounts: parking_lot::RwLock<Vec<GoogleAccount>>,
}

pub fn refresh_token_key(account_id: &str) -> String {
    format!("google:{account_id}:refresh_token")
}

impl GoogleClient {
    pub fn new(
        storage: Storage,
        secrets: Arc<dyn SecretStore>,
        bus: EventBus,
        settings: GoogleSettings,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("will-ia/0.1")
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        let oauth_http = oauth2::reqwest::ClientBuilder::new()
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            http,
            oauth_http,
            storage,
            secrets,
            bus,
            settings: parking_lot::RwLock::new(settings),
            endpoints: Endpoints::default(),
            tokens: parking_lot::Mutex::new(HashMap::new()),
            refresh_lock: tokio::sync::Mutex::new(()),
            accounts: parking_lot::RwLock::new(Vec::new()),
        }
    }

    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn oauth_http(&self) -> &oauth2::reqwest::Client {
        &self.oauth_http
    }

    pub fn set_settings(&self, s: GoogleSettings) {
        *self.settings.write() = s;
    }

    pub fn client_id(&self) -> Option<String> {
        self.settings
            .read()
            .client_id
            .clone()
            .filter(|s| !s.trim().is_empty())
    }

    fn client_secret_key(&self) -> String {
        self.settings
            .read()
            .client_secret_ref
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CLIENT_SECRET_KEY.to_string())
    }

    /// Monta o cliente OAuth (exige client_id + client_secret no keyring).
    pub async fn oauth_client(&self) -> Result<OauthClient> {
        let client_id = self
            .client_id()
            .ok_or_else(|| GoogleError::Config("client_id não configurado".into()))?;
        let key = self.client_secret_key();
        let secret = self
            .secrets
            .get(&key)
            .await
            .map_err(GoogleError::Secret)?
            .ok_or_else(|| GoogleError::Config("client_secret não encontrado no keyring".into()))?;
        let auth = AuthUrl::new(self.endpoints.auth_url.clone())
            .map_err(|e| GoogleError::Config(e.to_string()))?;
        let token = TokenUrl::new(self.endpoints.token_url.clone())
            .map_err(|e| GoogleError::Config(e.to_string()))?;
        let revoke = RevocationUrl::new(self.endpoints.revoke_url.clone())
            .map_err(|e| GoogleError::Config(e.to_string()))?;
        Ok(BasicClient::new(ClientId::new(client_id))
            .set_client_secret(ClientSecret::new(secret.expose_secret().to_string()))
            .set_auth_uri(auth)
            .set_token_uri(token)
            .set_revocation_url(revoke))
    }

    // ----- contas -----

    pub async fn reload_accounts(&self) -> Result<Vec<GoogleAccount>> {
        let list = self.storage.list_google_accounts().await?;
        *self.accounts.write() = list.clone();
        Ok(list)
    }

    pub fn cached_accounts(&self) -> Vec<GoogleAccount> {
        self.accounts.read().clone()
    }

    /// Resolve por id ou e-mail (cache primeiro, depois banco).
    pub async fn find_account(&self, id_or_email: &str) -> Result<GoogleAccount> {
        let needle = id_or_email.trim();
        if let Some(a) = self
            .accounts
            .read()
            .iter()
            .find(|a| a.id == needle || a.email.eq_ignore_ascii_case(needle))
        {
            return Ok(a.clone());
        }
        let list = self.reload_accounts().await?;
        list.into_iter()
            .find(|a| a.id == needle || a.email.eq_ignore_ascii_case(needle))
            .ok_or_else(|| GoogleError::AccountNotFound(needle.to_string()))
    }

    pub async fn set_status(&self, account_id: &str, status: &str) -> Result<()> {
        self.storage
            .set_google_account_status(account_id, status)
            .await?;
        let _ = self.reload_accounts().await;
        Ok(())
    }

    pub fn invalidate_token(&self, account_id: &str) {
        self.tokens.lock().remove(account_id);
    }

    /// Para testes: injeta um access token sem passar pelo refresh.
    pub fn seed_token(&self, account_id: &str, token: &str, ttl: Duration) {
        self.tokens.lock().insert(
            account_id.to_string(),
            CachedToken {
                token: token.to_string(),
                expires_at: Instant::now() + ttl,
            },
        );
    }

    /// Guarda refresh token no keyring.
    pub async fn store_refresh_token(&self, account_id: &str, token: &SecretString) -> Result<()> {
        self.secrets
            .set(&refresh_token_key(account_id), token)
            .await
            .map_err(GoogleError::Secret)
    }

    pub async fn delete_refresh_token(&self, account_id: &str) -> Result<()> {
        self.invalidate_token(account_id);
        self.secrets
            .delete(&refresh_token_key(account_id))
            .await
            .map_err(GoogleError::Secret)
    }

    // ----- tokens -----

    /// Access token válido para a conta (cache com margem de 60 s; senão renova).
    pub async fn access_token(&self, account: &GoogleAccount) -> Result<String> {
        if let Some(t) = self.tokens.lock().get(&account.id) {
            if t.expires_at > Instant::now() {
                return Ok(t.token.clone());
            }
        }
        let _guard = self.refresh_lock.lock().await;
        // Outra task pode ter renovado enquanto esperávamos.
        if let Some(t) = self.tokens.lock().get(&account.id) {
            if t.expires_at > Instant::now() {
                return Ok(t.token.clone());
            }
        }
        self.refresh(account).await
    }

    async fn refresh(&self, account: &GoogleAccount) -> Result<String> {
        let key = if account.secret_ref.is_empty() {
            refresh_token_key(&account.id)
        } else {
            account.secret_ref.clone()
        };
        let refresh = match self.secrets.get(&key).await.map_err(GoogleError::Secret)? {
            Some(s) => s,
            None => {
                self.mark_reauthorize(account).await;
                return Err(GoogleError::Reauthorize(account.email.clone()));
            }
        };
        let client = self.oauth_client().await?;
        let rt = RefreshToken::new(refresh.expose_secret().to_string());
        match client
            .exchange_refresh_token(&rt)
            .request_async(&self.oauth_http)
            .await
        {
            Ok(tr) => {
                let token = tr.access_token().secret().to_string();
                let ttl = tr.expires_in().unwrap_or(Duration::from_secs(3600));
                let ttl = ttl
                    .saturating_sub(Duration::from_secs(60))
                    .max(Duration::from_secs(30));
                if let Some(new_rt) = tr.refresh_token() {
                    let _ = self
                        .store_refresh_token(
                            &account.id,
                            &SecretString::from(new_rt.secret().to_string()),
                        )
                        .await;
                }
                self.tokens.lock().insert(
                    account.id.clone(),
                    CachedToken {
                        token: token.clone(),
                        expires_at: Instant::now() + ttl,
                    },
                );
                if account.status != STATUS_OK {
                    let _ = self.set_status(&account.id, STATUS_OK).await;
                }
                Ok(token)
            }
            Err(RequestTokenError::ServerResponse(e))
                if *e.error() == BasicErrorResponseType::InvalidGrant =>
            {
                self.mark_reauthorize(account).await;
                Err(GoogleError::Reauthorize(account.email.clone()))
            }
            Err(RequestTokenError::ServerResponse(e)) => Err(GoogleError::OAuth(format!(
                "renovação de token falhou: {}{}",
                e.error(),
                e.error_description()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            ))),
            Err(e) => Err(GoogleError::Http(format!("renovação de token: {e}"))),
        }
    }

    async fn mark_reauthorize(&self, account: &GoogleAccount) {
        self.invalidate_token(&account.id);
        if account.status != STATUS_REAUTHORIZE {
            if let Err(e) = self.set_status(&account.id, STATUS_REAUTHORIZE).await {
                tracing::warn!(module = "google", error = %e, "não foi possível marcar conta como reauthorize");
            }
            self.bus.log(
                "warn",
                "google",
                format!(
                    "conta {} precisa reautorizar (invalid_grant)",
                    account.email
                ),
            );
        }
    }

    /// Revoga o refresh token no Google (best-effort).
    pub async fn revoke(&self, account: &GoogleAccount) -> Result<()> {
        let key = if account.secret_ref.is_empty() {
            refresh_token_key(&account.id)
        } else {
            account.secret_ref.clone()
        };
        let Some(refresh) = self.secrets.get(&key).await.map_err(GoogleError::Secret)? else {
            return Ok(());
        };
        let client = self.oauth_client().await?;
        let token: StandardRevocableToken =
            RefreshToken::new(refresh.expose_secret().to_string()).into();
        let req = client
            .revoke_token(token)
            .map_err(|e| GoogleError::OAuth(e.to_string()))?;
        req.request_async(&self.oauth_http)
            .await
            .map_err(|e| GoogleError::OAuth(format!("revogação falhou: {e}")))?;
        Ok(())
    }

    // ----- REST -----

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!(
                "{}/{}",
                self.endpoints.api_base.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        }
    }

    fn url_with_query(&self, path: &str, query: &[(&str, String)]) -> Result<url::Url> {
        let mut u = url::Url::parse(&self.url(path))
            .map_err(|e| GoogleError::Invalid(format!("URL inválida: {e}")))?;
        if !query.is_empty() {
            let mut qp = u.query_pairs_mut();
            for (k, v) in query {
                qp.append_pair(k, v);
            }
        }
        Ok(u)
    }

    /// Requisição JSON autenticada com retry/backoff. `query` aceita chaves repetidas.
    pub async fn request(
        &self,
        account: &GoogleAccount,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let url = self.url_with_query(path, query)?;
        let mut attempt = 0u32;
        let mut refreshed_after_401 = false;
        loop {
            attempt += 1;
            let token = self.access_token(account).await?;
            let mut req = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(&token);
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt < MAX_ATTEMPTS && (e.is_timeout() || e.is_connect()) {
                        tokio::time::sleep(backoff(attempt, None)).await;
                        continue;
                    }
                    return Err(e.into());
                }
            };
            let status = resp.status();
            if status.is_success() {
                if status == reqwest::StatusCode::NO_CONTENT {
                    return Ok(serde_json::Value::Null);
                }
                let bytes = resp.bytes().await?;
                if bytes.is_empty() {
                    return Ok(serde_json::Value::Null);
                }
                return serde_json::from_slice(&bytes)
                    .map_err(|e| GoogleError::Http(format!("resposta JSON inválida: {e}")));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED && !refreshed_after_401 {
                refreshed_after_401 = true;
                self.invalidate_token(&account.id);
                continue;
            }
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(backoff(attempt, retry_after)).await;
                continue;
            }
            let text = resp.text().await.unwrap_or_default();
            return Err(parse_api_error(status.as_u16(), &text));
        }
    }

    pub async fn get(
        &self,
        account: &GoogleAccount,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value> {
        self.request(account, reqwest::Method::GET, path, query, None)
            .await
    }

    pub async fn post(
        &self,
        account: &GoogleAccount,
        path: &str,
        query: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.request(account, reqwest::Method::POST, path, query, Some(body))
            .await
    }

    pub async fn patch(
        &self,
        account: &GoogleAccount,
        path: &str,
        query: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.request(account, reqwest::Method::PATCH, path, query, Some(body))
            .await
    }

    pub async fn put(
        &self,
        account: &GoogleAccount,
        path: &str,
        query: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.request(account, reqwest::Method::PUT, path, query, Some(body))
            .await
    }

    pub async fn delete(
        &self,
        account: &GoogleAccount,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value> {
        self.request(account, reqwest::Method::DELETE, path, query, None)
            .await
    }

    /// GET paginado: acumula `items_key` de cada página seguindo `nextPageToken`
    /// até `max_items` (ou todas as páginas se `None`).
    pub async fn get_all_pages(
        &self,
        account: &GoogleAccount,
        path: &str,
        query: &[(&str, String)],
        items_key: &str,
        max_items: Option<usize>,
    ) -> Result<Vec<serde_json::Value>> {
        let mut out = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut q: Vec<(&str, String)> = query.to_vec();
            if let Some(t) = &page_token {
                q.push(("pageToken", t.clone()));
            }
            let v = self.get(account, path, &q).await?;
            if let Some(items) = v.get(items_key).and_then(|i| i.as_array()) {
                out.extend(items.iter().cloned());
            }
            if let Some(max) = max_items {
                if out.len() >= max {
                    out.truncate(max);
                    break;
                }
            }
            match v.get("nextPageToken").and_then(|t| t.as_str()) {
                Some(t) if !t.is_empty() => page_token = Some(t.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }
}

fn backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(d) = retry_after {
        return d.min(Duration::from_secs(30));
    }
    let ms = 500u64.saturating_mul(1u64 << attempt.min(6));
    Duration::from_millis(ms.min(10_000))
}

/// Interpreta o JSON de erro da Google (`{"error":{"code","message","errors":[{"reason"}],"status"}}`).
pub fn parse_api_error(status: u16, text: &str) -> GoogleError {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
    let err = v.get("error");
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
        .or_else(|| err.and_then(|e| e.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| will_core::tools::truncate(text.trim(), 300));
    let reason = err
        .and_then(|e| e.get("errors"))
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|f| f.get("reason"))
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            err.and_then(|e| e.get("status"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        });
    GoogleError::Api {
        status,
        reason,
        message,
    }
}
