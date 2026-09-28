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
    /// Summary: the first diagnostic, or why the file could not be handled.
    pub message: String,
    /// Every validation problem, tied to its field (empty when the file was
    /// rejected for another reason: unreadable, path collision…).
    pub diagnostics: Vec<playlist::Diag>,
}

impl SyncError {
    fn plain(path: String, message: String) -> Self {
        Self { path, message, diagnostics: Vec::new() }
    }

    fn from_diags(path: String, diagnostics: Vec<playlist::Diag>) -> Self {
        let message = diagnostics.first().map(|d| d.message.clone()).unwrap_or_default();
        Self { path, message, diagnostics }
    }
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
                errors.push(SyncError::plain(rel_display, msg));
                continue;
            }
        };

        // Two files normalizing to the same key = conflict (e.g. case).
        if let Some(prev) = seen_keys.get(&key) {
            errors.push(SyncError::plain(
                rel_display.clone(),
                format!("path collides with `{prev}` (same normalized key `{key}`)"),
            ));
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(SyncError::plain(rel_display, format!("cannot read: {e}")));
                continue;
            }
        };

        let playlist = match playlist::parse_with_diagnostics(&content) {
            Ok(p) => p,
            Err(d) => {
                errors.push(SyncError::from_diags(rel_display, vec![d]));
                continue;
            }
        };
        let diags = playlist.diagnostics();
        if !diags.is_empty() {
            errors.push(SyncError::from_diags(rel_display, diags));
            continue;
        }
        let (rewritten, id) = match playlist::assign_id(&content) {
            Ok(r) => r,
            Err(e) => {
                errors.push(SyncError::plain(rel_display, e.to_string()));
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

    // One report per file, with all its set-level diagnostics.
    let mut bad_keys: HashSet<String> = HashSet::new();
    let mut by_key: Vec<(String, Vec<playlist::Diag>)> = Vec::new();
    for se in set_errors {
        bad_keys.insert(se.key.clone());
        match by_key.iter_mut().find(|(k, _)| *k == se.key) {
            Some((_, v)) => v.push(se.diag()),
            None => by_key.push((se.key.clone(), vec![se.diag()])),
        }
    }
    for (key, diags) in by_key {
        // Map the key back to a displayable path for the report.
        let path = loaded.iter().find(|l| l.key == key).map(|l| l.rel_display.clone()).unwrap_or(key);
        errors.push(SyncError::from_diags(path, diags));
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
            errors.push(SyncError::plain(l.rel_display.clone(), format!("could not update playlist view: {e}")));
            continue;
        }
        // Write the id back only if assign_id changed the file (avoids
        // churning git on files that already carry an id).
        if l.rewritten != l.content {
            if let Err(e) = std::fs::write(&l.disk_path, &l.rewritten) {
                errors.push(SyncError::plain(
                    l.rel_display.clone(),
                    format!("reconciled but could not write id back: {e}"),
                ));
                continue;
            }
        }
        added += 1;
    }

    SyncOutcome { added, errors }
}

// ---------------------------------------------------------------------------
// remove / reload: the view follows the root, never a dangling reference
// ---------------------------------------------------------------------------

/// The playlist files under `root`, by canonical key (every `*.toml`,
/// valid or not: an invalid file still exists — its last valid row stays).
pub(crate) fn files_by_key(root: &Path) -> HashMap<String, Vec<PathBuf>> {
    let mut map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for entry in walkdir::WalkDir::new(root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        if let Ok(key) = playlist::normalize_ref(&rel.to_string_lossy()) {
            map.entry(key).or_default().push(path.to_path_buf());
        }
    }
    map
}

/// Who references playlist `key`: the grid rules airing it, and the groups
/// of the view listing it as a member — except the groups in `ignore`
/// (themselves on their way out). Human-readable, e.g. "grid rule `night`",
/// "group `emission/emission`".
pub async fn referrers(
    db: &SqlitePool,
    key: &str,
    ignore: &HashSet<String>,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let grid = crate::grid_index::load_grid(db).await.map_err(|e| format!("could not read the grid: {e}"))?;
    for rule in &grid.rules {
        if let Some(raw) = crate::grid_toml::playlist_ref_of(rule) {
            if playlist::normalize_ref(raw).as_deref() == Ok(key) {
                out.push(format!("grid rule `{}`", rule.id));
            }
        }
    }
    let rows = store::all(db).await.map_err(|e| format!("could not read the playlist view: {e}"))?;
    for row in rows {
        let Some(group_key) = row.rel_path else { continue };
        if group_key == key || ignore.contains(&group_key) {
            continue;
        }
        let Ok(pl) = Playlist::parse(&row.toml) else { continue };
        if pl.selection.mode != playlist::Mode::Group {
            continue;
        }
        let member = pl
            .selection
            .members
            .iter()
            .any(|m| playlist::resolve_member_ref(&group_key, &m.r#ref).as_deref() == Ok(key));
        if member {
            out.push(format!("group `{group_key}`"));
        }
    }
    Ok(out)
}

/// Why a `remove` did not happen.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoveError {
    /// No playlist with this UUID or ref.
    NotFound(String),
    /// Still aired by grid rules / listed by groups: nothing removed.
    Referenced { key: String, by: Vec<String> },
    /// Several files map to the same ref (case): ambiguous, nothing removed.
    Ambiguous { key: String, files: Vec<String> },
    /// The file could not be deleted / the view could not be read or written.
    Failed(String),
    /// The file changed since the revision the caller read: nothing removed.
    Conflict { key: String, revision: String },
}

impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoveError::NotFound(r) => write!(f, "no playlist `{r}` (neither a ref nor an id of the view)"),
            RemoveError::Referenced { key, by } => {
                write!(f, "playlist `{key}` is still referenced by {}: remove those references first", by.join(", "))
            }
            RemoveError::Ambiguous { key, files } => {
                write!(f, "several files map to `{key}` ({}): rename or delete one by hand", files.join(", "))
            }
            RemoveError::Failed(m) => f.write_str(m),
            RemoveError::Conflict { key, revision } => write!(
                f,
                "playlist `{key}` changed since it was read (file now at {}): nothing removed",
                if revision.is_empty() { "no file" } else { revision.as_str() }
            ),
        }
    }
}

/// What a `remove` did.
#[derive(Debug, Clone, PartialEq)]
pub struct Removed {
    pub id: String,
    /// `None` for an add-only entry (no position in the tree).
    pub rel_path: Option<String>,
    /// The file deleted, relative to the root (`None`: none on disk).
    pub file: Option<String>,
}

/// Remove playlist `reference` (UUID or ref): its file under `root` first,
/// then its row. File-first: removing the row alone would bring it back at
/// the next sync. Refused while referenced (no dangling ref in the grid or a
/// group). A file that can't be deleted leaves everything as it was.
/// `expected_revision` (when given) must still be the file's: otherwise
/// nothing is removed (someone changed it since it was read).
pub async fn remove(
    db: &SqlitePool,
    root: &Path,
    reference: &str,
    expected_revision: Option<&str>,
) -> Result<Removed, RemoveError> {
    let row = store::find(db, reference)
        .await
        .map_err(|e| RemoveError::Failed(format!("could not read the playlist view: {e}")))?
        .ok_or_else(|| RemoveError::NotFound(reference.to_string()))?;

    let mut file = None;
    if let Some(key) = &row.rel_path {
        let by = referrers(db, key, &HashSet::new()).await.map_err(RemoveError::Failed)?;
        if !by.is_empty() {
            return Err(RemoveError::Referenced { key: key.clone(), by });
        }
        let files = files_by_key(root).remove(key).unwrap_or_default();
        let display = |p: &PathBuf| p.strip_prefix(root).unwrap_or(p).display().to_string();
        if files.len() > 1 {
            return Err(RemoveError::Ambiguous { key: key.clone(), files: files.iter().map(display).collect() });
        }
        if let Some(expected) = expected_revision {
            let current = match files.first() {
                Some(p) => std::fs::read(p)
                    .map(|c| crate::playlist_edit::revision(&c))
                    .map_err(|e| RemoveError::Failed(format!("cannot read {}: {e}", display(p))))?,
                None => String::new(),
            };
            if current != expected.trim() {
                return Err(RemoveError::Conflict { key: key.clone(), revision: current });
            }
        }
        if let Some(path) = files.first() {
            std::fs::remove_file(path)
                .map_err(|e| RemoveError::Failed(format!("could not delete {}: {e}", display(path))))?;
            file = Some(display(path));
        }
    }
    store::delete(db, &row.id).await.map_err(|e| {
        RemoveError::Failed(format!("file deleted but the view was not updated ({e}): run `playlist reload`"))
    })?;
    Ok(Removed { id: row.id, rel_path: row.rel_path, file })
}

/// Outcome of a `reload`: the `sync` part, plus the refs dropped from the
/// view because their file is gone.
#[derive(Debug, Clone, PartialEq)]
pub struct ReloadOutcome {
    pub added: u32,
    pub removed: Vec<String>,
    pub errors: Vec<SyncError>,
}

/// Make the view exactly the root: [`sync_root`] (add / update), then drop
/// every row whose file is gone — unless a grid rule or a group that stays
/// still references it (kept, reported: never a dangling reference). A file
/// that is merely invalid still exists: its last valid row stays. Add-only
/// entries (no path) are not the root's: left alone.
pub async fn reload_root(db: &SqlitePool, root: &Path) -> ReloadOutcome {
    let SyncOutcome { added, mut errors } = sync_root(db, root).await;
    let mut removed = Vec::new();

    let on_disk = files_by_key(root);
    let rows = match store::all(db).await {
        Ok(r) => r,
        Err(e) => {
            errors.push(SyncError::plain("(view)".into(), format!("could not read the playlist view: {e}")));
            return ReloadOutcome { added, removed, errors };
        }
    };
    let gone: Vec<(String, String)> = rows
        .into_iter()
        .filter_map(|r| r.rel_path.map(|k| (r.id, k)))
        .filter(|(_, k)| !on_disk.contains_key(k))
        .collect();
    // A group going away with its member does not hold the member back —
    // but a group KEPT (still referenced) does: settle the kept set first.
    let mut leaving: HashSet<String> = gone.iter().map(|(_, k)| k.clone()).collect();
    let mut kept: HashMap<String, Vec<String>> = HashMap::new();
    loop {
        let mut changed = false;
        for (_, key) in &gone {
            if !leaving.contains(key) {
                continue;
            }
            match referrers(db, key, &leaving).await {
                Ok(by) if by.is_empty() => {}
                Ok(by) => {
                    leaving.remove(key);
                    kept.insert(key.clone(), by);
                    changed = true;
                }
                Err(e) => {
                    leaving.remove(key);
                    errors.push(SyncError::plain(key.clone(), e));
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    for (id, key) in gone {
        if let Some(by) = kept.remove(&key) {
            errors.push(SyncError::plain(
                key,
                format!("file gone but still referenced by {}: kept in the view", by.join(", ")),
            ));
        } else if leaving.contains(&key) {
            match store::delete(db, &id).await {
                Ok(_) => removed.push(key),
                Err(e) => errors.push(SyncError::plain(key, format!("file gone, could not drop it from the view: {e}"))),
            }
        }
    }
    ReloadOutcome { added, removed, errors }
}
