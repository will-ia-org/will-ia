use crate::{fmt, now, parse_dt, Result, Storage};
use sqlx::Row;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GoogleAccount {
    pub id: String,
    pub email: String,
    pub secret_ref: String,
    pub scopes: Vec<String>,
    pub status: String,
    pub connected_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GoogleCalendar {
    pub account_id: String,
    pub calendar_id: String,
    pub summary: String,
    pub timezone: Option<String>,
    pub access_role: Option<String>,
    pub color: Option<String>,
    pub enabled: bool,
    pub default_for_create: bool,
}

impl Storage {
    pub async fn upsert_google_account(&self, a: &GoogleAccount) -> Result<()> {
        sqlx::query(
            "INSERT INTO google_accounts (id, email, secret_ref, scopes, status, connected_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(email) DO UPDATE SET secret_ref = excluded.secret_ref, scopes = excluded.scopes, status = excluded.status, last_used_at = excluded.last_used_at",
        )
        .bind(&a.id)
        .bind(&a.email)
        .bind(&a.secret_ref)
        .bind(a.scopes.join(" "))
        .bind(&a.status)
        .bind(fmt(a.connected_at))
        .bind(a.last_used_at.map(fmt))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_google_accounts(&self) -> Result<Vec<GoogleAccount>> {
        let rows = sqlx::query("SELECT * FROM google_accounts ORDER BY connected_at")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let scopes: String = r.get("scopes");
                let c: String = r.get("connected_at");
                let l: Option<String> = r.get("last_used_at");
                GoogleAccount {
                    id: r.get("id"),
                    email: r.get("email"),
                    secret_ref: r.get("secret_ref"),
                    scopes: scopes.split_whitespace().map(|s| s.to_string()).collect(),
                    status: r.get("status"),
                    connected_at: parse_dt(&c),
                    last_used_at: l.as_deref().map(parse_dt),
                }
            })
            .collect())
    }

    pub async fn get_google_account(&self, id: &str) -> Result<Option<GoogleAccount>> {
        Ok(self
            .list_google_accounts()
            .await?
            .into_iter()
            .find(|a| a.id == id || a.email == id))
    }

    pub async fn set_google_account_status(&self, id: &str, status: &str) -> Result<()> {
        sqlx::query("UPDATE google_accounts SET status = ?, last_used_at = ? WHERE id = ?")
            .bind(status)
            .bind(fmt(now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_google_account(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM google_accounts WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_google_calendar(&self, c: &GoogleCalendar) -> Result<()> {
        sqlx::query(
            "INSERT INTO google_calendars (account_id, calendar_id, summary, timezone, access_role, color, enabled, default_for_create) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(account_id, calendar_id) DO UPDATE SET summary = excluded.summary, timezone = excluded.timezone, access_role = excluded.access_role, color = excluded.color",
        )
        .bind(&c.account_id)
        .bind(&c.calendar_id)
        .bind(&c.summary)
        .bind(&c.timezone)
        .bind(&c.access_role)
        .bind(&c.color)
        .bind(c.enabled as i64)
        .bind(c.default_for_create as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_google_calendar_flags(
        &self,
        account_id: &str,
        calendar_id: &str,
        enabled: bool,
        default_for_create: bool,
    ) -> Result<()> {
        if default_for_create {
            sqlx::query("UPDATE google_calendars SET default_for_create = 0")
                .execute(&self.pool)
                .await?;
        }
        sqlx::query("UPDATE google_calendars SET enabled = ?, default_for_create = ? WHERE account_id = ? AND calendar_id = ?")
            .bind(enabled as i64)
            .bind(default_for_create as i64)
            .bind(account_id)
            .bind(calendar_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_google_calendars(&self, only_enabled: bool) -> Result<Vec<GoogleCalendar>> {
        let rows = if only_enabled {
            sqlx::query("SELECT * FROM google_calendars WHERE enabled = 1 ORDER BY summary")
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query("SELECT * FROM google_calendars ORDER BY summary")
                .fetch_all(&self.pool)
                .await?
        };
        Ok(rows
            .iter()
            .map(|r| GoogleCalendar {
                account_id: r.get("account_id"),
                calendar_id: r.get("calendar_id"),
                summary: r.get("summary"),
                timezone: r.get("timezone"),
                access_role: r.get("access_role"),
                color: r.get("color"),
                enabled: r.get::<i64, _>("enabled") != 0,
                default_for_create: r.get::<i64, _>("default_for_create") != 0,
            })
            .collect())
    }
}
