//! Editing playlists through stationd: validate a draft, preview its pool,
//! save it into the playlist root — the metier behind `PlaylistService.
//! Validate / PreviewPool / Save / Export`, without any proto type (the gRPC
//! handlers in `playlist_grpc.rs` only translate).
//!
//! stationd is the only writer of its playlist root: a client (stationctl,
//! the TUI, later `api`), local or remote, sends TOML and never writes a
//! playlist file itself.
//!
//! Revision = fingerprint of the file CONTENT (`sha256:<hex>`), not its
//! modification time: reliable on NFS, and a file rewritten identically keeps
//! its revision. A save states the revision it started from; if the file
//! changed in between, nothing is written (conflict) — never an overwrite.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::playlist::{self, Diag, DiagCode, Mode, Playlist, SetEntry};
use crate::pool_inspection;
use crate::selection::{materialize_dynamic, materialize_static};
use crate::store;

/// Why an edit could not even be judged (distinct from a draft that is
/// merely invalid, which is an answer: diagnostics).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    /// The target ref is not a valid playlist ref (`..`, empty…).
    #[error("{0}")]
    BadRef(String),
    /// No playlist with this ref / id.
    #[error("no playlist `{0}` (neither a ref nor an id of the view)")]
    NotFound(String),
    /// A media path the index does not know.
    #[error("media `{0}` is unknown to the library index (paths are relative to the media root, case included)")]
    UnknownMedia(String),
    /// Several files map to the same ref (case): nothing done.
    #[error("several files map to `{key}` ({}): rename or delete one by hand", files.join(", "))]
    Ambiguous { key: String, files: Vec<String> },
    /// Reading / writing the file or the view failed.
    #[error("{0}")]
    Io(String),
}

/// `sha256:<hex>` of a file content.
pub fn revision(content: &[u8]) -> String {
    let digest = Sha256::digest(content);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// A playlist file on the disk.
#[derive(Debug, Clone, PartialEq)]
pub struct FileState {
    pub path: PathBuf,
    /// Relative to the root, as shown to the user.
    pub rel: String,
    pub content: String,
    pub revision: String,
}

/// The file of playlist `key` under `root`, if any. Several files for one
/// key (differing by case) = ambiguous: refused, nothing guessed.
pub fn file_for(root: &Path, key: &str) -> Result<Option<FileState>, EditError> {
    let files = crate::sync::files_by_key(root).remove(key).unwrap_or_default();
    let display = |p: &PathBuf| p.strip_prefix(root).unwrap_or(p).display().to_string();
    if files.len() > 1 {
        return Err(EditError::Ambiguous { key: key.to_string(), files: files.iter().map(display).collect() });
    }
    let Some(path) = files.into_iter().next() else { return Ok(None) };
    let content = std::fs::read_to_string(&path)
        .map_err(|e| EditError::Io(format!("cannot read {}: {e}", display(&path))))?;
    Ok(Some(FileState { rel: display(&path), revision: revision(content.as_bytes()), path, content }))
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Exported {
    pub id: String,
    pub rel_path: Option<String>,
    /// What stationd applied (the view).
    pub applied_toml: String,
    /// The file, when there is one: to edit it (comments included) and save
    /// it back with its revision.
    pub file: Option<FileState>,
}

impl Exported {
    /// The file differs from what is applied (edited by hand since, or
    /// invalid and so not applied).
    pub fn file_differs(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.content != self.applied_toml)
    }
}

pub async fn export(db: &SqlitePool, root: &Path, reference: &str) -> Result<Exported, EditError> {
    let row = store::find(db, reference)
        .await
        .map_err(|e| EditError::Io(format!("could not read the playlist view: {e}")))?
        .ok_or_else(|| EditError::NotFound(reference.to_string()))?;
    let file = match &row.rel_path {
        Some(key) => file_for(root, key)?,
        None => None,
    };
    Ok(Exported { id: row.id, rel_path: row.rel_path, applied_toml: row.toml, file })
}

// ---------------------------------------------------------------------------
// Validate
// ---------------------------------------------------------------------------

/// Key standing for a draft that has no place yet: cannot collide with a
/// real ref (a ref never contains a NUL).
const DRAFT_KEY: &str = "\u{0}draft";

/// Every problem of a draft: TOML / grammar, per-file rules, and the
/// set-level ones (member refs, cycles) judged against the CURRENT view
/// with this draft in place of `key` (or added, for a new playlist).
/// Returns the parsed playlist when it parses.
pub async fn validate_draft(
    db: &SqlitePool,
    text: &str,
    key: Option<&str>,
) -> Result<(Option<Playlist>, Vec<Diag>), EditError> {
    let playlist = match playlist::parse_with_diagnostics(text) {
        Ok(p) => p,
        Err(d) => return Ok((None, vec![d])),
    };
    let mut diags = playlist.diagnostics();

    let me = key.unwrap_or(DRAFT_KEY);
    let rows = store::all(db).await.map_err(|e| EditError::Io(format!("could not read the playlist view: {e}")))?;
    let mut set: Vec<SetEntry> = rows
        .into_iter()
        .filter_map(|r| {
            let k = r.rel_path?;
            if k == me {
                return None;
            }
            // Rows of the view were validated when applied; one that no
            // longer parses (older grammar) is left out rather than failing
            // every draft.
            Playlist::parse(&r.toml).ok().map(|p| SetEntry { key: k, playlist: p })
        })
        .collect();
    set.push(SetEntry { key: me.to_string(), playlist: playlist.clone() });
    diags.extend(playlist::validate_set(&set).into_iter().filter(|e| e.key == me).map(|e| e.diag()));
    Ok((Some(playlist), diags))
}

pub fn has_errors(diags: &[Diag]) -> bool {
    diags.iter().any(|d| d.error)
}

// ---------------------------------------------------------------------------
// PreviewPool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct PoolMedia {
    pub rel_path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemberPool {
    pub r#ref: String,
    pub resolved: Option<String>,
    pub count: Option<u64>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Preview {
    /// Valid draft, pool evaluated.
    pub ok: bool,
    pub diagnostics: Vec<Diag>,
    pub count: Option<u64>,
    pub duration_ms: Option<u64>,
    pub artists: Option<u64>,
    pub sample: Vec<PoolMedia>,
    pub members: Vec<MemberPool>,
}

pub const SAMPLE_DEFAULT: usize = 20;
pub const SAMPLE_MAX: usize = 100;

/// Evaluate the pool of a draft without applying it: what the index offers
/// TODAY (available media only), before anti-repetition and plugins — the
/// same materialization as playback, no cursor or state touched.
///
/// `now` (epoch seconds, the station clock) resolves the relative filters (`age`).
pub async fn preview(db: &SqlitePool, text: &str, key: Option<&str>, sample: usize, now: i64) -> Result<Preview, EditError> {
    let (playlist, diagnostics) = validate_draft(db, text, key).await?;
    let mut out = Preview { diagnostics, ..Preview::default() };
    let Some(playlist) = playlist else { return Ok(out) };
    if has_errors(&out.diagnostics) {
        return Ok(out);
    }
    let sample = if sample == 0 { SAMPLE_DEFAULT } else { sample.min(SAMPLE_MAX) };
    let sel = &playlist.selection;
    let candidates = match sel.mode {
        Mode::Dynamic => Some(materialize_dynamic(db, sel, now).await),
        Mode::Static => Some(materialize_static(db, &sel.files).await),
        Mode::Remote | Mode::Queue => None,
        Mode::Group => None,
    };
    match candidates {
        Some(Err(e)) => {
            // The per-file check passed, so this is the index talking (a
            // filter the database refuses): an error on the selection.
            out.diagnostics.push(Diag::error(DiagCode::BadFilter, "selection", &e.to_string()));
            return Ok(out);
        }
        Some(Ok(mut c)) => {
            c.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
            out.count = Some(c.len() as u64);
            out.duration_ms = Some(c.iter().map(|m| m.duration_ms).sum());
            let mut artists: Vec<&str> = c.iter().filter_map(|m| m.artist.as_deref()).filter(|a| !a.is_empty()).collect();
            artists.sort_unstable();
            artists.dedup();
            out.artists = Some(artists.len() as u64);
            out.sample = c
                .into_iter()
                .take(sample)
                .map(|m| PoolMedia { rel_path: m.rel_path, title: m.title, artist: m.artist, duration_ms: m.duration_ms })
                .collect();
            if out.count == Some(0) {
                out.diagnostics.push(Diag::warning(
                    DiagCode::EmptyPool,
                    if sel.mode == Mode::Static { "selection.files" } else { "selection.filter" },
                    "no available media matches today: this playlist would air nothing",
                ));
            }
        }
        None if sel.mode == Mode::Group => {
            let base = key.unwrap_or("draft");
            let (mut count, mut duration) = (Some(0_u64), Some(0_u64));
            for (i, m) in sel.members.iter().enumerate() {
                let mut mp = MemberPool { r#ref: m.r#ref.clone(), resolved: None, count: None, duration_ms: None, error: None };
                match playlist::resolve_member_ref(base, &m.r#ref) {
                    Err(e) => mp.error = Some(e),
                    Ok(k) => {
                        mp.resolved = Some(k.clone());
                        match pool_inspection::inspect_ref_at(db, &k, now).await {
                            Ok(i) => {
                                mp.count = i.stats.selected_count;
                                mp.duration_ms = i.stats.total_duration_ms;
                            }
                            Err(e) => mp.error = Some(e.to_string()),
                        }
                    }
                }
                if mp.count == Some(0) {
                    out.diagnostics.push(
                        Diag::warning(
                            DiagCode::EmptyPool,
                            &format!("selection.members[{}].ref", i + 1),
                            &format!("member `{}` has no available media today", m.r#ref),
                        )
                        .rejected(m.r#ref.clone()),
                    );
                }
                count = count.zip(mp.count).map(|(a, b)| a + b);
                duration = duration.zip(mp.duration_ms).map(|(a, b)| a + b);
                out.members.push(mp);
            }
            out.count = count;
            out.duration_ms = duration;
            if count == Some(0) {
                out.diagnostics.push(Diag::warning(
                    DiagCode::EmptyPool,
                    "selection.members",
                    "no member has an available media today: this group would air nothing",
                ));
            }
        }
        None => {}
    }
    out.ok = true;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Containing
// ---------------------------------------------------------------------------

/// A playlist of the view that can air a given media.
#[derive(Debug, Clone, PartialEq)]
pub struct Holder {
    pub id: String,
    pub rel_path: Option<String>,
    pub name: String,
    pub mode: Mode,
    pub enabled: bool,
}

/// The playlists of the view that can air media `rel_path`: static ones
/// listing it, dynamic ones whose filters keep it (available or not). Groups
/// are not unfolded (their members are listed). `NotFound` when the index
/// does not know the media: a mistyped path is said, never an empty answer.
///
/// `now` resolves the relative filters (`age`): « peut le diffuser » is as of now.
pub async fn containing(db: &SqlitePool, rel_path: &str, now: i64) -> Result<Vec<Holder>, EditError> {
    let io = |e: sqlx::Error| EditError::Io(format!("could not read the media index: {e}"));
    if crate::media_index::brief(db, rel_path).await.map_err(io)?.is_none() {
        return Err(EditError::UnknownMedia(rel_path.to_string()));
    }
    let rows = store::all(db).await.map_err(|e| EditError::Io(format!("could not read the playlist view: {e}")))?;
    let mut out = Vec::new();
    for row in rows {
        // A row that no longer parses (older grammar) cannot air anything.
        let Ok(p) = Playlist::parse(&row.toml) else { continue };
        let holds = match p.selection.mode {
            Mode::Static => crate::selection::static_lists(&p.selection.files, rel_path),
            // A filter the index refuses keeps nothing (it is reported by
            // Validate / on air, not here).
            Mode::Dynamic => crate::selection::dynamic_matches(db, &p.selection, rel_path, now).await.unwrap_or(false),
            Mode::Remote | Mode::Queue | Mode::Group => false,
        };
        if holds {
            out.push(Holder { id: row.id, rel_path: row.rel_path, name: p.name, mode: p.selection.mode, enabled: p.enabled });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved {
    /// Written and applied.
    pub ok: bool,
    pub diagnostics: Vec<Diag>,
    /// The expected revision no longer matches the file: nothing written.
    pub conflict: bool,
    /// After the write; the current one on a conflict (empty: no file).
    pub revision: String,
    /// What was written (id injected when absent).
    pub toml: String,
    pub id: String,
    /// Relative to the root.
    pub file: String,
    pub created: bool,
}

/// Validate `text`, write it as playlist `reference` under `root` (atomic:
/// temporary file + rename, then read back), apply it to the view.
///
/// - `expected_revision` empty = creation: refused (conflict) if the file
///   exists; otherwise it must equal the file's current revision.
/// - The playlist keeps its identity: a draft without `id` gets the file's
///   (or the view's) id; a draft with a different id is refused.
/// - Invalid → `ok = false` + diagnostics, nothing written.
pub async fn save(
    db: &SqlitePool,
    root: &Path,
    reference: &str,
    text: &str,
    expected_revision: &str,
) -> Result<Saved, EditError> {
    let key = playlist::normalize_ref(reference).map_err(EditError::BadRef)?;
    let existing = file_for(root, &key)?;
    let current = existing.as_ref().map(|f| f.revision.clone()).unwrap_or_default();
    let expected = expected_revision.trim();
    if expected != current {
        return Ok(Saved {
            conflict: true,
            revision: current,
            file: existing.map(|f| f.rel).unwrap_or_default(),
            ..Saved::default()
        });
    }

    let (playlist, mut diagnostics) = validate_draft(db, text, Some(&key)).await?;
    let Some(playlist) = playlist else {
        return Ok(Saved { diagnostics, revision: current, ..Saved::default() });
    };

    // Identity: the file's id, else the view's row for this ref.
    let view_row = store::find(db, &key).await.map_err(|e| EditError::Io(format!("could not read the playlist view: {e}")))?;
    let view_row = view_row.filter(|r| r.rel_path.as_deref() == Some(key.as_str()));
    let known_id = existing
        .as_ref()
        .and_then(|f| Playlist::parse(&f.content).ok())
        .and_then(|p| p.id)
        .map(|u| u.to_string())
        .or_else(|| view_row.as_ref().map(|r| r.id.clone()));
    match (playlist.id.map(|u| u.to_string()), &known_id) {
        (Some(draft), Some(known)) if &draft != known => diagnostics.push(
            Diag::error(DiagCode::IdChanged, "id", &format!("`id` cannot change (this playlist is `{known}`)")).rejected(draft),
        ),
        (Some(draft), None) => {
            // A new file claiming an id already used by another playlist.
            if let Some(other) = store::find(db, &draft).await.map_err(|e| EditError::Io(e.to_string()))? {
                let who = other.rel_path.unwrap_or_else(|| other.id.clone());
                diagnostics.push(
                    Diag::error(DiagCode::IdChanged, "id", &format!("`id` is already the id of playlist `{who}`")).rejected(draft),
                );
            }
        }
        _ => {}
    }
    if has_errors(&diagnostics) {
        return Ok(Saved { diagnostics, revision: current, ..Saved::default() });
    }

    let (content, id) = match (&playlist.id, &known_id) {
        (None, Some(known)) => {
            let mut doc = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e| EditError::Io(format!("could not rewrite the TOML: {e}")))?;
            doc["id"] = toml_edit::value(known.as_str());
            (doc.to_string(), known.clone())
        }
        _ => {
            let (t, id) = playlist::assign_id(text).map_err(|e| EditError::Io(e.to_string()))?;
            (t, id.to_string())
        }
    };
    let playlist = Playlist::parse(&content).map_err(|e| EditError::Io(e.to_string()))?;

    let created = existing.is_none();
    let path = match &existing {
        Some(f) => f.path.clone(),
        None => root.join(format!("{key}.toml")),
    };
    let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
    write_atomic(&path, &content).map_err(|e| EditError::Io(format!("could not write {rel}: {e}")))?;

    store::upsert(db, &id, &playlist, &content, Some(&key)).await.map_err(|e| {
        EditError::Io(format!("{rel} written but the view was not updated ({e}): run `playlist reload`"))
    })?;

    Ok(Saved {
        ok: true,
        diagnostics,
        conflict: false,
        revision: revision(content.as_bytes()),
        toml: content,
        id,
        file: rel,
        created,
    })
}

/// Write `content` to `path` so a reader never sees half a file: temporary
/// file in the same directory, flushed, renamed over the target, then read
/// back (a network filesystem may accept a write it did not keep — checked,
/// not assumed).
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("playlist.toml");
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        let back = std::fs::read(path)?;
        if back != content.as_bytes() {
            return Err(std::io::Error::other("read back differs from what was written"));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_a_content_fingerprint() {
        assert_eq!(revision(b"a"), revision(b"a"));
        assert_ne!(revision(b"a"), revision(b"b"));
        assert!(revision(b"").starts_with("sha256:"));
        assert_eq!(revision(b"").len(), 7 + 64);
    }

    #[test]
    fn write_atomic_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/x.toml");
        write_atomic(&p, "name = \"x\"\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "name = \"x\"\n");
        let names: Vec<_> = std::fs::read_dir(dir.path().join("sub")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }
}
