//! Backend real sobre `whatsapp-rust` 0.7 (feature `real`).
//!
//! Tudo que toca a biblioteca fica aqui. A sessão (chaves Signal) de cada conta
//! vive em `<data_dir>/whatsapp/<account_id>/store.db` (0600). Mensagens recentes ficam em um cache
//! em memória para citação/download; downloads também funcionam após reinício
//! via `media_ref` (JSON com os parâmetros do CDN) guardado no índice.

use crate::backend::{
    ext_for_mime, is_group_jid, media_class, mime_for_path, phone_from_jid, BackendEvent,
    IncomingMessage, WhatsAppBackend,
};
use base64::prelude::*;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use whatsapp_rust::bot::{Bot, BotHandle};
use whatsapp_rust::client::Client;
use whatsapp_rust::download::DownloadParams;
use whatsapp_rust::media::{
    audio_message, document_message, image_message, video_message, AudioOptions, DocumentOptions,
    ImageOptions, VideoOptions,
};
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::store::SqliteStore;
use whatsapp_rust::upload::UploadOptions;
use whatsapp_rust::wacore::download::{Downloadable, MediaType};
use whatsapp_rust::wacore::proto_helpers::{
    build_quote_context_with_info, ConversationExt, MessageBuilderExt, MessageExt,
};
use whatsapp_rust::wacore::types::events::{Event as WaEvent, LazyHistorySync};
use whatsapp_rust::wacore::types::message::MessageInfo;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::Jid;
use will_storage::whatsapp::WaContact;

const CACHE_MAX: usize = 4000;

struct CachedMsg {
    chat: Jid,
    sender: Jid,
    message: Arc<wa::Message>,
}

#[derive(Default)]
struct MsgCache {
    map: HashMap<String, CachedMsg>,
    order: VecDeque<String>,
}

impl MsgCache {
    fn insert(&mut self, id: String, m: CachedMsg) {
        if self.map.insert(id.clone(), m).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > CACHE_MAX {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

/// Estado compartilhado com os hooks da biblioteca.
struct Shared {
    account_id: String,
    tx: mpsc::Sender<(String, BackendEvent)>,
    connected: AtomicBool,
    me: Mutex<Option<Value>>,
    cache: Mutex<MsgCache>,
}

pub struct RealBackend {
    dir: PathBuf,
    store_path: PathBuf,
    handle: tokio::sync::Mutex<Option<BotHandle>>,
    client: Mutex<Option<Arc<Client>>>,
    shared: Arc<Shared>,
}

impl RealBackend {
    /// `dir` = `AppPaths::whatsapp_dir().join(account_id)`; eventos saem
    /// etiquetados com `account_id` no canal compartilhado do módulo.
    pub fn new(
        account_id: impl Into<String>,
        dir: PathBuf,
        tx: mpsc::Sender<(String, BackendEvent)>,
    ) -> Self {
        Self {
            store_path: dir.join("store.db"),
            dir,
            handle: tokio::sync::Mutex::new(None),
            client: Mutex::new(None),
            shared: Arc::new(Shared {
                account_id: account_id.into(),
                tx,
                connected: AtomicBool::new(false),
                me: Mutex::new(None),
                cache: Mutex::new(MsgCache::default()),
            }),
        }
    }

    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    fn client(&self) -> Result<Arc<Client>, String> {
        self.client
            .lock()
            .clone()
            .ok_or_else(|| "sessão WhatsApp não iniciada".to_string())
    }

    fn connected_client(&self) -> Result<Arc<Client>, String> {
        let c = self.client()?;
        if !c.is_connected() {
            return Err("WhatsApp desconectado".into());
        }
        Ok(c)
    }

    /// Diretório 0700 e arquivos da sessão 0600.
    fn secure_files(&self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let p = PathBuf::from(format!("{}{suffix}", self.store_path.display()));
                if p.exists() {
                    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
    }

    fn remove_store(&self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let p = PathBuf::from(format!("{}{suffix}", self.store_path.display()));
            let _ = std::fs::remove_file(p);
        }
    }
}

#[async_trait::async_trait]
impl WhatsAppBackend for RealBackend {
    async fn lid_mappings(&self) -> Vec<(String, String)> {
        match will_storage::whatsapp::read_lid_mappings(&self.store_path).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "lid_pn_mapping indisponível");
                vec![]
            }
        }
    }

    fn kind(&self) -> &'static str {
        "real"
    }

    fn has_session(&self) -> bool {
        self.store_path.exists()
    }

    async fn connect(&self, pair_phone: Option<String>) -> Result<(), String> {
        let mut guard = self.handle.lock().await;
        if guard.is_some() {
            let client = self.client.lock().clone();
            if let Some(c) = client {
                if !c.is_connected() {
                    c.reconnect_immediately().await;
                }
            }
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("criar {}: {e}", self.dir.display()))?;
        self.secure_files();
        let store = SqliteStore::new(&self.store_path.to_string_lossy())
            .await
            .map_err(|e| format!("abrir store da sessão: {e}"))?;
        self.secure_files();
        let shared = Arc::clone(&self.shared);
        let mut builder = Bot::builder()
            .with_backend(store)
            .on_event(move |event, client| {
                let shared = Arc::clone(&shared);
                async move { shared.handle(event, client).await }
            });
        if let Some(phone) = pair_phone {
            builder = builder.with_pair_code(PairCodeOptions {
                phone_number: phone,
                ..Default::default()
            });
        }
        let bot = builder
            .build()
            .await
            .map_err(|e| format!("iniciar whatsapp-rust: {e}"))?;
        let handle = bot.spawn();
        *self.client.lock() = Some(handle.client());
        *guard = Some(handle);
        Ok(())
    }

    async fn disconnect(&self) -> Result<(), String> {
        let handle = self.handle.lock().await.take();
        if let Some(h) = handle {
            h.shutdown().await;
        }
        *self.client.lock() = None;
        self.shared.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn logout(&self) -> Result<(), String> {
        let client = self.client.lock().clone();
        if let Some(c) = client {
            if c.is_connected() {
                c.logout().await;
            }
        }
        self.disconnect().await?;
        self.remove_store();
        *self.shared.me.lock() = None;
        Ok(())
    }

    async fn send_text(
        &self,
        chat_jid: &str,
        text: &str,
        quote_message_id: Option<&str>,
    ) -> Result<String, String> {
        let client = self.connected_client()?;
        let jid = Jid::from_str(chat_jid).map_err(|e| format!("JID inválido {chat_jid}: {e}"))?;
        let quoted = quote_message_id.and_then(|q| {
            let cache = self.shared.cache.lock();
            cache.map.get(q).map(|c| {
                (
                    q.to_string(),
                    c.chat.clone(),
                    c.sender.clone(),
                    Arc::clone(&c.message),
                )
            })
        });
        let message = match quoted {
            Some((qid, qchat, qsender, qmsg)) => {
                let ctx = build_quote_context_with_info(qid, &qsender, &qchat, &jid, &qmsg);
                wa::Message::text_with_context(text, ctx)
            }
            None => {
                if quote_message_id.is_some() {
                    tracing::debug!("reply_to fora do cache; enviando sem citação");
                }
                wa::Message::text(text)
            }
        };
        let res = client
            .send_message(jid, message)
            .await
            .map_err(|e| e.to_string())?;
        Ok(res.message_id)
    }

    async fn send_media(
        &self,
        chat_jid: &str,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<String, String> {
        let client = self.connected_client()?;
        let jid = Jid::from_str(chat_jid).map_err(|e| format!("JID inválido {chat_jid}: {e}"))?;
        let data = tokio::fs::read(path)
            .await
            .map_err(|e| format!("ler {}: {e}", path.display()))?;
        let mime = mime_for_path(path).to_string();
        let class = media_class(&mime);
        let file_name = path.file_name().map(|f| f.to_string_lossy().to_string());
        let media_type = match class {
            "image" => MediaType::Image,
            "video" => MediaType::Video,
            "audio" => MediaType::Audio,
            _ => MediaType::Document,
        };
        let upload = client
            .upload(data, media_type, UploadOptions::new())
            .await
            .map_err(|e| format!("upload: {e}"))?;
        let caption = caption.map(str::to_string);
        let message = match class {
            "image" => image_message(
                upload,
                ImageOptions {
                    caption,
                    mimetype: Some(mime),
                    ..Default::default()
                },
            ),
            "video" => video_message(
                upload,
                VideoOptions {
                    caption,
                    mimetype: Some(mime),
                    ..Default::default()
                },
            ),
            "audio" => audio_message(
                upload,
                AudioOptions {
                    mimetype: Some(mime),
                    ..Default::default()
                },
            ),
            _ => document_message(
                upload,
                DocumentOptions {
                    mimetype: Some(mime),
                    file_name: file_name.clone(),
                    title: file_name,
                    caption,
                    ..Default::default()
                },
            ),
        };
        let res = client
            .send_message(jid, message)
            .await
            .map_err(|e| e.to_string())?;
        Ok(res.message_id)
    }

    async fn download_media(
        &self,
        message_id: &str,
        media_ref: Option<&str>,
        dest_dir: &Path,
    ) -> Result<PathBuf, String> {
        let client = self.connected_client()?;
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| format!("criar {}: {e}", dest_dir.display()))?;
        let cached = self
            .shared
            .cache
            .lock()
            .map
            .get(message_id)
            .map(|c| Arc::clone(&c.message));
        let (bytes, mime, name) = if let Some(msg) = cached {
            let base = msg.get_base_message();
            let (dl, mime, name): (&dyn Downloadable, Option<String>, Option<String>) =
                if let Some(m) = base.image_message.as_option() {
                    (m, m.mimetype.clone(), None)
                } else if let Some(m) = base.video_message.as_option() {
                    (m, m.mimetype.clone(), None)
                } else if let Some(m) = base.audio_message.as_option() {
                    (m, m.mimetype.clone(), None)
                } else if let Some(m) = base.document_message.as_option() {
                    (m, m.mimetype.clone(), m.file_name.clone())
                } else if let Some(m) = base.sticker_message.as_option() {
                    (m, m.mimetype.clone(), None)
                } else {
                    return Err("mensagem sem mídia".into());
                };
            (
                client
                    .download(dl)
                    .await
                    .map_err(|e| format!("download: {e}"))?,
                mime,
                name,
            )
        } else if let Some(r) = media_ref {
            let (params, mime, name) = parse_media_ref(r)?;
            (
                client
                    .download_from_params(&params)
                    .await
                    .map_err(|e| format!("download: {e}"))?,
                mime,
                name,
            )
        } else {
            return Err(
                "mídia indisponível: mensagem fora do cache e sem referência de download".into(),
            );
        };
        let ext = ext_for_mime(mime.as_deref().unwrap_or(""));
        let safe_id = sanitize(message_id);
        let file = match name.as_deref().map(sanitize).filter(|n| !n.is_empty()) {
            Some(n) => format!("{safe_id}-{n}"),
            None => format!("{safe_id}.{ext}"),
        };
        let path = dest_dir.join(file);
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| format!("gravar {}: {e}", path.display()))?;
        Ok(path)
    }

    async fn mark_read(&self, chat_jid: &str) -> Result<(), String> {
        let client = self.connected_client()?;
        let jid = Jid::from_str(chat_jid).map_err(|e| format!("JID inválido {chat_jid}: {e}"))?;
        client
            .chat_actions()
            .mark_chat_as_read(&jid, true, None)
            .await
            .map_err(|e| e.to_string())
    }

    async fn fetch_group(&self, jid: &str) -> Result<Value, String> {
        let client = self.connected_client()?;
        let gjid = Jid::from_str(jid).map_err(|e| format!("JID inválido {jid}: {e}"))?;
        let meta = client
            .groups()
            .get_metadata(&gjid)
            .await
            .map_err(|e| e.to_string())?;
        let participant_jid = |p: &whatsapp_rust::features::GroupParticipant| {
            p.phone_number.as_ref().unwrap_or(&p.jid).to_non_ad_string()
        };
        let participants: Vec<String> = meta.participants.iter().map(participant_jid).collect();
        let admins: Vec<String> = meta
            .participants
            .iter()
            .filter(|p| p.is_admin())
            .map(participant_jid)
            .collect();
        Ok(json!({
            "jid": meta.id.to_non_ad_string(),
            "subject": meta.subject,
            "description": meta.description,
            "participants": participants,
            "admins": admins,
            "size": meta.size.unwrap_or(participants.len() as u32),
            "is_announcement": meta.is_announcement,
            "is_locked": meta.is_locked,
        }))
    }

    async fn resolve_phone(&self, phone: &str) -> Result<Option<String>, String> {
        let client = self.connected_client()?;
        let res = client
            .contacts()
            .is_on_whatsapp(&[Jid::pn(phone)])
            .await
            .map_err(|e| e.to_string())?;
        Ok(res.first().filter(|r| r.is_registered).map(|r| {
            r.pn_jid
                .clone()
                .unwrap_or_else(|| r.jid.clone())
                .to_non_ad_string()
        }))
    }

    fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
            && self
                .client
                .lock()
                .as_ref()
                .is_some_and(|c| c.is_connected())
    }

    fn me(&self) -> Option<Value> {
        self.shared.me.lock().clone()
    }
}

impl Shared {
    async fn handle(&self, event: Arc<WaEvent>, client: Arc<Client>) {
        let mut out: Vec<BackendEvent> = Vec::new();
        match &*event {
            WaEvent::PairingQrCode(q) => out.push(BackendEvent::Qr {
                code: q.code.clone(),
                timeout_secs: q.timeout.as_secs(),
            }),
            WaEvent::PairingCode(c) => out.push(BackendEvent::PairCode {
                code: c.code.clone(),
            }),
            WaEvent::Connected(_) => {
                self.connected.store(true, Ordering::SeqCst);
                let me = me_json(&client);
                *self.me.lock() = Some(me.clone());
                out.push(BackendEvent::Connected { me });
            }
            WaEvent::Disconnected(d) => {
                self.connected.store(false, Ordering::SeqCst);
                out.push(BackendEvent::Disconnected {
                    reason: d.reason.to_string(),
                });
            }
            WaEvent::LoggedOut(_) => {
                self.connected.store(false, Ordering::SeqCst);
                out.push(BackendEvent::LoggedOut);
            }
            // Outra conexão usou as mesmas credenciais; a biblioteca desliga a
            // reconexão automática e, em geral, o servidor passa a responder 401.
            WaEvent::StreamReplaced(_) => {
                self.connected.store(false, Ordering::SeqCst);
                tracing::warn!(
                    "sessão do WhatsApp substituída por outra conexão (conflict=replaced)"
                );
                out.push(BackendEvent::Disconnected {
                    reason: "sessão substituída por outra conexão com as mesmas credenciais".into(),
                });
            }
            WaEvent::Messages(batch) => {
                for m in batch.messages.iter() {
                    let info: &MessageInfo = &m.info;
                    let inc = convert(info, &m.message);
                    self.cache.lock().insert(
                        inc.id.clone(),
                        CachedMsg {
                            chat: info.source.chat.clone(),
                            sender: info.source.sender.clone(),
                            message: Arc::clone(&m.message),
                        },
                    );
                    out.push(BackendEvent::Message(inc));
                }
            }
            WaEvent::Receipt(r) => out.push(BackendEvent::Receipt {
                chat_jid: r.source.chat.to_non_ad_string(),
                message_ids: r.message_ids.iter().map(|i| i.to_string()).collect(),
                kind: receipt_kind(&format!("{:?}", r.r#type)),
            }),
            WaEvent::ContactUpdate(c) => {
                let jid = c.jid.to_non_ad_string();
                let name = c
                    .action
                    .full_name
                    .clone()
                    .or_else(|| c.action.first_name.clone());
                out.push(BackendEvent::ContactUpdate(WaContact {
                    account_id: self.account_id.clone(),
                    phone: phone_from_jid(&jid),
                    jid,
                    name,
                    push_name: None,
                }));
            }
            WaEvent::PushNameUpdate(p) => {
                let jid = p.jid.to_non_ad_string();
                out.push(BackendEvent::ContactUpdate(WaContact {
                    account_id: self.account_id.clone(),
                    phone: phone_from_jid(&jid),
                    jid,
                    name: None,
                    push_name: Some(p.new_push_name.clone()),
                }));
            }
            WaEvent::GroupUpdate(g) => out.push(BackendEvent::GroupUpdate {
                jid: g.group_jid.to_non_ad_string(),
                subject: None,
                participants: vec![],
                admins: vec![],
                description: None,
            }),
            WaEvent::ArchiveUpdate(a) => out.push(BackendEvent::ChatArchived {
                jid: a.jid.to_non_ad_string(),
                archived: a.action.archived.unwrap_or(false),
            }),
            WaEvent::MarkChatAsReadUpdate(m) => out.push(BackendEvent::Receipt {
                chat_jid: m.jid.to_non_ad_string(),
                message_ids: vec![],
                kind: if m.action.read.unwrap_or(true) {
                    "read".into()
                } else {
                    "unread".into()
                },
            }),
            WaEvent::HistorySync(h) => self.history_sync(h, &client, &mut out),
            _ => {}
        }
        for ev in out {
            if self.tx.send((self.account_id.clone(), ev)).await.is_err() {
                break;
            }
        }
    }

    fn history_sync(&self, h: &LazyHistorySync, client: &Client, out: &mut Vec<BackendEvent>) {
        let me_jid = client
            .pn()
            .map(|j| j.to_non_ad_string())
            .unwrap_or_default();
        let mut stream = h.stream();
        loop {
            let conv = match stream.next_conversation() {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("history sync: {e}");
                    break;
                }
            };
            let chat_jid = Jid::from_str(&conv.id)
                .map(|j| j.to_non_ad_string())
                .unwrap_or_else(|_| conv.id.clone());
            let is_group = is_group_jid(&chat_jid);
            let name = conv.name.clone();
            if is_group && !conv.participant.is_empty() {
                out.push(BackendEvent::GroupUpdate {
                    jid: chat_jid.clone(),
                    subject: name.clone(),
                    participants: conv
                        .participant_jids()
                        .iter()
                        .map(|j| j.to_non_ad_string())
                        .collect(),
                    admins: conv
                        .admin_jids()
                        .iter()
                        .map(|j| j.to_non_ad_string())
                        .collect(),
                    description: None,
                });
            }
            if conv.archived == Some(true) {
                out.push(BackendEvent::ChatArchived {
                    jid: chat_jid.clone(),
                    archived: true,
                });
            }
            for hm in &conv.messages {
                let Some(wmi) = hm.message.as_option() else {
                    continue;
                };
                let Some(key) = wmi.key.as_option() else {
                    continue;
                };
                let Some(id) = key.id.clone() else { continue };
                let Some(msg) = wmi.message.as_option() else {
                    continue;
                };
                let from_me = key.from_me.unwrap_or(false);
                let sender_raw = if from_me {
                    me_jid.clone()
                } else {
                    key.participant
                        .clone()
                        .or_else(|| wmi.participant.clone())
                        .unwrap_or_else(|| chat_jid.clone())
                };
                let sender = Jid::from_str(&sender_raw)
                    .map(|j| j.to_non_ad_string())
                    .unwrap_or(sender_raw);
                let ts = wmi
                    .message_timestamp
                    .and_then(|t| DateTime::<Utc>::from_timestamp(t as i64, 0))
                    .unwrap_or_else(Utc::now);
                let base = msg.get_base_message();
                let (kind, text, media_ref) = classify(base);
                if let (Ok(cj), Ok(sj)) = (Jid::from_str(&chat_jid), Jid::from_str(&sender)) {
                    self.cache.lock().insert(
                        id.clone(),
                        CachedMsg {
                            chat: cj,
                            sender: sj,
                            message: Arc::new(msg.clone()),
                        },
                    );
                }
                out.push(BackendEvent::Message(IncomingMessage {
                    id,
                    chat_jid: chat_jid.clone(),
                    sender_jid: sender,
                    sender_name: wmi.push_name.clone(),
                    from_me,
                    timestamp: ts,
                    kind: kind.into(),
                    text,
                    media_ref,
                    quoted_id: quoted_id(base),
                    is_group,
                    chat_name: name.clone(),
                    sender_phone: None,
                    chat_phone: None,
                }));
            }
        }
    }
}

fn me_json(client: &Client) -> Value {
    let pn = client.pn();
    json!({
        "jid": pn.as_ref().map(|j| j.to_non_ad_string()),
        "phone": pn.as_ref().map(|j| j.user.to_string()),
        "lid": client.lid().map(|j| j.to_non_ad_string()),
        "push_name": client.push_name(),
        "platform": Value::Null,
    })
}

fn receipt_kind(debug: &str) -> String {
    match debug.to_ascii_lowercase().as_str() {
        "read" | "readself" => "read".into(),
        "played" | "playedself" => "played".into(),
        "delivered" => "delivered".into(),
        other => other.to_string(),
    }
}

fn convert(info: &MessageInfo, msg: &wa::Message) -> IncomingMessage {
    let base = msg.get_base_message();
    let (kind, text, media_ref) = classify(base);
    IncomingMessage {
        id: info.id.to_string(),
        chat_jid: info.source.chat.to_non_ad_string(),
        sender_jid: info.source.sender.to_non_ad_string(),
        sender_name: (!info.push_name.is_empty()).then(|| info.push_name.clone()),
        from_me: info.source.is_from_me,
        timestamp: info.timestamp,
        kind: kind.into(),
        text,
        media_ref,
        quoted_id: quoted_id(base),
        is_group: info.source.is_group,
        chat_name: None,
        sender_phone: info
            .source
            .sender_alt
            .as_ref()
            .and_then(|j| phone_from_jid(&j.to_non_ad_string())),
        chat_phone: if info.source.is_group {
            None
        } else if info.source.is_from_me {
            info.source
                .recipient_alt
                .as_ref()
                .and_then(|j| phone_from_jid(&j.to_non_ad_string()))
        } else {
            info.source
                .sender_alt
                .as_ref()
                .and_then(|j| phone_from_jid(&j.to_non_ad_string()))
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn media_ref(
    t: &str,
    direct_path: Option<&str>,
    media_key: Option<&[u8]>,
    file_sha256: Option<&[u8]>,
    file_enc_sha256: Option<&[u8]>,
    file_length: Option<u64>,
    mime: Option<&str>,
    name: Option<&str>,
) -> Option<String> {
    Some(
        json!({
            "t": t,
            "dp": direct_path?,
            "mk": BASE64_STANDARD.encode(media_key?),
            "sha": BASE64_STANDARD.encode(file_sha256?),
            "esha": BASE64_STANDARD.encode(file_enc_sha256?),
            "len": file_length.unwrap_or(0),
            "mime": mime,
            "name": name,
        })
        .to_string(),
    )
}

fn parse_media_ref(r: &str) -> Result<(DownloadParams, Option<String>, Option<String>), String> {
    let v: Value = serde_json::from_str(r).map_err(|e| format!("media_ref inválido: {e}"))?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let b = |k: &str| -> Result<Vec<u8>, String> {
        BASE64_STANDARD
            .decode(s(k).ok_or_else(|| format!("media_ref sem {k}"))?)
            .map_err(|e| format!("media_ref {k}: {e}"))
    };
    let media_type = match s("t").as_deref() {
        Some("image") => MediaType::Image,
        Some("video") => MediaType::Video,
        Some("audio") => MediaType::Audio,
        Some("sticker") => MediaType::Sticker,
        _ => MediaType::Document,
    };
    let params = DownloadParams::encrypted(
        s("dp").ok_or("media_ref sem dp")?,
        &b("mk")?,
        &b("sha")?,
        &b("esha")?,
        v.get("len").and_then(|x| x.as_u64()).unwrap_or(0),
        media_type,
    );
    Ok((params, s("mime"), s("name")))
}

fn classify(base: &wa::Message) -> (&'static str, Option<String>, Option<String>) {
    if let Some(m) = base.image_message.as_option() {
        let r = media_ref(
            "image",
            m.direct_path.as_deref(),
            m.media_key.as_deref(),
            m.file_sha256.as_deref(),
            m.file_enc_sha256.as_deref(),
            m.file_length,
            m.mimetype.as_deref(),
            None,
        );
        return ("image", m.caption.clone(), r);
    }
    if let Some(m) = base.video_message.as_option() {
        let r = media_ref(
            "video",
            m.direct_path.as_deref(),
            m.media_key.as_deref(),
            m.file_sha256.as_deref(),
            m.file_enc_sha256.as_deref(),
            m.file_length,
            m.mimetype.as_deref(),
            None,
        );
        return ("video", m.caption.clone(), r);
    }
    if let Some(m) = base.audio_message.as_option() {
        let r = media_ref(
            "audio",
            m.direct_path.as_deref(),
            m.media_key.as_deref(),
            m.file_sha256.as_deref(),
            m.file_enc_sha256.as_deref(),
            m.file_length,
            m.mimetype.as_deref(),
            None,
        );
        return ("audio", None, r);
    }
    if let Some(m) = base.document_message.as_option() {
        let r = media_ref(
            "document",
            m.direct_path.as_deref(),
            m.media_key.as_deref(),
            m.file_sha256.as_deref(),
            m.file_enc_sha256.as_deref(),
            m.file_length,
            m.mimetype.as_deref(),
            m.file_name.as_deref(),
        );
        return (
            "document",
            m.caption.clone().or_else(|| m.file_name.clone()),
            r,
        );
    }
    if let Some(m) = base.sticker_message.as_option() {
        let r = media_ref(
            "sticker",
            m.direct_path.as_deref(),
            m.media_key.as_deref(),
            m.file_sha256.as_deref(),
            m.file_enc_sha256.as_deref(),
            m.file_length,
            m.mimetype.as_deref(),
            None,
        );
        return ("sticker", None, r);
    }
    if let Some(m) = base.location_message.as_option() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(n) = &m.name {
            parts.push(n.clone());
        }
        if let Some(a) = &m.address {
            parts.push(a.clone());
        }
        if let (Some(lat), Some(lon)) = (m.degrees_latitude, m.degrees_longitude) {
            parts.push(format!("({lat}, {lon})"));
        }
        return (
            "location",
            (!parts.is_empty()).then(|| parts.join(" ")),
            None,
        );
    }
    if let Some(t) = base.text_content() {
        return ("text", Some(t.to_string()), None);
    }
    ("other", None, None)
}

fn quoted_id(base: &wa::Message) -> Option<String> {
    let ctx = base
        .extended_text_message
        .as_option()
        .and_then(|m| m.context_info.as_option())
        .or_else(|| {
            base.image_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.video_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.audio_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.document_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.sticker_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })
        .or_else(|| {
            base.location_message
                .as_option()
                .and_then(|m| m.context_info.as_option())
        })?;
    ctx.stanza_id.clone()
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}
