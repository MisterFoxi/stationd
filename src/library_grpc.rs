//! gRPC transport for the media library service (library_v1.proto).
//!
//! Thin translator over `LibraryHandle`, same discipline as `grpc.rs` /
//! `schedule_grpc.rs`: map the proto to the actor and back, no metier here.
//! Served on the same tonic server as the other services (one port).

use tonic::{Request, Response, Status};

use crate::library_actor::{LibraryError, LibraryHandle};

// Keep the generated module reachable under a stable path for callers/tests.
pub use crate::proto::library;

use library::library_service_server::LibraryService;
use library::{ListMediaRequest, ListMediaResponse, Media, ScanRequest, ScanResponse, Skip};

pub struct LibraryGrpc {
    handle: LibraryHandle,
}

impl LibraryGrpc {
    pub fn new(handle: LibraryHandle) -> Self {
        Self { handle }
    }
}

/// A missing media root is a deployment/config fault the operator must fix →
/// `failed_precondition`; everything else is ours → `internal`.
fn map_error(e: LibraryError) -> Status {
    match e {
        LibraryError::BadRoot(p) => {
            Status::failed_precondition(format!("media root unavailable: {}", p.display()))
        }
        other => Status::internal(other.to_string()),
    }
}

fn map_skip(s: crate::media::ScanSkip) -> Skip {
    use crate::media::SkipReason;
    use library::skip::Reason;
    let (reason, detail) = match s.reason {
        SkipReason::Unreadable(d) => (Reason::Unreadable, d),
        SkipReason::ZeroDuration => (Reason::ZeroDuration, String::new()),
        SkipReason::WalkError(d) => (Reason::WalkError, d),
    };
    Skip {
        path: s.path,
        reason: reason as i32,
        detail,
    }
}

fn map_media(m: crate::media_index::MediaRow) -> Media {
    Media {
        rel_path: m.rel_path,
        title: m.title.unwrap_or_default(),
        artist: m.artist.unwrap_or_default(),
        album: m.album.unwrap_or_default(),
        year: m.year.unwrap_or(0) as u32,
        duration_ms: m.duration_ms as u64,
        size_bytes: m.size_bytes as u64,
        available: m.available,
        genres: m.genres,
    }
}

#[tonic::async_trait]
impl LibraryService for LibraryGrpc {
    async fn scan(&self, _request: Request<ScanRequest>) -> Result<Response<ScanResponse>, Status> {
        let outcome = self.handle.scan().await.map_err(map_error)?;
        let report = outcome.report;
        let found = report.media.len() as u32;
        let skipped = report.skipped.len() as u32;
        let skips = report.skipped.into_iter().map(map_skip).collect();
        Ok(Response::new(ScanResponse {
            found,
            skipped,
            present: outcome.stats.present as u32,
            unavailable: outcome.stats.unavailable as u32,
            skips,
        }))
    }

    async fn list_media(
        &self,
        request: Request<ListMediaRequest>,
    ) -> Result<Response<ListMediaResponse>, Status> {
        let only_available = request.into_inner().only_available;
        let media = self
            .handle
            .list(only_available)
            .await
            .map_err(map_error)?
            .into_iter()
            .map(map_media)
            .collect();
        Ok(Response::new(ListMediaResponse { media }))
    }
}
