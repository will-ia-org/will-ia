//! Ferramentas `gmail_*` (Gmail API v1). Sem sincronização local: busca
//! server-side. `archived` = sem `INBOX` (e fora de TRASH/SPAM); `unread` = `UNREAD`.

pub mod mime;

use crate::error::GoogleError;
use crate::{GoogleModule, STATUS_REAUTHORIZE};
use futures::StreamExt;
use mime::{
    all_headers, build_mime, cap_bytes, decode_b64url, encode_b64url, extract_body,
    flags_from_labels, header, html_to_text, labels_of, parse_addresses, reply_subject,
    sanitize_filename, strip_quotes, OutgoingMail,
};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use will_core::tools::{arg_opt_bool, arg_opt_str, arg_opt_u64, arg_str, truncate};
use will_core::{
    Module, ToolContext, ToolError, ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool,
};
use will_storage::google::GoogleAccount;

const FAMILY: &str = "gmail";
const BASE: &str = "gmail/v1/users/me";
const MAX_SEARCH: u64 = 100;
const DETAIL_CONCURRENCY: usize = 8;
const META_HEADERS: [&str; 7] = [
    "From",
    "To",
    "Cc",
    "Date",
    "Subject",
    "Content-Type",
    "Message-ID",
];

/// Estado em memória do Gmail: janela deslizante de envios e destinatários conhecidos.
pub struct GmailState {
    sends: parking_lot::Mutex<VecDeque<Instant>>,
    known: parking_lot::RwLock<HashSet<String>>,
}

impl Default for GmailState {
    fn default() -> Self {
        Self::new()
    }
}

impl GmailState {
    pub fn new() -> Self {
        Self {
            sends: parking_lot::Mutex::new(VecDeque::new()),
            known: parking_lot::RwLock::new(HashSet::new()),
        }
    }

    pub fn remember(&self, addrs: impl IntoIterator<Item = String>) {
        let mut k = self.known.write();
        for a in addrs {
            k.insert(a.to_ascii_lowercase());
        }
    }

    pub fn is_known(&self, addr: &str) -> bool {
        self.known.read().contains(&addr.to_ascii_lowercase())
    }

    /// Reserva uma vaga na janela de 1 h; erro acionável se o limite foi atingido.
    pub fn try_acquire_send(&self, limit_per_hour: u32) -> Result<(), ToolError> {
        let mut q = self.sends.lock();
        let cutoff = Instant::now() - Duration::from_secs(3600);
        while q.front().is_some_and(|t| *t < cutoff) {
            q.pop_front();
        }
        if limit_per_hour > 0 && q.len() as u32 >= limit_per_hour {
            return Err(ToolError::Forbidden(format!(
                "limite de envio atingido ({limit_per_hour}/hora); tente mais tarde"
            )));
        }
        q.push_back(Instant::now());
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ListLabels,
    Search,
    GetMessage,
    GetThread,
    DownloadAttachment,
    Send,
    CreateDraft,
    Modify,
}

impl Kind {
    const ALL: [Kind; 8] = [
        Kind::ListLabels,
        Kind::Search,
        Kind::GetMessage,
        Kind::GetThread,
        Kind::DownloadAttachment,
        Kind::Send,
        Kind::CreateDraft,
        Kind::Modify,
    ];
    fn name(self) -> &'static str {
        match self {
            Kind::ListLabels => "gmail_list_labels",
            Kind::Search => "gmail_search",
            Kind::GetMessage => "gmail_get_message",
            Kind::GetThread => "gmail_get_thread",
            Kind::DownloadAttachment => "gmail_download_attachment",
            Kind::Send => "gmail_send",
            Kind::CreateDraft => "gmail_create_draft",
            Kind::Modify => "gmail_modify",
        }
    }
}

pub struct GmailProvider {
    module: Arc<GoogleModule>,
}

impl GmailProvider {
    pub fn new(module: Arc<GoogleModule>) -> Self {
        Self { module }
    }
}

impl ToolProvider for GmailProvider {
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        Kind::ALL
            .iter()
            .map(|k| {
                Arc::new(GmailTool {
                    kind: *k,
                    module: self.module.clone(),
                }) as Arc<dyn WillTool>
            })
            .collect()
    }
    fn available(&self) -> bool {
        self.module.config().gmail.enabled
            && self
                .module
                .gmail_accounts()
                .iter()
                .any(|a| a.status == crate::STATUS_OK)
    }
}

impl GoogleModule {
    /// Contas usáveis pelo Gmail (restritas a `gmail.accounts` se não vazio).
    pub(crate) fn gmail_accounts(&self) -> Vec<GoogleAccount> {
        let allowed = self.config().gmail.accounts;
        self.client
            .cached_accounts()
            .into_iter()
            .filter(|a| {
                allowed.is_empty()
                    || allowed
                        .iter()
                        .any(|x| x == &a.id || x.eq_ignore_ascii_case(&a.email))
            })
            .collect()
    }

    /// Resolve `account` (id ou e-mail); opcional quando há exatamente uma.
    pub(crate) async fn resolve_gmail_account(
        &self,
        arg: Option<&str>,
    ) -> Result<GoogleAccount, ToolError> {
        if self.client.cached_accounts().is_empty() {
            let _ = self.client.reload_accounts().await;
        }
        let candidates = self.gmail_accounts();
        let acc = match arg {
            Some(a) => candidates
                .into_iter()
                .find(|c| c.id == a.trim() || c.email.eq_ignore_ascii_case(a.trim()))
                .ok_or_else(|| {
                    ToolError::InvalidArgs(format!(
                        "conta Google '{a}' não encontrada ou não habilitada para o Gmail"
                    ))
                })?,
            None => match candidates.len() {
                0 => {
                    return Err(ToolError::Unavailable(
                        "nenhuma conta Google habilitada para o Gmail".into(),
                    ))
                }
                1 => candidates
                    .into_iter()
                    .next()
                    .ok_or_else(|| ToolError::Unavailable("sem conta".into()))?,
                _ => {
                    let emails: Vec<String> = candidates.iter().map(|c| c.email.clone()).collect();
                    return Err(ToolError::InvalidArgs(format!(
                        "várias contas disponíveis; informe `account` ({})",
                        emails.join(", ")
                    )));
                }
            },
        };
        if acc.status == STATUS_REAUTHORIZE {
            return Err(GoogleError::Reauthorize(acc.email).into());
        }
        Ok(acc)
    }
}

pub struct GmailTool {
    kind: Kind,
    module: Arc<GoogleModule>,
}

fn account_prop() -> Value {
    json!({"type": "string", "description": "Conta (e-mail ou id); opcional quando há uma só"})
}

fn format_prop() -> Value {
    json!({"type": "string", "enum": ["summary", "text", "full"], "default": "text",
           "description": "summary = cabeçalhos/snippet; text = + corpo em texto e anexos; full = + HTML e todos os headers"})
}

fn compose_props() -> Value {
    json!({
        "account": account_prop(),
        "to": {"type": "array", "items": {"type": "string"}, "description": "Destinatários"},
        "subject": {"type": "string"},
        "body_text": {"type": "string", "description": "Corpo em texto puro"},
        "body_html": {"type": "string", "description": "Corpo HTML (opcional, além do texto)"},
        "cc": {"type": "array", "items": {"type": "string"}},
        "bcc": {"type": "array", "items": {"type": "string"}},
        "reply_to_message_id": {"type": "string", "description": "Id da mensagem a responder (mantém thread, In-Reply-To/References e 'Re:')"},
        "attachments": {"type": "array", "items": {"type": "string"}, "description": "Caminhos de arquivos dentro da base de conhecimento"}
    })
}

#[async_trait::async_trait]
impl WillTool for GmailTool {
    fn spec(&self) -> ToolSpec {
        let (description, input_schema, default_policy) = match self.kind {
            Kind::ListLabels => (
                "Lista labels do Gmail (sistema e usuário) com contagens.",
                json!({"type": "object", "properties": {"account": account_prop(), "include_counts": {"type": "boolean", "default": true}}}),
                ToolPolicy::Auto,
            ),
            Kind::Search => (
                "Busca e-mails com a sintaxe do Gmail (from:, subject:, newer_than:7d, is:unread, has:attachment, label:). Devolve resumo com unread/archived.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "query": {"type": "string", "description": "Consulta na sintaxe do Gmail"},
                    "label_ids": {"type": "array", "items": {"type": "string"}, "description": "Filtra por labels (ex.: INBOX, UNREAD, Label_123)"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH, "default": 20},
                    "page_token": {"type": "string"},
                    "include_spam_trash": {"type": "boolean", "default": false}
                }}),
                ToolPolicy::Auto,
            ),
            Kind::GetMessage => (
                "Lê uma mensagem: cabeçalhos normalizados, corpo em texto (HTML convertido), anexos, labels, archived/unread.",
                json!({"type": "object", "properties": {
                    "account": account_prop(), "id": {"type": "string"}, "format": format_prop(),
                    "strip_quotes": {"type": "boolean", "default": false, "description": "Remove citações/respostas anteriores do corpo"}
                }, "required": ["id"]}),
                ToolPolicy::Auto,
            ),
            Kind::GetThread => (
                "Lê todas as mensagens de uma thread, em ordem.",
                json!({"type": "object", "properties": {
                    "account": account_prop(), "thread_id": {"type": "string"}, "format": format_prop(),
                    "strip_quotes": {"type": "boolean", "default": true}
                }, "required": ["thread_id"]}),
                ToolPolicy::Auto,
            ),
            Kind::DownloadAttachment => (
                "Baixa um anexo para a pasta de anexos da base de conhecimento.",
                json!({"type": "object", "properties": {
                    "account": account_prop(), "message_id": {"type": "string"}, "attachment_id": {"type": "string"}
                }, "required": ["message_id", "attachment_id"]}),
                ToolPolicy::Auto,
            ),
            Kind::Send => (
                "Envia um e-mail (ou resposta em thread). Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": compose_props(), "required": ["to", "subject", "body_text"]}),
                ToolPolicy::Confirm,
            ),
            Kind::CreateDraft => (
                "Cria um rascunho (não envia).",
                json!({"type": "object", "properties": compose_props(), "required": ["subject"]}),
                ToolPolicy::Auto,
            ),
            Kind::Modify => (
                "Modifica mensagens: labels, marcar lida, arquivar, lixeira.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "ids": {"type": "array", "items": {"type": "string"}, "description": "Ids das mensagens"},
                    "add_labels": {"type": "array", "items": {"type": "string"}},
                    "remove_labels": {"type": "array", "items": {"type": "string"}},
                    "mark_read": {"type": "boolean"},
                    "mark_unread": {"type": "boolean"},
                    "archive": {"type": "boolean", "description": "Remove INBOX"},
                    "trash": {"type": "boolean", "description": "Move para a lixeira"}
                }, "required": ["ids"]}),
                ToolPolicy::Confirm,
            ),
        };
        ToolSpec {
            name: self.kind.name().to_string(),
            description: description.to_string(),
            input_schema,
            requires: vec![Module::Google, Module::Gmail],
            default_policy,
            family: FAMILY.to_string(),
        }
    }

    fn approval_summary(&self, args: &Value) -> String {
        match self.kind {
            Kind::Send => {
                let to = str_list(args, "to").unwrap_or_default().join(", ");
                let subject = arg_opt_str(args, "subject").unwrap_or("(sem assunto)");
                let reply = if arg_opt_str(args, "reply_to_message_id").is_some() {
                    " (resposta)"
                } else {
                    ""
                };
                format!(
                    "Enviar e-mail para {} — {}{reply}",
                    truncate(&to, 120),
                    truncate(subject, 100)
                )
            }
            Kind::Modify => {
                let n = args
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let mut actions = Vec::new();
                if arg_opt_bool(args, "mark_read").unwrap_or(false) {
                    actions.push("marcar como lida".to_string());
                }
                if arg_opt_bool(args, "mark_unread").unwrap_or(false) {
                    actions.push("marcar como não lida".to_string());
                }
                if arg_opt_bool(args, "archive").unwrap_or(false) {
                    actions.push("arquivar".to_string());
                }
                if arg_opt_bool(args, "trash").unwrap_or(false) {
                    actions.push("mover para lixeira".to_string());
                }
                if let Some(l) = str_list(args, "add_labels").filter(|l| !l.is_empty()) {
                    actions.push(format!("adicionar labels {}", l.join(",")));
                }
                if let Some(l) = str_list(args, "remove_labels").filter(|l| !l.is_empty()) {
                    actions.push(format!("remover labels {}", l.join(",")));
                }
                format!(
                    "Modificar {n} e-mail(s): {}",
                    if actions.is_empty() {
                        "nada".to_string()
                    } else {
                        actions.join(", ")
                    }
                )
            }
            _ => format!("{} {}", self.kind.name(), truncate(&args.to_string(), 200)),
        }
    }

    async fn call(&self, _ctx: ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let m = &self.module;
        if !m.config().gmail.enabled {
            return Err(ToolError::Unavailable("módulo Gmail desligado".into()));
        }
        let acc = m
            .resolve_gmail_account(arg_opt_str(&args, "account"))
            .await?;
        m.gmail_state.remember([acc.email.clone()]);
        match self.kind {
            Kind::ListLabels => list_labels(m, &acc, &args).await,
            Kind::Search => search(m, &acc, &args).await,
            Kind::GetMessage => get_message(m, &acc, &args).await,
            Kind::GetThread => get_thread(m, &acc, &args).await,
            Kind::DownloadAttachment => download_attachment(m, &acc, &args).await,
            Kind::Send => send(m, &acc, &args).await,
            Kind::CreateDraft => create_draft(m, &acc, &args).await,
            Kind::Modify => modify(m, &acc, &args).await,
        }
    }
}

// ----- helpers -----

fn str_list(args: &Value, key: &str) -> Option<Vec<String>> {
    match args.get(key)? {
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        Value::String(s) => Some(
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

fn internal_date(msg: &Value) -> Option<String> {
    let ms: i64 = msg
        .get("internalDate")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())?;
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn has_attachments(payload: &Value) -> bool {
    if payload.get("parts").is_some() {
        return !extract_body(payload).attachments.is_empty();
    }
    // format=metadata não traz partes: heurística via Content-Type.
    header(payload, "Content-Type")
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("multipart/mixed"))
}

/// Resumo de mensagem (saída de `gmail_search` e base dos detalhes).
pub(crate) fn summarize(msg: &Value) -> Value {
    let payload = msg.get("payload").cloned().unwrap_or(Value::Null);
    let labels = labels_of(msg);
    let (unread, archived) = flags_from_labels(&labels);
    json!({
        "id": msg.get("id"),
        "thread_id": msg.get("threadId"),
        "date": internal_date(msg).or_else(|| header(&payload, "Date")),
        "from": header(&payload, "From"),
        "to": header(&payload, "To"),
        "cc": header(&payload, "Cc"),
        "subject": header(&payload, "Subject").unwrap_or_default(),
        "snippet": msg.get("snippet").and_then(|s| s.as_str()).map(decode_entities),
        "labels": labels,
        "unread": unread,
        "archived": archived,
        "has_attachments": has_attachments(&payload),
    })
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// Detalhe de mensagem conforme `format`.
pub(crate) fn detail(msg: &Value, format: &str, max_body_bytes: usize, strip: bool) -> Value {
    let mut out = summarize(msg);
    let payload = msg.get("payload").cloned().unwrap_or(Value::Null);
    out["message_id_header"] = json!(header(&payload, "Message-ID"));
    if format == "summary" {
        return out;
    }
    let body = extract_body(&payload);
    let mut text = body
        .text
        .clone()
        .or_else(|| body.html.as_deref().map(html_to_text))
        .unwrap_or_default();
    if strip {
        text = strip_quotes(&text);
    }
    let (text, truncated) = cap_bytes(&text, max_body_bytes);
    out["body_text"] = json!(text);
    out["body_truncated"] = json!(truncated);
    out["body_source"] = json!(if body.text.is_some() {
        "text/plain"
    } else if body.html.is_some() {
        "text/html"
    } else {
        "none"
    });
    out["attachments"] = json!(body.attachments);
    out["size_estimate"] = msg.get("sizeEstimate").cloned().unwrap_or(Value::Null);
    if format == "full" {
        out["body_html"] = json!(body.html.map(|h| cap_bytes(&h, max_body_bytes).0));
        out["headers"] = Value::Object(all_headers(&payload));
        out["history_id"] = msg.get("historyId").cloned().unwrap_or(Value::Null);
    }
    out
}

fn remember_addresses(m: &GoogleModule, msg: &Value) {
    let payload = msg.get("payload").cloned().unwrap_or(Value::Null);
    let mut addrs = Vec::new();
    for h in ["From", "To", "Cc", "Reply-To"] {
        if let Some(v) = header(&payload, h) {
            addrs.extend(parse_addresses(&v));
        }
    }
    m.gmail_state.remember(addrs);
}

fn format_arg(args: &Value, default: &str) -> Result<String, ToolError> {
    let f = arg_opt_str(args, "format")
        .unwrap_or(default)
        .to_ascii_lowercase();
    match f.as_str() {
        "summary" | "text" | "full" => Ok(f),
        other => Err(ToolError::InvalidArgs(format!(
            "format inválido: {other} (summary|text|full)"
        ))),
    }
}

fn metadata_query() -> Vec<(&'static str, String)> {
    let mut q = vec![("format", "metadata".to_string())];
    for h in META_HEADERS {
        q.push(("metadataHeaders", h.to_string()));
    }
    q
}

async fn fetch_message(
    m: &GoogleModule,
    acc: &GoogleAccount,
    id: &str,
    full: bool,
) -> Result<Value, GoogleError> {
    let q = if full {
        vec![("format", "full".to_string())]
    } else {
        metadata_query()
    };
    m.client
        .get(
            acc,
            &format!("{BASE}/messages/{}", crate::calendar::enc(id)),
            &q,
        )
        .await
}

// ----- tools -----

async fn list_labels(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let v = m.client.get(acc, &format!("{BASE}/labels"), &[]).await?;
    let labels: Vec<Value> = v
        .get("labels")
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();
    let include_counts = arg_opt_bool(args, "include_counts").unwrap_or(true);
    let details: Vec<Option<Value>> = if include_counts {
        let ids: Vec<Option<String>> = labels
            .iter()
            .map(|l| l.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
            .collect();
        futures::stream::iter(ids)
            .map(|id| async move {
                let id = id?;
                m.client
                    .get(
                        acc,
                        &format!("{BASE}/labels/{}", crate::calendar::enc(&id)),
                        &[],
                    )
                    .await
                    .ok()
            })
            .buffered(DETAIL_CONCURRENCY)
            .collect()
            .await
    } else {
        vec![None; labels.len()]
    };
    let items: Vec<Value> = labels
        .iter()
        .zip(details)
        .map(|(l, d)| {
            let d = d.unwrap_or(Value::Null);
            json!({
                "id": l.get("id"),
                "name": l.get("name"),
                "type": l.get("type"),
                "messages_unread": d.get("messagesUnread"),
                "messages_total": d.get("messagesTotal"),
                "threads_unread": d.get("threadsUnread"),
            })
        })
        .collect();
    Ok(ToolOutput::json(
        &json!({"account": acc.email, "labels": items, "count": items.len()}),
    ))
}

async fn search(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let max = arg_opt_u64(args, "max_results")
        .unwrap_or(20)
        .clamp(1, MAX_SEARCH);
    let mut q: Vec<(&str, String)> = vec![("maxResults", max.to_string())];
    if let Some(s) = arg_opt_str(args, "query").or_else(|| arg_opt_str(args, "q")) {
        q.push(("q", s.to_string()));
    }
    for l in str_list(args, "label_ids").unwrap_or_default() {
        q.push(("labelIds", l));
    }
    if let Some(t) = arg_opt_str(args, "page_token") {
        q.push(("pageToken", t.to_string()));
    }
    if arg_opt_bool(args, "include_spam_trash").unwrap_or(false) {
        q.push(("includeSpamTrash", "true".into()));
    }
    let list = m.client.get(acc, &format!("{BASE}/messages"), &q).await?;
    let ids: Vec<String> = list
        .get("messages")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let details: Vec<Result<Value, GoogleError>> = futures::stream::iter(ids.clone())
        .map(|id| async move { fetch_message(m, acc, &id, false).await })
        .buffered(DETAIL_CONCURRENCY)
        .collect()
        .await;
    let mut items = Vec::with_capacity(ids.len());
    let mut warnings = Vec::new();
    for (id, d) in ids.iter().zip(details) {
        match d {
            Ok(msg) => {
                remember_addresses(m, &msg);
                items.push(summarize(&msg));
            }
            Err(e) => warnings.push(format!("{id}: {}", ToolError::from(e))),
        }
    }
    Ok(ToolOutput::json(&json!({
        "account": acc.email,
        "items": items,
        "count": items.len(),
        "next_page_token": list.get("nextPageToken"),
        "result_size_estimate": list.get("resultSizeEstimate"),
        "warnings": warnings,
    })))
}

async fn get_message(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let id = arg_opt_str(args, "id")
        .or_else(|| arg_opt_str(args, "message_id"))
        .ok_or_else(|| ToolError::InvalidArgs("campo obrigatório ausente: id".into()))?;
    let format = format_arg(args, "text")?;
    let strip = arg_opt_bool(args, "strip_quotes").unwrap_or(false);
    let max_body = m.config().gmail.max_body_kb.max(1) as usize * 1024;
    let msg = fetch_message(m, acc, id, format != "summary").await?;
    remember_addresses(m, &msg);
    let mut out = detail(&msg, &format, max_body, strip);
    out["account"] = json!(acc.email);
    Ok(ToolOutput::json(&out))
}

async fn get_thread(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let thread_id = arg_str(args, "thread_id")?;
    let format = format_arg(args, "text")?;
    let strip = arg_opt_bool(args, "strip_quotes").unwrap_or(true);
    let max_body = m.config().gmail.max_body_kb.max(1) as usize * 1024;
    let q = if format == "summary" {
        metadata_query()
    } else {
        vec![("format", "full".to_string())]
    };
    let t = m
        .client
        .get(
            acc,
            &format!("{BASE}/threads/{}", crate::calendar::enc(thread_id)),
            &q,
        )
        .await?;
    let msgs: Vec<Value> = t
        .get("messages")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .map(|msg| {
                    remember_addresses(m, msg);
                    detail(msg, &format, max_body, strip)
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ToolOutput::json(&json!({
        "account": acc.email,
        "thread_id": t.get("id"),
        "subject": msgs.first().and_then(|x| x.get("subject")).cloned(),
        "messages": msgs,
        "count": msgs.len(),
    })))
}

/// Pasta de anexos dentro da KB (rejeita caminhos absolutos ou com `..`).
fn attachments_dir(kb: &Path, configured: &str) -> PathBuf {
    let rel = Path::new(configured.trim());
    let safe = !configured.trim().is_empty()
        && rel
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    kb.join(if safe { rel } else { Path::new("inbox/gmail") })
}

async fn download_attachment(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let message_id = arg_str(args, "message_id")?;
    let attachment_id = arg_str(args, "attachment_id")?;
    let cfg = m.config().gmail;
    let max_bytes = cfg.max_attachment_mb.max(1) as u64 * 1024 * 1024;

    // Metadados do anexo (nome/mime/tamanho) a partir da mensagem.
    let msg = fetch_message(m, acc, message_id, true).await?;
    let body = extract_body(&msg.get("payload").cloned().unwrap_or(Value::Null));
    let info = body
        .attachments
        .iter()
        .find(|a| a.attachment_id.as_deref() == Some(attachment_id))
        .cloned()
        .unwrap_or_else(|| mime::AttachmentInfo {
            attachment_id: Some(attachment_id.to_string()),
            filename: "anexo.bin".into(),
            mime: "application/octet-stream".into(),
            size: 0,
            inline: false,
        });
    if info.size > max_bytes {
        return Err(ToolError::Forbidden(format!(
            "anexo de {} bytes excede o limite de {} MB",
            info.size, cfg.max_attachment_mb
        )));
    }
    let att = m
        .client
        .get(
            acc,
            &format!(
                "{BASE}/messages/{}/attachments/{}",
                crate::calendar::enc(message_id),
                crate::calendar::enc(attachment_id)
            ),
            &[],
        )
        .await?;
    let data = att
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or_else(|| ToolError::Failed("anexo sem dados".into()))?;
    let bytes = decode_b64url(data).map_err(ToolError::Failed)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ToolError::Forbidden(format!(
            "anexo excede o limite de {} MB",
            cfg.max_attachment_mb
        )));
    }
    let dir = attachments_dir(&m.kb_path, &cfg.attachments_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ToolError::Failed(format!("criar pasta de anexos: {e}")))?;
    let filename = format!(
        "{}-{}",
        sanitize_filename(message_id),
        sanitize_filename(&info.filename)
    );
    let path = dir.join(&filename);
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| ToolError::Failed(format!("gravar anexo: {e}")))?;
    let relative = path
        .strip_prefix(&m.kb_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(ToolOutput::json(&json!({
        "path": path.to_string_lossy(),
        "relative_path": relative,
        "filename": info.filename,
        "mime": info.mime,
        "size": bytes.len(),
        "message_id": message_id,
    })))
}

/// Resolve um caminho de anexo garantindo que fica dentro da KB.
fn resolve_in_kb(kb: &Path, p: &str) -> Result<PathBuf, ToolError> {
    let candidate = {
        let path = Path::new(p.trim());
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            kb.join(path)
        }
    };
    let kb_c = kb
        .canonicalize()
        .map_err(|e| ToolError::Failed(format!("base de conhecimento inacessível: {e}")))?;
    let c = candidate
        .canonicalize()
        .map_err(|e| ToolError::InvalidArgs(format!("anexo '{p}' não encontrado: {e}")))?;
    if !c.starts_with(&kb_c) {
        return Err(ToolError::Forbidden(format!(
            "anexo '{p}' fora da base de conhecimento"
        )));
    }
    if !c.is_file() {
        return Err(ToolError::InvalidArgs(format!(
            "anexo '{p}' não é um arquivo"
        )));
    }
    Ok(c)
}

struct Prepared {
    raw: String,
    thread_id: Option<String>,
    recipients: Vec<String>,
    subject: String,
}

/// Monta a mensagem (envio ou rascunho) a partir dos argumentos.
async fn prepare(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
    require_to: bool,
) -> Result<Prepared, ToolError> {
    let cfg = m.config().gmail;
    let to = str_list(args, "to").unwrap_or_default();
    let cc = str_list(args, "cc").unwrap_or_default();
    let bcc = str_list(args, "bcc").unwrap_or_default();
    if require_to && to.is_empty() {
        return Err(ToolError::InvalidArgs(
            "campo obrigatório ausente: to".into(),
        ));
    }
    for a in to.iter().chain(cc.iter()).chain(bcc.iter()) {
        if parse_addresses(a).is_empty() {
            return Err(ToolError::InvalidArgs(format!("endereço inválido: {a}")));
        }
    }
    let mut subject = arg_opt_str(args, "subject").unwrap_or("").to_string();
    let text = arg_opt_str(args, "body_text")
        .or_else(|| arg_opt_str(args, "body"))
        .unwrap_or("")
        .to_string();
    let html = arg_opt_str(args, "body_html").map(|s| s.to_string());

    let mut mail = OutgoingMail {
        from: acc.email.clone(),
        to,
        cc,
        bcc,
        subject: String::new(),
        text,
        html,
        in_reply_to: None,
        references: Vec::new(),
        attachments: Vec::new(),
    };
    let mut thread_id = None;

    if let Some(reply_id) = arg_opt_str(args, "reply_to_message_id") {
        let q = vec![
            ("format", "metadata".to_string()),
            ("metadataHeaders", "Message-ID".to_string()),
            ("metadataHeaders", "References".to_string()),
            ("metadataHeaders", "Subject".to_string()),
            ("metadataHeaders", "From".to_string()),
            ("metadataHeaders", "Reply-To".to_string()),
        ];
        let orig = m
            .client
            .get(
                acc,
                &format!("{BASE}/messages/{}", crate::calendar::enc(reply_id)),
                &q,
            )
            .await?;
        let payload = orig.get("payload").cloned().unwrap_or(Value::Null);
        thread_id = orig
            .get("threadId")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        if let Some(mid) = header(&payload, "Message-ID") {
            let mid = mid.trim().to_string();
            let mut refs: Vec<String> = header(&payload, "References")
                .map(|r| r.split_whitespace().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            if !refs.contains(&mid) {
                refs.push(mid.clone());
            }
            mail.in_reply_to = Some(mid);
            mail.references = refs;
        }
        if subject.is_empty() {
            subject = reply_subject(&header(&payload, "Subject").unwrap_or_default());
        }
        if mail.to.is_empty() && !require_to {
            if let Some(from) = header(&payload, "Reply-To").or_else(|| header(&payload, "From")) {
                mail.to = parse_addresses(&from);
            }
        }
    }
    if subject.is_empty() {
        return Err(ToolError::InvalidArgs(
            "campo obrigatório ausente: subject".into(),
        ));
    }
    mail.subject = subject.clone();

    let max_att = cfg.max_attachment_mb.max(1) as u64 * 1024 * 1024;
    let mut total = 0u64;
    for p in str_list(args, "attachments").unwrap_or_default() {
        let path = resolve_in_kb(&m.kb_path, &p)?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::Failed(format!("ler anexo '{p}': {e}")))?;
        total += bytes.len() as u64;
        if total > max_att {
            return Err(ToolError::Forbidden(format!(
                "anexos excedem o limite de {} MB",
                cfg.max_attachment_mb
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "anexo".into());
        let ctype = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .essence_str()
            .to_string();
        mail.attachments.push((ctype, name, bytes));
    }

    let recipients: Vec<String> = mail
        .to
        .iter()
        .chain(mail.cc.iter())
        .chain(mail.bcc.iter())
        .flat_map(|a| parse_addresses(a))
        .collect();
    let bytes = build_mime(&mail).map_err(ToolError::Failed)?;
    Ok(Prepared {
        raw: encode_b64url(&bytes),
        thread_id,
        recipients,
        subject,
    })
}

async fn send(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().gmail;
    if !cfg.allow_send {
        return Err(ToolError::Unavailable(
            "envio de e-mail desligado (Configurações → Gmail → Permitir envio)".into(),
        ));
    }
    let p = prepare(m, acc, args, true).await?;
    if cfg.only_known_recipients {
        let unknown: Vec<&String> = p
            .recipients
            .iter()
            .filter(|r| !m.gmail_state.is_known(r))
            .collect();
        if !unknown.is_empty() {
            return Err(ToolError::Forbidden(format!(
                "destinatários desconhecidos (opção 'só contatos já vistos' ativa): {}",
                unknown
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    m.gmail_state
        .try_acquire_send(cfg.send_rate_limit_per_hour)?;
    let mut body = json!({"raw": p.raw});
    if let Some(t) = &p.thread_id {
        body["threadId"] = json!(t);
    }
    let sent = m
        .client
        .post(acc, &format!("{BASE}/messages/send"), &[], &body)
        .await?;
    m.gmail_state.remember(p.recipients.clone());
    Ok(ToolOutput::json(&json!({
        "message_id": sent.get("id"),
        "thread_id": sent.get("threadId"),
        "labels": sent.get("labelIds"),
        "subject": p.subject,
        "to": p.recipients,
        "account": acc.email,
    })))
}

async fn create_draft(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let p = prepare(m, acc, args, false).await?;
    let mut message = json!({"raw": p.raw});
    if let Some(t) = &p.thread_id {
        message["threadId"] = json!(t);
    }
    let draft = m
        .client
        .post(
            acc,
            &format!("{BASE}/drafts"),
            &[],
            &json!({"message": message}),
        )
        .await?;
    Ok(ToolOutput::json(&json!({
        "draft_id": draft.get("id"),
        "message_id": draft.get("message").and_then(|x| x.get("id")),
        "thread_id": draft.get("message").and_then(|x| x.get("threadId")),
        "subject": p.subject,
        "account": acc.email,
    })))
}

async fn modify(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().gmail;
    if !cfg.allow_modify {
        return Err(ToolError::Unavailable(
            "modificação de e-mails desligada (Configurações → Gmail → Permitir modificar)".into(),
        ));
    }
    let ids = str_list(args, "ids")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ToolError::InvalidArgs("campo obrigatório ausente: ids".into()))?;
    if ids.len() > 1000 {
        return Err(ToolError::InvalidArgs(
            "máximo de 1000 ids por chamada".into(),
        ));
    }
    let mut add: Vec<String> = str_list(args, "add_labels").unwrap_or_default();
    let mut remove: Vec<String> = str_list(args, "remove_labels").unwrap_or_default();
    if arg_opt_bool(args, "mark_read").unwrap_or(false) {
        remove.push(mime::LABEL_UNREAD.into());
    }
    if arg_opt_bool(args, "mark_unread").unwrap_or(false) {
        add.push(mime::LABEL_UNREAD.into());
    }
    if arg_opt_bool(args, "archive").unwrap_or(false) {
        remove.push(mime::LABEL_INBOX.into());
    }
    let trash = arg_opt_bool(args, "trash").unwrap_or(false);
    add.retain(|l| l != mime::LABEL_TRASH);
    add.sort();
    add.dedup();
    remove.sort();
    remove.dedup();
    if add.is_empty() && remove.is_empty() && !trash {
        return Err(ToolError::InvalidArgs(
            "nenhuma ação informada (add_labels, remove_labels, mark_read, archive, trash)".into(),
        ));
    }
    if !add.is_empty() || !remove.is_empty() {
        let body = json!({"ids": ids, "addLabelIds": add, "removeLabelIds": remove});
        m.client
            .post(acc, &format!("{BASE}/messages/batchModify"), &[], &body)
            .await?;
    }
    let mut trashed = 0usize;
    let mut warnings = Vec::new();
    if trash {
        for id in &ids {
            match m
                .client
                .post(
                    acc,
                    &format!("{BASE}/messages/{}/trash", crate::calendar::enc(id)),
                    &[],
                    &json!({}),
                )
                .await
            {
                Ok(_) => trashed += 1,
                Err(e) => warnings.push(format!("{id}: {}", ToolError::from(e))),
            }
        }
    }
    Ok(ToolOutput::json(&json!({
        "ok": warnings.is_empty(),
        "count": ids.len(),
        "added": add,
        "removed": remove,
        "trashed": trashed,
        "warnings": warnings,
        "account": acc.email,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_derives_flags_and_attachments() {
        let msg = json!({
            "id": "m1", "threadId": "t1", "internalDate": "1741600800000", "snippet": "Oi &amp; tchau",
            "labelIds": ["UNREAD", "IMPORTANT"],
            "payload": {"headers": [
                {"name": "From", "value": "Ana <ana@x.com>"}, {"name": "Subject", "value": "Olá"},
                {"name": "Content-Type", "value": "multipart/mixed; boundary=x"}
            ]}
        });
        let s = summarize(&msg);
        assert_eq!(s["unread"], true);
        assert_eq!(s["archived"], true);
        assert_eq!(s["has_attachments"], true);
        assert_eq!(s["snippet"], "Oi & tchau");
        assert_eq!(s["date"], "2025-03-10T10:00:00Z");
        assert_eq!(s["subject"], "Olá");
    }

    #[test]
    fn detail_text_and_full() {
        let msg = json!({
            "id": "m1", "threadId": "t1", "labelIds": ["INBOX"], "sizeEstimate": 10,
            "payload": {"mimeType": "text/html", "headers": [{"name": "Message-ID", "value": "<a@b>"}],
                        "body": {"data": encode_b64url(b"<p>Oi <b>a</b></p>")}}
        });
        let d = detail(&msg, "text", 64 * 1024, false);
        assert_eq!(d["archived"], false);
        assert_eq!(d["body_source"], "text/html");
        assert!(d["body_text"].as_str().unwrap().contains("Oi"));
        assert!(d.get("body_html").is_none());
        let f = detail(&msg, "full", 64 * 1024, false);
        assert!(f["body_html"].as_str().unwrap().contains("<b>"));
        assert_eq!(f["headers"]["Message-ID"], "<a@b>");
        let s = detail(&msg, "summary", 1024, false);
        assert!(s.get("body_text").is_none());
    }

    #[test]
    fn rate_limit_and_known_recipients() {
        let st = GmailState::new();
        assert!(st.try_acquire_send(2).is_ok());
        assert!(st.try_acquire_send(2).is_ok());
        assert!(st.try_acquire_send(2).is_err());
        assert!(st.try_acquire_send(0).is_ok());
        st.remember(["Ana@X.com".to_string()]);
        assert!(st.is_known("ana@x.com"));
        assert!(!st.is_known("bob@x.com"));
    }

    #[test]
    fn attachments_dir_is_confined() {
        let kb = Path::new("/kb");
        assert_eq!(
            attachments_dir(kb, "inbox/gmail"),
            PathBuf::from("/kb/inbox/gmail")
        );
        assert_eq!(
            attachments_dir(kb, "../etc"),
            PathBuf::from("/kb/inbox/gmail")
        );
        assert_eq!(
            attachments_dir(kb, "/abs"),
            PathBuf::from("/kb/inbox/gmail")
        );
    }

    #[test]
    fn str_list_accepts_array_or_csv() {
        assert_eq!(
            str_list(&json!({"to": ["a@x.com", " b@x.com "]}), "to").unwrap(),
            vec!["a@x.com", "b@x.com"]
        );
        assert_eq!(
            str_list(&json!({"to": "a@x.com, b@x.com"}), "to").unwrap(),
            vec!["a@x.com", "b@x.com"]
        );
        assert!(str_list(&json!({}), "to").is_none());
    }
}
