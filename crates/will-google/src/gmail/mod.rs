//! Ferramentas `gmail_*` (Gmail API v1). Sem sincronização local: busca
//! server-side. `archived` = sem `INBOX` (e fora de TRASH/SPAM); `unread` = `UNREAD`.

pub mod mime;

use crate::error::GoogleError;
use crate::GoogleModule;
use futures::StreamExt;
use mime::{
    all_headers, build_mime, cap_bytes, decode_b64url, encode_b64url, extract_body,
    flags_from_labels, header, html_to_text, labels_of, parse_addresses, reply_subject,
    sanitize_filename, strip_quotes, OutgoingMail,
};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
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
const MAX_DRAFTS: u64 = 50;
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
    ListDrafts,
    SendDraft,
    UpdateDraft,
    DeleteDraft,
    Forward,
    ReplyAll,
}

impl Kind {
    const ALL: [Kind; 14] = [
        Kind::ListLabels,
        Kind::Search,
        Kind::GetMessage,
        Kind::GetThread,
        Kind::DownloadAttachment,
        Kind::Send,
        Kind::CreateDraft,
        Kind::Modify,
        Kind::ListDrafts,
        Kind::SendDraft,
        Kind::UpdateDraft,
        Kind::DeleteDraft,
        Kind::Forward,
        Kind::ReplyAll,
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
            Kind::ListDrafts => "gmail_list_drafts",
            Kind::SendDraft => "gmail_send_draft",
            Kind::UpdateDraft => "gmail_update_draft",
            Kind::DeleteDraft => "gmail_delete_draft",
            Kind::Forward => "gmail_forward",
            Kind::ReplyAll => "gmail_reply_all",
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
        self.accounts_allowed(&self.config().gmail.accounts)
    }

    /// Resolve `account` (id ou e-mail); opcional quando há exatamente uma.
    pub(crate) async fn resolve_gmail_account(
        &self,
        arg: Option<&str>,
    ) -> Result<GoogleAccount, ToolError> {
        if self.client.cached_accounts().is_empty() {
            let _ = self.client.reload_accounts().await;
        }
        self.resolve_account(arg, self.gmail_accounts(), "Gmail")
            .await
    }
}

impl GoogleModule {
    /// Varredura para as triggers (`docs/30`): e-mails que casam com `query`
    /// (sintaxe do Gmail; vazio = `in:inbox`) na conta (`None` = única), já
    /// com corpo em texto (`detail(.., "text")`) e `internal_date_ms` para o
    /// cursor. Mais novos primeiro, como o Gmail devolve.
    pub async fn gmail_poll(
        &self,
        account: Option<&str>,
        query: &str,
        max: u64,
    ) -> Result<Vec<Value>, String> {
        if !self.config().gmail.enabled {
            return Err("Gmail desligado nas configurações".into());
        }
        let acc = self
            .resolve_gmail_account(account)
            .await
            .map_err(|e| e.to_string())?;
        let q = query.trim();
        let q = if q.is_empty() { "in:inbox" } else { q };
        let list = self
            .client
            .get(
                &acc,
                &format!("{BASE}/messages"),
                &[
                    ("maxResults", max.clamp(1, MAX_SEARCH).to_string()),
                    ("q", q.to_string()),
                ],
            )
            .await
            .map_err(|e| ToolError::from(e).to_string())?;
        let ids: Vec<String> = list
            .get("messages")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let max_body = self.config().gmail.max_body_kb.max(1) as usize * 1024;
        let acc_ref = &acc;
        let details: Vec<Result<Value, GoogleError>> = futures::stream::iter(ids)
            .map(|id| async move { fetch_message(self, acc_ref, &id, true).await })
            .buffered(DETAIL_CONCURRENCY)
            .collect()
            .await;
        let mut out = Vec::new();
        for d in details {
            match d {
                Ok(msg) => {
                    remember_addresses(self, &msg);
                    let mut v = detail(&msg, "text", max_body, true);
                    v["internal_date_ms"] = json!(msg
                        .get("internalDate")
                        .and_then(|x| x.as_str())
                        .and_then(|x| x.parse::<i64>().ok()));
                    v["account"] = json!(acc.email);
                    out.push(v);
                }
                Err(e) => tracing::warn!(error = %e, "gmail_poll: e-mail não lido"),
            }
        }
        Ok(out)
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
            Kind::ListDrafts => (
                "Lista os rascunhos (id do rascunho, assunto, destinatários, data, snippet).",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_DRAFTS, "default": 20},
                    "page_token": {"type": "string"}
                }}),
                ToolPolicy::Auto,
            ),
            Kind::SendDraft => (
                "Envia um rascunho existente (já revisado). Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "account": account_prop(), "draft_id": {"type": "string", "description": "Id do rascunho (gmail_list_drafts/gmail_create_draft)"}
                }, "required": ["draft_id"]}),
                ToolPolicy::Confirm,
            ),
            Kind::UpdateDraft => {
                let mut props = compose_props();
                props["draft_id"] = json!({"type": "string", "description": "Id do rascunho a substituir"});
                (
                    "Substitui o conteúdo de um rascunho (destinatários, assunto, corpo, anexos). Não envia.",
                    json!({"type": "object", "properties": props, "required": ["draft_id", "subject"]}),
                    ToolPolicy::Auto,
                )
            }
            Kind::DeleteDraft => (
                "Apaga um rascunho (definitivo).",
                json!({"type": "object", "properties": {
                    "account": account_prop(), "draft_id": {"type": "string"}
                }, "required": ["draft_id"]}),
                ToolPolicy::Confirm,
            ),
            Kind::Forward => (
                "Encaminha um e-mail (cabeçalho 'Mensagem encaminhada' + corpo original + anexos) para novos destinatários, com um comentário opcional antes. Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "message_id": {"type": "string", "description": "Id da mensagem a encaminhar"},
                    "to": {"type": "array", "items": {"type": "string"}, "description": "Destinatários"},
                    "cc": {"type": "array", "items": {"type": "string"}},
                    "bcc": {"type": "array", "items": {"type": "string"}},
                    "note": {"type": "string", "description": "Comentário acima da mensagem encaminhada (opcional)"},
                    "include_attachments": {"type": "boolean", "default": true}
                }, "required": ["message_id", "to"]}),
                ToolPolicy::Confirm,
            ),
            Kind::ReplyAll => (
                "Responde a todos: remetente (ou Reply-To) + todos os Para/Cc originais, menos você; mantém a thread e o 'Re:'. Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "account": account_prop(),
                    "message_id": {"type": "string", "description": "Id da mensagem a responder"},
                    "body_text": {"type": "string", "description": "Corpo em texto puro"},
                    "body_html": {"type": "string", "description": "Corpo HTML (opcional, além do texto)"},
                    "extra_to": {"type": "array", "items": {"type": "string"}, "description": "Destinatários a acrescentar em Para"},
                    "extra_cc": {"type": "array", "items": {"type": "string"}, "description": "Destinatários a acrescentar em Cc"},
                    "attachments": {"type": "array", "items": {"type": "string"}, "description": "Caminhos de arquivos dentro da base de conhecimento"}
                }, "required": ["message_id", "body_text"]}),
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
            Kind::SendDraft => format!(
                "Enviar o rascunho {}",
                arg_opt_str(args, "draft_id").unwrap_or("?")
            ),
            Kind::DeleteDraft => format!(
                "Apagar o rascunho {}",
                arg_opt_str(args, "draft_id").unwrap_or("?")
            ),
            Kind::Forward => format!(
                "Encaminhar e-mail {} para {}",
                arg_opt_str(args, "message_id").unwrap_or("?"),
                truncate(&str_list(args, "to").unwrap_or_default().join(", "), 120)
            ),
            Kind::ReplyAll => format!(
                "Responder a todos no e-mail {}: {}",
                arg_opt_str(args, "message_id").unwrap_or("?"),
                truncate(arg_opt_str(args, "body_text").unwrap_or(""), 120)
            ),
            _ => format!("{} {}", self.kind.name(), truncate(&args.to_string(), 200)),
        }
    }

    async fn call(&self, ctx: ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let m = &self.module;
        crate::traced_op("gmail", self.kind.name(), &ctx, async {
            if !m.config().gmail.enabled {
                return Err(ToolError::Unavailable("módulo Gmail desligado".into()));
            }
            let acc = m
                .resolve_gmail_account(arg_opt_str(&args, "account"))
                .await?;
            m.gmail_state.remember([acc.email.clone()]);
            self.dispatch(m, &acc, &args).await
        })
        .await
    }
}

impl GmailTool {
    async fn dispatch(
        &self,
        m: &GoogleModule,
        acc: &GoogleAccount,
        args: &Value,
    ) -> Result<ToolOutput, ToolError> {
        let acc = acc.clone();
        let args = args.clone();
        match self.kind {
            Kind::ListLabels => list_labels(m, &acc, &args).await,
            Kind::Search => search(m, &acc, &args).await,
            Kind::GetMessage => get_message(m, &acc, &args).await,
            Kind::GetThread => get_thread(m, &acc, &args).await,
            Kind::DownloadAttachment => download_attachment(m, &acc, &args).await,
            Kind::Send => send(m, &acc, &args).await,
            Kind::CreateDraft => create_draft(m, &acc, &args).await,
            Kind::Modify => modify(m, &acc, &args).await,
            Kind::ListDrafts => list_drafts(m, &acc, &args).await,
            Kind::SendDraft => send_draft(m, &acc, &args).await,
            Kind::UpdateDraft => update_draft(m, &acc, &args).await,
            Kind::DeleteDraft => delete_draft(m, &acc, &args).await,
            Kind::Forward => forward(m, &acc, &args).await,
            Kind::ReplyAll => reply_all(m, &acc, &args).await,
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
    crate::fs::confined_dir(kb, configured, "inbox/gmail")
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
        let path = crate::fs::resolve_in_kb(&m.kb_path, &p, "anexo")?;
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
    deliver(m, acc, p).await
}

/// Políticas de envio (destinatários conhecidos, rate limit) e o `messages.send`.
async fn deliver(
    m: &GoogleModule,
    acc: &GoogleAccount,
    p: Prepared,
) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().gmail;
    check_recipients(m, &p.recipients)?;
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

/// Opção "só contatos já vistos".
fn check_recipients(m: &GoogleModule, recipients: &[String]) -> Result<(), ToolError> {
    if !m.config().gmail.only_known_recipients {
        return Ok(());
    }
    let unknown: Vec<&String> = recipients
        .iter()
        .filter(|r| !m.gmail_state.is_known(r))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(ToolError::Forbidden(format!(
        "destinatários desconhecidos (opção 'só contatos já vistos' ativa): {}",
        unknown
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

// ----- rascunhos, encaminhar, responder a todos (2026-10-29) -----

fn draft_meta_query() -> Vec<(&'static str, String)> {
    let mut q = vec![("format", "metadata".to_string())];
    for h in ["To", "Cc", "Bcc", "Subject", "Date", "From"] {
        q.push(("metadataHeaders", h.to_string()));
    }
    q
}

fn draft_json(draft: &Value) -> Value {
    let msg = draft.get("message").cloned().unwrap_or(Value::Null);
    let payload = msg.get("payload").cloned().unwrap_or(Value::Null);
    let mut out = summarize(&msg);
    out["draft_id"] = draft.get("id").cloned().unwrap_or(Value::Null);
    out["bcc"] = json!(header(&payload, "Bcc"));
    out
}

async fn list_drafts(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let max = arg_opt_u64(args, "max_results")
        .unwrap_or(20)
        .clamp(1, MAX_DRAFTS);
    let mut q = vec![("maxResults", max.to_string())];
    if let Some(t) = arg_opt_str(args, "page_token") {
        q.push(("pageToken", t.to_string()));
    }
    let list = m.client.get(acc, &format!("{BASE}/drafts"), &q).await?;
    let ids: Vec<String> = list
        .get("drafts")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|d| d.get("id").and_then(|i| i.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let meta = draft_meta_query();
    let details: Vec<Result<Value, GoogleError>> = futures::stream::iter(ids.iter().cloned())
        .map(|id| {
            let meta = meta.clone();
            async move {
                m.client
                    .get(
                        acc,
                        &format!("{BASE}/drafts/{}", crate::calendar::enc(&id)),
                        &meta,
                    )
                    .await
            }
        })
        .buffered(DETAIL_CONCURRENCY)
        .collect()
        .await;
    let mut items = Vec::with_capacity(ids.len());
    let mut warnings = Vec::new();
    for (id, r) in ids.iter().zip(details) {
        match r {
            Ok(d) => items.push(draft_json(&d)),
            Err(e) => warnings.push(format!("{id}: {}", ToolError::from(e))),
        }
    }
    Ok(ToolOutput::json(&json!({
        "count": items.len(),
        "drafts": items,
        "next_page_token": list.get("nextPageToken"),
        "warnings": warnings,
        "account": acc.email,
    })))
}

async fn send_draft(
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
    let draft_id = arg_str(args, "draft_id")?;
    let draft = m
        .client
        .get(
            acc,
            &format!("{BASE}/drafts/{}", crate::calendar::enc(draft_id)),
            &draft_meta_query(),
        )
        .await?;
    let payload = draft
        .get("message")
        .and_then(|x| x.get("payload"))
        .cloned()
        .unwrap_or(Value::Null);
    let recipients: Vec<String> = ["To", "Cc", "Bcc"]
        .iter()
        .filter_map(|h| header(&payload, h))
        .flat_map(|v| parse_addresses(&v))
        .collect();
    if recipients.is_empty() {
        return Err(ToolError::InvalidArgs(
            "o rascunho não tem destinatário; use gmail_update_draft antes".into(),
        ));
    }
    check_recipients(m, &recipients)?;
    m.gmail_state
        .try_acquire_send(cfg.send_rate_limit_per_hour)?;
    let sent = m
        .client
        .post(
            acc,
            &format!("{BASE}/drafts/send"),
            &[],
            &json!({"id": draft_id}),
        )
        .await?;
    m.gmail_state.remember(recipients.clone());
    Ok(ToolOutput::json(&json!({
        "message_id": sent.get("id"),
        "thread_id": sent.get("threadId"),
        "labels": sent.get("labelIds"),
        "subject": header(&payload, "Subject"),
        "to": recipients,
        "draft_id": draft_id,
        "account": acc.email,
    })))
}

async fn update_draft(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let draft_id = arg_str(args, "draft_id")?;
    let p = prepare(m, acc, args, false).await?;
    let mut message = json!({"raw": p.raw});
    if let Some(t) = &p.thread_id {
        message["threadId"] = json!(t);
    }
    let draft = m
        .client
        .put(
            acc,
            &format!("{BASE}/drafts/{}", crate::calendar::enc(draft_id)),
            &[],
            &json!({"id": draft_id, "message": message}),
        )
        .await?;
    Ok(ToolOutput::json(&json!({
        "draft_id": draft.get("id").cloned().unwrap_or_else(|| json!(draft_id)),
        "message_id": draft.get("message").and_then(|x| x.get("id")),
        "thread_id": draft.get("message").and_then(|x| x.get("threadId")),
        "subject": p.subject,
        "to": p.recipients,
        "account": acc.email,
    })))
}

async fn delete_draft(
    m: &GoogleModule,
    acc: &GoogleAccount,
    args: &Value,
) -> Result<ToolOutput, ToolError> {
    let draft_id = arg_str(args, "draft_id")?;
    m.client
        .delete(
            acc,
            &format!("{BASE}/drafts/{}", crate::calendar::enc(draft_id)),
            &[],
        )
        .await?;
    Ok(ToolOutput::json(
        &json!({"ok": true, "draft_id": draft_id, "account": acc.email}),
    ))
}

/// `Fwd: assunto` (sem duplicar quando já é encaminhado).
fn forward_subject(original: &str) -> String {
    let t = original.trim();
    let l = t.to_ascii_lowercase();
    if l.starts_with("fwd:") || l.starts_with("fw:") || l.starts_with("enc:") {
        t.to_string()
    } else {
        format!("Fwd: {t}")
    }
}

/// Cabeçalho da mensagem encaminhada, como o Gmail monta.
fn forward_intro(from: &str, date: &str, subject: &str, to: &str, cc: Option<&str>) -> String {
    let mut s = String::from("---------- Mensagem encaminhada ----------\n");
    s.push_str(&format!(
        "De: {from}\nData: {date}\nAssunto: {subject}\nPara: {to}\n"
    ));
    if let Some(c) = cc.filter(|c| !c.trim().is_empty()) {
        s.push_str(&format!("Cc: {c}\n"));
    }
    s
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}

async fn forward(
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
    let message_id = arg_str(args, "message_id")?;
    let to = str_list(args, "to").unwrap_or_default();
    if to.is_empty() {
        return Err(ToolError::InvalidArgs(
            "campo obrigatório ausente: to".into(),
        ));
    }
    let cc = str_list(args, "cc").unwrap_or_default();
    let bcc = str_list(args, "bcc").unwrap_or_default();
    for a in to.iter().chain(cc.iter()).chain(bcc.iter()) {
        if parse_addresses(a).is_empty() {
            return Err(ToolError::InvalidArgs(format!("endereço inválido: {a}")));
        }
    }
    let note = arg_opt_str(args, "note").unwrap_or("").trim().to_string();
    let include_attachments = arg_opt_bool(args, "include_attachments").unwrap_or(true);

    let orig = fetch_message(m, acc, message_id, true).await?;
    let payload = orig.get("payload").cloned().unwrap_or(Value::Null);
    let h = |n: &str| header(&payload, n).unwrap_or_default();
    let body = extract_body(&payload);
    let subject = forward_subject(&h("Subject"));
    let intro = forward_intro(
        &h("From"),
        &h("Date"),
        &h("Subject"),
        &h("To"),
        header(&payload, "Cc").as_deref(),
    );
    let orig_text = body
        .text
        .clone()
        .or_else(|| body.html.as_deref().map(html_to_text))
        .unwrap_or_default();
    let mut text = String::new();
    if !note.is_empty() {
        text.push_str(&note);
        text.push_str("\n\n");
    }
    text.push_str(&intro);
    text.push('\n');
    text.push_str(&orig_text);
    let html = body.html.as_ref().map(|orig_html| {
        let mut h = String::new();
        if !note.is_empty() {
            h.push_str(&format!("<div>{}</div><br>", html_escape(&note)));
        }
        h.push_str(&format!(
            "<div style=\"color:#555\">{}</div><blockquote style=\"margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex\">{}</blockquote>",
            html_escape(&intro),
            orig_html
        ));
        h
    });

    let mut mail = OutgoingMail {
        from: acc.email.clone(),
        to,
        cc,
        bcc,
        subject: subject.clone(),
        text,
        html,
        in_reply_to: None,
        references: Vec::new(),
        attachments: Vec::new(),
    };
    let mut skipped = Vec::new();
    if include_attachments {
        let max_bytes = cfg.max_attachment_mb.max(1) as u64 * 1024 * 1024;
        let mut total = 0u64;
        for a in body.attachments.iter().filter(|a| !a.inline) {
            let Some(att_id) = a.attachment_id.as_deref() else {
                continue;
            };
            if total + a.size > max_bytes {
                skipped.push(a.filename.clone());
                continue;
            }
            let att = m
                .client
                .get(
                    acc,
                    &format!(
                        "{BASE}/messages/{}/attachments/{}",
                        crate::calendar::enc(message_id),
                        crate::calendar::enc(att_id)
                    ),
                    &[],
                )
                .await?;
            let bytes = att
                .get("data")
                .and_then(|d| d.as_str())
                .map(decode_b64url)
                .transpose()
                .map_err(ToolError::Failed)?
                .unwrap_or_default();
            total += bytes.len() as u64;
            if total > max_bytes {
                skipped.push(a.filename.clone());
                continue;
            }
            mail.attachments
                .push((a.mime.clone(), a.filename.clone(), bytes));
        }
    }
    let recipients: Vec<String> = mail
        .to
        .iter()
        .chain(mail.cc.iter())
        .chain(mail.bcc.iter())
        .flat_map(|a| parse_addresses(a))
        .collect();
    let bytes = build_mime(&mail).map_err(ToolError::Failed)?;
    let p = Prepared {
        raw: encode_b64url(&bytes),
        thread_id: None,
        recipients,
        subject,
    };
    let attached = mail.attachments.len();
    let out = deliver(m, acc, p).await?;
    let mut v = out.structured.unwrap_or(Value::Null);
    v["forwarded_message_id"] = json!(message_id);
    v["attachments_included"] = json!(attached);
    v["attachments_skipped"] = json!(skipped);
    Ok(ToolOutput::json(&v))
}

/// Destinatários de "responder a todos": Para = Reply-To ou From (se o remetente
/// sou eu, Para = os Para originais); Cc = Para + Cc originais, menos eu e menos
/// quem já está em Para. Tudo em minúsculas, sem duplicatas, na ordem original.
fn reply_all_recipients(
    me: &str,
    from: &str,
    reply_to: Option<&str>,
    to: &str,
    cc: &str,
) -> (Vec<String>, Vec<String>) {
    let me = me.trim().to_ascii_lowercase();
    let norm = |v: &str| -> Vec<String> {
        parse_addresses(v)
            .into_iter()
            .map(|a| a.to_ascii_lowercase())
            .collect()
    };
    let from_list = norm(from);
    let orig_to = norm(to);
    let orig_cc = norm(cc);
    let from_me = from_list.contains(&me);
    let mut to_out: Vec<String> = match reply_to.map(norm).filter(|v| !v.is_empty()) {
        Some(r) => r,
        None if from_me => orig_to.clone(),
        None => from_list.clone(),
    };
    to_out.retain(|a| *a != me);
    to_out.dedup();
    if to_out.is_empty() {
        to_out = from_list.into_iter().filter(|a| *a != me).collect();
    }
    let mut cc_out: Vec<String> = Vec::new();
    for a in orig_to.into_iter().chain(orig_cc) {
        if a != me && !to_out.contains(&a) && !cc_out.contains(&a) {
            cc_out.push(a);
        }
    }
    (to_out, cc_out)
}

async fn reply_all(
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
    let message_id = arg_str(args, "message_id")?;
    let body_text = arg_str(args, "body_text")?;
    let q = vec![
        ("format", "metadata".to_string()),
        ("metadataHeaders", "From".to_string()),
        ("metadataHeaders", "Reply-To".to_string()),
        ("metadataHeaders", "To".to_string()),
        ("metadataHeaders", "Cc".to_string()),
    ];
    let orig = m
        .client
        .get(
            acc,
            &format!("{BASE}/messages/{}", crate::calendar::enc(message_id)),
            &q,
        )
        .await?;
    let payload = orig.get("payload").cloned().unwrap_or(Value::Null);
    let h = |n: &str| header(&payload, n).unwrap_or_default();
    let (mut to, mut cc) = reply_all_recipients(
        &acc.email,
        &h("From"),
        header(&payload, "Reply-To").as_deref(),
        &h("To"),
        &h("Cc"),
    );
    for a in str_list(args, "extra_to").unwrap_or_default() {
        if !to.contains(&a) {
            to.push(a);
        }
    }
    for a in str_list(args, "extra_cc").unwrap_or_default() {
        if !to.contains(&a) && !cc.contains(&a) {
            cc.push(a);
        }
    }
    if to.is_empty() {
        return Err(ToolError::InvalidArgs(
            "não há a quem responder (a mensagem só tem você como destinatário)".into(),
        ));
    }
    let mut compose = json!({
        "to": to,
        "cc": cc,
        "body_text": body_text,
        "reply_to_message_id": message_id,
    });
    if let Some(h) = arg_opt_str(args, "body_html") {
        compose["body_html"] = json!(h);
    }
    if let Some(a) = args.get("attachments") {
        compose["attachments"] = a.clone();
    }
    // `prepare` resolve In-Reply-To/References, "Re:" e o threadId pela mensagem original.
    let p = prepare(m, acc, &compose, true).await?;
    let out = deliver(m, acc, p).await?;
    let mut v = out.structured.unwrap_or(Value::Null);
    v["in_reply_to_message_id"] = json!(message_id);
    v["cc"] = compose["cc"].clone();
    Ok(ToolOutput::json(&v))
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
    fn reply_all_recipients_rules() {
        // outro remetente: Para = From; Cc = Para+Cc originais menos eu
        let (to, cc) = reply_all_recipients(
            "me@x.com",
            "Ana <ana@x.com>",
            None,
            "me@x.com, Bob <bob@x.com>",
            "carol@x.com, ANA@x.com",
        );
        assert_eq!(to, vec!["ana@x.com"]);
        assert_eq!(cc, vec!["bob@x.com", "carol@x.com"]);
        // Reply-To manda
        let (to, _) =
            reply_all_recipients("me@x.com", "ana@x.com", Some("lista@x.com"), "me@x.com", "");
        assert_eq!(to, vec!["lista@x.com"]);
        // respondendo a um e-mail meu: Para = os Para originais
        let (to, cc) = reply_all_recipients(
            "me@x.com",
            "me@x.com",
            None,
            "ana@x.com, bob@x.com",
            "carol@x.com",
        );
        assert_eq!(to, vec!["ana@x.com", "bob@x.com"]);
        assert_eq!(cc, vec!["carol@x.com"]);
        // só eu: nada a responder
        let (to, cc) = reply_all_recipients("me@x.com", "me@x.com", None, "me@x.com", "");
        assert!(to.is_empty() && cc.is_empty());
    }

    #[test]
    fn forward_subject_and_intro() {
        assert_eq!(forward_subject("Olá"), "Fwd: Olá");
        assert_eq!(forward_subject("Fwd: Olá"), "Fwd: Olá");
        assert_eq!(forward_subject("ENC: x"), "ENC: x");
        let i = forward_intro(
            "Ana <ana@x.com>",
            "Mon, 1 Jan",
            "Olá",
            "me@x.com",
            Some("bob@x.com"),
        );
        assert!(i.starts_with("---------- Mensagem encaminhada ----------\n"));
        assert!(i.contains("De: Ana <ana@x.com>\n") && i.contains("Cc: bob@x.com\n"));
        assert!(!forward_intro("a", "b", "c", "d", None).contains("Cc:"));
        assert_eq!(html_escape("a<b>\n&"), "a&lt;b&gt;<br>&amp;");
        let d = draft_json(
            &json!({"id": "r1", "message": {"id": "m1", "threadId": "t1", "payload": {"headers": [
            {"name": "Subject", "value": "Rascunho"}, {"name": "Bcc", "value": "x@y.com"}]}}}),
        );
        assert_eq!(d["draft_id"], "r1");
        assert_eq!(d["subject"], "Rascunho");
        assert_eq!(d["bcc"], "x@y.com");
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
