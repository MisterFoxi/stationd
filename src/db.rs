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
    #[error("could not copy {path} into memory: {source}")]
    Copy {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
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

/// A throwaway in-memory copy of the station database at `src`: same schema
/// (the same embedded migrations), same rows, read in ONE transaction (a
/// consistent snapshot; in WAL the live writer is never blocked). Nothing
/// written to the copy ever reaches `src`: it is what the on-air simulation
/// runs the real engine on (`onair_sim`).
///
/// Single connection, never recycled — an in-memory database lives and dies
/// with its connection. Foreign keys are off during the bulk copy (tables are
/// filled in creation order, rows as they are) and back on afterwards.
pub async fn memory_copy(src: &Path) -> Result<SqlitePool, DbError> {
    let copy_err = |source| DbError::Copy { path: src.to_path_buf(), source };
    // The special name `:memory:`, NOT sqlx's `sqlite::memory:` URL: the
    // latter opens with SQLITE_OPEN_MEMORY, a flag SQLite then applies to
    // ATTACHed databases too — the live file would attach as an empty
    // in-memory database.
    let options = SqliteConnectOptions::new().filename(":memory:");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(options)
        .await
        .map_err(copy_err)?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|source| DbError::Migrate { path: PathBuf::from(":memory:"), source })?;

    let mut conn = pool.acquire().await.map_err(copy_err)?;
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM main.sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'
         ORDER BY rowid",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(copy_err)?;
    sqlx::query("PRAGMA foreign_keys = OFF").execute(&mut *conn).await.map_err(copy_err)?;
    sqlx::query("ATTACH DATABASE ?1 AS src")
        .bind(src.to_string_lossy().into_owned())
        .execute(&mut *conn)
        .await
        .map_err(copy_err)?;
    let copied: Result<(), sqlx::Error> = async {
        sqlx::query("BEGIN").execute(&mut *conn).await?;
        for (t,) in &tables {
            let t = t.replace('"', "\"\"");
            sqlx::query(&format!("INSERT INTO main.\"{t}\" SELECT * FROM src.\"{t}\""))
                .execute(&mut *conn)
                .await?;
        }
        sqlx::query("COMMIT").execute(&mut *conn).await?;
        Ok(())
    }
    .await;
    if copied.is_err() {
        let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
    }
    let _ = sqlx::query("DETACH DATABASE src").execute(&mut *conn).await;
    copied.map_err(copy_err)?;
    sqlx::query("PRAGMA foreign_keys = ON").execute(&mut *conn).await.map_err(copy_err)?;
    drop(conn);
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

    #[tokio::test]
    async fn memory_copy_is_a_detached_snapshot() {
        let dir = tempfile::tempdir().expect("temp directory");
        let db_path = dir.path().join("live.db");
        let live = init(&db_path).await.unwrap();
        sqlx::query("INSERT INTO broadcast_log (rel_path, artist, played_at) VALUES ('a.mp3', NULL, 10)")
            .execute(&live)
            .await
            .unwrap();

        let copy = memory_copy(&db_path).await.expect("copy");
        let n: (i64,) = sqlx::query_as("SELECT count(*) FROM broadcast_log").fetch_one(&copy).await.unwrap();
        assert_eq!(n.0, 1, "rows are copied");

        // Writing the copy never reaches the live database, and vice versa.
        sqlx::query("INSERT INTO broadcast_log (rel_path, artist, played_at) VALUES ('b.mp3', NULL, 20)")
            .execute(&copy)
            .await
            .unwrap();
        sqlx::query("INSERT INTO broadcast_log (rel_path, artist, played_at) VALUES ('c.mp3', NULL, 30)")
            .execute(&live)
            .await
            .unwrap();
        let live_rows: Vec<(String,)> = sqlx::query_as("SELECT rel_path FROM broadcast_log ORDER BY id")
            .fetch_all(&live)
            .await
            .unwrap();
        let copy_rows: Vec<(String,)> = sqlx::query_as("SELECT rel_path FROM broadcast_log ORDER BY id")
            .fetch_all(&copy)
            .await
            .unwrap();
        assert_eq!(live_rows, vec![("a.mp3".into(),), ("c.mp3".into(),)]);
        assert_eq!(copy_rows, vec![("a.mp3".into(),), ("b.mp3".into(),)]);
    }
}
