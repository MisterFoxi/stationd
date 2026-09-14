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
use crate::media_index::{self, MediaRow, ReplaceStats};

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
        reply: oneshot::Sender<Result<Vec<MediaRow>, LibraryError>>,
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
    pub async fn list(&self, only_available: bool) -> Result<Vec<MediaRow>, LibraryError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::List { only_available, reply })
            .await
            .map_err(|_| LibraryError::ActorGone)?;
        rx.await.map_err(|_| LibraryError::ActorGone)?
    }
}

/// Spawn the owning task and return a handle to it. The task lives until every
/// handle is dropped (the channel closes and the loop ends).
pub fn spawn(pool: SqlitePool, root: PathBuf) -> LibraryHandle {
    let (tx, mut rx) = mpsc::channel::<Command>(32);
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::Scan { reply } => {
                    let _ = reply.send(do_scan(&pool, &root).await);
                }
                Command::List { only_available, reply } => {
                    let out = media_index::list(&pool, only_available)
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

/// Run the blocking scan off the async runtime, then reconcile the view. The
/// `??` unwraps two layers: the `spawn_blocking` join, then the scan's own
/// `Result` (a bad root → `LibraryError::BadRoot`).
async fn do_scan(pool: &SqlitePool, root: &Path) -> Result<ScanOutcome, LibraryError> {
    let root = root.to_path_buf();
    let report: ScanReport = tokio::task::spawn_blocking(move || media::scan_library(&root))
        .await
        .map_err(|e| LibraryError::Join(e.to_string()))??;
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
        assert!(lib.list(true).await.unwrap().is_empty());
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
}
