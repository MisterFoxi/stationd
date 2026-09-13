//! Playlist reconciliation (`sync`): walk stationd's playlist root, load and
//! validate every `*.toml`, and project the survivors into the SQLite view.
//!
//! This is the metier behind the `PlaylistSync` RPC, deliberately extracted
//! from the gRPC handler so it can be driven end-to-end by an integration
//! test (a temp DB + a `tempdir` tree of files) without standing up a tonic
//! server. The handler in `grpc.rs` is a thin translator that calls
//! [`sync_root`] and maps [`SyncError`] to the proto error type — the same
//! discipline as `store` returning `PlaylistRow` rather than the proto
//! `PlaylistSummary` (no proto types leaking into the metier).
//!
//! stationd is the single writer here: it reads its *own* source-of-truth
//! directory and is allowed to rewrite a file's id back in place. This is
//! distinct from `playlist add`, where a client brings in one file from
//! outside and the file I/O is delegated to the CLI.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::playlist::{self, Playlist};
use crate::store;

/// One reconciliation problem, tied to a displayable path. A plain metier
/// type — `grpc.rs` maps it to the proto `PlaylistSyncError`.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncError {
    /// Path as shown to the user (relative to the root when possible).
    pub path: String,
    pub message: String,
}

/// Outcome of a reconciliation pass: how many playlists were persisted, and
/// every problem encountered (best-effort — a bad file is reported, it does
/// not abort the pass).
#[derive(Debug, Clone, PartialEq)]
pub struct SyncOutcome {
    pub added: u32,
    pub errors: Vec<SyncError>,
}

/// Reconcile every `*.toml` under `root` (recursive) into the view.
///
/// Three passes, matching the architecture doc:
/// 1. load + per-file validation (best-effort; bad files reported, not fatal);
/// 2. whole-set validation (ref resolution + cycle detection), which excludes
///    any offending playlist from the write;
/// 3. persist the survivors, writing an id back only to files that lacked one
///    (no churn on files that already carry an id).
pub async fn sync_root(db: &SqlitePool, root: &Path) -> SyncOutcome {
    let mut errors: Vec<SyncError> = Vec::new();

    // ---- Pass 1: load + per-file validation --------------------------
    // Collect the files that pass parse + per-file validation, each with its
    // canonical key (normalized relative path) and rewritten TOML. Bad files
    // are reported but do not stop the pass (best-effort).
    struct Loaded {
        rel_display: String, // path as shown to the user
        key: String,         // canonical key (normalized)
        disk_path: PathBuf,
        content: String,
        rewritten: String,
        id: String,
        playlist: Playlist,
    }
    let mut loaded: Vec<Loaded> = Vec::new();
    let mut seen_keys: HashMap<String, String> = HashMap::new();

    for entry in walkdir::WalkDir::new(root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }

        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel_display = rel.display().to_string();

        // Canonical key from the file's relative path.
        let key = match playlist::normalize_ref(&rel.to_string_lossy()) {
            Ok(k) => k,
            Err(msg) => {
                errors.push(SyncError { path: rel_display, message: msg });
                continue;
            }
        };

        // Two files normalizing to the same key = conflict (e.g. case).
        if let Some(prev) = seen_keys.get(&key) {
            errors.push(SyncError {
                path: rel_display.clone(),
                message: format!("path collides with `{prev}` (same normalized key `{key}`)"),
            });
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(SyncError {
                    path: rel_display,
                    message: format!("cannot read: {e}"),
                });
                continue;
            }
        };

        let playlist = match Playlist::parse(&content) {
            Ok(p) => p,
            Err(e) => {
                errors.push(SyncError { path: rel_display, message: e.to_string() });
                continue;
            }
        };
        if let Err(e) = playlist.validate() {
            errors.push(SyncError { path: rel_display, message: e.to_string() });
            continue;
        }
        let (rewritten, id) = match playlist::assign_id(&content) {
            Ok(r) => r,
            Err(e) => {
                errors.push(SyncError { path: rel_display, message: e.to_string() });
                continue;
            }
        };

        seen_keys.insert(key.clone(), rel_display.clone());
        loaded.push(Loaded {
            rel_display,
            key,
            disk_path: path.to_path_buf(),
            content,
            rewritten,
            id: id.to_string(),
            playlist,
        });
    }

    // ---- Pass 2: whole-set validation (refs + cycles) ----------------
    // Pure, database-free. Any entry flagged here is excluded from the write
    // below — we never persist a playlist that fails set-level validation.
    let set_entries: Vec<playlist::SetEntry> = loaded
        .iter()
        .map(|l| playlist::SetEntry {
            key: l.key.clone(),
            playlist: l.playlist.clone(),
        })
        .collect();
    let set_errors = playlist::validate_set(&set_entries);

    let mut bad_keys: HashSet<String> = HashSet::new();
    for se in set_errors {
        bad_keys.insert(se.key.clone());
        // Map the key back to a displayable path for the report.
        let path = loaded
            .iter()
            .find(|l| l.key == se.key)
            .map(|l| l.rel_display.clone())
            .unwrap_or(se.key);
        errors.push(SyncError { path, message: se.message });
    }

    // ---- Pass 3: persist the survivors -------------------------------
    // Only files that passed BOTH per-file and set-level validation are
    // written to the view and (if needed) rewritten with their id.
    let mut added: u32 = 0;
    for l in &loaded {
        if bad_keys.contains(&l.key) {
            continue;
        }
        if let Err(e) = store::upsert(db, &l.id, &l.playlist, &l.rewritten, Some(&l.key)).await {
            errors.push(SyncError {
                path: l.rel_display.clone(),
                message: format!("could not update playlist view: {e}"),
            });
            continue;
        }
        // Write the id back only if assign_id changed the file (avoids
        // churning git on files that already carry an id).
        if l.rewritten != l.content {
            if let Err(e) = std::fs::write(&l.disk_path, &l.rewritten) {
                errors.push(SyncError {
                    path: l.rel_display.clone(),
                    message: format!("reconciled but could not write id back: {e}"),
                });
                continue;
            }
        }
        added += 1;
    }

    SyncOutcome { added, errors }
}
