//! Integration test for `sync` reconciliation, driven end-to-end through the
//! library's public surface: a temp SQLite DB + a `tempdir` tree of `.toml`
//! files, no tonic server. This is exactly what the lib+bin split unlocked —
//! `tests/` sees only the *library* crate, so the sync logic had to be
//! reachable there (hence its extraction out of the gRPC handler into
//! `sync::sync_root`).

use std::fs;
use std::path::Path;

use stationd::{db, store, sync};

/// A fresh temp database with all real migrations applied — the same setup
/// the `store` unit tests use, so the reconciliation is exercised against
/// the actual migrated schema, not a mock.
async fn fresh_db() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempfile::tempdir().expect("temp dir for db");
    let path = dir.path().join("test.db");
    let pool = db::init(&path).await.expect("init + migrations");
    (dir, pool)
}

/// Write `content` to `root/rel`, creating parent directories as needed.
fn write_playlist(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(&path, content).expect("write playlist file");
}

/// A weighted group referencing a single member — used to build cycles.
fn group_referencing(target: &str) -> String {
    format!(
        r#"
name = "grp"
[selection]
mode = "group"
strategy = "weighted"
members = [{{ ref = "{target}", weight = 1 }}]
"#
    )
}

const VALID_DYNAMIC: &str = r#"
name = "Hits"
[selection]
mode = "dynamic"
order = "shuffle"
"#;

const VALID_REMOTE: &str = r#"
name = "goodnight"
[selection]
mode = "remote"
url = "http://nightmusic.live"
"#;

/// Best-effort: valid files are persisted, a broken file is reported but does
/// not abort the pass, and a non-`.toml` file is ignored entirely. Recursion
/// into subdirectories is exercised too (the remote lives in `nested/`).
#[tokio::test]
async fn persists_valid_reports_bad_ignores_non_toml() {
    let (_db_dir, pool) = fresh_db().await;
    let tree = tempfile::tempdir().expect("temp dir for playlists");
    let root = tree.path();

    write_playlist(root, "hits.toml", VALID_DYNAMIC);
    write_playlist(root, "nested/night.toml", VALID_REMOTE);
    // Syntactically broken TOML: reported, not fatal, not persisted.
    write_playlist(root, "broken.toml", "name =\n[selection]\nmode = \"dynamic\"\n");
    // Not a .toml file: neither counted nor reported.
    write_playlist(root, "README.md", "not a playlist\n");

    let outcome = sync::sync_root(&pool, root).await;

    assert_eq!(outcome.added, 2, "the two valid playlists are persisted");
    assert_eq!(outcome.errors.len(), 1, "only the broken file errors");
    assert_eq!(outcome.errors[0].path, "broken.toml");
    assert!(
        !outcome.errors.iter().any(|e| e.path.contains("README")),
        "a non-.toml file must be ignored, not reported"
    );

    // Survivors are in the view, keyed by their normalized relative path.
    let rows = store::list(&pool).await.expect("list");
    let keys: Vec<Option<&str>> = rows.iter().map(|r| r.rel_path.as_deref()).collect();
    assert_eq!(rows.len(), 2);
    assert!(keys.contains(&Some("hits")), "hits.toml → key `hits`");
    assert!(keys.contains(&Some("nested/night")), "recursion into nested/");
}

/// Targeted rewrite: a file lacking an id gets one written back; a file that
/// already carries a valid id is left byte-for-byte (no git churn).
#[tokio::test]
async fn writes_id_back_only_when_missing() {
    let (_db_dir, pool) = fresh_db().await;
    let tree = tempfile::tempdir().expect("temp dir");
    let root = tree.path();

    write_playlist(root, "fresh.toml", VALID_DYNAMIC);
    let with_id = r#"
id = "b3c55b4e-168d-449e-9bd2-046b814cbbd9"
name = "Already"
[selection]
mode = "dynamic"
"#;
    write_playlist(root, "kept.toml", with_id);
    let before_kept = fs::read_to_string(root.join("kept.toml")).unwrap();

    let outcome = sync::sync_root(&pool, root).await;
    assert_eq!(outcome.added, 2);
    assert!(outcome.errors.is_empty(), "both files are valid");

    let after_fresh = fs::read_to_string(root.join("fresh.toml")).unwrap();
    assert!(
        after_fresh.contains("id ="),
        "a file without an id must be rewritten with one"
    );

    let after_kept = fs::read_to_string(root.join("kept.toml")).unwrap();
    assert_eq!(
        after_kept, before_kept,
        "a file that already has a valid id must not be rewritten"
    );
}

/// Whole-set validation: a group cycle is reported and its members are
/// excluded from persistence, while a valid standalone still goes through.
#[tokio::test]
async fn excludes_cycle_members_from_persistence() {
    let (_db_dir, pool) = fresh_db().await;
    let tree = tempfile::tempdir().expect("temp dir");
    let root = tree.path();

    // a → b → a : both groups sit on a cycle and must be excluded.
    write_playlist(root, "a.toml", &group_referencing("b"));
    write_playlist(root, "b.toml", &group_referencing("a"));
    // A valid standalone survives alongside the rejected cycle.
    write_playlist(root, "solo.toml", VALID_DYNAMIC);

    let outcome = sync::sync_root(&pool, root).await;

    assert_eq!(outcome.added, 1, "only the non-cycle playlist is persisted");
    assert!(
        outcome.errors.iter().any(|e| e.message.contains("cycle")),
        "the cycle must be reported"
    );

    let rows = store::list(&pool).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].rel_path.as_deref(), Some("solo"));
}

/// Key collision: two files whose relative paths normalize to the same key
/// (case-only difference). One wins, the other is reported. Creating both
/// requires a case-sensitive filesystem — true on the Linux build/test
/// target; a case-insensitive FS simply could not hold both files.
#[tokio::test]
async fn reports_key_collision() {
    let (_db_dir, pool) = fresh_db().await;
    let tree = tempfile::tempdir().expect("temp dir");
    let root = tree.path();

    write_playlist(root, "dup.toml", VALID_DYNAMIC);
    write_playlist(root, "DUP.toml", VALID_REMOTE);

    let outcome = sync::sync_root(&pool, root).await;

    // Which file wins depends on walk order, so we don't assert on identity.
    assert_eq!(outcome.added, 1, "only one of the colliding files persists");
    assert!(
        outcome.errors.iter().any(|e| e.message.contains("normalized key")),
        "the collision must be reported"
    );

    let rows = store::list(&pool).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].rel_path.as_deref(), Some("dup"));
}
