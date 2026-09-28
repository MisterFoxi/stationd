//! `playlist remove` / `export` / `reload`, end to end through the library
//! (temp DB + a `tempdir` playlist root, no tonic server).

use std::fs;
use std::path::Path;

use stationd::grid_index::insert_rule;
use stationd::resolver::{Rule, RuleKind, Validity};
use stationd::sync::{self, RemoveError};
use stationd::{db, store};

async fn fresh_db() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let pool = db::init(&dir.path().join("test.db")).await.unwrap();
    (dir, pool)
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

const DYNAMIC: &str = "name = \"Hits\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n";

fn group(members: &[&str]) -> String {
    let m: Vec<String> = members.iter().map(|r| format!("{{ ref = \"{r}\", weight = 1 }}")).collect();
    format!("name = \"grp\"\n[selection]\nmode = \"group\"\nstrategy = \"weighted\"\nmembers = [{}]\n", m.join(", "))
}

async fn base_rule(pool: &sqlx::SqlitePool, id: &str, playlist: &str) {
    let rule = Rule {
        id: id.into(),
        enabled: true,
        validity: Validity::default(),
        kind: RuleKind::BaseRotation { playlist_ref: playlist.into() },
    };
    insert_rule(pool, &rule).await.unwrap();
}

async fn keys(pool: &sqlx::SqlitePool) -> Vec<String> {
    store::list(pool).await.unwrap().into_iter().filter_map(|r| r.rel_path).collect()
}

#[tokio::test]
async fn remove_deletes_the_file_and_the_row_and_survives_a_sync() {
    let (_d, pool) = fresh_db().await;
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path();
    write(root, "Rock/Hits.toml", DYNAMIC); // case kept on disk, key lowercased
    write(root, "jingles.toml", DYNAMIC);
    sync::sync_root(&pool, root).await;

    let r = sync::remove(&pool, root, "rock/hits").await.unwrap();
    assert_eq!(r.rel_path.as_deref(), Some("rock/hits"));
    assert_eq!(r.file.as_deref(), Some(Path::new("Rock").join("Hits.toml").to_str().unwrap()));
    assert!(!root.join("Rock/Hits.toml").exists());
    assert_eq!(keys(&pool).await, ["jingles"]);
    // File-first: the next sync does not bring it back.
    sync::sync_root(&pool, root).await;
    assert_eq!(keys(&pool).await, ["jingles"]);
    // By UUID too.
    let id = store::find(&pool, "jingles").await.unwrap().unwrap().id;
    sync::remove(&pool, root, &id).await.unwrap();
    assert!(keys(&pool).await.is_empty());
}

#[tokio::test]
async fn remove_is_refused_while_referenced_and_unknown_is_not_found() {
    let (_d, pool) = fresh_db().await;
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path();
    write(root, "music.toml", DYNAMIC);
    write(root, "shows/intro.toml", DYNAMIC);
    write(root, "shows/main.toml", &group(&["./intro"]));
    sync::sync_root(&pool, root).await;
    base_rule(&pool, "floor", "Music").await;

    match sync::remove(&pool, root, "music").await {
        Err(RemoveError::Referenced { by, .. }) => assert_eq!(by, ["grid rule `floor`"]),
        other => panic!("{other:?}"),
    }
    match sync::remove(&pool, root, "shows/intro").await {
        Err(RemoveError::Referenced { by, .. }) => assert_eq!(by, ["group `shows/main`"]),
        other => panic!("{other:?}"),
    }
    assert!(root.join("music.toml").exists() && root.join("shows/intro.toml").exists(), "nothing touched");
    assert!(matches!(sync::remove(&pool, root, "nope").await, Err(RemoveError::NotFound(_))));
    // The group goes first, then its member is free.
    sync::remove(&pool, root, "shows/main").await.unwrap();
    sync::remove(&pool, root, "shows/intro").await.unwrap();
}

#[tokio::test]
async fn remove_of_an_add_only_entry_touches_no_file() {
    let (_d, pool) = fresh_db().await;
    let tree = tempfile::tempdir().unwrap();
    let pl = stationd::playlist::Playlist::parse(DYNAMIC).unwrap();
    store::upsert(&pool, "11111111-1111-1111-1111-111111111111", &pl, DYNAMIC, None).await.unwrap();
    let r = sync::remove(&pool, tree.path(), "11111111-1111-1111-1111-111111111111").await.unwrap();
    assert_eq!((r.rel_path, r.file), (None, None));
    assert!(store::list(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn export_is_the_applied_toml_by_ref_or_id() {
    let (_d, pool) = fresh_db().await;
    let tree = tempfile::tempdir().unwrap();
    write(tree.path(), "hits.toml", DYNAMIC);
    sync::sync_root(&pool, tree.path()).await;
    let row = store::find(&pool, "HITS.toml").await.unwrap().unwrap();
    assert_eq!(row.rel_path.as_deref(), Some("hits"));
    assert_eq!(row.toml, fs::read_to_string(tree.path().join("hits.toml")).unwrap(), "with its id");
    assert_eq!(store::find(&pool, &row.id).await.unwrap(), Some(row));
    assert_eq!(store::find(&pool, "missing").await.unwrap(), None);
}

#[tokio::test]
async fn reload_drops_the_gone_files_but_never_a_referenced_one() {
    let (_d, pool) = fresh_db().await;
    let tree = tempfile::tempdir().unwrap();
    let root = tree.path();
    for k in ["a", "b", "c", "d", "m"] {
        write(root, &format!("{k}.toml"), DYNAMIC);
    }
    write(root, "g.toml", &group(&["m"])); // group g → m
    write(root, "h.toml", &group(&["d"])); // group h → d
    sync::sync_root(&pool, root).await;
    base_rule(&pool, "floor", "b").await; // grid → b
    base_rule(&pool, "night", "h").await; // grid → h → d

    // Gone: a (free), b (grid), g + m (a group leaving with its member),
    // h + d (a group kept by the grid holds its member back). c stays.
    for k in ["a", "b", "g", "m", "h", "d"] {
        fs::remove_file(root.join(format!("{k}.toml"))).unwrap();
    }
    // An invalid file still exists: its last valid row stays.
    write(root, "c.toml", "name =\n");
    let out = sync::reload_root(&pool, root).await;

    let mut removed = out.removed.clone();
    removed.sort();
    assert_eq!(removed, ["a", "g", "m"]);
    assert_eq!(keys(&pool).await, ["b", "c", "d", "h"]);
    let kept: Vec<&str> = out.errors.iter().map(|e| e.path.as_str()).collect();
    assert!(kept.contains(&"b") && kept.contains(&"h") && kept.contains(&"d") && kept.contains(&"c.toml"), "{:?}", out.errors);
    let msg = |p: &str| out.errors.iter().find(|e| e.path == p).unwrap().message.clone();
    assert!(msg("b").contains("grid rule `floor`"), "{}", msg("b"));
    assert!(msg("d").contains("group `h`"), "{}", msg("d"));
}
