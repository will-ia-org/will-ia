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
use tracing::Instrument;
use will_core::{EngineDelta, ThreadId, TurnId};

#[derive(Debug, Clone)]
pub struct SendRequest {
    pub turn_id: TurnId,
    pub text: String,
    /// Quem originou o turno (`telegram`, `voice`, `api`, `schedule`, `mcp`…).
    /// Só para rastreio nos logs (`turn.source`); não muda o comportamento.
    pub source: String,
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
    source: String,
    retried: bool,
    /// Ecoada pelo processo (entrou na conversa).
    accepted: bool,
    started_emitted: bool,
    started_at: Instant,
    /// Span `turn` do turno: UM por mensagem, vivo da entrada na fila até o
    /// `result` (ou descarte). Todos os eventos do turno saem dentro dele; a
    /// duração do span é a duração real do turno.
    span: tracing::Span,
}

struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    stale_flag: Arc<AtomicBool>,
    pid: Option<u32>,
    /// Span `engine_proc` que acompanha o processo do spawn ao exit.
    span: tracing::Span,
    started_at: Instant,
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
    /// `interrupt` escrito no stdin e ainda sem `result`: o próximo `result`
    /// é do turno interrompido (marca `interrupted`, sem texto de diagnóstico).
    interrupt_sent: bool,
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
            interrupt_sent: false,
        };
        tokio::spawn(runner.run());
        handle
    }

    fn emit(&self, e: EngineEvent) {
        let _ = self.events.send(e);
    }

    /// Canal da thread (`ui`/`telegram`/`realtime`), vindo do env por thread.
    fn channel(&self) -> &str {
        self.thread_env
            .get("WILL_IA_ORIGIN")
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Span `turn`: um por mensagem enviada; eventos do turno saem dentro dele.
    fn turn_span(&self, turn_id: TurnId, source: &str) -> tracing::Span {
        tracing::info_span!(
            "turn",
            thread_id = %self.thread,
            turn_id = %turn_id,
            source,
            channel = self.channel()
        )
    }

    /// Span `engine_proc` do processo vivo (ou nenhum).
    fn proc_span(&self) -> tracing::Span {
        self.proc_
            .as_ref()
            .map(|p| p.span.clone())
            .unwrap_or_else(tracing::Span::none)
    }

    /// Span `turn` da cabeça da fila (turno corrente), se houver.
    fn current_turn_span(&self) -> tracing::Span {
        self.pending
            .front()
            .map(|p| p.span.clone())
            .unwrap_or_else(tracing::Span::none)
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
                let span = self.turn_span(req.turn_id, &req.source);
                if self.proc_.is_none() {
                    if let Err(e) = self
                        .spawn_process(Some(req.turn_id))
                        .instrument(span.clone())
                        .await
                    {
                        span.in_scope(|| {
                            tracing::error!(error = %e, "spawn do claude falhou; turno descartado")
                        });
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
                    source: req.source,
                    retried: false,
                    accepted: false,
                    started_emitted: false,
                    started_at: Instant::now(),
                    span: span.clone(),
                });
                *self.shared.last_used.lock() = Some(Instant::now());
                if let Err(e) = self.write_stdin(&line).await {
                    span.in_scope(
                        || tracing::warn!(error = %e, "stdin falhou; aguardando saída do processo"),
                    );
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
                span.in_scope(|| {
                    tracing::debug!(
                        mid_turn,
                        bytes = line.len(),
                        pending = self.pending.len(),
                        "mensagem escrita no stdin do claude"
                    )
                });
                self.emit(EngineEvent::Sent {
                    thread: self.thread,
                    turn_id: req.turn_id,
                    mid_turn,
                });
            }
            RunnerCommand::Restart { reason } => {
                tracing::info!(thread_id = %self.thread, %reason, alive = self.proc_.is_some(), "retire da thread (sessão mantida)");
                self.kill_process(&reason).await;
                self.emit(EngineEvent::Retired {
                    thread: self.thread,
                    reason,
                });
            }
            RunnerCommand::Reset => {
                tracing::info!(thread_id = %self.thread, alive = self.proc_.is_some(), "reset da thread (sessão esquecida)");
                self.kill_process("reset").await;
                self.session_id = None;
                self.emit(EngineEvent::Retired {
                    thread: self.thread,
                    reason: "reset".into(),
                });
            }
            RunnerCommand::Interrupt => {
                if self.proc_.is_some() && self.pending.iter().any(|p| p.accepted) {
                    let span = self.current_turn_span();
                    match self.write_stdin(&interrupt_line()).await {
                        Ok(()) => {
                            self.interrupt_sent = true;
                            span.in_scope(|| {
                                tracing::info!(thread_id = %self.thread, "interrupção enviada ao claude")
                            })
                        }
                        Err(e) => span.in_scope(|| {
                            tracing::warn!(thread_id = %self.thread, error = %e, "falha ao enviar interrupção")
                        }),
                    }
                } else if !self.pending.is_empty() {
                    // Nada rodando ainda (processo subindo/sem processo): cancela a
                    // fila em vez de ignorar — senão o botão "fica vermelho e nada acontece".
                    let ids: Vec<TurnId> = self.pending.drain(..).map(|p| p.turn_id).collect();
                    tracing::info!(thread_id = %self.thread, n = ids.len(), "interrupção: fila cancelada sem processo em turno");
                    self.sync_shared();
                    self.emit(EngineEvent::Cancelled {
                        thread: self.thread,
                        turn_ids: ids,
                    });
                } else {
                    tracing::debug!(thread_id = %self.thread, "interrupção ignorada: sem processo vivo nem fila");
                }
            }
            RunnerCommand::Reconfigure(cfg) => {
                self.cfg = *cfg;
                tracing::info!(
                    thread_id = %self.thread,
                    token_id = self.cfg.token_id.as_deref().unwrap_or(""),
                    alive = self.proc_.is_some(),
                    "reconfigure: nova configuração; processo vivo será derrubado"
                );
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
                tracing::debug!(
                    thread_id = %self.thread,
                    token_id = self.cfg.token_id.as_deref().unwrap_or(""),
                    "reconfigure suave: vale no próximo spawn"
                );
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

    /// Sobe o `claude -p`. `turn_id`: turno que motivou o spawn (só para o span).
    async fn spawn_process(&mut self, turn_id: Option<TurnId>) -> Result<(), EngineError> {
        let new_id = uuid::Uuid::new_v4().to_string();
        let args = self.cfg.args(self.session_id.as_deref(), &new_id);
        self.resumed = self.session_id.is_some();
        let span = tracing::info_span!(
            "engine_proc",
            thread_id = %self.thread,
            turn_id = tracing::field::Empty,
            pid = tracing::field::Empty,
            exit_code = tracing::field::Empty,
            token_id = self.cfg.token_id.as_deref().unwrap_or("")
        );
        if let Some(t) = turn_id {
            span.record("turn_id", tracing::field::display(t));
        }
        let _entered = span.clone().entered();
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
        will_core::process::hide_console(&mut cmd);
        will_platform::process::die_with_parent(&mut cmd);
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().map_err(|e| {
            let err = if e.kind() == std::io::ErrorKind::NotFound {
                EngineError::BinaryNotFound(self.cfg.claude_bin.to_string_lossy().to_string())
            } else {
                EngineError::Spawn(e.to_string())
            };
            tracing::error!(
                error = %err,
                bin = %self.cfg.claude_bin.display(),
                cwd = %self.cfg.cwd.display(),
                "não foi possível iniciar o claude"
            );
            err
        })?;
        will_platform::process::attach_to_job(&child);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Spawn("sem stdout".into()))?;
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let pid = child.id();
        if let Some(p) = pid {
            span.record("pid", p);
        }
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stale = Arc::new(AtomicBool::new(false));
        if let Some(err) = stderr {
            let tail2 = tail.clone();
            let stale2 = stale.clone();
            let thread = self.thread;
            let events = self.events.clone();
            let stderr_span = span.clone();
            tokio::spawn(
                async move {
                    let mut lines = BufReader::new(err).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        tracing::debug!(thread_id = %thread, "stderr: {l}");
                        if l.contains("No conversation found with session ID") {
                            tracing::warn!(thread_id = %thread, "stderr: sessão do claude não existe mais (expirada)");
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
                            tracing::error!(thread_id = %thread, "stderr indica falha de autenticação do token");
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
                            tracing::warn!(thread_id = %thread, "stderr indica rate limit/quota");
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
                }
                .instrument(stderr_span),
            );
        }
        if let Some(dir) = &self.cfg.raw_log_dir {
            let _ = std::fs::create_dir_all(dir);
            let name = format!("claude-{}.log", chrono::Utc::now().format("%Y-%m-%d"));
            self.raw_log = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(&name))
            {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!(error = %e, file = %dir.join(&name).display(), "não abriu o log bruto do stream");
                    None
                }
            };
        }
        self.proc_ = Some(Proc {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr_tail: tail,
            stale_flag: stale,
            pid,
            span: span.clone(),
            started_at: Instant::now(),
        });
        self.emit(EngineEvent::ProcessStarted {
            thread: self.thread,
            pid,
        });
        tracing::info!(
            resumed = self.resumed,
            pid = ?pid,
            mcp_config = self.cfg.mcp_config_path.is_some(),
            permission_mode = ?self.cfg.permission_mode,
            model = self.cfg.model.as_deref().unwrap_or(""),
            "claude spawn"
        );
        Ok(())
    }

    fn handle_line(&mut self, line: &str) {
        if let Some(f) = self.raw_log.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
        let proc_span = self.proc_span();
        let _proc_guard = proc_span.enter();
        match parse_line(line) {
            StreamLine::Init {
                session_id,
                mcp_servers,
                ..
            } => {
                if !session_id.is_empty() && self.session_id.as_deref() != Some(&session_id) {
                    self.session_id = Some(session_id.clone());
                }
                let will_ia_mcp = mcp_servers
                    .iter()
                    .find(|(n, _)| n == "will_ia")
                    .map(|(_, s)| s.as_str())
                    .unwrap_or("ausente");
                for (name, status) in &mcp_servers {
                    if name == "will_ia" && status != "connected" {
                        tracing::warn!(thread_id = %self.thread, status, "MCP will_ia não conectado");
                    }
                }
                tracing::info!(
                    thread_id = %self.thread,
                    session_id = self.session_id.as_deref().unwrap_or(""),
                    resumed = self.resumed,
                    mcp_servers = mcp_servers.len(),
                    will_ia_mcp,
                    "claude pronto (init)"
                );
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
                    let span = p.span.clone();
                    span.in_scope(|| {
                        tracing::debug!(mid_turn, "mensagem ecoada pelo claude (aceita)")
                    });
                    self.emit(EngineEvent::Accepted {
                        thread: self.thread,
                        turn_id: tid,
                        mid_turn,
                    });
                } else {
                    tracing::debug!(thread_id = %self.thread, "replay sem pendência correspondente");
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
                    self.current_turn_span().in_scope(
                        || tracing::warn!(%tool, "ferramenta negada por permissão do claude"),
                    );
                    self.emit(EngineEvent::PermissionDenied {
                        thread: self.thread,
                        tool,
                    });
                } else if is_error {
                    self.current_turn_span().in_scope(|| {
                        tracing::debug!(bytes = preview.len(), "tool_result com erro")
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
                self.current_turn_span().in_scope(|| {
                    tracing::warn!(thread_id = %self.thread, "claude reportou rate limit no stream")
                });
                self.emit(EngineEvent::RateLimited {
                    thread: self.thread,
                    detail,
                });
            }
            StreamLine::Result {
                mut result,
                stale_session,
            } => {
                if self.interrupt_sent {
                    // `result` do turno interrompido: sem texto de diagnóstico, sem
                    // contar como erro do modelo.
                    self.interrupt_sent = false;
                    result.interrupted = true;
                    result.is_error = true;
                    result.text.clear();
                }
                if stale_session {
                    self.current_turn_span().in_scope(|| {
                        tracing::warn!(thread_id = %self.thread, "result indica sessão expirada; aguardando exit para recomeçar")
                    });
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
                    tracing::warn!(thread_id = %self.thread, is_error = result.is_error, "result sem pendência na fila");
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
                let usage_u64 = |k: &str| {
                    result
                        .usage
                        .as_ref()
                        .and_then(|u| u.get(k))
                        .and_then(|v| v.as_u64())
                };
                head.span.in_scope(|| {
                    tracing::info!(
                        is_error = result.is_error,
                        cost_usd = result.cost_usd.unwrap_or(0.0),
                        duration_ms = result.duration_ms.unwrap_or(0),
                        wall_ms = head.started_at.elapsed().as_millis() as u64,
                        model = result.model.as_deref().unwrap_or(""),
                        input_tokens = usage_u64("input_tokens").unwrap_or(0),
                        output_tokens = usage_u64("output_tokens").unwrap_or(0),
                        cache_read_tokens = usage_u64("cache_read_input_tokens").unwrap_or(0),
                        merged = merged.len(),
                        response_chars = result.text.chars().count(),
                        retried = head.retried,
                        "turno concluído"
                    )
                });
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
                let span = p.span.clone();
                let wait_ms = p.started_at.elapsed().as_millis() as u64;
                span.in_scope(|| tracing::debug!(wait_ms, "turno começou (primeira atividade)"));
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

    /// Descarta um turno pendente com log no span dele.
    fn drop_pending(&self, q: Pending, reason: &str) {
        q.span.in_scope(|| {
            tracing::warn!(
                reason,
                accepted = q.accepted,
                retried = q.retried,
                "turno descartado"
            )
        });
        self.emit(EngineEvent::Dropped {
            thread: self.thread,
            turn_id: q.turn_id,
            reason: reason.to_string(),
        });
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
        if let Some(c) = code {
            p.span.record("exit_code", c);
        }
        p.span.in_scope(|| {
            tracing::warn!(
                thread_id = %self.thread,
                exit_code = ?code,
                stale,
                pending = self.pending.len(),
                uptime_ms = p.started_at.elapsed().as_millis() as u64,
                stderr_lines = tail.len(),
                "claude encerrou"
            )
        });
        self.emit(EngineEvent::ProcessExited {
            thread: self.thread,
            code,
            stderr_tail: tail.join("\n"),
        });

        if stale && !self.pending.is_empty() && self.pending.iter().all(|q| !q.retried) {
            // Sessão expirada: esquece, sobe do zero e reenvia (uma vez).
            tracing::warn!(
                thread_id = %self.thread,
                pending = self.pending.len(),
                "sessão expirada: recomeçando do zero e reenviando as pendências (uma vez)"
            );
            self.session_id = None;
            self.emit(EngineEvent::StaleSession {
                thread: self.thread,
            });
            let msgs: Vec<(SendRequest, tracing::Span)> = self
                .pending
                .drain(..)
                .map(|q| {
                    (
                        SendRequest {
                            turn_id: q.turn_id,
                            text: q.text,
                            source: q.source,
                        },
                        q.span,
                    )
                })
                .collect();
            let first_turn = msgs.first().map(|(m, _)| m.turn_id);
            if self.spawn_process(first_turn).await.is_ok() {
                for (m, span) in msgs {
                    let line = user_message_line(&m.text);
                    match self.write_stdin(&line).await {
                        Ok(()) => {
                            span.in_scope(|| {
                                tracing::info!(
                                    bytes = line.len(),
                                    "mensagem reenviada após sessão expirada"
                                )
                            });
                            self.pending.push_back(Pending {
                                turn_id: m.turn_id,
                                text: m.text,
                                source: m.source,
                                retried: true,
                                accepted: false,
                                started_emitted: false,
                                started_at: Instant::now(),
                                span: span.clone(),
                            });
                        }
                        Err(e) => {
                            span.in_scope(
                                || tracing::error!(error = %e, "reenvio falhou; turno descartado"),
                            );
                            self.emit(EngineEvent::Dropped {
                                thread: self.thread,
                                turn_id: m.turn_id,
                                reason: "reenvio falhou".into(),
                            });
                        }
                    }
                }
            } else {
                for (m, span) in msgs {
                    span.in_scope(|| {
                        tracing::error!("respawn falhou após sessão expirada; turno descartado")
                    });
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
            self.drop_pending(q, &reason);
        }
    }

    async fn kill_process(&mut self, reason: &str) {
        let Some(mut p) = self.proc_.take() else {
            return;
        };
        p.span.in_scope(|| {
            tracing::info!(
                thread_id = %self.thread,
                reason,
                pid = ?p.pid,
                pending = self.pending.len(),
                uptime_ms = p.started_at.elapsed().as_millis() as u64,
                "encerrando claude"
            )
        });
        drop(p.stdin.take());
        kill_tree(&mut p.child, p.pid, self.cfg.kill_grace)
            .instrument(p.span.clone())
            .await;
        self.raw_log = None;
        // Pendências morrem com o processo (quem pediu restart sabe disso).
        let reason_s = format!("thread reiniciada: {reason}");
        while let Some(q) = self.pending.pop_front() {
            self.drop_pending(q, &reason_s);
        }
    }
}

/// SIGTERM no grupo → espera → SIGKILL. No Windows, taskkill /T.
pub async fn kill_tree(child: &mut Child, pid: Option<u32>, grace: Duration) {
    #[cfg(unix)]
    {
        if let Some(pid) = pid {
            let pg = nix::unistd::Pid::from_raw(pid as i32);
            if let Err(e) = nix::sys::signal::killpg(pg, nix::sys::signal::Signal::SIGTERM) {
                tracing::debug!(pid, error = %e, "SIGTERM no grupo falhou (já encerrado?)");
            }
            if tokio::time::timeout(grace, child.wait()).await.is_ok() {
                return;
            }
            tracing::warn!(
                pid,
                grace_ms = grace.as_millis() as u64,
                "claude não encerrou no prazo; SIGKILL no grupo"
            );
            let _ = nix::sys::signal::killpg(pg, nix::sys::signal::Signal::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        // No Windows o taskkill /T /F já é imediato; não há período de graça.
        let _ = grace;
        if let Some(pid) = pid {
            let mut kill = Command::new("taskkill");
            kill.args(["/PID", &pid.to_string(), "/T", "/F"]);
            will_core::process::hide_console(&mut kill);
            let _ = kill.output().await;
        }
    }
    let _ = child.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
}
