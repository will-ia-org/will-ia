//! Repositório de tarefas (`tasks`, `docs/31`).

use crate::{fmt, now, parse_dt, Result, Storage, StorageError};
use chrono::{DateTime, Utc};
use sqlx::Row;
use will_core::{Task, TaskId, TaskPriority};

#[derive(Debug, Clone)]
pub struct NewTask {
    pub title: String,
    pub description: String,
    pub start_at: DateTime<Utc>,
    pub end_at: Option<DateTime<Utc>>,
    pub priority: TaskPriority,
    pub created_by: String,
}

/// Campos editáveis; `None` = não mexe. `end_at: Some(None)` limpa o fim.
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<Option<DateTime<Utc>>>,
    pub priority: Option<TaskPriority>,
}

fn opt_dt(r: &sqlx::sqlite::SqliteRow, col: &str) -> Option<DateTime<Utc>> {
    r.get::<Option<String>, _>(col).map(|s| parse_dt(&s))
}

fn row_to_task(r: &sqlx::sqlite::SqliteRow) -> Option<Task> {
    let id: String = r.get("id");
    Some(Task {
        id: TaskId::parse(&id).ok()?,
        title: r.get("title"),
        description: r.get("description"),
        start_at: parse_dt(&r.get::<String, _>("start_at")),
        end_at: opt_dt(r, "end_at"),
        done_at: opt_dt(r, "done_at"),
        priority: TaskPriority::from_rank(r.get::<i64, _>("priority")),
        created_by: r.get("created_by"),
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        updated_at: parse_dt(&r.get::<String, _>("updated_at")),
        notified_start_at: opt_dt(r, "notified_start_at"),
        notified_overdue_at: opt_dt(r, "notified_overdue_at"),
    })
}

impl Storage {
    pub async fn insert_task(&self, n: &NewTask) -> Result<Task> {
        let id = TaskId::new();
        let ts = now();
        sqlx::query(
            "INSERT INTO tasks (id, title, description, start_at, end_at, priority, created_by, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(&n.title)
        .bind(&n.description)
        .bind(fmt(n.start_at))
        .bind(n.end_at.map(fmt))
        .bind(i64::from(n.priority.rank()))
        .bind(&n.created_by)
        .bind(fmt(ts))
        .bind(fmt(ts))
        .execute(&self.pool)
        .await?;
        self.get_task(id).await
    }

    pub async fn get_task(&self, id: TaskId) -> Result<Task> {
        let r = sqlx::query("SELECT * FROM tasks WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StorageError::NotFound)?;
        row_to_task(&r).ok_or(StorageError::NotFound)
    }

    /// Todas (ou só pendentes), por início.
    pub async fn list_tasks(&self, include_done: bool) -> Result<Vec<Task>> {
        let rows = if include_done {
            sqlx::query("SELECT * FROM tasks ORDER BY start_at, created_at")
                .fetch_all(&self.pool)
                .await?
        } else {
            sqlx::query("SELECT * FROM tasks WHERE done_at IS NULL ORDER BY start_at, created_at")
                .fetch_all(&self.pool)
                .await?
        };
        Ok(rows.iter().filter_map(row_to_task).collect())
    }

    /// Id completo ou prefixo (≥ 4 caracteres); erro se ambíguo.
    pub async fn resolve_task_id(&self, s: &str) -> Result<Option<TaskId>> {
        if let Ok(id) = TaskId::parse(s) {
            return Ok(Some(id));
        }
        let s = s.trim().to_lowercase();
        if s.len() < 4 {
            return Ok(None);
        }
        let rows = sqlx::query("SELECT id FROM tasks WHERE id LIKE ? LIMIT 2")
            .bind(format!("{s}%"))
            .fetch_all(&self.pool)
            .await?;
        if rows.len() != 1 {
            return Ok(None);
        }
        let id: String = rows[0].get("id");
        Ok(TaskId::parse(&id).ok())
    }

    pub async fn update_task(&self, id: TaskId, p: &TaskPatch) -> Result<Task> {
        let cur = self.get_task(id).await?;
        let title = p.title.clone().unwrap_or(cur.title);
        let description = p.description.clone().unwrap_or(cur.description);
        let start_at = p.start_at.unwrap_or(cur.start_at);
        let end_at = match p.end_at {
            Some(e) => e,
            None => cur.end_at,
        };
        let priority = p.priority.unwrap_or(cur.priority);
        // mudou o período → as marcas de aviso voltam a valer
        let reset_marks = p.start_at.is_some() || p.end_at.is_some();
        sqlx::query(
            "UPDATE tasks SET title = ?, description = ?, start_at = ?, end_at = ?, priority = ?, updated_at = ?, \
             notified_start_at = CASE WHEN ? THEN NULL ELSE notified_start_at END, \
             notified_overdue_at = CASE WHEN ? THEN NULL ELSE notified_overdue_at END \
             WHERE id = ?",
        )
        .bind(title)
        .bind(description)
        .bind(fmt(start_at))
        .bind(end_at.map(fmt))
        .bind(i64::from(priority.rank()))
        .bind(fmt(now()))
        .bind(reset_marks)
        .bind(reset_marks)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        self.get_task(id).await
    }

    /// `done = true` conclui agora; `false` reabre (e limpa a marca de atraso
    /// para o laço poder avisar de novo se continuar vencida).
    pub async fn set_task_done(&self, id: TaskId, done: bool) -> Result<Task> {
        let ts = fmt(now());
        if done {
            sqlx::query("UPDATE tasks SET done_at = ?, updated_at = ? WHERE id = ?")
                .bind(&ts)
                .bind(&ts)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(
                "UPDATE tasks SET done_at = NULL, notified_overdue_at = NULL, updated_at = ? WHERE id = ?",
            )
            .bind(&ts)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        }
        self.get_task(id).await
    }

    pub async fn delete_task(&self, id: TaskId) -> Result<()> {
        let n = sqlx::query("DELETE FROM tasks WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    /// Marca que o laço viu a tarefa entrar em andamento / em atraso.
    pub async fn mark_task_notified(&self, id: TaskId, overdue: bool) -> Result<()> {
        let q = if overdue {
            sqlx::query("UPDATE tasks SET notified_overdue_at = ? WHERE id = ?")
        } else {
            sqlx::query("UPDATE tasks SET notified_start_at = ? WHERE id = ?")
        };
        q.bind(fmt(now()))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
