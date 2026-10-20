//! will-engine — o motor: um processo `claude -p` vivo por thread, stdin aberto
//! em stream-json, mensagens entregues na hora (inclusive mid-turn), fila de
//! pendências para correlacionar `result`s, retire/teto/idle, sessão expirada
//! e troca de token sem downtime.

pub mod config;
pub mod parser;
pub mod pool;
pub mod runner;
pub mod token_test;

pub use config::{EngineConfig, PermissionMode};
pub use pool::{EnginePool, RunnerStats};
pub use runner::{RunnerCommand, SendRequest, ThreadEnv};

use will_core::{EngineDelta, ThreadId, TurnId, TurnResult};

/// Eventos do motor. O servidor converte em `will_core::Event` e persiste.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Processo subiu (init recebido) e informou o session_id.
    Session {
        thread: ThreadId,
        session_id: String,
        resumed: bool,
    },
    /// Mensagem do usuário foi escrita no stdin.
    Sent {
        thread: ThreadId,
        turn_id: TurnId,
        mid_turn: bool,
    },
    /// Mensagem ecoada pelo processo (`--replay-user-messages`): entrou na conversa.
    Accepted {
        thread: ThreadId,
        turn_id: TurnId,
        mid_turn: bool,
    },
    /// Primeiro sinal de atividade do assistant após um envio.
    TurnStarted {
        thread: ThreadId,
        turn_id: TurnId,
    },
    Delta {
        thread: ThreadId,
        turn_id: Option<TurnId>,
        delta: EngineDelta,
    },
    /// `result` recebido: fecha a cabeça da fila (e as mensagens já consumidas).
    TurnFinished {
        thread: ThreadId,
        turn_id: TurnId,
        merged: Vec<TurnId>,
        result: TurnResult,
    },
    /// Processo morreu com pendências; cada uma vira um Dropped.
    Dropped {
        thread: ThreadId,
        turn_id: TurnId,
        reason: String,
    },
    /// Sessão salva não existe mais: recomeçou do zero e reenviou pendências.
    StaleSession {
        thread: ThreadId,
    },
    ProcessStarted {
        thread: ThreadId,
        pid: Option<u32>,
    },
    ProcessExited {
        thread: ThreadId,
        code: Option<i32>,
        stderr_tail: String,
    },
    AuthFailed {
        thread: ThreadId,
        detail: String,
    },
    RateLimited {
        thread: ThreadId,
        detail: String,
    },
    /// Falha de permissão detectada (ferramenta negada).
    PermissionDenied {
        thread: ThreadId,
        tool: String,
    },
    Retired {
        thread: ThreadId,
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("binário do claude não encontrado em '{0}'")]
    BinaryNotFound(String),
    #[error("falha ao iniciar processo: {0}")]
    Spawn(String),
    #[error("falha ao escrever no stdin: {0}")]
    Stdin(String),
    #[error("thread não encontrada")]
    NoThread,
    #[error("{0}")]
    Other(String),
}
