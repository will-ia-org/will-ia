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

/// Token aleatório de 32 bytes em hex.
pub fn random_token() -> String {
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
        tracing::debug!(%thread_id, channel = origin.kind().as_str(), "bearer MCP emitido para a thread");
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

    /// Emite um token geral novo (não vinculado a thread).
    pub fn issue_generic(&self) -> String {
        let t = random_token();
        self.register_generic(&t);
        t
    }

    /// Registra um token geral já existente (ex.: o token fixo para scripts,
    /// guardado no cofre de segredos e reaplicado a cada subida).
    pub fn register_generic(&self, token: &str) {
        tracing::debug!("bearer MCP genérico (scripts) registrado");
        self.by_token.write().insert(
            token.to_string(),
            Caller {
                thread_id: None,
                origin: None,
                generic: true,
            },
        );
    }

    /// Revoga todos os tokens gerais (rotação do token de scripts).
    pub fn revoke_generic(&self) {
        let mut g = self.by_token.write();
        let before = g.len();
        g.retain(|_, c| !c.generic);
        tracing::info!(
            revoked = before - g.len(),
            "bearers MCP genéricos revogados (rotação)"
        );
    }

    pub fn revoke_thread(&self, thread_id: ThreadId) {
        if let Some(t) = self.by_thread.write().remove(&thread_id) {
            self.by_token.write().remove(&t);
            tracing::debug!(%thread_id, "bearer MCP da thread revogado");
        }
    }

    pub fn lookup(&self, token: &str) -> Option<Caller> {
        self.by_token.read().get(token).cloned()
    }

    pub fn token_for(&self, thread_id: ThreadId) -> Option<String> {
        self.by_thread.read().get(&thread_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_tokens_register_and_rotate() {
        let reg = TokenRegistry::new();
        reg.register_generic("abc");
        let c = reg.lookup("abc").expect("registrado");
        assert!(c.generic && c.thread_id.is_none());
        let t = reg.issue(ThreadId::new(), Origin::Ui);
        reg.revoke_generic();
        assert!(reg.lookup("abc").is_none());
        assert!(reg.lookup(&t).is_some(), "tokens de thread sobrevivem");
    }
}
