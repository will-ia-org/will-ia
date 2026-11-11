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
    /// Só na importação do Google Calendar: já nasce fechada e/ou vinculada.
    pub done_at: Option<DateTime<Utc>>,
    pub skipped: bool,
    pub link: Option<TaskLink>,
    /// Já normalizadas (`will_core::task::normalize_tags`).
    pub tags: Vec<String>,
}

/// Vínculo com um evento do Google Calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLink {
    pub calendar_id: String,
    pub event_id: String,
    /// Relógio local no momento da sincronização.
    pub synced_at: DateTime<Utc>,
    /// `updated` do evento no Google nesse momento.
    pub remote_updated_at: DateTime<Utc>,
}

/// Conteúdo vindo do evento do Google (aplicado inteiro, `updated_at = synced_at`).
#[derive(Debug, Clone)]
pub struct RemoteTask {
    pub title: String,
    pub description: String,
    pub start_at: DateTime<Utc>,
    pub end_at: Option<DateTime<Utc>>,
    pub done_at: Option<DateTime<Utc>>,
    pub skipped: bool,
    pub priority: TaskPriority,
    pub link: TaskLink,
}

/// Campos editáveis; `None` = não mexe. `end_at: Some(None)` limpa o fim.
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<Option<DateTime<Utc>>>,
    pub priority: Option<TaskPriority>,
    /// Substitui a lista inteira (já normalizada).
    pub tags: Option<Vec<String>>,
}

fn tags_json(tags: &[String]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".into())
}

fn parse_tags(s: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
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
        skipped: r.get::<i64, _>("skipped") != 0,
        priority: TaskPriority::from_rank(r.get::<i64, _>("priority")),
        created_by: r.get("created_by"),
        created_at: parse_dt(&r.get::<String, _>("created_at")),
        updated_at: parse_dt(&r.get::<String, _>("updated_at")),
        notified_start_at: opt_dt(r, "notified_start_at"),
        notified_overdue_at: opt_dt(r, "notified_overdue_at"),
        calendar_id: r.get::<Option<String>, _>("calendar_id"),
        event_id: r.get::<Option<String>, _>("event_id"),
        synced_at: opt_dt(r, "synced_at"),
        remote_updated_at: opt_dt(r, "remote_updated_at"),
        tags: parse_tags(&r.get::<String, _>("tags")),
    })
}

impl Storage {
    pub async fn insert_task(&self, n: &NewTask) -> Result<Task> {
        let id = TaskId::new();
        // Importada da agenda: `created_at`/`updated_at` = `synced_at`, senão a
        // tarefa pareceria "editada depois da sincronização" na rodada seguinte.
        let ts = n.link.as_ref().map(|l| l.synced_at).unwrap_or_else(now);
        // Importada já concluída: as marcas de aviso entram preenchidas (o
        // laço não avisa "entrou em andamento" de algo que já acabou).
        let marks = n.done_at.map(|_| fmt(ts));
        sqlx::query(
            "INSERT INTO tasks (id, title, description, start_at, end_at, done_at, skipped, priority, created_by, created_at, updated_at, \
             notified_start_at, notified_overdue_at, calendar_id, event_id, synced_at, remote_updated_at, tags) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(&n.title)
        .bind(&n.description)
        .bind(fmt(n.start_at))
        .bind(n.end_at.map(fmt))
        .bind(n.done_at.map(fmt))
        .bind(n.done_at.is_some() && n.skipped)
        .bind(i64::from(n.priority.rank()))
        .bind(&n.created_by)
        .bind(fmt(ts))
        .bind(fmt(ts))
        .bind(&marks)
        .bind(&marks)
        .bind(n.link.as_ref().map(|l| l.calendar_id.clone()))
        .bind(n.link.as_ref().map(|l| l.event_id.clone()))
        .bind(n.link.as_ref().map(|l| fmt(l.synced_at)))
        .bind(n.link.as_ref().map(|l| fmt(l.remote_updated_at)))
        .bind(tags_json(&n.tags))
        .execute(&self.pool)
        .await?;
        self.get_task(id).await
    }

    /// Todas as tags em uso (inclusive de tarefas fechadas), sem repetição,
    /// em ordem alfabética — para o filtro do painel.
    pub async fn list_task_tags(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT tags FROM tasks WHERE tags != '[]'")
            .fetch_all(&self.pool)
            .await?;
        let mut set = std::collections::BTreeSet::new();
        for r in rows {
            for t in parse_tags(&r.get::<String, _>("tags")) {
                set.insert(t);
            }
        }
        Ok(set.into_iter().collect())
    }

    /// Tarefa vinculada a um evento da agenda.
    pub async fn find_task_by_event(
        &self,
        calendar_id: &str,
        event_id: &str,
    ) -> Result<Option<Task>> {
        let r = sqlx::query("SELECT * FROM tasks WHERE calendar_id = ? AND event_id = ?")
            .bind(calendar_id)
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().and_then(row_to_task))
    }

    /// Grava (ou renova) o vínculo com o evento; não mexe em `updated_at`.
    pub async fn set_task_link(&self, id: TaskId, link: &TaskLink) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET calendar_id = ?, event_id = ?, synced_at = ?, remote_updated_at = ? WHERE id = ?",
        )
        .bind(&link.calendar_id)
        .bind(&link.event_id)
        .bind(fmt(link.synced_at))
        .bind(fmt(link.remote_updated_at))
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Aplica o conteúdo do evento do Google por cima da tarefa (a agenda venceu).
    /// `updated_at` e `synced_at` recebem o mesmo instante, então a tarefa não
    /// conta como "editada localmente" na próxima sincronização. Mudança de
    /// período zera as marcas de aviso; concluída, as marcas ficam preenchidas.
    pub async fn apply_remote_task(&self, id: TaskId, r: &RemoteTask) -> Result<Task> {
        let cur = self.get_task(id).await?;
        let period_changed = cur.start_at != r.start_at || cur.end_at != r.end_at;
        let ts = fmt(r.link.synced_at);
        sqlx::query(
            "UPDATE tasks SET title = ?, description = ?, start_at = ?, end_at = ?, done_at = ?, skipped = ?, priority = ?, \
             updated_at = ?, calendar_id = ?, event_id = ?, synced_at = ?, remote_updated_at = ?, \
             notified_start_at = CASE WHEN ? THEN NULL WHEN ? THEN COALESCE(notified_start_at, ?) ELSE notified_start_at END, \
             notified_overdue_at = CASE WHEN ? THEN NULL WHEN ? THEN COALESCE(notified_overdue_at, ?) ELSE notified_overdue_at END \
             WHERE id = ?",
        )
        .bind(&r.title)
        .bind(&r.description)
        .bind(fmt(r.start_at))
        .bind(r.end_at.map(fmt))
        .bind(r.done_at.map(fmt))
        .bind(r.done_at.is_some() && r.skipped)
        .bind(i64::from(r.priority.rank()))
        .bind(&ts)
        .bind(&r.link.calendar_id)
        .bind(&r.link.event_id)
        .bind(&ts)
        .bind(fmt(r.link.remote_updated_at))
        .bind(period_changed && r.done_at.is_none())
        .bind(r.done_at.is_some())
        .bind(&ts)
        .bind(period_changed && r.done_at.is_none())
        .bind(r.done_at.is_some())
        .bind(&ts)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        self.get_task(id).await
    }

    // ----- lápides (evento de tarefa apagada que ainda não saiu do Google) -----

    pub async fn add_task_tombstone(&self, calendar_id: &str, event_id: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO task_tombstones (calendar_id, event_id, deleted_at) VALUES (?, ?, ?)",
        )
        .bind(calendar_id)
        .bind(event_id)
        .bind(fmt(now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ids de evento pendentes de exclusão na agenda.
    pub async fn list_task_tombstones(&self, calendar_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT event_id FROM task_tombstones WHERE calendar_id = ? ORDER BY deleted_at",
        )
        .bind(calendar_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| r.get::<String, _>("event_id"))
            .collect())
    }

    pub async fn remove_task_tombstone(&self, calendar_id: &str, event_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM task_tombstones WHERE calendar_id = ? AND event_id = ?")
            .bind(calendar_id)
            .bind(event_id)
            .execute(&self.pool)
            .await?;
        Ok(())
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
        let tags = p.tags.clone().unwrap_or(cur.tags);
        // mudou o período → as marcas de aviso voltam a valer
        let reset_marks = p.start_at.is_some() || p.end_at.is_some();
        sqlx::query(
            "UPDATE tasks SET title = ?, description = ?, start_at = ?, end_at = ?, priority = ?, tags = ?, updated_at = ?, \
             notified_start_at = CASE WHEN ? THEN NULL ELSE notified_start_at END, \
             notified_overdue_at = CASE WHEN ? THEN NULL ELSE notified_overdue_at END \
             WHERE id = ?",
        )
        .bind(title)
        .bind(description)
        .bind(fmt(start_at))
        .bind(end_at.map(fmt))
        .bind(i64::from(priority.rank()))
        .bind(tags_json(&tags))
        .bind(fmt(now()))
        .bind(reset_marks)
        .bind(reset_marks)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        self.get_task(id).await
    }

    /// `done = true` fecha agora (concluída, ou **não realizada** com `skipped`);
    /// `false` reabre (e limpa a marca de atraso para o laço poder avisar de
    /// novo se continuar vencida).
    pub async fn set_task_done(&self, id: TaskId, done: bool, skipped: bool) -> Result<Task> {
        let ts = fmt(now());
        if done {
            sqlx::query("UPDATE tasks SET done_at = ?, skipped = ?, updated_at = ? WHERE id = ?")
                .bind(&ts)
                .bind(skipped)
                .bind(&ts)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(
                "UPDATE tasks SET done_at = NULL, skipped = 0, notified_overdue_at = NULL, updated_at = ? WHERE id = ?",
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
