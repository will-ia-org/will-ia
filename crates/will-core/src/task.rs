//! Tarefas (`docs/31`): itens com início obrigatório (data + hora), fim
//! opcional, prioridade e conclusão, vistos num Gantt no app e manipulados
//! pelo agente (`tasks_*`). O estado é **derivado** do relógio:
//! `upcoming` (ainda não começou) → `running` (começou; sem fim ou antes do fim)
//! → `overdue` (passou do fim sem concluir); `done` quando marcada concluída e
//! `skipped` quando fechada como **não realizada** (some da tela como a
//! concluída, mas registra que não foi feita — ex.: faltou à academia).

use crate::ids::TaskId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Prioridade: alta (1), média (2, padrão), baixa (3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    High,
    #[default]
    Medium,
    Low,
}

impl TaskPriority {
    /// Número da prioridade (1 = alta, 3 = baixa), usado na ordenação e no banco.
    pub fn rank(self) -> u8 {
        match self {
            TaskPriority::High => 1,
            TaskPriority::Medium => 2,
            TaskPriority::Low => 3,
        }
    }
    pub fn from_rank(n: i64) -> Self {
        match n {
            1 => TaskPriority::High,
            3 => TaskPriority::Low,
            _ => TaskPriority::Medium,
        }
    }
    /// Aceita `high|medium|low`, `alta|media|média|baixa` ou `1|2|3`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_lowercase();
        Some(match s.as_str() {
            "high" | "alta" | "1" => TaskPriority::High,
            "medium" | "media" | "média" | "normal" | "2" => TaskPriority::Medium,
            "low" | "baixa" | "3" => TaskPriority::Low,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            TaskPriority::High => "high",
            TaskPriority::Medium => "medium",
            TaskPriority::Low => "low",
        }
    }
    pub fn label_pt(self) -> &'static str {
        match self {
            TaskPriority::High => "alta",
            TaskPriority::Medium => "média",
            TaskPriority::Low => "baixa",
        }
    }
}

/// Estado derivado do relógio e da conclusão.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Upcoming,
    Running,
    Overdue,
    Done,
    Skipped,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Upcoming => "upcoming",
            TaskState::Running => "running",
            TaskState::Overdue => "overdue",
            TaskState::Done => "done",
            TaskState::Skipped => "skipped",
        }
    }
    pub fn label_pt(self) -> &'static str {
        match self {
            TaskState::Upcoming => "futura",
            TaskState::Running => "em andamento",
            TaskState::Overdue => "atrasada",
            TaskState::Done => "concluída",
            TaskState::Skipped => "não realizada",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub description: String,
    /// Início (obrigatório, com hora).
    pub start_at: DateTime<Utc>,
    /// Fim (opcional): a partir dele, sem conclusão, a tarefa fica atrasada.
    pub end_at: Option<DateTime<Utc>>,
    /// Quando foi fechada (`None` = pendente): concluída, ou não realizada se `skipped`.
    pub done_at: Option<DateTime<Utc>>,
    /// Fechada como **não realizada** (só vale com `done_at`).
    #[serde(default)]
    pub skipped: bool,
    pub priority: TaskPriority,
    /// `agent` (via MCP) ou `user` (UI/API).
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Quando o laço notou que entrou em andamento / em atraso (evita avisar duas vezes).
    pub notified_start_at: Option<DateTime<Utc>>,
    pub notified_overdue_at: Option<DateTime<Utc>>,
    /// Vínculo com o Google Calendar (sincronização, `docs/31`): agenda e
    /// evento espelhados; `synced_at` = última sincronização no relógio local
    /// (`updated_at > synced_at` ⇒ editada aqui depois); `remote_updated_at` =
    /// `updated` do evento visto nela (diferente ⇒ editado no Google depois).
    #[serde(default)]
    pub calendar_id: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub synced_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub remote_updated_at: Option<DateTime<Utc>>,
}

impl Task {
    /// Está espelhada num evento do Google Calendar.
    pub fn is_linked(&self) -> bool {
        self.calendar_id.is_some() && self.event_id.is_some()
    }

    pub fn state_at(&self, now: DateTime<Utc>) -> TaskState {
        if self.done_at.is_some() {
            return if self.skipped {
                TaskState::Skipped
            } else {
                TaskState::Done
            };
        }
        if now < self.start_at {
            return TaskState::Upcoming;
        }
        match self.end_at {
            Some(end) if now >= end => TaskState::Overdue,
            _ => TaskState::Running,
        }
    }
    pub fn is_done(&self) -> bool {
        self.done_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 10, h, 0, 0).unwrap()
    }

    fn task(start: u32, end: Option<u32>) -> Task {
        Task {
            id: TaskId::new(),
            title: "x".into(),
            description: String::new(),
            start_at: t(start),
            end_at: end.map(t),
            done_at: None,
            skipped: false,
            priority: TaskPriority::Medium,
            created_by: "user".into(),
            created_at: t(0),
            updated_at: t(0),
            notified_start_at: None,
            notified_overdue_at: None,
            calendar_id: None,
            event_id: None,
            synced_at: None,
            remote_updated_at: None,
        }
    }

    #[test]
    fn state_follows_clock() {
        let k = task(10, Some(12));
        assert_eq!(k.state_at(t(9)), TaskState::Upcoming);
        assert_eq!(k.state_at(t(10)), TaskState::Running);
        assert_eq!(k.state_at(t(11)), TaskState::Running);
        assert_eq!(k.state_at(t(12)), TaskState::Overdue);
        let open = task(10, None);
        assert_eq!(open.state_at(t(23)), TaskState::Running);
        let mut done = task(10, Some(12));
        done.done_at = Some(t(11));
        assert_eq!(done.state_at(t(20)), TaskState::Done);
        done.skipped = true;
        assert_eq!(done.state_at(t(20)), TaskState::Skipped);
    }

    #[test]
    fn priority_parse() {
        assert_eq!(TaskPriority::parse("Alta"), Some(TaskPriority::High));
        assert_eq!(TaskPriority::parse("2"), Some(TaskPriority::Medium));
        assert_eq!(TaskPriority::parse("low"), Some(TaskPriority::Low));
        assert_eq!(TaskPriority::parse("urgente"), None);
        assert_eq!(TaskPriority::from_rank(3), TaskPriority::Low);
    }
}
