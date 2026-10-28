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

#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
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
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http,
        }
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
    pub async fn google_reauthorize(&self, account: &str) -> ApiResult {
        self.post(&format!("google/accounts/{account}/reauthorize"), None)
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
                match tokio_tungstenite::connect_async(&url).await {
                    Ok((mut ws, _)) => {
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
