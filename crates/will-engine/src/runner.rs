//! Um `claude -p` vivo por thread, como ator: comandos entram por canal, eventos saem por broadcast.

use crate::config::EngineConfig;
use crate::parser::{
    deltas_from_assistant, interrupt_line, parse_line, user_message_line, StreamLine,
};
use crate::{EngineError, EngineEvent};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use will_core::{EngineDelta, ThreadId, TurnId};

#[derive(Debug, Clone)]
pub struct SendRequest {
    pub turn_id: TurnId,
    pub text: String,
}

/// Variáveis específicas da thread (bearer MCP, WILL_IA_THREAD_ID…), mescladas ao env base no spawn.
pub type ThreadEnv = std::collections::BTreeMap<String, String>;

impl RunnerCommand {
    /// Converte um Reconfigure em ReconfigureSoft.
    pub fn soft(self) -> Self {
        match self {
            RunnerCommand::Reconfigure(c) => RunnerCommand::ReconfigureSoft(c),
            other => other,
        }
    }
}

#[derive(Debug)]
pub enum RunnerCommand {
    Send(SendRequest),
    /// Mata o processo mantendo a sessão (volta com --resume).
    Restart {
        reason: String,
    },
    /// Mata o processo e esquece a sessão.
    Reset,
    Interrupt,
    /// Nova configuração (token/flags) para o próximo spawn; derruba o processo atual.
    Reconfigure(Box<EngineConfig>),
    /// Nova configuração sem derrubar (vale no próximo spawn).
    ReconfigureSoft(Box<EngineConfig>),
    /// Env específico da thread (aplicado no próximo spawn; não derruba).
    SetThreadEnv(ThreadEnv),
    Shutdown(oneshot::Sender<()>),
}

/// Estado observável sem passar pelo ator.
#[derive(Default)]
pub struct RunnerShared {
    pub alive: AtomicBool,
    pub pending: AtomicUsize,
    pub last_used: Mutex<Option<Instant>>,
    pub session_id: Mutex<Option<String>>,
    pub in_turn: AtomicBool,
}

#[derive(Clone)]
pub struct RunnerHandle {
    pub thread: ThreadId,
    pub tx: mpsc::UnboundedSender<RunnerCommand>,
    pub shared: Arc<RunnerShared>,
}

impl RunnerHandle {
    pub fn send(&self, req: SendRequest) -> Result<(), EngineError> {
        self.tx
            .send(RunnerCommand::Send(req))
            .map_err(|_| EngineError::NoThread)
    }
    pub fn command(&self, cmd: RunnerCommand) -> Result<(), EngineError> {
        self.tx.send(cmd).map_err(|_| EngineError::NoThread)
    }
    pub fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Relaxed)
    }
    pub fn pending(&self) -> usize {
        self.shared.pending.load(Ordering::Relaxed)
    }
    pub fn idle_for(&self) -> Option<Duration> {
        self.shared.last_used.lock().map(|t| t.elapsed())
    }
    pub fn session_id(&self) -> Option<String> {
        self.shared.session_id.lock().clone()
    }
}

struct Pending {
    turn_id: TurnId,
    text: String,
    retried: bool,
    /// Ecoada pelo processo (entrou na conversa).
    accepted: bool,
    started_emitted: bool,
}

struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    stale_flag: Arc<AtomicBool>,
    pid: Option<u32>,
}

pub struct Runner {
    thread: ThreadId,
    cfg: EngineConfig,
    session_id: Option<String>,
    proc_: Option<Proc>,
    pending: VecDeque<Pending>,
    shared: Arc<RunnerShared>,
    events: broadcast::Sender<EngineEvent>,
    rx: mpsc::UnboundedReceiver<RunnerCommand>,
    raw_log: Option<std::fs::File>,
    resumed: bool,
    thread_env: ThreadEnv,
}

impl Runner {
    pub fn spawn(
        thread: ThreadId,
        cfg: EngineConfig,
        session_id: Option<String>,
        thread_env: ThreadEnv,
        events: broadcast::Sender<EngineEvent>,
    ) -> RunnerHandle {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(RunnerShared::default());
        *shared.session_id.lock() = session_id.clone();
        let handle = RunnerHandle {
            thread,
            tx,
            shared: shared.clone(),
        };
        let runner = Runner {
            thread,
            cfg,
            session_id,
            proc_: None,
            pending: VecDeque::new(),
            shared,
            events,
            rx,
            raw_log: None,
            resumed: false,
            thread_env,
        };
        tokio::spawn(runner.run());
        handle
    }

    fn emit(&self, e: EngineEvent) {
        let _ = self.events.send(e);
    }

    fn sync_shared(&self) {
        self.shared
            .alive
            .store(self.proc_.is_some(), Ordering::Relaxed);
        self.shared
            .pending
            .store(self.pending.len(), Ordering::Relaxed);
        self.shared
            .in_turn
            .store(self.pending.iter().any(|p| p.accepted), Ordering::Relaxed);
        *self.shared.session_id.lock() = self.session_id.clone();
    }

    async fn run(mut self) {
        loop {
            let has_proc = self.proc_.is_some();
            tokio::select! {
                cmd = self.rx.recv() => {
                    match cmd {
                        None => { self.kill_process("handle dropped").await; return; }
                        Some(RunnerCommand::Shutdown(ack)) => {
                            self.kill_process("shutdown").await;
                            let _ = ack.send(());
                            return;
                        }
                        Some(cmd) => self.handle_cmd(cmd).await,
                    }
                }
                line = async {
                    match self.proc_.as_mut() {
                        Some(p) => p.lines.next_line().await,
                        None => std::future::pending().await,
                    }
                }, if has_proc => {
                    match line {
                        Ok(Some(l)) => self.handle_line(&l),
                        Ok(None) | Err(_) => self.handle_exit().await,
                    }
                }
            }
            self.sync_shared();
        }
    }

    async fn handle_cmd(&mut self, cmd: RunnerCommand) {
        match cmd {
            RunnerCommand::Send(req) => {
                if self.proc_.is_none() {
                    if let Err(e) = self.spawn_process().await {
                        tracing::error!(thread = %self.thread, error = %e, "spawn falhou");
                        self.emit(EngineEvent::Dropped {
                            thread: self.thread,
                            turn_id: req.turn_id,
                            reason: e.to_string(),
                        });
                        return;
                    }
                }
                let mid_turn = self.pending.iter().any(|p| p.accepted);
                let line = user_message_line(&req.text);
                // A pendência entra ANTES da escrita: se o processo morreu entre o spawn e o write
                // (ex.: --resume de sessão expirada sai em milissegundos), o handle_exit decide
                // entre reenviar (stale) e descartar (Dropped).
                self.pending.push_back(Pending {
                    turn_id: req.turn_id,
                    text: req.text,
                    retried: false,
                    accepted: false,
                    started_emitted: false,
                });
                *self.shared.last_used.lock() = Some(Instant::now());
                if let Err(e) = self.write_stdin(&line).await {
                    tracing::warn!(thread = %self.thread, error = %e, "stdin falhou; aguardando saída do processo");
                    let exited = self
                        .proc_
                        .as_mut()
                        .and_then(|p| p.child.try_wait().ok().flatten())
                        .is_some();
                    if !exited {
                        if let Some(p) = self.proc_.as_mut() {
                            let _ = p.child.start_kill();
                        }
                    }
                    return;
                }
                self.emit(EngineEvent::Sent {
                    thread: self.thread,
                    turn_id: req.turn_id,
                    mid_turn,
                });
            }
            RunnerCommand::Restart { reason } => {
                self.kill_process(&reason).await;
                self.emit(EngineEvent::Retired {
                    thread: self.thread,
                    reason,
                });
            }
            RunnerCommand::Reset => {
                self.kill_process("reset").await;
                self.session_id = None;
                self.emit(EngineEvent::Retired {
                    thread: self.thread,
                    reason: "reset".into(),
                });
            }
            RunnerCommand::Interrupt => {
                if self.proc_.is_some() {
                    let _ = self.write_stdin(&interrupt_line()).await;
                }
            }
            RunnerCommand::Reconfigure(cfg) => {
                self.cfg = *cfg;
                if self.proc_.is_some() {
                    self.kill_process("reconfigure").await;
                    self.emit(EngineEvent::Retired {
                        thread: self.thread,
                        reason: "reconfigure".into(),
                    });
                }
            }
            RunnerCommand::ReconfigureSoft(cfg) => {
                self.cfg = *cfg;
            }
            RunnerCommand::SetThreadEnv(env) => {
                self.thread_env = env;
            }
            RunnerCommand::Shutdown(_) => {}
        }
    }

    async fn write_stdin(&mut self, line: &str) -> Result<(), EngineError> {
        let p = self.proc_.as_mut().ok_or(EngineError::NoThread)?;
        let stdin = p
            .stdin
            .as_mut()
            .ok_or_else(|| EngineError::Stdin("stdin fechado".into()))?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| EngineError::Stdin(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| EngineError::Stdin(e.to_string()))?;
        Ok(())
    }

    async fn spawn_process(&mut self) -> Result<(), EngineError> {
        let new_id = uuid::Uuid::new_v4().to_string();
        let args = self.cfg.args(self.session_id.as_deref(), &new_id);
        self.resumed = self.session_id.is_some();
        let mut cmd = Command::new(&self.cfg.claude_bin);
        cmd.args(&args)
            .current_dir(&self.cfg.cwd)
            .env_clear()
            .envs(&self.cfg.env)
            .envs(&self.thread_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                EngineError::BinaryNotFound(self.cfg.claude_bin.to_string_lossy().to_string())
            } else {
                EngineError::Spawn(e.to_string())
            }
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Spawn("sem stdout".into()))?;
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let pid = child.id();
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stale = Arc::new(AtomicBool::new(false));
        if let Some(err) = stderr {
            let tail2 = tail.clone();
            let stale2 = stale.clone();
            let thread = self.thread;
            let events = self.events.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::debug!(thread = %thread, "stderr: {l}");
                    if l.contains("No conversation found with session ID") {
                        stale2.store(true, Ordering::Relaxed);
                    }
                    let lower = l.to_lowercase();
                    if lower.contains("not logged in")
                        || lower.contains("authentication") && lower.contains("fail")
                        || lower.contains("invalid api key")
                        || lower.contains("oauth token")
                            && (lower.contains("expired") || lower.contains("invalid"))
                        || lower.contains("401")
                    {
                        let _ = events.send(EngineEvent::AuthFailed {
                            thread,
                            detail: will_core::tools::truncate(&l, 200),
                        });
                    }
                    if lower.contains("rate limit")
                        || lower.contains("limit reached")
                        || lower.contains("quota")
                        || lower.contains("429")
                    {
                        let _ = events.send(EngineEvent::RateLimited {
                            thread,
                            detail: will_core::tools::truncate(&l, 200),
                        });
                    }
                    let mut t = tail2.lock();
                    if t.len() >= 30 {
                        t.pop_front();
                    }
                    t.push_back(l);
                }
            });
        }
        if let Some(dir) = &self.cfg.raw_log_dir {
            let _ = std::fs::create_dir_all(dir);
            let name = format!("claude-{}.log", chrono::Utc::now().format("%Y-%m-%d"));
            self.raw_log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(name))
                .ok();
        }
        self.proc_ = Some(Proc {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr_tail: tail,
            stale_flag: stale,
            pid,
        });
        self.emit(EngineEvent::ProcessStarted {
            thread: self.thread,
            pid,
        });
        tracing::info!(thread = %self.thread, resumed = self.resumed, pid = ?pid, "claude spawn");
        Ok(())
    }

    fn handle_line(&mut self, line: &str) {
        if let Some(f) = self.raw_log.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
        match parse_line(line) {
            StreamLine::Init {
                session_id,
                mcp_servers,
                ..
            } => {
                if !session_id.is_empty() && self.session_id.as_deref() != Some(&session_id) {
                    self.session_id = Some(session_id.clone());
                }
                for (name, status) in &mcp_servers {
                    if name == "will_ia" && status != "connected" {
                        tracing::warn!(thread = %self.thread, status, "MCP will_ia não conectado");
                    }
                }
                self.emit(EngineEvent::Session {
                    thread: self.thread,
                    session_id: self.session_id.clone().unwrap_or_default(),
                    resumed: self.resumed,
                });
            }
            StreamLine::UserReplay { text } => {
                // Marca como aceita a primeira pendência não aceita (preferindo texto igual).
                let idx = self
                    .pending
                    .iter()
                    .position(|p| !p.accepted && p.text == text)
                    .or_else(|| self.pending.iter().position(|p| !p.accepted));
                if let Some(i) = idx {
                    let mid_turn = self.pending.iter().take(i).any(|p| p.accepted);
                    let p = &mut self.pending[i];
                    p.accepted = true;
                    let tid = p.turn_id;
                    self.emit(EngineEvent::Accepted {
                        thread: self.thread,
                        turn_id: tid,
                        mid_turn,
                    });
                }
            }
            StreamLine::Assistant {
                narration,
                tool_uses,
                thinking,
            } => {
                self.mark_started();
                let tid = self.current_turn();
                for d in deltas_from_assistant(&narration, &tool_uses, &thinking) {
                    self.emit(EngineEvent::Delta {
                        thread: self.thread,
                        turn_id: tid,
                        delta: d,
                    });
                }
            }
            StreamLine::TextDelta { text } => {
                self.mark_started();
                let tid = self.current_turn();
                self.emit(EngineEvent::Delta {
                    thread: self.thread,
                    turn_id: tid,
                    delta: EngineDelta::TextDelta { text },
                });
            }
            StreamLine::ToolResult {
                preview,
                is_error,
                permission_denied,
            } => {
                let tid = self.current_turn();
                if let Some(tool) = permission_denied {
                    self.emit(EngineEvent::PermissionDenied {
                        thread: self.thread,
                        tool,
                    });
                }
                self.emit(EngineEvent::Delta {
                    thread: self.thread,
                    turn_id: tid,
                    delta: EngineDelta::ToolResult { preview, is_error },
                });
            }
            StreamLine::Status { text } => {
                let tid = self.current_turn();
                if !text.is_empty() {
                    self.emit(EngineEvent::Delta {
                        thread: self.thread,
                        turn_id: tid,
                        delta: EngineDelta::Status { text },
                    });
                }
            }
            StreamLine::RateLimit { detail } => {
                self.emit(EngineEvent::RateLimited {
                    thread: self.thread,
                    detail,
                });
            }
            StreamLine::Result {
                result,
                stale_session,
            } => {
                if stale_session {
                    if let Some(p) = self.proc_.as_ref() {
                        p.stale_flag.store(true, Ordering::Relaxed);
                    }
                    return; // o exit resolve
                }
                if let Some(sid) = &result.session_id {
                    if !sid.is_empty() {
                        self.session_id = Some(sid.clone());
                    }
                }
                // Fecha a cabeça + todas já aceitas (consumidas neste turno).
                let Some(head) = self.pending.pop_front() else {
                    tracing::debug!(thread = %self.thread, "result sem pendência");
                    return;
                };
                let mut merged = vec![];
                while let Some(p) = self.pending.front() {
                    if p.accepted {
                        merged.push(
                            self.pending
                                .pop_front()
                                .map(|p| p.turn_id)
                                .unwrap_or_default(),
                        );
                    } else {
                        break;
                    }
                }
                *self.shared.last_used.lock() = Some(Instant::now());
                self.emit(EngineEvent::TurnFinished {
                    thread: self.thread,
                    turn_id: head.turn_id,
                    merged,
                    result,
                });
            }
            StreamLine::Other => {}
        }
    }

    fn mark_started(&mut self) {
        // Só a cabeça da fila pode "começar": atividade do assistant pertence ao turno corrente.
        // Mensagens posteriores só entram quando o processo as ecoa (replay).
        if let Some(p) = self.pending.front_mut() {
            if !p.started_emitted {
                p.started_emitted = true;
                // Sem replay (versões antigas), considerar aceita ao ver atividade.
                p.accepted = true;
                let tid = p.turn_id;
                self.emit(EngineEvent::TurnStarted {
                    thread: self.thread,
                    turn_id: tid,
                });
            }
        }
    }

    fn current_turn(&self) -> Option<TurnId> {
        self.pending.front().map(|p| p.turn_id)
    }

    async fn handle_exit(&mut self) {
        let Some(mut p) = self.proc_.take() else {
            return;
        };
        let status = tokio::time::timeout(Duration::from_secs(2), p.child.wait())
            .await
            .ok()
            .and_then(|r| r.ok());
        let code = status.and_then(|s| s.code());
        let stale = p.stale_flag.load(Ordering::Relaxed);
        let tail: Vec<String> = p.stderr_tail.lock().iter().cloned().collect();
        self.raw_log = None;
        tracing::warn!(thread = %self.thread, code = ?code, stale, "claude encerrou");
        self.emit(EngineEvent::ProcessExited {
            thread: self.thread,
            code,
            stderr_tail: tail.join("\n"),
        });

        if stale && !self.pending.is_empty() && self.pending.iter().all(|q| !q.retried) {
            // Sessão expirada: esquece, sobe do zero e reenvia (uma vez).
            self.session_id = None;
            self.emit(EngineEvent::StaleSession {
                thread: self.thread,
            });
            let msgs: Vec<SendRequest> = self
                .pending
                .drain(..)
                .map(|q| SendRequest {
                    turn_id: q.turn_id,
                    text: q.text,
                })
                .collect();
            if self.spawn_process().await.is_ok() {
                for m in msgs {
                    let line = user_message_line(&m.text);
                    if self.write_stdin(&line).await.is_ok() {
                        self.pending.push_back(Pending {
                            turn_id: m.turn_id,
                            text: m.text,
                            retried: true,
                            accepted: false,
                            started_emitted: false,
                        });
                    } else {
                        self.emit(EngineEvent::Dropped {
                            thread: self.thread,
                            turn_id: m.turn_id,
                            reason: "reenvio falhou".into(),
                        });
                    }
                }
            } else {
                for m in msgs {
                    self.emit(EngineEvent::Dropped {
                        thread: self.thread,
                        turn_id: m.turn_id,
                        reason: "respawn falhou".into(),
                    });
                }
            }
            return;
        }
        let reason = if code == Some(0) {
            "processo encerrou".to_string()
        } else {
            format!("processo caiu (code {code:?})")
        };
        while let Some(q) = self.pending.pop_front() {
            self.emit(EngineEvent::Dropped {
                thread: self.thread,
                turn_id: q.turn_id,
                reason: reason.clone(),
            });
        }
    }

    async fn kill_process(&mut self, reason: &str) {
        let Some(mut p) = self.proc_.take() else {
            return;
        };
        tracing::info!(thread = %self.thread, reason, pid = ?p.pid, "encerrando claude");
        drop(p.stdin.take());
        kill_tree(&mut p.child, p.pid, self.cfg.kill_grace).await;
        self.raw_log = None;
        // Pendências morrem com o processo (quem pediu restart sabe disso).
        let reason_s = format!("thread reiniciada: {reason}");
        while let Some(q) = self.pending.pop_front() {
            self.emit(EngineEvent::Dropped {
                thread: self.thread,
                turn_id: q.turn_id,
                reason: reason_s.clone(),
            });
        }
    }
}

/// SIGTERM no grupo → espera → SIGKILL. No Windows, taskkill /T.
pub async fn kill_tree(child: &mut Child, pid: Option<u32>, grace: Duration) {
    #[cfg(unix)]
    {
        if let Some(pid) = pid {
            let pg = nix::unistd::Pid::from_raw(pid as i32);
            let _ = nix::sys::signal::killpg(pg, nix::sys::signal::Signal::SIGTERM);
            if tokio::time::timeout(grace, child.wait()).await.is_ok() {
                return;
            }
            let _ = nix::sys::signal::killpg(pg, nix::sys::signal::Signal::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        // No Windows o taskkill /T /F já é imediato; não há período de graça.
        let _ = grace;
        if let Some(pid) = pid {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output()
                .await;
        }
    }
    let _ = child.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}
