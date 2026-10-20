use crate::{fmt, now, parse_dt, Result, Storage, StorageError};
use sqlx::Row;
use will_core::{Origin, ThreadId, ThreadStatus, ThreadSummary, TurnId, TurnResult};

#[derive(Debug, Clone, serde::Serialize)]
pub struct TurnRecord {
    pub id: TurnId,
    pub thread_id: ThreadId,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub prompt: String,
    pub result_text: Option<String>,
    pub is_error: bool,
    pub error_kind: Option<String>,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<i64>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TurnEventRecord {
    pub id: i64,
    pub turn_id: TurnId,
    pub at: chrono::DateTime<chrono::Utc>,
    pub kind: String,
    pub payload: serde_json::Value,
}

fn row_to_thread(r: &sqlx::sqlite::SqliteRow) -> Option<ThreadSummary> {
    let id: String = r.get("id");
    let origin_kind: String = r.get("origin");
    let origin_ref: Option<String> = r.get("origin_ref");
    let origin = Origin::from_parts(&origin_kind, origin_ref.as_deref())?;
    let created: String = r.get("created_at");
    let last: String = r.get("last_activity_at");
    let status: String = r.get("status");
    Some(ThreadSummary {
        id: ThreadId::parse(&id).ok()?,
        origin,
        title: r.get("title"),
        claude_session_id: r.get("claude_session_id"),
        status: ThreadStatus::parse(&status),
        created_at: parse_dt(&created),
        last_activity_at: parse_dt(&last),
        total_cost_usd: r.get::<f64, _>("total_cost_usd"),
        alive: false,
        pending: 0,
        session_expired: false,
    })
}

impl Storage {
    pub async fn create_thread(
        &self,
        origin: &Origin,
        title: Option<&str>,
    ) -> Result<ThreadSummary> {
        let id = ThreadId::new();
        let ts = now();
        sqlx::query(
            "INSERT INTO threads (id, origin, origin_ref, title, claude_session_id, status, created_at, last_activity_at, total_cost_usd) \
             VALUES (?, ?, ?, ?, NULL, 'idle', ?, ?, 0)",
        )
        .bind(id.to_string())
        .bind(origin.kind().as_str())
        .bind(origin.origin_ref())
        .bind(title)
        .bind(fmt(ts))
        .bind(fmt(ts))
        .execute(&self.pool)
        .await?;
        Ok(ThreadSummary {
            id,
            origin: origin.clone(),
            title: title.map(|s| s.to_string()),
            claude_session_id: None,
            status: ThreadStatus::Idle,
            created_at: ts,
            last_activity_at: ts,
            total_cost_usd: 0.0,
            alive: false,
            pending: 0,
            session_expired: false,
        })
    }

    pub async fn get_thread(&self, id: ThreadId) -> Result<ThreadSummary> {
        let r = sqlx::query("SELECT * FROM threads WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StorageError::NotFound)?;
        row_to_thread(&r).ok_or(StorageError::NotFound)
    }

    pub async fn find_thread_by_origin(&self, origin: &Origin) -> Result<Option<ThreadSummary>> {
        let r = sqlx::query(
            "SELECT * FROM threads WHERE origin = ? AND origin_ref IS ? AND status != 'archived' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(origin.kind().as_str())
        .bind(origin.origin_ref())
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.and_then(|r| row_to_thread(&r)))
    }

    pub async fn list_threads(&self, include_archived: bool) -> Result<Vec<ThreadSummary>> {
        let rows = if include_archived {
            sqlx::query("SELECT * FROM threads ORDER BY last_activity_at DESC")
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query(
                "SELECT * FROM threads WHERE status != 'archived' ORDER BY last_activity_at DESC",
            )
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows.iter().filter_map(row_to_thread).collect())
    }

    pub async fn set_thread_session(&self, id: ThreadId, session_id: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE threads SET claude_session_id = ? WHERE id = ?")
            .bind(session_id)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_thread_status(&self, id: ThreadId, status: ThreadStatus) -> Result<()> {
        sqlx::query("UPDATE threads SET status = ?, last_activity_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(fmt(now()))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn touch_thread(&self, id: ThreadId) -> Result<()> {
        sqlx::query("UPDATE threads SET last_activity_at = ? WHERE id = ?")
            .bind(fmt(now()))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn rename_thread(&self, id: ThreadId, title: &str) -> Result<()> {
        sqlx::query("UPDATE threads SET title = ? WHERE id = ?")
            .bind(title)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn add_thread_cost(&self, id: ThreadId, cost: f64) -> Result<()> {
        sqlx::query("UPDATE threads SET total_cost_usd = total_cost_usd + ? WHERE id = ?")
            .bind(cost)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_thread(&self, id: ThreadId) -> Result<()> {
        sqlx::query("DELETE FROM threads WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Ao subir o serviço nenhuma thread está rodando.
    pub async fn reset_running_threads(&self) -> Result<()> {
        sqlx::query("UPDATE threads SET status = 'idle' WHERE status IN ('running', 'live')")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ----- turns -----

    pub async fn start_turn(&self, thread_id: ThreadId, prompt: &str) -> Result<TurnId> {
        let id = TurnId::new();
        sqlx::query("INSERT INTO turns (id, thread_id, started_at, prompt) VALUES (?, ?, ?, ?)")
            .bind(id.to_string())
            .bind(thread_id.to_string())
            .bind(fmt(now()))
            .bind(prompt)
            .execute(&self.pool)
            .await?;
        Ok(id)
    }

    pub async fn finish_turn(
        &self,
        id: TurnId,
        result: &TurnResult,
        error_kind: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE turns SET finished_at = ?, result_text = ?, is_error = ?, error_kind = ?, cost_usd = ?, duration_ms = ?, model = ?, usage_json = ? WHERE id = ?",
        )
        .bind(fmt(now()))
        .bind(&result.text)
        .bind(result.is_error as i64)
        .bind(error_kind)
        .bind(result.cost_usd)
        .bind(result.duration_ms.map(|d| d as i64))
        .bind(&result.model)
        .bind(result.usage.as_ref().map(|u| u.to_string()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn add_turn_event(
        &self,
        turn_id: TurnId,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO turn_events (turn_id, at, kind, payload) VALUES (?, ?, ?, ?)")
            .bind(turn_id.to_string())
            .bind(fmt(now()))
            .bind(kind)
            .bind(payload.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_turns(&self, thread_id: ThreadId, limit: i64) -> Result<Vec<TurnRecord>> {
        let rows =
            sqlx::query("SELECT * FROM turns WHERE thread_id = ? ORDER BY started_at DESC LIMIT ?")
                .bind(thread_id.to_string())
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::new();
        for r in rows {
            let id: String = r.get("id");
            let started: String = r.get("started_at");
            let finished: Option<String> = r.get("finished_at");
            out.push(TurnRecord {
                id: TurnId::parse(&id).unwrap_or_default(),
                thread_id,
                started_at: parse_dt(&started),
                finished_at: finished.as_deref().map(parse_dt),
                prompt: r.get("prompt"),
                result_text: r.get("result_text"),
                is_error: r.get::<i64, _>("is_error") != 0,
                error_kind: r.get("error_kind"),
                cost_usd: r.get("cost_usd"),
                duration_ms: r.get("duration_ms"),
                model: r.get("model"),
            });
        }
        out.reverse();
        Ok(out)
    }

    pub async fn list_turn_events(&self, turn_id: TurnId) -> Result<Vec<TurnEventRecord>> {
        let rows = sqlx::query("SELECT * FROM turn_events WHERE turn_id = ? ORDER BY id")
            .bind(turn_id.to_string())
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let at: String = r.get("at");
                let payload: String = r.get("payload");
                TurnEventRecord {
                    id: r.get("id"),
                    turn_id,
                    at: parse_dt(&at),
                    kind: r.get("kind"),
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                }
            })
            .collect())
    }

    pub async fn prune_turn_events(&self, older_than_days: i64) -> Result<u64> {
        let cutoff = fmt(now() - chrono::Duration::days(older_than_days));
        let r = sqlx::query("DELETE FROM turn_events WHERE at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}
