//! Pool de runners: um por thread, teto de vivos, recolhimento de ociosos,
//! troca de configuração (token) sem downtime.

use crate::config::EngineConfig;
use crate::runner::{Runner, RunnerCommand, RunnerHandle, SendRequest, ThreadEnv};
use crate::{EngineError, EngineEvent};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use will_core::ThreadId;

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunnerStats {
    pub thread: ThreadId,
    pub alive: bool,
    pub pending: usize,
    pub idle_secs: Option<u64>,
    pub session_id: Option<String>,
}

pub struct EnginePool {
    cfg: RwLock<EngineConfig>,
    runners: RwLock<HashMap<ThreadId, RunnerHandle>>,
    events: broadcast::Sender<EngineEvent>,
}

impl EnginePool {
    pub fn new(cfg: EngineConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(4096);
        let pool = Arc::new(Self {
            cfg: RwLock::new(cfg),
            runners: RwLock::new(HashMap::new()),
            events,
        });
        let sweeper = pool.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tick.tick().await;
                sweeper.sweep_idle();
            }
        });
        pool
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    pub fn config(&self) -> EngineConfig {
        self.cfg.read().clone()
    }

    /// Garante um runner (ator) para a thread. Não spawna o processo ainda.
    fn ensure(
        &self,
        thread: ThreadId,
        session_id: Option<String>,
        thread_env: ThreadEnv,
    ) -> RunnerHandle {
        if let Some(h) = self.runners.read().get(&thread) {
            let _ = h.command(RunnerCommand::SetThreadEnv(thread_env));
            return h.clone();
        }
        let cfg = self.cfg.read().clone();
        let h = Runner::spawn(thread, cfg, session_id, thread_env, self.events.clone());
        self.runners.write().insert(thread, h.clone());
        h
    }

    /// Entrega a mensagem na hora. `session_id` é o salvo no banco (para --resume);
    /// `thread_env` são variáveis específicas da thread (bearer MCP etc.).
    pub fn send(
        &self,
        thread: ThreadId,
        session_id: Option<String>,
        thread_env: ThreadEnv,
        req: SendRequest,
    ) -> Result<(), EngineError> {
        let exists = self.runners.read().contains_key(&thread);
        if !exists {
            self.make_room_for(thread);
        }
        let h = self.ensure(thread, session_id, thread_env);
        h.send(req)
    }

    pub fn restart(&self, thread: ThreadId, reason: &str) {
        if let Some(h) = self.runners.read().get(&thread) {
            let _ = h.command(RunnerCommand::Restart {
                reason: reason.to_string(),
            });
        }
    }

    pub fn reset(&self, thread: ThreadId) {
        if let Some(h) = self.runners.read().get(&thread) {
            let _ = h.command(RunnerCommand::Reset);
        }
    }

    pub fn interrupt(&self, thread: ThreadId) {
        if let Some(h) = self.runners.read().get(&thread) {
            let _ = h.command(RunnerCommand::Interrupt);
        }
    }

    /// Remove o runner (mata o processo). A thread some do pool.
    pub async fn forget(&self, thread: ThreadId) {
        let h = self.runners.write().remove(&thread);
        if let Some(h) = h {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = h.command(RunnerCommand::Shutdown(tx));
            let _ = tokio::time::timeout(Duration::from_secs(10), rx).await;
        }
    }

    /// Atualiza a configuração sem derrubar processos (vale no próximo spawn).
    pub fn set_config(&self, cfg: EngineConfig) {
        *self.cfg.write() = cfg.clone();
        for h in self.runners.read().values() {
            let _ = h.command(RunnerCommand::Reconfigure(Box::new(cfg.clone())).soft());
        }
    }

    /// Nova configuração para todos. Derruba os vivos; voltam com --resume. Retorna quantos.
    pub fn reconfigure(&self, cfg: EngineConfig) -> usize {
        *self.cfg.write() = cfg.clone();
        let mut n = 0;
        for h in self.runners.read().values() {
            if h.alive() {
                n += 1;
            }
            let _ = h.command(RunnerCommand::Reconfigure(Box::new(cfg.clone())));
        }
        n
    }

    pub fn stats(&self) -> Vec<RunnerStats> {
        self.runners
            .read()
            .values()
            .map(|h| RunnerStats {
                thread: h.thread,
                alive: h.alive(),
                pending: h.pending(),
                idle_secs: h.idle_for().map(|d| d.as_secs()),
                session_id: h.session_id(),
            })
            .collect()
    }

    pub fn is_alive(&self, thread: ThreadId) -> bool {
        self.runners
            .read()
            .get(&thread)
            .map(|h| h.alive())
            .unwrap_or(false)
    }

    pub fn pending(&self, thread: ThreadId) -> usize {
        self.runners
            .read()
            .get(&thread)
            .map(|h| h.pending())
            .unwrap_or(0)
    }

    fn make_room_for(&self, thread: ThreadId) {
        let max = self.cfg.read().max_live.max(1);
        let runners = self.runners.read();
        let alive: Vec<&RunnerHandle> = runners
            .values()
            .filter(|h| h.alive() && h.thread != thread)
            .collect();
        if alive.len() < max {
            return;
        }
        let mut idle: Vec<&RunnerHandle> = alive.into_iter().filter(|h| h.pending() == 0).collect();
        idle.sort_by_key(|h| std::cmp::Reverse(h.idle_for().unwrap_or(Duration::ZERO)));
        if let Some(victim) = idle.first() {
            tracing::info!(victim = %victim.thread, "teto de processos: recolhendo ociosa");
            let _ = victim.command(RunnerCommand::Restart {
                reason: "teto de processos".into(),
            });
        } else {
            tracing::warn!("teto de processos atingido e todas ocupadas; spawnando mesmo assim");
        }
    }

    fn sweep_idle(&self) {
        let idle_timeout = self.cfg.read().idle_timeout;
        for h in self.runners.read().values() {
            if h.alive() && h.pending() == 0 {
                if let Some(d) = h.idle_for() {
                    if d > idle_timeout {
                        tracing::info!(thread = %h.thread, idle_secs = d.as_secs(), "ociosa: recolhendo");
                        let _ = h.command(RunnerCommand::Restart {
                            reason: "ociosidade".into(),
                        });
                    }
                }
            }
        }
    }

    pub async fn shutdown(&self) {
        let handles: Vec<RunnerHandle> = self.runners.write().drain().map(|(_, h)| h).collect();
        for h in handles {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = h.command(RunnerCommand::Shutdown(tx));
            let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
        }
    }
}
