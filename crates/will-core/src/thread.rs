use crate::ids::ThreadId;
use serde::{Deserialize, Serialize};

/// De onde uma thread se origina. Somente canais criam threads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Ui,
    Telegram {
        chat_id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        topic_id: Option<i64>,
    },
    Realtime,
}

impl Origin {
    pub fn kind(&self) -> OriginKind {
        match self {
            Origin::Ui => OriginKind::Ui,
            Origin::Telegram { .. } => OriginKind::Telegram,
            Origin::Realtime => OriginKind::Realtime,
        }
    }

    /// Representação estável para persistência/env (`WILL_IA_ORIGIN`).
    pub fn origin_ref(&self) -> Option<String> {
        match self {
            Origin::Ui => None,
            Origin::Telegram { chat_id, topic_id } => Some(match topic_id {
                Some(t) => format!("{chat_id}:{t}"),
                None => chat_id.to_string(),
            }),
            Origin::Realtime => Some("voice".into()),
        }
    }

    pub fn from_parts(kind: &str, origin_ref: Option<&str>) -> Option<Self> {
        match kind {
            "ui" => Some(Origin::Ui),
            "realtime" => Some(Origin::Realtime),
            "telegram" => {
                let r = origin_ref?;
                let mut it = r.split(':');
                let chat_id = it.next()?.parse().ok()?;
                let topic_id = it.next().and_then(|t| t.parse().ok());
                Some(Origin::Telegram { chat_id, topic_id })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Ui,
    Telegram,
    Realtime,
}

impl OriginKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OriginKind::Ui => "ui",
            OriginKind::Telegram => "telegram",
            OriginKind::Realtime => "realtime",
        }
    }
}

impl std::fmt::Display for OriginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    /// Sem processo vivo (sessão salva, volta com --resume) ou nunca iniciada.
    Idle,
    /// Processo vivo processando um turno.
    Running,
    /// Processo vivo aguardando mensagens.
    Live,
    Error,
    Archived,
}

impl ThreadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThreadStatus::Idle => "idle",
            ThreadStatus::Running => "running",
            ThreadStatus::Live => "live",
            ThreadStatus::Error => "error",
            ThreadStatus::Archived => "archived",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => ThreadStatus::Running,
            "live" => ThreadStatus::Live,
            "error" => ThreadStatus::Error,
            "archived" => ThreadStatus::Archived,
            _ => ThreadStatus::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub id: ThreadId,
    pub origin: Origin,
    pub title: Option<String>,
    pub claude_session_id: Option<String>,
    pub status: ThreadStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_activity_at: chrono::DateTime<chrono::Utc>,
    pub total_cost_usd: f64,
    /// Processo vivo neste momento?
    #[serde(default)]
    pub alive: bool,
    /// Mensagens enviadas ainda sem `result`.
    #[serde(default)]
    pub pending: usize,
    /// Sessão salva não existe mais no disco (vai voltar do zero).
    #[serde(default)]
    pub session_expired: bool,
}
