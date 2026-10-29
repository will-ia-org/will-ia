//! Backend simulado: roteiros de eventos para testes e fallback quando o crate
//! é compilado sem a feature `real`.

use crate::backend::{jid_from_phone, BackendEvent, MessageRef, WhatsAppBackend};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::mpsc;

/// Registro de um envio feito pelo mock (para asserções em testes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockSent {
    pub id: String,
    pub chat_jid: String,
    pub text: Option<String>,
    pub path: Option<PathBuf>,
    pub caption: Option<String>,
    pub quote: Option<String>,
}

/// Ação registrada pelo mock além dos envios de texto/mídia (para asserções).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockAction {
    Reaction {
        msg: MessageRef,
        emoji: String,
    },
    Edit {
        msg: MessageRef,
        text: String,
    },
    Delete {
        msg: MessageRef,
        for_everyone: bool,
    },
    Contact {
        chat_jid: String,
        name: String,
        vcard: String,
    },
    Voice {
        chat_jid: String,
        path: PathBuf,
        duration_secs: Option<u32>,
    },
}

pub struct MockBackend {
    account_id: String,
    tx: mpsc::Sender<(String, BackendEvent)>,
    available: bool,
    has_session: AtomicBool,
    connected: AtomicBool,
    me: Mutex<Option<serde_json::Value>>,
    sent: Mutex<Vec<MockSent>>,
    actions: Mutex<Vec<MockAction>>,
    phones: Mutex<HashMap<String, Option<String>>>,
    groups: Mutex<HashMap<String, serde_json::Value>>,
    fail_send: AtomicBool,
    seq: AtomicU64,
}

impl MockBackend {
    /// Mock funcional: `connect()` emite QR/código (ou `Connected` se `with_session`).
    /// Os eventos saem etiquetados com `account_id` no canal compartilhado do módulo.
    pub fn new(account_id: impl Into<String>, tx: mpsc::Sender<(String, BackendEvent)>) -> Self {
        Self {
            account_id: account_id.into(),
            tx,
            available: true,
            has_session: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            me: Mutex::new(None),
            sent: Mutex::new(Vec::new()),
            actions: Mutex::new(Vec::new()),
            phones: Mutex::new(HashMap::new()),
            groups: Mutex::new(HashMap::new()),
            fail_send: AtomicBool::new(false),
            seq: AtomicU64::new(1),
        }
    }

    /// Mock que recusa conectar: usado quando o build não tem a feature `real`.
    pub fn unavailable(
        account_id: impl Into<String>,
        tx: mpsc::Sender<(String, BackendEvent)>,
    ) -> Self {
        let mut m = Self::new(account_id, tx);
        m.available = false;
        m
    }

    /// Simula sessão persistida: `connect(None)` emite `Connected { me }`.
    pub fn with_session(self, me: serde_json::Value) -> Self {
        self.has_session.store(true, Ordering::SeqCst);
        *self.me.lock() = Some(me);
        self
    }

    /// Injeta um evento no roteiro (atualiza o estado interno quando aplicável).
    pub async fn push(&self, ev: BackendEvent) {
        match &ev {
            BackendEvent::Connected { me } => {
                self.connected.store(true, Ordering::SeqCst);
                self.has_session.store(true, Ordering::SeqCst);
                *self.me.lock() = Some(me.clone());
            }
            BackendEvent::Disconnected { .. } => self.connected.store(false, Ordering::SeqCst),
            BackendEvent::LoggedOut => {
                self.connected.store(false, Ordering::SeqCst);
                self.has_session.store(false, Ordering::SeqCst);
                *self.me.lock() = None;
            }
            _ => {}
        }
        let _ = self.tx.send((self.account_id.clone(), ev)).await;
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn sent(&self) -> Vec<MockSent> {
        self.sent.lock().clone()
    }

    pub fn actions(&self) -> Vec<MockAction> {
        self.actions.lock().clone()
    }

    /// Define o resultado de `resolve_phone` para um número (`None` = não está no WhatsApp).
    pub fn set_phone(&self, digits: &str, jid: Option<String>) {
        self.phones.lock().insert(digits.to_string(), jid);
    }

    pub fn set_group(&self, jid: &str, meta: serde_json::Value) {
        self.groups.lock().insert(jid.to_string(), meta);
    }

    pub fn set_fail_send(&self, fail: bool) {
        self.fail_send.store(fail, Ordering::SeqCst);
    }

    fn next_id(&self) -> String {
        format!("MOCK{:06}", self.seq.fetch_add(1, Ordering::SeqCst))
    }

    fn ensure_connected(&self) -> Result<(), String> {
        if self.connected.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err("WhatsApp desconectado".into())
        }
    }
}

#[async_trait::async_trait]
impl WhatsAppBackend for MockBackend {
    fn kind(&self) -> &'static str {
        "mock"
    }

    fn has_session(&self) -> bool {
        self.available && self.has_session.load(Ordering::SeqCst)
    }

    async fn connect(&self, pair_phone: Option<String>) -> Result<(), String> {
        if !self.available {
            return Err(
                "backend WhatsApp indisponível neste build (compile com --features real)".into(),
            );
        }
        if self.has_session.load(Ordering::SeqCst) {
            let me = self.me.lock().clone().unwrap_or(serde_json::json!({}));
            self.push(BackendEvent::Connected { me }).await;
        } else if pair_phone.is_some() {
            self.push(BackendEvent::PairCode {
                code: "ABCD-1234".into(),
            })
            .await;
        } else {
            let code = format!("mock-qr-{}", self.seq.fetch_add(1, Ordering::SeqCst));
            self.push(BackendEvent::Qr {
                code,
                timeout_secs: 60,
            })
            .await;
        }
        Ok(())
    }

    async fn disconnect(&self) -> Result<(), String> {
        if self.connected.swap(false, Ordering::SeqCst) {
            self.push(BackendEvent::Disconnected {
                reason: "solicitado".into(),
            })
            .await;
        }
        Ok(())
    }

    async fn logout(&self) -> Result<(), String> {
        self.push(BackendEvent::LoggedOut).await;
        Ok(())
    }

    async fn send_text(
        &self,
        chat_jid: &str,
        text: &str,
        quote_message_id: Option<&str>,
    ) -> Result<String, String> {
        self.ensure_connected()?;
        if self.fail_send.load(Ordering::SeqCst) {
            return Err("falha simulada no envio".into());
        }
        let id = self.next_id();
        self.sent.lock().push(MockSent {
            id: id.clone(),
            chat_jid: chat_jid.to_string(),
            text: Some(text.to_string()),
            path: None,
            caption: None,
            quote: quote_message_id.map(str::to_string),
        });
        Ok(id)
    }

    async fn send_media(
        &self,
        chat_jid: &str,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<String, String> {
        self.ensure_connected()?;
        if self.fail_send.load(Ordering::SeqCst) {
            return Err("falha simulada no envio".into());
        }
        let id = self.next_id();
        self.sent.lock().push(MockSent {
            id: id.clone(),
            chat_jid: chat_jid.to_string(),
            text: None,
            path: Some(path.to_path_buf()),
            caption: caption.map(str::to_string),
            quote: None,
        });
        Ok(id)
    }

    async fn download_media(
        &self,
        message_id: &str,
        _media_ref: Option<&str>,
        dest_dir: &Path,
    ) -> Result<PathBuf, String> {
        self.ensure_connected()?;
        std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
        let safe: String = message_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let path = dest_dir.join(format!("{safe}.bin"));
        std::fs::write(&path, b"mock media").map_err(|e| e.to_string())?;
        Ok(path)
    }

    async fn mark_read(&self, _chat_jid: &str) -> Result<(), String> {
        self.ensure_connected()
    }

    async fn fetch_group(&self, jid: &str) -> Result<serde_json::Value, String> {
        self.ensure_connected()?;
        self.groups
            .lock()
            .get(jid)
            .cloned()
            .ok_or_else(|| format!("grupo {jid} não encontrado"))
    }

    async fn resolve_phone(&self, phone: &str) -> Result<Option<String>, String> {
        self.ensure_connected()?;
        if let Some(v) = self.phones.lock().get(phone) {
            return Ok(v.clone());
        }
        Ok(Some(jid_from_phone(phone)))
    }

    async fn send_reaction(&self, msg: &MessageRef, emoji: &str) -> Result<(), String> {
        self.ensure_connected()?;
        if self.fail_send.load(Ordering::SeqCst) {
            return Err("falha simulada".into());
        }
        self.actions.lock().push(MockAction::Reaction {
            msg: msg.clone(),
            emoji: emoji.to_string(),
        });
        Ok(())
    }

    async fn edit_message(&self, msg: &MessageRef, new_text: &str) -> Result<(), String> {
        self.ensure_connected()?;
        if self.fail_send.load(Ordering::SeqCst) {
            return Err("falha simulada".into());
        }
        self.actions.lock().push(MockAction::Edit {
            msg: msg.clone(),
            text: new_text.to_string(),
        });
        Ok(())
    }

    async fn delete_message(&self, msg: &MessageRef, for_everyone: bool) -> Result<(), String> {
        self.ensure_connected()?;
        if self.fail_send.load(Ordering::SeqCst) {
            return Err("falha simulada".into());
        }
        self.actions.lock().push(MockAction::Delete {
            msg: msg.clone(),
            for_everyone,
        });
        Ok(())
    }

    async fn send_contact(
        &self,
        chat_jid: &str,
        display_name: &str,
        vcard: &str,
    ) -> Result<String, String> {
        self.ensure_connected()?;
        if self.fail_send.load(Ordering::SeqCst) {
            return Err("falha simulada no envio".into());
        }
        self.actions.lock().push(MockAction::Contact {
            chat_jid: chat_jid.to_string(),
            name: display_name.to_string(),
            vcard: vcard.to_string(),
        });
        Ok(self.next_id())
    }

    async fn send_voice(
        &self,
        chat_jid: &str,
        path: &Path,
        duration_secs: Option<u32>,
    ) -> Result<String, String> {
        self.ensure_connected()?;
        if self.fail_send.load(Ordering::SeqCst) {
            return Err("falha simulada no envio".into());
        }
        self.actions.lock().push(MockAction::Voice {
            chat_jid: chat_jid.to_string(),
            path: path.to_path_buf(),
            duration_secs,
        });
        Ok(self.next_id())
    }

    async fn contact_info(&self, jid: &str) -> Result<serde_json::Value, String> {
        self.ensure_connected()?;
        Ok(serde_json::json!({
            "jid": jid,
            "lid": null,
            "is_business": jid.starts_with("5511"),
            "verified_name": null,
            "status": "mock: disponível",
            "picture_url": null,
            "business": null,
        }))
    }

    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn me(&self) -> Option<serde_json::Value> {
        self.me.lock().clone()
    }
}
