//! Repositório de agendamentos (`schedules`, `schedule_runs`).

use crate::{fmt, now, parse_dt, Result, Storage, StorageError};
use sqlx::Row;
use will_core::{Origin, Schedule, ScheduleId, ScheduleRun, ScheduleTrigger, ThreadId};

/// Campos de criação; `id`, datas e contadores são preenchidos aqui.
#[derive(Debug, Clone)]
pub struct NewSchedule {
    pub name: String,
    pub message: String,
    pub trigger: ScheduleTrigger,
    pub timezone: Option<String>,
    pub thread_id: ThreadId,
    pub claude_session_id: Option<String>,
    pub origin: Origin,
    pub enabled: bool,
    pub created_by: String,
    pub next_run_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn opt_dt(r: &sqlx::sqlite::SqliteRow, col: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    r.get::<Option<String>, _>(col).map(|s| parse_dt(&s))
}

fn row_to_schedule(r: &sqlx::sqlite::SqliteRow) -> Option<Schedule> {
    let id: String = r.get("id");
    let kind: String = r.get("trigger_kind");
    let trigger = match kind.as_str() {
        "cron" => ScheduleTrigger::Cron {
            expr: r.get::<Option<String>, _>("cron_expr").unwrap_or_default(),
        },
        "once" => ScheduleTrigger::Once {
            at: opt_dt(r, "run_at")?,
        },
        _ => return None,
    };
    let origin_kind: String = r.get("origin");
    let origin_ref: Option<String> = r.get("origin_ref");
    let thread_id: String = r.get("thread_id");
    Some(Schedule {
        id: ScheduleId::parse(&id).ok()?,
        name: r.get("name"),
        message: r.get("message"),
        trigger,
        timezone: r.get("timezone"),
        thread_id: ThreadId::parse(&thread_id).ok()?,
        claude_session_id: r.get("claude_session_id"),
        origin: Origin::from_parts(&origin_kind, origin_ref.as_deref()).unwrap_or(Origin::Ui),
        enabled: r.get::<i64, _>("enabled") != 0,
        created_by: r.get("created_by"),
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        updated_at: parse_dt(&r.get::<String, _>("updated_at")),
        next_run_at: opt_dt(r, "next_run_at"),
        last_run_at: opt_dt(r, "last_run_at"),
        last_result: r.get("last_result"),
        run_count: r.get("run_count"),
        paused_reason: r.get("paused_reason"),
    })
}

fn trigger_cols(t: &ScheduleTrigger) -> (&'static str, Option<String>, Option<String>) {
    match t {
        ScheduleTrigger::Cron { expr } => ("cron", Some(expr.clone()), None),
        ScheduleTrigger::Once { at } => ("once", None, Some(fmt(*at))),
    }
}

impl Storage {
    pub async fn insert_schedule(&self, n: &NewSchedule) -> Result<Schedule> {
        let id = ScheduleId::new();
        let ts = now();
        let (kind, cron_expr, run_at) = trigger_cols(&n.trigger);
        sqlx::query(
            "INSERT INTO schedules (id, name, message, trigger_kind, cron_expr, run_at, timezone, thread_id, claude_session_id, origin, origin_ref, enabled, created_by, created_at, updated_at, next_run_at, run_count) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(id.to_string())
        .bind(&n.name)
        .bind(&n.message)
        .bind(kind)
        .bind(cron_expr)
        .bind(run_at)
        .bind(&n.timezone)
        .bind(n.thread_id.to_string())
        .bind(&n.claude_session_id)
        .bind(n.origin.kind().as_str())
        .bind(n.origin.origin_ref())
        .bind(n.enabled as i64)
        .bind(&n.created_by)
        .bind(fmt(ts))
        .bind(fmt(ts))
        .bind(n.next_run_at.map(fmt))
        .execute(&self.pool)
        .await?;
        self.get_schedule(id).await
    }

    pub async fn get_schedule(&self, id: ScheduleId) -> Result<Schedule> {
        let r = sqlx::query("SELECT * FROM schedules WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StorageError::NotFound)?;
        row_to_schedule(&r).ok_or(StorageError::NotFound)
    }

    /// Todos, ordenados pelo próximo disparo (nulos por último) e depois por nome.
    pub async fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let rows = sqlx::query(
            "SELECT * FROM schedules ORDER BY enabled DESC, next_run_at IS NULL, next_run_at ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().filter_map(row_to_schedule).collect())
    }

    pub async fn list_schedules_for_thread(&self, thread: ThreadId) -> Result<Vec<Schedule>> {
        let rows = sqlx::query(
            "SELECT * FROM schedules WHERE thread_id = ? ORDER BY next_run_at IS NULL, next_run_at ASC",
        )
        .bind(thread.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().filter_map(row_to_schedule).collect())
    }

    pub async fn count_enabled_schedules(&self) -> Result<i64> {
        let r = sqlx::query("SELECT COUNT(*) AS n FROM schedules WHERE enabled = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(r.get("n"))
    }

    /// Grava os campos editáveis + `next_run_at`/`paused_reason` (já recalculados pelo chamador).
    pub async fn update_schedule(&self, s: &Schedule) -> Result<Schedule> {
        let (kind, cron_expr, run_at) = trigger_cols(&s.trigger);
        let res = sqlx::query(
            "UPDATE schedules SET name = ?, message = ?, trigger_kind = ?, cron_expr = ?, run_at = ?, timezone = ?, thread_id = ?, claude_session_id = ?, origin = ?, origin_ref = ?, enabled = ?, updated_at = ?, next_run_at = ?, paused_reason = ? WHERE id = ?",
        )
        .bind(&s.name)
        .bind(&s.message)
        .bind(kind)
        .bind(cron_expr)
        .bind(run_at)
        .bind(&s.timezone)
        .bind(s.thread_id.to_string())
        .bind(&s.claude_session_id)
        .bind(s.origin.kind().as_str())
        .bind(s.origin.origin_ref())
        .bind(s.enabled as i64)
        .bind(fmt(now()))
        .bind(s.next_run_at.map(fmt))
        .bind(&s.paused_reason)
        .bind(s.id.to_string())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        self.get_schedule(s.id).await
    }

    /// Registra um disparo: atualiza último resultado/contador, grava o run e o
    /// próximo horário (ou pausa, quando `next_run_at` é `None` e há motivo).
    pub async fn record_schedule_run(
        &self,
        id: ScheduleId,
        thread_id: Option<ThreadId>,
        ok: bool,
        detail: Option<&str>,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
        paused_reason: Option<&str>,
    ) -> Result<()> {
        let ts = now();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE schedules SET last_run_at = ?, last_result = ?, run_count = run_count + 1, next_run_at = ?, thread_id = COALESCE(?, thread_id), enabled = CASE WHEN ? IS NULL THEN enabled ELSE 0 END, paused_reason = ?, updated_at = ? WHERE id = ?",
        )
        .bind(fmt(ts))
        .bind(if ok { "ok".to_string() } else { detail.unwrap_or("erro").to_string() })
        .bind(next_run_at.map(fmt))
        .bind(thread_id.map(|t| t.to_string()))
        .bind(paused_reason)
        .bind(paused_reason)
        .bind(fmt(ts))
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO schedule_runs (schedule_id, at, thread_id, ok, detail) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(fmt(ts))
        .bind(thread_id.map(|t| t.to_string()))
        .bind(ok as i64)
        .bind(detail)
        .execute(&mut *tx)
        .await?;
        // mantém só os últimos 50 por agendamento
        sqlx::query(
            "DELETE FROM schedule_runs WHERE schedule_id = ? AND id NOT IN (SELECT id FROM schedule_runs WHERE schedule_id = ? ORDER BY id DESC LIMIT 50)",
        )
        .bind(id.to_string())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Só recalcula `next_run_at`/pausa (ex.: disparo perdido na partida).
    pub async fn set_schedule_next(
        &self,
        id: ScheduleId,
        next_run_at: Option<chrono::DateTime<chrono::Utc>>,
        paused_reason: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE schedules SET next_run_at = ?, enabled = CASE WHEN ? IS NULL THEN enabled ELSE 0 END, paused_reason = ?, updated_at = ? WHERE id = ?",
        )
        .bind(next_run_at.map(fmt))
        .bind(paused_reason)
        .bind(paused_reason)
        .bind(fmt(now()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_schedule(&self, id: ScheduleId) -> Result<()> {
        let res = sqlx::query("DELETE FROM schedules WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub async fn list_schedule_runs(&self, id: ScheduleId, limit: i64) -> Result<Vec<ScheduleRun>> {
        let rows = sqlx::query(
            "SELECT * FROM schedule_runs WHERE schedule_id = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let sid: String = r.get("schedule_id");
                Some(ScheduleRun {
                    id: r.get("id"),
                    schedule_id: ScheduleId::parse(&sid).ok()?,
                    at: parse_dt(&r.get::<String, _>("at")),
                    thread_id: r
                        .get::<Option<String>, _>("thread_id")
                        .and_then(|t| ThreadId::parse(&t).ok()),
                    ok: r.get::<i64, _>("ok") != 0,
                    detail: r.get("detail"),
                })
            })
            .collect())
    }
}
