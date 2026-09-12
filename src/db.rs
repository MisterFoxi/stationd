use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("could not create database directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not connect to SQLite database {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
    },
    #[error("migrations failed on {path}: {source}")]
    Migrate {
        path: PathBuf,
        #[source]
        source: sqlx::migrate::MigrateError,
    },
}

/// Opens (or creates) the SQLite database at the given path and applies the
/// migrations embedded in `migrations/` (read and baked into the binary at
/// compile time by `sqlx::migrate!`). Which migrations have already been
/// applied is tracked by sqlx itself (`_sqlx_migrations` table) — no
/// homegrown mechanism to maintain.
///
/// Deliberate choice: no `sqlx::query!`/`query_as!` macros (compile-time
/// checked) for now, to avoid requiring a `DATABASE_URL` env var just to
/// compile. We can switch to compile-time-checked queries the day not
/// having them becomes annoying.
pub async fn init(path: &Path) -> Result<SqlitePool, DbError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| DbError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .map_err(|source| DbError::Connect {
            path: path.to_path_buf(),
            source,
        })?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|source| DbError::Migrate {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn init_runs_migrations() {
        let dir = tempfile::tempdir().expect("temp directory");
        let db_path = dir.path().join("test.db");

        let pool = init(&db_path).await.expect("init should succeed");

        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_check'",
        )
        .fetch_one(&pool)
        .await
        .expect("query should succeed");

        assert_eq!(
            count, 1,
            "migration 0001_init should have created the schema_check table"
        );
    }

    #[tokio::test]
    async fn init_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().expect("temp directory");
        let db_path = dir.path().join("nested").join("dir").join("test.db");

        init(&db_path)
            .await
            .expect("init should create missing directories on its own");

        assert!(db_path.exists(), "the database file should have been created");
    }
}
