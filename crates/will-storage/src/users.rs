//! Usuários e sessões (`users`, `sessions`).

use crate::{fmt, now, parse_dt, Result, Storage, StorageError};
use sqlx::Row;

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserRecord {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: Option<String>,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: String,
    pub active: bool,
    pub must_change_password: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_login_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn row_to_user(r: &sqlx::sqlite::SqliteRow) -> UserRecord {
    UserRecord {
        id: r.get("id"),
        username: r.get("username"),
        name: r.get("name"),
        email: r.get("email"),
        password_hash: r.get("password_hash"),
        role: r.get("role"),
        active: r.get::<i64, _>("active") != 0,
        must_change_password: r.get::<i64, _>("must_change_password") != 0,
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        last_login_at: r
            .get::<Option<String>, _>("last_login_at")
            .map(|s| parse_dt(&s)),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionRecord {
    pub token_hash: String,
    pub user_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub client: Option<String>,
}

impl Storage {
    pub async fn count_users(&self) -> Result<i64> {
        let r = sqlx::query("SELECT COUNT(*) AS n FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(r.get("n"))
    }

    pub async fn list_users(&self) -> Result<Vec<UserRecord>> {
        let rows = sqlx::query("SELECT * FROM users ORDER BY created_at ASC")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(row_to_user).collect())
    }

    pub async fn get_user(&self, id: &str) -> Result<UserRecord> {
        let r = sqlx::query("SELECT * FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StorageError::NotFound)?;
        Ok(row_to_user(&r))
    }

    /// Por nome de usuário ou e-mail (sem diferenciar maiúsculas).
    pub async fn find_user_by_login(&self, login: &str) -> Result<Option<UserRecord>> {
        let r = sqlx::query(
            "SELECT * FROM users WHERE username = ? COLLATE NOCASE OR (email IS NOT NULL AND email = ? COLLATE NOCASE) LIMIT 1",
        )
        .bind(login)
        .bind(login)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(row_to_user))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_user(
        &self,
        id: &str,
        username: &str,
        name: &str,
        email: Option<&str>,
        password_hash: &str,
        role: &str,
        must_change_password: bool,
    ) -> Result<UserRecord> {
        sqlx::query(
            "INSERT INTO users (id, username, name, email, password_hash, role, active, must_change_password, created_at) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(id)
        .bind(username)
        .bind(name)
        .bind(email)
        .bind(password_hash)
        .bind(role)
        .bind(must_change_password as i64)
        .bind(fmt(now()))
        .execute(&self.pool)
        .await?;
        self.get_user(id).await
    }

    pub async fn update_user(
        &self,
        id: &str,
        name: Option<&str>,
        email: Option<Option<&str>>,
        role: Option<&str>,
        active: Option<bool>,
    ) -> Result<UserRecord> {
        if let Some(n) = name {
            sqlx::query("UPDATE users SET name = ? WHERE id = ?")
                .bind(n)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        if let Some(e) = email {
            sqlx::query("UPDATE users SET email = ? WHERE id = ?")
                .bind(e)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        if let Some(r) = role {
            sqlx::query("UPDATE users SET role = ? WHERE id = ?")
                .bind(r)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        if let Some(a) = active {
            sqlx::query("UPDATE users SET active = ? WHERE id = ?")
                .bind(a as i64)
                .bind(id)
                .execute(&self.pool)
                .await?;
            if !a {
                // desativar derruba as sessões
                sqlx::query("DELETE FROM sessions WHERE user_id = ?")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
        }
        self.get_user(id).await
    }

    pub async fn set_password(
        &self,
        id: &str,
        password_hash: &str,
        must_change: bool,
    ) -> Result<()> {
        let res = sqlx::query(
            "UPDATE users SET password_hash = ?, must_change_password = ? WHERE id = ?",
        )
        .bind(password_hash)
        .bind(must_change as i64)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub async fn touch_login(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
            .bind(fmt(now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_session(
        &self,
        token_hash: &str,
        user_id: &str,
        ttl: chrono::Duration,
        client: Option<&str>,
    ) -> Result<()> {
        let ts = now();
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at, last_used_at, client) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(user_id)
        .bind(fmt(ts))
        .bind(fmt(ts + ttl))
        .bind(fmt(ts))
        .bind(client)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Sessão válida (não expirada) + usuário ativo. Atualiza `last_used_at`
    /// no máximo uma vez por minuto.
    pub async fn session_user(&self, token_hash: &str) -> Result<Option<UserRecord>> {
        let r = sqlx::query(
            "SELECT u.*, s.expires_at AS s_expires, s.last_used_at AS s_last FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else { return Ok(None) };
        let expires = parse_dt(&r.get::<String, _>("s_expires"));
        if expires < now() {
            let _ = self.delete_session(token_hash).await;
            return Ok(None);
        }
        let user = row_to_user(&r);
        if !user.active {
            return Ok(None);
        }
        let last = r.get::<Option<String>, _>("s_last").map(|s| parse_dt(&s));
        if last
            .map(|l| now() - l > chrono::Duration::minutes(1))
            .unwrap_or(true)
        {
            let _ = sqlx::query("UPDATE sessions SET last_used_at = ? WHERE token_hash = ?")
                .bind(fmt(now()))
                .bind(token_hash)
                .execute(&self.pool)
                .await;
        }
        Ok(Some(user))
    }

    pub async fn delete_session(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_user_sessions(&self, user_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_sessions(&self, user_id: &str) -> Result<Vec<SessionRecord>> {
        let rows = sqlx::query("SELECT * FROM sessions WHERE user_id = ? ORDER BY created_at DESC")
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| SessionRecord {
                token_hash: r.get("token_hash"),
                user_id: r.get("user_id"),
                created_at: parse_dt(&r.get::<String, _>("created_at")),
                expires_at: parse_dt(&r.get::<String, _>("expires_at")),
                last_used_at: r
                    .get::<Option<String>, _>("last_used_at")
                    .map(|s| parse_dt(&s)),
                client: r.get("client"),
            })
            .collect())
    }

    pub async fn purge_expired_sessions(&self) -> Result<u64> {
        let res = sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
            .bind(fmt(now()))
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }
}
