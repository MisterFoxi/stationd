//! The shipped examples (`examples/`) follow the real grammar: every playlist
//! goes through the very `playlist sync` pass (parse, per-file validation,
//! whole-set ref resolution + cycle detection, persistence), and the grid is
//! parsed and its refs resolved against those playlists. An example that
//! drifts from the grammar fails `cargo test` instead of misleading a user.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use stationd::{db, grid_toml, playlist, sync};

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

/// Copy `src` into `dst` recursively: `sync` writes ids back into the files,
/// which must never touch the repository's examples.
fn copy_tree(src: &Path, dst: &Path) {
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.expect("walk examples");
        let rel = entry.path().strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).expect("create dir");
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Canonical keys of the example playlists (what `playlist_ref` / `ref` name).
fn playlist_keys(root: &Path) -> HashSet<String> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
        .map(|e| {
            let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().to_string();
            playlist::normalize_ref(&rel).expect("example path is a valid ref")
        })
        .collect()
}

#[tokio::test]
async fn example_playlists_sync_without_error() {
    let src = examples().join("playlist");
    let n_files = playlist_keys(&src).len();
    assert!(n_files > 0, "no example playlist found");

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("playlist");
    copy_tree(&src, &root);
    let pool = db::init(&dir.path().join("t.db")).await.expect("db");

    let out = sync::sync_root(&pool, &root).await;
    let errors: Vec<String> = out.errors.iter().map(|e| format!("{}: {}", e.path, e.message)).collect();
    assert!(errors.is_empty(), "example playlists rejected:\n{}", errors.join("\n"));
    assert_eq!(out.added as usize, n_files, "every example playlist is loaded");
}

#[test]
fn example_constraints_use_valid_durations() {
    // Anti-repetition windows are only parsed when the playlist airs: check
    // them here so a bad duration in an example cannot wait until then.
    for key in playlist_keys(&examples().join("playlist")) {
        let path = examples().join("playlist").join(format!("{key}.toml"));
        let p = playlist::Playlist::parse(&fs::read_to_string(&path).unwrap()).unwrap();
        let Some(c) = p.broadcast.and_then(|b| b.constraints) else { continue };
        for w in [c.no_same_artist_within, c.no_same_track_within].into_iter().flatten() {
            playlist::parse_duration_secs(&w).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
    }
}

#[test]
fn example_grid_parses_and_refs_resolve() {
    let text = fs::read_to_string(examples().join("grid.toml")).expect("examples/grid.toml");
    let rules = grid_toml::parse_grid(&text).unwrap_or_else(|e| panic!("examples/grid.toml: {e}"));
    assert!(!rules.is_empty());
    let known = playlist_keys(&examples().join("playlist"));
    let errors = grid_toml::validate_refs(&rules, &known);
    assert!(errors.is_empty(), "unknown refs:\n{}", errors.join("\n"));
    // The live slot is commented out: without [live] a live rule is an error.
    assert!(grid_toml::validate_djs(&rules, None).is_empty());
}
