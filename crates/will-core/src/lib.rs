//! will-core — tipos de domínio, eventos, settings e contratos compartilhados.
//!
//! Este crate não depende de nenhum outro crate interno. Tudo que os módulos
//! (engine, telegram, mcp, google, whatsapp, realtime) precisam compartilhar
//! vive aqui: identificadores, `Origin`, `Event`, `Settings`, o contrato de
//! ferramentas MCP (`WillTool`/`ToolProvider`) e o gerenciador de aprovações.

pub mod approvals;
pub mod auth;
pub mod bus;
pub mod host;
pub mod ids;
pub mod module;
pub mod paths;
pub mod process;
pub mod schedule;
pub mod settings;
pub mod thread;
pub mod tools;
pub mod trigger;

pub use approvals::{ApprovalDecision, ApprovalManager, ApprovalRequest};
pub use auth::{Principal, Role};
pub use bus::EventBus;
pub use host::{BrainHost, BrainTask, ClaudeTokenInfo, SharedThreadHost, ThreadHost};
pub use ids::{ApprovalId, MessageId, ScheduleId, ThreadId, TriggerId, TurnId};
pub use module::{Module, ModuleStatus};
pub use paths::AppPaths;
pub use schedule::{Schedule, ScheduleRun, ScheduleTrigger};
pub use settings::Settings;
pub use thread::{
    attachment_note, derive_title, is_placeholder_title, split_attachment_notes, Attachment,
    Origin, OriginKind, ThreadStatus, ThreadSummary,
};
pub use tools::{ToolContext, ToolError, ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool};
pub use trigger::{Trigger, TriggerEvent, TriggerFilter, TriggerRun, TriggerSource};

/// Eventos difundidos pelo serviço (UI, canais e módulos assinam). Também é o
/// formato do WS `/api/events`, por isso desserializa (o app é um cliente).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    ThreadCreated {
        thread: ThreadSummary,
    },
    ThreadUpdated {
        thread: ThreadSummary,
    },
    ThreadDeleted {
        thread_id: ThreadId,
    },
    /// Mensagem do usuário aceita pelo processo (via --replay-user-messages ou escrita no stdin).
    MessageAccepted {
        thread_id: ThreadId,
        message_id: MessageId,
        mid_turn: bool,
    },
    TurnStarted {
        thread_id: ThreadId,
        turn_id: TurnId,
    },
    /// Texto parcial/narração/uso de ferramenta durante o turno.
    TurnDelta {
        thread_id: ThreadId,
        turn_id: Option<TurnId>,
        delta: EngineDelta,
    },
    TurnFinished {
        thread_id: ThreadId,
        turn_id: TurnId,
        result: TurnResult,
    },
    TurnFailed {
        thread_id: ThreadId,
        turn_id: Option<TurnId>,
        error: String,
        kind: String,
    },
    /// A sessão salva expirou; a thread recomeçou e reenviou as pendências.
    StaleSession {
        thread_id: ThreadId,
    },
    ModuleStatus {
        module: Module,
        status: ModuleStatus,
    },
    McpToolCalled {
        thread_id: Option<ThreadId>,
        tool: String,
        ok: bool,
        duration_ms: u64,
    },
    ApprovalRequested {
        approval: ApprovalRequest,
    },
    ApprovalDecided {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    },
    SettingsChanged {
        section: String,
    },
    /// Linha de log para a aba Logs da UI.
    Log {
        at: chrono::DateTime<chrono::Utc>,
        level: String,
        module: String,
        message: String,
    },
    /// Eventos de voz (sessão aberta/fechada, falando, ouvindo…). `user`: dono
    /// da sessão (uma por usuário conectado); `None` em registros antigos.
    Voice {
        state: String,
        detail: Option<String>,
        #[serde(default)]
        user: Option<String>,
    },
    /// Estado do pareamento do WhatsApp (qr, code, connected, disconnected).
    WhatsApp {
        state: String,
        detail: Option<String>,
    },
    Notification {
        title: String,
        body: String,
    },
    /// Um agendamento disparou (a mensagem já foi entregue à thread) ou falhou ao disparar.
    ScheduleFired {
        schedule_id: ScheduleId,
        thread_id: Option<ThreadId>,
        name: String,
        ok: bool,
        detail: Option<String>,
    },
    /// Lista de agendamentos mudou (criado/editado/removido/pausado); a UI recarrega.
    SchedulesChanged,
    /// Mensagem do WhatsApp indexada (entrada das triggers; `text` pode ser
    /// vazio em mídia). Vai no WS de eventos como os demais.
    WhatsAppMessage {
        account_id: String,
        msg_id: String,
        chat_jid: String,
        chat_name: Option<String>,
        sender_jid: String,
        sender_name: Option<String>,
        sender_phone: Option<String>,
        from_me: bool,
        is_group: bool,
        kind: String,
        text: Option<String>,
        at: chrono::DateTime<chrono::Utc>,
    },
    /// Uma trigger disparou (mensagem entregue à thread) ou falhou ao disparar.
    TriggerFired {
        trigger_id: TriggerId,
        thread_id: Option<ThreadId>,
        name: String,
        ok: bool,
        detail: Option<String>,
    },
    /// Lista de triggers mudou (criada/editada/removida/pausada); a UI recarrega.
    TriggersChanged,
    /// Estado do Browser CDP (proxy no ar, navegador lançado/fechado, upstream caiu…).
    Browser {
        state: String,
        detail: Option<String>,
    },
    /// Custo de IA registrado (voz, Claude…): `summary` já vem pronto para mostrar.
    CostRecorded {
        source: String,
        usd: f64,
        estimated: bool,
        summary: String,
    },
}

/// Fragmentos emitidos pelo motor durante um turno.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineDelta {
    /// Bloco de texto completo do assistant (narração).
    Narration {
        text: String,
    },
    /// Delta token a token (somente com include-partial-messages).
    TextDelta {
        text: String,
    },
    ToolUse {
        name: String,
        input_preview: String,
    },
    ToolResult {
        preview: String,
        is_error: bool,
    },
    Status {
        text: String,
    },
    Thinking {
        text: String,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TurnResult {
    pub text: String,
    pub is_error: bool,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub usage: Option<serde_json::Value>,
    /// O turno foi interrompido pelo usuário (`interrupt` no stdin ou fila
    /// cancelada): não é falha do modelo; a UI mostra um cartão neutro.
    #[serde(default)]
    pub interrupted: bool,
}
