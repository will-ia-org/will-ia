//! Usuários (admin): listar, criar, alterar, desativar, redefinir senha.

use super::{bad, ApiResult, App};
use crate::auth;
use axum::extract::{Extension, Path, State};
use axum::Json;
use serde_json::{json, Value};
use will_core::Principal;

pub async fn list(State(state): State<App>) -> ApiResult<Value> {
    Ok(Json(json!(state.storage.list_users().await?)))
}

#[derive(serde::Deserialize)]
pub struct CreateBody {
    pub username: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub password: String,
    #[serde(default)]
    pub role: Option<String>,
    /// Obrigar troca de senha no primeiro login (padrão: sim).
    #[serde(default)]
    pub must_change_password: Option<bool>,
}

pub async fn create(State(state): State<App>, Json(b): Json<CreateBody>) -> ApiResult<Value> {
    let username = b.username.trim().to_lowercase();
    if username.is_empty()
        || !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(bad("nome de usuário inválido (letras, números, ., _ e -)"));
    }
    auth::check_password_strength(&b.password).map_err(bad)?;
    if state.storage.find_user_by_login(&username).await?.is_some() {
        return Err(bad("já existe um usuário com esse nome"));
    }
    let hash = auth::hash_password(&b.password).map_err(super::internal)?;
    let id = will_core::ThreadId::new().to_string();
    let role = match b.role.as_deref() {
        Some("admin") => "admin",
        _ => "member",
    };
    let u = state
        .storage
        .create_user(
            &id,
            &username,
            b.name
                .as_deref()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or(&username),
            b.email.as_deref().filter(|e| !e.trim().is_empty()),
            &hash,
            role,
            b.must_change_password.unwrap_or(true),
        )
        .await?;
    state.bus.log(
        "info",
        "auth",
        format!("usuário criado: {} ({role})", u.username),
    );
    Ok(Json(json!(u)))
}

#[derive(serde::Deserialize, Default)]
pub struct PatchBody {
    pub name: Option<String>,
    pub email: Option<String>,
    pub role: Option<String>,
    pub active: Option<bool>,
    /// Redefine a senha (admin); o usuário precisará trocá-la no próximo login.
    pub password: Option<String>,
}

pub async fn patch(
    State(state): State<App>,
    Extension(me): Extension<Principal>,
    Path(id): Path<String>,
    Json(b): Json<PatchBody>,
) -> ApiResult<Value> {
    if me.user_id.as_deref() == Some(id.as_str())
        && (b.active == Some(false) || b.role.as_deref() == Some("member"))
    {
        return Err(bad("você não pode desativar nem rebaixar a própria conta"));
    }
    let role = b
        .role
        .as_deref()
        .map(|r| if r == "admin" { "admin" } else { "member" });
    let email = b.email.as_deref().map(|e| {
        if e.trim().is_empty() {
            None
        } else {
            Some(e.trim())
        }
    });
    let u = state
        .storage
        .update_user(&id, b.name.as_deref(), email, role, b.active)
        .await?;
    if let Some(pw) = b.password.as_deref().filter(|p| !p.is_empty()) {
        auth::check_password_strength(pw).map_err(bad)?;
        let hash = auth::hash_password(pw).map_err(super::internal)?;
        state.storage.set_password(&id, &hash, true).await?;
        let _ = state.storage.delete_user_sessions(&id).await;
    }
    Ok(Json(json!(u)))
}

/// Desativa (não apaga: autoria em turnos/aprovações continua válida).
pub async fn delete(
    State(state): State<App>,
    Extension(me): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    if me.user_id.as_deref() == Some(id.as_str()) {
        return Err(bad("você não pode desativar a própria conta"));
    }
    let u = state
        .storage
        .update_user(&id, None, None, None, Some(false))
        .await?;
    Ok(Json(json!(u)))
}

pub async fn sessions(State(state): State<App>, Path(id): Path<String>) -> ApiResult<Value> {
    Ok(Json(json!(state.storage.list_sessions(&id).await?)))
}
