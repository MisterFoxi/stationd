//! gRPC transport for the playlists (playlist_v1.proto): a thin translator
//! over `store` (view), `sync` (root reconciliation, remove) and
//! `playlist_edit` (validate, preview, save, export). No business logic here.
//!
//! Writes to the playlist root (Save, Remove, Sync / Reload writing ids back)
//! are serialised by one lock: two clients saving at once cannot interleave
//! their check-then-write.

use std::path::PathBuf;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};

use crate::playlist::{self, Diag, DiagCode, Playlist};
use crate::playlist_edit::{self, EditError};
use crate::station_control::StationControl;
use crate::store;
use crate::sync::{RemoveError, SyncError};

pub use crate::proto::playlist as proto;
use proto::playlist_service_server::PlaylistService;
use proto::*;

pub struct PlaylistGrpc {
    db: SqlitePool,
    root: PathBuf,
    /// The on-air view is told when the playlists change (what airs next may
    /// differ). `None` in tests.
    control: Option<StationControl>,
    writes: Arc<Mutex<()>>,
}

impl PlaylistGrpc {
    pub fn new(db: SqlitePool, root: PathBuf, control: Option<StationControl>) -> Self {
        Self { db, root, control, writes: Arc::new(Mutex::new(())) }
    }

    /// The station's now (manual clock if set): relative filters (`age`)
    /// resolve against it.
    fn now(&self) -> i64 {
        match &self.control {
            Some(c) => c.now().0,
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }

    fn changed(&self) {
        if let Some(c) = &self.control {
            c.bump_air();
        }
    }
}

fn code(c: DiagCode) -> proto::diagnostic::Code {
    use proto::diagnostic::Code as P;
    match c {
        DiagCode::Syntax => P::Syntax,
        DiagCode::UnknownField => P::UnknownField,
        DiagCode::MissingField => P::MissingField,
        DiagCode::BadValue => P::BadValue,
        DiagCode::NotAllowed => P::NotAllowed,
        DiagCode::RequiredForMode => P::RequiredForMode,
        DiagCode::Conflict => P::Conflict,
        DiagCode::BadFilter => P::BadFilter,
        DiagCode::BadDuration => P::BadDuration,
        DiagCode::UnknownRef => P::UnknownRef,
        DiagCode::BadRef => P::BadRef,
        DiagCode::Cycle => P::Cycle,
        DiagCode::IdChanged => P::IdChanged,
        DiagCode::EmptyPool => P::EmptyPool,
    }
}

pub fn to_proto(d: &Diag, file: &str) -> Diagnostic {
    Diagnostic {
        severity: if d.error { proto::diagnostic::Severity::Error } else { proto::diagnostic::Severity::Warning } as i32,
        file: file.to_string(),
        field_path: d.field.clone(),
        rejected: d.rejected.clone(),
        expected: d.expected.clone(),
        message: d.message.clone(),
        code: code(d.code) as i32,
    }
}

fn diags(ds: &[Diag], file: &str) -> Vec<Diagnostic> {
    ds.iter().map(|d| to_proto(d, file)).collect()
}

fn file_errors(errors: Vec<SyncError>) -> Vec<FileError> {
    errors
        .into_iter()
        .map(|e| FileError { diagnostics: diags(&e.diagnostics, &e.path), path: e.path, message: e.message })
        .collect()
}

fn edit_status(e: EditError) -> Status {
    match e {
        EditError::BadRef(_) => Status::invalid_argument(e.to_string()),
        EditError::NotFound(_) | EditError::UnknownMedia(_) => Status::not_found(e.to_string()),
        EditError::Ambiguous { .. } => Status::failed_precondition(e.to_string()),
        EditError::Io(_) => Status::internal(e.to_string()),
    }
}

/// A draft's place, normalized; empty = none. A malformed one is refused up
/// front (the caller named a place that cannot exist).
#[allow(clippy::result_large_err)] // tonic::Status, as every handler returns
fn draft_key(reference: &str) -> Result<Option<String>, Status> {
    if reference.trim().is_empty() {
        return Ok(None);
    }
    playlist::normalize_ref(reference).map(Some).map_err(Status::invalid_argument)
}

#[tonic::async_trait]
impl PlaylistService for PlaylistGrpc {
    async fn list(&self, _: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let rows = store::list(&self.db)
            .await
            .map_err(|e| Status::internal(format!("could not read playlist view: {e}")))?;
        let refs = crate::sync::reference_index(&self.db).await.map_err(Status::internal)?;
        let playlists = rows
            .into_iter()
            .map(|r| {
                let by = r.rel_path.as_ref().and_then(|k| refs.get(k)).cloned().unwrap_or_default();
                PlaylistSummary {
                    id: r.id,
                    rel_path: r.rel_path.unwrap_or_default(),
                    name: r.name,
                    mode: r.mode,
                    enabled: r.enabled,
                    rules: by.rules,
                    groups: by.groups,
                }
            })
            .collect();
        Ok(Response::new(ListResponse { playlists }))
    }

    async fn export(&self, request: Request<ExportRequest>) -> Result<Response<ExportResponse>, Status> {
        let reference = request.into_inner().reference;
        let x = playlist_edit::export(&self.db, &self.root, &reference).await.map_err(edit_status)?;
        let file_differs = x.file_differs();
        let file = x.file.unwrap_or_else(|| playlist_edit::FileState {
            path: PathBuf::new(),
            rel: String::new(),
            content: String::new(),
            revision: String::new(),
        });
        Ok(Response::new(ExportResponse {
            id: x.id,
            rel_path: x.rel_path.unwrap_or_default(),
            applied_toml: x.applied_toml,
            file: file.rel,
            file_toml: file.content,
            revision: file.revision,
            file_differs,
        }))
    }

    async fn validate(&self, request: Request<ValidateRequest>) -> Result<Response<ValidateResponse>, Status> {
        let r = request.into_inner();
        let key = draft_key(&r.reference)?;
        let (_, ds) = playlist_edit::validate_draft(&self.db, &r.toml, key.as_deref()).await.map_err(edit_status)?;
        let file = key.unwrap_or_default();
        Ok(Response::new(ValidateResponse { ok: !playlist_edit::has_errors(&ds), diagnostics: diags(&ds, &file) }))
    }

    async fn preview_pool(
        &self,
        request: Request<PreviewPoolRequest>,
    ) -> Result<Response<PreviewPoolResponse>, Status> {
        let r = request.into_inner();
        let key = draft_key(&r.reference)?;
        let p = playlist_edit::preview(&self.db, &r.toml, key.as_deref(), r.sample as usize, self.now())
            .await
            .map_err(edit_status)?;
        let file = key.unwrap_or_default();
        Ok(Response::new(PreviewPoolResponse {
            ok: p.ok,
            diagnostics: diags(&p.diagnostics, &file),
            count: p.count,
            duration_ms: p.duration_ms,
            artists: p.artists,
            sample: p
                .sample
                .into_iter()
                .map(|m| PoolMedia {
                    rel_path: m.rel_path,
                    title: m.title.unwrap_or_default(),
                    artist: m.artist.unwrap_or_default(),
                    duration_ms: m.duration_ms,
                })
                .collect(),
            members: p
                .members
                .into_iter()
                .map(|m| MemberPool {
                    r#ref: m.r#ref,
                    resolved: m.resolved.unwrap_or_default(),
                    count: m.count,
                    duration_ms: m.duration_ms,
                    error: m.error.unwrap_or_default(),
                })
                .collect(),
        }))
    }

    async fn save(&self, request: Request<SaveRequest>) -> Result<Response<SaveResponse>, Status> {
        let r = request.into_inner();
        let _w = self.writes.lock().await;
        let s = playlist_edit::save(&self.db, &self.root, &r.reference, &r.toml, &r.expected_revision)
            .await
            .map_err(edit_status)?;
        if s.ok {
            self.changed();
            tracing::info!(file = %s.file, id = %s.id, created = s.created, "playlist saved");
        }
        let file = playlist::normalize_ref(&r.reference).unwrap_or_default();
        Ok(Response::new(SaveResponse {
            ok: s.ok,
            diagnostics: diags(&s.diagnostics, &file),
            conflict: s.conflict,
            revision: s.revision,
            toml: s.toml,
            id: s.id,
            file: s.file,
            created: s.created,
        }))
    }

    async fn remove(&self, request: Request<RemoveRequest>) -> Result<Response<RemoveResponse>, Status> {
        let r = request.into_inner();
        let expected = (!r.expected_revision.trim().is_empty()).then_some(r.expected_revision.as_str());
        let _w = self.writes.lock().await;
        match crate::sync::remove(&self.db, &self.root, &r.reference, expected).await {
            Ok(x) => {
                self.changed();
                tracing::info!(
                    id = %x.id,
                    rel_path = x.rel_path.as_deref().unwrap_or("-"),
                    file = x.file.as_deref().unwrap_or("-"),
                    "playlist removed"
                );
                Ok(Response::new(RemoveResponse {
                    id: x.id,
                    rel_path: x.rel_path.unwrap_or_default(),
                    file: x.file.unwrap_or_default(),
                }))
            }
            Err(e @ RemoveError::NotFound(_)) => Err(Status::not_found(e.to_string())),
            Err(e @ (RemoveError::Referenced { .. } | RemoveError::Ambiguous { .. })) => {
                Err(Status::failed_precondition(e.to_string()))
            }
            Err(e @ RemoveError::Conflict { .. }) => Err(Status::aborted(e.to_string())),
            Err(e @ RemoveError::Failed(_)) => Err(Status::internal(e.to_string())),
        }
    }

    async fn sync(&self, _: Request<SyncRequest>) -> Result<Response<SyncResponse>, Status> {
        let _w = self.writes.lock().await;
        let outcome = crate::sync::sync_root(&self.db, &self.root).await;
        self.changed();
        Ok(Response::new(SyncResponse { added: outcome.added, errors: file_errors(outcome.errors) }))
    }

    async fn reload(&self, _: Request<ReloadRequest>) -> Result<Response<ReloadResponse>, Status> {
        let _w = self.writes.lock().await;
        let outcome = crate::sync::reload_root(&self.db, &self.root).await;
        self.changed();
        for r in &outcome.removed {
            tracing::info!(playlist = %r, "playlist file gone: dropped from the view");
        }
        Ok(Response::new(ReloadResponse {
            added: outcome.added,
            removed: outcome.removed,
            errors: file_errors(outcome.errors),
        }))
    }

    async fn containing(&self, request: Request<ContainingRequest>) -> Result<Response<ContainingResponse>, Status> {
        let media = request.into_inner().media_path;
        let media = media.trim();
        if media.is_empty() {
            return Err(Status::invalid_argument("media_path is empty"));
        }
        let holders = playlist_edit::containing(&self.db, media, self.now()).await.map_err(edit_status)?;
        let refs = crate::sync::reference_index(&self.db).await.map_err(Status::internal)?;
        let playlists = holders
            .into_iter()
            .map(|h| {
                let by = h.rel_path.as_ref().and_then(|k| refs.get(k)).cloned().unwrap_or_default();
                PlaylistSummary {
                    id: h.id,
                    rel_path: h.rel_path.unwrap_or_default(),
                    name: h.name,
                    mode: format!("{:?}", h.mode).to_lowercase(),
                    enabled: h.enabled,
                    rules: by.rules,
                    groups: by.groups,
                }
            })
            .collect();
        Ok(Response::new(ContainingResponse { playlists }))
    }

    async fn add(&self, request: Request<AddRequest>) -> Result<Response<AddResponse>, Status> {
        let text = request.into_inner().toml;
        // Per-file rules only: an entry without a place in the tree has no
        // group-relative refs to resolve (set checks belong to Sync / Save).
        let playlist = playlist::parse_with_diagnostics(&text).map_err(|d| Status::invalid_argument(d.message))?;
        if let Some(d) = playlist.diagnostics().into_iter().next() {
            return Err(Status::invalid_argument(format!("validation failed: {}", d.message)));
        }
        let (rewritten, id) = playlist::assign_id(&text).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let playlist = Playlist::parse(&rewritten).map_err(|e| Status::internal(e.to_string()))?;
        store::upsert(&self.db, &id.to_string(), &playlist, &rewritten, None)
            .await
            .map_err(|e| Status::internal(format!("could not update playlist view: {e}")))?;
        self.changed();
        Ok(Response::new(AddResponse { toml: rewritten, id: id.to_string() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn svc() -> (tempfile::TempDir, PlaylistGrpc) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        let root = dir.path().join("pl");
        std::fs::create_dir_all(&root).unwrap();
        (dir, PlaylistGrpc::new(pool, root, None))
    }

    const Q: &str = "name = \"Req\"\n[selection]\nmode = \"queue\"\norder = \"fifo\"\n";

    #[tokio::test]
    async fn save_then_export_then_remove_with_the_revision() {
        let (_d, s) = svc().await;
        let r = s
            .save(Request::new(SaveRequest { reference: "req".into(), toml: Q.into(), expected_revision: String::new() }))
            .await
            .unwrap()
            .into_inner();
        assert!(r.ok && r.created, "{r:?}");
        let x = s.export(Request::new(ExportRequest { reference: "req".into() })).await.unwrap().into_inner();
        assert_eq!((x.file.as_str(), x.revision.as_str(), x.file_differs), ("req.toml", r.revision.as_str(), false));
        let listed = s.list(Request::new(ListRequest {})).await.unwrap().into_inner();
        assert_eq!(listed.playlists.len(), 1);
        let stale = s
            .remove(Request::new(RemoveRequest { reference: "req".into(), expected_revision: "sha256:00".into() }))
            .await
            .unwrap_err();
        assert_eq!(stale.code(), tonic::Code::Aborted);
        s.remove(Request::new(RemoveRequest { reference: "req".into(), expected_revision: r.revision }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_invalid_draft_is_an_answer_not_an_error() {
        let (_d, s) = svc().await;
        let bad = Q.replace("fifo", "shuffle");
        let r = s
            .validate(Request::new(ValidateRequest { toml: bad, reference: String::new() }))
            .await
            .unwrap()
            .into_inner();
        assert!(!r.ok);
        let d = &r.diagnostics[0];
        assert_eq!(d.field_path, "selection.order");
        assert_eq!(d.code, proto::diagnostic::Code::BadValue as i32);
        assert_eq!(d.severity, proto::diagnostic::Severity::Error as i32);
        // A place that cannot exist is the caller's mistake.
        let e = s
            .validate(Request::new(ValidateRequest { toml: Q.into(), reference: "../x".into() }))
            .await
            .unwrap_err();
        assert_eq!(e.code(), tonic::Code::InvalidArgument);
    }
}
