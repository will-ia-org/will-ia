//! Repositório das triggers universais (`triggers`, `trigger_runs`; `docs/30`).

use crate::{fmt, now, parse_dt, Result, Storage, StorageError};
use sqlx::Row;
use will_core::trigger::TriggerFilter;
use will_core::{Origin, ThreadId, Trigger, TriggerId, TriggerRun, TriggerSource};

/// Campos de criação; `id`, datas e contadores são preenchidos aqui.
#[derive(Debug, Clone)]
pub struct NewTrigger {
    pub name: String,
    pub source: TriggerSource,
    pub enabled: bool,
    pub filter: TriggerFilter,
    pub prompt: String,
    pub thread_id: Option<ThreadId>,
    pub thread_title: Option<String>,
    pub claude_session_id: Option<String>,
    pub origin: Origin,
    pub cooldown_secs: u32,
    pub created_by: String,
}

fn opt_dt(r: &sqlx::sqlite::SqliteRow, col: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    r.get::<Option<String>, _>(col).map(|s| parse_dt(&s))
}

fn row_to_trigger(r: &sqlx::sqlite::SqliteRow) -> Option<Trigger> {
    let id: String = r.get("id");
    let source: String = r.get("source");
    let origin_kind: String = r.get("origin");
    let origin_ref: Option<String> = r.get("origin_ref");
    let filter_json: String = r.get("filter_json");
    Some(Trigger {
        id: TriggerId::parse(&id).ok()?,
        name: r.get("name"),
        source: TriggerSource::parse(&source)?,
        enabled: r.get::<i64, _>("enabled") != 0,
        filter: serde_json::from_str(&filter_json).unwrap_or_default(),
        prompt: r.get("prompt"),
        thread_id: r
            .get::<Option<String>, _>("thread_id")
            .and_then(|t| ThreadId::parse(&t).ok()),
        thread_title: r.get("thread_title"),
        claude_session_id: r.get("claude_session_id"),
        origin: Origin::from_parts(&origin_kind, origin_ref.as_deref()).unwrap_or(Origin::Ui),
        cooldown_secs: r.get::<i64, _>("cooldown_secs").max(0) as u32,
        created_by: r.get("created_by"),
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        updated_at: parse_dt(&r.get::<String, _>("updated_at")),
        last_fired_at: opt_dt(r, "last_fired_at"),
        last_result: r.get("last_result"),
        fire_count: r.get("fire_count"),
        paused_reason: r.get("paused_reason"),
        cursor: r.get("cursor"),
    })
}

impl Storage {
    pub async fn insert_trigger(&self, n: &NewTrigger) -> Result<Trigger> {
        let id = TriggerId::new();
        let ts = now();
        sqlx::query(
            "INSERT INTO triggers (id, name, source, enabled, filter_json, prompt, thread_id, thread_title, claude_session_id, origin, origin_ref, cooldown_secs, created_by, created_at, updated_at, fire_count) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(id.to_string())
        .bind(&n.name)
        .bind(n.source.as_str())
        .bind(n.enabled as i64)
        .bind(serde_json::to_string(&n.filter)?)
        .bind(&n.prompt)
        .bind(n.thread_id.map(|t| t.to_string()))
        .bind(&n.thread_title)
        .bind(&n.claude_session_id)
        .bind(n.origin.kind().as_str())
        .bind(n.origin.origin_ref())
        .bind(i64::from(n.cooldown_secs))
        .bind(&n.created_by)
        .bind(fmt(ts))
        .bind(fmt(ts))
        .execute(&self.pool)
        .await?;
        self.get_trigger(id).await
    }

    pub async fn get_trigger(&self, id: TriggerId) -> Result<Trigger> {
        let r = sqlx::query("SELECT * FROM triggers WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StorageError::NotFound)?;
        row_to_trigger(&r).ok_or(StorageError::NotFound)
    }

    pub async fn list_triggers(&self) -> Result<Vec<Trigger>> {
        let rows = sqlx::query("SELECT * FROM triggers ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().filter_map(row_to_trigger).collect())
    }

    pub async fn list_triggers_by_source(&self, source: TriggerSource) -> Result<Vec<Trigger>> {
        let rows = sqlx::query("SELECT * FROM triggers WHERE source = ? ORDER BY created_at")
            .bind(source.as_str())
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().filter_map(row_to_trigger).collect())
    }

    pub async fn count_enabled_triggers(&self) -> Result<i64> {
        let r = sqlx::query("SELECT COUNT(*) AS n FROM triggers WHERE enabled = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(r.get::<i64, _>("n"))
    }

    /// Atualiza os campos editáveis (nome, fonte, filtro, prompt, thread, cooldown,
    /// habilitada) e limpa `paused_reason` quando reabilitada.
    pub async fn update_trigger(&self, t: &Trigger) -> Result<Trigger> {
        let res = sqlx::query(
            "UPDATE triggers SET name = ?, source = ?, enabled = ?, filter_json = ?, prompt = ?, thread_id = ?, thread_title = ?, claude_session_id = ?, origin = ?, origin_ref = ?, cooldown_secs = ?, paused_reason = CASE WHEN ? THEN NULL ELSE paused_reason END, updated_at = ? WHERE id = ?",
        )
        .bind(&t.name)
        .bind(t.source.as_str())
        .bind(t.enabled as i64)
        .bind(serde_json::to_string(&t.filter)?)
        .bind(&t.prompt)
        .bind(t.thread_id.map(|x| x.to_string()))
        .bind(&t.thread_title)
        .bind(&t.claude_session_id)
        .bind(t.origin.kind().as_str())
        .bind(t.origin.origin_ref())
        .bind(i64::from(t.cooldown_secs))
        .bind(t.enabled as i64)
        .bind(fmt(now()))
        .bind(t.id.to_string())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        self.get_trigger(t.id).await
    }

    /// Thread criada/recriada pelo disparo: grava id e sessão.
    pub async fn set_trigger_thread(
        &self,
        id: TriggerId,
        thread_id: ThreadId,
        claude_session_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE triggers SET thread_id = ?, claude_session_id = COALESCE(?, claude_session_id), updated_at = ? WHERE id = ?",
        )
        .bind(thread_id.to_string())
        .bind(claude_session_id)
        .bind(fmt(now()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_trigger_cursor(&self, id: TriggerId, cursor: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE triggers SET cursor = ? WHERE id = ?")
            .bind(cursor)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_trigger_run(
        &self,
        id: TriggerId,
        thread_id: Option<ThreadId>,
        ok: bool,
        detail: Option<&str>,
        input: Option<&str>,
        paused_reason: Option<&str>,
    ) -> Result<()> {
        let ts = now();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE triggers SET last_fired_at = ?, last_result = ?, fire_count = fire_count + 1, enabled = CASE WHEN ? IS NULL THEN enabled ELSE 0 END, paused_reason = ?, updated_at = ? WHERE id = ?",
        )
        .bind(fmt(ts))
        .bind(if ok { "ok".to_string() } else { detail.unwrap_or("erro").to_string() })
        .bind(paused_reason)
        .bind(paused_reason)
        .bind(fmt(ts))
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO trigger_runs (trigger_id, at, thread_id, ok, detail, input) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(fmt(ts))
        .bind(thread_id.map(|t| t.to_string()))
        .bind(ok as i64)
        .bind(detail)
        .bind(input)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM trigger_runs WHERE trigger_id = ? AND id NOT IN (SELECT id FROM trigger_runs WHERE trigger_id = ? ORDER BY id DESC LIMIT 50)",
        )
        .bind(id.to_string())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_trigger(&self, id: TriggerId) -> Result<()> {
        let res = sqlx::query("DELETE FROM triggers WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub async fn list_trigger_runs(&self, id: TriggerId, limit: i64) -> Result<Vec<TriggerRun>> {
        let rows =
            sqlx::query("SELECT * FROM trigger_runs WHERE trigger_id = ? ORDER BY id DESC LIMIT ?")
                .bind(id.to_string())
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let tid: String = r.get("trigger_id");
                Some(TriggerRun {
                    id: r.get("id"),
                    trigger_id: TriggerId::parse(&tid).ok()?,
                    at: parse_dt(&r.get::<String, _>("at")),
                    thread_id: r
                        .get::<Option<String>, _>("thread_id")
                        .and_then(|t| ThreadId::parse(&t).ok()),
                    ok: r.get::<i64, _>("ok") != 0,
                    detail: r.get("detail"),
                    input: r.get("input"),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn crud_e_disparo() {
        let st = Storage::open_memory().await.unwrap();
        let t = st
            .insert_trigger(&NewTrigger {
                name: "Pedro".into(),
                source: TriggerSource::WhatsApp,
                enabled: true,
                filter: TriggerFilter::default(),
                prompt: "faz".into(),
                thread_id: None,
                thread_title: Some("Pedro (WhatsApp)".into()),
                claude_session_id: None,
                origin: Origin::Ui,
                cooldown_secs: 30,
                created_by: "user".into(),
            })
            .await
            .unwrap();
        assert_eq!(t.fire_count, 0);
        assert!(t.thread_id.is_none());
        let th = ThreadId::new();
        st.set_trigger_thread(t.id, th, Some("sess")).await.unwrap();
        st.record_trigger_run(t.id, Some(th), true, None, Some("oi"), None)
            .await
            .unwrap();
        let t2 = st.get_trigger(t.id).await.unwrap();
        assert_eq!(t2.fire_count, 1);
        assert_eq!(t2.thread_id, Some(th));
        assert_eq!(t2.claude_session_id.as_deref(), Some("sess"));
        assert_eq!(st.list_trigger_runs(t.id, 10).await.unwrap().len(), 1);
        assert_eq!(
            st.list_triggers_by_source(TriggerSource::Gmail)
                .await
                .unwrap()
                .len(),
            0
        );
        st.delete_trigger(t.id).await.unwrap();
        assert!(st.get_trigger(t.id).await.is_err());
    }
}
