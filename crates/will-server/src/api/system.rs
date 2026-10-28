use super::{bad, internal, ApiError, ApiResult, App};
use axum::extract::{Path, Query, State};
use axum::Json;
use secrecy::SecretString;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use will_core::Module;

pub async fn health(State(state): State<App>) -> ApiResult<Value> {
    Ok(Json(
        json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "uptime_secs": state.started_at.elapsed().as_secs() }),
    ))
}

pub async fn status(State(state): State<App>) -> ApiResult<Value> {
    let modules: BTreeMap<String, Value> = Module::all()
        .iter()
        .map(|m| {
            (
                m.as_str().to_string(),
                serde_json::to_value(
                    state
                        .module_status
                        .read()
                        .get(m)
                        .cloned()
                        .unwrap_or(will_core::ModuleStatus::Stopped),
                )
                .unwrap_or_default(),
            )
        })
        .collect();
    let kb = crate::kb::status(state.kb_path().as_deref()).await;
    let stats = state.engine.stats();
    let threads = state.storage.list_threads(false).await.unwrap_or_default();
    let settings = state.settings();
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.started_at.elapsed().as_secs(),
        "modules": modules,
        "kb": kb,
        "engine": {
            "live_processes": stats.iter().filter(|s| s.alive).count(),
            "max_live_processes": settings.engine.max_live_processes,
            "threads": threads.len(),
            "running_turns": stats.iter().map(|s| s.pending).sum::<usize>(),
            "active_token": settings.engine.tokens.iter().find(|t| t.active).map(|t| t.label.clone()),
        },
        "counts": { "threads": threads.len(), "pending_approvals": state.approvals.pending().len() },
        "permission_mode": settings.engine.permission_mode,
        "mcp_url": *state.mcp_url.read(),
        "secrets_backend": state.secrets.backend(),
        "data_dir": state.paths.data_dir,
        "logs_dir": state.paths.logs_dir,
        "portable_root": state.paths.portable_root,
        "db_path": state.paths.db_path(),
        "onboarding_done": settings.general.onboarding_done,
    })))
}

#[derive(serde::Deserialize, Default)]
pub struct CostsQuery {
    pub limit: Option<i64>,
    pub source: Option<String>,
}

pub async fn costs(State(state): State<App>, Query(q): Query<CostsQuery>) -> ApiResult<Value> {
    let rows = state
        .storage
        .list_ai_costs(
            q.limit.unwrap_or(200).min(2000),
            q.source.as_deref().filter(|s| !s.is_empty()),
        )
        .await?;
    Ok(Json(json!(rows)))
}

pub async fn costs_summary(
    State(state): State<App>,
    Query(q): Query<CostsQuery>,
) -> ApiResult<Value> {
    Ok(Json(json!(
        state
            .storage
            .ai_cost_summary(q.source.as_deref().filter(|s| !s.is_empty()))
            .await?
    )))
}

pub const SECRET_KEYS: &[&str] = &[
    "openai_api_key",
    "telegram_bot_token",
    "google_client_secret",
];

async fn secrets_present(state: &App) -> BTreeMap<String, bool> {
    let mut m = BTreeMap::new();
    for k in SECRET_KEYS {
        m.insert(
            k.to_string(),
            state.secrets.get(k).await.ok().flatten().is_some(),
        );
    }
    m
}

pub async fn get_settings(State(state): State<App>) -> ApiResult<Value> {
    let s = state.settings();
    let mut v = serde_json::to_value(&s).map_err(|e| internal(e.to_string()))?;
    v["secrets_present"] = json!(secrets_present(&state).await);
    Ok(Json(v))
}

#[derive(serde::Deserialize)]
pub struct PutSettings {
    pub section: String,
    pub value: Value,
}

pub async fn put_settings(
    State(state): State<App>,
    Json(body): Json<PutSettings>,
) -> ApiResult<Value> {
    let current = state.settings();
    let mut v = serde_json::to_value(&current).map_err(|e| internal(e.to_string()))?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| internal("settings inválidas"))?;
    if !obj.contains_key(&body.section) {
        return Err(bad(format!("seção desconhecida: {}", body.section)));
    }
    // Preserva tokens do motor se o cliente não os enviar (evita apagar por engano).
    let mut value = body.value.clone();
    if body.section == "engine" {
        if let Some(o) = value.as_object_mut() {
            if !o.contains_key("tokens") {
                o.insert("tokens".into(), obj["engine"]["tokens"].clone());
            }
        }
    }
    obj.insert(body.section.clone(), value);
    let new: will_core::Settings =
        serde_json::from_value(v).map_err(|e| bad(format!("valor inválido: {e}")))?;
    if body.section == "kb" {
        if let Some(p) = &new.kb.path {
            let pb = state.paths.resolve_user_path(p);
            if !pb.is_dir() {
                return Err(bad("a pasta informada não existe"));
            }
            if will_platform::is_dangerous_kb(&pb) {
                return Err(bad(
                    "pasta perigosa (home ou raiz do disco); escolha uma subpasta dedicada",
                ));
            }
        }
    }
    *state.settings.write() = new;
    state.save_settings().await.map_err(internal)?;
    crate::modules::apply_section(&state, &body.section).await;
    get_settings(State(state)).await
}

#[derive(serde::Deserialize)]
pub struct SetSecret {
    pub key: String,
    pub value: String,
}

fn valid_secret_key(k: &str) -> bool {
    SECRET_KEYS.contains(&k)
        || k.starts_with("claude_token:")
        || k.starts_with("mcp:")
        || k.starts_with("google:")
}

pub async fn set_secret(State(state): State<App>, Json(body): Json<SetSecret>) -> ApiResult<Value> {
    if !valid_secret_key(&body.key) {
        return Err(bad("chave de segredo não permitida"));
    }
    if body.value.trim().is_empty() {
        return Err(bad("valor vazio"));
    }
    state
        .secrets
        .set(
            &body.key,
            &SecretString::from(body.value.trim().to_string()),
        )
        .await
        .map_err(internal)?;
    // efeitos colaterais
    match body.key.as_str() {
        "openai_api_key" => crate::modules::apply_section(&state, "realtime").await,
        "telegram_bot_token" => crate::modules::apply_section(&state, "telegram").await,
        "google_client_secret" => crate::modules::apply_section(&state, "google").await,
        _ => {}
    }
    Ok(Json(
        json!({ "ok": true, "secrets_present": secrets_present(&state).await }),
    ))
}

pub async fn delete_secret(State(state): State<App>, Path(key): Path<String>) -> ApiResult<Value> {
    if !valid_secret_key(&key) {
        return Err(bad("chave de segredo não permitida"));
    }
    state.secrets.delete(&key).await.map_err(internal)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
pub struct LogsQuery {
    pub limit: Option<i64>,
    pub module: Option<String>,
    pub level: Option<String>,
}

pub async fn logs(State(state): State<App>, Query(q): Query<LogsQuery>) -> ApiResult<Value> {
    let rows = state
        .storage
        .list_logs(
            q.limit.unwrap_or(200).min(2000),
            q.module.as_deref().filter(|s| !s.is_empty()),
            q.level.as_deref().filter(|s| !s.is_empty()),
        )
        .await?;
    Ok(Json(json!(rows)))
}

pub async fn diagnostics(State(state): State<App>) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let s = state.settings();
    let sanitized = serde_json::to_string_pretty(&s).unwrap_or_default();
    let logs = state
        .storage
        .list_logs(500, None, None)
        .await
        .unwrap_or_default();
    let mut text = String::new();
    text.push_str(&format!(
        "will-ia {}\n{}\n\n== settings ==\n{}\n\n== logs ==\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        crate::logging::redact(&sanitized)
    ));
    for l in logs.iter().rev() {
        text.push_str(&format!(
            "{} [{}] {}: {}\n",
            l.at,
            l.level,
            l.module,
            crate::logging::redact(&l.message)
        ));
    }
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"will-ia-diagnostico.txt\"",
            ),
        ],
        text,
    )
        .into_response())
}

/// Pede à instância com GUI para mostrar a janela principal (segunda execução do binário).
pub async fn ui_show(State(state): State<App>) -> ApiResult<Value> {
    ui_cmd(state, "show")
}

/// Captura da janela em `<data_dir>/screenshot.png` (diagnóstico).
#[derive(serde::Deserialize, Default)]
pub struct ScreenshotQuery {
    pub nav: Option<String>,
}

pub async fn ui_screenshot(
    State(state): State<App>,
    Query(q): Query<ScreenshotQuery>,
) -> ApiResult<Value> {
    let cmd = match q.nav {
        Some(n) if !n.is_empty() => format!("screenshot:{n}"),
        _ => "screenshot".to_string(),
    };
    ui_cmd(state, &cmd)
}

fn ui_cmd(state: App, cmd: &str) -> ApiResult<Value> {
    let hook = state.ui_hook.read().clone();
    match hook {
        Some(h) => {
            h(cmd);
            Ok(Json(json!({ "ok": true })))
        }
        None => Ok(Json(
            json!({ "ok": false, "error": "sem GUI nesta instância (modo headless)" }),
        )),
    }
}
