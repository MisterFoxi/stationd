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
use tokio::sync::{mpsc, oneshot};

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
    Scan {
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
}

/// The standard tags of a file and their revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagsRead {
    pub tags: StandardTags,
    pub revision: String,
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
}

impl LibraryHandle {
    /// Trigger a full scan + reconciliation. Serialised with every other
    /// library command by the owning task.
    pub async fn scan(&self) -> Result<ScanOutcome, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Scan { reply })
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
    let (tx, mut rx) = mpsc::channel::<Command>(32);
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::Scan { reply } => {
                    let _ = reply.send(do_scan(&pool, &root, plugins.as_ref()).await);
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
                    let _ = reply.send(get_tags(&root, rel_path).await);
                }
                Command::SetTags { rel_path, revision, edit, reply } => {
                    let _ = reply.send(set_tags(&pool, &root, plugins.as_ref(), rel_path, revision, edit).await);
                }
            }
        }
    });
    LibraryHandle { tx }
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
        for g in add {
            let key = media_index::genre_key(g);
            if !m.genres.iter().any(|x| media_index::genre_key(x) == key) {
                m.genres.push(g.clone());
            }
        }
    }
}

async fn get_tags(root: &Path, rel_path: String) -> Result<TagsRead, LibraryError> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let full = media_tags::resolve(&root, &rel_path)?;
        let (tags, size) = media_tags::read(&full)?;
        Ok(TagsRead { revision: media_tags::revision(&tags, size), tags })
    })
    .await
    .map_err(|e| LibraryError::Join(e.to_string()))?
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
                Ok(Ok((TagsRead { tags, revision }, report)))
            }
            Err(TagError::Conflict { current, .. }) => {
                let (tags, _) = media_tags::read(&full)?;
                Ok(Err(TagsRead { tags, revision: current }))
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
        Err(LibraryError::Touched(e, Some(mut report))) => {
            if let Some(plugins) = plugins {
                enrich(&mut report, plugins).await;
            }
            if let Some(m) = report.media.first() {
                media_index::refresh_one(pool, m, now_epoch_seconds()).await?;
            }
            return Err(LibraryError::Tags(e));
        }
        Err(LibraryError::Touched(e, None)) => return Err(LibraryError::Tags(e)),
        other => other?,
    };
    let (tags, mut report) = match written {
        Ok(x) => x,
        Err(current) => return Ok(TagsWritten { conflict: true, tags: current, row: None }),
    };
    if let Some(plugins) = plugins {
        enrich(&mut report, plugins).await;
    }
    let row = match report.media.first() {
        Some(m) => media_index::refresh_one(pool, m, now_epoch_seconds()).await?,
        None => None,
    };
    tracing::info!(media = %rel_path, "media tags written");
    Ok(TagsWritten { conflict: false, tags, row })
}

/// Run the blocking scan off the async runtime, let the plugins enrich it,
/// then reconcile the view. The `??` unwraps two layers: the `spawn_blocking`
/// join, then the scan's own `Result` (a bad root → `LibraryError::BadRoot`).
async fn do_scan(
    pool: &SqlitePool,
    root: &Path,
    plugins: Option<&PluginHandle>,
) -> Result<ScanOutcome, LibraryError> {
    let root = root.to_path_buf();
    let mut report: ScanReport = tokio::task::spawn_blocking(move || media::scan_library(&root))
        .await
        .map_err(|e| LibraryError::Join(e.to_string()))??;
    if let Some(plugins) = plugins {
        enrich(&mut report, plugins).await;
    }
    let stats = media_index::replace_library(pool, &report.media, now_epoch_seconds()).await?;
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
        extras.insert("talk.mp3".into(), vec!["talks".into(), "news".into()]);
        apply_extras(&mut report, &extras);
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
        let edit = TagEdit { title: Some(Some("Épisode 1".into())), genre: Some(Some("talks".into())), ..Default::default() };
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
        // Outside the root: not found.
        assert!(matches!(lib.get_tags("../x.wav".into()).await, Err(LibraryError::Tags(TagError::NotFound(_)))));
    }
}
