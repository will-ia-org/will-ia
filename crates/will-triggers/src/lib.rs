//! Triggers universais (`docs/30`): fontes de evento → filtro → acordar uma
//! thread do Claude Code com `prompt` + contexto do evento. Dispara e esquece.
//!
//! Fontes hoje: **WhatsApp** (evento `WhatsAppMessage` no barramento, emitido
//! pelo módulo WhatsApp ao indexar) e **Gmail** (varredura periódica via
//! [`MailSource`], implementado no servidor sobre o módulo Google). O
//! agendamento por tempo é o `will-scheduler` (mesma saída, outra entrada).
//! Fonte nova = produzir um [`TriggerEvent`] e chamar [`TriggersModule::dispatch`].

use chrono::{DateTime, Local, Utc};
use parking_lot::RwLock;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use will_core::settings::TriggersSettings;
use will_core::trigger::{fired_message, TriggerFilter};
use will_core::{
    Event, EventBus, Module, ModuleStatus, Origin, SharedThreadHost, ThreadId, Trigger,
    TriggerEvent, TriggerId, TriggerRun, TriggerSource,
};
use will_storage::triggers::NewTrigger;
use will_storage::Storage;

pub const FAMILY: &str = "triggers";
const NAME_MAX: usize = 80;
const PROMPT_MAX: usize = 8_000;

/// Fonte de e-mails (o servidor liga ao módulo Google). Devolve os itens no
/// formato de `GoogleModule::gmail_poll` (`from`, `subject`, `body_text`,
/// `internal_date_ms`…).
#[async_trait::async_trait]
pub trait MailSource: Send + Sync {
    async fn poll(
        &self,
        account: Option<&str>,
        query: &str,
        max: u64,
    ) -> Result<Vec<Value>, String>;
    /// Contas conectadas com Gmail: `(id, e-mail)`.
    async fn accounts(&self) -> Vec<(String, String)>;
}

/// `account` da trigger casa com uma conta conectada (id ou e-mail, sem
/// distinção de maiúsculas)? Vazio = qualquer.
fn account_known(account: &str, accounts: &[(String, String)]) -> bool {
    let a = account.trim();
    a.is_empty()
        || accounts
            .iter()
            .any(|(id, email)| id.eq_ignore_ascii_case(a) || email.eq_ignore_ascii_case(a))
}

pub struct TriggersDeps {
    pub storage: Storage,
    pub host: SharedThreadHost,
    pub bus: EventBus,
    pub mail: Option<Arc<dyn MailSource>>,
}

/// Entrada de criação/edição (UI/API).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct TriggerInput {
    pub name: Option<String>,
    pub source: Option<String>,
    pub enabled: Option<bool>,
    pub filter: Option<TriggerFilter>,
    pub prompt: Option<String>,
    /// Thread dona; vazio/`None` na criação = criar no primeiro disparo.
    pub thread_id: Option<String>,
    /// Título da thread a criar (quando `thread_id` vazio).
    pub thread_title: Option<String>,
    pub cooldown_secs: Option<u32>,
    pub debounce_secs: Option<u32>,
}

/// Trigger + campos derivados para a lista.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TriggerView {
    #[serde(flatten)]
    pub trigger: Trigger,
    pub source_label: String,
    pub thread_name: Option<String>,
    pub thread_exists: bool,
    pub last_fired_local: Option<String>,
}

/// Mensagens acumuladas de uma (trigger, conversa) esperando o debounce.
struct Pending {
    trigger: Trigger,
    events: Vec<TriggerEvent>,
    first_at: std::time::Instant,
    /// Incrementa a cada mensagem nova: o temporizador só dispara se ninguém
    /// chegou depois dele (ou se o teto de espera venceu).
    generation: u64,
}

pub struct TriggersModule {
    deps: TriggersDeps,
    cfg: RwLock<TriggersSettings>,
    cancel: RwLock<Option<CancellationToken>>,
    /// Debounce do WhatsApp por (trigger, chat).
    pending: parking_lot::Mutex<std::collections::HashMap<(TriggerId, String), Pending>>,
}

impl TriggersModule {
    pub fn new(deps: TriggersDeps, cfg: TriggersSettings) -> Arc<Self> {
        Arc::new(Self {
            deps,
            cfg: RwLock::new(cfg),
            cancel: RwLock::new(None),
            pending: parking_lot::Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub fn config(&self) -> TriggersSettings {
        self.cfg.read().clone()
    }

    pub fn enabled(&self) -> bool {
        self.cfg.read().enabled
    }

    pub fn has_mail_source(&self) -> bool {
        self.deps.mail.is_some()
    }

    /// Aplica configuração nova; liga/desliga os laços conforme `enabled`.
    pub fn update_config(self: &Arc<Self>, cfg: TriggersSettings) {
        let restart = {
            let cur = self.cfg.read();
            cur.enabled != cfg.enabled || cur.gmail_poll_minutes != cfg.gmail_poll_minutes
        };
        *self.cfg.write() = cfg;
        if restart {
            self.stop();
        }
        self.start();
    }

    /// Sobe os laços (idempotente).
    pub fn start(self: &Arc<Self>) {
        if !self.enabled() {
            self.stop();
            self.set_status(ModuleStatus::Disabled);
            return;
        }
        if self.cancel.read().is_some() {
            return;
        }
        let token = CancellationToken::new();
        *self.cancel.write() = Some(token.clone());
        let me = self.clone();
        let t1 = token.clone();
        tokio::spawn(async move { me.whatsapp_loop(t1).await });
        let me = self.clone();
        tokio::spawn(async move { me.gmail_loop(token).await });
        self.set_status(ModuleStatus::Running);
        tracing::info!(module = FAMILY, status = "running", "triggers no ar");
    }

    pub fn stop(&self) {
        if let Some(t) = self.cancel.write().take() {
            t.cancel();
            tracing::info!(module = FAMILY, status = "stopped", "triggers paradas");
        }
    }

    fn set_status(&self, status: ModuleStatus) {
        self.deps.bus.emit(Event::ModuleStatus {
            module: Module::Triggers,
            status,
        });
    }

    // ----- WhatsApp -----

    async fn whatsapp_loop(self: Arc<Self>, token: CancellationToken) {
        let mut rx = self.deps.bus.subscribe();
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                r = rx.recv() => match r {
                    Ok(Event::WhatsAppMessage { account_id, msg_id, chat_jid, chat_name, sender_jid, sender_name, sender_phone, from_me, is_group, kind, text, at }) => {
                        let ev = whatsapp_event(&account_id, &msg_id, &chat_jid, chat_name.as_deref(), &sender_jid, sender_name.as_deref(), sender_phone.as_deref(), from_me, is_group, &kind, text.as_deref(), at);
                        let me = self.clone();
                        tokio::spawn(async move { me.dispatch(TriggerSource::WhatsApp, ev).await });
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(n, "triggers: barramento atrasou; mensagens do WhatsApp perdidas");
                    }
                    Err(_) => break,
                }
            }
        }
    }

    /// Entrega um evento a todas as triggers habilitadas da fonte que casarem.
    /// No WhatsApp, triggers com `debounce_secs > 0` acumulam por conversa e
    /// disparam uma vez quando a pessoa para de escrever.
    pub async fn dispatch(self: &Arc<Self>, source: TriggerSource, ev: TriggerEvent) {
        let list = match self.deps.storage.list_triggers_by_source(source).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "triggers: falha ao listar");
                return;
            }
        };
        for t in list.into_iter().filter(|t| t.enabled) {
            if !t.filter.matches(&ev) {
                continue;
            }
            if source == TriggerSource::WhatsApp && t.debounce_secs > 0 {
                self.debounce(t, ev.clone());
            } else {
                self.fire(t, ev.clone()).await;
            }
        }
    }

    /// Guarda a mensagem no balde (trigger, chat) e (re)arma o temporizador:
    /// dispara quando passar `debounce_secs` sem mensagem nova ou quando a
    /// primeira mensagem do balde completar `debounce_max_wait_secs`.
    fn debounce(self: &Arc<Self>, t: Trigger, ev: TriggerEvent) {
        let key = (t.id, ev.chat_jid.clone());
        let wait = Duration::from_secs(u64::from(t.debounce_secs));
        let max_wait =
            Duration::from_secs(u64::from(self.cfg.read().debounce_max_wait_secs.max(1)));
        let generation = {
            let mut map = self.pending.lock();
            let p = map.entry(key.clone()).or_insert_with(|| Pending {
                trigger: t.clone(),
                events: Vec::new(),
                first_at: std::time::Instant::now(),
                generation: 0,
            });
            p.trigger = t;
            p.events.push(ev);
            p.generation += 1;
            tracing::debug!(trigger = %key.0, chat = %key.1, acumuladas = p.events.len(), "triggers: debounce (re)armado");
            p.generation
        };
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let batch = {
                let mut map = me.pending.lock();
                let Some(p) = map.get(&key) else { return };
                let quiet = p.generation == generation;
                let capped = p.first_at.elapsed() >= max_wait;
                if !quiet && !capped {
                    return; // chegou mensagem depois: o temporizador dela assume
                }
                map.remove(&key)
            };
            if let Some(p) = batch {
                let merged = merge_events(p.events);
                me.fire(p.trigger, merged).await;
            }
        });
    }

    // ----- Gmail -----

    async fn gmail_loop(self: Arc<Self>, token: CancellationToken) {
        let Some(mail) = self.deps.mail.clone() else {
            tracing::info!(
                "triggers: sem fonte de e-mail (módulo Google ausente); laço do Gmail não sobe"
            );
            return;
        };
        loop {
            let minutes = self.cfg.read().gmail_poll_minutes.max(1);
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(u64::from(minutes) * 60)) => {}
            }
            if token.is_cancelled() {
                break;
            }
            self.gmail_poll_once(&mail).await;
        }
    }

    /// Uma varredura: agrupa triggers por (conta, query), consulta uma vez por
    /// grupo e aplica o cursor de cada trigger. Também é o `Varrer agora` da UI.
    pub async fn gmail_poll_once(&self, mail: &Arc<dyn MailSource>) -> usize {
        let list = match self
            .deps
            .storage
            .list_triggers_by_source(TriggerSource::Gmail)
            .await
        {
            Ok(l) => l.into_iter().filter(|t| t.enabled).collect::<Vec<_>>(),
            Err(e) => {
                tracing::error!(error = %e, "triggers: falha ao listar (gmail)");
                return 0;
            }
        };
        if list.is_empty() {
            return 0;
        }
        let max = u64::from(self.cfg.read().gmail_max_per_poll.max(1));
        let mut groups: BTreeMap<(String, String), Vec<Trigger>> = BTreeMap::new();
        for t in list {
            let key = (
                t.filter.account.trim().to_string(),
                t.filter.gmail_query.trim().to_string(),
            );
            groups.entry(key).or_default().push(t);
        }
        let mut fired = 0usize;
        let accounts = mail.accounts().await;
        for ((account, query), triggers) in groups {
            if !account_known(&account, &accounts) {
                // Conta inexistente: pausa com motivo visível (a UI mostra em
                // amarelo) em vez de falhar em silêncio a cada varredura.
                let reason = format!(
                    "conta Google '{account}' não conectada ao Gmail — para filtrar remetente use a condição from"
                );
                for t in triggers {
                    tracing::warn!(trigger = %t.id, account = %account, "triggers: {reason}; pausada");
                    let _ = self
                        .deps
                        .storage
                        .record_trigger_run(t.id, None, false, Some(&reason), None, Some(&reason))
                        .await;
                }
                self.deps.bus.emit(Event::TriggersChanged);
                continue;
            }
            // Conta vazia = todas as contas conectadas; cursor por conta.
            let targets: Vec<String> = if account.is_empty() {
                accounts.iter().map(|(_, email)| email.clone()).collect()
            } else {
                vec![account.clone()]
            };
            if targets.is_empty() {
                tracing::debug!(query = %query, "triggers: nenhuma conta Gmail conectada; nada a varrer");
                continue;
            }
            // Uma consulta por conta, compartilhada entre as triggers do grupo.
            let mut per_account: Vec<(String, Vec<Value>)> = Vec::new();
            for acct in &targets {
                match mail.poll(Some(acct.as_str()), &query, max.max(20)).await {
                    Ok(items) => per_account.push((acct.clone(), items)),
                    Err(e) => {
                        tracing::warn!(account = %acct, query = %query, error = %e, "triggers: varredura do Gmail falhou")
                    }
                }
            }
            for t in triggers {
                let mut cursors = parse_cursors(t.cursor.as_deref());
                let mut changed = false;
                for (acct, items) in &per_account {
                    let newest = items
                        .iter()
                        .filter_map(|v| v.get("internal_date_ms").and_then(|x| x.as_i64()))
                        .max();
                    let Some(cursor) = cursor_for(&cursors, acct) else {
                        // Primeira varredura desta conta: só marca a posição (não dispara
                        // pelo que já estava na caixa).
                        let c = newest.unwrap_or_else(|| Utc::now().timestamp_millis());
                        cursors.insert(acct.clone(), c);
                        changed = true;
                        tracing::info!(trigger = %t.id, account = %acct, cursor = c, "triggers: cursor do Gmail inicializado");
                        continue;
                    };
                    let mut fresh: Vec<&Value> = items
                        .iter()
                        .filter(|v| {
                            v.get("internal_date_ms")
                                .and_then(|x| x.as_i64())
                                .map(|ms| ms > cursor)
                                .unwrap_or(false)
                        })
                        .collect();
                    fresh.sort_by_key(|v| v.get("internal_date_ms").and_then(|x| x.as_i64()));
                    fresh.truncate(max as usize);
                    let mut seen: HashSet<String> = HashSet::new();
                    for v in &fresh {
                        let ev = gmail_event(v);
                        if !seen.insert(ev.ref_id.clone()) {
                            continue;
                        }
                        if t.filter.matches(&ev) {
                            self.fire(t.clone(), ev).await;
                            fired += 1;
                        }
                    }
                    if let Some(n) = newest.filter(|n| *n > cursor) {
                        cursors.insert(acct.clone(), n);
                        changed = true;
                    }
                }
                if changed {
                    let json = serde_json::to_string(&cursors).unwrap_or_default();
                    let _ = self
                        .deps
                        .storage
                        .set_trigger_cursor(t.id, Some(&json))
                        .await;
                }
            }
        }
        fired
    }

    // ----- disparo -----

    /// Entrega `prompt + contexto` na thread (criando-a se preciso) e registra.
    /// Não espera a resposta do agente.
    pub async fn fire(&self, t: Trigger, ev: TriggerEvent) {
        if t.cooldown_secs > 0 {
            if let Some(last) = t.last_fired_at {
                let since = (Utc::now() - last).num_seconds();
                if since < i64::from(t.cooldown_secs) {
                    tracing::debug!(trigger = %t.id, since, cooldown = t.cooldown_secs, "triggers: em cooldown; ignorado");
                    return;
                }
            }
        }
        let now_local = Local::now().format("%d/%m/%Y %H:%M").to_string();
        let text = fired_message(&t, &ev, &now_local);
        let outcome = self.deliver(&t, &text).await;
        let (thread_id, ok, detail) = match &outcome {
            Ok(th) => (Some(*th), true, None),
            Err(e) => (None, false, Some(e.as_str())),
        };
        let input = Some(ev.summary.as_str()).filter(|s| !s.is_empty());
        if let Err(e) = self
            .deps
            .storage
            .record_trigger_run(t.id, thread_id, ok, detail, input, None)
            .await
        {
            tracing::error!(error = %e, "triggers: falha ao registrar disparo");
        }
        match &outcome {
            Ok(th) => {
                tracing::info!(trigger = %t.id, thread_id = %th, source = t.source.as_str(), "trigger \"{}\" disparou", t.name)
            }
            Err(e) => {
                tracing::error!(trigger = %t.id, error = %e, "trigger \"{}\" falhou ao disparar", t.name)
            }
        }
        self.deps.bus.emit(Event::TriggerFired {
            trigger_id: t.id,
            thread_id,
            name: t.name.clone(),
            ok,
            detail: detail.map(|d| d.to_string()),
        });
        self.deps.bus.emit(Event::TriggersChanged);
        if self.cfg.read().notify_on_fire {
            let body = match &outcome {
                Ok(_) => format!("\"{}\" disparou: {}", t.name, ev.summary),
                Err(e) => format!("\"{}\" não pôde ser entregue: {e}", t.name),
            };
            self.deps.bus.emit(Event::Notification {
                title: "Trigger".into(),
                body,
            });
        }
    }

    async fn deliver(&self, t: &Trigger, text: &str) -> Result<ThreadId, String> {
        let host = &self.deps.host;
        let existing = match t.thread_id {
            Some(id) => host.get_thread(id).await.ok().map(|x| x.id),
            None => None,
        };
        let thread = match existing {
            Some(id) => id,
            None => {
                let title = t
                    .thread_title
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| format!("{} ({})", t.name, t.source.label()));
                let created = host
                    .revive_thread(&t.origin, Some(&title), t.claude_session_id.as_deref())
                    .await?;
                let _ = self
                    .deps
                    .storage
                    .set_trigger_thread(t.id, created.id, created.claude_session_id.as_deref())
                    .await;
                tracing::info!(trigger = %t.id, thread_id = %created.id, "triggers: thread criada para a trigger");
                created.id
            }
        };
        host.send_message_from(
            thread,
            text,
            &[],
            Some(&format!("trigger:{}", t.source.as_str())),
        )
        .await?;
        Ok(thread)
    }

    // ----- operações (API/UI) -----

    fn validate(name: &str, prompt: &str) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("nome obrigatório".into());
        }
        if name.chars().count() > NAME_MAX {
            return Err(format!("nome com mais de {NAME_MAX} caracteres"));
        }
        if prompt.trim().is_empty() {
            return Err("prompt obrigatório".into());
        }
        if prompt.chars().count() > PROMPT_MAX {
            return Err(format!("prompt com mais de {PROMPT_MAX} caracteres"));
        }
        Ok(())
    }

    fn parse_thread(s: Option<&str>) -> Result<Option<ThreadId>, String> {
        match s.map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => ThreadId::parse(id)
                .map(Some)
                .map_err(|_| "thread_id inválido".to_string()),
            None => Ok(None),
        }
    }

    pub async fn create(
        &self,
        input: TriggerInput,
        created_by: &str,
    ) -> Result<TriggerView, String> {
        if !self.enabled() {
            return Err("triggers desligadas nas configurações".into());
        }
        let cfg = self.config();
        let n = self
            .deps
            .storage
            .count_enabled_triggers()
            .await
            .map_err(|e| e.to_string())?;
        if n >= i64::from(cfg.max_triggers) {
            return Err(format!(
                "limite de {} triggers ativas atingido",
                cfg.max_triggers
            ));
        }
        let name = input.name.unwrap_or_default();
        let prompt = input.prompt.unwrap_or_default();
        Self::validate(&name, &prompt)?;
        let source = input
            .source
            .as_deref()
            .and_then(TriggerSource::parse)
            .ok_or("fonte inválida (whatsapp | gmail)")?;
        if source == TriggerSource::Gmail {
            let Some(mail) = &self.deps.mail else {
                return Err("fonte Gmail indisponível: conecte uma conta Google com Gmail".into());
            };
            if let Some(f) = &input.filter {
                self.check_gmail_account(mail, &f.account).await?;
            }
        }
        let thread_id = Self::parse_thread(input.thread_id.as_deref())?;
        let (origin, session) = match thread_id {
            Some(id) => {
                let t = self
                    .deps
                    .host
                    .get_thread(id)
                    .await
                    .map_err(|_| "thread não encontrada".to_string())?;
                (t.origin, t.claude_session_id)
            }
            None => (Origin::Ui, None),
        };
        let t = self
            .deps
            .storage
            .insert_trigger(&NewTrigger {
                name: name.trim().to_string(),
                source,
                enabled: input.enabled.unwrap_or(true),
                filter: input.filter.unwrap_or_default(),
                prompt: prompt.trim().to_string(),
                thread_id,
                thread_title: input
                    .thread_title
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                claude_session_id: session,
                origin,
                cooldown_secs: input.cooldown_secs.unwrap_or(cfg.default_cooldown_secs),
                debounce_secs: input.debounce_secs.unwrap_or(cfg.default_debounce_secs),
                created_by: created_by.into(),
            })
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(trigger = %t.id, source = t.source.as_str(), "trigger criada: {}", t.name);
        self.deps.bus.emit(Event::TriggersChanged);
        self.view(t).await
    }

    pub async fn update(&self, id: TriggerId, input: TriggerInput) -> Result<TriggerView, String> {
        let mut t = self
            .deps
            .storage
            .get_trigger(id)
            .await
            .map_err(|_| "trigger não encontrada".to_string())?;
        if let Some(n) = input.name {
            t.name = n.trim().to_string();
        }
        if let Some(p) = input.prompt {
            t.prompt = p.trim().to_string();
        }
        Self::validate(&t.name, &t.prompt)?;
        if let Some(s) = input.source.as_deref() {
            t.source = TriggerSource::parse(s).ok_or("fonte inválida (whatsapp | gmail)")?;
        }
        if let Some(e) = input.enabled {
            t.enabled = e;
        }
        if let Some(f) = input.filter {
            t.filter = f;
        }
        if t.source == TriggerSource::Gmail {
            if let Some(mail) = &self.deps.mail {
                self.check_gmail_account(mail, &t.filter.account).await?;
            }
        }
        if let Some(c) = input.cooldown_secs {
            t.cooldown_secs = c;
        }
        if let Some(d) = input.debounce_secs {
            t.debounce_secs = d;
        }
        if let Some(title) = input.thread_title {
            t.thread_title = Some(title.trim().to_string()).filter(|s| !s.is_empty());
        }
        if let Some(raw) = input.thread_id.as_deref() {
            let new_thread = Self::parse_thread(Some(raw))?;
            if new_thread != t.thread_id {
                t.thread_id = new_thread;
                match new_thread {
                    Some(tid) => {
                        let th = self
                            .deps
                            .host
                            .get_thread(tid)
                            .await
                            .map_err(|_| "thread não encontrada".to_string())?;
                        t.origin = th.origin;
                        t.claude_session_id = th.claude_session_id;
                    }
                    None => {
                        t.origin = Origin::Ui;
                        t.claude_session_id = None;
                    }
                }
            }
        }
        let t = self
            .deps
            .storage
            .update_trigger(&t)
            .await
            .map_err(|e| e.to_string())?;
        self.deps.bus.emit(Event::TriggersChanged);
        self.view(t).await
    }

    async fn check_gmail_account(
        &self,
        mail: &Arc<dyn MailSource>,
        account: &str,
    ) -> Result<(), String> {
        let accounts = mail.accounts().await;
        if account_known(account, &accounts) {
            return Ok(());
        }
        let known: Vec<&str> = accounts.iter().map(|(_, e)| e.as_str()).collect();
        Err(format!(
            "conta Google '{}' não conectada ao Gmail (conectadas: {}). Para filtrar por remetente/domínio use uma condição `from`, ex.: *@devmania*",
            account.trim(),
            if known.is_empty() { "nenhuma".to_string() } else { known.join(", ") }
        ))
    }

    pub async fn delete(&self, id: TriggerId) -> Result<(), String> {
        self.deps
            .storage
            .delete_trigger(id)
            .await
            .map_err(|_| "trigger não encontrada".to_string())?;
        self.deps.bus.emit(Event::TriggersChanged);
        Ok(())
    }

    pub async fn get(&self, id: TriggerId) -> Result<TriggerView, String> {
        let t = self
            .deps
            .storage
            .get_trigger(id)
            .await
            .map_err(|_| "trigger não encontrada".to_string())?;
        self.view(t).await
    }

    pub async fn list(&self) -> Result<Vec<TriggerView>, String> {
        let list = self
            .deps
            .storage
            .list_triggers()
            .await
            .map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(list.len());
        for t in list {
            out.push(self.view(t).await?);
        }
        Ok(out)
    }

    pub async fn runs(&self, id: TriggerId) -> Result<Vec<TriggerRun>, String> {
        self.deps
            .storage
            .list_trigger_runs(id, 50)
            .await
            .map_err(|e| e.to_string())
    }

    /// Dispara com um evento de exemplo (ignora filtro e cooldown): valida o
    /// prompt e a thread sem esperar uma mensagem real.
    pub async fn test(&self, id: TriggerId) -> Result<TriggerView, String> {
        let mut t = self
            .deps
            .storage
            .get_trigger(id)
            .await
            .map_err(|_| "trigger não encontrada".to_string())?;
        t.cooldown_secs = 0;
        let ev = sample_event(t.source);
        self.fire(t, ev).await;
        self.get(id).await
    }

    async fn view(&self, t: Trigger) -> Result<TriggerView, String> {
        let (thread_name, thread_exists) = match t.thread_id {
            Some(id) => match self.deps.host.get_thread(id).await {
                Ok(th) => (th.title.clone(), true),
                Err(_) => (None, false),
            },
            None => (None, false),
        };
        Ok(TriggerView {
            source_label: t.source.label().to_string(),
            last_fired_local: t
                .last_fired_at
                .map(|d| d.with_timezone(&Local).format("%d/%m %H:%M").to_string()),
            thread_name,
            thread_exists,
            trigger: t,
        })
    }
}

// ----- conversão de eventos -----

#[allow(clippy::too_many_arguments)]
fn whatsapp_event(
    account_id: &str,
    msg_id: &str,
    chat_jid: &str,
    chat_name: Option<&str>,
    sender_jid: &str,
    sender_name: Option<&str>,
    sender_phone: Option<&str>,
    from_me: bool,
    is_group: bool,
    kind: &str,
    text: Option<&str>,
    at: DateTime<Utc>,
) -> TriggerEvent {
    let who = sender_name
        .filter(|s| !s.is_empty())
        .or(sender_phone)
        .unwrap_or(sender_jid);
    let body = text.unwrap_or("").to_string();
    let preview: String = body.chars().take(80).collect();
    TriggerEvent {
        source: Some(TriggerSource::WhatsApp),
        account: account_id.to_string(),
        at: Some(at),
        summary: format!(
            "WhatsApp · {who}: {}",
            if preview.is_empty() {
                format!("[{kind}]")
            } else {
                preview
            }
        ),
        fields: vec![
            ("from".into(), sender_jid.to_string()),
            ("from_name".into(), sender_name.unwrap_or("").to_string()),
            ("from_phone".into(), sender_phone.unwrap_or("").to_string()),
            ("chat".into(), chat_name.unwrap_or("").to_string()),
            ("chat_jid".into(), chat_jid.to_string()),
            ("is_group".into(), is_group.to_string()),
            ("from_me".into(), from_me.to_string()),
            ("kind".into(), kind.to_string()),
        ],
        body,
        ref_id: msg_id.to_string(),
        from_me,
        is_group,
        chat_jid: chat_jid.to_string(),
    }
}

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Null) | None => String::new(),
        Some(x) => x.to_string(),
    }
}

/// Tamanho máximo do corpo do e-mail entregue à thread (caracteres).
const MAIL_BODY_MAX: usize = 8_000;

/// Limpa o texto de um e-mail para ir à thread: remove caracteres invisíveis
/// (zero-width, soft hyphen), linhas que são só moldura/tabela (`─│┬┴…`),
/// referências de link `[n]` e linhas que são só URL, colapsa espaços e linhas
/// em branco. Newsletters em HTML viram quilos disso no `text/plain`.
pub fn clean_mail_text(raw: &str) -> String {
    let invisible = |c: char| {
        matches!(
            c,
            '\u{200B}'
                | '\u{200C}'
                | '\u{200D}'
                | '\u{2060}'
                | '\u{FEFF}'
                | '\u{00AD}'
                | '\u{034F}'
        )
    };
    let text: String = raw
        .chars()
        .filter(|c| !invisible(*c))
        .map(|c| if c == '\u{00A0}' { ' ' } else { c })
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut blank_run = 0usize;
    for line in text.lines() {
        let mut l = strip_link_refs(line.trim_end());
        // só moldura / pontuação de tabela
        let frame_only = !l.trim().is_empty()
            && l.trim().chars().all(|c| {
                ('\u{2500}'..='\u{257F}').contains(&c)
                    || matches!(
                        c,
                        ' ' | '|' | '-' | '=' | '_' | '+' | '*' | '.' | '[' | ']' | ':' | '·' | '•'
                    )
            });
        if frame_only {
            continue;
        }
        let t = l.trim();
        if (t.starts_with("http://") || t.starts_with("https://")) && !t.contains(' ') {
            continue;
        }
        // colapsa espaços internos repetidos (colunas de tabela)
        let mut collapsed = String::with_capacity(l.len());
        let mut spaces = 0;
        for c in l.trim().chars() {
            if c == ' ' {
                spaces += 1;
                if spaces <= 1 {
                    collapsed.push(c);
                }
            } else {
                spaces = 0;
                collapsed.push(c);
            }
        }
        l = collapsed;
        if l.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 && !out.is_empty() {
                out.push(String::new());
            }
            continue;
        }
        blank_run = 0;
        out.push(l);
    }
    let mut s = out.join("\n").trim().to_string();
    let n = s.chars().count();
    if n > MAIL_BODY_MAX {
        s = s.chars().take(MAIL_BODY_MAX).collect();
        s.push_str(&format!(
            "\n[… corpo cortado em {MAIL_BODY_MAX} de {n} caracteres; o e-mail inteiro está no Gmail (gmail_get_message) …]"
        ));
    }
    s
}

/// Remove `[12]`/`][12]` (referências de links do text/plain gerado de HTML).
fn strip_link_refs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < chars.len() && chars[j] == ']' {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Junta as mensagens acumuladas pelo debounce num evento só: campos da
/// última, corpo com todas em ordem (`[HH:MM] texto`), `messages` = total.
pub fn merge_events(mut events: Vec<TriggerEvent>) -> TriggerEvent {
    if events.len() <= 1 {
        return events.pop().unwrap_or_default();
    }
    events.sort_by_key(|e| e.at);
    let n = events.len();
    let body = events
        .iter()
        .map(|e| {
            let when =
                e.at.map(|a| a.with_timezone(&Local).format("%H:%M").to_string())
                    .unwrap_or_default();
            let kind = e.field("kind").unwrap_or("");
            let text = if e.body.trim().is_empty() {
                format!("[{kind}]")
            } else {
                e.body.trim().to_string()
            };
            if when.is_empty() {
                text
            } else {
                format!("[{when}] {text}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let last = events.last().cloned().unwrap_or_default();
    let who = last
        .field("from_name")
        .filter(|s| !s.is_empty())
        .or_else(|| last.field("from_phone").filter(|s| !s.is_empty()))
        .unwrap_or("?")
        .to_string();
    let mut fields = last.fields.clone();
    fields.push(("messages".into(), n.to_string()));
    fields.push((
        "first_at".into(),
        events
            .first()
            .and_then(|e| e.at)
            .map(|a| a.to_rfc3339())
            .unwrap_or_default(),
    ));
    TriggerEvent {
        summary: format!("WhatsApp · {who}: {n} mensagens"),
        fields,
        body,
        ..last
    }
}

/// Cursor do Gmail por conta. Aceita o formato antigo (um número = cursor da
/// conta única, guardado sob a chave `""` e também aplicado a qualquer conta
/// na primeira leitura) e o novo (`{"email": ms}`).
fn parse_cursors(raw: Option<&str>) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return out;
    };
    if let Ok(n) = raw.parse::<i64>() {
        out.insert(String::new(), n);
        return out;
    }
    if let Ok(m) = serde_json::from_str::<BTreeMap<String, i64>>(raw) {
        out = m;
    }
    out
}

/// Cursor da conta, ou o legado (`""`) quando a conta ainda não tem o seu.
fn cursor_for(cursors: &BTreeMap<String, i64>, acct: &str) -> Option<i64> {
    cursors.get(acct).or_else(|| cursors.get("")).copied()
}

/// Item de `gmail_poll` → evento normalizado.
pub fn gmail_event(v: &Value) -> TriggerEvent {
    let from = s(v, "from");
    let subject = s(v, "subject");
    let from_name = from
        .split('<')
        .next()
        .map(|x| x.trim().trim_matches('"').to_string())
        .unwrap_or_default();
    let at = v
        .get("internal_date_ms")
        .and_then(|x| x.as_i64())
        .and_then(DateTime::<Utc>::from_timestamp_millis);
    let attachments = v
        .get("attachments")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .map(|x| s(x, "filename"))
                .filter(|n| !n.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    TriggerEvent {
        source: Some(TriggerSource::Gmail),
        account: s(v, "account"),
        at,
        summary: format!("E-mail · {from_name}: {subject}"),
        fields: vec![
            ("from".into(), from),
            ("from_name".into(), from_name),
            ("to".into(), s(v, "to")),
            ("cc".into(), s(v, "cc")),
            ("subject".into(), subject),
            ("date".into(), s(v, "date")),
            ("labels".into(), s(v, "labels")),
            ("gmail_message_id".into(), s(v, "id")),
            ("gmail_thread_id".into(), s(v, "thread_id")),
            ("attachments".into(), attachments),
            (
                "snippet".into(),
                clean_mail_text(&s(v, "snippet"))
                    .chars()
                    .take(200)
                    .collect(),
            ),
        ],
        body: clean_mail_text(&s(v, "body_text")),
        ref_id: s(v, "id"),
        from_me: false,
        is_group: false,
        chat_jid: String::new(),
    }
}

/// Evento de exemplo por fonte (botão "Testar" da UI).
pub fn sample_event(source: TriggerSource) -> TriggerEvent {
    match source {
        TriggerSource::WhatsApp => whatsapp_event(
            "teste",
            "TESTE-1",
            "5511999999999@s.whatsapp.net",
            Some("Contato de teste"),
            "5511999999999@s.whatsapp.net",
            Some("Contato de teste"),
            Some("5511999999999"),
            false,
            false,
            "text",
            Some("Mensagem de teste da trigger (botão Testar)."),
            Utc::now(),
        ),
        TriggerSource::Gmail => gmail_event(&serde_json::json!({
            "account": "teste",
            "id": "TESTE-1",
            "thread_id": "TESTE-T1",
            "from": "Remetente de teste <teste@exemplo.com>",
            "to": "voce@exemplo.com",
            "subject": "E-mail de teste da trigger",
            "date": Utc::now().to_rfc3339(),
            "labels": ["INBOX", "UNREAD"],
            "snippet": "Corpo de teste…",
            "body_text": "Corpo de teste do e-mail (botão Testar).",
            "internal_date_ms": Utc::now().timestamp_millis(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_item_becomes_event() {
        let ev = gmail_event(&serde_json::json!({
            "account": "a@b.com", "id": "m1", "thread_id": "t1",
            "from": "João <joao@xpto.com>", "subject": "Fatura", "labels": ["INBOX"],
            "body_text": "segue a fatura", "internal_date_ms": 1_700_000_000_000i64,
            "attachments": [{"filename": "fatura.pdf"}]
        }));
        assert_eq!(ev.field("from_name"), Some("João"));
        assert_eq!(ev.field("attachments"), Some("fatura.pdf"));
        assert_eq!(ev.field("labels"), Some("INBOX"));
        assert!(ev.at.is_some());
        let f = TriggerFilter {
            conditions: vec![will_core::trigger::Condition {
                field: "from".into(),
                glob: "*@xpto.com".into(),
                negate: false,
            }],
            ..Default::default()
        };
        assert!(f.matches(&ev));
    }

    #[test]
    fn mail_text_cleanup() {
        let raw = "Encontre seleções que inspiram \u{200c} \u{200c} \u{200c}\n────────────────────\n[ [AliExpress]      \n][1]\n   -50% │.│R$16,18    \n\n\n\nVeja as ofertas\nhttps://x.y/z?track=1\n[3] https://a.b\n";
        let c = clean_mail_text(raw);
        assert!(c.starts_with("Encontre seleções que inspiram"));
        assert!(!c.contains('\u{200c}'));
        assert!(!c.contains("────"));
        assert!(!c.contains("[1]"));
        assert!(!c.contains("https://"));
        assert!(c.contains("-50% │.│R$16,18"));
        assert!(!c.contains("\n\n\n"));
        assert_eq!(strip_link_refs("texto][12] fim [x]"), "texto] fim [x]");
    }

    #[test]
    fn merge_keeps_order_and_counts() {
        let mk = |t: &str, secs: i64| {
            let mut e = whatsapp_event(
                "acc",
                t,
                "551199@s.whatsapp.net",
                Some("Pedro"),
                "551199@s.whatsapp.net",
                Some("Pedro"),
                Some("551199"),
                false,
                false,
                "text",
                Some(t),
                DateTime::<Utc>::from_timestamp(1_700_000_000 + secs, 0).unwrap(),
            );
            e.ref_id = t.into();
            e
        };
        let m = merge_events(vec![
            mk("segunda", 10),
            mk("primeira", 0),
            mk("terceira", 20),
        ]);
        assert_eq!(m.field("messages"), Some("3"));
        assert_eq!(m.summary, "WhatsApp · Pedro: 3 mensagens");
        let lines: Vec<&str> = m.body.lines().collect();
        assert!(lines[0].ends_with("primeira"));
        assert!(lines[2].ends_with("terceira"));
        assert_eq!(m.ref_id, "terceira");
        assert_eq!(merge_events(vec![mk("só", 0)]).body, "só");
    }

    #[test]
    fn cursors_legacy_and_per_account() {
        let c = parse_cursors(Some("1700"));
        assert_eq!(cursor_for(&c, "a@b.com"), Some(1700));
        let c = parse_cursors(Some(r#"{"a@b.com":5,"c@d.com":9}"#));
        assert_eq!(cursor_for(&c, "a@b.com"), Some(5));
        assert_eq!(cursor_for(&c, "zz@x.com"), None);
        assert!(parse_cursors(None).is_empty());
        assert!(parse_cursors(Some("lixo")).is_empty());
    }

    #[test]
    fn account_matching() {
        let accs = vec![("id1".to_string(), "will@gmail.com".to_string())];
        assert!(account_known("", &accs));
        assert!(account_known("WILL@gmail.com", &accs));
        assert!(account_known("id1", &accs));
        assert!(!account_known("@devmania", &accs));
    }

    #[test]
    fn whatsapp_event_summary() {
        let ev = whatsapp_event(
            "acc",
            "id1",
            "551199@s.whatsapp.net",
            Some("Pedro"),
            "551199@s.whatsapp.net",
            Some("Pedro"),
            Some("551199"),
            false,
            false,
            "text",
            Some("oi"),
            Utc::now(),
        );
        assert_eq!(ev.summary, "WhatsApp · Pedro: oi");
        assert_eq!(ev.field("text"), Some("oi"));
        assert_eq!(ev.chat_jid, "551199@s.whatsapp.net");
    }
}
