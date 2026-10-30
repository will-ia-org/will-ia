//! will-client — cliente tipado da API local do will-ia (`docs/22`) para o app
//! desktop e para automações. HTTP/1.1-2 via `reqwest`, eventos pelo WS
//! `/api/events` (com reconexão) e áudio da voz pelo WS `/api/voice/audio`.
//!
//! O app nunca fala com o `AppState` diretamente: tudo passa por aqui, em
//! loopback ou remoto (`docs/26`, ADR-0019).

pub mod voice;

use futures::StreamExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use will_core::Event;

pub use voice::{AudioOut, VoiceLink};

/// Como validar o certificado do servidor (só para `https://`).
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    /// Certificado/CA do servidor em PEM (o autoassinado gerado por ele), fixado no perfil.
    pub ca_pem: Option<String>,
    /// Aceitar qualquer certificado (não recomendado; só para testes).
    pub insecure: bool,
}

#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
    /// Config rustls para o WebSocket (mesma política do HTTP).
    ws_tls: Option<Arc<rustls::ClientConfig>>,
}

/// Verificador que aceita qualquer certificado (modo inseguro explícito).
#[derive(Debug)]
struct NoVerify(rustls::crypto::CryptoProvider);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn ws_tls_config(tls: &TlsOptions) -> Option<Arc<rustls::ClientConfig>> {
    let provider = rustls::crypto::ring::default_provider();
    if tls.insecure {
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(provider.clone()))
            .with_safe_default_protocol_versions()
            .ok()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
            .with_no_client_auth();
        return Some(Arc::new(cfg));
    }
    let pem = tls.ca_pem.as_deref()?.trim();
    if pem.is_empty() {
        return None;
    }
    let mut roots = rustls::RootCertStore::empty();
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    for cert in rustls_pemfile::certs(&mut reader).flatten() {
        let _ = roots.add(cert);
    }
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Some(Arc::new(cfg))
}

/// SHA-256 (hex) do primeiro certificado de um PEM, para conferir com `/api/health`.
pub fn pem_fingerprint(pem: &str) -> Option<String> {
    use sha2::Digest;
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    let cert = rustls_pemfile::certs(&mut reader).next()?.ok()?;
    let mut h = sha2::Sha256::new();
    h.update(cert.as_ref());
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Resultado padrão: corpo JSON ou mensagem de erro legível (campo `error`
/// da API, ou erro de transporte).
pub type ApiResult = Result<Value, String>;

/// O que o laço de eventos entrega ao app.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    Connected,
    Disconnected(String),
    Event(Event),
}

impl Client {
    /// `base` = `http://127.0.0.1:47300` (sem barra final). `token` = token de sessão.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        Self::with_tls(base, token, &TlsOptions::default())
    }

    /// Como [`new`](Self::new), com política de certificado para `https://`.
    pub fn with_tls(base: impl Into<String>, token: impl Into<String>, tls: &TlsOptions) -> Self {
        let mut b = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(5));
        if tls.insecure {
            b = b.danger_accept_invalid_certs(true);
        } else if let Some(pem) = tls
            .ca_pem
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            if let Ok(certs) = reqwest::Certificate::from_pem_bundle(pem.as_bytes()) {
                for c in certs {
                    b = b.add_root_certificate(c);
                }
            }
        }
        let http = b.build().unwrap_or_default();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http,
            ws_tls: ws_tls_config(tls),
        }
    }

    /// Abre um WebSocket na API respeitando a política TLS do cliente.
    pub async fn connect_ws(
        &self,
        url: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Error,
    > {
        let connector = self
            .ws_tls
            .clone()
            .map(tokio_tungstenite::Connector::Rustls);
        let (ws, _) =
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, connector).await?;
        Ok(ws)
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// `ws://…` ou `wss://…` equivalente à base.
    pub fn ws_base(&self) -> String {
        if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            format!("ws://{}", self.base)
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Verdadeiro quando o servidor é esta máquina (loopback): a UI pode
    /// oferecer "abrir pasta" e outras ações locais.
    pub fn is_local(&self) -> bool {
        let host = self
            .base
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]")
    }

    fn url(&self, path: &str) -> String {
        let p = path.trim_start_matches('/');
        if p.starts_with("api/") {
            format!("{}/{p}", self.base)
        } else {
            format!("{}/api/{p}", self.base)
        }
    }

    async fn finish(resp: Result<reqwest::Response, reqwest::Error>) -> ApiResult {
        let resp = resp.map_err(|e| transport_error(&e))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status.is_success() {
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)));
        }
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| {
                if text.trim().is_empty() {
                    format!("HTTP {status}")
                } else {
                    will_core::tools::truncate(text.trim(), 300)
                }
            });
        Err(msg)
    }

    // ----- genéricos (qualquer rota de docs/22) -----

    pub async fn get(&self, path: &str) -> ApiResult {
        Self::finish(
            self.http
                .get(self.url(path))
                .bearer_auth(&self.token)
                .send()
                .await,
        )
        .await
    }

    /// GET com query string (`&[("limit", "300")]`); valores vazios são omitidos.
    pub async fn get_q(&self, path: &str, query: &[(&str, String)]) -> ApiResult {
        let url = format!("{}{}", self.url(path), query_string(query));
        Self::finish(self.http.get(url).bearer_auth(&self.token).send().await).await
    }

    pub async fn post(&self, path: &str, body: Option<Value>) -> ApiResult {
        let mut r = self.http.post(self.url(path)).bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        Self::finish(r.send().await).await
    }

    pub async fn put(&self, path: &str, body: Value) -> ApiResult {
        Self::finish(
            self.http
                .put(self.url(path))
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await,
        )
        .await
    }

    pub async fn patch(&self, path: &str, body: Value) -> ApiResult {
        Self::finish(
            self.http
                .patch(self.url(path))
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await,
        )
        .await
    }

    pub async fn delete(&self, path: &str) -> ApiResult {
        Self::finish(
            self.http
                .delete(self.url(path))
                .bearer_auth(&self.token)
                .send()
                .await,
        )
        .await
    }

    pub async fn delete_q(&self, path: &str, query: &[(&str, String)]) -> ApiResult {
        let url = format!("{}{}", self.url(path), query_string(query));
        Self::finish(self.http.delete(url).bearer_auth(&self.token).send().await).await
    }

    // ----- autenticação e usuários -----

    /// `POST /api/auth/login` sem token (função associada: ainda não há sessão).
    pub async fn login(
        base: &str,
        login: &str,
        password: &str,
        client: Option<&str>,
    ) -> Result<Value, String> {
        let c = Client::new(base, "");
        let url = c.url("auth/login");
        let body = json!({ "login": login, "password": password, "client": client });
        Self::finish(c.http.post(url).json(&body).send().await).await
    }
    pub async fn logout(&self) -> ApiResult {
        self.post("auth/logout", None).await
    }
    pub async fn me(&self) -> ApiResult {
        self.get("auth/me").await
    }
    pub async fn change_password(&self, current: &str, new: &str) -> ApiResult {
        self.post(
            "auth/password",
            Some(json!({ "current": current, "new": new })),
        )
        .await
    }
    pub async fn users(&self) -> ApiResult {
        self.get("users").await
    }
    pub async fn create_user(&self, body: Value) -> ApiResult {
        self.post("users", Some(body)).await
    }
    pub async fn patch_user(&self, id: &str, body: Value) -> ApiResult {
        self.patch(&format!("users/{id}"), body).await
    }
    pub async fn deactivate_user(&self, id: &str) -> ApiResult {
        self.delete(&format!("users/{id}")).await
    }
    pub async fn user_sessions(&self, id: &str) -> ApiResult {
        self.get(&format!("users/{id}/sessions")).await
    }

    // ----- sistema -----

    pub async fn health(&self) -> ApiResult {
        self.get("health").await
    }
    pub async fn status(&self) -> ApiResult {
        self.get("status").await
    }
    pub async fn settings(&self) -> ApiResult {
        self.get("settings").await
    }
    /// `PUT /api/settings { section, value }`.
    pub async fn put_settings(&self, section: &str, value: Value) -> ApiResult {
        self.put("settings", json!({ "section": section, "value": value }))
            .await
    }
    pub async fn set_secret(&self, key: &str, value: &str) -> ApiResult {
        self.post("secrets", Some(json!({ "key": key, "value": value })))
            .await
    }
    pub async fn delete_secret(&self, key: &str) -> ApiResult {
        self.delete(&format!("secrets/{key}")).await
    }
    pub async fn logs(&self, query: &[(&str, String)]) -> ApiResult {
        self.get_q("logs", query).await
    }
    pub async fn costs(&self, limit: Option<usize>, source: Option<&str>) -> ApiResult {
        self.get_q(
            "costs",
            &[
                ("limit", limit.map(|l| l.to_string()).unwrap_or_default()),
                ("source", source.unwrap_or_default().to_string()),
            ],
        )
        .await
    }
    pub async fn costs_summary(&self, source: Option<&str>) -> ApiResult {
        self.get_q(
            "costs/summary",
            &[("source", source.unwrap_or_default().to_string())],
        )
        .await
    }
    pub async fn diagnostics(&self) -> ApiResult {
        self.get("diagnostics").await
    }

    // ----- threads -----

    pub async fn threads(&self) -> ApiResult {
        self.get("threads").await
    }
    pub async fn create_thread(&self, title: Option<&str>) -> ApiResult {
        self.post("threads", title.map(|t| json!({ "title": t })))
            .await
    }
    pub async fn thread(&self, id: &str) -> ApiResult {
        self.get(&format!("threads/{id}")).await
    }
    pub async fn rename_thread(&self, id: &str, title: &str) -> ApiResult {
        self.patch(&format!("threads/{id}"), json!({ "title": title }))
            .await
    }
    pub async fn delete_thread(&self, id: &str, purge_transcript: bool) -> ApiResult {
        self.delete_q(
            &format!("threads/{id}"),
            &[(
                "purge",
                if purge_transcript {
                    "true".into()
                } else {
                    String::new()
                },
            )],
        )
        .await
    }
    pub async fn turns(&self, id: &str) -> ApiResult {
        self.get(&format!("threads/{id}/turns")).await
    }
    /// Caminhos de anexos são do **servidor** (em loopback, locais).
    pub async fn send_message(&self, id: &str, text: &str, attachments: Vec<String>) -> ApiResult {
        self.post(
            &format!("threads/{id}/messages"),
            Some(json!({ "text": text, "attachments": attachments })),
        )
        .await
    }
    pub async fn restart_thread(&self, id: &str) -> ApiResult {
        self.post(&format!("threads/{id}/restart"), None).await
    }
    pub async fn reset_thread(&self, id: &str) -> ApiResult {
        self.post(&format!("threads/{id}/reset"), None).await
    }
    pub async fn interrupt_thread(&self, id: &str) -> ApiResult {
        self.post(&format!("threads/{id}/interrupt"), None).await
    }
    pub async fn turn_events(&self, turn_id: &str) -> ApiResult {
        self.get(&format!("turns/{turn_id}/events")).await
    }

    // ----- motor -----

    pub async fn engine_tokens(&self) -> ApiResult {
        self.get("engine/tokens").await
    }
    /// `activate`: testa e ativa logo após cadastrar.
    pub async fn add_engine_token(&self, label: &str, token: &str, activate: bool) -> ApiResult {
        self.post(
            "engine/tokens",
            Some(json!({ "label": label, "token": token, "activate": activate })),
        )
        .await
    }
    pub async fn test_engine_token(&self, id: &str) -> ApiResult {
        self.post(&format!("engine/tokens/{id}/test"), None).await
    }
    pub async fn activate_engine_token(&self, id: &str) -> ApiResult {
        self.post(&format!("engine/tokens/{id}/activate"), None)
            .await
    }
    pub async fn delete_engine_token(&self, id: &str) -> ApiResult {
        self.delete(&format!("engine/tokens/{id}")).await
    }
    pub async fn engine_check(&self) -> ApiResult {
        self.get("engine/check").await
    }

    // ----- base de conhecimento -----

    pub async fn kb_status(&self) -> ApiResult {
        self.get("kb/status").await
    }
    pub async fn kb_scaffold(&self) -> ApiResult {
        self.post("kb/scaffold", None).await
    }
    pub async fn kb_open(&self) -> ApiResult {
        self.post("kb/open", None).await
    }
    pub async fn kb_mcp_json(&self) -> ApiResult {
        self.get("kb/mcp-json").await
    }
    /// Subpastas de um diretório do servidor (`None` = home). Para escolher a KB remotamente.
    pub async fn kb_browse(&self, path: Option<&str>) -> ApiResult {
        self.get_q(
            "kb/browse",
            &[("path", path.unwrap_or_default().to_string())],
        )
        .await
    }

    // ----- arquivos -----

    /// Envia bytes para `<kb>/inbox/ui/` e devolve o anexo (`{name, mime, path, size}`).
    pub async fn upload(&self, name: &str, bytes: Vec<u8>) -> ApiResult {
        let url = format!(
            "{}{}",
            self.url("uploads"),
            query_string(&[("name", name.to_string())])
        );
        Self::finish(
            self.http
                .post(url)
                .bearer_auth(&self.token)
                .header("content-type", "application/octet-stream")
                .body(bytes)
                .send()
                .await,
        )
        .await
    }

    /// Conteúdo de um arquivo da KB do servidor (miniaturas, abrir/baixar).
    pub async fn file_bytes(&self, path: &str) -> Result<(String, Vec<u8>), String> {
        let url = format!(
            "{}{}",
            self.url("files"),
            query_string(&[("path", path.to_string())])
        );
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        let status = resp.status();
        let mime = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes).to_string();
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(msg);
        }
        Ok((mime, bytes.to_vec()))
    }

    // ----- MCP / aprovações -----

    pub async fn mcp_script_token(&self) -> ApiResult {
        self.get("mcp/script-token").await
    }
    pub async fn mcp_rotate_script_token(&self) -> ApiResult {
        self.post("mcp/script-token/rotate", None).await
    }
    pub async fn mcp_tools(&self) -> ApiResult {
        self.get("mcp/tools").await
    }
    pub async fn mcp_set_policy(&self, tool: &str, policy: &str) -> ApiResult {
        self.put(
            &format!("mcp/tools/{tool}/policy"),
            json!({ "policy": policy }),
        )
        .await
    }
    pub async fn mcp_calls(&self, query: &[(&str, String)]) -> ApiResult {
        self.get_q("mcp/calls", query).await
    }
    pub async fn approvals(&self) -> ApiResult {
        self.get("approvals").await
    }
    pub async fn approvals_history(&self, query: &[(&str, String)]) -> ApiResult {
        self.get_q("approvals/history", query).await
    }
    pub async fn decide_approval(&self, id: &str, body: Value) -> ApiResult {
        self.post(&format!("approvals/{id}"), Some(body)).await
    }
    pub async fn external_mcps(&self) -> ApiResult {
        self.get("mcp/external").await
    }
    pub async fn external_import_claude(&self) -> ApiResult {
        self.post("mcp/external/import-claude", None).await
    }
    pub async fn external_import_json(&self, body: Value) -> ApiResult {
        self.post("mcp/external/import-json", Some(body)).await
    }
    pub async fn external_put(&self, name: &str, body: Value) -> ApiResult {
        self.put(&format!("mcp/external/{name}"), body).await
    }
    pub async fn external_delete(&self, name: &str) -> ApiResult {
        self.delete(&format!("mcp/external/{name}")).await
    }
    pub async fn external_test(&self, name: &str) -> ApiResult {
        self.post(&format!("mcp/external/{name}/test"), None).await
    }

    // ----- agendamentos -----

    pub async fn schedules(&self, thread_id: Option<&str>) -> ApiResult {
        self.get_q(
            "schedules",
            &[("thread_id", thread_id.unwrap_or_default().to_string())],
        )
        .await
    }
    pub async fn create_schedule(&self, body: Value) -> ApiResult {
        self.post("schedules", Some(body)).await
    }
    pub async fn schedule_preview(
        &self,
        cron: &str,
        timezone: Option<&str>,
        n: usize,
    ) -> ApiResult {
        self.get_q(
            "schedules/preview",
            &[
                ("cron", cron.to_string()),
                ("timezone", timezone.unwrap_or_default().to_string()),
                ("n", n.to_string()),
            ],
        )
        .await
    }
    pub async fn schedule(&self, id: &str) -> ApiResult {
        self.get(&format!("schedules/{id}")).await
    }
    pub async fn update_schedule(&self, id: &str, body: Value) -> ApiResult {
        self.put(&format!("schedules/{id}"), body).await
    }
    pub async fn delete_schedule(&self, id: &str) -> ApiResult {
        self.delete(&format!("schedules/{id}")).await
    }
    pub async fn run_schedule(&self, id: &str) -> ApiResult {
        self.post(&format!("schedules/{id}/run"), None).await
    }

    // ----- Browser CDP -----

    pub async fn browser_status(&self) -> ApiResult {
        self.get("browser/status").await
    }
    pub async fn browser_detect(&self) -> ApiResult {
        self.get("browser/detect").await
    }
    pub async fn browser_traffic(&self, after: u64, limit: usize) -> ApiResult {
        self.get_q(
            "browser/traffic",
            &[("after", after.to_string()), ("limit", limit.to_string())],
        )
        .await
    }
    pub async fn browser_traffic_clear(&self) -> ApiResult {
        self.post("browser/traffic/clear", None).await
    }
    pub async fn browser_launch(&self) -> ApiResult {
        self.post("browser/launch", None).await
    }
    pub async fn browser_close(&self) -> ApiResult {
        self.post("browser/close", None).await
    }
    pub async fn browser_test_upstream(&self) -> ApiResult {
        self.post("browser/test/upstream", None).await
    }
    pub async fn browser_test_new_tab(&self, url: Option<&str>) -> ApiResult {
        self.post("browser/test/new-tab", url.map(|u| json!({ "url": u })))
            .await
    }
    pub async fn browser_test_agent_browser(&self) -> ApiResult {
        self.post("browser/test/agent-browser", None).await
    }
    pub async fn browser_install_status(&self) -> ApiResult {
        self.get("browser/install/status").await
    }
    pub async fn browser_install_job(&self) -> ApiResult {
        self.get("browser/install/job").await
    }
    pub async fn browser_install_agent_browser(&self) -> ApiResult {
        self.post("browser/install/agent-browser", None).await
    }
    pub async fn browser_install_chromium(&self) -> ApiResult {
        self.post("browser/install/chromium", None).await
    }
    pub async fn browser_install_skill(&self) -> ApiResult {
        self.post("browser/install/skill", None).await
    }

    // ----- integrações -----

    pub async fn telegram_test(&self) -> ApiResult {
        self.post("telegram/test", None).await
    }
    pub async fn telegram_status(&self) -> ApiResult {
        self.get("telegram/status").await
    }
    /// `GET /api/telemetry/status`: contadores, fila, último envio/erro e
    /// `instance_id` da telemetria (Axiom).
    pub async fn telemetry_status(&self) -> ApiResult {
        self.get("telemetry/status").await
    }
    /// `POST /api/telemetry/test`: manda um evento de teste com o token/dataset salvos.
    pub async fn telemetry_test(&self) -> ApiResult {
        self.post("telemetry/test", None).await
    }
    /// `GET /api/telemetry/client-config`: destino OTLP para o app enviar os próprios logs (admin).
    pub async fn telemetry_client_config(&self) -> ApiResult {
        self.get("telemetry/client-config").await
    }
    pub async fn voice_start(&self) -> ApiResult {
        self.post("voice/start", None).await
    }
    pub async fn voice_stop(&self) -> ApiResult {
        self.post("voice/stop", None).await
    }
    pub async fn voice_toggle(&self) -> ApiResult {
        self.post("voice/toggle", None).await
    }
    pub async fn voice_status(&self) -> ApiResult {
        self.get("voice/status").await
    }
    /// Ditado: WAV PCM16 mono 16 kHz → `{text, secs, usd}` (Whisper no servidor).
    pub async fn transcribe(&self, wav: Vec<u8>, lang: Option<&str>) -> ApiResult {
        let url = format!(
            "{}{}",
            self.url("voice/transcribe"),
            query_string(&[("lang", lang.unwrap_or_default().to_string())])
        );
        Self::finish(
            self.http
                .post(url)
                .bearer_auth(&self.token)
                .header("content-type", "audio/wav")
                .body(wav)
                .send()
                .await,
        )
        .await
    }
    /// Resposta (Markdown) → roteiro falável `{ summary, usd }` (OpenAI no servidor).
    pub async fn voice_summary(&self, text: &str) -> ApiResult {
        self.post("voice/summary", Some(json!({ "text": text })))
            .await
    }
    /// Texto → PCM16 LE mono (taxa no header `X-Sample-Rate`, 24 kHz) sintetizado
    /// no servidor com a voz do módulo. Devolve `(taxa, pcm)`.
    pub async fn voice_tts(&self, text: &str) -> Result<(u32, Vec<u8>), String> {
        let resp = self
            .http
            .post(self.url("voice/tts"))
            .bearer_auth(&self.token)
            .json(&json!({ "text": text }))
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        let status = resp.status();
        let rate = resp
            .headers()
            .get("x-sample-rate")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(24_000);
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes).to_string();
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(msg);
        }
        Ok((rate, bytes.to_vec()))
    }
    pub async fn voice_transcripts(&self, query: &[(&str, String)]) -> ApiResult {
        self.get_q("voice/transcripts", query).await
    }
    pub async fn wa_status(&self) -> ApiResult {
        self.get("whatsapp/status").await
    }
    pub async fn wa_pair(&self, body: Option<Value>) -> ApiResult {
        self.post("whatsapp/pair", body).await
    }
    pub async fn wa_unlink(&self, body: Option<Value>) -> ApiResult {
        self.post("whatsapp/unlink", body).await
    }
    pub async fn wa_reconnect(&self, body: Option<Value>) -> ApiResult {
        self.post("whatsapp/reconnect", body).await
    }
    pub async fn wa_account_patch(&self, account_id: &str, body: Value) -> ApiResult {
        self.patch(&format!("whatsapp/accounts/{account_id}"), body)
            .await
    }
    pub async fn google_accounts(&self) -> ApiResult {
        self.get("google/accounts").await
    }
    pub async fn google_connect(&self, body: Option<Value>) -> ApiResult {
        self.post("google/connect", body).await
    }
    pub async fn google_callback(&self, state: &str, code: &str) -> ApiResult {
        self.post(
            "google/callback",
            Some(json!({ "state": state, "code": code })),
        )
        .await
    }
    pub async fn google_reauthorize(&self, account: &str, body: Option<Value>) -> ApiResult {
        self.post(&format!("google/accounts/{account}/reauthorize"), body)
            .await
    }
    pub async fn google_disconnect(&self, account: &str) -> ApiResult {
        self.delete(&format!("google/accounts/{account}")).await
    }
    pub async fn google_calendars(&self, query: &[(&str, String)]) -> ApiResult {
        self.get_q("google/calendars", query).await
    }
    pub async fn google_calendars_refresh(&self) -> ApiResult {
        self.post("google/calendars/refresh", None).await
    }
    pub async fn google_calendar_flags(
        &self,
        account: &str,
        calendar_id: &str,
        body: Value,
    ) -> ApiResult {
        self.put(&format!("google/calendars/{account}/{calendar_id}"), body)
            .await
    }

    // ----- eventos -----

    /// Mantém o WS `/api/events` aberto, reconectando com recuo (1 s → 10 s).
    /// Cada evento vai para `sink`; `wake` é chamado depois de cada entrega
    /// (o app usa para `request_repaint`). Termina quando `sink` falha.
    pub fn spawn_events(
        self: &Arc<Self>,
        rt: &tokio::runtime::Handle,
        sink: std::sync::mpsc::Sender<ClientEvent>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        rt.spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let url = format!("{}/api/events?token={}", me.ws_base(), me.token);
                match me.connect_ws(&url).await {
                    Ok(mut ws) => {
                        backoff = Duration::from_secs(1);
                        if sink.send(ClientEvent::Connected).is_err() {
                            return;
                        }
                        wake();
                        let reason = loop {
                            match ws.next().await {
                                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) => {
                                    match serde_json::from_str::<Event>(t.as_str()) {
                                        Ok(ev) => {
                                            if sink.send(ClientEvent::Event(ev)).is_err() {
                                                return;
                                            }
                                            wake();
                                        }
                                        Err(e) => tracing::debug!(error = %e, "evento desconhecido ignorado"),
                                    }
                                }
                                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => {
                                    break "conexão fechada".to_string()
                                }
                                Some(Ok(_)) => {}
                                Some(Err(e)) => break e.to_string(),
                            }
                        };
                        if sink.send(ClientEvent::Disconnected(reason)).is_err() {
                            return;
                        }
                        wake();
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "eventos: sem conexão; tentando de novo");
                        if sink.send(ClientEvent::Disconnected(e.to_string())).is_err() {
                            return;
                        }
                        wake();
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        })
    }
}

/// `?a=1&b=x%20y` (vazio quando não há valores); codifica tudo fora de
/// `A-Za-z0-9-_.~`.
pub fn query_string(query: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (k, v) in query.iter().filter(|(_, v)| !v.is_empty()) {
        out.push(if out.is_empty() { '?' } else { '&' });
        out.push_str(&percent_encode(k));
        out.push('=');
        out.push_str(&percent_encode(v));
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn transport_error(e: &reqwest::Error) -> String {
    if e.is_connect() {
        "sem conexão com o servidor".to_string()
    } else if e.is_timeout() {
        "tempo esgotado".to_string()
    } else {
        e.to_string()
    }
}
