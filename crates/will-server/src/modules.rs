//! Ciclo de vida dos módulos e aplicação de mudanças de configuração.

use crate::app::AppState;
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;
use will_core::settings::{ClaudeTokenRef, ToolPolicySetting};
use will_core::{Module, ModuleStatus, ToolPolicy};
use will_mcp::McpServerDeps;

pub async fn start_all(state: &Arc<AppState>) {
    start_mcp(state).await;
    register_core_tools(state);
    refresh_engine(state, false).await;
    crate::orchestrator::retitle_placeholder_threads(state).await;
    let _ = crate::mcp_json::write(state).await;
    start_realtime(state).await;
    #[cfg(feature = "google")]
    start_google(state).await;
    #[cfg(feature = "telegram")]
    start_telegram(state).await;
    #[cfg(feature = "whatsapp")]
    start_whatsapp(state).await;
    let kb_ok = state.kb_path().map(|p| p.is_dir()).unwrap_or(false);
    let has_token = state.settings.read().engine.tokens.iter().any(|t| t.active);
    if !kb_ok || !has_token {
        state.set_module_status(
            Module::Engine,
            ModuleStatus::Degraded(if !kb_ok {
                "Pasta da base de conhecimento não configurada".into()
            } else {
                "Nenhum token do Claude ativo".into()
            }),
        );
    } else {
        state.set_module_status(Module::Engine, ModuleStatus::Stopped);
    }
}

pub async fn stop_all(state: &Arc<AppState>) {
    let rt = state.realtime.read().clone();
    if let Some(rt) = rt {
        rt.stop().await;
    }
    #[cfg(feature = "telegram")]
    if let Some(t) = state.telegram.read().await.clone() {
        t.stop().await;
    }
    #[cfg(feature = "whatsapp")]
    if let Some(w) = state.whatsapp.read().await.clone() {
        w.stop().await;
    }
    if let Some(m) = state.mcp.lock().await.take() {
        m.stop();
    }
}

fn policy_overrides(state: &AppState) -> std::collections::BTreeMap<String, ToolPolicy> {
    state
        .settings
        .read()
        .mcp
        .policies
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                match v {
                    ToolPolicySetting::Auto => ToolPolicy::Auto,
                    ToolPolicySetting::Confirm => ToolPolicy::Confirm,
                    ToolPolicySetting::Off => ToolPolicy::Off,
                },
            )
        })
        .collect()
}

pub async fn start_mcp(state: &Arc<AppState>) {
    let (enabled, port, timeout) = {
        let s = state.settings.read();
        (s.mcp.enabled, s.mcp.port, s.mcp.approval_timeout_secs)
    };
    let mut overrides = state.storage.tool_policies().await.unwrap_or_default();
    overrides.extend(policy_overrides(state));
    state.registry.set_overrides(overrides);
    if let Some(old) = state.mcp.lock().await.take() {
        old.stop();
    }
    if !enabled {
        *state.mcp_url.write() = String::new();
        state.set_module_status(Module::Mcp, ModuleStatus::Disabled);
        return;
    }
    let st = state.clone();
    let deps = McpServerDeps {
        registry: state.registry.clone(),
        tokens: state.tokens.clone(),
        approvals: state.approvals.clone(),
        bus: state.bus.clone(),
        approval_timeout: Duration::from_secs(timeout.max(10)),
        on_call: Arc::new(move |r| {
            let st = st.clone();
            tokio::spawn(async move {
                let _ = st
                    .storage
                    .log_mcp_call(
                        r.thread_id,
                        &r.tool,
                        &r.args_redacted,
                        r.ok,
                        r.error.as_deref(),
                        r.duration_ms,
                    )
                    .await;
            });
        }),
    };
    match will_mcp::McpServer::start(deps, port).await {
        Ok(srv) => {
            *state.mcp_url.write() = srv.url();
            *state.mcp.lock().await = Some(srv);
            if let Err(e) = ensure_script_token(state).await {
                tracing::warn!(error = %e, "token de scripts do MCP indisponível");
                state
                    .bus
                    .log("warn", "mcp", format!("Token para scripts: {e}"));
            }
            state.set_module_status(Module::Mcp, ModuleStatus::Running);
        }
        Err(e) => {
            *state.mcp_url.write() = String::new();
            state.set_module_status(Module::Mcp, ModuleStatus::Error(e));
        }
    }
}

/// Chave, no cofre de segredos, do token fixo para scripts fora das threads.
pub const SCRIPT_TOKEN_KEY: &str = "mcp_script_token";

/// Garante que existe um token de scripts (gera e grava na primeira vez) e o
/// registra no servidor MCP como chamador geral (sem thread).
pub async fn ensure_script_token(state: &Arc<AppState>) -> Result<String, String> {
    use secrecy::ExposeSecret;
    let existing = state.secrets.get(SCRIPT_TOKEN_KEY).await?;
    let token = match existing {
        Some(t) if !t.expose_secret().trim().is_empty() => t.expose_secret().to_string(),
        _ => {
            let t = will_mcp::tokens::random_token();
            state
                .secrets
                .set(SCRIPT_TOKEN_KEY, &SecretString::from(t.clone()))
                .await?;
            t
        }
    };
    state.tokens.register_generic(&token);
    Ok(token)
}

/// Troca o token de scripts: o antigo deixa de valer na hora.
pub async fn rotate_script_token(state: &Arc<AppState>) -> Result<String, String> {
    let t = will_mcp::tokens::random_token();
    state
        .secrets
        .set(SCRIPT_TOKEN_KEY, &SecretString::from(t.clone()))
        .await?;
    state.tokens.revoke_generic();
    state.tokens.register_generic(&t);
    state
        .bus
        .log("info", "mcp", "Token para scripts regenerado.".to_string());
    Ok(t)
}

fn register_core_tools(state: &Arc<AppState>) {
    let kb = state
        .kb_path()
        .unwrap_or_else(|| state.paths.data_dir.clone());
    state
        .registry
        .add_provider(Arc::new(will_mcp::core_tools::CoreTools {
            host: state.host(),
            bus: state.bus.clone(),
            kb_path: kb,
            mcp_url: state.mcp_url.clone(),
            enable_thread_tools: true,
        }));
}

/// Reconstrói a EngineConfig. `restart_live`: derruba processos vivos (troca de token/KB/permissões).
pub async fn refresh_engine(state: &Arc<AppState>, restart_live: bool) -> usize {
    let host = state.host();
    let token = host.active_token().await;
    let ext_env = crate::mcp_json::external_env(state).await;
    let settings = state.settings();
    let url = state.mcp_url.read().clone();
    let cfg = crate::engine_cfg::build(&settings, &state.paths, token.as_ref(), &url, &ext_env);
    if restart_live {
        state.engine.reconfigure(cfg)
    } else {
        state.engine.set_config(cfg);
        0
    }
}

pub async fn start_realtime(state: &Arc<AppState>) {
    let s = state.settings().realtime;
    let key = state
        .secrets
        .get(s.openai_api_key_ref.as_deref().unwrap_or("openai_api_key"))
        .await
        .ok()
        .flatten();
    let existing = state.realtime.read().clone();
    match existing {
        Some(rt) => {
            rt.update(s.clone(), key);
            state.set_module_status(Module::Realtime, rt.module_status());
        }
        None => {
            let rt = will_realtime::RealtimeManager::new(
                will_realtime::RealtimeDeps {
                    host: state.host(),
                    bus: state.bus.clone(),
                    storage: state.storage.clone(),
                },
                s.clone(),
                key,
            );
            state.set_module_status(Module::Realtime, rt.module_status());
            *state.realtime.write() = Some(rt);
        }
    }
}

pub async fn openai_key(state: &AppState) -> Option<SecretString> {
    let r = state
        .settings
        .read()
        .realtime
        .openai_api_key_ref
        .clone()
        .unwrap_or_else(|| "openai_api_key".into());
    state.secrets.get(&r).await.ok().flatten()
}

#[cfg(feature = "telegram")]
pub async fn start_telegram(state: &Arc<AppState>) {
    if let Some(t) = state.telegram.write().await.take() {
        t.stop().await;
    }
    state.registry.remove_family("telegram");
    let s = state.settings().telegram;
    if !s.enabled {
        state.set_module_status(Module::Telegram, ModuleStatus::Disabled);
        return;
    }
    let Some(token) = state
        .secrets
        .get(s.bot_token_ref.as_deref().unwrap_or("telegram_bot_token"))
        .await
        .ok()
        .flatten()
    else {
        state.set_module_status(
            Module::Telegram,
            ModuleStatus::Error("Token do bot não configurado".into()),
        );
        return;
    };
    let kb = state
        .kb_path()
        .unwrap_or_else(|| state.paths.data_dir.clone());
    let module = will_telegram::TelegramModule::new(will_telegram::TelegramDeps {
        settings: s,
        bot_token: token,
        openai_api_key: openai_key(state).await,
        storage: state.storage.clone(),
        host: state.host(),
        bus: state.bus.clone(),
        kb_path: kb,
    });
    state.set_module_status(Module::Telegram, ModuleStatus::Starting);
    match module.start().await {
        Ok(()) => {
            state.registry.add_provider(module.clone());
            state.set_module_status(Module::Telegram, module.status());
            *state.telegram.write().await = Some(module);
        }
        Err(e) => {
            state.set_module_status(Module::Telegram, ModuleStatus::Error(e));
        }
    }
}

#[cfg(feature = "google")]
pub async fn start_google(state: &Arc<AppState>) {
    let s = state.settings();
    let cfg = will_google::GoogleConfig {
        google: s.google.clone(),
        calendar: s.calendar.clone(),
        gmail: s.gmail.clone(),
    };
    let kb = state
        .kb_path()
        .unwrap_or_else(|| state.paths.data_dir.clone());
    let existing = state.google.read().clone();
    let module = match existing {
        Some(m) => {
            m.update_config(cfg);
            m
        }
        None => {
            let m = will_google::GoogleModule::new(
                will_google::GoogleDeps {
                    storage: state.storage.clone(),
                    secrets: state.secrets.clone(),
                    bus: state.bus.clone(),
                    kb_path: kb,
                },
                cfg,
            );
            *state.google.write() = Some(m.clone());
            m
        }
    };
    state.registry.add_provider(module.calendar_provider());
    state.registry.add_provider(module.gmail_provider());
    let st = module.status();
    state.set_module_status(Module::Google, st.clone());
    state.set_module_status(
        Module::Calendar,
        if s.calendar.enabled {
            st.clone()
        } else {
            ModuleStatus::Disabled
        },
    );
    state.set_module_status(
        Module::Gmail,
        if s.gmail.enabled {
            st
        } else {
            ModuleStatus::Disabled
        },
    );
}

#[cfg(feature = "whatsapp")]
pub async fn start_whatsapp(state: &Arc<AppState>) {
    let s = state.settings().whatsapp;
    let kb = state
        .kb_path()
        .unwrap_or_else(|| state.paths.data_dir.clone());
    let existing = state.whatsapp.read().await.clone();
    let module = match existing {
        Some(m) => {
            m.update_settings(s.clone()).await;
            m
        }
        None => {
            let m = will_whatsapp::WhatsAppModule::new_default(
                will_whatsapp::WhatsAppDeps {
                    storage: state.storage.clone(),
                    paths: state.paths.clone(),
                    bus: state.bus.clone(),
                    kb_path: kb,
                },
                s.clone(),
            );
            *state.whatsapp.write().await = Some(m.clone());
            m
        }
    };
    state.registry.add_provider(module.clone());
    if s.enabled {
        match module.start().await {
            Ok(()) => state.set_module_status(Module::WhatsApp, module.module_status()),
            Err(e) => state.set_module_status(Module::WhatsApp, ModuleStatus::Error(e)),
        }
    } else {
        module.stop().await;
        state.set_module_status(Module::WhatsApp, ModuleStatus::Disabled);
    }
    sync_whatsapp_accounts(state, &module).await;
}

/// Persiste `settings.whatsapp.accounts` a partir das contas do módulo
/// (pareamento cria contas; `unlink(remove)` as tira), se houver diferença.
#[cfg(feature = "whatsapp")]
pub async fn sync_whatsapp_accounts(
    state: &Arc<AppState>,
    module: &Arc<will_whatsapp::WhatsAppModule>,
) {
    let refs = module.account_refs();
    let changed = {
        let mut s = state.settings.write();
        if s.whatsapp.accounts != refs {
            s.whatsapp.accounts = refs;
            true
        } else {
            false
        }
    };
    if changed {
        if let Err(e) = state.save_settings().await {
            tracing::warn!("salvar contas do WhatsApp nas configurações: {e}");
        }
        state.bus.emit(will_core::Event::SettingsChanged {
            section: "whatsapp".into(),
        });
    }
}

/// Marca o onboarding como concluído quando há KB válida e token ativo.
pub async fn maybe_finish_onboarding(state: &Arc<AppState>) {
    let (done, kb_ok, token_ok) = {
        let s = state.settings.read();
        (
            s.general.onboarding_done,
            s.kb.path
                .as_ref()
                .map(|p| state.paths.resolve_user_path(p).is_dir())
                .unwrap_or(false),
            s.engine.tokens.iter().any(|t| t.active),
        )
    };
    if !done && kb_ok && token_ok {
        state.settings.write().general.onboarding_done = true;
        let _ = state.save_settings().await;
        state.bus.emit(will_core::Event::SettingsChanged {
            section: "general".into(),
        });
    }
}

/// Aplica uma seção alterada.
pub async fn apply_section(state: &Arc<AppState>, section: &str) {
    state.bus.emit(will_core::Event::SettingsChanged {
        section: section.to_string(),
    });
    if section == "kb" || section == "engine" {
        maybe_finish_onboarding(state).await;
    }
    match section {
        "engine" => {
            // Troca de permissões/modelo/allowlist exige novo processo; aplicamos sem derrubar
            // (vale no próximo spawn). Troca de token é via activate_token.
            refresh_engine(state, false).await;
            let has_token = state.settings.read().engine.tokens.iter().any(|t| t.active);
            if !has_token {
                state.set_module_status(
                    Module::Engine,
                    ModuleStatus::Degraded("Nenhum token do Claude ativo".into()),
                );
            }
        }
        "kb" => {
            let _ = crate::mcp_json::write(state).await;
            register_core_tools(state);
            refresh_engine(state, true).await;
            #[cfg(feature = "telegram")]
            start_telegram(state).await;
            #[cfg(feature = "google")]
            start_google(state).await;
            #[cfg(feature = "whatsapp")]
            start_whatsapp(state).await;
        }
        "mcp" => {
            start_mcp(state).await;
            let _ = crate::mcp_json::write(state).await;
            refresh_engine(state, false).await;
        }
        "external_mcps" => {
            let _ = crate::mcp_json::write(state).await;
            refresh_engine(state, false).await;
        }
        "realtime" => start_realtime(state).await,
        #[cfg(feature = "telegram")]
        "telegram" => start_telegram(state).await,
        #[cfg(feature = "google")]
        "google" | "calendar" | "gmail" => start_google(state).await,
        #[cfg(feature = "whatsapp")]
        "whatsapp" => start_whatsapp(state).await,
        _ => {}
    }
}

/// Testa e ativa um token. Retorna quantas threads vivas foram derrubadas.
pub async fn activate_token(state: &Arc<AppState>, id: &str) -> Result<usize, String> {
    let entry: ClaudeTokenRef = state
        .settings
        .read()
        .engine
        .tokens
        .iter()
        .find(|t| t.id == id)
        .cloned()
        .ok_or("token não encontrado")?;
    let secret = state
        .secrets
        .get(&entry.secret_ref)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("segredo do token ausente no keyring")?;
    let res = test_token(state, &secret).await;
    {
        let mut s = state.settings.write();
        if let Some(t) = s.engine.tokens.iter_mut().find(|t| t.id == id) {
            t.verified_at = Some(chrono::Utc::now());
            t.status = Some(if res.ok { "ok".into() } else { "error".into() });
            t.error = res.error.clone();
        }
    }
    if !res.ok {
        state.save_settings().await?;
        return Err(res.error.unwrap_or_else(|| "token reprovado".into()));
    }
    {
        let mut s = state.settings.write();
        for t in s.engine.tokens.iter_mut() {
            t.active = t.id == id;
            if t.active {
                t.selected_at = Some(chrono::Utc::now());
            }
        }
    }
    state.save_settings().await?;
    let n = refresh_engine(state, true).await;
    state.set_module_status(Module::Engine, ModuleStatus::Stopped);
    state.bus.log(
        "info",
        "engine",
        format!(
            "Token '{}' ativado; {} thread(s) derrubada(s), voltam com --resume.",
            entry.label, n
        ),
    );
    maybe_finish_onboarding(state).await;
    Ok(n)
}

pub async fn test_token(
    state: &AppState,
    secret: &SecretString,
) -> will_engine::token_test::TokenTestResult {
    use secrecy::ExposeSecret;
    let cfg = state.engine.config();
    let cwd = if cfg.cwd.is_dir() {
        cfg.cwd.clone()
    } else {
        state.paths.data_dir.clone()
    };
    will_engine::token_test::test_token(
        &cfg.claude_bin,
        &cwd,
        secret.expose_secret(),
        &will_engine::config::minimal_env(),
        Duration::from_secs(90),
    )
    .await
}
