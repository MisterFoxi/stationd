//! Library actor: the single owning task for the media library view.
//!
//! Why an actor rather than a handler calling `replace_library` directly: one
//! place through which every library mutation flows, so concurrency is
//! deterministic and instrumentable — the RTOS shape of "one task owns the
//! resource, everyone else posts to its queue". The owning loop consumes
//! commands one at a time, which buys the anti-concurrent-scan guarantee for
//! free: two `Scan`s cannot interleave, and a `List` issued mid-scan simply
//! waits for it. The heavy work (walk + tag parse) is offloaded to a blocking
//! thread via `spawn_blocking`, so the actor awaiting it yields to the runtime
//! — the rest of stationd (Station, Schedule) stays responsive; only Library
//! commands serialise behind a running scan, which is exactly what we want.
//!
//! Ownership is by discipline, not by the type system: the `sqlx` pool is
//! clonable, and this actor merely owns the sole *command channel* to the
//! library. Nothing else must write `media`/`media_genre` — same single-writer
//! discipline as the TOML source of truth. This little `spawn` + `Handle` +
//! `mpsc<Command>` shape is meant to become the template for the eventual
//! `GridEngine` refactor.

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;
use tokio::sync::{mpsc, oneshot, watch};

use crate::media::{self, ScanError, ScanReport};
use crate::media_tags::{self, StandardTags, TagEdit, TagError};
use crate::media_index::{self, GenreInventory, MediaRow, ReplaceStats, SearchPage, SearchQuery};
use crate::plugin::{PluginHandle, ScanInput};

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("media root {0} does not exist or is not a directory")]
    BadRoot(PathBuf),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("scan worker failed to join: {0}")]
    Join(String),
    #[error("library actor is no longer running")]
    ActorGone,
    #[error("invalid filter: {0}")]
    BadFilter(String),
    #[error(transparent)]
    Tags(#[from] TagError),
    /// Internal: a tag write that may have changed the file, with the file
    /// re-read (to refresh the index before reporting the error).
    #[error("{0}")]
    Touched(TagError, Option<ScanReport>),
}

impl From<ScanError> for LibraryError {
    fn from(e: ScanError) -> Self {
        match e {
            ScanError::BadRoot(p) => LibraryError::BadRoot(p),
        }
    }
}

/// Result of a scan: what the scanner saw (`report`) and how the view was
/// reconciled (`stats`).
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    pub report: ScanReport,
    pub stats: ReplaceStats,
}

enum Command {
    Reorganize {
        dry_run: bool,
        reply: oneshot::Sender<Result<crate::library_reorganize::Report, LibraryError>>,
    },
    Folders { include_unavailable: bool, after: String, limit: usize, reply: oneshot::Sender<Result<media_index::FolderPage, LibraryError>> },
    Scan {
        reanalyze: bool,
        reply: oneshot::Sender<Result<ScanOutcome, LibraryError>>,
    },
    List {
        only_available: bool,
        genres: Vec<String>,
        reply: oneshot::Sender<Result<Vec<MediaRow>, LibraryError>>,
    },
    Genres {
        only_available: bool,
        reply: oneshot::Sender<Result<GenreInventory, LibraryError>>,
    },
    Search {
        query: Box<SearchQuery>,
        reply: oneshot::Sender<Result<SearchPage, LibraryError>>,
    },
    Prune {
        seen_before: Option<i64>,
        reply: oneshot::Sender<Result<u64, LibraryError>>,
    },
    GetTags {
        rel_path: String,
        reply: oneshot::Sender<Result<TagsRead, LibraryError>>,
    },
    SetTags {
        rel_path: String,
        revision: String,
        edit: TagEdit,
        reply: oneshot::Sender<Result<TagsWritten, LibraryError>>,
    },
    TagInventory {
        reply: oneshot::Sender<Result<Vec<media_index::OriginInventory>, LibraryError>>,
    },
    RenamePreview {
        origin: String,
        from: String,
        to: String,
        reply: oneshot::Sender<Result<RenamePreview, LibraryError>>,
    },
    Rename {
        origin: String,
        from: String,
        to: String,
        steps: mpsc::Sender<RenameStep>,
        reply: oneshot::Sender<Result<(), LibraryError>>,
    },
}

/// The tags of a file and their revision, with what the editor needs
/// around them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagsRead {
    pub tags: StandardTags,
    pub revision: String,
    /// User frames that the `custom-tags` plugin turns into genres.
    pub genre_sources: Vec<String>,
    /// Tempo labels to choose from: the plugin's ranges, then those in use.
    pub tempo_choices: Vec<String>,
    /// Effective tempo / creation (`media_meta`), whatever their source.
    pub tempo: Option<String>,
    pub creation: Option<String>,
}

/// What a tag edit did: `conflict` (nothing written, `tags` = current ones)
/// or written, read back, and the index row refreshed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagsWritten {
    pub conflict: bool,
    pub tags: TagsRead,
    pub row: Option<MediaRow>,
}

/// Cheap, clonable handle to the library actor. Every caller (gRPC handler,
/// tests) reaches the library only through this.
#[derive(Clone)]
pub struct LibraryHandle {
    tx: mpsc::Sender<Command>,
    status: watch::Receiver<ScanStatus>,
}

/// Where a scan is. `phase` = `Idle` between scans; `last` = how the last
/// one ended (kept until the next one ends).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStatus {
    pub phase: ScanPhase,
    /// Files read so far / audio files found by the walk (`Reading`).
    pub done: u64,
    pub total: u64,
    /// Epoch seconds, 0 = never.
    pub started_at: i64,
    pub last: Option<ScanEnd>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScanPhase {
    #[default]
    Idle,
    /// Walking the directories (counting the audio files).
    Listing,
    /// Reading each file's tags.
    Reading,
    /// Estimating missing BPMs (a plugin asked for it).
    Analyzing,
    /// The plugins' `on_scan`.
    Plugins,
    /// Writing derived values back into the files.
    Writing,
    /// Reconciling the index.
    Indexing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEnd {
    pub finished_at: i64,
    /// `Ok` = the counts; `Err` = why it failed.
    pub result: Result<ScanCounts, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanCounts {
    pub found: u64,
    pub skipped: u64,
    pub present: u64,
    pub unavailable: u64,
    pub vanished: u64,
}

/// One step of a rename across the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameStep {
    /// The files to change are known.
    Started { files: u64 },
    /// One file done (`error` = why it was not changed).
    File { rel_path: String, error: Option<String> },
    /// All done.
    Finished { changed: u64, unchanged: u64, failed: u64 },
}

/// What a rename would touch: the files, and the playlists whose filters
/// name the value (they select differently afterwards).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenamePreview {
    pub files: Vec<String>,
    pub playlists: Vec<String>,
    /// `to` already exists for this origin: the rename merges into it.
    pub merges: bool,
}

impl LibraryHandle {
    pub async fn reorganize(&self, dry_run: bool) -> Result<crate::library_reorganize::Report, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Command::Reorganize { dry_run, reply }).await.map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }
    /// The scan's progress, current value then each change.
    pub fn watch_scan(&self) -> watch::Receiver<ScanStatus> {
        self.status.clone()
    }

    /// Values per origin (the file's genre, then each `custom-tags` source).
    pub async fn tag_inventory(&self) -> Result<Vec<media_index::OriginInventory>, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Command::TagInventory { reply }).await.map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// What renaming `from` into `to` for `origin` would touch.
    pub async fn rename_preview(&self, origin: String, from: String, to: String) -> Result<RenamePreview, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::RenamePreview { origin, from, to, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// Rename `from` into `to` for `origin`, file by file (merging when `to`
    /// exists). Each step is sent on `steps`; returns once all are done. A
    /// file that fails is reported and the rename goes on.
    pub async fn rename(
        &self,
        origin: String,
        from: String,
        to: String,
        steps: mpsc::Sender<RenameStep>,
    ) -> Result<(), LibraryError> {
        if from.trim().is_empty() || to.trim().is_empty() {
            return Err(LibraryError::BadFilter("empty value".into()));
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Rename { origin, from, to, steps, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// Trigger a full scan + reconciliation. Serialised with every other
    /// library command by the owning task.
    pub async fn scan(&self) -> Result<ScanOutcome, LibraryError> {
        self.scan_with(false).await
    }

    /// [`scan`](Self::scan) en forçant la ré-analyse offline de tous les
    /// fichiers (`reanalyze = true`), au lieu des seuls non marqués.
    pub async fn scan_with(&self, reanalyze: bool) -> Result<ScanOutcome, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Scan { reanalyze, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// List the media view. `only_available` excludes vanished-but-known rows.
    /// `genres` keeps media carrying at least one of them, case-insensitively
    /// (empty = no filter). A blank genre is a loud `BadFilter`, never a
    /// silently ignored criterion.
    pub async fn list(
        &self,
        only_available: bool,
        genres: Vec<String>,
    ) -> Result<Vec<MediaRow>, LibraryError> {
        if genres.iter().any(|g| g.trim().is_empty()) {
            return Err(LibraryError::BadFilter("empty genre".into()));
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::List { only_available, genres, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// One page of a search (filters, stable sort, cursor). A blank genre is
    /// a loud `BadFilter`, like in [`list`](Self::list).
    pub async fn folders(&self, include_unavailable: bool, after: String, limit: usize) -> Result<media_index::FolderPage, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Command::Folders { include_unavailable, after, limit, reply }).await.map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    pub async fn search(&self, query: SearchQuery) -> Result<SearchPage, LibraryError> {
        if query.genres.iter().any(|g| g.trim().is_empty()) {
            return Err(LibraryError::BadFilter("empty genre".into()));
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Search { query: Box::new(query), reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// Forget the media that vanished from disk (see
    /// `media_index::prune_unavailable`). Serialised with the scans.
    pub async fn prune(&self, seen_before: Option<i64>) -> Result<u64, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Prune { seen_before, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// The standard tags of one file, read from the file itself.
    pub async fn get_tags(&self, rel_path: String) -> Result<TagsRead, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::GetTags { rel_path, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// Write standard tags into one file (see `media_tags`), then refresh its
    /// index row through the plugins' `on_scan`. Serialised with the scans.
    pub async fn set_tags(&self, rel_path: String, revision: String, edit: TagEdit) -> Result<TagsWritten, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::SetTags { rel_path, revision, edit, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }

    /// Genre inventory (case-folded buckets + untagged count), same
    /// `only_available` scope as [`list`](Self::list).
    pub async fn genres(&self, only_available: bool) -> Result<GenreInventory, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Genres { only_available, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }
}

/// Spawn the owning task without plugins: scans are indexed as read (tests,
/// tools).
pub fn spawn(pool: SqlitePool, root: PathBuf) -> LibraryHandle {
    spawn_with(pool, root, None)
}

/// Spawn the owning task and return a handle to it. The task lives until every
/// handle is dropped (the channel closes and the loop ends). `plugins`, when
/// set, runs each scan through the plugins' `on_scan` before indexing.
pub fn spawn_with(pool: SqlitePool, root: PathBuf, plugins: Option<PluginHandle>) -> LibraryHandle {
    spawn_with_analysis(pool, root, plugins, crate::config::AnalysisConfig::default())
}

/// Comme [`spawn_with`], en précisant la config d'analyse offline (`[analysis]`).
pub fn spawn_with_analysis(
    pool: SqlitePool,
    root: PathBuf,
    plugins: Option<PluginHandle>,
    analysis: crate::config::AnalysisConfig,
) -> LibraryHandle {
    let (tx, mut rx) = mpsc::channel::<Command>(32);
    let (status_tx, status) = watch::channel(ScanStatus::default());
    let status_tx = std::sync::Arc::new(status_tx);
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::Reorganize { dry_run, reply } => {
                    let _ = reply.send(crate::library_reorganize::run(&pool, &root, dry_run).await);
                }
                Command::Folders { include_unavailable, after, limit, reply } => {
                    let _ = reply.send(media_index::folders(&pool, include_unavailable, &after, limit).await.map_err(LibraryError::from));
                }
                Command::Scan { reanalyze, reply } => {
                    let _ = reply.send(scan_journaled(&pool, &root, plugins.as_ref(), &status_tx, &analysis, reanalyze).await);
                }
                Command::TagInventory { reply } => {
                    let _ = reply.send(tag_inventory(&pool, plugins.as_ref()).await);
                }
                Command::RenamePreview { origin, from, to, reply } => {
                    let _ = reply.send(rename_preview(&pool, &origin, &from, &to).await);
                }
                Command::Rename { origin, from, to, steps, reply } => {
                    let _ = reply.send(rename(&pool, &root, plugins.as_ref(), &origin, &from, &to, &steps).await);
                }
                Command::List { only_available, genres, reply } => {
                    let out = media_index::list(&pool, only_available, &genres)
                        .await
                        .map_err(LibraryError::from);
                    let _ = reply.send(out);
                }
                Command::Genres { only_available, reply } => {
                    let out = media_index::genres(&pool, only_available)
                        .await
                        .map_err(LibraryError::from);
                    let _ = reply.send(out);
                }
                Command::Prune { seen_before, reply } => {
                    let out = media_index::prune_unavailable(&pool, seen_before).await.map_err(LibraryError::from);
                    let _ = reply.send(out);
                }
                Command::Search { query, reply } => {
                    let out = media_index::search(&pool, &query).await.map_err(LibraryError::from);
                    let _ = reply.send(out);
                }
                Command::GetTags { rel_path, reply } => {
                    let _ = reply.send(get_tags(&pool, &root, plugins.as_ref(), rel_path).await);
                }
                Command::SetTags { rel_path, revision, edit, reply } => {
                    let _ = reply.send(set_tags(&pool, &root, plugins.as_ref(), rel_path, revision, edit).await);
                }
            }
        }
    });
    LibraryHandle { tx, status }
}

type StatusTx = std::sync::Arc<watch::Sender<ScanStatus>>;

/// Résumé d'une passe d'analyse Essentia dans le journal (le log a une ligne
/// par fichier).
fn journal_analysis(tally: &crate::media_analysis::Tally) {
    if tally.analyzed == 0 && tally.failed.is_empty() {
        return;
    }
    tracing::info!(
        analyzed = tally.analyzed,
        failed = tally.failed.len(),
        "media analysis pass complete"
    );
}

/// A scan, its progress published and its outcome in the journal.
async fn scan_journaled(
    pool: &SqlitePool,
    root: &Path,
    plugins: Option<&PluginHandle>,
    status: &StatusTx,
    analysis: &crate::config::AnalysisConfig,
    reanalyze: bool,
) -> Result<ScanOutcome, LibraryError> {
    use crate::events::{record, Code, Component, Level};
    status.send_modify(|s| {
        s.phase = ScanPhase::Listing;
        s.done = 0;
        s.total = 0;
        s.started_at = now_epoch_seconds();
    });
    record(Level::Info, Component::Library, Code::ScanStarted, std::iter::empty::<(&str, &str)>());
    let out = do_scan(pool, root, plugins, Some(status), analysis, reanalyze).await;
    let result = match &out {
        Ok(o) => {
            let c = ScanCounts {
                found: o.report.found() as u64,
                skipped: o.report.skipped_count() as u64,
                present: o.stats.present as u64,
                unavailable: o.stats.unavailable as u64,
                vanished: o.stats.vanished as u64,
            };
            record(
                if c.skipped > 0 { Level::Warn } else { Level::Info },
                Component::Library,
                Code::ScanFinished,
                [
                    ("found", c.found),
                    ("skipped", c.skipped),
                    ("present", c.present),
                    ("unavailable", c.unavailable),
                    ("vanished", c.vanished),
                ],
            );
            Ok(c)
        }
        Err(e) => {
            record(Level::Error, Component::Library, Code::ScanFailed, [("error", e.to_string())]);
            Err(e.to_string())
        }
    };
    status.send_modify(|s| {
        s.phase = ScanPhase::Idle;
        s.last = Some(ScanEnd { finished_at: now_epoch_seconds(), result });
    });
    out
}

fn phase(status: Option<&StatusTx>, phase: ScanPhase) {
    if let Some(s) = status {
        s.send_modify(|s| s.phase = phase);
    }
}

/// The file's own genres (`TCON`), before the plugins add theirs.
fn file_genres(report: &ScanReport) -> std::collections::BTreeMap<String, Vec<String>> {
    report.media.iter().map(|m| (m.rel_path.clone(), m.genres.clone())).collect()
}

/// Where each value comes from: the file's genres (origin `""`), then the
/// values of the user frames the `custom-tags` plugin reads as genres
/// (origin = the source's name as declared).
fn tag_values(
    report: &ScanReport,
    file_genres: &std::collections::BTreeMap<String, Vec<String>>,
    sources: &[String],
) -> media_index::TagValues {
    let mut out = media_index::TagValues::new();
    for m in &report.media {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for g in file_genres.get(&m.rel_path).into_iter().flatten() {
            pairs.push((String::new(), g.clone()));
        }
        for t in report.custom_tags.get(&m.rel_path).into_iter().flatten() {
            if let Some(src) = sources.iter().find(|s| s.trim().eq_ignore_ascii_case(t.name.trim())) {
                pairs.push((src.trim().to_string(), t.value.clone()));
            }
        }
        out.insert(m.rel_path.clone(), pairs);
    }
    out
}

async fn sources_of(plugins: Option<&PluginHandle>) -> Vec<String> {
    match plugins {
        Some(p) => p.tag_hints().await.genre_sources,
        None => Vec::new(),
    }
}

async fn tag_inventory(
    pool: &SqlitePool,
    plugins: Option<&PluginHandle>,
) -> Result<Vec<media_index::OriginInventory>, LibraryError> {
    let mut origins = vec![String::new()];
    origins.extend(sources_of(plugins).await.into_iter().map(|s| s.trim().to_string()));
    Ok(media_index::tag_inventory(pool, &origins).await?)
}

/// Names of the playlists whose `genre` filters name `value` (case folded).
async fn playlists_naming(pool: &SqlitePool, value: &str) -> Result<Vec<String>, LibraryError> {
    let key = media_index::genre_key(value);
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT name, toml FROM playlists ORDER BY name").fetch_all(pool).await?;
    let names = |v: &toml::Value| -> bool {
        match v {
            toml::Value::String(s) => media_index::genre_key(s) == key,
            toml::Value::Array(a) => a.iter().any(|x| x.as_str().is_some_and(|s| media_index::genre_key(s) == key)),
            _ => false,
        }
    };
    let mut out = Vec::new();
    for (name, text) in rows {
        let Ok(p) = crate::playlist::Playlist::parse(&text) else { continue };
        if p.selection.filter.iter().any(|f| f.field == "genre" && names(&f.value)) {
            out.push(name);
        }
    }
    Ok(out)
}

async fn rename_preview(pool: &SqlitePool, origin: &str, from: &str, to: &str) -> Result<RenamePreview, LibraryError> {
    let files = media_index::files_with_value(pool, origin, from).await?;
    let merges = media_index::genre_key(from) != media_index::genre_key(to)
        && !media_index::files_with_value(pool, origin, to).await?.is_empty();
    Ok(RenamePreview { files, playlists: playlists_naming(pool, from).await?, merges })
}

/// `values` with every spelling of `from` replaced by `to`, once (a value
/// already there is not doubled). `None` = `from` is not among them.
fn renamed(values: &[String], from: &str, to: &str) -> Option<Vec<String>> {
    let key = media_index::genre_key(from);
    if !values.iter().any(|v| media_index::genre_key(v) == key) {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let v = if media_index::genre_key(v) == key { to.trim().to_string() } else { v.clone() };
        if !out.iter().any(|x| media_index::genre_key(x) == media_index::genre_key(&v)) {
            out.push(v);
        }
    }
    Some(out)
}

async fn rename(
    pool: &SqlitePool,
    root: &Path,
    plugins: Option<&PluginHandle>,
    origin: &str,
    from: &str,
    to: &str,
    steps: &mpsc::Sender<RenameStep>,
) -> Result<(), LibraryError> {
    use crate::events::{record, Code, Component, Level};
    let files = media_index::files_with_value(pool, origin, from).await?;
    // The watcher may have gone: the rename carries on regardless.
    let _ = steps.send(RenameStep::Started { files: files.len() as u64 }).await;
    let (mut changed, mut unchanged, mut failed) = (0u64, 0u64, 0u64);
    for rel_path in files {
        let outcome = rename_one(pool, root, plugins, origin, from, to, &rel_path).await;
        let error = match outcome {
            Ok(true) => {
                changed += 1;
                None
            }
            Ok(false) => {
                unchanged += 1;
                None
            }
            Err(e) => {
                failed += 1;
                Some(e.to_string())
            }
        };
        let _ = steps.send(RenameStep::File { rel_path, error }).await;
    }
    record(
        if failed > 0 { Level::Warn } else { Level::Info },
        Component::Library,
        Code::TagRenamed,
        [
            ("origin", origin.to_string()),
            ("from", from.to_string()),
            ("to", to.to_string()),
            ("files", changed.to_string()),
            ("failed", failed.to_string()),
        ],
    );
    let _ = steps.send(RenameStep::Finished { changed, unchanged, failed }).await;
    Ok(())
}

/// One file of a rename: read, change the value where it is written, write
/// with the revision just read. `Ok(false)` = the value was no longer there.
async fn rename_one(
    pool: &SqlitePool,
    root: &Path,
    plugins: Option<&PluginHandle>,
    origin: &str,
    from: &str,
    to: &str,
    rel_path: &str,
) -> Result<bool, LibraryError> {
    let read = get_tags(pool, root, plugins, rel_path.to_string()).await?;
    let mut edit = TagEdit::default();
    if origin.is_empty() {
        let Some(genres) = renamed(&read.tags.genres, from, to) else { return Ok(false) };
        edit.genres = Some(genres);
    } else {
        let Some(values) = renamed(&read.tags.user_values(origin), from, to) else { return Ok(false) };
        edit.user.insert(origin.to_string(), values);
    }
    let written = set_tags(pool, root, plugins, rel_path.to_string(), read.revision, edit).await?;
    if written.conflict {
        // Changed between the read and the write (by someone else): say so.
        return Err(LibraryError::Tags(TagError::Io(format!("{rel_path} changed during the rename: not written"))));
    }
    Ok(true)
}

fn now_epoch_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Hand the scanned batch to the plugins' `on_scan` and fold the extra genres
/// they return into each media's genre set (case-insensitive dedup, the tag
/// spelling wins). Never fails: a failing plugin is recorded on its side and
/// simply contributes nothing.
async fn enrich(report: &mut ScanReport, plugins: &PluginHandle) {
    let inputs: Vec<ScanInput> = report
        .media
        .iter()
        .map(|m| ScanInput {
            rel_path: m.rel_path.clone(),
            artist: m.artist.clone(),
            title: m.title.clone(),
            album: m.album.clone(),
            year: m.year,
            duration_ms: m.duration_ms,
            genres: m.genres.clone(),
            custom_tags: report.custom_tags.get(&m.rel_path).cloned().unwrap_or_default(),
        })
        .collect();
    let extras = plugins.on_scan(inputs).await;
    apply_extras(report, &extras);
}

/// Fold the plugins' extra genres into the report (pure; tested directly).
fn apply_extras(report: &mut ScanReport, extras: &crate::plugin::ScanExtras) {
    for m in report.media.iter_mut() {
        let Some(add) = extras.get(&m.rel_path) else { continue };
        let metadata = report.metadata.entry(m.rel_path.clone()).or_default();
        for (key, value) in &add.metadata { metadata.entry(key.clone()).or_insert_with(|| value.clone()); }
        for g in &add.genres {
            let key = media_index::genre_key(g);
            if !m.genres.iter().any(|x| media_index::genre_key(x) == key) {
                m.genres.push(g.clone());
            }
        }
    }
}

/// Manual values written by a tag editor (`TXXX:tempo_manual`,
/// `TXXX:creation_manual`) win over what the plugins derived: they become
/// the media's `tempo` / `creation`, and the scan writes them back into
/// `TXXX:tempo` / `TXXX:creation` like a derived value.
fn apply_manual(report: &mut ScanReport) {
    for m in &report.media {
        let Some(tags) = report.custom_tags.get(&m.rel_path) else { continue };
        let first = |name: &str| {
            tags.iter()
                .find(|t| t.name.trim().eq_ignore_ascii_case(name) && !t.value.trim().is_empty())
                .map(|t| t.value.trim().to_string())
        };
        if let Some(v) = first(media_tags::TEMPO_MANUAL) {
            report.metadata.entry(m.rel_path.clone()).or_default().insert("tempo".into(), v);
        }
        if let Some(v) = first(media_tags::CREATION_MANUAL).filter(|v| v.parse::<jiff::Timestamp>().is_ok()) {
            report.metadata.entry(m.rel_path.clone()).or_default().insert("creation".into(), v);
        }
    }
}

async fn get_tags(
    pool: &SqlitePool,
    root: &Path,
    plugins: Option<&PluginHandle>,
    rel_path: String,
) -> Result<TagsRead, LibraryError> {
    let root = root.to_path_buf();
    let rel = rel_path.clone();
    let mut read = tokio::task::spawn_blocking(move || -> Result<TagsRead, LibraryError> {
        let full = media_tags::resolve(&root, &rel)?;
        let (tags, size) = media_tags::read(&full)?;
        Ok(TagsRead { revision: media_tags::revision(&tags, size), tags, ..TagsRead::default() })
    })
    .await
    .map_err(|e| LibraryError::Join(e.to_string()))??;
    let hints = match plugins {
        Some(p) => p.tag_hints().await,
        None => Default::default(),
    };
    let meta = media_index::meta_of(pool, &rel_path).await?;
    let mut choices = hints.tempo_labels;
    for v in media_index::meta_values(pool, "tempo").await? {
        if !choices.contains(&v) {
            choices.push(v);
        }
    }
    read.genre_sources = hints.genre_sources;
    read.tempo_choices = choices;
    read.tempo = meta.get("tempo").cloned();
    read.creation = meta.get("creation").cloned();
    Ok(read)
}

/// After a tag edit: the file read again as a scan would (plugins, manual
/// values, write-back of `TXXX:tempo` / `TXXX:creation`), then its index
/// row, genres and `media_meta` replaced.
async fn refresh_file(
    pool: &SqlitePool,
    root: &Path,
    plugins: Option<&PluginHandle>,
    mut report: ScanReport,
) -> Result<Option<MediaRow>, LibraryError> {
    let values = tag_values(&report, &file_genres(&report), &sources_of(plugins).await);
    if let Some(plugins) = plugins {
        enrich(&mut report, plugins).await;
    }
    apply_manual(&mut report);
    let write_root = root.to_path_buf();
    let report = tokio::task::spawn_blocking(move || {
        crate::scan_writeback::apply(&write_root, &mut report)?;
        Ok::<_, TagError>(report)
    })
    .await
    .map_err(|e| LibraryError::Join(e.to_string()))??;
    let Some(m) = report.media.first() else { return Ok(None) };
    let meta = report.metadata.get(&m.rel_path).cloned().unwrap_or_default();
    let row = media_index::refresh_one_with(pool, m, Some(&meta), now_epoch_seconds()).await?;
    media_index::replace_tag_values(pool, &values, false).await?;
    Ok(row)
}

async fn set_tags(
    pool: &SqlitePool,
    root: &Path,
    plugins: Option<&PluginHandle>,
    rel_path: String,
    revision: String,
    edit: TagEdit,
) -> Result<TagsWritten, LibraryError> {
    let root_buf = root.to_path_buf();
    let rel = rel_path.clone();
    // Write + read back + re-read the file as a scan would, off the runtime.
    let written = tokio::task::spawn_blocking(move || -> Result<Result<(TagsRead, ScanReport), TagsRead>, LibraryError> {
        let full = media_tags::resolve(&root_buf, &rel)?;
        match media_tags::write(&full, &rel, &revision, &edit) {
            Ok((tags, revision)) => {
                let report = media::scan_file(&root_buf, &full)
                    .map_err(|e| LibraryError::Tags(TagError::Io(format!("{rel} written but not readable: {e:?}"))))?;
                Ok(Ok((TagsRead { tags, revision, ..TagsRead::default() }, report)))
            }
            Err(TagError::Conflict { current, .. }) => {
                let (tags, _) = media_tags::read(&full)?;
                Ok(Err(TagsRead { tags, revision: current, ..TagsRead::default() }))
            }
            Err(e) if e.file_touched() => {
                // Partly written: the index follows the file, then the error is said.
                Err(LibraryError::Touched(e, media::scan_file(&root_buf, &full).ok()))
            }
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| LibraryError::Join(e.to_string()))?;
    let written = match written {
        Err(LibraryError::Touched(e, Some(report))) => {
            refresh_file(pool, root, plugins, report).await?;
            return Err(LibraryError::Tags(e));
        }
        Err(LibraryError::Touched(e, None)) => return Err(LibraryError::Tags(e)),
        other => other?,
    };
    let (_, report) = match written {
        Ok(x) => x,
        Err(current) => return Ok(TagsWritten { conflict: true, tags: current, row: None }),
    };
    let row = refresh_file(pool, root, plugins, report).await?;
    tracing::info!(media = %rel_path, "media tags written");
    crate::events::record(
        crate::events::Level::Info,
        crate::events::Component::Library,
        crate::events::Code::TagsWritten,
        [("media", rel_path.clone())],
    );
    // What the editor shows next: the file as it is now (write-back included).
    let tags = get_tags(pool, root, plugins, rel_path).await?;
    Ok(TagsWritten { conflict: false, tags, row })
}

/// Run the blocking scan off the async runtime, let the plugins enrich it,
/// then reconcile the view. The `??` unwraps two layers: the `spawn_blocking`
/// join, then the scan's own `Result` (a bad root → `LibraryError::BadRoot`).
async fn do_scan(
    pool: &SqlitePool,
    root: &Path,
    plugins: Option<&PluginHandle>,
    status: Option<&StatusTx>,
    analysis: &crate::config::AnalysisConfig,
    reanalyze: bool,
) -> Result<ScanOutcome, LibraryError> {
    let write_root = root.to_path_buf();
    let root = root.to_path_buf();
    let progress_tx = status.cloned();
    let mut report: ScanReport = tokio::task::spawn_blocking(move || {
        media::scan_library_with(&root, &mut |done, total| {
            if let Some(s) = &progress_tx {
                s.send_modify(|s| {
                    s.phase = ScanPhase::Reading;
                    s.done = done as u64;
                    s.total = total as u64;
                });
            }
        })
    })
    .await
    .map_err(|e| LibraryError::Join(e.to_string()))??;
    let values = tag_values(&report, &file_genres(&report), &sources_of(plugins).await);
    if let Some(plugins) = plugins {
        phase(status, ScanPhase::Plugins);
        enrich(&mut report, plugins).await;
    }
    // Analyse média offline (Essentia) — cœur, indépendante des plugins. Opt-in
    // (STATIOND_ANALYSIS). Remplit report.metadata pour les fichiers non encore
    // marqués ; scan_writeback écrira les frames, la table typée suivra.
    if analysis.enabled || reanalyze {
        phase(status, ScanPhase::Analyzing);
        let analyzer = crate::media_analysis::EssentiaExtractor {
            exe: analysis
                .extractor
                .clone()
                .map(std::ffi::OsString::from)
                .unwrap_or_else(crate::media_analysis::EssentiaExtractor::default_exe),
            profile: analysis.profile.clone(),
            timeout: std::time::Duration::from_secs(analysis.timeout_secs),
        };
        let analysis_root = write_root.clone();
        let progress_tx = status.cloned();
        let max_per_scan = analysis.max_per_scan;
        let (r, tally) = tokio::task::spawn_blocking(move || {
            let tally = crate::media_analysis::analyze_pending(
                &analysis_root,
                &mut report,
                &analyzer,
                reanalyze,
                max_per_scan,
                &mut |done, total| {
                    if let Some(s) = &progress_tx {
                        s.send_modify(|s| {
                            s.done = done as u64;
                            s.total = total as u64;
                        });
                    }
                },
            );
            (report, tally)
        })
        .await
        .map_err(|e| LibraryError::Join(e.to_string()))?;
        report = r;
        journal_analysis(&tally);
    }
    apply_manual(&mut report);
    phase(status, ScanPhase::Writing);
    // Host-owned MP3 writes run off the async runtime, after enrichment.
    let (report, originals) = tokio::task::spawn_blocking(move || {
        let originals = crate::scan_writeback::apply(&write_root, &mut report)?;
        Ok::<_, TagError>((report, originals))
    }).await.map_err(|e| LibraryError::Join(e.to_string()))??;
    phase(status, ScanPhase::Indexing);
    let stats = media_index::replace_library_with_writeback(
        pool, &report.media, &report.metadata, &originals, now_epoch_seconds(),
    ).await?;
    // Cache typé d'analyse. Source de vérité = les tags :
    //  - reconstitution : lue depuis report.custom_tags (ce que le scan a relu
    //    du disque) — repeuple la table sur une VM neuve sans réanalyse ;
    //  - fraîche ce scan : lue depuis report.metadata, mais SEULEMENT pour les
    //    fichiers que scan_writeback a effectivement écrits (présents dans
    //    `originals`), pour ne jamais devancer ce qui est réellement dans le tag.
    // Un fichier sans marqueur d'analyse valide n'y figure pas.
    let analysis: std::collections::BTreeMap<String, (crate::media_analysis::Analysis, String)> = report
        .media
        .iter()
        .filter_map(|m| {
            let fresh = if originals.contains_key(&m.rel_path) {
                report.metadata.get(&m.rel_path).and_then(|meta| {
                    let tags: Vec<crate::media::CustomTag> = meta
                        .iter()
                        .map(|(name, value)| crate::media::CustomTag { name: name.clone(), value: value.clone() })
                        .collect();
                    crate::media_analysis::Analysis::from_tags(&tags)
                })
            } else {
                None
            };
            fresh
                .or_else(|| {
                    report
                        .custom_tags
                        .get(&m.rel_path)
                        .and_then(|tags| crate::media_analysis::Analysis::from_tags(tags))
                })
                .map(|av| (m.rel_path.clone(), av))
        })
        .collect();
    media_index::replace_analysis(pool, &analysis, now_epoch_seconds()).await?;
    media_index::replace_tag_values(pool, &values, true).await?;
    Ok(ScanOutcome { report, stats })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn scan_empty_dir_then_list_is_empty() {
        let db_dir = tempfile::tempdir().unwrap();
        let media_dir = tempfile::tempdir().unwrap();
        let pool = db::init(&db_dir.path().join("t.db")).await.unwrap();
        let lib = spawn(pool, media_dir.path().to_path_buf());

        let outcome = lib.scan().await.unwrap();
        assert_eq!(outcome.report.found(), 0);
        assert_eq!(outcome.stats.present, 0);
        assert!(lib.list(true, vec![]).await.unwrap().is_empty());
        assert!(lib.genres(true).await.unwrap().genres.is_empty());
    }

    #[tokio::test]
    async fn scan_missing_root_surfaces_bad_root_through_the_channel() {
        let db_dir = tempfile::tempdir().unwrap();
        let pool = db::init(&db_dir.path().join("t.db")).await.unwrap();
        // A path under a real temp dir that does not exist: missing cross-platform.
        let missing = db_dir.path().join("nope/nested/missing");
        let lib = spawn(pool, missing);
        assert!(matches!(lib.scan().await, Err(LibraryError::BadRoot(_))));
    }

    #[tokio::test]
    async fn plugin_genres_are_merged_and_indexed_in_media_genre() {
        let db_dir = tempfile::tempdir().unwrap();
        let pool = db::init(&db_dir.path().join("t.db")).await.unwrap();
        let m = |rel: &str, genres: &[&str]| media::ScannedMedia {
            rel_path: rel.into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: genres.iter().map(|g| g.to_string()).collect(),
            duration_ms: 1000,
            size_bytes: 1,
            mtime_ns: 1,
        };
        let mut report = ScanReport {
            media: vec![m("talk.mp3", &["Talks"]), m("song.mp3", &["Rock"])],
            ..Default::default()
        };
        let mut extras = crate::plugin::ScanExtras::new();
        report.metadata.insert("talk.mp3".into(), [("bpm".into(), "70".into())].into());
        extras.insert("talk.mp3".into(), crate::plugin::ScanAddition {
            genres: vec!["talks".into(), "news".into()],
            metadata: [("tempo".into(), "slow".into())].into(),
        });
        apply_extras(&mut report, &extras);
        assert_eq!(report.metadata["talk.mp3"]["tempo"], "slow");
        assert_eq!(report.metadata["talk.mp3"]["bpm"], "70");
        assert_eq!(report.media[1].genres, vec!["Rock".to_string()], "untouched");
        assert_eq!(
            report.media[0].genres,
            vec!["Talks".to_string(), "news".to_string()],
            "case-insensitive dedup, tag spelling wins"
        );

        media_index::replace_library(&pool, &report.media, 0).await.unwrap();
        let rows = media_index::list(&pool, true, &["NEWS".to_string()]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rel_path, "talk.mp3");
    }

    #[tokio::test]
    async fn blank_genre_filter_is_rejected() {
        let db_dir = tempfile::tempdir().unwrap();
        let media_dir = tempfile::tempdir().unwrap();
        let pool = db::init(&db_dir.path().join("t.db")).await.unwrap();
        let lib = spawn(pool, media_dir.path().to_path_buf());
        assert!(matches!(
            lib.list(true, vec!["  ".into()]).await,
            Err(LibraryError::BadFilter(_))
        ));
    }

    fn wav(path: &Path) {
        use std::io::Write;
        let n: u32 = 8000;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + n).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        for v in [16u32.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec()] {
            f.write_all(&v).unwrap();
        }
        f.write_all(&8000u32.to_le_bytes()).unwrap();
        f.write_all(&8000u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&8u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&n.to_le_bytes()).unwrap();
        f.write_all(&vec![128u8; n as usize]).unwrap();
    }

    #[tokio::test]
    async fn set_tags_writes_the_file_refreshes_the_row_and_keeps_played_episodes() {
        let db_dir = tempfile::tempdir().unwrap();
        let media_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(media_dir.path().join("Pod")).unwrap();
        wav(&media_dir.path().join("Pod/ep1.wav"));
        let pool = db::init(&db_dir.path().join("t.db")).await.unwrap();
        let lib = spawn(pool.clone(), media_dir.path().to_path_buf());
        lib.scan().await.unwrap();
        let (size, mtime): (i64, i64) =
            sqlx::query_as("SELECT size_bytes, mtime_ns FROM media WHERE rel_path = 'Pod/ep1.wav'")
                .fetch_one(&pool)
                .await
                .unwrap();
        crate::episode_play::mark(&pool, "pod", "Pod/ep1.wav", size, mtime, crate::resolver::Epoch(1)).await.unwrap();

        let read = lib.get_tags("Pod/ep1.wav".into()).await.unwrap();
        assert_eq!(read.tags, StandardTags::default());
        let edit = TagEdit { title: Some(Some("Épisode 1".into())), genres: Some(vec!["talks".into()]), ..Default::default() };
        let w = lib.set_tags("Pod/ep1.wav".into(), read.revision.clone(), edit.clone()).await.unwrap();
        assert!(!w.conflict);
        let row = w.row.unwrap();
        assert_eq!(row.title.as_deref(), Some("Épisode 1"));
        assert_eq!(row.genres, vec!["talks".to_string()]);
        // The played-episode guard followed the file (not a new episode).
        let (gs, gm): (i64, i64) = sqlx::query_as("SELECT size_bytes, mtime_ns FROM episode_play WHERE rel_path = 'Pod/ep1.wav'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let (ns, nm): (i64, i64) = sqlx::query_as("SELECT size_bytes, mtime_ns FROM media WHERE rel_path = 'Pod/ep1.wav'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((gs, gm), (ns, nm));
        assert_eq!(nm, mtime, "mtime kept");
        // The old revision now conflicts; the current tags come back.
        let again = lib.set_tags("Pod/ep1.wav".into(), read.revision, edit).await.unwrap();
        assert!(again.conflict && again.row.is_none());
        assert_eq!(again.tags.tags.title.as_deref(), Some("Épisode 1"));
        // Manual tempo / creation: they reach media_meta (no plugin needed)
        // and GetTags shows them as effective.
        let now = lib.get_tags("Pod/ep1.wav".into()).await.unwrap();
        let mut user = std::collections::BTreeMap::new();
        user.insert(media_tags::TEMPO_MANUAL.to_string(), vec!["lent".to_string()]);
        user.insert(media_tags::CREATION_MANUAL.to_string(), vec!["2026-06-14T08:36:48+02:00".to_string()]);
        let w = lib.set_tags("Pod/ep1.wav".into(), now.revision, TagEdit { user, ..Default::default() }).await.unwrap();
        assert_eq!(w.tags.tempo.as_deref(), Some("lent"));
        assert_eq!(w.tags.creation.as_deref(), Some("2026-06-14T06:36:48.000000000Z"), "normalisée en UTC");
        let meta = media_index::meta_of(&pool, "Pod/ep1.wav").await.unwrap();
        assert_eq!(meta.get("tempo").map(String::as_str), Some("lent"));
        // A full scan keeps them (read back from the file).
        lib.scan().await.unwrap();
        assert_eq!(media_index::meta_of(&pool, "Pod/ep1.wav").await.unwrap().get("tempo").map(String::as_str), Some("lent"));
        // Removing the manual tempo: nothing derived here, so no tempo.
        let now = lib.get_tags("Pod/ep1.wav".into()).await.unwrap();
        assert_eq!(now.tempo_choices, ["lent"], "libellés déjà utilisés proposés");
        let mut user = std::collections::BTreeMap::new();
        user.insert(media_tags::TEMPO_MANUAL.to_string(), vec![]);
        let w = lib.set_tags("Pod/ep1.wav".into(), now.revision, TagEdit { user, ..Default::default() }).await.unwrap();
        assert_eq!(w.tags.tempo, None);
        // Outside the root: not found.
        assert!(matches!(lib.get_tags("../x.wav".into()).await, Err(LibraryError::Tags(TagError::NotFound(_)))));
    }

    /// Give `rel` the file genres `genres`.
    async fn genres_of(lib: &LibraryHandle, rel: &str, genres: &[&str]) {
        let read = lib.get_tags(rel.into()).await.unwrap();
        let edit = TagEdit { genres: Some(genres.iter().map(|g| g.to_string()).collect()), ..Default::default() };
        assert!(!lib.set_tags(rel.into(), read.revision, edit).await.unwrap().conflict);
    }

    #[tokio::test]
    async fn values_are_counted_per_origin_and_renamed_file_by_file() {
        let db_dir = tempfile::tempdir().unwrap();
        let media_dir = tempfile::tempdir().unwrap();
        for f in ["a.wav", "b.wav", "c.wav"] {
            wav(&media_dir.path().join(f));
        }
        let pool = db::init(&db_dir.path().join("t.db")).await.unwrap();
        let lib = spawn(pool.clone(), media_dir.path().to_path_buf());
        let status = lib.watch_scan();
        lib.scan().await.unwrap();
        {
            let s = status.borrow();
            assert_eq!(s.phase, ScanPhase::Idle);
            assert_eq!((s.done, s.total), (3, 3));
            assert!(matches!(&s.last, Some(ScanEnd { result: Ok(c), .. }) if c.found == 3));
        }
        genres_of(&lib, "a.wav", &["Jazz", "Soul"]).await;
        genres_of(&lib, "b.wav", &["jazz"]).await;
        genres_of(&lib, "c.wav", &["Swing"]).await;
        sqlx::query("INSERT INTO playlists (id, name, toml) VALUES ('p1', 'Du jazz', ?1)")
            .bind("name = \"Du jazz\"\n[selection]\nmode = \"dynamic\"\n[[selection.filter]]\nfield = \"genre\"\nop = \"has\"\nvalue = \"JAZZ\"\n")
            .execute(&pool)
            .await
            .unwrap();

        // The file's genre: every value, all spellings together, and who has none.
        let inv = lib.tag_inventory().await.unwrap();
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].origin, "");
        assert_eq!(inv[0].without, 0);
        let jazz = inv[0].values.iter().find(|v| v.value.eq_ignore_ascii_case("jazz")).unwrap();
        assert_eq!((jazz.count, jazz.spellings.len()), (2, 2));

        // Preview: two files, the playlist that filters on it, a merge.
        let p = lib.rename_preview(String::new(), "jazz".into(), "swing".into()).await.unwrap();
        assert_eq!(p.files, ["a.wav", "b.wav"]);
        assert_eq!(p.playlists, ["Du jazz"]);
        assert!(p.merges);

        // Rename: each file told, then the tally; values merged, not doubled.
        let (tx, mut rx) = mpsc::channel(16);
        lib.rename(String::new(), "jazz".into(), "Swing".into(), tx).await.unwrap();
        let mut steps = Vec::new();
        while let Some(s) = rx.recv().await {
            steps.push(s);
        }
        assert_eq!(steps.first(), Some(&RenameStep::Started { files: 2 }));
        assert_eq!(steps.last(), Some(&RenameStep::Finished { changed: 2, unchanged: 0, failed: 0 }));
        assert_eq!(lib.get_tags("a.wav".into()).await.unwrap().tags.genres, ["Swing", "Soul"]);
        assert_eq!(lib.get_tags("b.wav".into()).await.unwrap().tags.genres, ["Swing"]);
        let inv = lib.tag_inventory().await.unwrap();
        let names: Vec<(&str, usize)> = inv[0].values.iter().map(|v| (v.value.as_str(), v.count)).collect();
        assert_eq!(names, [("Soul", 1), ("Swing", 3)]);
        // The index (what playlists see) followed.
        assert_eq!(media_index::list(&pool, true, &["jazz".into()]).await.unwrap().len(), 0);
        assert_eq!(media_index::list(&pool, true, &["swing".into()]).await.unwrap().len(), 3);
    }

    #[test]
    fn a_rename_replaces_every_spelling_once() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(renamed(&v(&["Jazz", "soul", "JAZZ"]), "jazz", "Swing"), Some(v(&["Swing", "soul"])));
        assert_eq!(renamed(&v(&["Swing", "jazz"]), "jazz", "swing"), Some(v(&["Swing"])));
        assert_eq!(renamed(&v(&["soul"]), "jazz", "Swing"), None);
    }
}

