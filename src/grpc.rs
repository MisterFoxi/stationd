use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sqlx::SqlitePool;
use tokio::sync::{oneshot, Mutex};
use tonic::{Request, Response, Status as TonicStatus};

// Keep the existing public path available to callers.
pub use crate::proto::station;

use station::station_server::Station;
use station::{
    PlaylistAddReply, PlaylistAddRequest, PlaylistExportReply, PlaylistExportRequest,
    PlaylistListReply, PlaylistListRequest, PlaylistReloadReply, PlaylistReloadRequest,
    PlaylistRemoveReply, PlaylistRemoveRequest, PlaylistSummary, PlaylistSyncError,
    PlaylistSyncReply, PlaylistSyncRequest, QuitReply, QuitRequest,
    ShutdownReply, ShutdownRequest, StatusReply, StatusRequest,
};

use crate::playlist::{self, Playlist};
use crate::store;

/// Implementation of the `Station` gRPC service.
///
/// Status, `quit` (restart-style exit), the operator's stop (`shutdown`,
/// which parks Liquidsoap through its control socket before exiting — see
/// `operator_stop`) and the playlist view.
/// Future RPCs (playlists, roles...) will grow this same service over time,
/// starting in proto/station.proto.
pub struct StationService {
    station_name: String,
    timezone: String,
    started_at: Instant,
    db: SqlitePool,
    playlist_root: PathBuf,
    // A `oneshot::Sender` only fires once. The `Mutex<Option<_>>` lets us
    // "consume" it (`.take()`) on the first `quit` received without
    // panicking if a misbehaving client calls `quit` twice.
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// The operator's stop (`Shutdown`): `None` = not wired (refused).
    operator_stop: Option<OperatorStop>,
}

/// What `Shutdown` needs: the station control (DJ on air, halt every
/// boundary), the Liquidsoap control socket (kick, park, flush) when wired,
/// and where the marker goes.
#[derive(Clone)]
pub struct OperatorStop {
    pub control: crate::station_control::StationControl,
    pub ls: Option<crate::ls_control::LsControl>,
    pub marker: PathBuf,
}

impl StationService {
    pub fn new(
        station_name: String,
        timezone: String,
        db: SqlitePool,
        playlist_root: PathBuf,
        shutdown: oneshot::Sender<()>,
    ) -> Self {
        Self {
            station_name,
            timezone,
            started_at: Instant::now(),
            db,
            playlist_root,
            shutdown: Arc::new(Mutex::new(Some(shutdown))),
            operator_stop: None,
        }
    }

    /// Wire the operator's stop (`Shutdown`).
    pub fn with_operator_stop(mut self, stop: OperatorStop) -> Self {
        self.operator_stop = Some(stop);
        self
    }

    /// The operator's stop, minus the exit itself (testable): refuse during a
    /// live unless `force`, write the marker (error = nothing stopped), halt
    /// every boundary, then act on the air (best effort: logged).
    pub async fn operator_stop(&self, force: bool) -> Result<ShutdownReply, TonicStatus> {
        let stop = self
            .operator_stop
            .as_ref()
            .ok_or_else(|| TonicStatus::failed_precondition("operator stop is not wired"))?;
        let live = stop.control.live_dj();
        if let Some(dj) = &live {
            if !force {
                return Err(TonicStatus::failed_precondition(format!(
                    "DJ `{dj}` is on air: `stationctl station stop --force` disconnects them first"
                )));
            }
        }
        let at = stop.control.now();
        crate::operator_stop::write_marker(&stop.marker, at, "cli").map_err(|e| {
            tracing::error!(path = ?stop.marker, error = %e, "operator stop REFUSED: marker not written, stationd keeps running");
            TonicStatus::internal(format!(
                "cannot write the stop marker {}: {e} — nothing stopped",
                stop.marker.display()
            ))
        })?;
        stop.control.begin_operator_stop();
        let mut kicked = false;
        let mut parked = false;
        if let Some(ls) = &stop.ls {
            if live.is_some() {
                match ls.command("stationd.live_kick").await {
                    Ok(_) => kicked = true,
                    Err(e) => tracing::error!(error = %e, "operator stop: could NOT disconnect the DJ"),
                }
            }
            match ls.command("stationd.park").await {
                Ok(_) => parked = true,
                Err(e) => tracing::warn!(error = %e, "operator stop: Liquidsoap not parked, the safety fallback will air"),
            }
            if let Err(e) = ls.command("stationd.flush").await {
                tracing::warn!(error = %e, "operator stop: flush failed, the prepared track airs first");
            }
        }
        tracing::warn!(marker = ?stop.marker, kicked, parked, "stopped by the operator: exiting, not restarted until `stationctl station start`");
        Ok(ShutdownReply {
            marker: stop.marker.display().to_string(),
            kicked,
            parked,
        })
    }

    /// Upsert one already-parsed playlist into the SQLite view. Thin
    /// wrapper over `store::upsert` (the metier lives there so it can be
    /// tested without a gRPC server). stationd is the single writer.
    async fn upsert_view(
        &self,
        id: &str,
        playlist: &Playlist,
        rewritten: &str,
        rel_path: Option<&str>,
    ) -> Result<(), String> {
        store::upsert(&self.db, id, playlist, rewritten, rel_path)
            .await
            .map_err(|e| format!("could not update playlist view: {e}"))
    }
}

#[tonic::async_trait]
impl Station for StationService {
    async fn status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusReply>, TonicStatus> {
        let reply = StatusReply {
            station_name: self.station_name.clone(),
            uptime_seconds: self.started_at.elapsed().as_secs(),
            pid: std::process::id(),
            timezone: self.timezone.clone(),
        };
        Ok(Response::new(reply))
    }

    async fn quit(
        &self,
        _request: Request<QuitRequest>,
    ) -> Result<Response<QuitReply>, TonicStatus> {
        // Reply to the client BEFORE tearing down the server: otherwise it
        // never receives the acknowledgement (the connection drops with the
        // process).
        if let Some(tx) = self.shutdown.lock().await.take() {
            let _ = tx.send(());
        }
        Ok(Response::new(QuitReply {}))
    }

    async fn shutdown(
        &self,
        request: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownReply>, TonicStatus> {
        let reply = self.operator_stop(request.into_inner().force).await?;
        // Reply first, then leave (same as `quit`).
        if let Some(tx) = self.shutdown.lock().await.take() {
            let _ = tx.send(());
        }
        Ok(Response::new(reply))
    }

    async fn playlist_add(
        &self,
        request: Request<PlaylistAddRequest>,
    ) -> Result<Response<PlaylistAddReply>, TonicStatus> {
        let toml_content = request.into_inner().toml_content;

        // Per-file metier: parse (strict) + validate (business rules).
        // Cross-playlist checks (refs/cycles) are NOT done here — a single
        // file cannot see the whole set; that is `sync`'s job.
        let playlist = Playlist::parse(&toml_content)
            .map_err(|e| TonicStatus::invalid_argument(e.to_string()))?;
        playlist
            .validate()
            .map_err(|e| TonicStatus::invalid_argument(e.to_string()))?;

        let (rewritten, id) = playlist::assign_id(&toml_content)
            .map_err(|e| TonicStatus::invalid_argument(e.to_string()))?;

        // A file brought in from outside has no position in the tree → no
        // rel_path. A later `sync` will set it if the file lives under the
        // root.
        self.upsert_view(&id.to_string(), &playlist, &rewritten, None)
            .await
            .map_err(TonicStatus::internal)?;

        Ok(Response::new(PlaylistAddReply {
            toml_content: rewritten,
            id: id.to_string(),
        }))
    }

    async fn playlist_sync(
        &self,
        _request: Request<PlaylistSyncRequest>,
    ) -> Result<Response<PlaylistSyncReply>, TonicStatus> {
        // The whole reconciliation (walk the root, per-file load+validate,
        // whole-set validate, persist survivors, write ids back) lives in
        // `sync::sync_root` — a plain function with no tonic dependency, so
        // it can be driven directly by an integration test. This handler is
        // a thin translator: run it, map the metier errors to the proto.
        let outcome = crate::sync::sync_root(&self.db, &self.playlist_root).await;

        let errors = outcome
            .errors
            .into_iter()
            .map(|e| PlaylistSyncError {
                path: e.path,
                message: e.message,
            })
            .collect();

        Ok(Response::new(PlaylistSyncReply {
            added: outcome.added,
            errors,
        }))
    }

    async fn playlist_list(
        &self,
        _request: Request<PlaylistListRequest>,
    ) -> Result<Response<PlaylistListReply>, TonicStatus> {
        // Read from the view via `store::list`; the handler only maps the
        // metier rows to the proto message.
        let rows = store::list(&self.db)
            .await
            .map_err(|e| TonicStatus::internal(format!("could not read playlist view: {e}")))?;

        let playlists = rows
            .into_iter()
            .map(|r| PlaylistSummary {
                id: r.id,
                rel_path: r.rel_path.unwrap_or_default(),
                name: r.name,
                mode: r.mode,
                enabled: r.enabled,
            })
            .collect();

        Ok(Response::new(PlaylistListReply { playlists }))
    }

    async fn playlist_remove(
        &self,
        request: Request<PlaylistRemoveRequest>,
    ) -> Result<Response<PlaylistRemoveReply>, TonicStatus> {
        use crate::sync::RemoveError;
        let reference = request.into_inner().reference;
        match crate::sync::remove(&self.db, &self.playlist_root, &reference).await {
            Ok(r) => {
                tracing::info!(
                    id = %r.id,
                    rel_path = r.rel_path.as_deref().unwrap_or("-"),
                    file = r.file.as_deref().unwrap_or("-"),
                    "playlist removed"
                );
                Ok(Response::new(PlaylistRemoveReply {
                    id: r.id,
                    rel_path: r.rel_path.unwrap_or_default(),
                    file: r.file.unwrap_or_default(),
                }))
            }
            Err(e @ RemoveError::NotFound(_)) => Err(TonicStatus::not_found(e.to_string())),
            Err(e @ (RemoveError::Referenced { .. } | RemoveError::Ambiguous { .. })) => {
                Err(TonicStatus::failed_precondition(e.to_string()))
            }
            Err(e @ RemoveError::Failed(_)) => Err(TonicStatus::internal(e.to_string())),
        }
    }

    async fn playlist_export(
        &self,
        request: Request<PlaylistExportRequest>,
    ) -> Result<Response<PlaylistExportReply>, TonicStatus> {
        let reference = request.into_inner().reference;
        let row = store::find(&self.db, &reference)
            .await
            .map_err(|e| TonicStatus::internal(format!("could not read playlist view: {e}")))?
            .ok_or_else(|| {
                TonicStatus::not_found(format!("no playlist `{reference}` (neither a ref nor an id of the view)"))
            })?;
        Ok(Response::new(PlaylistExportReply {
            id: row.id,
            rel_path: row.rel_path.unwrap_or_default(),
            toml_content: row.toml,
        }))
    }

    async fn playlist_reload(
        &self,
        _request: Request<PlaylistReloadRequest>,
    ) -> Result<Response<PlaylistReloadReply>, TonicStatus> {
        let outcome = crate::sync::reload_root(&self.db, &self.playlist_root).await;
        for r in &outcome.removed {
            tracing::info!(playlist = %r, "playlist file gone: dropped from the view");
        }
        Ok(Response::new(PlaylistReloadReply {
            added: outcome.added,
            removed: outcome.removed,
            errors: outcome
                .errors
                .into_iter()
                .map(|e| PlaylistSyncError { path: e.path, message: e.message })
                .collect(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station_control::{Gate, StationControl};
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A Liquidsoap control socket answering OK, recording the commands.
    fn fake_ls(dir: &std::path::Path) -> (PathBuf, Arc<StdMutex<Vec<String>>>) {
        let path = dir.join("ls.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = seen2.clone();
                tokio::spawn(async move {
                    let (rd, mut wr) = stream.into_split();
                    let mut lines = BufReader::new(rd).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        if l == "quit" {
                            break;
                        }
                        seen.lock().unwrap().push(l);
                        let _ = wr.write_all(b"OK\r\nEND\r\n").await;
                    }
                });
            }
        });
        (path, seen)
    }

    async fn service(
        dir: &std::path::Path,
        ls: Option<crate::ls_control::LsControl>,
    ) -> (StationService, StationControl, PathBuf, oneshot::Receiver<()>) {
        let pool = crate::db::init(&dir.join("data").join("t.db")).await.unwrap();
        let control = StationControl::new_in_memory();
        let marker = crate::operator_stop::marker_under(dir);
        let (tx, rx) = oneshot::channel();
        let svc = StationService::new("r".into(), "UTC".into(), pool, dir.join("pl"), tx)
            .with_operator_stop(OperatorStop { control: control.clone(), ls, marker: marker.clone() });
        (svc, control, marker, rx)
    }

    #[tokio::test]
    async fn operator_stop_writes_the_marker_parks_and_exits() {
        let dir = tempfile::tempdir().unwrap();
        let (sock, seen) = fake_ls(dir.path());
        let ls = crate::ls_control::LsControl::new(sock);
        let (svc, control, marker, mut rx) = service(dir.path(), Some(ls)).await;
        let reply = svc.shutdown(Request::new(ShutdownRequest { force: false })).await.unwrap().into_inner();
        assert!(reply.parked && !reply.kicked);
        assert!(crate::operator_stop::read_marker(&marker).is_some());
        assert_eq!(*seen.lock().unwrap(), ["stationd.park", "stationd.flush"]);
        assert_eq!(control.gate(), Gate::Halt(crate::station_control::BroadcastState::Sleeping));
        assert!(rx.try_recv().is_ok(), "exit requested");
    }

    #[tokio::test]
    async fn operator_stop_refuses_during_a_live_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let (sock, seen) = fake_ls(dir.path());
        let ls = crate::ls_control::LsControl::new(sock);
        let (svc, control, marker, mut rx) = service(dir.path(), Some(ls)).await;
        control.set_live(Some("marc".into()));
        let err = svc.shutdown(Request::new(ShutdownRequest { force: false })).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(err.message().contains("--force"));
        assert!(crate::operator_stop::read_marker(&marker).is_none(), "nothing stopped");
        assert!(rx.try_recv().is_err());
        assert_eq!(control.gate(), Gate::Play);
        let reply = svc.shutdown(Request::new(ShutdownRequest { force: true })).await.unwrap().into_inner();
        assert!(reply.kicked);
        assert_eq!(*seen.lock().unwrap(), ["stationd.live_kick", "stationd.park", "stationd.flush"]);
    }

    #[tokio::test]
    async fn an_unwritable_marker_stops_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut svc, control, _marker, mut rx) = service(dir.path(), None).await;
        // The marker's directory does not exist: the write fails.
        svc.operator_stop.as_mut().unwrap().marker = dir.path().join("nope").join("stationd.stopped");
        let err = svc.shutdown(Request::new(ShutdownRequest { force: false })).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
        assert!(err.message().contains("nothing stopped"));
        assert!(rx.try_recv().is_err(), "stationd keeps running");
        assert_eq!(control.gate(), Gate::Play);
    }

    #[tokio::test]
    async fn without_liquidsoap_the_stop_still_happens() {
        let dir = tempfile::tempdir().unwrap();
        let (svc, _control, marker, mut rx) = service(dir.path(), None).await;
        let reply = svc.shutdown(Request::new(ShutdownRequest { force: false })).await.unwrap().into_inner();
        assert!(!reply.parked);
        assert!(crate::operator_stop::read_marker(&marker).is_some());
        assert!(rx.try_recv().is_ok());
    }
}
