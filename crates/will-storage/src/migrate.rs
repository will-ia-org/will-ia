//! Migrations tolerantes (`docs/11`). O `sqlx::migrate!().run()` padrão derruba
//! o serviço na subida em três situações que acontecem com bancos de máquinas de
//! usuário (instalar por cima, voltar de versão, binário de teste):
//!
//! - **checksum diferente** (`VersionMismatch`): a mesma versão foi aplicada
//!   por um binário cujo `.sql` tinha outro texto → aceitamos a versão
//!   embutida e corrigimos o checksum gravado;
//! - **coluna/tabela já existe** (`duplicate column name` em `ALTER TABLE …
//!   ADD COLUMN`): o esquema já está lá sem a linha em `_sqlx_migrations` →
//!   marcamos a migration como aplicada sem rodar (`skip`);
//! - **banco mais novo que o binário** (`VersionMissing`): versão aplicada que
//!   este binário não conhece → só aviso (downgrade não apaga nada).
//!
//! Antes de aplicar qualquer migration pendente num banco em disco, uma cópia
//! `will-ia.db.antes-de-<versão>` é feita com `VACUUM INTO`. Migration parcial
//! (`success = 0`) tem a linha removida e é reaplicada.

use crate::Result;
use sqlx::migrate::{Migrate, MigrateError, Migration};
use sqlx::sqlite::SqlitePool;
use std::collections::HashMap;
use std::path::Path;

const TABLE: &str = "_sqlx_migrations";

/// Aplica as migrations embutidas em `pool`. `db_path` = arquivo do banco
/// (`None` para `:memory:`, sem backup).
pub async fn run(pool: &SqlitePool, db_path: Option<&Path>) -> Result<()> {
    let migrator = sqlx::migrate!("./migrations");
    let mut conn = pool.acquire().await?;
    conn.ensure_migrations_table(TABLE).await?;

    if let Some(v) = conn.dirty_version(TABLE).await? {
        tracing::warn!(
            version = v,
            "migration parcial encontrada; vai ser reaplicada"
        );
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {TABLE} WHERE version = ?"
        )))
        .bind(v)
        .execute(&mut *conn)
        .await?;
    }

    let applied: HashMap<i64, Vec<u8>> = conn
        .list_applied_migrations(TABLE)
        .await?
        .into_iter()
        .map(|a| (a.version, a.checksum.into_owned()))
        .collect();

    let known: Vec<i64> = migrator.iter().map(|m| m.version).collect();
    for v in applied.keys() {
        if !known.contains(v) {
            tracing::warn!(
                version = v,
                "banco tem migration que este binário não conhece (versão mais nova); seguindo"
            );
        }
    }

    let pending: Vec<&Migration> = migrator
        .iter()
        .filter(|m| !applied.contains_key(&m.version))
        .collect();
    if let (Some(path), Some(first)) = (db_path, pending.first()) {
        backup_before(pool, path, first.version).await;
    }

    for m in migrator.iter() {
        match applied.get(&m.version) {
            Some(cs) if cs.as_slice() == m.checksum.as_ref() => {}
            Some(_) => {
                tracing::warn!(
                    version = m.version,
                    description = %m.description,
                    "migration aplicada com outro texto; aceitando a versão embutida"
                );
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {TABLE} SET checksum = ? WHERE version = ?"
                )))
                .bind(m.checksum.as_ref())
                .bind(m.version)
                .execute(&mut *conn)
                .await?;
            }
            None => match conn.apply(TABLE, m).await {
                Ok(t) => tracing::info!(
                    version = m.version,
                    description = %m.description,
                    ms = t.as_millis() as u64,
                    "migration aplicada"
                ),
                Err(MigrateError::ExecuteMigration(e, _)) if schema_already_there(&e) => {
                    tracing::warn!(
                        version = m.version,
                        description = %m.description,
                        error = %e,
                        "esquema já existia; marcando a migration como aplicada"
                    );
                    conn.skip(TABLE, m).await?;
                }
                Err(e) => return Err(e.into()),
            },
        }
    }
    Ok(())
}

/// `ALTER TABLE … ADD COLUMN` numa coluna que já existe, ou `CREATE TABLE`/
/// `CREATE INDEX` sem `IF NOT EXISTS` em algo que já está lá.
fn schema_already_there(e: &sqlx::Error) -> bool {
    let msg = e
        .as_database_error()
        .map(|d| d.message().to_ascii_lowercase())
        .unwrap_or_default();
    msg.contains("duplicate column name") || msg.contains("already exists")
}

/// Cópia do banco antes da primeira migration pendente (best-effort).
async fn backup_before(pool: &SqlitePool, db_path: &Path, version: i64) {
    let name = format!(
        "{}.antes-de-{version}",
        db_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("will-ia.db")
    );
    let dest = db_path.with_file_name(name);
    if dest.exists() {
        return;
    }
    let dest_s = dest.to_string_lossy().replace('\'', "''");
    match sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{dest_s}'")))
        .execute(pool)
        .await
    {
        Ok(_) => tracing::info!(backup = %dest.display(), "cópia do banco antes das migrations"),
        Err(e) => tracing::warn!(error = %e, "não consegui copiar o banco antes das migrations"),
    }
}

#[cfg(test)]
mod tests {
    use crate::Storage;
    use sqlx::Row;

    async fn fresh() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("will-ia.db");
        let st = Storage::open(&path).await.unwrap();
        st.pool.close().await;
        (dir, path)
    }

    async fn exec(path: &std::path::Path, sql: &str) {
        let st = Storage::open_raw(path).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
            .execute(&st.pool)
            .await
            .unwrap();
        st.pool.close().await;
    }

    #[tokio::test]
    async fn coluna_ja_existente_nao_derruba() {
        let (_d, path) = fresh().await;
        // esquema já tem `summary`, mas o registro da 0007 sumiu
        exec(&path, "DELETE FROM _sqlx_migrations WHERE version = 7").await;
        let st = Storage::open(&path)
            .await
            .expect("abre mesmo com a coluna já lá");
        let n: i64 = sqlx::query("SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 7")
            .fetch_one(&st.pool)
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn checksum_diferente_e_corrigido() {
        let (_d, path) = fresh().await;
        exec(
            &path,
            "UPDATE _sqlx_migrations SET checksum = x'00' WHERE version = 7",
        )
        .await;
        let st = Storage::open(&path)
            .await
            .expect("abre com checksum divergente");
        let cs: Vec<u8> = sqlx::query("SELECT checksum FROM _sqlx_migrations WHERE version = 7")
            .fetch_one(&st.pool)
            .await
            .unwrap()
            .get(0);
        assert_ne!(cs, vec![0u8]);
    }

    #[tokio::test]
    async fn banco_mais_novo_so_avisa() {
        let (_d, path) = fresh().await;
        exec(&path, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (999, 'futuro', 1, x'00', 0)").await;
        Storage::open(&path)
            .await
            .expect("abre banco de versão mais nova");
    }

    #[tokio::test]
    async fn faz_backup_antes_da_primeira_pendente() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("will-ia.db");
        Storage::open(&path).await.unwrap().pool.close().await;
        assert!(path.with_file_name("will-ia.db.antes-de-1").exists());
    }
}
