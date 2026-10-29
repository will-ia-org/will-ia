use super::{bad, internal, ApiError, ApiResult, App};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use will_core::settings::ClaudeTokenRef;

fn masked(s: &str) -> String {
    will_storage::secrets::mask(s)
}

/// Impressão digital do token (SHA-256, primeiros 16 bytes em hex): compara
/// cadastros sem guardar nem expor o segredo. `trim()` para colar com espaço.
pub fn fingerprint(token: &str) -> String {
    let digest = Sha256::digest(token.trim().as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Token já cadastrado? Devolve `(id, rótulo)` do existente. Entradas antigas
/// sem `fingerprint` são completadas lendo o segredo do cofre (migração
/// preguiçosa) e persistidas.
async fn find_duplicate(state: &App, fp: &str) -> Result<Option<(String, String)>, ApiError> {
    let tokens = state.settings().engine.tokens;
    let mut filled: Vec<(String, String)> = Vec::new();
    let mut found = None;
    for t in &tokens {
        let this = match &t.fingerprint {
            Some(f) => f.clone(),
            None => {
                let Some(secret) = state.secrets.get(&t.secret_ref).await.map_err(internal)? else {
                    continue;
                };
                let f = fingerprint(secret.expose_secret());
                filled.push((t.id.clone(), f.clone()));
                f
            }
        };
        if this == fp {
            found = Some((t.id.clone(), t.label.clone()));
            break;
        }
    }
    if !filled.is_empty() {
        {
            let mut s = state.settings.write();
            for (id, f) in &filled {
                if let Some(t) = s.engine.tokens.iter_mut().find(|t| &t.id == id) {
                    t.fingerprint = Some(f.clone());
                }
            }
        }
        state.save_settings().await.map_err(internal)?;
    }
    Ok(found)
}

pub async fn list_tokens(State(state): State<App>) -> ApiResult<Value> {
    let tokens = state.settings().engine.tokens;
    let mut out = vec![];
    for t in tokens {
        let m = state
            .secrets
            .get(&t.secret_ref)
            .await
            .ok()
            .flatten()
            .map(|s| masked(s.expose_secret()))
            .unwrap_or_else(|| "(ausente)".into());
        out.push(json!({"id": t.id, "label": t.label, "active": t.active, "status": t.status, "verified_at": t.verified_at, "error": t.error, "selected_at": t.selected_at, "masked": m}));
    }
    Ok(Json(json!(out)))
}

#[derive(serde::Deserialize)]
pub struct AddToken {
    pub label: String,
    pub token: String,
    pub activate: Option<bool>,
}

pub async fn add_token(
    State(state): State<App>,
    Json(b): Json<AddToken>,
) -> Result<Response, ApiError> {
    let token = b.token.trim().to_string();
    if token.len() < 20 {
        return Err(bad("token muito curto"));
    }
    let fp = fingerprint(&token);
    if let Some((existing_id, existing_label)) = find_duplicate(&state, &fp).await? {
        // 409: mesmo token já cadastrado — nada é gravado. O `error` traz o rótulo
        // porque os clientes mostram só essa string.
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("token já cadastrado como '{existing_label}'"),
                "existing_id": existing_id,
                "existing_label": existing_label,
            })),
        )
            .into_response());
    }
    let id = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let secret_ref = format!("claude_token:{id}");
    state
        .secrets
        .set(&secret_ref, &SecretString::from(token))
        .await
        .map_err(internal)?;
    let first = {
        let mut s = state.settings.write();
        let first = s.engine.tokens.is_empty();
        s.engine.tokens.push(ClaudeTokenRef {
            id: id.clone(),
            label: if b.label.trim().is_empty() {
                format!("Token {id}")
            } else {
                b.label.trim().to_string()
            },
            secret_ref,
            active: false,
            fingerprint: Some(fp),
            ..Default::default()
        });
        first
    };
    state.save_settings().await.map_err(internal)?;
    let mut result = json!({ "id": id, "activated": false });
    if first || b.activate.unwrap_or(false) {
        match crate::modules::activate_token(&state, &id).await {
            Ok(n) => {
                result["activated"] = json!(true);
                result["restarted_threads"] = json!(n);
            }
            Err(e) => {
                result["activation_error"] = json!(e);
            }
        }
    }
    Ok(Json(result).into_response())
}

pub async fn test_token(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let entry = state
        .settings()
        .engine
        .tokens
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| bad("token não encontrado"))?;
    let secret = state
        .secrets
        .get(&entry.secret_ref)
        .await
        .map_err(internal)?
        .ok_or_else(|| bad("segredo ausente no keyring"))?;
    let r = crate::modules::test_token(&state, &secret).await;
    {
        let mut s = state.settings.write();
        if let Some(t) = s.engine.tokens.iter_mut().find(|t| t.id == id) {
            t.verified_at = Some(chrono::Utc::now());
            t.status = Some(if r.ok { "ok".into() } else { "error".into() });
            t.error = r.error.clone();
        }
    }
    let _ = state.save_settings().await;
    Ok(Json(
        json!({ "ok": r.ok, "error": r.error, "output": crate::logging::redact(&r.output_tail) }),
    ))
}

pub async fn activate_token(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let n = crate::modules::activate_token(&state, &id)
        .await
        .map_err(bad)?;
    Ok(Json(
        json!({ "ok": true, "restarted_threads": n, "killed": n }),
    ))
}

pub async fn delete_token(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    let entry = {
        let mut s = state.settings.write();
        let idx = s.engine.tokens.iter().position(|t| t.id == id);
        idx.map(|i| s.engine.tokens.remove(i))
    };
    let Some(entry) = entry else {
        return Err(bad("token não encontrado"));
    };
    let _ = state.secrets.delete(&entry.secret_ref).await;
    state.save_settings().await.map_err(internal)?;
    if entry.active {
        crate::modules::refresh_engine(&state, true).await;
        state.set_module_status(
            will_core::Module::Engine,
            will_core::ModuleStatus::Degraded("Nenhum token do Claude ativo".into()),
        );
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn check(State(state): State<App>) -> ApiResult<Value> {
    let bin = state.settings().engine.claude_bin;
    let bin = if bin.trim().is_empty() {
        "claude".to_string()
    } else {
        bin
    };
    match will_platform::which(&bin) {
        Some(p) => {
            let v = will_platform::version_of(&p).await;
            Ok(Json(json!({ "found": true, "path": p, "version": v })))
        }
        None => Ok(Json(
            json!({ "found": false, "error": format!("'{bin}' não encontrado no PATH. Instale o Claude Code (npm i -g @anthropic-ai/claude-code) ou informe o caminho completo.") }),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::fingerprint;

    #[test]
    fn fingerprint_is_deterministic_trimmed_hex() {
        let a = fingerprint("sk-ant-oat01-abcdefghijklmnopqrstuvwxyz");
        let b = fingerprint("  sk-ant-oat01-abcdefghijklmnopqrstuvwxyz\n");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, fingerprint("sk-ant-oat01-outro-token-qualquer-xyz"));
    }
}
