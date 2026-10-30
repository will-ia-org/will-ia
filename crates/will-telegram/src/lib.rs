//! will-telegram — canal Telegram do Will IA.
//!
//! Porte do bot de referência (Node.js): long polling via `teloxide` (só a camada de
//! requisições), roteamento próprio, uma thread do cérebro por *thread key*
//! (`<chat_id>` ou `<chat_id>:<topic_id>`), pilha de status editada ao vivo e
//! resposta final como mensagem nova. Ver `docs/08-telegram.md`.

mod commands;
pub mod conversation;
mod events;
mod handler;
mod sender;
pub mod text;
mod tool;

pub use sender::TelegramSender;
pub use tool::SendMessageTool;

use crate::conversation::Conversation;
use parking_lot::Mutex;
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use teloxide::payloads::setters::*;
use teloxide::prelude::Requester;
use teloxide::types::{AllowedUpdate, BotCommand};
use teloxide::{Bot, RequestError};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use will_core::settings::TelegramSettings;
use will_core::{
    ApprovalId, Event, EventBus, Module, ModuleStatus, Origin, SharedThreadHost, ThreadId,
    ToolProvider, WillTool,
};
use will_realtime::openai::OpenAi;
use will_storage::Storage;

/// Dependências injetadas pelo servidor.
pub struct TelegramDeps {
    pub settings: TelegramSettings,
    pub bot_token: SecretString,
    /// Chave da OpenAI para transcrição de voice notes e resumo em áudio (opcional).
    pub openai_api_key: Option<SecretString>,
    pub storage: Storage,
    pub host: SharedThreadHost,
    pub bus: EventBus,
    pub kb_path: PathBuf,
}

#[derive(Default)]
pub(crate) struct Bindings {
    by_key: HashMap<String, ThreadId>,
    by_thread: HashMap<ThreadId, String>,
}

pub(crate) struct ApprovalMsg {
    pub chat_id: i64,
    pub message_id: i32,
    pub text: String,
}

pub struct TelegramModule {
    pub(crate) settings: TelegramSettings,
    pub(crate) bot: Bot,
    pub(crate) sender: Arc<TelegramSender>,
    pub(crate) openai: Option<OpenAi>,
    pub(crate) storage: Storage,
    pub(crate) host: SharedThreadHost,
    pub(crate) bus: EventBus,
    pub(crate) kb_path: PathBuf,
    status: Mutex<ModuleStatus>,
    cancel: Mutex<Option<CancellationToken>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) convs: Mutex<HashMap<String, Arc<Conversation>>>,
    pub(crate) bindings: Mutex<Bindings>,
    pub(crate) approval_msgs: Mutex<HashMap<ApprovalId, ApprovalMsg>>,
    /// Nome no menu do Telegram (`will_noticias`) → nome real do comando da KB (`will-noticias`).
    pub(crate) repo_alias: Mutex<HashMap<String, String>>,
    pub(crate) unauthorized_notice: Mutex<HashMap<i64, Instant>>,
    pub(crate) bot_username: Mutex<Option<String>>,
    /// thread key → nome do tópico (quando conhecido).
    pub(crate) topic_names: Mutex<HashMap<String, String>>,
    offset: AtomicI64,
}

/// Long polling do `getUpdates` (segundos). O cliente HTTP precisa de timeout
/// maior que isto, senão toda espera sem mensagens vira "network error".
const POLL_TIMEOUT_SECS: u32 = 25;

/// Bot com os padrões do teloxide (proxy via env, connect 5 s) mas timeout total
/// acima do long polling. Em caso de falha ao construir o cliente, usa o padrão.
fn make_bot(token: &str) -> Bot {
    match teloxide::net::default_reqwest_settings()
        .timeout(Duration::from_secs(u64::from(POLL_TIMEOUT_SECS) + 20))
        .build()
    {
        Ok(client) => Bot::with_client(token, client),
        Err(_) => Bot::new(token),
    }
}

/// Valida um token no `getMe` → `{id, username, first_name}`.
pub async fn test_bot_token(token: &str) -> Result<serde_json::Value, String> {
    let bot = make_bot(token);
    let me = bot.get_me().await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "id": me.user.id.0,
        "username": me.user.username,
        "first_name": me.user.first_name,
    }))
}

impl TelegramModule {
    pub fn new(deps: TelegramDeps) -> Arc<Self> {
        let bot = make_bot(deps.bot_token.expose_secret());
        let sender = Arc::new(TelegramSender::new(bot.clone(), deps.settings.clone()));
        let openai = deps.openai_api_key.map(OpenAi::new);
        Arc::new(Self {
            settings: deps.settings,
            bot,
            sender,
            openai,
            storage: deps.storage,
            host: deps.host,
            bus: deps.bus,
            kb_path: deps.kb_path,
            status: Mutex::new(ModuleStatus::Stopped),
            cancel: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            convs: Mutex::new(HashMap::new()),
            bindings: Mutex::new(Bindings::default()),
            approval_msgs: Mutex::new(HashMap::new()),
            repo_alias: Mutex::new(HashMap::new()),
            unauthorized_notice: Mutex::new(HashMap::new()),
            bot_username: Mutex::new(None),
            topic_names: Mutex::new(HashMap::new()),
            offset: AtomicI64::new(0),
        })
    }

    pub fn status(&self) -> ModuleStatus {
        self.status.lock().clone()
    }

    pub(crate) fn set_status(&self, status: ModuleStatus) {
        *self.status.lock() = status.clone();
        self.bus.emit(Event::ModuleStatus {
            module: Module::Telegram,
            status,
        });
    }

    pub fn sender(&self) -> Arc<TelegramSender> {
        self.sender.clone()
    }

    /// Sobe o módulo: valida allowlists, `getMe`, `setMyCommands`, aviso de boot,
    /// limpeza de status órfãos e as duas tarefas (long polling + eventos do cérebro).
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        if self.cancel.lock().is_some() {
            return Ok(());
        }
        let span = tracing::info_span!(
            "tg_conn",
            attempt = 1u32,
            reason = "start",
            ok = tracing::field::Empty
        );
        let res = self.start_in().instrument(span.clone()).await;
        span.record("ok", res.is_ok());
        res
    }

    async fn start_in(self: &Arc<Self>) -> Result<(), String> {
        self.set_status(ModuleStatus::Starting);

        if self.settings.allowed_chats.is_empty() || self.settings.allowed_users.is_empty() {
            let msg = "allowlists vazias: configure pelo menos um chat ID e um user ID (mande /start ao bot para descobrir os seus)".to_string();
            tracing::warn!(ok = false, "telegram não subiu: allowlists vazias");
            self.set_status(ModuleStatus::Error(msg.clone()));
            return Err(msg);
        }

        let me = match self.bot.get_me().await {
            Ok(me) => me,
            Err(e) => {
                let msg = format!("getMe falhou: {e}");
                tracing::error!(ok = false, error = %e, "getMe falhou");
                self.set_status(ModuleStatus::Error(msg.clone()));
                return Err(msg);
            }
        };
        tracing::info!(bot_id = me.user.id.0, "bot conectado ao Telegram");
        *self.bot_username.lock() = me.user.username.clone();
        self.bus.log(
            "info",
            "telegram",
            format!(
                "bot @{} conectado",
                me.user.username.clone().unwrap_or_default()
            ),
        );

        // comandos: builtins + comandos da KB (.claude/commands/*.md)
        let repo = commands::load_repo_commands(&self.kb_path);
        {
            let mut alias = self.repo_alias.lock();
            alias.clear();
            for c in &repo {
                alias.insert(c.tg_name.clone(), c.real_name.clone());
            }
        }
        let mut cmds: Vec<BotCommand> = repo
            .iter()
            .map(|c| BotCommand::new(c.tg_name.clone(), c.description.clone()))
            .collect();
        cmds.extend(
            commands::BUILTIN_COMMANDS
                .iter()
                .map(|(c, d)| BotCommand::new(*c, *d)),
        );
        if let Err(e) = self.bot.set_my_commands(cmds).await {
            tracing::warn!(error = %e, "setMyCommands falhou");
        }
        self.bus.log(
            "info",
            "telegram",
            format!(
                "comandos: {} da KB + {} do bot",
                repo.len(),
                commands::BUILTIN_COMMANDS.len()
            ),
        );

        // vínculos persistidos (thread key → thread) + threads do host com origem Telegram
        let saved = self.storage.all_telegram_status().await.unwrap_or_default();
        for s in &saved {
            if let Some(tid) = s.thread_id {
                self.bind(&s.thread_key, tid);
            }
        }
        if let Ok(threads) = self.host.list_threads().await {
            for t in threads {
                if let Origin::Telegram { chat_id, topic_id } = t.origin {
                    self.bind(&text::thread_key(chat_id, topic_id), t.id);
                }
            }
        }

        let offset = self.storage.telegram_offset().await.unwrap_or(0);
        tracing::debug!(
            offset,
            bindings = self.bindings.lock().by_key.len(),
            "estado do Telegram carregado"
        );
        self.offset.store(offset, Ordering::SeqCst);

        if self.settings.announce_boot {
            self.announce_boot().await;
        }

        // status órfãos: o turno deles morreu com o reinício
        for s in saved {
            if s.status_message_id.is_some() || !s.stack.is_empty() {
                if let Some((chat_id, topic_id)) = text::parse_thread_key(&s.thread_key) {
                    tracing::info!(key = %s.thread_key, "status órfão limpo no boot");
                    let conv = self.conv(&s.thread_key, chat_id, topic_id).await;
                    conv.clear().await;
                }
            }
        }

        let cancel = CancellationToken::new();
        *self.cancel.lock() = Some(cancel.clone());
        let me1 = self.clone();
        let c1 = cancel.clone();
        let me2 = self.clone();
        let c2 = cancel.clone();
        let mut tasks = self.tasks.lock();
        tasks.push(tokio::spawn(async move { me1.poll_loop(c1).await }));
        tasks.push(tokio::spawn(async move { me2.event_loop(c2).await }));
        drop(tasks);

        self.set_status(ModuleStatus::Running);
        tracing::info!(ok = true, "bot no ar, ouvindo updates");
        self.bus
            .log("info", "telegram", "bot no ar, ouvindo updates");
        Ok(())
    }

    /// Para as tarefas (polling e eventos).
    pub async fn stop(&self) {
        let cancel = self.cancel.lock().take();
        if let Some(c) = cancel {
            c.cancel();
        }
        let tasks: Vec<JoinHandle<()>> = std::mem::take(&mut *self.tasks.lock());
        for mut t in tasks {
            if tokio::time::timeout(Duration::from_secs(2), &mut t)
                .await
                .is_err()
            {
                // o long poll pode estar pendurado; aborta
                tracing::debug!("tarefa do Telegram não encerrou em 2s; abortada");
                t.abort();
            }
        }
        tracing::info!(channel = "telegram", "bot parado");
        self.set_status(ModuleStatus::Stopped);
        self.bus.log("info", "telegram", "bot parado");
    }

    /// Linhas para `/api/telegram/status`: uma por thread key conhecida.
    pub async fn thread_status_rows(&self) -> Vec<serde_json::Value> {
        let threads = self.host.list_threads().await.unwrap_or_default();
        let mut by_key: HashMap<String, will_core::ThreadSummary> = HashMap::new();
        for t in threads {
            if let Origin::Telegram { chat_id, topic_id } = t.origin {
                by_key.insert(text::thread_key(chat_id, topic_id), t);
            }
        }
        let convs: Vec<Arc<Conversation>> = self.convs.lock().values().cloned().collect();
        let mut keys: Vec<String> = by_key.keys().cloned().collect();
        for c in &convs {
            if !keys.contains(&c.key) {
                keys.push(c.key.clone());
            }
        }
        keys.sort();
        let topic_names = self.topic_names.lock().clone();
        keys.into_iter()
            .map(|key| {
                let (chat_id, topic_id) = text::parse_thread_key(&key).unwrap_or((0, None));
                let t = by_key.get(&key);
                let conv = convs.iter().find(|c| c.key == key).map(|c| c.snapshot());
                serde_json::json!({
                    "thread_key": key,
                    "chat_id": chat_id,
                    "topic_id": topic_id,
                    "topic_name": topic_names.get(&key),
                    "thread_id": t.map(|t| t.id).or(conv.as_ref().and_then(|c| c.thread_id)),
                    "title": t.and_then(|t| t.title.clone()),
                    "status": t.map(|t| t.status.as_str()),
                    "alive": t.map(|t| t.alive).unwrap_or(false),
                    "pending": t.map(|t| t.pending).unwrap_or(0),
                    "session_expired": t.map(|t| t.session_expired).unwrap_or(false),
                    "last_activity_at": t.map(|t| t.last_activity_at),
                    "stack_len": conv.as_ref().map(|c| c.stack.len()).unwrap_or(0),
                    "status_message_id": conv.as_ref().and_then(|c| c.status_message_id),
                    "last_user_message_id": conv.as_ref().and_then(|c| c.last_user_message_id),
                })
            })
            .collect()
    }

    // ---------- vínculos thread key ↔ thread ----------

    pub(crate) fn bind(&self, key: &str, tid: ThreadId) {
        let mut b = self.bindings.lock();
        if let Some(old) = b.by_key.insert(key.to_string(), tid) {
            if old != tid {
                b.by_thread.remove(&old);
            }
        }
        b.by_thread.insert(tid, key.to_string());
    }

    pub(crate) fn unbind_key(&self, key: &str) {
        let mut b = self.bindings.lock();
        if let Some(tid) = b.by_key.remove(key) {
            b.by_thread.remove(&tid);
        }
    }

    pub(crate) fn unbind_thread(&self, tid: ThreadId) -> Option<String> {
        let mut b = self.bindings.lock();
        let key = b.by_thread.remove(&tid)?;
        b.by_key.remove(&key);
        Some(key)
    }

    pub(crate) fn thread_of(&self, key: &str) -> Option<ThreadId> {
        self.bindings.lock().by_key.get(key).copied()
    }

    pub(crate) fn key_of(&self, tid: ThreadId) -> Option<String> {
        self.bindings.lock().by_thread.get(&tid).cloned()
    }

    /// Conversa (pilha de status) da thread key; carrega o estado persistido na 1ª vez.
    pub(crate) async fn conv(
        &self,
        key: &str,
        chat_id: i64,
        topic_id: Option<i64>,
    ) -> Arc<Conversation> {
        if let Some(c) = self.convs.lock().get(key) {
            return c.clone();
        }
        let state = self.storage.telegram_status(key).await.unwrap_or_default();
        let conv = Conversation::new(
            key,
            chat_id,
            topic_id,
            self.sender.clone(),
            self.storage.clone(),
            state,
            Duration::from_secs(self.settings.status_delete_delay_secs),
        );
        let mut map = self.convs.lock();
        map.entry(key.to_string()).or_insert(conv).clone()
    }

    /// Conversa a partir só da key (eventos do cérebro).
    pub(crate) async fn conv_by_key(&self, key: &str) -> Option<Arc<Conversation>> {
        let (chat_id, topic_id) = text::parse_thread_key(key)?;
        Some(self.conv(key, chat_id, topic_id).await)
    }

    /// Thread do cérebro para a key (cache → `ensure_thread`).
    pub(crate) async fn thread_for(
        &self,
        key: &str,
        chat_id: i64,
        topic_id: Option<i64>,
        title: Option<&str>,
    ) -> Result<ThreadId, String> {
        if let Some(t) = self.thread_of(key) {
            return Ok(t);
        }
        let origin = Origin::Telegram { chat_id, topic_id };
        let summary = self.host.ensure_thread(&origin, title).await?;
        tracing::info!(channel = "telegram", key, thread_id = %summary.id, "thread vinculada à thread key");
        self.bind(key, summary.id);
        let conv = self.conv(key, chat_id, topic_id).await;
        if conv.thread_id() != Some(summary.id) {
            conv.set_thread_id(Some(summary.id)).await;
        }
        Ok(summary.id)
    }

    /// Esquece a thread no bot (vínculo, conversa e estado persistido).
    pub(crate) async fn forget_key(&self, key: &str) {
        self.unbind_key(key);
        self.convs.lock().remove(key);
        if let Err(e) = self.storage.delete_telegram_status(key).await {
            tracing::warn!(key, error = %e, "delete_telegram_status falhou");
        }
    }

    // ---------- boot ----------

    async fn announce_boot(&self) {
        let when = chrono::Local::now().format("%d/%m/%Y %H:%M:%S");
        let text = format!("🟢 <b>Bot no ar</b>\nSubi agora ({when}). Pode mandar ver.");
        for chat_id in &self.settings.allowed_chats {
            let span = sender::outbound_span(*chat_id, None, None, "boot");
            async {
                let started = Instant::now();
                let res = self
                    .sender
                    .send_one(*chat_id, None, &text, true, None, None)
                    .await;
                sender::note_outbound(&res, started, false);
            }
            .instrument(span)
            .await;
        }
    }

    // ---------- long polling ----------

    async fn poll_loop(self: Arc<Self>, cancel: CancellationToken) {
        // tentativas consecutivas com falha no getUpdates (0 = saudável)
        let mut failures: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let offset = self.offset.load(Ordering::SeqCst);
            let req = self
                .bot
                .get_updates()
                .offset(offset as i32)
                .timeout(POLL_TIMEOUT_SECS)
                .allowed_updates(vec![AllowedUpdate::Message, AllowedUpdate::CallbackQuery]);
            let res = tokio::select! {
                _ = cancel.cancelled() => break,
                r = req => r,
            };
            match res {
                Ok(updates) => {
                    if failures > 0 {
                        tracing::info!(name: "tg_conn", attempt = failures + 1, reason = "getUpdates", ok = true, "polling do Telegram restabelecido");
                        failures = 0;
                    }
                    tracing::debug!(updates = updates.len(), offset, "getUpdates");
                    for u in updates {
                        let next = i64::from(u.id.0) + 1;
                        if next > self.offset.load(Ordering::SeqCst) {
                            self.offset.store(next, Ordering::SeqCst);
                            if let Err(e) = self.storage.set_telegram_offset(next).await {
                                tracing::warn!(offset = next, error = %e, "persistir offset falhou");
                            }
                        }
                        self.handle_update(u).await;
                    }
                }
                Err(RequestError::RetryAfter(s)) => {
                    failures += 1;
                    tracing::warn!(name: "tg_conn", attempt = failures, reason = "rate_limit", ok = false, retry_after_secs = s.seconds(), "getUpdates: rate limit");
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(s.duration()) => {}
                    }
                }
                Err(e) => {
                    failures += 1;
                    tracing::error!(name: "tg_conn", attempt = failures, reason = "getUpdates", ok = false, error = %e, "getUpdates falhou; nova tentativa em 3s");
                    self.bus.log("warn", "telegram", format!("getUpdates: {e}"));
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                    }
                }
            }
        }
        tracing::info!(channel = "telegram", "long polling encerrado");
    }
}

impl ToolProvider for TelegramModule {
    fn family(&self) -> &'static str {
        "telegram"
    }

    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        vec![Arc::new(SendMessageTool {
            sender: self.sender.clone(),
        })]
    }

    fn available(&self) -> bool {
        self.status().is_running()
    }
}
