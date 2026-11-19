//! Fichas das gravações de conversa (`recordings`). O áudio e a transcrição
//! ficam em disco, em `<dados>/recordings/<id>/`.

use crate::{fmt, now, parse_dt, Result, Storage, StorageError};
use sqlx::Row;

/// Estado de uma gravação.
pub mod status {
    pub const SAVED: &str = "saved";
    pub const TRANSCRIBING: &str = "transcribing";
    pub const DONE: &str = "done";
    pub const ERROR: &str = "error";
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Recording {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub duration_secs: f64,
    /// Com quem foi a conversa: rótulo das falas do canal do sistema.
    pub with_whom: String,
    pub subject: String,
    pub has_system: bool,
    pub size_bytes: i64,
    pub status: String,
    pub error: Option<String>,
    pub created_by: Option<String>,
    /// Nome da pasta em `<dados>/recordings/` (`None` = a pasta é o próprio id).
    pub folder: Option<String>,
    /// Resumo detalhado da conversa.
    pub summary: Option<String>,
}

/// Trecho indexado de uma gravação.
#[derive(Debug, Clone)]
pub struct RecordingChunk {
    pub recording_id: String,
    /// `summary` ou `speech`.
    pub kind: String,
    pub idx: i64,
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
    pub embedding: Vec<f32>,
}

fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_vector(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

#[derive(Debug, Clone)]
pub struct NewRecording<'a> {
    pub id: &'a str,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub duration_secs: f64,
    pub with_whom: &'a str,
    pub subject: &'a str,
    pub has_system: bool,
    pub size_bytes: i64,
    pub created_by: Option<&'a str>,
    pub folder: &'a str,
}

fn row_to_recording(r: &sqlx::sqlite::SqliteRow) -> Recording {
    let created: String = r.get("created_at");
    let started: String = r.get("started_at");
    Recording {
        id: r.get("id"),
        created_at: parse_dt(&created),
        started_at: parse_dt(&started),
        duration_secs: r.get("duration_secs"),
        with_whom: r.get("with_whom"),
        subject: r.get("subject"),
        has_system: r.get::<i64, _>("has_system") != 0,
        size_bytes: r.get("size_bytes"),
        status: r.get("status"),
        error: r.get("error"),
        created_by: r.get("created_by"),
        folder: r.try_get::<Option<String>, _>("folder").ok().flatten(),
        summary: r
            .try_get::<Option<String>, _>("summary")
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty()),
    }
}

impl Storage {
    pub async fn insert_recording(&self, n: &NewRecording<'_>) -> Result<Recording> {
        sqlx::query(
            "INSERT INTO recordings (id, created_at, started_at, duration_secs, with_whom, subject, has_system, size_bytes, status, created_by, folder) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(n.id)
        .bind(fmt(now()))
        .bind(fmt(n.started_at))
        .bind(n.duration_secs)
        .bind(n.with_whom)
        .bind(n.subject)
        .bind(n.has_system as i64)
        .bind(n.size_bytes)
        .bind(status::SAVED)
        .bind(n.created_by)
        .bind(n.folder)
        .execute(&self.pool)
        .await?;
        self.get_recording(n.id).await
    }

    pub async fn get_recording(&self, id: &str) -> Result<Recording> {
        let r = sqlx::query("SELECT * FROM recordings WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StorageError::NotFound)?;
        Ok(row_to_recording(&r))
    }

    /// Mais recentes primeiro.
    pub async fn list_recordings(&self, limit: i64) -> Result<Vec<Recording>> {
        let rows = sqlx::query("SELECT * FROM recordings ORDER BY started_at DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(row_to_recording).collect())
    }

    /// Com quem / assunto, informados (ou corrigidos) depois de gravar.
    pub async fn set_recording_labels(
        &self,
        id: &str,
        with_whom: &str,
        subject: &str,
    ) -> Result<()> {
        let done = sqlx::query("UPDATE recordings SET with_whom = ?, subject = ? WHERE id = ?")
            .bind(with_whom)
            .bind(subject)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub async fn set_recording_status(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE recordings SET status = ?, error = ? WHERE id = ?")
            .bind(status)
            .bind(error)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Gravações que ficaram "transcrevendo" quando o serviço caiu: voltam a
    /// "salva" para poderem ser transcritas de novo.
    pub async fn reset_stuck_recordings(&self) -> Result<u64> {
        let done = sqlx::query("UPDATE recordings SET status = ? WHERE status = ?")
            .bind(status::SAVED)
            .bind(status::TRANSCRIBING)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    pub async fn set_recording_summary(&self, id: &str, summary: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE recordings SET summary = ? WHERE id = ?")
            .bind(summary)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Troca o índice de busca de uma gravação (apaga o anterior e grava o novo).
    pub async fn replace_recording_chunks(
        &self,
        id: &str,
        model: &str,
        chunks: &[RecordingChunk],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM recording_chunks WHERE recording_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for c in chunks {
            sqlx::query(
                "INSERT INTO recording_chunks (recording_id, kind, idx, start_secs, end_secs, text, model, embedding) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(&c.kind)
            .bind(c.idx)
            .bind(c.start_secs)
            .bind(c.end_secs)
            .bind(&c.text)
            .bind(model)
            .bind(vector_to_blob(&c.embedding))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Todos os trechos indexados com o modelo `model` (a busca compara em memória).
    pub async fn recording_chunks(&self, model: &str) -> Result<Vec<RecordingChunk>> {
        let rows = sqlx::query(
            "SELECT recording_id, kind, idx, start_secs, end_secs, text, embedding \
             FROM recording_chunks WHERE model = ?",
        )
        .bind(model)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| RecordingChunk {
                recording_id: r.get("recording_id"),
                kind: r.get("kind"),
                idx: r.get("idx"),
                start_secs: r.get("start_secs"),
                end_secs: r.get("end_secs"),
                text: r.get("text"),
                embedding: blob_to_vector(&r.get::<Vec<u8>, _>("embedding")),
            })
            .collect())
    }

    /// Quantos trechos indexados cada gravação tem.
    pub async fn recording_chunk_count(&self, id: &str) -> Result<i64> {
        let r = sqlx::query("SELECT COUNT(*) AS n FROM recording_chunks WHERE recording_id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(r.get("n"))
    }

    pub async fn delete_recording(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM recording_chunks WHERE recording_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        let done = sqlx::query("DELETE FROM recordings WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }
}
