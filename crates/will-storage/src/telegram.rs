use crate::{Result, Storage};
use sqlx::Row;
use will_core::ThreadId;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TelegramStatusState {
    pub thread_key: String,
    pub thread_id: Option<ThreadId>,
    pub stack: Vec<StackEntry>,
    pub status_message_id: Option<i32>,
    pub last_user_message_id: Option<i32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StackEntry {
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i32>,
}

impl Storage {
    pub async fn telegram_offset(&self) -> Result<i64> {
        let r = sqlx::query("SELECT update_offset FROM telegram_state WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.map(|r| r.get::<i64, _>("update_offset")).unwrap_or(0))
    }

    pub async fn set_telegram_offset(&self, offset: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO telegram_state (id, update_offset) VALUES (1, ?) ON CONFLICT(id) DO UPDATE SET update_offset = excluded.update_offset",
        )
        .bind(offset)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn telegram_status(&self, key: &str) -> Result<TelegramStatusState> {
        let r = sqlx::query("SELECT * FROM telegram_status WHERE thread_key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match r {
            Some(r) => {
                let stack: String = r.get("stack_json");
                let tid: Option<String> = r.get("thread_id");
                TelegramStatusState {
                    thread_key: key.to_string(),
                    thread_id: tid.and_then(|t| ThreadId::parse(&t).ok()),
                    stack: serde_json::from_str(&stack).unwrap_or_default(),
                    status_message_id: r.get("status_message_id"),
                    last_user_message_id: r.get("last_user_message_id"),
                }
            }
            None => TelegramStatusState {
                thread_key: key.to_string(),
                ..Default::default()
            },
        })
    }

    pub async fn save_telegram_status(&self, s: &TelegramStatusState) -> Result<()> {
        sqlx::query(
            "INSERT INTO telegram_status (thread_key, thread_id, stack_json, status_message_id, last_user_message_id) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(thread_key) DO UPDATE SET thread_id = excluded.thread_id, stack_json = excluded.stack_json, status_message_id = excluded.status_message_id, last_user_message_id = excluded.last_user_message_id",
        )
        .bind(&s.thread_key)
        .bind(s.thread_id.map(|t| t.to_string()))
        .bind(serde_json::to_string(&s.stack)?)
        .bind(s.status_message_id)
        .bind(s.last_user_message_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn all_telegram_status(&self) -> Result<Vec<TelegramStatusState>> {
        let rows = sqlx::query("SELECT thread_key FROM telegram_status")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for r in rows {
            let k: String = r.get("thread_key");
            out.push(self.telegram_status(&k).await?);
        }
        Ok(out)
    }

    pub async fn delete_telegram_status(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM telegram_status WHERE thread_key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
