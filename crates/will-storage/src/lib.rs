//! will-storage — SQLite (sqlx) + segredos (keyring do SO com fallback em arquivo).

pub mod costs;
pub mod google;
pub mod logs;
pub mod mcp;
pub mod migrate;
pub mod schedules;
pub mod secrets;
pub mod settings;
pub mod telegram;
pub mod threads;
pub mod users;
pub mod whatsapp;

pub use secrets::{AutoSecretStore, SecretStore};
pub use sqlx::SqlitePool;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("banco de dados: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migração: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("serialização: {0}")]
    Json(#[from] serde_json::Error),
    #[error("segredo: {0}")]
    Secret(String),
    #[error("não encontrado")]
    NotFound,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Clone)]
pub struct Storage {
    pub pool: SqlitePool,
}

impl Storage {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        migrate::run(&pool, Some(path)).await?;
        Ok(Self { pool })
    }

    /// Abre o arquivo **sem** rodar migrations (diagnóstico e testes).
    pub async fn open_raw(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(opts)
            .await?;
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(":memory:")
            .foreign_keys(true)
            .in_memory(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        migrate::run(&pool, None).await?;
        Ok(Self { pool })
    }

    /// Cópia consistente do banco para backup (VACUUM INTO).
    pub async fn backup_to(&self, dest: &Path) -> Result<()> {
        let dest_s = dest.to_string_lossy().replace('\'', "''");
        sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{dest_s}'")))
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

pub(crate) fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

pub(crate) fn fmt(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(crate) fn parse_dt(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}
