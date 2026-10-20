use crate::{fmt, now, parse_dt, Result, Storage};
use sqlx::Row;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogRecord {
    pub id: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub level: String,
    pub module: String,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VoiceTranscript {
    pub id: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub role: String,
    pub text: String,
}

impl Storage {
    pub async fn log_event(&self, level: &str, module: &str, message: &str) -> Result<()> {
        sqlx::query("INSERT INTO events_log (at, level, module, message) VALUES (?, ?, ?, ?)")
            .bind(fmt(now()))
            .bind(level)
            .bind(module)
            .bind(message)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_logs(
        &self,
        limit: i64,
        module: Option<&str>,
        level: Option<&str>,
    ) -> Result<Vec<LogRecord>> {
        let rows = sqlx::query(
            "SELECT * FROM events_log WHERE (? IS NULL OR module = ?) AND (? IS NULL OR level = ?) ORDER BY id DESC LIMIT ?",
        )
        .bind(module)
        .bind(module)
        .bind(level)
        .bind(level)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let at: String = r.get("at");
                LogRecord {
                    id: r.get("id"),
                    at: parse_dt(&at),
                    level: r.get("level"),
                    module: r.get("module"),
                    message: r.get("message"),
                }
            })
            .collect())
    }

    pub async fn prune_logs(&self, days: i64) -> Result<u64> {
        let cutoff = fmt(now() - chrono::Duration::days(days));
        let r = sqlx::query("DELETE FROM events_log WHERE at < ?")
            .bind(&cutoff)
            .execute(&self.pool)
            .await?;
        let _ = sqlx::query("DELETE FROM mcp_calls WHERE at < ?")
            .bind(&cutoff)
            .execute(&self.pool)
            .await?;
        let _ = sqlx::query("DELETE FROM approvals WHERE requested_at < ?")
            .bind(&cutoff)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    pub async fn add_voice_transcript(&self, role: &str, text: &str) -> Result<()> {
        sqlx::query("INSERT INTO voice_transcripts (at, role, text) VALUES (?, ?, ?)")
            .bind(fmt(now()))
            .bind(role)
            .bind(text)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_voice_transcripts(&self, limit: i64) -> Result<Vec<VoiceTranscript>> {
        let rows = sqlx::query("SELECT * FROM voice_transcripts ORDER BY id DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        let mut v: Vec<VoiceTranscript> = rows
            .iter()
            .map(|r| {
                let at: String = r.get("at");
                VoiceTranscript {
                    id: r.get("id"),
                    at: parse_dt(&at),
                    role: r.get("role"),
                    text: r.get("text"),
                }
            })
            .collect();
        v.reverse();
        Ok(v)
    }
}
