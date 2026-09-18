//! Playlist view persistence: the pure SQLite operations, decoupled from
//! the gRPC transport so they can be exercised directly by integration
//! tests (a temp DB + migrations, no tonic server).
//!
//! stationd is the single writer of this view. These functions take a
//! `&SqlitePool` and do exactly one thing each; the gRPC handlers in
//! `grpc.rs` are thin translators on top of them (business logic must not
//! live in the transport layer).
//!
//! File-first reminder: this view is a rebuildable projection of the TOML
//! files. Nothing here is authoritative.

use sqlx::SqlitePool;

use crate::playlist::Playlist;

/// One row of the playlist view, as returned by [`list`]. A named type
/// rather than a raw tuple so the metier does not depend on the proto
/// (`grpc.rs` maps this to `PlaylistSummary`).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistRow {
    pub id: String,
    /// Canonical relative-path handle. `None` for an add-only entry that has
    /// no position in the playlist tree yet.
    pub rel_path: Option<String>,
    pub name: String,
    pub enabled: bool,
    /// Selection mode, recovered from the stored TOML (not a column).
    pub mode: String,
}

/// Upsert one already-parsed playlist into the view. Keyed by `id`
/// (the UUID): re-adding the same id refreshes the row instead of failing.
/// `rel_path` is the canonical key for a file from the tree (sync), or
/// `None` for a file brought in from outside (add).
pub async fn upsert(
    pool: &SqlitePool,
    id: &str,
    playlist: &Playlist,
    rewritten: &str,
    rel_path: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO playlists (id, name, enabled, toml, rel_path) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET name = ?2, enabled = ?3, toml = ?4, rel_path = ?5",
    )
    .bind(id)
    .bind(&playlist.name)
    .bind(playlist.enabled as i64)
    .bind(rewritten)
    .bind(rel_path)
    .execute(pool)
    .await?;
    Ok(())
}

/// List the playlists in the view, ordered by the human handle (rel_path),
/// with add-only entries (no rel_path) last. `mode` is recovered from each
/// row's stored TOML; a parse failure there falls back to "?" rather than
/// failing the whole listing (we only ever store TOML we validated, so this
/// is defensive, not expected).
pub async fn list(pool: &SqlitePool) -> Result<Vec<PlaylistRow>, sqlx::Error> {
    let rows: Vec<(String, Option<String>, String, i64, String)> = sqlx::query_as(
        "SELECT id, rel_path, name, enabled, toml FROM playlists
         ORDER BY rel_path IS NULL, rel_path, name",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, rel_path, name, enabled, toml)| {
            let mode = Playlist::parse(&toml)
                .map(|p| format!("{:?}", p.selection.mode).to_lowercase())
                .unwrap_or_else(|_| "?".to_string());
            PlaylistRow {
                id,
                rel_path,
                name,
                enabled: enabled != 0,
                mode,
            }
        })
        .collect())
}

/// Fetch the raw canonical TOML of the playlist whose handle (`rel_path`)
/// matches `reference`. `None` if no such playlist. Used by the selection
/// stage to resolve a grid `playlist_ref` back to its definition (the stored
/// TOML is the source the view keeps; there is no detail-column view yet).
pub async fn playlist_toml_by_ref(
    pool: &SqlitePool,
    reference: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT toml FROM playlists WHERE rel_path = ?1")
        .bind(reference)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(t,)| t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::playlist::Playlist;

    const DYNAMIC: &str = r#"
        name = "Hits"
        [selection]
        mode = "dynamic"
    "#;

    const REMOTE: &str = r#"
        name = "goodnight"
        [selection]
        mode = "remote"
        url = "http://nightmusic.live"
    "#;

    /// A fresh temp database with all real migrations applied. This is what
    /// makes these tests catch a schema/query mismatch (e.g. a column the
    /// migration forgot, or a typo between the migration and the INSERT):
    /// they run against the actual migrated schema, not an in-memory mock.
    async fn fresh_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.db");
        let pool = db::init(&path).await.expect("init + migrations");
        (dir, pool)
    }

    #[tokio::test]
    async fn upsert_then_list_roundtrip() {
        let (_dir, pool) = fresh_db().await;
        let pl = Playlist::parse(DYNAMIC).unwrap();

        // The real point: an upsert touching `rel_path` must succeed against
        // the migrated schema. If migration 0003 were missing the column,
        // this fails loudly here — unlike the pure unit tests.
        upsert(&pool, "id-1", &pl, DYNAMIC, Some("nuit/hits"))
            .await
            .expect("upsert should succeed on a migrated db");

        let rows = list(&pool).await.expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "id-1");
        assert_eq!(rows[0].rel_path.as_deref(), Some("nuit/hits"));
        assert_eq!(rows[0].name, "Hits");
        assert_eq!(rows[0].mode, "dynamic");
        assert!(rows[0].enabled);
    }

    #[tokio::test]
    async fn upsert_is_idempotent_on_same_id() {
        let (_dir, pool) = fresh_db().await;
        let pl = Playlist::parse(DYNAMIC).unwrap();

        upsert(&pool, "id-1", &pl, DYNAMIC, Some("a")).await.unwrap();
        // Same id again, different rel_path: must UPDATE, not duplicate.
        upsert(&pool, "id-1", &pl, DYNAMIC, Some("b")).await.unwrap();

        let rows = list(&pool).await.unwrap();
        assert_eq!(rows.len(), 1, "same id must update in place, not duplicate");
        assert_eq!(rows[0].rel_path.as_deref(), Some("b"));
    }

    #[tokio::test]
    async fn rel_path_unique_constraint_rejects_collision() {
        let (_dir, pool) = fresh_db().await;
        let pl = Playlist::parse(DYNAMIC).unwrap();

        upsert(&pool, "id-1", &pl, DYNAMIC, Some("same/key")).await.unwrap();
        // A DIFFERENT id but the SAME rel_path must violate UNIQUE(rel_path).
        let res = upsert(&pool, "id-2", &pl, DYNAMIC, Some("same/key")).await;
        assert!(res.is_err(), "two rows sharing a rel_path must be rejected");
    }

    #[tokio::test]
    async fn null_rel_path_allowed_and_listed_last() {
        let (_dir, pool) = fresh_db().await;
        let dyn_pl = Playlist::parse(DYNAMIC).unwrap();
        let rem_pl = Playlist::parse(REMOTE).unwrap();

        // Two add-only entries (rel_path NULL) plus one with a path. NULL is
        // allowed (UNIQUE tolerates multiple NULLs in SQLite) and must sort
        // last per the ORDER BY.
        upsert(&pool, "id-1", &dyn_pl, DYNAMIC, None).await.unwrap();
        upsert(&pool, "id-2", &rem_pl, REMOTE, None).await.unwrap();
        upsert(&pool, "id-3", &dyn_pl, DYNAMIC, Some("aaa")).await.unwrap();

        let rows = list(&pool).await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].rel_path.as_deref(), Some("aaa"), "pathed entry first");
        assert!(rows[1].rel_path.is_none() && rows[2].rel_path.is_none());
    }

    #[tokio::test]
    async fn list_recovers_mode_from_stored_toml() {
        let (_dir, pool) = fresh_db().await;
        let rem_pl = Playlist::parse(REMOTE).unwrap();

        upsert(&pool, "id-r", &rem_pl, REMOTE, Some("n/gn")).await.unwrap();
        let rows = list(&pool).await.unwrap();
        assert_eq!(rows[0].mode, "remote", "mode is recovered from the TOML");
    }
}
