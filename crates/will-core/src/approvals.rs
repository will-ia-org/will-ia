//! Fila de aprovações para ferramentas com política `confirm`.

use crate::ids::{ApprovalId, ThreadId};
use crate::{Event, EventBus};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Denied,
    Timeout,
}

impl ApprovalDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalDecision::Approved => "approved",
            ApprovalDecision::Denied => "denied",
            ApprovalDecision::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub thread_id: Option<ThreadId>,
    pub tool: String,
    pub summary: String,
    pub args_redacted: serde_json::Value,
    pub timeout_secs: u64,
}

struct Pending {
    request: ApprovalRequest,
    tx: Option<oneshot::Sender<(ApprovalDecision, String)>>,
}

/// Gerencia pedidos pendentes em memória. Persistência das decisões fica a cargo
/// de quem assina `Event::ApprovalDecided`.
#[derive(Clone)]
pub struct ApprovalManager {
    inner: Arc<Mutex<HashMap<ApprovalId, Pending>>>,
    bus: EventBus,
}

impl ApprovalManager {
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            bus,
        }
    }

    pub fn pending(&self) -> Vec<ApprovalRequest> {
        let mut v: Vec<_> = self
            .inner
            .lock()
            .values()
            .map(|p| p.request.clone())
            .collect();
        v.sort_by_key(|r| r.requested_at);
        v
    }

    /// Registra o pedido, emite o evento e aguarda a decisão (ou timeout).
    pub async fn request(
        &self,
        thread_id: Option<ThreadId>,
        tool: &str,
        summary: String,
        args_redacted: serde_json::Value,
        timeout: Duration,
    ) -> (ApprovalRequest, ApprovalDecision, String) {
        let id = ApprovalId::new();
        let request = ApprovalRequest {
            id,
            requested_at: chrono::Utc::now(),
            thread_id,
            tool: tool.to_string(),
            summary,
            args_redacted,
            timeout_secs: timeout.as_secs(),
        };
        let (tx, rx) = oneshot::channel();
        self.inner.lock().insert(
            id,
            Pending {
                request: request.clone(),
                tx: Some(tx),
            },
        );
        self.bus.emit(Event::ApprovalRequested {
            approval: request.clone(),
        });

        let outcome = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok((d, via))) => (d, via),
            Ok(Err(_)) => (ApprovalDecision::Denied, "cancelled".to_string()),
            Err(_) => (ApprovalDecision::Timeout, "timeout".to_string()),
        };
        self.inner.lock().remove(&id);
        self.bus.emit(Event::ApprovalDecided {
            approval_id: id,
            decision: outcome.0,
        });
        (request, outcome.0, outcome.1)
    }

    /// Decide um pedido pendente. `via` = ui | notification | telegram.
    pub fn decide(&self, id: ApprovalId, decision: ApprovalDecision, via: &str) -> bool {
        let mut guard = self.inner.lock();
        if let Some(p) = guard.get_mut(&id) {
            if let Some(tx) = p.tx.take() {
                let _ = tx.send((decision, via.to_string()));
                return true;
            }
        }
        false
    }
}
