use parking_lot::RwLock;
use std::collections::HashMap;
use will_core::{Origin, ThreadId};

#[derive(Debug, Clone)]
pub struct Caller {
    pub thread_id: Option<ThreadId>,
    pub origin: Option<Origin>,
    /// Token de uso geral (scripts/UI), não vinculado a thread.
    pub generic: bool,
}

/// Bearer tokens efêmeros: um por processo `claude`, mais um token geral.
#[derive(Default)]
pub struct TokenRegistry {
    by_token: RwLock<HashMap<String, Caller>>,
    by_thread: RwLock<HashMap<ThreadId, String>>,
}

fn random_token() -> String {
    use rand::Rng;
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl TokenRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Emite (ou reutiliza) o token da thread.
    pub fn issue(&self, thread_id: ThreadId, origin: Origin) -> String {
        if let Some(t) = self.by_thread.read().get(&thread_id) {
            return t.clone();
        }
        let t = random_token();
        self.by_token.write().insert(
            t.clone(),
            Caller {
                thread_id: Some(thread_id),
                origin: Some(origin),
                generic: false,
            },
        );
        self.by_thread.write().insert(thread_id, t.clone());
        t
    }

    pub fn issue_generic(&self) -> String {
        let t = random_token();
        self.by_token.write().insert(
            t.clone(),
            Caller {
                thread_id: None,
                origin: None,
                generic: true,
            },
        );
        t
    }

    pub fn revoke_thread(&self, thread_id: ThreadId) {
        if let Some(t) = self.by_thread.write().remove(&thread_id) {
            self.by_token.write().remove(&t);
        }
    }

    pub fn lookup(&self, token: &str) -> Option<Caller> {
        self.by_token.read().get(token).cloned()
    }

    pub fn token_for(&self, thread_id: ThreadId) -> Option<String> {
        self.by_thread.read().get(&thread_id).cloned()
    }
}
