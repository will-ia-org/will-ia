//! Contratos pelos quais canais e módulos falam com o orquestrador (implementado em will-server).

use crate::ids::{ApprovalId, ThreadId, TurnId};
use crate::thread::{Origin, ThreadSummary};
use crate::{ApprovalDecision, Event};
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClaudeTokenInfo {
    pub id: String,
    pub label: String,
    pub active: bool,
    pub status: Option<String>,
    pub masked: String,
}

/// O que um canal (Telegram, UI, voz) pode pedir ao orquestrador.
#[async_trait::async_trait]
pub trait ThreadHost: Send + Sync {
    /// Devolve a thread ativa para a origem, criando se não existir.
    async fn ensure_thread(
        &self,
        origin: &Origin,
        title: Option<&str>,
    ) -> Result<ThreadSummary, String>;
    async fn get_thread(&self, thread: ThreadId) -> Result<ThreadSummary, String>;
    async fn list_threads(&self) -> Result<Vec<ThreadSummary>, String>;
    /// Escreve a mensagem no stdin do processo da thread imediatamente (inclusive mid-turn).
    async fn send_message(&self, thread: ThreadId, text: &str) -> Result<TurnId, String>;
    /// Mata o processo mantendo a sessão (volta com --resume).
    async fn restart_thread(&self, thread: ThreadId) -> Result<(), String>;
    /// Mata o processo e esquece a sessão (começa do zero).
    async fn reset_thread(&self, thread: ThreadId) -> Result<(), String>;
    async fn interrupt_thread(&self, thread: ThreadId) -> Result<(), String>;
    async fn delete_thread(&self, thread: ThreadId, purge_transcript: bool) -> Result<(), String>;
    fn subscribe(&self) -> broadcast::Receiver<Event>;
    fn claude_tokens(&self) -> Vec<ClaudeTokenInfo>;
    /// Testa e ativa o token; retorna quantas threads vivas foram derrubadas.
    async fn switch_claude_token(&self, id: &str) -> Result<usize, String>;
    fn decide_approval(&self, id: ApprovalId, decision: ApprovalDecision, via: &str) -> bool;
    fn uptime_secs(&self) -> u64;
}

pub type SharedThreadHost = Arc<dyn ThreadHost>;

/// Resultado de uma delegação ao cérebro iniciada pela voz.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BrainTask {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
}

/// O que a voz (Realtime) precisa do orquestrador.
#[async_trait::async_trait]
pub trait BrainHost: Send + Sync {
    /// Envia o prompt ao cérebro e retorna imediatamente. `thread`: id (prefixo) ou
    /// título de uma thread existente para continuar nela; `new_thread`: começa uma
    /// conversa nova; sem ambos, usa a thread de voz atual. `title`: título curto
    /// proposto pela voz para uma thread nova (senão deriva do primeiro pedido).
    async fn ask_brain(
        &self,
        prompt: &str,
        new_thread: bool,
        thread: Option<&str>,
        title: Option<&str>,
    ) -> Result<BrainTask, String>;
    /// Threads existentes (não arquivadas), mais recentes primeiro, para a voz
    /// oferecer "continuar o assunto X".
    async fn brain_threads(&self) -> Result<Vec<ThreadSummary>, String>;
    /// Há turno em andamento na thread de voz?
    async fn brain_busy(&self) -> bool;
    fn subscribe(&self) -> broadcast::Receiver<Event>;
}
