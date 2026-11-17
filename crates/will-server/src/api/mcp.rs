use super::{bad, internal, ApiResult, App};
use axum::extract::{Path, Query, State};
use axum::Json;
use secrecy::SecretString;
use serde_json::{json, Map, Value};
use std::time::Duration;
use will_core::settings::{ConfigValue, ExternalMcpServer};
use will_core::{ApprovalDecision, ApprovalId, Recipient, ThreadHost, ToolPolicy, TrustChannel};

/// Token fixo para scripts fora das threads (o valor só sai pela API local da UI).
pub async fn script_token(State(state): State<App>) -> ApiResult<Value> {
    let enabled = state.settings().mcp.enabled;
    let token = crate::modules::ensure_script_token(&state)
        .await
        .map_err(internal)?;
    Ok(Json(script_token_json(&state, &token, enabled)))
}

pub async fn rotate_script_token(State(state): State<App>) -> ApiResult<Value> {
    let enabled = state.settings().mcp.enabled;
    let token = crate::modules::rotate_script_token(&state)
        .await
        .map_err(internal)?;
    Ok(Json(script_token_json(&state, &token, enabled)))
}

fn script_token_json(state: &App, token: &str, enabled: bool) -> Value {
    json!({
        "token": token,
        "masked": will_storage::secrets::mask(token),
        "url": state.mcp_url.read().clone(),
        "active": enabled && !state.mcp_url.read().is_empty(),
    })
}

pub async fn tools(State(state): State<App>) -> ApiResult<Value> {
    Ok(Json(json!(state.registry.listing())))
}

#[derive(serde::Deserialize)]
pub struct PolicyBody {
    pub policy: Option<String>,
}

pub async fn set_policy(
    State(state): State<App>,
    Path(name): Path<String>,
    Json(b): Json<PolicyBody>,
) -> ApiResult<Value> {
    let p = match b.policy.as_deref() {
        None | Some("") | Some("default") | Some("null") => None,
        Some(s) => Some(ToolPolicy::parse(s).ok_or_else(|| bad("política inválida"))?),
    };
    if p == Some(ToolPolicy::AutoListed) {
        // só faz sentido em ferramenta que sabe dizer para quem vai
        let has_recipient = state
            .registry
            .find(&name)
            .is_some_and(|(_, tool, _)| tool.trust_channel().is_some());
        if !has_recipient {
            return Err(bad(
                "\"automático para listados\" só vale para ferramentas com destinatário",
            ));
        }
    }
    state.registry.set_policy(&name, p);
    state.storage.set_tool_policy(&name, p).await?;
    tracing::info!(
        tool = %name,
        policy = p.map(|p| p.as_str()).unwrap_or("default"),
        "política da ferramenta alterada"
    );
    {
        let mut s = state.settings.write();
        match p {
            Some(p) => {
                s.mcp.policies.insert(name.clone(), p);
            }
            None => {
                s.mcp.policies.remove(&name);
            }
        }
    }
    let _ = state.save_settings().await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
pub struct Limit {
    pub limit: Option<i64>,
}

pub async fn calls(State(state): State<App>, Query(q): Query<Limit>) -> ApiResult<Value> {
    Ok(Json(json!(
        state
            .storage
            .list_mcp_calls(q.limit.unwrap_or(200).min(2000))
            .await?
    )))
}

pub async fn approvals(State(state): State<App>) -> ApiResult<Value> {
    Ok(Json(json!(state.approvals.pending())))
}

pub async fn approvals_history(
    State(state): State<App>,
    Query(q): Query<Limit>,
) -> ApiResult<Value> {
    let rows = state
        .storage
        .list_approvals(q.limit.unwrap_or(100).min(1000))
        .await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|r| json!({"id": r.id, "requested_at": r.requested_at, "thread_id": r.thread_id, "tool": r.tool, "summary": r.summary, "args_redacted": r.args_redacted, "decided_at": r.decided_at, "decision": r.decision, "via": r.decided_via, "decided_via": r.decided_via}))
        .collect();
    Ok(Json(json!(out)))
}

// ----- destinatários confiáveis (política `auto_listed`, docs/09) -----

fn trusted_json(state: &App) -> Value {
    json!(state.settings().mcp.trusted)
}

pub async fn trusted_list(State(state): State<App>) -> ApiResult<Value> {
    Ok(Json(trusted_json(&state)))
}

#[derive(serde::Deserialize)]
pub struct TrustedBody {
    /// `whatsapp` | `email`.
    pub channel: String,
    /// Como digitado: telefone, JID de grupo, e-mail ou `@dominio`.
    pub value: String,
    /// Nome para mostrar na lista (opcional).
    #[serde(default)]
    pub label: String,
}

fn trusted_target(b: &TrustedBody) -> Result<(TrustChannel, String), super::ApiError> {
    let channel = TrustChannel::parse(&b.channel).ok_or_else(|| bad("canal inválido"))?;
    let key = channel.key(&b.value).ok_or_else(|| {
        bad(match channel {
            TrustChannel::WhatsApp => {
                "informe um telefone com DDI (ex.: 5511987654321) ou o JID de um grupo (…@g.us)"
            }
            TrustChannel::Email => {
                "informe um e-mail (fulano@empresa.com) ou um domínio (@empresa.com)"
            }
        })
    })?;
    Ok((channel, key))
}

pub async fn trusted_add(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
    Json(b): Json<TrustedBody>,
) -> ApiResult<Value> {
    let (channel, key) = trusted_target(&b)?;
    let changed = state
        .settings
        .write()
        .mcp
        .trusted
        .add(channel, Recipient::new(key.clone(), b.label.trim()));
    if changed {
        state.save_settings().await.map_err(internal)?;
        tracing::info!(channel = channel.as_str(), key = %key, user = %me.username, "destinatário confiável adicionado");
    }
    Ok(Json(trusted_json(&state)))
}

pub async fn trusted_remove(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
    Json(b): Json<TrustedBody>,
) -> ApiResult<Value> {
    let channel = TrustChannel::parse(&b.channel).ok_or_else(|| bad("canal inválido"))?;
    // remove pela chave gravada (o app manda a chave) ou pelo valor digitado
    let key = channel
        .key(&b.value)
        .unwrap_or_else(|| b.value.trim().to_string());
    let removed = state.settings.write().mcp.trusted.remove(channel, &key);
    if removed {
        state.save_settings().await.map_err(internal)?;
        tracing::info!(channel = channel.as_str(), key = %key, user = %me.username, "destinatário confiável removido");
    }
    Ok(Json(trusted_json(&state)))
}

#[derive(serde::Deserialize)]
pub struct DecideBody {
    pub decision: String,
    /// Com `approved`: inclui na lista de confiança os destinatários que o
    /// pedido oferece (`ApprovalRequest.trust`), para as próximas irem sozinhas.
    #[serde(default)]
    pub trust: bool,
}

pub async fn decide(
    State(state): State<App>,
    axum::extract::Extension(me): axum::extract::Extension<will_core::Principal>,
    Path(id): Path<String>,
    Json(b): Json<DecideBody>,
) -> ApiResult<Value> {
    let id = ApprovalId::parse(&id).map_err(|_| bad("id inválido"))?;
    let d = match b.decision.as_str() {
        "approved" | "approve" | "yes" => ApprovalDecision::Approved,
        _ => ApprovalDecision::Denied,
    };
    let trust = b.trust && d == ApprovalDecision::Approved;
    // a oferta sai do pedido pendente (montado pelo serviço), nunca do corpo
    let offer = if trust {
        state
            .approvals
            .pending()
            .into_iter()
            .find(|r| r.id == id)
            .and_then(|r| r.trust)
    } else {
        None
    };
    let via = if offer.is_some() {
        format!("ui:{}+confiar", me.author())
    } else {
        format!("ui:{}", me.author())
    };
    let ok = state.host().decide_approval(id, d, &via);
    if !ok {
        tracing::warn!(approval_id = %id, user = %me.username, "decisão recebida para pedido que não está mais pendente");
        return Err(bad("pedido não está mais pendente"));
    }
    let mut trusted = 0;
    if let Some(offer) = offer {
        {
            let mut s = state.settings.write();
            for r in offer.recipients {
                if s.mcp.trusted.add(offer.channel, r) {
                    trusted += 1;
                }
            }
        }
        if trusted > 0 {
            state.save_settings().await.map_err(internal)?;
            tracing::info!(approval_id = %id, channel = offer.channel.as_str(), added = trusted, user = %me.username, "destinatários confiáveis adicionados pela aprovação");
        }
    }
    Ok(Json(json!({ "ok": true, "trusted": trusted })))
}

fn mask_external(e: &ExternalMcpServer) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or_default();
    for section in ["env", "headers"] {
        if let Some(m) = v.get_mut(section).and_then(|x| x.as_object_mut()) {
            for (_, val) in m.iter_mut() {
                if val.get("$secret").is_some() {
                    *val = json!({ "$secret": val["$secret"], "configured": true });
                }
            }
        }
    }
    v
}

pub async fn external_list(State(state): State<App>) -> ApiResult<Value> {
    let list: Vec<Value> = state
        .settings()
        .external_mcps
        .iter()
        .map(mask_external)
        .collect();
    Ok(Json(json!(list)))
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Converte valores `{ "$secret_value": "..." }` em segredos no keyring.
async fn absorb_secrets(
    state: &App,
    name: &str,
    map: &mut Map<String, Value>,
) -> Result<(), String> {
    for (k, v) in map.iter_mut() {
        if let Some(sv) = v.get("$secret_value").and_then(|s| s.as_str()) {
            let key = format!("mcp:{name}:{k}");
            if !sv.is_empty() {
                state
                    .secrets
                    .set(&key, &SecretString::from(sv.to_string()))
                    .await?;
            }
            *v = json!({ "$secret": key });
        } else if v.get("$secret").is_some() {
            // mantém referência existente
            let key = v["$secret"].as_str().unwrap_or_default().to_string();
            *v = json!({ "$secret": key });
        } else if !v.is_string() {
            *v = Value::String(v.to_string());
        }
    }
    Ok(())
}

pub async fn external_put(
    State(state): State<App>,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> ApiResult<Value> {
    if !valid_name(&name) {
        return Err(bad("nome inválido: use letras, números, _ ou -"));
    }
    if name == "will_ia" {
        return Err(bad("nome reservado"));
    }
    let obj = body.as_object_mut().ok_or_else(|| bad("corpo inválido"))?;
    obj.insert("name".into(), json!(name));
    for section in ["env", "headers"] {
        if let Some(m) = obj.get_mut(section).and_then(|x| x.as_object_mut()) {
            absorb_secrets(&state, &name, m).await.map_err(internal)?;
        }
    }
    let e: ExternalMcpServer =
        serde_json::from_value(body).map_err(|e| bad(format!("servidor inválido: {e}")))?;
    if !["stdio", "http", "sse"].contains(&e.kind.as_str()) {
        return Err(bad("tipo deve ser stdio, http ou sse"));
    }
    let (kind, enabled) = (e.kind.clone(), e.enabled);
    {
        let mut s = state.settings.write();
        s.external_mcps.retain(|x| x.name != name);
        s.external_mcps.push(e);
        s.external_mcps.sort_by(|a, b| a.name.cmp(&b.name));
    }
    state.save_settings().await.map_err(internal)?;
    tracing::info!(server = %name, kind, enabled, "MCP externo salvo");
    crate::modules::apply_section(&state, "external_mcps").await;
    external_list(State(state)).await
}

pub async fn external_delete(
    State(state): State<App>,
    Path(name): Path<String>,
) -> ApiResult<Value> {
    let removed = {
        let mut s = state.settings.write();
        let before = s.external_mcps.len();
        s.external_mcps.retain(|x| x.name != name);
        before != s.external_mcps.len()
    };
    if !removed {
        return Err(bad("servidor não encontrado"));
    }
    state.save_settings().await.map_err(internal)?;
    tracing::info!(server = %name, "MCP externo removido");
    crate::modules::apply_section(&state, "external_mcps").await;
    Ok(Json(json!({ "ok": true })))
}

async fn resolve(state: &App, e: &ExternalMcpServer) -> will_mcp::external::ResolvedExternal {
    use secrecy::ExposeSecret;
    let mut env = std::collections::BTreeMap::new();
    let mut headers = std::collections::BTreeMap::new();
    for (dst, src) in [(&mut env, &e.env), (&mut headers, &e.headers)] {
        for (k, v) in src {
            let val = match v {
                ConfigValue::Plain(s) => s.clone(),
                ConfigValue::Secret { secret } => state
                    .secrets
                    .get(secret)
                    .await
                    .ok()
                    .flatten()
                    .map(|s| s.expose_secret().to_string())
                    .unwrap_or_default(),
            };
            dst.insert(k.clone(), val);
        }
    }
    will_mcp::external::ResolvedExternal {
        name: e.name.clone(),
        kind: e.kind.clone(),
        command: e.command.clone(),
        args: e.args.clone(),
        env,
        cwd: e.cwd.clone(),
        url: e.url.clone(),
        headers,
    }
}

pub async fn external_test(State(state): State<App>, Path(name): Path<String>) -> ApiResult<Value> {
    let e = state
        .settings()
        .external_mcps
        .into_iter()
        .find(|x| x.name == name)
        .ok_or_else(|| bad("servidor não encontrado"))?;
    let def = resolve(&state, &e).await;
    let r = will_mcp::external::test_external(&def, Duration::from_secs(45)).await;
    if r.ok {
        tracing::info!(server = %name, tools = r.tools.len(), "teste de MCP externo: ok");
    } else {
        tracing::warn!(server = %name, error = r.error.as_deref().unwrap_or(""), "teste de MCP externo: falhou");
    }
    {
        let mut s = state.settings.write();
        if let Some(x) = s.external_mcps.iter_mut().find(|x| x.name == name) {
            x.last_test = Some(r.clone());
        }
    }
    let _ = state.save_settings().await;
    Ok(Json(json!(r)))
}

pub async fn external_import_claude(State(state): State<App>) -> ApiResult<Value> {
    let servers = crate::mcp_json::read_claude_user_mcps().map_err(bad)?;
    let imported = crate::mcp_json::import_servers(&state, &servers).await;
    tracing::info!(
        found = servers.len(),
        imported = imported.len(),
        "MCPs importados do ~/.claude.json"
    );
    state.save_settings().await.map_err(internal)?;
    crate::modules::apply_section(&state, "external_mcps").await;
    Ok(Json(
        json!({ "imported": imported, "count": imported.len(), "found": servers.len() }),
    ))
}

#[derive(serde::Deserialize)]
pub struct ImportJson {
    pub json: Value,
}

pub async fn external_import_json(
    State(state): State<App>,
    Json(b): Json<ImportJson>,
) -> ApiResult<Value> {
    let v: Value = match b.json {
        Value::String(s) => {
            serde_json::from_str(&s).map_err(|e| bad(format!("JSON inválido: {e}")))?
        }
        other => other,
    };
    let servers: Map<String, Value> = match v.get("mcpServers") {
        Some(Value::Object(m)) => m.clone(),
        _ => v
            .as_object()
            .cloned()
            .ok_or_else(|| bad("esperado objeto com mcpServers"))?,
    };
    let imported = crate::mcp_json::import_servers(&state, &servers).await;
    tracing::info!(imported = imported.len(), "MCPs importados de JSON colado");
    state.save_settings().await.map_err(internal)?;
    crate::modules::apply_section(&state, "external_mcps").await;
    Ok(Json(
        json!({ "imported": imported, "count": imported.len() }),
    ))
}
