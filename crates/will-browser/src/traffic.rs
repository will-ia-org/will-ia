//! Log em memória das mensagens CDP que passam pelo proxy (para a página Browser CDP).

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, serde::Serialize)]
pub struct TrafficEntry {
    pub seq: u64,
    pub at: chrono::DateTime<chrono::Utc>,
    /// Conexão WebSocket (contador por processo).
    pub conn: u32,
    /// `to_browser` (cliente → navegador) ou `from_browser`.
    pub dir: &'static str,
    pub id: Option<i64>,
    /// Método do comando, ou do comando correspondente numa resposta.
    pub method: Option<String>,
    pub session_id: Option<String>,
    /// `command` | `response` | `event` | `error` | `other`.
    pub kind: &'static str,
    pub preview: String,
    pub bytes: usize,
}

pub struct Traffic {
    inner: Mutex<Inner>,
    capacity: usize,
}

struct Inner {
    entries: VecDeque<TrafficEntry>,
    seq: u64,
    /// id → método, por conexão, para anotar respostas.
    pending: HashMap<u32, HashMap<i64, String>>,
    counters: Counters,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Counters {
    pub commands: u64,
    pub responses: u64,
    pub events: u64,
    pub errors: u64,
    pub bytes_to_browser: u64,
    pub bytes_from_browser: u64,
}

const PREVIEW_MAX: usize = 400;

impl Traffic {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::new(),
                seq: 0,
                pending: HashMap::new(),
                counters: Counters::default(),
            }),
            capacity: capacity.max(10),
        }
    }

    /// Registra uma mensagem de texto. `to_browser` = sentido cliente → navegador.
    pub fn record(&self, conn: u32, to_browser: bool, text: &str) {
        let v: Option<serde_json::Value> = serde_json::from_str(text).ok();
        let id = v
            .as_ref()
            .and_then(|v| v.get("id"))
            .and_then(|i| i.as_i64());
        let mut method = v
            .as_ref()
            .and_then(|v| v.get("method"))
            .and_then(|m| m.as_str())
            .map(str::to_string);
        let session_id = v
            .as_ref()
            .and_then(|v| v.get("sessionId"))
            .and_then(|m| m.as_str())
            .map(str::to_string);
        let is_error = v.as_ref().is_some_and(|v| v.get("error").is_some());
        let mut g = self.inner.lock();
        let kind: &'static str = match (to_browser, id, method.is_some(), is_error) {
            (true, Some(id), true, _) => {
                if let Some(m) = &method {
                    let map = g.pending.entry(conn).or_default();
                    if map.len() > 2000 {
                        map.clear();
                    }
                    map.insert(id, m.clone());
                }
                g.counters.commands += 1;
                "command"
            }
            (false, Some(id), _, err) => {
                if method.is_none() {
                    method = g.pending.get_mut(&conn).and_then(|m| m.remove(&id));
                }
                if err {
                    g.counters.errors += 1;
                    "error"
                } else {
                    g.counters.responses += 1;
                    "response"
                }
            }
            (false, None, true, _) => {
                g.counters.events += 1;
                "event"
            }
            _ => "other",
        };
        if to_browser {
            g.counters.bytes_to_browser += text.len() as u64;
        } else {
            g.counters.bytes_from_browser += text.len() as u64;
        }
        g.seq += 1;
        let entry = TrafficEntry {
            seq: g.seq,
            at: chrono::Utc::now(),
            conn,
            dir: if to_browser {
                "to_browser"
            } else {
                "from_browser"
            },
            id,
            method,
            session_id,
            kind,
            preview: will_core::tools::truncate(text, PREVIEW_MAX),
            bytes: text.len(),
        };
        g.entries.push_back(entry);
        while g.entries.len() > self.capacity {
            g.entries.pop_front();
        }
    }

    pub fn forget_conn(&self, conn: u32) {
        self.inner.lock().pending.remove(&conn);
    }

    /// Entradas com `seq > after`, limitadas a `limit` (mais antigas primeiro).
    pub fn since(&self, after: u64, limit: usize) -> Vec<TrafficEntry> {
        let g = self.inner.lock();
        g.entries
            .iter()
            .filter(|e| e.seq > after)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn counters(&self) -> Counters {
        self.inner.lock().counters.clone()
    }

    pub fn last_seq(&self) -> u64 {
        self.inner.lock().seq
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock();
        g.entries.clear();
        g.counters = Counters::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotates_responses_with_method_and_counts() {
        let t = Traffic::new(100);
        t.record(1, true, r#"{"id":1,"method":"Target.getTargets"}"#);
        t.record(1, false, r#"{"id":1,"result":{"targetInfos":[]}}"#);
        t.record(1, false, r#"{"method":"Target.targetCreated","params":{}}"#);
        t.record(
            1,
            false,
            r#"{"id":2,"error":{"code":-32601,"message":"x"}}"#,
        );
        let e = t.since(0, 10);
        assert_eq!(e.len(), 4);
        assert_eq!(e[0].kind, "command");
        assert_eq!(e[1].kind, "response");
        assert_eq!(e[1].method.as_deref(), Some("Target.getTargets"));
        assert_eq!(e[2].kind, "event");
        assert_eq!(e[3].kind, "error");
        let c = t.counters();
        assert_eq!((c.commands, c.responses, c.events, c.errors), (1, 1, 1, 1));
        assert_eq!(t.since(3, 10).len(), 1);
    }

    #[test]
    fn capacity_is_bounded() {
        let t = Traffic::new(10);
        for i in 0..50 {
            t.record(1, true, &format!(r#"{{"id":{i},"method":"X"}}"#));
        }
        assert_eq!(t.since(0, 100).len(), 10);
        assert_eq!(t.last_seq(), 50);
    }
}
