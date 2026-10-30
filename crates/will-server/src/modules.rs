//! Ciclo de vida dos módulos e aplicação de mudanças de configuração.

use crate::app::AppState;
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;
use will_core::settings::{ClaudeTokenRef, ToolPolicySetting};
use will_core::{Module, ModuleStatus, ToolPolicy};
use will_mcp::McpServerDeps;

pub async fn start_all(state: &Arc<AppState>) {
    tracing::info!("subindo módulos");
    spawn_status_sync(state);
    start_mcp(state).await;
    register_core_tools(state);
    refresh_engine(state, false).await;
    crate::orchestrator::retitle_placeholder_threads(state).await;
    write_mcp_json(state).await;
    start_realtime(state).await;
    start_scheduler(state);
    start_browser(state).await;
    #[cfg(feature = "google")]
    start_google(state).await;
    #[cfg(feature = "telegram")]
    start_telegram(state).await;
    #[cfg(feature = "whatsapp")]
    start_whatsapp(state).await;
    start_triggers(state);
    recompute_engine_status(state);
    tracing::info!("módulos no ar");
}

/// Fonte de e-mails das triggers sobre o módulo Google (`gmail_poll`).
#[cfg(feature = "google")]
struct GmailSource(Arc<AppState>);

#[cfg(feature = "google")]
#[async_trait::async_trait]
impl will_triggers::MailSource for GmailSource {
    async fn poll(
        &self,
        account: Option<&str>,
        query: &str,
        max: u64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let g = self
            .0
            .google
            .read()
            .clone()
            .ok_or_else(|| "módulo Google não iniciado".to_string())?;
        g.gmail_poll(account, query, max).await
    }
}

#[cfg(feature = "google")]
pub fn gmail_source(state: &Arc<AppState>) -> impl will_triggers::MailSource {
    GmailSource(state.clone())
}

/// Triggers universais (`docs/30`): sobe/aplica configuração. A fonte de
/// e-mail depende do módulo Google (feature `google`); o WhatsApp entra pelo
/// barramento, sem dependência direta.
pub fn start_triggers(state: &Arc<AppState>) {
    let cfg = state.settings().triggers;
    let existing = state.triggers.read().clone();
    match existing {
        Some(m) => m.update_config(cfg),
        None => {
            #[cfg(feature = "google")]
            let mail: Option<Arc<dyn will_triggers::MailSource>> =
                Some(Arc::new(GmailSource(state.clone())));
            #[cfg(not(feature = "google"))]
            let mail: Option<Arc<dyn will_triggers::MailSource>> = None;
            let m = will_triggers::TriggersModule::new(
                will_triggers::TriggersDeps {
                    storage: state.storage.clone(),
                    host: state.host(),
                    bus: state.bus.clone(),
                    mail,
                },
                cfg,
            );
            *state.triggers.write() = Some(m.clone());
            m.start();
        }
    }
}

/// Regrava o `.mcp.json` da KB, logando a falha (os chamadores não param por ela).
async fn write_mcp_json(state: &Arc<AppState>) {
    if let Err(e) = crate::mcp_json::write(state).await {
        tracing::warn!(error = %e, "não regravou o .mcp.json da base de conhecimento");
    }
}

/// Um evento por mudança de status de módulo, no span `module` (nome, status
/// e motivo), para rastrear ciclo de vida em qualquer máquina.
fn log_module_status(module: Module, status: &ModuleStatus) {
    let (label, reason) = match status {
        ModuleStatus::Disabled => ("stopped", String::new()),
        ModuleStatus::Starting => ("starting", String::new()),
        ModuleStatus::Running => ("running", String::new()),
        ModuleStatus::Degraded(r) => ("degraded", r.clone()),
        ModuleStatus::Error(r) => ("error", r.clone()),
        ModuleStatus::Stopped => ("stopped", String::new()),
    };
    let span = tracing::info_span!(
        "module",
        module = module.as_str(),
        status = label,
        reason = %crate::logging::redact(&reason)
    );
    let _entered = span.entered();
    match status {
        ModuleStatus::Error(_) => tracing::error!("módulo com erro"),
        ModuleStatus::Degraded(_) => tracing::warn!("módulo degradado"),
        ModuleStatus::Disabled => tracing::info!("módulo desligado nas configurações"),
        _ => tracing::info!("status do módulo"),
    }
}

// ----- estado dos módulos (semântica em `will_core::ModuleStatus`) -----

/// Mantém `AppState::module_status` em dia com os status que os módulos emitem
/// direto no barramento (Google, voz, WhatsApp, Telegram), para que o snapshot
/// inicial da UI e `/api/status` não fiquem defasados. Para o Google recalcula
/// também Agenda, Gmail e Drive, que dependem das contas.
fn spawn_status_sync(state: &Arc<AppState>) {
    let state = state.clone();
    tokio::spawn(async move {
        let mut rx = state.bus.subscribe();
        loop {
            let ev = tokio::select! {
                _ = state.cancel.cancelled() => break,
                ev = rx.recv() => ev,
            };
            match ev {
                Ok(will_core::Event::ModuleStatus { module, status }) => {
                    log_module_status(module, &status);
                    #[cfg(feature = "google")]
                    if matches!(
                        module,
                        Module::Google | Module::Calendar | Module::Gmail | Module::Drive
                    ) {
                        recompute_google_status(&state);
                        continue;
                    }
                    // Sem `set_module_status`: o evento já está no barramento.
                    state.module_status.write().insert(module, status);
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "sincronização de status: eventos perdidos");
                    continue;
                }
                Err(_) => break,
            }
        }
    });
}

/// Motor: `Running` = pronto (token ativo + KB válida), com ou sem processos
/// vivos; senão `Degraded` com o que falta. Chamar após mudar KB/tokens e ao
/// subir um processo (limpa um `Degraded` transitório de auth/rate limit).
pub fn recompute_engine_status(state: &Arc<AppState>) {
    let kb_ok = state.kb_path().map(|p| p.is_dir()).unwrap_or(false);
    let has_token = state.settings.read().engine.tokens.iter().any(|t| t.active);
    let status = if !kb_ok {
        ModuleStatus::Degraded("pasta da base de conhecimento não configurada".into())
    } else if !has_token {
        ModuleStatus::Degraded("sem token do Claude ativo".into())
    } else {
        ModuleStatus::Running
    };
    state.set_module_status(Module::Engine, status);
}

/// Voz: delega ao `RealtimeManager` (`Running` = habilitada + chave, em sessão
/// ou não). Sem gerenciador: `Disabled` se desligada nas configurações.
pub fn recompute_realtime_status(state: &Arc<AppState>) {
    let enabled = state.settings.read().realtime.enabled;
    let status = if !enabled {
        ModuleStatus::Disabled
    } else if state.voice_key.read().is_none() {
        ModuleStatus::Degraded("sem chave da OpenAI".into())
    } else {
        // Qualquer sessão com erro recente aparece como atenção.
        let sessions = state.voice.read().clone();
        sessions
            .values()
            .map(|m| m.module_status())
            .find(|s| matches!(s, ModuleStatus::Degraded(_)))
            .unwrap_or(ModuleStatus::Running)
    };
    state.set_module_status(Module::Realtime, status);
}

/// Sessão de voz do usuário (cria sob demanda com as configurações atuais).
pub fn voice_manager_for(state: &Arc<AppState>, user: &str) -> Arc<will_realtime::RealtimeManager> {
    if let Some(m) = state.voice.read().get(user) {
        return m.clone();
    }
    let settings = state.settings().realtime;
    let key = state.voice_key.read().clone();
    let host = state.host();
    let brain: Arc<dyn will_core::BrainHost> = Arc::new(crate::orchestrator::UserBrain {
        host,
        user: user.to_string(),
    });
    let m = will_realtime::RealtimeManager::new(
        will_realtime::RealtimeDeps {
            host: brain,
            bus: state.bus.clone(),
            storage: state.storage.clone(),
            owner: Some(user.to_string()),
        },
        settings,
        key,
    );
    state.voice.write().insert(user.to_string(), m.clone());
    m
}

/// Quantas sessões de voz estão ativas agora.
pub fn active_voice_sessions(state: &AppState) -> usize {
    state
        .voice
        .read()
        .values()
        .filter(|m| m.is_active())
        .count()
}

/// Google, Agenda, Gmail e Drive a partir das contas conectadas no módulo.
#[cfg(feature = "google")]
pub fn recompute_google_status(state: &Arc<AppState>) {
    let module = state.google.read().clone();
    let (google, calendar, gmail, drive) = match module {
        Some(m) => (
            m.status(),
            m.calendar_status(),
            m.gmail_status(),
            m.drive_status(),
        ),
        None => {
            let s = state.settings.read();
            let off = |enabled: bool| {
                if enabled {
                    ModuleStatus::Degraded("módulo Google não inicializado".into())
                } else {
                    ModuleStatus::Disabled
                }
            };
            (
                ModuleStatus::Disabled,
                off(s.calendar.enabled),
                off(s.gmail.enabled),
                off(s.drive.enabled),
            )
        }
    };
    state.set_module_status(Module::Google, google);
    state.set_module_status(Module::Calendar, calendar);
    state.set_module_status(Module::Gmail, gmail);
    state.set_module_status(Module::Drive, drive);
}

pub async fn stop_all(state: &Arc<AppState>) {
    tracing::info!("parando módulos");
    let managers: Vec<Arc<will_realtime::RealtimeManager>> =
        state.voice.read().values().cloned().collect();
    for m in managers {
        m.stop().await;
    }
    #[cfg(feature = "telegram")]
    if let Some(t) = state.telegram.read().await.clone() {
        t.stop().await;
    }
    #[cfg(feature = "whatsapp")]
    if let Some(w) = state.whatsapp.read().await.clone() {
        w.stop().await;
    }
    if let Some(sch) = state.scheduler.read().clone() {
        sch.stop();
    }
    let browser = state.browser.read().clone();
    if let Some(b) = browser {
        b.stop().await;
    }
    if let Some(m) = state.mcp.lock().await.take() {
        m.stop();
    }
    tracing::info!("módulos parados");
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
        tracing::info!("servidor MCP anterior parado para reiniciar");
        old.stop();
    }
    if !enabled {
        *state.mcp_url.write() = String::new();
        state.set_module_status(Module::Mcp, ModuleStatus::Disabled);
        return;
    }
    tracing::debug!(
        port,
        approval_timeout_secs = timeout,
        overrides = state
            .registry
            .listing()
            .iter()
            .filter(|t| t.overridden)
            .count(),
        "iniciando servidor MCP"
    );
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
                tracing::warn!(target: "will_mcp", error = %e, "Token para scripts: {e}");
            }
            state.set_module_status(Module::Mcp, ModuleStatus::Running);
        }
        Err(e) => {
            tracing::error!(port, error = %e, "servidor MCP não subiu");
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
    tracing::info!(target: "will_mcp", "Token para scripts regenerado.");
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
    tracing::debug!(
        restart_live,
        token_id = cfg.token_id.as_deref().unwrap_or(""),
        has_token = token.is_some(),
        mcp_url = !url.is_empty(),
        external_env = ext_env.len(),
        "configuração do motor reconstruída"
    );
    if restart_live {
        state.engine.reconfigure(cfg)
    } else {
        state.engine.set_config(cfg);
        0
    }
}

/// Browser CDP: proxy em loopback + lançamento do navegador sob demanda.
pub async fn start_browser(state: &Arc<AppState>) {
    let cfg = state.settings().browser;
    tracing::debug!(
        enabled = cfg.enabled,
        "aplicando configuração do Browser CDP"
    );
    // Pasta das capturas/downloads do agent-browser (dentro do inbox da KB).
    if cfg.enabled {
        if let Some(kb) = state.kb_path().filter(|p| p.is_dir()) {
            let _ = std::fs::create_dir_all(kb.join("inbox").join("browser"));
        }
    }
    let existing = state.browser.read().clone();
    match existing {
        Some(m) => {
            m.set_kb_path(state.kb_path());
            m.update_config(cfg).await;
        }
        None => {
            let m = will_browser::BrowserModule::new(
                will_browser::BrowserDeps {
                    bus: state.bus.clone(),
                    data_dir: state.paths.data_dir.clone(),
                    kb_path: parking_lot::RwLock::new(state.kb_path()),
                },
                cfg,
            );
            *state.browser.write() = Some(m.clone());
            m.start().await;
        }
    }
}

/// Scheduler (agendamentos): laço único em processo; `enabled` liga/desliga.
pub fn start_scheduler(state: &Arc<AppState>) {
    let cfg = state.settings().scheduler;
    tracing::debug!(enabled = cfg.enabled, "aplicando configuração do scheduler");
    let existing = state.scheduler.read().clone();
    match existing {
        Some(m) => m.update_config(cfg),
        None => {
            let m = will_scheduler::SchedulerModule::new(
                will_scheduler::SchedulerDeps {
                    storage: state.storage.clone(),
                    host: state.host(),
                    bus: state.bus.clone(),
                },
                cfg,
            );
            *state.scheduler.write() = Some(m.clone());
            state
                .registry
                .add_provider(Arc::new(will_scheduler::tools::SchedulerProvider::new(
                    m.clone(),
                )));
            m.start();
        }
    }
}

/// Aplica configurações/chave da voz a todas as sessões por usuário.
pub async fn start_realtime(state: &Arc<AppState>) {
    let s = state.settings().realtime;
    let key = state
        .secrets
        .get(s.openai_api_key_ref.as_deref().unwrap_or("openai_api_key"))
        .await
        .ok()
        .flatten();
    *state.voice_key.write() = key.clone();
    let managers: Vec<Arc<will_realtime::RealtimeManager>> =
        state.voice.read().values().cloned().collect();
    tracing::debug!(
        enabled = s.enabled,
        has_key = key.is_some(),
        sessions = managers.len(),
        "aplicando configuração de voz"
    );
    for m in managers {
        m.update(s.clone(), key.clone());
    }
    recompute_realtime_status(state);
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
        tracing::info!("módulo Telegram anterior parado para reiniciar");
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
            tracing::error!(error = %e, "módulo Telegram não subiu");
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
        drive: s.drive.clone(),
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
    state.registry.add_provider(module.drive_provider());
    // As contas carregam em segundo plano na primeira vez; quando terminarem o
    // módulo emite `ModuleStatus` e `spawn_status_sync` recalcula os quatro.
    recompute_google_status(state);
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
            // Voz sintetizada para `whatsapp_send_voice { text }`: usa a chave da
            // OpenAI e a voz/velocidade do "ouvir resumo" (Configurações → Voz).
            let tts: will_whatsapp::Tts = {
                let st = state.clone();
                std::sync::Arc::new(move |text: String| {
                    let st = st.clone();
                    Box::pin(async move {
                        let key = openai_key(&st).await.ok_or_else(|| {
                            "sem chave da OpenAI (Configurações → Voz)".to_string()
                        })?;
                        let cfg = st.settings().realtime.summary;
                        let voice = if cfg.voice.trim().is_empty() {
                            st.settings().realtime.voice
                        } else {
                            cfg.voice
                        };
                        will_realtime::openai::OpenAi::new(key)
                            .synthesize(&text, &voice, cfg.speed)
                            .await
                    })
                })
            };
            let m = will_whatsapp::WhatsAppModule::new_default(
                will_whatsapp::WhatsAppDeps {
                    storage: state.storage.clone(),
                    paths: state.paths.clone(),
                    bus: state.bus.clone(),
                    kb_path: kb,
                    tts: Some(tts),
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
            Err(e) => {
                tracing::error!(error = %e, "módulo WhatsApp não subiu");
                state.set_module_status(Module::WhatsApp, ModuleStatus::Error(e))
            }
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
        tracing::info!("contas do WhatsApp sincronizadas nas configurações");
        if let Err(e) = state.save_settings().await {
            tracing::warn!(error = %e, "salvar contas do WhatsApp nas configurações");
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
        tracing::info!("onboarding concluído (KB válida + token ativo)");
        state.settings.write().general.onboarding_done = true;
        if let Err(e) = state.save_settings().await {
            tracing::warn!(error = %e, "onboarding concluído mas configurações não salvas");
        }
        state.bus.emit(will_core::Event::SettingsChanged {
            section: "general".into(),
        });
    }
}

/// Aplica uma seção alterada.
pub async fn apply_section(state: &Arc<AppState>, section: &str) {
    tracing::info!(section, "configurações aplicadas");
    state.bus.emit(will_core::Event::SettingsChanged {
        section: section.to_string(),
    });
    if section == "kb" || section == "engine" {
        maybe_finish_onboarding(state).await;
    }
    match section {
        "telemetry" => crate::telemetry::apply(state).await,
        "engine" => {
            // Troca de permissões/modelo/allowlist exige novo processo; aplicamos sem derrubar
            // (vale no próximo spawn). Troca de token é via activate_token.
            refresh_engine(state, false).await;
            recompute_engine_status(state);
        }
        "kb" => {
            write_mcp_json(state).await;
            register_core_tools(state);
            let browser = state.browser.read().clone();
            if let Some(b) = browser {
                b.set_kb_path(state.kb_path());
                if let Some(kb) = state.kb_path().filter(|p| p.is_dir()) {
                    let _ = std::fs::create_dir_all(kb.join("inbox").join("browser"));
                }
                b.ensure_skill().await;
                b.refresh_readiness();
            }
            refresh_engine(state, true).await;
            recompute_engine_status(state);
            #[cfg(feature = "telegram")]
            start_telegram(state).await;
            #[cfg(feature = "google")]
            start_google(state).await;
            #[cfg(feature = "whatsapp")]
            start_whatsapp(state).await;
        }
        "mcp" => {
            start_mcp(state).await;
            write_mcp_json(state).await;
            refresh_engine(state, false).await;
        }
        "external_mcps" => {
            write_mcp_json(state).await;
            refresh_engine(state, false).await;
        }
        "realtime" => start_realtime(state).await,
        "scheduler" => start_scheduler(state),
        "triggers" => start_triggers(state),
        "network" => {
            tracing::warn!(
                target: "will_network",
                "Configurações de rede salvas; valem no próximo início do servidor."
            );
        }
        "browser" => {
            start_browser(state).await;
            refresh_engine(state, false).await;
        }
        #[cfg(feature = "telegram")]
        "telegram" => start_telegram(state).await,
        #[cfg(feature = "google")]
        "google" | "calendar" | "gmail" | "drive" => start_google(state).await,
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
        .ok_or_else(|| {
            tracing::warn!(token_id = id, "ativação: token não encontrado");
            "token não encontrado".to_string()
        })?;
    let secret = state
        .secrets
        .get(&entry.secret_ref)
        .await
        .map_err(|e| {
            tracing::error!(token_id = id, error = %e, "ativação: falha ao ler o segredo do token");
            e.to_string()
        })?
        .ok_or_else(|| {
            tracing::error!(
                token_id = id,
                "ativação: segredo do token ausente no keyring"
            );
            "segredo do token ausente no keyring".to_string()
        })?;
    tracing::info!(token_id = id, label = %entry.label, "testando token antes de ativar");
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
        tracing::warn!(
            token_id = id,
            error = %crate::logging::redact(res.error.as_deref().unwrap_or("token reprovado")),
            "token reprovado no teste; não ativado"
        );
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
    recompute_engine_status(state);
    tracing::info!(
        target: "will_engine",
        token_id = id,
        restarted = n,
        "Token '{}' ativado; {} thread(s) derrubada(s), voltam com --resume.",
        entry.label,
        n
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
