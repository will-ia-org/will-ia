//! Agendamentos: lembretes/tarefas que o agente (ou o usuário) programa e que o
//! serviço entrega como mensagem na thread dona quando chega a hora (`docs/24`).
//!
//! Um agendamento é **recorrente** (expressão cron, 5 campos + opcional de
//! segundos, no fuso `timezone`) ou **único** (`run_at`). Ele guarda a thread
//! que o criou e o `claude_session_id` dela na criação: se a thread for apagada,
//! o scheduler a recria com a mesma origem e sessão (`ThreadHost::revive_thread`).

use crate::ids::{ScheduleId, ThreadId};
use crate::thread::Origin;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Quando disparar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleTrigger {
    /// Expressão cron (`0 8 * * *`), avaliada no fuso do agendamento.
    Cron { expr: String },
    /// Uma única vez, no instante dado.
    Once { at: DateTime<Utc> },
}

impl ScheduleTrigger {
    pub fn is_recurring(&self) -> bool {
        matches!(self, ScheduleTrigger::Cron { .. })
    }
    /// Texto curto para listas/logs (`cron 0 8 * * *` ou `once 2026-10-28T13:00:00Z`).
    pub fn short(&self) -> String {
        match self {
            ScheduleTrigger::Cron { expr } => format!("cron {expr}"),
            ScheduleTrigger::Once { at } => format!("once {}", at.to_rfc3339()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: ScheduleId,
    /// Nome curto (aparece na lista, na notificação e no cabeçalho da mensagem).
    pub name: String,
    /// Texto entregue ao agente como se fosse o usuário escrevendo.
    pub message: String,
    pub trigger: ScheduleTrigger,
    /// Fuso IANA das expressões cron; `None` = fuso padrão do scheduler/sistema.
    pub timezone: Option<String>,
    /// Thread dona (recebe a mensagem). Pode ser atualizada se a thread for recriada.
    pub thread_id: ThreadId,
    /// Sessão do Claude da thread na criação (para `--resume` ao recriar).
    pub claude_session_id: Option<String>,
    /// Origem da thread na criação (para recriar no mesmo canal/tópico).
    pub origin: Origin,
    pub enabled: bool,
    /// `agent` (via MCP) ou `user` (UI/API).
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Próximo disparo previsto (recalculado a cada disparo/edição). `None` = nada a fazer.
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    /// Resultado do último disparo (`ok`, ou motivo da falha).
    pub last_result: Option<String>,
    pub run_count: i64,
    /// Preenchido quando o scheduler desligou o agendamento sozinho (ex.: único já passou).
    pub paused_reason: Option<String>,
}

/// Registro de um disparo (histórico curto por agendamento).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub id: i64,
    pub schedule_id: ScheduleId,
    pub at: DateTime<Utc>,
    pub thread_id: Option<ThreadId>,
    pub ok: bool,
    pub detail: Option<String>,
}

/// Cabeçalho que antecede a mensagem entregue ao agente. Gerado pelo serviço,
/// nunca pelo agente, para que ele saiba que é um disparo e não o usuário ao vivo.
pub fn fired_message(s: &Schedule, fired_at_local: &str) -> String {
    let kind = match &s.trigger {
        ScheduleTrigger::Cron { expr } => format!("recorrente, cron `{expr}`"),
        ScheduleTrigger::Once { .. } => "único".to_string(),
    };
    format!(
        "[Agendamento \"{}\" disparado em {} ({kind}; id {}). Esta mensagem foi programada anteriormente nesta conversa; execute o que ela pede. Para ajustar ou cancelar, use as ferramentas schedule_*.]\n\n{}",
        s.name, fired_at_local, s.id, s.message
    )
}
