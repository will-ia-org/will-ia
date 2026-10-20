//! Ferramentas MCP `whatsapp_*` (ver docs/17-whatsapp.md).
//!
//! Todas aceitam `account` (id, rótulo ou telefone da conta pareada). Com uma
//! única conta conectada ela é usada; com várias, as ferramentas de leitura
//! "largas" (`list_chats`, `search_messages`, `list_contacts`) agregam todas as
//! contas e as demais exigem `account`.

use crate::backend::{is_group_jid, jid_from_phone, mime_for_path, phone_digits, phone_from_jid};
use crate::module::{WaAccount, WhatsAppModule};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use will_core::tools::{arg_opt_bool, arg_opt_i64, arg_opt_str, arg_str, truncate};
use will_core::{
    Module, ToolContext, ToolError, ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool,
};

pub const FAMILY: &str = "whatsapp";
/// Máximo de mensagens por chamada.
pub const MAX_MESSAGES: i64 = 200;
/// Máximo de texto (bytes) por chamada.
pub const MAX_TEXT_BYTES: usize = 50 * 1024;
const DEFAULT_LIMIT: i64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    ListChats,
    GetMessages,
    SearchMessages,
    ListContacts,
    GetGroup,
    DownloadMedia,
    MarkRead,
    SendMessage,
    SendMedia,
}

impl ToolKind {
    pub const ALL: [ToolKind; 9] = [
        ToolKind::ListChats,
        ToolKind::GetMessages,
        ToolKind::SearchMessages,
        ToolKind::ListContacts,
        ToolKind::GetGroup,
        ToolKind::DownloadMedia,
        ToolKind::MarkRead,
        ToolKind::SendMessage,
        ToolKind::SendMedia,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            ToolKind::ListChats => "whatsapp_list_chats",
            ToolKind::GetMessages => "whatsapp_get_messages",
            ToolKind::SearchMessages => "whatsapp_search_messages",
            ToolKind::ListContacts => "whatsapp_list_contacts",
            ToolKind::GetGroup => "whatsapp_get_group",
            ToolKind::DownloadMedia => "whatsapp_download_media",
            ToolKind::MarkRead => "whatsapp_mark_read",
            ToolKind::SendMessage => "whatsapp_send_message",
            ToolKind::SendMedia => "whatsapp_send_media",
        }
    }

    /// Com várias contas e sem `account`, agrega todas as contas conectadas.
    fn aggregates(&self) -> bool {
        matches!(
            self,
            ToolKind::ListChats | ToolKind::SearchMessages | ToolKind::ListContacts
        )
    }

    fn spec(&self) -> ToolSpec {
        let account_desc = if self.aggregates() {
            "Conta WhatsApp (id, rótulo ou telefone). Opcional: com uma só conta ela é usada; com várias e sem `account`, agrega todas (cada item traz `account`)."
        } else {
            "Conta WhatsApp (id, rótulo ou telefone). Opcional com uma só conta; obrigatório quando há várias contas pareadas."
        };
        let (description, mut properties, required, policy): (&str, Value, Vec<&str>, ToolPolicy) = match self {
            ToolKind::ListChats => (
                "Lista conversas do WhatsApp pessoal do usuário (índice local) com nome, id (JID), não lidas e última mensagem. Com várias contas pareadas, use `account` para filtrar ou omita para agregar.",
                json!({
                    "filter": {"type": "string", "enum": ["all", "unread", "groups", "archived"], "description": "Filtro (padrão all)."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Máximo de chats (padrão 50)."}
                }),
                vec![],
                ToolPolicy::Auto,
            ),
            ToolKind::GetMessages => (
                "Mensagens de uma conversa em ordem cronológica, com remetente, texto, tipo e message_id. Prefira `since` a carregar históricos inteiros; pagine com `before`. Com várias contas, informe `account`.",
                json!({
                    "chat_id": {"type": "string", "description": "JID do chat (ex.: 5511999999999@s.whatsapp.net, 1203...@g.us) ou nome exato do contato/grupo."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Máximo de mensagens (padrão 50, teto 200)."},
                    "before": {"type": "string", "description": "Só mensagens anteriores a este instante (ISO-8601 UTC); use `next_before` da resposta anterior."},
                    "since": {"type": "string", "description": "Só mensagens a partir deste instante (ISO-8601 UTC)."}
                }),
                vec!["chat_id"],
                ToolPolicy::Auto,
            ),
            ToolKind::SearchMessages => (
                "Busca texto nas mensagens indexadas (FTS). Retorna ocorrências com chat, remetente e data. Com várias contas e sem `account`, busca em todas.",
                json!({
                    "query": {"type": "string", "description": "Termos de busca."},
                    "chat_id": {"type": "string", "description": "Restringe a um chat (JID ou nome)."},
                    "since": {"type": "string", "description": "Só mensagens a partir deste instante (ISO-8601 UTC)."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Máximo de resultados (padrão 50)."}
                }),
                vec!["query"],
                ToolPolicy::Auto,
            ),
            ToolKind::ListContacts => (
                "Lista contatos conhecidos (nome, número, JID). `query` filtra por nome ou número. Com várias contas e sem `account`, agrega todas.",
                json!({
                    "query": {"type": "string", "description": "Filtro por nome/número (opcional)."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500, "description": "Máximo (padrão 100)."}
                }),
                vec![],
                ToolPolicy::Auto,
            ),
            ToolKind::GetGroup => (
                "Metadados de um grupo: nome, descrição, participantes e admins. Com várias contas, informe `account`.",
                json!({
                    "chat_id": {"type": "string", "description": "JID do grupo (…@g.us) ou nome do grupo."},
                    "refresh": {"type": "boolean", "description": "Força consulta ao servidor em vez do índice local."}
                }),
                vec!["chat_id"],
                ToolPolicy::Auto,
            ),
            ToolKind::DownloadMedia => (
                "Baixa a mídia de uma mensagem para a pasta inbox/whatsapp da base de conhecimento e retorna caminho, mime e tamanho. Com várias contas, informe `account` (a mesma da mensagem).",
                json!({
                    "message_id": {"type": "string", "description": "message_id retornado por whatsapp_get_messages/whatsapp_search_messages."}
                }),
                vec!["message_id"],
                ToolPolicy::Auto,
            ),
            ToolKind::MarkRead => (
                "Marca um chat como lido no índice local (e envia recibo de leitura se a opção estiver ligada). Com várias contas, informe `account`.",
                json!({
                    "chat_id": {"type": "string", "description": "JID ou nome do chat."}
                }),
                vec!["chat_id"],
                ToolPolicy::Auto,
            ),
            ToolKind::SendMessage => (
                "Envia uma mensagem de texto pelo WhatsApp pessoal do usuário. Exige aprovação. Nunca use sem pedido explícito do usuário. Com várias contas, informe `account` (de qual número enviar).",
                json!({
                    "chat_id": {"type": "string", "description": "JID do chat/contato ou nome exato do contato/grupo."},
                    "phone": {"type": "string", "description": "Telefone com DDI (só dígitos). Aceito apenas se o envio para números não salvos estiver habilitado."},
                    "text": {"type": "string", "description": "Texto da mensagem."},
                    "reply_to": {"type": "string", "description": "message_id a citar (opcional)."}
                }),
                vec!["text"],
                ToolPolicy::Confirm,
            ),
            ToolKind::SendMedia => (
                "Envia um arquivo (imagem, vídeo, áudio ou documento) que esteja dentro da base de conhecimento. Exige aprovação. Com várias contas, informe `account`.",
                json!({
                    "chat_id": {"type": "string", "description": "JID do chat/contato ou nome exato."},
                    "path": {"type": "string", "description": "Caminho do arquivo dentro da KB (absoluto ou relativo à KB)."},
                    "caption": {"type": "string", "description": "Legenda (opcional)."}
                }),
                vec!["chat_id", "path"],
                ToolPolicy::Confirm,
            ),
        };
        properties["account"] = json!({"type": "string", "description": account_desc});
        ToolSpec {
            name: self.name().to_string(),
            description: description.to_string(),
            input_schema: json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false}),
            requires: vec![Module::WhatsApp],
            default_policy: policy,
            family: FAMILY.to_string(),
        }
    }
}

/// Conta(s) sobre as quais uma chamada opera.
enum Target {
    One(Arc<WaAccount>),
    All(Vec<Arc<WaAccount>>),
}

pub struct WaTool {
    kind: ToolKind,
    module: Weak<WhatsAppModule>,
}

impl WaTool {
    pub fn new(kind: ToolKind, module: Weak<WhatsAppModule>) -> Self {
        Self { kind, module }
    }

    fn module(&self) -> Result<Arc<WhatsAppModule>, ToolError> {
        self.module
            .upgrade()
            .ok_or_else(|| ToolError::Unavailable("módulo WhatsApp encerrado".into()))
    }

    /// Aplica a regra de seleção de conta (ver doc do módulo).
    fn target(&self, m: &WhatsAppModule, args: &Value) -> Result<Target, ToolError> {
        let connected = m.connected_accounts();
        if connected.is_empty() {
            return Err(ToolError::Unavailable(
                "WhatsApp não conectado; pareie um número nas configurações".into(),
            ));
        }
        let list = || {
            m.accounts()
                .iter()
                .map(|a| a.describe())
                .collect::<Vec<_>>()
                .join("; ")
        };
        if let Some(q) = arg_opt_str(args, "account") {
            return match m.resolve_account(q) {
                Some(a) if a.is_connected() => Ok(Target::One(a)),
                Some(a) => Err(ToolError::Unavailable(format!("a conta WhatsApp {} não está conectada", a.describe()))),
                None => Err(ToolError::InvalidArgs(format!("conta WhatsApp \"{q}\" não encontrada; contas disponíveis (id — rótulo — telefone): {}", list()))),
            };
        }
        if connected.len() == 1 {
            return Ok(Target::One(
                connected
                    .into_iter()
                    .next()
                    .ok_or_else(|| ToolError::Failed("conta indisponível".into()))?,
            ));
        }
        if self.kind.aggregates() {
            return Ok(Target::All(connected));
        }
        Err(ToolError::InvalidArgs(format!(
            "há várias contas WhatsApp conectadas; informe `account` (id, rótulo ou telefone). Contas (id — rótulo — telefone): {}",
            list()
        )))
    }

    /// Rótulo da conta para o resumo de aprovação (síncrono, melhor esforço).
    fn account_label(&self, args: &Value) -> Option<String> {
        let m = self.module.upgrade()?;
        if let Some(q) = arg_opt_str(args, "account") {
            return m
                .resolve_account(q)
                .map(|a| a.label())
                .or_else(|| Some(q.to_string()));
        }
        let accounts = m.accounts();
        if accounts.len() == 1 {
            return Some(accounts[0].label());
        }
        None
    }
}

#[async_trait::async_trait]
impl WillTool for WaTool {
    fn spec(&self) -> ToolSpec {
        self.kind.spec()
    }

    fn approval_summary(&self, args: &Value) -> String {
        let via = self
            .account_label(args)
            .map(|l| format!(" (conta {l})"))
            .unwrap_or_default();
        match self.kind {
            ToolKind::SendMessage => {
                let target = arg_opt_str(args, "chat_id")
                    .or_else(|| arg_opt_str(args, "phone"))
                    .unwrap_or("?");
                let text = arg_opt_str(args, "text").unwrap_or("");
                format!(
                    "Enviar WhatsApp{via} para {target}: {}",
                    truncate(text, 200)
                )
            }
            ToolKind::SendMedia => {
                let target = arg_opt_str(args, "chat_id").unwrap_or("?");
                let path = arg_opt_str(args, "path").unwrap_or("?");
                let caption = arg_opt_str(args, "caption")
                    .map(|c| format!(" — {}", truncate(c, 100)))
                    .unwrap_or_default();
                format!("Enviar arquivo {path} por WhatsApp{via} para {target}{caption}")
            }
            _ => format!("{} {}", self.kind.name(), truncate(&args.to_string(), 300)),
        }
    }

    async fn call(&self, _ctx: ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let m = self.module()?;
        let target = self.target(&m, &args)?;
        match target {
            Target::One(acc) => {
                let mut out = match self.kind {
                    ToolKind::ListChats => list_chats(&m, &acc, &args).await?,
                    ToolKind::GetMessages => get_messages(&m, &acc, &args).await?,
                    ToolKind::SearchMessages => search_messages(&m, &acc, &args).await?,
                    ToolKind::ListContacts => list_contacts(&m, &acc, &args).await?,
                    ToolKind::GetGroup => get_group(&m, &acc, &args).await?,
                    ToolKind::DownloadMedia => download_media(&m, &acc, &args).await?,
                    ToolKind::MarkRead => mark_read(&m, &acc, &args).await?,
                    ToolKind::SendMessage => send_message(&m, &acc, &args).await?,
                    ToolKind::SendMedia => send_media(&m, &acc, &args).await?,
                };
                out["account"] = json!(acc.id);
                out["account_label"] = json!(acc.label());
                Ok(ToolOutput::json(&out))
            }
            Target::All(accounts) => {
                let mut out = match self.kind {
                    ToolKind::ListChats => list_chats_all(&m, &accounts, &args).await?,
                    ToolKind::SearchMessages => search_messages_all(&m, &accounts, &args).await?,
                    ToolKind::ListContacts => list_contacts_all(&m, &accounts, &args).await?,
                    _ => return Err(ToolError::InvalidArgs("informe `account`".into())),
                };
                out["account"] = Value::Null;
                out["accounts"] = json!(accounts
                    .iter()
                    .map(|a| json!({"id": a.id, "label": a.label(), "phone": a.phone()}))
                    .collect::<Vec<_>>());
                Ok(ToolOutput::json(&out))
            }
        }
    }
}

impl ToolProvider for WhatsAppModule {
    fn family(&self) -> &'static str {
        FAMILY
    }

    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        ToolKind::ALL
            .iter()
            .map(|k| Arc::new(WaTool::new(*k, self.weak.clone())) as Arc<dyn WillTool>)
            .collect()
    }

    fn available(&self) -> bool {
        self.is_connected()
    }
}

// ----- helpers -----

fn db_err(e: will_storage::StorageError) -> ToolError {
    ToolError::Failed(format!("índice WhatsApp: {e}"))
}

fn limit_arg(args: &Value, default: i64, max: i64) -> i64 {
    arg_opt_i64(args, "limit").unwrap_or(default).clamp(1, max)
}

fn rfc3339(d: &chrono::DateTime<Utc>) -> String {
    d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Normaliza `before`/`since` para o formato guardado no índice (RFC 3339 UTC).
fn time_arg(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    let Some(raw) = arg_opt_str(args, key) else {
        return Ok(None);
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S").map(|n| n.and_utc())
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc())
        })
        .map_err(|_| {
            ToolError::InvalidArgs(format!(
                "{key} inválido: use ISO-8601 (ex.: 2026-01-31T12:00:00Z)"
            ))
        })?;
    Ok(Some(rfc3339(&parsed)))
}

/// Etiqueta um item agregado com a conta de origem.
fn tag_account(mut v: Value, acc: &WaAccount) -> Value {
    v["account"] = json!(acc.id);
    v["account_label"] = json!(acc.label());
    v
}

/// Resolve `chat_id`: JID direto, ou nome de chat/contato (único) → JID.
pub(crate) async fn resolve_chat_id(
    m: &WhatsAppModule,
    acc: &WaAccount,
    raw: &str,
) -> Result<String, ToolError> {
    let raw = raw.trim();
    if raw.contains('@') {
        return Ok(raw.to_string());
    }
    let st = m.storage();
    let chats = st
        .wa_find_chat_by_name(&acc.id, raw)
        .await
        .map_err(db_err)?;
    let exact: Vec<_> = chats
        .iter()
        .filter(|c| {
            c.name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(raw))
        })
        .collect();
    let candidates: Vec<_> = if exact.len() == 1 {
        exact
    } else {
        chats.iter().collect()
    };
    match candidates.len() {
        1 => return Ok(candidates[0].jid.clone()),
        0 => {}
        _ => {
            let list: Vec<String> = candidates
                .iter()
                .map(|c| format!("{} ({})", c.name.clone().unwrap_or_default(), c.jid))
                .collect();
            return Err(ToolError::InvalidArgs(format!(
                "chat_id ambíguo \"{raw}\"; candidatos: {}",
                list.join("; ")
            )));
        }
    }
    let contacts = st
        .wa_search_contacts(&acc.id, Some(raw), 10)
        .await
        .map_err(db_err)?;
    let named: Vec<_> = contacts
        .iter()
        .filter(|c| {
            c.name
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains(&raw.to_lowercase()))
                || c.push_name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&raw.to_lowercase()))
        })
        .collect();
    match named.len() {
        1 => Ok(named[0].jid.clone()),
        0 => Err(ToolError::InvalidArgs(format!("chat ou contato \"{raw}\" não encontrado no índice da conta {}; use whatsapp_list_chats/whatsapp_list_contacts", acc.label()))),
        _ => {
            let list: Vec<String> = named.iter().map(|c| format!("{} ({})", c.name.clone().or(c.push_name.clone()).unwrap_or_default(), c.jid)).collect();
            Err(ToolError::InvalidArgs(format!("contato ambíguo \"{raw}\"; candidatos: {}", list.join("; "))))
        }
    }
}

fn message_json(msg: &will_storage::whatsapp::WaMessage) -> Value {
    json!({
        "message_id": msg.id,
        "chat_id": msg.chat_jid,
        "at": rfc3339(&msg.at),
        "from_me": msg.from_me,
        "sender": msg.sender_name,
        "sender_id": msg.sender_jid,
        "kind": msg.kind,
        "text": msg.text,
        "has_media": msg.media_ref.is_some(),
        "media_path": msg.media_path,
        "quoted_id": msg.quoted_id,
        "read": msg.read,
    })
}

fn chat_json(c: &will_storage::whatsapp::WaChat) -> Value {
    json!({
        "id": c.jid,
        "name": c.name,
        "is_group": c.is_group,
        "archived": c.archived,
        "unread": c.unread_count,
        "last_message": c.last_message_preview,
        "last_message_at": c.last_message_at.as_ref().map(rfc3339),
    })
}

fn contact_json(c: &will_storage::whatsapp::WaContact) -> Value {
    json!({
        "name": c.name.clone().or(c.push_name.clone()),
        "push_name": c.push_name,
        "phone": c.phone.clone().or_else(|| phone_from_jid(&c.jid)),
        "jid": c.jid,
    })
}

/// Mantém as mensagens mais recentes dentro do teto de bytes de texto.
fn cap_messages(
    mut msgs: Vec<will_storage::whatsapp::WaMessage>,
) -> (Vec<will_storage::whatsapp::WaMessage>, bool) {
    let mut total = 0usize;
    let mut keep_from = 0usize;
    for (i, m) in msgs.iter().enumerate().rev() {
        total += m.text.as_deref().map(str::len).unwrap_or(0) + 64;
        if total > MAX_TEXT_BYTES {
            keep_from = i + 1;
            break;
        }
    }
    let truncated = keep_from > 0;
    if truncated {
        msgs.drain(..keep_from);
    }
    (msgs, truncated)
}

fn filter_arg(args: &Value) -> Result<&str, ToolError> {
    let filter = arg_opt_str(args, "filter").unwrap_or("all");
    if !["all", "unread", "groups", "archived"].contains(&filter) {
        return Err(ToolError::InvalidArgs(
            "filter deve ser all|unread|groups|archived".into(),
        ));
    }
    Ok(filter)
}

// ----- ferramentas (uma conta) -----

async fn list_chats(m: &WhatsAppModule, acc: &WaAccount, args: &Value) -> Result<Value, ToolError> {
    let filter = filter_arg(args)?;
    let limit = limit_arg(args, DEFAULT_LIMIT, MAX_MESSAGES);
    let chats = m
        .storage()
        .wa_list_chats(&acc.id, filter, limit)
        .await
        .map_err(db_err)?;
    let items: Vec<Value> = chats.iter().map(chat_json).collect();
    Ok(json!({"filter": filter, "count": items.len(), "chats": items}))
}

async fn get_messages(
    m: &WhatsAppModule,
    acc: &WaAccount,
    args: &Value,
) -> Result<Value, ToolError> {
    let chat = resolve_chat_id(m, acc, arg_str(args, "chat_id")?).await?;
    let limit = limit_arg(args, DEFAULT_LIMIT, MAX_MESSAGES);
    let before = time_arg(args, "before")?;
    let since = time_arg(args, "since")?;
    let msgs = m
        .storage()
        .wa_get_messages(&acc.id, &chat, limit, before.as_deref(), since.as_deref())
        .await
        .map_err(db_err)?;
    let full = msgs.len() as i64 >= limit;
    let (msgs, truncated) = cap_messages(msgs);
    let next_before = if full || truncated {
        msgs.first().map(|x| rfc3339(&x.at))
    } else {
        None
    };
    let chat_meta = m
        .storage()
        .wa_get_chat(&acc.id, &chat)
        .await
        .map_err(db_err)?;
    Ok(json!({
        "chat_id": chat,
        "chat_name": chat_meta.as_ref().and_then(|c| c.name.clone()),
        "count": msgs.len(),
        "truncated": truncated,
        "next_before": next_before,
        "messages": msgs.iter().map(message_json).collect::<Vec<_>>(),
    }))
}

/// Resultados de busca de uma conta já formatados (com `chat_name`).
async fn search_items(
    m: &WhatsAppModule,
    acc: &WaAccount,
    query: &str,
    chat: Option<&str>,
    since: Option<&str>,
    limit: i64,
) -> Result<(Vec<Value>, bool), ToolError> {
    let msgs = m
        .storage()
        .wa_search_messages(&acc.id, query, chat, since, limit)
        .await
        .map_err(db_err)?;
    let (msgs, truncated) = cap_messages(msgs);
    let mut names: std::collections::HashMap<String, Option<String>> = Default::default();
    let mut results = Vec::with_capacity(msgs.len());
    for msg in &msgs {
        if !names.contains_key(&msg.chat_jid) {
            let name = m
                .storage()
                .wa_get_chat(&acc.id, &msg.chat_jid)
                .await
                .ok()
                .flatten()
                .and_then(|c| c.name);
            names.insert(msg.chat_jid.clone(), name);
        }
        let mut j = message_json(msg);
        j["chat_name"] = json!(names.get(&msg.chat_jid).cloned().flatten());
        if let Some(t) = msg.text.as_deref() {
            j["text"] = json!(truncate(t, 500));
        }
        results.push(j);
    }
    Ok((results, truncated))
}

async fn search_messages(
    m: &WhatsAppModule,
    acc: &WaAccount,
    args: &Value,
) -> Result<Value, ToolError> {
    let query = arg_str(args, "query")?;
    let chat = match arg_opt_str(args, "chat_id") {
        Some(c) => Some(resolve_chat_id(m, acc, c).await?),
        None => None,
    };
    let since = time_arg(args, "since")?;
    let limit = limit_arg(args, DEFAULT_LIMIT, MAX_MESSAGES);
    let (results, truncated) =
        search_items(m, acc, query, chat.as_deref(), since.as_deref(), limit).await?;
    Ok(json!({"query": query, "count": results.len(), "truncated": truncated, "results": results}))
}

async fn list_contacts(
    m: &WhatsAppModule,
    acc: &WaAccount,
    args: &Value,
) -> Result<Value, ToolError> {
    let query = arg_opt_str(args, "query");
    let limit = limit_arg(args, 100, 500);
    let contacts = m
        .storage()
        .wa_search_contacts(&acc.id, query, limit)
        .await
        .map_err(db_err)?;
    let items: Vec<Value> = contacts.iter().map(contact_json).collect();
    Ok(json!({"count": items.len(), "contacts": items}))
}

async fn get_group(m: &WhatsAppModule, acc: &WaAccount, args: &Value) -> Result<Value, ToolError> {
    let jid = resolve_chat_id(m, acc, arg_str(args, "chat_id")?).await?;
    if !is_group_jid(&jid) {
        return Err(ToolError::InvalidArgs(format!("{jid} não é um grupo")));
    }
    let refresh = arg_opt_bool(args, "refresh").unwrap_or(false);
    let local = if refresh {
        None
    } else {
        m.storage()
            .wa_get_group(&acc.id, &jid)
            .await
            .map_err(db_err)?
    };
    let has_participants = local
        .as_ref()
        .and_then(|g| g.get("participants"))
        .and_then(|p| p.as_array())
        .is_some_and(|a| !a.is_empty());
    if let (Some(g), true) = (&local, has_participants) {
        return Ok(g.clone());
    }
    match acc.backend.fetch_group(&jid).await {
        Ok(meta) => {
            m.upsert_group_json(acc, &jid, &meta).await;
            Ok(meta)
        }
        Err(e) => match local {
            Some(g) => Ok(g),
            None => Err(ToolError::Failed(format!("grupo não disponível: {e}"))),
        },
    }
}

async fn download_media(
    m: &WhatsAppModule,
    acc: &WaAccount,
    args: &Value,
) -> Result<Value, ToolError> {
    let id = arg_str(args, "message_id")?;
    let msg = m
        .storage()
        .wa_get_message(&acc.id, id)
        .await
        .map_err(db_err)?
        .ok_or_else(|| {
            ToolError::InvalidArgs(format!(
                "mensagem {id} não encontrada no índice da conta {}",
                acc.label()
            ))
        })?;
    if msg.media_ref.is_none()
        && !matches!(
            msg.kind.as_str(),
            "image" | "video" | "audio" | "document" | "sticker"
        )
    {
        return Err(ToolError::InvalidArgs(format!(
            "mensagem {id} não tem mídia (tipo {})",
            msg.kind
        )));
    }
    let dest_dir = m.media_dir();
    let path = match msg
        .media_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        Some(p) => p,
        None => {
            std::fs::create_dir_all(&dest_dir)
                .map_err(|e| ToolError::Failed(format!("criar {}: {e}", dest_dir.display())))?;
            let p = acc
                .backend
                .download_media(id, msg.media_ref.as_deref(), &dest_dir)
                .await
                .map_err(ToolError::Failed)?;
            m.storage()
                .wa_set_media_path(&acc.id, id, &p.to_string_lossy())
                .await
                .map_err(db_err)?;
            p
        }
    };
    let size = std::fs::metadata(&path).map(|md| md.len()).unwrap_or(0);
    let mime = mime_for_path(&path);
    let rel = path
        .strip_prefix(m.kb_path())
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|_| path.clone());
    Ok(json!({
        "message_id": id,
        "path": path,
        "kb_relative_path": rel,
        "mime": mime,
        "size": size,
        "kind": msg.kind,
    }))
}

async fn mark_read(m: &WhatsAppModule, acc: &WaAccount, args: &Value) -> Result<Value, ToolError> {
    let jid = resolve_chat_id(m, acc, arg_str(args, "chat_id")?).await?;
    m.storage()
        .wa_mark_read(&acc.id, &jid)
        .await
        .map_err(db_err)?;
    let mut receipt_sent = false;
    if m.settings().send_read_receipts {
        match acc.backend.mark_read(&jid).await {
            Ok(()) => receipt_sent = true,
            Err(e) => tracing::warn!("mark_read remoto {jid}: {e}"),
        }
    }
    Ok(json!({"ok": true, "chat_id": jid, "receipt_sent": receipt_sent}))
}

/// Resolve o destino de um envio aplicando as regras de números não salvos.
async fn resolve_send_target(
    m: &WhatsAppModule,
    acc: &WaAccount,
    args: &Value,
) -> Result<(String, Option<String>), ToolError> {
    let settings = m.settings();
    let st = m.storage();
    let chat_id = arg_opt_str(args, "chat_id");
    let phone = arg_opt_str(args, "phone");
    let phone = match (chat_id, phone) {
        (None, None) => return Err(ToolError::InvalidArgs("informe chat_id ou phone".into())),
        (Some(c), _)
            if !c.contains('@')
                && phone_digits(c).is_some()
                && c.chars()
                    .all(|ch| ch.is_ascii_digit() || "+ -()".contains(ch)) =>
        {
            Some(c)
        }
        (_, Some(p)) => Some(p),
        _ => None,
    };
    if let Some(p) = phone {
        if !settings.allow_unsaved_numbers {
            return Err(ToolError::Forbidden("envio para números não salvos está desligado nas configurações; use chat_id de um chat/contato existente".into()));
        }
        let digits = phone_digits(p).ok_or_else(|| {
            ToolError::InvalidArgs("phone inválido: use DDI+DDD+número, só dígitos".into())
        })?;
        let jid = match acc.backend.resolve_phone(&digits).await {
            Ok(Some(j)) => j,
            Ok(None) => {
                return Err(ToolError::Failed(format!(
                    "o número {digits} não está no WhatsApp"
                )))
            }
            Err(e) => {
                tracing::warn!("resolve_phone {digits}: {e}; usando formato padrão");
                jid_from_phone(&digits)
            }
        };
        let name = st
            .wa_get_chat(&acc.id, &jid)
            .await
            .ok()
            .flatten()
            .and_then(|c| c.name);
        return Ok((jid, name));
    }
    let raw = chat_id.unwrap_or_default();
    let jid = resolve_chat_id(m, acc, raw).await?;
    let chat = st.wa_get_chat(&acc.id, &jid).await.map_err(db_err)?;
    if chat.is_none() {
        let known_contact = st
            .wa_search_contacts(&acc.id, Some(&jid), 1)
            .await
            .map_err(db_err)?
            .iter()
            .any(|c| c.jid == jid);
        if !known_contact && !settings.allow_unsaved_numbers {
            return Err(ToolError::Forbidden(format!("{jid} não é um chat/contato conhecido e o envio para números não salvos está desligado")));
        }
    }
    Ok((jid.clone(), chat.and_then(|c| c.name)))
}

async fn check_rate_limit(m: &WhatsAppModule, acc: &WaAccount) -> Result<(), ToolError> {
    let s = m.settings();
    if s.send_rate_limit_count == 0 {
        return Ok(());
    }
    let window = Duration::minutes(s.send_rate_limit_minutes.max(1) as i64);
    let sent = m
        .storage()
        .wa_sent_count_since(&acc.id, Utc::now() - window)
        .await
        .map_err(db_err)?;
    if sent >= s.send_rate_limit_count as i64 {
        return Err(ToolError::Failed(format!(
            "limite de envio da conta {} atingido ({} mensagens em {} min); aguarde antes de enviar mais, para reduzir o risco de bloqueio do número",
            acc.label(),
            s.send_rate_limit_count,
            s.send_rate_limit_minutes
        )));
    }
    Ok(())
}

async fn record_sent(
    m: &WhatsAppModule,
    acc: &WaAccount,
    id: &str,
    jid: &str,
    kind: &str,
    text: Option<String>,
    sender_name: Option<String>,
) {
    let st = m.storage();
    let _ = st
        .wa_record_outbox(&acc.id, id, jid, kind, "sent", None, None)
        .await;
    let _ = st
        .wa_upsert_chat(&acc.id, jid, None, is_group_jid(jid))
        .await;
    let msg = will_storage::whatsapp::WaMessage {
        account_id: acc.id.clone(),
        id: id.to_string(),
        chat_jid: jid.to_string(),
        sender_jid: acc
            .backend
            .me()
            .and_then(|v| v.get("jid").and_then(|j| j.as_str()).map(str::to_string)),
        sender_name,
        from_me: true,
        at: Utc::now(),
        kind: kind.to_string(),
        text,
        media_ref: None,
        media_path: None,
        quoted_id: None,
        read: true,
    };
    let _ = st.wa_insert_message(&msg).await;
}

async fn send_message(
    m: &WhatsAppModule,
    acc: &WaAccount,
    args: &Value,
) -> Result<Value, ToolError> {
    let text = arg_str(args, "text")?;
    if text.len() > 65_000 {
        return Err(ToolError::InvalidArgs("text excede 65000 bytes".into()));
    }
    let reply_to = arg_opt_str(args, "reply_to");
    let (jid, name) = resolve_send_target(m, acc, args).await?;
    check_rate_limit(m, acc).await?;
    match acc.backend.send_text(&jid, text, reply_to).await {
        Ok(id) => {
            record_sent(m, acc, &id, &jid, "text", Some(text.to_string()), None).await;
            Ok(json!({"ok": true, "message_id": id, "chat_id": jid, "chat_name": name}))
        }
        Err(e) => {
            let id = format!("err-{}", uuid::Uuid::new_v4());
            let _ = m
                .storage()
                .wa_record_outbox(&acc.id, &id, &jid, "text", "error", Some(&e), None)
                .await;
            Err(ToolError::Failed(format!("falha ao enviar: {e}")))
        }
    }
}

fn resolve_kb_path(kb: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let candidate = {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            kb.join(p)
        }
    };
    let kb_canon = kb
        .canonicalize()
        .map_err(|e| ToolError::Failed(format!("KB inacessível ({}): {e}", kb.display())))?;
    let canon = candidate
        .canonicalize()
        .map_err(|_| ToolError::InvalidArgs(format!("arquivo não encontrado: {raw}")))?;
    if !canon.starts_with(&kb_canon) {
        return Err(ToolError::Forbidden(
            "path deve estar dentro da base de conhecimento".into(),
        ));
    }
    if !canon.is_file() {
        return Err(ToolError::InvalidArgs(format!(
            "path não é um arquivo: {raw}"
        )));
    }
    Ok(canon)
}

async fn send_media(m: &WhatsAppModule, acc: &WaAccount, args: &Value) -> Result<Value, ToolError> {
    let raw_path = arg_str(args, "path")?;
    let caption = arg_opt_str(args, "caption");
    let path = resolve_kb_path(m.kb_path(), raw_path)?;
    let (jid, name) = resolve_send_target(m, acc, args).await?;
    check_rate_limit(m, acc).await?;
    let mime = mime_for_path(&path);
    let kind = crate::backend::media_class(mime);
    match acc.backend.send_media(&jid, &path, caption).await {
        Ok(id) => {
            let text = caption
                .map(str::to_string)
                .or_else(|| path.file_name().map(|f| f.to_string_lossy().to_string()));
            record_sent(m, acc, &id, &jid, kind, text, None).await;
            Ok(
                json!({"ok": true, "message_id": id, "chat_id": jid, "chat_name": name, "mime": mime, "kind": kind}),
            )
        }
        Err(e) => {
            let id = format!("err-{}", uuid::Uuid::new_v4());
            let _ = m
                .storage()
                .wa_record_outbox(&acc.id, &id, &jid, kind, "error", Some(&e), None)
                .await;
            Err(ToolError::Failed(format!("falha ao enviar mídia: {e}")))
        }
    }
}

// ----- ferramentas (agregadas: várias contas, sem `account`) -----

async fn list_chats_all(
    m: &WhatsAppModule,
    accounts: &[Arc<WaAccount>],
    args: &Value,
) -> Result<Value, ToolError> {
    let filter = filter_arg(args)?;
    let limit = limit_arg(args, DEFAULT_LIMIT, MAX_MESSAGES);
    let mut rows: Vec<(Option<chrono::DateTime<Utc>>, Value)> = Vec::new();
    for acc in accounts {
        let chats = m
            .storage()
            .wa_list_chats(&acc.id, filter, limit)
            .await
            .map_err(db_err)?;
        rows.extend(
            chats
                .iter()
                .map(|c| (c.last_message_at, tag_account(chat_json(c), acc))),
        );
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    rows.truncate(limit as usize);
    let items: Vec<Value> = rows.into_iter().map(|(_, v)| v).collect();
    Ok(json!({"filter": filter, "count": items.len(), "chats": items}))
}

async fn search_messages_all(
    m: &WhatsAppModule,
    accounts: &[Arc<WaAccount>],
    args: &Value,
) -> Result<Value, ToolError> {
    let query = arg_str(args, "query")?;
    let since = time_arg(args, "since")?;
    let limit = limit_arg(args, DEFAULT_LIMIT, MAX_MESSAGES);
    let chat_raw = arg_opt_str(args, "chat_id");
    let mut rows: Vec<Value> = Vec::new();
    let mut truncated = false;
    for acc in accounts {
        // Nome de chat pode não existir em todas as contas: ignora a conta nesse caso.
        let chat = match chat_raw {
            Some(c) => match resolve_chat_id(m, acc, c).await {
                Ok(j) => Some(j),
                Err(_) => continue,
            },
            None => None,
        };
        let (items, t) =
            search_items(m, acc, query, chat.as_deref(), since.as_deref(), limit).await?;
        truncated |= t;
        rows.extend(items.into_iter().map(|v| tag_account(v, acc)));
    }
    rows.sort_by(|a, b| {
        b["at"]
            .as_str()
            .unwrap_or("")
            .cmp(a["at"].as_str().unwrap_or(""))
    });
    rows.truncate(limit as usize);
    Ok(json!({"query": query, "count": rows.len(), "truncated": truncated, "results": rows}))
}

async fn list_contacts_all(
    m: &WhatsAppModule,
    accounts: &[Arc<WaAccount>],
    args: &Value,
) -> Result<Value, ToolError> {
    let query = arg_opt_str(args, "query");
    let limit = limit_arg(args, 100, 500);
    let mut rows: Vec<Value> = Vec::new();
    for acc in accounts {
        let contacts = m
            .storage()
            .wa_search_contacts(&acc.id, query, limit)
            .await
            .map_err(db_err)?;
        rows.extend(contacts.iter().map(|c| tag_account(contact_json(c), acc)));
    }
    rows.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
    });
    rows.truncate(limit as usize);
    Ok(json!({"count": rows.len(), "contacts": rows}))
}
