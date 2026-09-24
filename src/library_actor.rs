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
use crate::media_index::{self, GenreInventory, MediaRow, ReplaceStats};
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
}
