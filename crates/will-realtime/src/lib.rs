//! will-realtime — sessão de voz com a OpenAI Realtime API e handoff para o
//! cérebro (Claude) via `will_core::BrainHost`.
//!
//! O áudio é capturado/reproduzido na UI (web/Tauri) e chega aqui como PCM16
//! mono 24 kHz pelo `will-server`; esta crate só fala WebSocket com a OpenAI.
//!
//! Estados emitidos em `Event::Voice{state}`: `connecting`, `listening`,
//! `user_speaking`, `thinking`, `speaking`, `brain_working`, `stopped`, `error`.

pub mod audio;
pub mod protocol;
mod session;
pub mod speech;

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
pub use session::SessionUsage;
use session::{Cmd, PendingMap, PendingTask, SessionHandle, SessionRunner};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use will_core::settings::RealtimeSettings;
use will_core::{BrainHost, Event, EventBus, Module, ModuleStatus, ThreadId, TurnId};

pub use protocol::SessionVariant;
pub use speech::clean_for_speech;

/// Endpoint padrão da OpenAI Realtime.
pub const DEFAULT_ENDPOINT: &str = "wss://api.openai.com/v1/realtime";
/// Capacidade do broadcast de áudio: respostas longas chegam em rajadas de centenas
/// de deltas; um consumidor que atrase além disso perderia áudio (`Lagged`).
const AUDIO_OUT_CAPACITY: usize = 16384;
const NOTIFICATION_BODY_CHARS: usize = 200;

/// O que sai da sessão em direção à UI.
#[derive(Debug, Clone)]
pub enum AudioOut {
    /// PCM16 mono 24 kHz do assistente.
    Pcm(Bytes),
    /// Mensagem de controle JSON (`stop_playback`, `state`, `transcript`).
    Control(Value),
}

/// Dependências injetadas pelo will-server.
#[derive(Clone)]
pub struct RealtimeDeps {
    pub host: Arc<dyn BrainHost>,
    pub bus: EventBus,
    pub storage: will_storage::Storage,
}

#[derive(Clone)]
struct Config {
    settings: RealtimeSettings,
    api_key: Option<SecretString>,
    endpoint: String,
}

struct VoiceState {
    state: String,
    detail: Option<String>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    last_error: Option<String>,
}

/// Gerencia a sessão de voz (uma por vez).
pub struct RealtimeManager {
    pub(crate) deps: RealtimeDeps,
    config: RwLock<Config>,
    session: Mutex<Option<Arc<SessionHandle>>>,
    voice: Mutex<VoiceState>,
    pending: Mutex<PendingMap>,
    audio_out: broadcast::Sender<AudioOut>,
    ptt_down: AtomicBool,
    listener_spawned: AtomicBool,
    /// Resumo da última sessão encerrada (tokens, custo estimado, duração).
    last_session: Mutex<Option<Value>>,
    next_session_id: AtomicU64,
}

impl RealtimeManager {
    pub fn new(
        deps: RealtimeDeps,
        settings: RealtimeSettings,
        api_key: Option<SecretString>,
    ) -> Arc<Self> {
        let (audio_out, _) = broadcast::channel(AUDIO_OUT_CAPACITY);
        Arc::new(Self {
            deps,
            config: RwLock::new(Config {
                settings,
                api_key,
                endpoint: DEFAULT_ENDPOINT.to_string(),
            }),
            session: Mutex::new(None),
            voice: Mutex::new(VoiceState {
                state: "stopped".into(),
                detail: None,
                since: None,
                last_error: None,
            }),
            pending: Mutex::new(PendingMap::new()),
            audio_out,
            ptt_down: AtomicBool::new(false),
            listener_spawned: AtomicBool::new(false),
            last_session: Mutex::new(None),
            next_session_id: AtomicU64::new(1),
        })
    }

    /// Troca settings/chave. Uma sessão aberta continua com a configuração antiga
    /// até ser reiniciada.
    pub fn update(&self, settings: RealtimeSettings, api_key: Option<SecretString>) {
        let mut cfg = self.config.write();
        cfg.settings = settings;
        cfg.api_key = api_key;
        self.voice.lock().last_error = None;
    }

    /// Endpoint WebSocket (`wss://.../v1/realtime`); útil para testes com servidor mock.
    pub fn set_endpoint(&self, endpoint: impl Into<String>) {
        self.config.write().endpoint = endpoint.into();
    }

    /// Abre a sessão. `Err` se o módulo está desabilitado, sem chave, já ativo ou
    /// se a conexão falhar.
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        let cfg = self.config.read().clone();
        if !cfg.settings.enabled {
            return Err("voz desabilitada nas configurações".into());
        }
        let Some(key) = cfg
            .api_key
            .as_ref()
            .map(|k| k.expose_secret().trim().to_string())
            .filter(|k| !k.is_empty())
        else {
            return Err("API key da OpenAI não configurada".into());
        };
        if self.is_active() {
            return Err("sessão de voz já ativa".into());
        }
        self.spawn_brain_listener();

        let id = self.next_session_id.fetch_add(1, Ordering::SeqCst);
        let runner = Arc::new(SessionRunner::new(
            id,
            Arc::clone(self),
            cfg.settings.clone(),
            key,
            cfg.endpoint.clone(),
        ));
        self.set_state("connecting", None);
        let ws = match runner.connect().await {
            Ok(ws) => ws,
            Err(e) => {
                self.set_state("error", Some(e.clone()));
                self.voice.lock().last_error = Some(e.clone());
                self.set_state("stopped", None);
                self.deps
                    .bus
                    .log("error", "realtime", format!("falha ao abrir sessão: {e}"));
                return Err(e);
            }
        };

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let handle = Arc::new(SessionHandle {
            id,
            cmd_tx,
            cancel: cancel.clone(),
            task: Mutex::new(None),
        });
        {
            let mut slot = self.session.lock();
            if slot.is_some() {
                return Err("sessão de voz já ativa".into());
            }
            *slot = Some(Arc::clone(&handle));
        }
        {
            let mut v = self.voice.lock();
            v.since = Some(chrono::Utc::now());
            v.last_error = None;
        }
        self.ptt_down.store(false, Ordering::SeqCst);
        let task = tokio::spawn(runner.run(ws, cmd_rx, cancel));
        *handle.task.lock() = Some(task);
        self.deps.bus.log(
            "info",
            "realtime",
            format!("sessão de voz aberta ({})", cfg.settings.model),
        );
        self.publish_module_status();
        Ok(())
    }

    /// Encerra a sessão (no-op se inativa).
    pub async fn stop(&self) {
        let handle = self.session.lock().take();
        let Some(handle) = handle else { return };
        handle.cancel.cancel();
        let task = handle.task.lock().take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
        }
        self.ptt_down.store(false, Ordering::SeqCst);
        self.audio_control(json!({"type": "stop_playback"}));
        self.set_state("stopped", None);
        self.voice.lock().since = None;
        self.deps
            .bus
            .log("info", "realtime", "sessão de voz encerrada");
        self.publish_module_status();
    }

    /// Alterna a sessão; devolve o novo estado (ativa?).
    pub async fn toggle(self: &Arc<Self>) -> Result<bool, String> {
        if self.is_active() {
            self.stop().await;
            Ok(false)
        } else {
            self.start().await?;
            Ok(true)
        }
    }

    pub fn is_active(&self) -> bool {
        self.session.lock().is_some()
    }

    /// `{active, state, since, pending_tasks, model, activation}`.
    pub fn status(&self) -> Value {
        let cfg = self.config.read();
        let v = self.voice.lock();
        let pending: Vec<Value> = self
            .pending
            .lock()
            .iter()
            .map(|(id, p)| json!({"task_id": id.to_string(), "thread_id": p.thread_id.to_string(), "started_at": p.started_at}))
            .collect();
        json!({
            "active": self.is_active(),
            "state": v.state,
            "detail": v.detail,
            "since": v.since,
            "pending_tasks": pending,
            "model": cfg.settings.model,
            "activation": cfg.settings.activation,
            "enabled": cfg.settings.enabled,
            "has_api_key": cfg.api_key.is_some(),
            "last_session": self.last_session.lock().clone(),
        })
    }

    /// Áudio PCM16 24 kHz vindo da UI. Ignorado sem sessão; em PTT só com a tecla pressionada.
    pub fn audio_in(&self, pcm16: Bytes) {
        if pcm16.is_empty() {
            return;
        }
        let session = self.session.lock().clone();
        let Some(h) = session else { return };
        if self.is_ptt() && !self.ptt_down.load(Ordering::SeqCst) {
            return;
        }
        h.send(Cmd::Audio(pcm16));
    }

    /// Push-to-talk: pressionar limpa o buffer; soltar faz commit + pede resposta.
    pub fn ptt(&self, down: bool) {
        let was = self.ptt_down.swap(down, Ordering::SeqCst);
        if was == down {
            return;
        }
        let session = self.session.lock().clone();
        if let Some(h) = session {
            h.send(Cmd::Ptt(down));
        }
    }

    /// Fim manual da fala do usuário (commit + response.create).
    pub fn commit(&self) {
        let session = self.session.lock().clone();
        if let Some(h) = session {
            h.send(Cmd::Commit);
        }
    }

    pub fn subscribe_audio_out(&self) -> broadcast::Receiver<AudioOut> {
        self.audio_out.subscribe()
    }

    /// `Running` = pronta (habilitada + chave da OpenAI), em sessão ou ociosa;
    /// `Degraded` = falta chave ou a última sessão falhou; `Disabled` = desligada.
    /// Se há sessão ativa é `status().active` / `is_active()`, não o `ModuleStatus`.
    pub fn module_status(&self) -> ModuleStatus {
        let cfg = self.config.read();
        if !cfg.settings.enabled {
            return ModuleStatus::Disabled;
        }
        if cfg.api_key.is_none() {
            return ModuleStatus::Degraded("sem chave da OpenAI".into());
        }
        let v = self.voice.lock();
        if let Some(err) = &v.last_error {
            return ModuleStatus::Degraded(err.clone());
        }
        ModuleStatus::Running
    }

    // ----- interno -----------------------------------------------------------

    fn is_ptt(&self) -> bool {
        protocol::is_push_to_talk(&self.config.read().settings)
    }

    fn publish_module_status(&self) {
        self.deps.bus.emit(Event::ModuleStatus {
            module: Module::Realtime,
            status: self.module_status(),
        });
    }

    pub(crate) fn set_state(&self, state: &str, detail: Option<String>) {
        {
            let mut v = self.voice.lock();
            if v.state == state && v.detail == detail {
                return;
            }
            v.state = state.to_string();
            v.detail = detail.clone();
        }
        tracing::debug!(state, detail = ?detail, "realtime: estado");
        self.deps.bus.emit(Event::Voice {
            state: state.to_string(),
            detail: detail.clone(),
        });
        self.audio_control(json!({"type": "state", "state": state, "detail": detail}));
    }

    pub(crate) fn audio_pcm(&self, pcm: Bytes) {
        let _ = self.audio_out.send(AudioOut::Pcm(pcm));
    }

    pub(crate) fn audio_control(&self, value: Value) {
        let _ = self.audio_out.send(AudioOut::Control(value));
    }

    pub(crate) fn track_pending(&self, turn_id: TurnId, thread_id: ThreadId) {
        self.pending.lock().insert(
            turn_id,
            PendingTask {
                thread_id,
                started_at: chrono::Utc::now(),
            },
        );
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    /// Fecha a contabilidade de uma sessão: grava em `ai_costs`, emite `CostRecorded`
    /// e guarda o resumo para a UI.
    pub(crate) fn session_finished(&self, id: u64, usage: SessionUsage, duration: Duration) {
        let pricing = self.config.read().settings.pricing.clone();
        let model = self.config.read().settings.model.clone();
        let usd = usage.usd(&pricing);
        let mins = duration.as_secs() / 60;
        let secs = duration.as_secs() % 60;
        let summary = format!(
            "Sessão de voz ({mins} min {secs} s): {} respostas · áudio {} in / {} out · texto {} in / {} out · {} em cache · ≈ US$ {:.4}",
            usage.responses,
            usage.audio_in,
            usage.audio_out,
            usage.text_in,
            usage.text_out,
            usage.text_cached + usage.audio_cached,
            usd
        );
        let units = serde_json::to_value(&usage).unwrap_or(Value::Null);
        *self.last_session.lock() = Some(json!({
            "session": id,
            "duration_secs": duration.as_secs(),
            "usage": units,
            "usd": usd,
            "summary": summary,
            "at": chrono::Utc::now(),
        }));
        if usage.responses == 0 && usage.total() == 0 {
            return;
        }
        let storage = self.deps.storage.clone();
        let rec = will_storage::costs::NewAiCost {
            source: "voice".into(),
            r#ref: Some(format!("voice-session-{id}")),
            model: Some(model),
            units,
            usd,
            estimated: true,
            duration_secs: Some(duration.as_secs() as i64),
            note: None,
        };
        tokio::spawn(async move {
            if let Err(e) = storage.add_ai_cost(&rec).await {
                tracing::warn!(error = %e, "não gravou custo da sessão de voz");
            }
        });
        self.deps.bus.log("info", "realtime", summary.clone());
        self.deps.bus.emit(Event::CostRecorded {
            source: "voice".into(),
            usd,
            estimated: true,
            summary,
        });
    }

    /// Chamado pela task da sessão ao terminar por conta própria.
    pub(crate) fn session_ended(&self, id: u64) {
        let mut slot = self.session.lock();
        if slot.as_ref().is_some_and(|h| h.id == id) {
            *slot = None;
            drop(slot);
            self.voice.lock().since = None;
            self.publish_module_status();
        }
    }

    fn spawn_brain_listener(self: &Arc<Self>) {
        if self.listener_spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak: Weak<Self> = Arc::downgrade(self);
        let mut rx = self.deps.host.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let Some(mgr) = weak.upgrade() else { break };
                        mgr.on_bus_event(event);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(n, "realtime: eventos do cérebro perdidos");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn on_bus_event(&self, event: Event) {
        match event {
            Event::TurnFinished {
                turn_id, result, ..
            } => {
                let Some(_task) = self.pending.lock().remove(&turn_id) else {
                    return;
                };
                let limit = self.config.read().settings.brain_speech_limit_chars;
                let clean = clean_for_speech(&result.text, limit);
                let speech = format!("[Resultado do cérebro para a tarefa {turn_id}]: {clean}");
                self.deliver_brain_text(speech, "O cérebro terminou", &result.text);
            }
            Event::TurnFailed {
                thread_id,
                turn_id,
                error,
                ..
            } => {
                let matched = {
                    let mut pending = self.pending.lock();
                    match turn_id {
                        Some(id) => pending.remove(&id).is_some(),
                        None => {
                            let before = pending.len();
                            pending.retain(|_, p| p.thread_id != thread_id);
                            pending.len() != before
                        }
                    }
                };
                if !matched {
                    return;
                }
                let limit = self.config.read().settings.brain_speech_limit_chars;
                let clean = clean_for_speech(&error, limit.min(400));
                let speech = format!("[O cérebro falhou: {clean}]");
                self.deliver_brain_text(speech, "O cérebro falhou", &error);
            }
            _ => {}
        }
    }

    /// Injeta na sessão aberta ou, se não houver, notifica.
    fn deliver_brain_text(&self, speech: String, title: &str, full_text: &str) {
        let delivered = {
            let session = self.session.lock().clone();
            session.is_some_and(|h| h.send(Cmd::BrainText(speech)))
        };
        if delivered {
            return;
        }
        let body: String = full_text.chars().take(NOTIFICATION_BODY_CHARS).collect();
        self.deps.bus.emit(Event::Notification {
            title: title.to_string(),
            body,
        });
        if self.pending_count() == 0 && self.voice.lock().state == "brain_working" {
            self.set_state("stopped", None);
        }
    }
}
