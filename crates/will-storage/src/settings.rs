use crate::{fmt, now, Result, Storage};
use sqlx::Row;
use will_core::settings::SCHEMA_VERSION;
use will_core::Settings;

impl Storage {
    pub async fn load_settings(&self) -> Result<Settings> {
        let row = sqlx::query("SELECT json FROM settings WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let json: String = r.get("json");
                let s: Settings = serde_json::from_str(&json)?;
                Ok(s.migrate())
            }
            None => {
                let s = Settings::fresh();
                self.save_settings(&s).await?;
                Ok(s)
            }
        }
    }

    pub async fn save_settings(&self, s: &Settings) -> Result<()> {
        let json = serde_json::to_string(s)?;
        sqlx::query(
            "INSERT INTO settings (id, schema_version, json, updated_at) VALUES (1, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET schema_version = excluded.schema_version, json = excluded.json, updated_at = excluded.updated_at",
        )
        .bind(SCHEMA_VERSION as i64)
        .bind(json)
        .bind(fmt(now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
