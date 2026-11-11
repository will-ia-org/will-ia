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

/// Argumentos de texto **longos** de um pedido (mensagem, corpo de e-mail,
/// legenda…): valores string com quebra de linha ou mais de 80 caracteres que
/// o `summary` (curto por desenho: cabe num card) não traz por inteiro. Serve
/// para o app e o Telegram mostrarem a mensagem completa antes de aprovar.
/// Devolve `(chave, valor)` em ordem alfabética de chave (ordem do mapa JSON).
pub fn long_text_args(args: &serde_json::Value, summary: &str) -> Vec<(String, String)> {
    let Some(obj) = args.as_object() else {
        return Vec::new();
    };
    obj.iter()
        .filter_map(|(k, v)| {
            let s = v.as_str()?.trim();
            let long = s.contains('\n') || s.chars().count() > 80;
            (long && !summary.contains(s)).then(|| (k.clone(), s.to_string()))
        })
        .collect()
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

#[cfg(test)]
mod long_text_tests {
    use super::*;

    #[test]
    fn long_text_args_picks_untruncated_strings() {
        let msg = "linha um\nlinha dois";
        let args = serde_json::json!({
            "chat_id": "5511@c.us",
            "message": msg,
            "n": 3,
            "short": "ok",
            "long": "x".repeat(100),
        });
        let got = long_text_args(&args, &format!("enviar: {msg}"));
        assert_eq!(got, vec![("long".to_string(), "x".repeat(100))]);
        let got = long_text_args(&args, "enviar: linha um…");
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|(k, v)| k == "message" && v == msg));
        assert!(long_text_args(&serde_json::Value::Null, "").is_empty());
    }
}
