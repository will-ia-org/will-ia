use super::{bad, internal, ApiResult, App};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

// ---------- Telegram ----------

#[derive(serde::Deserialize, Default)]
pub struct TgTest {
    pub token: Option<String>,
}

pub async fn telegram_test(
    State(state): State<App>,
    body: Option<Json<TgTest>>,
) -> ApiResult<Value> {
    #[cfg(feature = "telegram")]
    {
        use secrecy::ExposeSecret;
        let token = match body
            .and_then(|b| b.0.token)
            .filter(|t| !t.trim().is_empty())
        {
            Some(t) => t,
            None => state
                .secrets
                .get("telegram_bot_token")
                .await
                .ok()
                .flatten()
                .map(|s| s.expose_secret().to_string())
                .ok_or_else(|| bad("token do bot não configurado"))?,
        };
        match will_telegram::test_bot_token(token.trim()).await {
            Ok(v) => Ok(Json(
                json!({ "ok": true, "username": v.get("username"), "id": v.get("id"), "first_name": v.get("first_name") }),
            )),
            Err(e) => Ok(Json(json!({ "ok": false, "error": e }))),
        }
    }
    #[cfg(not(feature = "telegram"))]
    {
        let _ = (state, body);
        Err(bad("módulo Telegram não compilado"))
    }
}

pub async fn telegram_status(State(state): State<App>) -> ApiResult<Value> {
    #[cfg(feature = "telegram")]
    {
        let m = state.telegram.read().await.clone();
        match m {
            Some(m) => Ok(Json(
                json!({ "status": m.status(), "threads": m.thread_status_rows().await }),
            )),
            None => Ok(Json(
                json!({ "status": state.module_status.read().get(&will_core::Module::Telegram), "threads": [] }),
            )),
        }
    }
    #[cfg(not(feature = "telegram"))]
    {
        let _ = state;
        Ok(Json(json!({ "threads": [] })))
    }
}

// ---------- Voz ----------

/// Sessão de voz do usuário que chamou (uma por usuário, `docs/26` etapa 6).
fn rt(state: &App, me: &will_core::Principal) -> std::sync::Arc<will_realtime::RealtimeManager> {
    crate::modules::voice_manager_for(state, &me.username)
}

/// Respeita `max_sessions` contando as sessões ativas de **outros** usuários.
fn check_capacity(state: &App, me: &will_core::Principal) -> Result<(), super::ApiError> {
    let max = state.settings.read().realtime.max_sessions.max(1) as usize;
    let mine_active = state
        .voice
        .read()
        .get(&me.username)
        .map(|m| m.is_active())
        .unwrap_or(false);
    let active = crate::modules::active_voice_sessions(state);
    if !mine_active && active >= max {
        return Err(bad(format!(
            "limite de {max} sessão(ões) de voz simultânea(s) atingido; tente mais tarde"
        )));
    }
    Ok(())
}

pub async fn voice_start(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
) -> ApiResult<Value> {
    check_capacity(&state, &me)?;
    let r = rt(&state, &me);
    r.start().await.map_err(bad)?;
    crate::modules::recompute_realtime_status(&state);
    Ok(Json(
        json!({ "ok": true, "active": true, "user": me.username }),
    ))
}

pub async fn voice_stop(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
) -> ApiResult<Value> {
    let r = rt(&state, &me);
    r.stop().await;
    crate::modules::recompute_realtime_status(&state);
    Ok(Json(
        json!({ "ok": true, "active": false, "user": me.username }),
    ))
}

pub async fn voice_toggle(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
) -> ApiResult<Value> {
    let r = rt(&state, &me);
    if !r.is_active() {
        check_capacity(&state, &me)?;
    }
    let active = r.toggle().await.map_err(bad)?;
    crate::modules::recompute_realtime_status(&state);
    Ok(Json(
        json!({ "ok": true, "active": active, "user": me.username }),
    ))
}

/// Status da **minha** sessão + resumo das outras (quem está falando).
pub async fn voice_status(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
) -> ApiResult<Value> {
    let settings = state.settings().realtime;
    let mut v = match state.voice.read().get(&me.username) {
        Some(r) => r.status(),
        None => json!({
            "active": false,
            "state": "stopped",
            "enabled": settings.enabled,
            "has_api_key": state.voice_key.read().is_some(),
            "model": settings.model,
            "activation": settings.activation,
        }),
    };
    let sessions: Vec<Value> = state
        .voice
        .read()
        .iter()
        .map(|(u, m)| {
            let st = m.status();
            json!({ "user": u, "active": m.is_active(), "state": st["state"] })
        })
        .collect();
    if let Some(o) = v.as_object_mut() {
        o.insert("user".into(), json!(me.username));
        o.insert("sessions".into(), json!(sessions));
        o.insert("max_sessions".into(), json!(settings.max_sessions));
        o.insert(
            "active_sessions".into(),
            json!(crate::modules::active_voice_sessions(&state)),
        );
    }
    Ok(Json(v))
}

#[derive(serde::Deserialize)]
pub struct Limit {
    pub limit: Option<i64>,
}

#[derive(serde::Deserialize, Default)]
pub struct TranscribeQuery {
    /// Idioma ISO-639-1 (ex.: `pt`); vazio = detecção automática.
    pub lang: Option<String>,
}

/// `POST /api/voice/transcribe` com o corpo em WAV (PCM16 mono): ditado do
/// campo de mensagem. Usa a chave da OpenAI do módulo de voz (Whisper) e
/// devolve `{text}`; registra custo estimado (US$ 0,006/min) em `ai_costs`.
pub async fn voice_transcribe(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
    Query(q): Query<TranscribeQuery>,
    body: axum::body::Bytes,
) -> ApiResult<Value> {
    if !state.settings().realtime.enabled {
        return Err(bad("módulo de voz desligado"));
    }
    let key = crate::modules::openai_key(&state)
        .await
        .ok_or_else(|| bad("sem chave da OpenAI (Configurações → Voz)"))?;
    if body.len() < 2_000 {
        return Err(bad("áudio curto demais"));
    }
    let secs = (body.len().saturating_sub(44)) as f64 / 32_000.0; // 16 kHz · 16 bits · mono
    let mut form = reqwest::multipart::Form::new()
        .text("model", "whisper-1")
        .text("response_format", "json")
        .part(
            "file",
            reqwest::multipart::Part::bytes(body.to_vec())
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|e| super::internal(e.to_string()))?,
        );
    if let Some(l) = q.lang.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        form = form.text("language", l.to_string());
    }
    use secrecy::ExposeSecret;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| super::internal(e.to_string()))?;
    let resp = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .bearer_auth(key.expose_secret())
        .multipart(form)
        .send()
        .await
        .map_err(|e| super::internal(format!("OpenAI: {e}")))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| super::internal(format!("OpenAI: resposta inválida: {e}")))?;
    if !status.is_success() {
        let msg = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("falha na transcrição")
            .to_string();
        return Err(bad(format!("OpenAI {status}: {msg}")));
    }
    let text = v
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let usd = secs / 60.0 * 0.006;
    let rec = will_storage::costs::NewAiCost {
        source: "voice_stt".into(),
        r#ref: Some(format!("user:{}", me.username)),
        model: Some("whisper-1".into()),
        units: json!({ "audio_secs": secs.round() as i64 }),
        usd,
        estimated: true,
        duration_secs: Some(secs.round() as i64),
        note: Some(format!("ditado de {}", me.username)),
    };
    let _ = state.storage.add_ai_cost(&rec).await;
    Ok(Json(json!({ "text": text, "secs": secs, "usd": usd })))
}

pub async fn voice_transcripts(
    State(state): State<App>,
    Query(q): Query<Limit>,
) -> ApiResult<Value> {
    Ok(Json(json!(
        state
            .storage
            .list_voice_transcripts(q.limit.unwrap_or(100).min(1000))
            .await?
    )))
}

// ---------- WhatsApp ----------

pub async fn wa_status(State(state): State<App>) -> ApiResult<Value> {
    #[cfg(feature = "whatsapp")]
    {
        let m = state.whatsapp.read().await.clone();
        match m {
            Some(m) => Ok(Json(m.status().await)),
            None => Ok(Json(
                json!({ "enabled": false, "accounts": [], "state": "disabled" }),
            )),
        }
    }
    #[cfg(not(feature = "whatsapp"))]
    {
        let _ = state;
        Ok(Json(
            json!({ "state": "unavailable", "detail": "módulo WhatsApp não compilado" }),
        ))
    }
}

#[cfg(feature = "whatsapp")]
async fn wa(state: &App) -> Result<std::sync::Arc<will_whatsapp::WhatsAppModule>, super::ApiError> {
    state
        .whatsapp
        .read()
        .await
        .clone()
        .ok_or_else(|| bad("módulo WhatsApp não inicializado"))
}

#[derive(serde::Deserialize, Default)]
pub struct PairBody {
    /// Conta existente a (re)parear; ausente → cria uma conta nova.
    pub account_id: Option<String>,
    /// Pareamento por código (telefone com DDI); ausente → QR.
    pub phone: Option<String>,
    /// Rótulo da conta nova (padrão "Conta N", trocado pelo nome do perfil ao conectar).
    pub label: Option<String>,
}

pub async fn wa_pair(State(state): State<App>, body: Option<Json<PairBody>>) -> ApiResult<Value> {
    #[cfg(feature = "whatsapp")]
    {
        let s = state.settings().whatsapp;
        let m = wa(&state).await?;
        if !s.enabled {
            return Err(bad("habilite o módulo WhatsApp e salve antes de parear"));
        }
        let b = body.map(|b| b.0).unwrap_or_default();
        let phone = b
            .phone
            .filter(|p| !p.trim().is_empty())
            .map(|p| p.chars().filter(|c| c.is_ascii_digit()).collect::<String>());
        let account_id = m.pair(b.account_id, phone, b.label).await.map_err(bad)?;
        crate::modules::sync_whatsapp_accounts(&state, &m).await;
        state.set_module_status(will_core::Module::WhatsApp, m.module_status());
        Ok(Json(json!({ "ok": true, "account_id": account_id })))
    }
    #[cfg(not(feature = "whatsapp"))]
    {
        let _ = (state, body);
        Err(bad("módulo WhatsApp não compilado"))
    }
}

#[derive(serde::Deserialize)]
pub struct UnlinkBody {
    pub account_id: String,
    /// Apaga o índice local da conta.
    pub clear_index: Option<bool>,
    /// Remove a conta da lista (e das configurações).
    pub remove: Option<bool>,
}

pub async fn wa_unlink(State(state): State<App>, Json(body): Json<UnlinkBody>) -> ApiResult<Value> {
    #[cfg(feature = "whatsapp")]
    {
        let m = wa(&state).await?;
        m.unlink(
            &body.account_id,
            body.clear_index.unwrap_or(false),
            body.remove.unwrap_or(false),
        )
        .await
        .map_err(bad)?;
        crate::modules::sync_whatsapp_accounts(&state, &m).await;
        state.set_module_status(will_core::Module::WhatsApp, m.module_status());
        Ok(Json(json!({ "ok": true })))
    }
    #[cfg(not(feature = "whatsapp"))]
    {
        let _ = (state, body);
        Err(bad("módulo WhatsApp não compilado"))
    }
}

#[derive(serde::Deserialize)]
pub struct AccountIdBody {
    pub account_id: String,
}

pub async fn wa_reconnect(
    State(state): State<App>,
    Json(body): Json<AccountIdBody>,
) -> ApiResult<Value> {
    #[cfg(feature = "whatsapp")]
    {
        let m = wa(&state).await?;
        m.reconnect(&body.account_id).await.map_err(bad)?;
        Ok(Json(json!({ "ok": true })))
    }
    #[cfg(not(feature = "whatsapp"))]
    {
        let _ = (state, body);
        Err(bad("módulo WhatsApp não compilado"))
    }
}

#[derive(serde::Deserialize, Default)]
pub struct AccountPatch {
    pub label: Option<String>,
    pub enabled: Option<bool>,
}

/// `PATCH /api/whatsapp/accounts/{id}` — rótulo e/ou ativação de uma conta.
pub async fn wa_account_patch(
    State(state): State<App>,
    Path(id): Path<String>,
    body: Option<Json<AccountPatch>>,
) -> ApiResult<Value> {
    #[cfg(feature = "whatsapp")]
    {
        let m = wa(&state).await?;
        let b = body.map(|b| b.0).unwrap_or_default();
        m.set_account(&id, b.label, b.enabled).await.map_err(bad)?;
        crate::modules::sync_whatsapp_accounts(&state, &m).await;
        state.set_module_status(will_core::Module::WhatsApp, m.module_status());
        let account = m.account(&id).map(|a| a.to_ref());
        Ok(Json(json!({ "ok": true, "account": account })))
    }
    #[cfg(not(feature = "whatsapp"))]
    {
        let _ = (state, id, body);
        Err(bad("módulo WhatsApp não compilado"))
    }
}

// ---------- Google ----------

#[cfg(feature = "google")]
fn google(state: &App) -> Result<std::sync::Arc<will_google::GoogleModule>, super::ApiError> {
    state
        .google
        .read()
        .clone()
        .ok_or_else(|| bad("módulo Google não inicializado"))
}

pub async fn google_accounts(State(state): State<App>) -> ApiResult<Value> {
    #[cfg(feature = "google")]
    {
        let g = google(&state)?;
        let accounts = g.list_accounts().await.map_err(internal)?;
        let out: Vec<Value> = accounts.iter().map(|a| json!({"id": a.id, "email": a.email, "scopes": a.scopes, "status": a.status, "connected_at": a.connected_at, "last_used_at": a.last_used_at})).collect();
        Ok(Json(json!(out)))
    }
    #[cfg(not(feature = "google"))]
    {
        let _ = state;
        Ok(Json(json!([])))
    }
}

#[derive(serde::Deserialize, Default)]
pub struct ConnectBody {
    pub scopes: Option<Vec<String>>,
    pub open_browser: Option<bool>,
    /// Porta de loopback onde o cliente remoto escuta o callback. Quando
    /// presente, o servidor só devolve `auth_url` + `state` e espera o `code`
    /// em `POST /google/callback`.
    pub callback_port: Option<u16>,
}

#[derive(serde::Deserialize, Default)]
pub struct ReauthBody {
    pub open_browser: Option<bool>,
    pub callback_port: Option<u16>,
}

#[derive(serde::Deserialize)]
pub struct GoogleCallbackBody {
    pub state: String,
    pub code: String,
}

pub async fn google_connect(
    State(state): State<App>,
    body: Option<Json<ConnectBody>>,
) -> ApiResult<Value> {
    #[cfg(feature = "google")]
    {
        let g = google(&state)?;
        let b = body.map(|b| b.0).unwrap_or_default();
        let scopes = b
            .scopes
            .unwrap_or_else(|| vec!["calendar".into(), "gmail".into()]);
        let open = b.open_browser.unwrap_or(b.callback_port.is_none());
        let start = g
            .start_connect(scopes, open, b.callback_port)
            .await
            .map_err(bad)?;
        Ok(Json(
            json!({ "auth_url": start.auth_url, "state": start.state }),
        ))
    }
    #[cfg(not(feature = "google"))]
    {
        let _ = (state, body);
        Err(bad("módulo Google não compilado"))
    }
}

pub async fn google_reauthorize(
    State(state): State<App>,
    Path(id): Path<String>,
    body: Option<Json<ReauthBody>>,
) -> ApiResult<Value> {
    #[cfg(feature = "google")]
    {
        let g = google(&state)?;
        let b = body.map(|b| b.0).unwrap_or_default();
        let open = b.open_browser.unwrap_or(b.callback_port.is_none());
        let start = g
            .reauthorize(&id, open, b.callback_port)
            .await
            .map_err(bad)?;
        Ok(Json(
            json!({ "auth_url": start.auth_url, "state": start.state }),
        ))
    }
    #[cfg(not(feature = "google"))]
    {
        let _ = (state, id, body);
        Err(bad("módulo Google não compilado"))
    }
}

/// Conclui o fluxo OAuth iniciado por um cliente remoto: o app repassa o
/// `code` que recebeu no loopback dele e o servidor faz a troca pelo token.
pub async fn google_callback(
    State(state): State<App>,
    Json(b): Json<GoogleCallbackBody>,
) -> ApiResult<Value> {
    #[cfg(feature = "google")]
    {
        let g = google(&state)?;
        let email = g.complete_flow(&b.state, &b.code).await.map_err(bad)?;
        Ok(Json(json!({ "email": email })))
    }
    #[cfg(not(feature = "google"))]
    {
        let _ = (state, b);
        Err(bad("módulo Google não compilado"))
    }
}

pub async fn google_disconnect(
    State(state): State<App>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    #[cfg(feature = "google")]
    {
        let g = google(&state)?;
        g.disconnect(&id).await.map_err(bad)?;
        crate::modules::apply_section(&state, "google").await;
        Ok(Json(json!({ "ok": true })))
    }
    #[cfg(not(feature = "google"))]
    {
        let _ = (state, id);
        Err(bad("módulo Google não compilado"))
    }
}

pub async fn google_calendars(State(state): State<App>) -> ApiResult<Value> {
    let cals = state.storage.list_google_calendars(false).await?;
    let accounts = state
        .storage
        .list_google_accounts()
        .await
        .unwrap_or_default();
    let out: Vec<Value> = cals
        .iter()
        .map(|c| {
            let email = accounts.iter().find(|a| a.id == c.account_id).map(|a| a.email.clone());
            json!({"account": c.account_id, "account_id": c.account_id, "account_email": email, "calendar_id": c.calendar_id, "summary": c.summary, "timezone": c.timezone, "role": c.access_role, "access_role": c.access_role, "color": c.color, "enabled": c.enabled, "default_for_create": c.default_for_create})
        })
        .collect();
    Ok(Json(json!(out)))
}

pub async fn google_calendars_refresh(State(state): State<App>) -> ApiResult<Value> {
    #[cfg(feature = "google")]
    {
        let g = google(&state)?;
        g.refresh_calendars().await.map_err(bad)?;
        google_calendars(State(state)).await
    }
    #[cfg(not(feature = "google"))]
    {
        let _ = state;
        Err(bad("módulo Google não compilado"))
    }
}

#[derive(serde::Deserialize)]
pub struct CalFlags {
    pub enabled: bool,
    pub default_for_create: Option<bool>,
}

pub async fn google_calendar_flags(
    State(state): State<App>,
    Path((account, calendar_id)): Path<(String, String)>,
    Json(b): Json<CalFlags>,
) -> ApiResult<Value> {
    state
        .storage
        .set_google_calendar_flags(
            &account,
            &calendar_id,
            b.enabled,
            b.default_for_create.unwrap_or(false),
        )
        .await?;
    Ok(Json(json!({ "ok": true })))
}
