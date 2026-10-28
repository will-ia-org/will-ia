//! Orquestrador: implementa ThreadHost (e BrainHost por usuário, `UserBrain`) e converte eventos do motor em estado + eventos.

use crate::app::AppState;
use async_trait::async_trait;
use secrecy::SecretString;
use std::sync::Arc;
use tokio::sync::broadcast;
use will_core::{
    ApprovalDecision, ApprovalId, BrainHost, BrainTask, ClaudeTokenInfo, EngineDelta, Event,
    Origin, ThreadHost, ThreadId, ThreadStatus, ThreadSummary, TurnId, TurnResult,
};
use will_engine::{EngineEvent, SendRequest};

pub struct Host {
    state: Arc<AppState>,
}

impl Host {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// Preenche alive/pending/session_expired.
    pub fn enrich(&self, mut t: ThreadSummary) -> ThreadSummary {
        t.alive = self.state.engine.is_alive(t.id);
        t.pending = self.state.engine.pending(t.id);
        if t.alive {
            t.status = if t.pending > 0 {
                ThreadStatus::Running
            } else {
                ThreadStatus::Live
            };
        } else if t.status != ThreadStatus::Archived && t.status != ThreadStatus::Error {
            t.status = ThreadStatus::Idle;
        }
        t.session_expired = match (&t.claude_session_id, self.state.kb_path()) {
            (Some(sid), Some(kb)) if !t.alive => will_core::paths::claude_transcript_path(&kb, sid)
                .map(|p| !p.exists())
                .unwrap_or(false),
            _ => false,
        };
        t
    }

    pub async fn active_token(&self) -> Option<SecretString> {
        let r = self
            .state
            .settings
            .read()
            .engine
            .tokens
            .iter()
            .find(|t| t.active)
            .map(|t| t.secret_ref.clone())?;
        self.state.secrets.get(&r).await.ok().flatten()
    }

    pub async fn thread_env_for(&self, t: &ThreadSummary) -> will_engine::ThreadEnv {
        let token = if self.state.settings.read().mcp.enabled {
            Some(self.state.tokens.issue(t.id, t.origin.clone()))
        } else {
            None
        };
        crate::engine_cfg::thread_env(t.id, &t.origin, token)
    }

    /// Encontra uma thread por id (prefixo) ou por título (contém, sem diferenciar
    /// maiúsculas). Ambíguo → erro listando as candidatas.
    pub async fn resolve_thread(&self, query: &str) -> Result<ThreadSummary, String> {
        let q = query.trim().to_lowercase();
        let all = self.list_threads().await?;
        let live: Vec<&ThreadSummary> = all
            .iter()
            .filter(|t| t.status != ThreadStatus::Archived)
            .collect();
        if let Some(t) = live
            .iter()
            .find(|t| t.id.to_string().to_lowercase().starts_with(&q))
        {
            return Ok((*t).clone());
        }
        // título exato (sem diferenciar maiúsculas) ganha da busca parcial; se
        // houver mais de um exato (ex.: várias "Voz"), fica com o mais recente.
        let mut exact: Vec<&&ThreadSummary> = live
            .iter()
            .filter(|t| {
                t.title
                    .as_deref()
                    .map(|x| x.trim().to_lowercase() == q)
                    .unwrap_or(false)
            })
            .collect();
        if !exact.is_empty() {
            exact.sort_by_key(|t| std::cmp::Reverse(t.last_activity_at));
            return Ok((*exact[0]).clone());
        }
        let by_title: Vec<&&ThreadSummary> = live
            .iter()
            .filter(|t| {
                t.title
                    .as_deref()
                    .map(|x| x.to_lowercase().contains(&q))
                    .unwrap_or(false)
            })
            .collect();
        match by_title.len() {
            1 => Ok((*by_title[0]).clone()),
            0 => Err(format!(
                "nenhuma thread com id ou título contendo '{query}'"
            )),
            _ => Err(format!(
                "mais de uma thread combina com '{query}': {}",
                by_title
                    .iter()
                    .map(|t| format!(
                        "{} ({})",
                        t.title.clone().unwrap_or_default(),
                        &t.id.to_string()[..8]
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Título da thread de voz de um usuário ("Voz" para o token local).
    pub fn voice_title(user: &str) -> String {
        if user == "local" {
            "Voz".into()
        } else {
            format!("Voz · {user}")
        }
    }

    /// Thread de voz do usuário: a corrente, senão a última de origem `realtime`
    /// com o título dele, senão uma nova.
    pub async fn voice_thread_for(
        &self,
        user: &str,
        new_thread: bool,
        title: Option<&str>,
    ) -> Result<ThreadSummary, String> {
        if !new_thread {
            let current = self.state.voice_threads.read().get(user).copied();
            if let Some(id) = current {
                if let Ok(t) = self.state.storage.get_thread(id).await {
                    return Ok(self.enrich(t));
                }
            }
            let wanted = Self::voice_title(user);
            if let Ok(list) = self.state.storage.list_threads(false).await {
                if let Some(t) = list
                    .into_iter()
                    .filter(|t| t.origin == Origin::Realtime)
                    .filter(|t| {
                        t.title.as_deref() == Some(wanted.as_str())
                            || (user == "local" && t.title.is_none())
                    })
                    .max_by_key(|t| t.last_activity_at)
                {
                    self.state
                        .voice_threads
                        .write()
                        .insert(user.to_string(), t.id);
                    return Ok(self.enrich(t));
                }
            }
        }
        let title = title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .unwrap_or_else(|| Self::voice_title(user));
        let t = self
            .state
            .storage
            .create_thread(&Origin::Realtime, Some(&title))
            .await
            .map_err(|e| e.to_string())?;
        self.state
            .bus
            .emit(Event::ThreadCreated { thread: t.clone() });
        self.state
            .voice_threads
            .write()
            .insert(user.to_string(), t.id);
        Ok(self.enrich(t))
    }
}

#[async_trait]
impl ThreadHost for Host {
    async fn ensure_thread(
        &self,
        origin: &Origin,
        title: Option<&str>,
    ) -> Result<ThreadSummary, String> {
        if *origin != Origin::Ui {
            if let Some(t) = self
                .state
                .storage
                .find_thread_by_origin(origin)
                .await
                .map_err(|e| e.to_string())?
            {
                return Ok(self.enrich(t));
            }
        }
        let t = self
            .state
            .storage
            .create_thread(origin, title)
            .await
            .map_err(|e| e.to_string())?;
        self.state
            .bus
            .emit(Event::ThreadCreated { thread: t.clone() });
        Ok(t)
    }

    async fn get_thread(&self, thread: ThreadId) -> Result<ThreadSummary, String> {
        self.state
            .storage
            .get_thread(thread)
            .await
            .map(|t| self.enrich(t))
            .map_err(|e| e.to_string())
    }

    async fn list_threads(&self) -> Result<Vec<ThreadSummary>, String> {
        Ok(self
            .state
            .storage
            .list_threads(false)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|t| self.enrich(t))
            .collect())
    }

    async fn send_message_with(
        &self,
        thread: ThreadId,
        text: &str,
        attachments: &[will_core::Attachment],
    ) -> Result<TurnId, String> {
        self.send_message_from(thread, text, attachments, None)
            .await
    }

    async fn send_message_from(
        &self,
        thread: ThreadId,
        text: &str,
        attachments: &[will_core::Attachment],
        author: Option<&str>,
    ) -> Result<TurnId, String> {
        let t = self
            .state
            .storage
            .get_thread(thread)
            .await
            .map_err(|_| "thread não encontrada".to_string())?;
        if self.state.kb_path().map(|p| !p.is_dir()).unwrap_or(true) {
            return Err(
                "Configure a pasta da base de conhecimento antes de conversar com o cérebro."
                    .into(),
            );
        }
        if self.active_token().await.is_none() {
            return Err("Nenhum token do Claude ativo. Cadastre e ative um token em Configurações → Motor Claude.".into());
        }
        // Primeira mensagem de uma thread sem assunto: o título vira o pedido.
        let first_message = self
            .state
            .storage
            .first_turn_prompt(thread)
            .await
            .map(|p| p.is_none())
            .unwrap_or(false);
        let turn_id = self
            .state
            .storage
            .start_turn_by(thread, text, attachments, author)
            .await
            .map_err(|e| e.to_string())?;
        if first_message && will_core::is_placeholder_title(t.title.as_deref()) {
            // título a partir do que o usuário escreveu, sem a nota de anexo
            let (clean, _) = will_core::split_attachment_notes(text);
            let title = will_core::derive_title(if clean.is_empty() { text } else { &clean });
            if self
                .state
                .storage
                .rename_thread(thread, &title)
                .await
                .is_ok()
            {
                emit_thread_updated(&self.state, thread).await;
            }
        }
        let env = self.thread_env_for(&t).await;
        self.state
            .engine
            .send(
                thread,
                t.claude_session_id.clone(),
                env,
                SendRequest {
                    turn_id,
                    text: text.to_string(),
                },
            )
            .map_err(|e| e.to_string())?;
        let _ = self.state.storage.touch_thread(thread).await;
        Ok(turn_id)
    }

    async fn revive_thread(
        &self,
        origin: &Origin,
        title: Option<&str>,
        claude_session_id: Option<&str>,
    ) -> Result<ThreadSummary, String> {
        let t = self
            .state
            .storage
            .create_thread(origin, title)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(sid) = claude_session_id {
            self.state
                .storage
                .set_thread_session(t.id, Some(sid))
                .await
                .map_err(|e| e.to_string())?;
        }
        let t = self
            .state
            .storage
            .get_thread(t.id)
            .await
            .map_err(|e| e.to_string())?;
        let t = self.enrich(t);
        self.state
            .bus
            .emit(Event::ThreadCreated { thread: t.clone() });
        Ok(t)
    }

    async fn restart_thread(&self, thread: ThreadId) -> Result<(), String> {
        self.state.engine.restart(thread, "pedido do usuário");
        Ok(())
    }

    async fn reset_thread(&self, thread: ThreadId) -> Result<(), String> {
        self.state.engine.reset(thread);
        self.state
            .storage
            .set_thread_session(thread, None)
            .await
            .map_err(|e| e.to_string())?;
        if let Ok(t) = self.state.storage.get_thread(thread).await {
            self.state.bus.emit(Event::ThreadUpdated {
                thread: self.enrich(t),
            });
        }
        Ok(())
    }

    async fn interrupt_thread(&self, thread: ThreadId) -> Result<(), String> {
        self.state.engine.interrupt(thread);
        Ok(())
    }

    async fn delete_thread(&self, thread: ThreadId, purge_transcript: bool) -> Result<(), String> {
        let t = self.state.storage.get_thread(thread).await.ok();
        self.state.engine.forget(thread).await;
        self.state.tokens.revoke_thread(thread);
        if purge_transcript {
            if let (Some(t), Some(kb)) = (&t, self.state.kb_path()) {
                if let Some(sid) = &t.claude_session_id {
                    if let Some(p) = will_core::paths::claude_transcript_path(&kb, sid) {
                        let _ = std::fs::remove_file(p);
                    }
                }
            }
        }
        self.state
            .storage
            .delete_thread(thread)
            .await
            .map_err(|e| e.to_string())?;
        self.state
            .bus
            .emit(Event::ThreadDeleted { thread_id: thread });
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.state.bus.subscribe()
    }

    fn claude_tokens(&self) -> Vec<ClaudeTokenInfo> {
        self.state
            .settings
            .read()
            .engine
            .tokens
            .iter()
            .map(|t| ClaudeTokenInfo {
                id: t.id.clone(),
                label: t.label.clone(),
                active: t.active,
                status: t.status.clone(),
                masked: String::new(),
            })
            .collect()
    }

    async fn switch_claude_token(&self, id: &str) -> Result<usize, String> {
        crate::modules::activate_token(&self.state, id).await
    }

    fn decide_approval(&self, id: ApprovalId, decision: ApprovalDecision, via: &str) -> bool {
        let ok = self.state.approvals.decide(id, decision, via);
        if ok {
            let st = self.state.clone();
            let via = via.to_string();
            tokio::spawn(async move {
                let _ = st
                    .storage
                    .record_approval_decision(id, decision, &via)
                    .await;
            });
        }
        ok
    }

    fn uptime_secs(&self) -> u64 {
        self.state.started_at.elapsed().as_secs()
    }
}

/// Cérebro visto por uma sessão de voz de **um** usuário: cada um tem a sua
/// thread de voz, mas as threads são compartilhadas (`docs/26`, etapa 6).
pub struct UserBrain {
    pub host: Arc<Host>,
    pub user: String,
}

#[async_trait]
impl BrainHost for UserBrain {
    async fn ask_brain(
        &self,
        prompt: &str,
        new_thread: bool,
        thread: Option<&str>,
        title: Option<&str>,
    ) -> Result<BrainTask, String> {
        let t = match thread.map(str::trim).filter(|q| !q.is_empty()) {
            Some(q) => {
                let found = self.host.resolve_thread(q).await?;
                self.host
                    .state
                    .voice_threads
                    .write()
                    .insert(self.user.clone(), found.id);
                found
            }
            None => {
                self.host
                    .voice_thread_for(&self.user, new_thread, title)
                    .await?
            }
        };
        let author = format!("voz:{}", self.user);
        let turn_id = self
            .host
            .send_message_from(t.id, prompt, &[], Some(&author))
            .await?;
        Ok(BrainTask {
            thread_id: t.id,
            turn_id,
        })
    }
    async fn brain_threads(&self) -> Result<Vec<ThreadSummary>, String> {
        let mut list = self.host.list_threads().await?;
        list.retain(|t| t.status != ThreadStatus::Archived);
        list.sort_by_key(|t| std::cmp::Reverse(t.last_activity_at));
        Ok(list)
    }
    async fn brain_busy(&self) -> bool {
        let current = self
            .host
            .state
            .voice_threads
            .read()
            .get(&self.user)
            .copied();
        match current {
            Some(id) => self.host.state.engine.pending(id) > 0,
            None => false,
        }
    }
    fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.host.state.bus.subscribe()
    }
}

/// Dá assunto às threads antigas que ficaram com título genérico ("Voz", vazio)
/// usando o primeiro pedido de cada uma. Roda na subida; é idempotente.
pub async fn retitle_placeholder_threads(state: &AppState) {
    let Ok(all) = state.storage.list_threads(true).await else {
        return;
    };
    for t in all {
        if !will_core::is_placeholder_title(t.title.as_deref()) {
            continue;
        }
        let Ok(Some(prompt)) = state.storage.first_turn_prompt(t.id).await else {
            continue;
        };
        let title = will_core::derive_title(&prompt);
        if state.storage.rename_thread(t.id, &title).await.is_ok() {
            tracing::info!(thread = %t.id, %title, "thread renomeada pelo primeiro pedido");
        }
    }
}

/// Converte EngineEvent → persistência + Event.
pub fn spawn_engine_loop(state: Arc<AppState>) {
    let mut rx = state.engine.subscribe();
    tokio::spawn(async move {
        loop {
            let ev = match rx.recv().await {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "engine events: atrasado");
                    continue;
                }
                Err(_) => break,
            };
            handle_engine_event(&state, ev).await;
        }
    });
}

async fn emit_thread_updated(state: &AppState, thread: ThreadId) {
    if let Ok(t) = state.storage.get_thread(thread).await {
        let t = state.host().enrich(t);
        state.bus.emit(Event::ThreadUpdated { thread: t });
    }
}

async fn handle_engine_event(state: &Arc<AppState>, ev: EngineEvent) {
    let storage = &state.storage;
    match ev {
        EngineEvent::Session {
            thread, session_id, ..
        } => {
            let _ = storage.set_thread_session(thread, Some(&session_id)).await;
            emit_thread_updated(state, thread).await;
        }
        EngineEvent::Sent {
            thread,
            turn_id,
            mid_turn,
        } => {
            state.bus.emit(Event::MessageAccepted {
                thread_id: thread,
                message_id: will_core::MessageId(turn_id.0),
                mid_turn,
            });
            emit_thread_updated(state, thread).await;
        }
        EngineEvent::Accepted { .. } => {}
        EngineEvent::TurnStarted { thread, turn_id } => {
            let _ = storage
                .set_thread_status(thread, ThreadStatus::Running)
                .await;
            state.bus.emit(Event::TurnStarted {
                thread_id: thread,
                turn_id,
            });
        }
        EngineEvent::Delta {
            thread,
            turn_id,
            delta,
        } => {
            if let Some(tid) = turn_id {
                let kind = match &delta {
                    EngineDelta::Narration { .. } => Some("narration"),
                    EngineDelta::ToolUse { .. } => Some("tool_use"),
                    EngineDelta::ToolResult { .. } => Some("tool_result"),
                    EngineDelta::Status { .. } => Some("status"),
                    EngineDelta::Thinking { .. } => Some("thinking"),
                    EngineDelta::TextDelta { .. } => None,
                };
                if let Some(k) = kind {
                    let _ = storage
                        .add_turn_event(tid, k, &serde_json::to_value(&delta).unwrap_or_default())
                        .await;
                }
            }
            state.bus.emit(Event::TurnDelta {
                thread_id: thread,
                turn_id,
                delta,
            });
        }
        EngineEvent::TurnFinished {
            thread,
            turn_id,
            merged,
            result,
        } => {
            let _ = storage.finish_turn(turn_id, &result, None).await;
            for m in &merged {
                // Turno mesclado: o texto guarda só o id do turno que o
                // absorveu (a UI agrupa os balões; `error_kind = "merged"`).
                let merged_res = TurnResult {
                    text: turn_id.to_string(),
                    is_error: false,
                    cost_usd: None,
                    duration_ms: None,
                    model: None,
                    session_id: None,
                    usage: None,
                };
                let _ = storage.finish_turn(*m, &merged_res, Some("merged")).await;
            }
            if let Some(c) = result.cost_usd {
                let _ = storage.add_thread_cost(thread, c).await;
                let rec = will_storage::costs::NewAiCost {
                    source: "claude".into(),
                    r#ref: Some(format!("{thread}/{turn_id}")),
                    model: result.model.clone(),
                    units: result.usage.clone().unwrap_or(serde_json::Value::Null),
                    usd: c,
                    estimated: false,
                    duration_secs: result.duration_ms.map(|d| (d / 1000) as i64),
                    note: None,
                };
                let _ = storage.add_ai_cost(&rec).await;
                state.bus.emit(Event::CostRecorded {
                    source: "claude".into(),
                    usd: c,
                    estimated: false,
                    summary: format!("Turno do Claude: US$ {c:.4}"),
                });
            }
            if let Some(sid) = &result.session_id {
                let _ = storage.set_thread_session(thread, Some(sid)).await;
            }
            let _ = storage.set_thread_status(thread, ThreadStatus::Live).await;
            state.bus.emit(Event::TurnFinished {
                thread_id: thread,
                turn_id,
                result,
            });
            emit_thread_updated(state, thread).await;
        }
        EngineEvent::Dropped {
            thread,
            turn_id,
            reason,
        } => {
            let res = TurnResult {
                text: reason.clone(),
                is_error: true,
                cost_usd: None,
                duration_ms: None,
                model: None,
                session_id: None,
                usage: None,
            };
            let _ = storage.finish_turn(turn_id, &res, Some("dropped")).await;
            let _ = storage.set_thread_status(thread, ThreadStatus::Idle).await;
            state.bus.emit(Event::TurnFailed { thread_id: thread, turn_id: Some(turn_id), error: format!("A instância do Claude reiniciou no meio do processamento ({reason}). Manda de novo."), kind: "dropped".into() });
            emit_thread_updated(state, thread).await;
        }
        EngineEvent::StaleSession { thread } => {
            let _ = storage.set_thread_session(thread, None).await;
            state.bus.emit(Event::StaleSession { thread_id: thread });
        }
        EngineEvent::ProcessStarted { thread, .. } => {
            // Processo subiu: motor está pronto (limpa Degraded transitório de
            // auth/rate limit). A contagem de vivos vem de `engine.stats()`.
            crate::modules::recompute_engine_status(state);
            emit_thread_updated(state, thread).await;
        }
        EngineEvent::ProcessExited {
            thread,
            code,
            stderr_tail,
        } => {
            if code.is_some_and(|c| c != 0) && !stderr_tail.trim().is_empty() {
                state.bus.log(
                    "warn",
                    "engine",
                    format!(
                        "claude saiu com código {code:?}: {}",
                        will_core::tools::truncate(&crate::logging::redact(&stderr_tail), 300)
                    ),
                );
            }
            let _ = storage.set_thread_status(thread, ThreadStatus::Idle).await;
            emit_thread_updated(state, thread).await;
        }
        EngineEvent::AuthFailed { detail, .. } => {
            state.set_module_status(will_core::Module::Engine, will_core::ModuleStatus::Degraded(format!("Autenticação falhou: {}. Gere um novo token com `claude setup-token` ou ative outro.", crate::logging::redact(&detail))));
        }
        EngineEvent::RateLimited { detail, .. } => {
            state.set_module_status(
                will_core::Module::Engine,
                will_core::ModuleStatus::Degraded(format!(
                    "Rate limit: {}. Considere trocar o token ativo.",
                    crate::logging::redact(&detail)
                )),
            );
        }
        EngineEvent::PermissionDenied { thread, tool } => {
            state.bus.log("warn", "engine", format!("thread {thread}: ação bloqueada por permissão ({tool}). Ajuste a allowlist em Motor Claude."));
        }
        EngineEvent::Retired { thread, .. } => {
            let _ = storage.set_thread_status(thread, ThreadStatus::Idle).await;
            emit_thread_updated(state, thread).await;
        }
    }
}

/// Persiste aprovações/logs e dispara notificações nativas.
pub fn spawn_bus_persistence(state: Arc<AppState>) {
    let mut rx = state.bus.subscribe();
    tokio::spawn(async move {
        loop {
            let ev = match rx.recv().await {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            match ev {
                Event::ApprovalRequested { approval } => {
                    let _ = state.storage.record_approval_request(&approval).await;
                    if state.settings.read().general.notifications && will_platform::has_desktop() {
                        will_platform::notify("will-ia: aprovação necessária", &approval.summary);
                    }
                }
                Event::ApprovalDecided {
                    approval_id,
                    decision,
                } => {
                    // decisão por timeout não passa pelo decide(): registra aqui
                    if decision == ApprovalDecision::Timeout {
                        let _ = state
                            .storage
                            .record_approval_decision(approval_id, decision, "timeout")
                            .await;
                    }
                }
                Event::Notification { title, body } => {
                    // No servidor só faz sentido com desktop; os apps conectados
                    // recebem o evento pelo WS e notificam na máquina deles.
                    if state.settings.read().general.notifications && will_platform::has_desktop() {
                        will_platform::notify(&title, &body);
                    }
                }
                Event::Log {
                    level,
                    module,
                    message,
                    ..
                } => {
                    if level != "debug" && level != "trace" {
                        let _ = state.storage.log_event(&level, &module, &message).await;
                    }
                }
                Event::ModuleStatus { module, status } => {
                    let _ = state
                        .storage
                        .log_event("info", module.as_str(), &format!("status: {status:?}"))
                        .await;
                }
                _ => {}
            }
        }
    });
}
