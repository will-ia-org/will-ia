//! Login, sessão e troca de senha (`/api/auth/*`).

use super::{bad, ApiError, ApiResult, App};
use crate::auth;
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::net::SocketAddr;
use will_core::Principal;

#[derive(serde::Deserialize)]
pub struct LoginBody {
    pub login: String,
    pub password: String,
    #[serde(default)]
    pub client: Option<String>,
}

/// `POST /api/auth/login` (sem token). Devolve o token de sessão e o usuário.
pub async fn login(
    State(state): State<App>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(b): Json<LoginBody>,
) -> ApiResult<Value> {
    if !state.login_limiter.allow(addr.ip()) {
        tracing::warn!(ip = %addr.ip(), "login bloqueado por excesso de tentativas");
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "muitas tentativas; aguarde alguns minutos".into(),
        ));
    }
    let login = b.login.trim();
    let user = state
        .storage
        .find_user_by_login(login)
        .await?
        .filter(|u| u.active && auth::verify_password(&b.password, &u.password_hash))
        .ok_or_else(|| {
            tracing::warn!(ip = %addr.ip(), login, "login recusado: usuário ou senha inválidos");
            ApiError(
                StatusCode::UNAUTHORIZED,
                "usuário ou senha inválidos".into(),
            )
        })?;
    state.login_limiter.reset(addr.ip());
    let token = auth::random_token();
    state
        .storage
        .create_session(
            &auth::token_hash(&token),
            &user.id,
            chrono::Duration::days(auth::SESSION_TTL_DAYS),
            b.client.as_deref(),
        )
        .await?;
    let _ = state.storage.touch_login(&user.id).await;
    tracing::info!(
        target: "will_auth",
        user = %user.username,
        ip = %addr.ip(),
        client = b.client.as_deref().unwrap_or(""),
        "login de {} ({})",
        user.username,
        addr.ip()
    );
    Ok(Json(json!({
        "token": token,
        "user": auth::principal_of(&user),
        "must_change_password": user.must_change_password,
    })))
}

/// `POST /api/auth/logout`: invalida a sessão do token usado.
pub async fn logout(
    State(state): State<App>,
    Extension(token): Extension<super::BearerToken>,
) -> ApiResult<Value> {
    state
        .storage
        .delete_session(&auth::token_hash(&token.0))
        .await?;
    tracing::info!("logout: sessão encerrada");
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/auth/me`: quem sou eu.
pub async fn me(Extension(p): Extension<Principal>) -> ApiResult<Value> {
    Ok(Json(json!(p)))
}

#[derive(serde::Deserialize)]
pub struct PasswordBody {
    #[serde(default)]
    pub current: String,
    pub new: String,
}

/// `POST /api/auth/password`: troca a própria senha (exige a atual, salvo no
/// primeiro login obrigatório).
pub async fn change_password(
    State(state): State<App>,
    Extension(p): Extension<Principal>,
    Json(b): Json<PasswordBody>,
) -> ApiResult<Value> {
    let id = p
        .user_id
        .clone()
        .ok_or_else(|| bad("o token local não tem senha"))?;
    let user = state.storage.get_user(&id).await?;
    if !user.must_change_password && !auth::verify_password(&b.current, &user.password_hash) {
        tracing::warn!(user = %p.username, "troca de senha recusada: senha atual incorreta");
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "senha atual incorreta".into(),
        ));
    }
    auth::check_password_strength(&b.new).map_err(bad)?;
    let hash = auth::hash_password(&b.new).map_err(super::internal)?;
    state.storage.set_password(&id, &hash, false).await?;
    let _ = std::fs::remove_file(state.paths.data_dir.join(auth::INITIAL_PASSWORD_FILE));
    tracing::info!(user = %p.username, first_login = user.must_change_password, "senha alterada");
    Ok(Json(json!({ "ok": true })))
}
