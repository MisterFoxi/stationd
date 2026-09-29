//! gRPC transport for the media library service (library_v1.proto).
//!
//! Thin translator over `LibraryHandle`, same discipline as `grpc.rs` /
//! `schedule_grpc.rs`: map the proto to the actor and back, no metier here.
//! Served on the same tonic server as the other services (one port).

use tonic::{Request, Response, Status};

use crate::library_actor::{LibraryError, LibraryHandle, TagsRead};
use crate::media_tags::{TagEdit, TagError};

// Keep the generated module reachable under a stable path for callers/tests.
pub use crate::proto::library;

use library::library_service_server::LibraryService;
use library::{
    GenreCount, ListGenresRequest, ListGenresResponse, ListMediaRequest, ListMediaResponse, Media,
    GetTagsRequest, MediaTags, PruneRequest, PruneResponse, ScanRequest, ScanResponse, SearchMediaRequest,
    SearchMediaResponse, SetTagsRequest, SetTagsResponse, Skip,
};

fn map_tags(rel_path: &str, t: TagsRead) -> MediaTags {
    MediaTags {
        rel_path: rel_path.to_string(),
        title: t.tags.title.unwrap_or_default(),
        artist: t.tags.artist.unwrap_or_default(),
        album: t.tags.album.unwrap_or_default(),
        year: t.tags.year.unwrap_or(0),
        genre: t.tags.genre.unwrap_or_default(),
        revision: t.revision,
    }
}

/// Proto edit → `TagEdit`: absent = unchanged, "" / 0 = removed.
fn edit_of(r: &SetTagsRequest) -> TagEdit {
    let text = |v: &Option<String>| v.as_ref().map(|s| (!s.trim().is_empty()).then(|| s.clone()));
    TagEdit {
        title: text(&r.title),
        artist: text(&r.artist),
        album: text(&r.album),
        year: r.year.map(|y| (y != 0).then_some(y)),
        genre: text(&r.genre),
    }
}

pub struct LibraryGrpc {
    handle: LibraryHandle,
}

impl LibraryGrpc {
    pub fn new(handle: LibraryHandle) -> Self {
        Self { handle }
    }
}

/// A missing media root is a deployment/config fault the operator must fix →
/// `failed_precondition`; a bad request filter → `invalid_argument`;
/// everything else is ours → `internal`.
fn map_error(e: LibraryError) -> Status {
    match e {
        LibraryError::BadRoot(p) => {
            Status::failed_precondition(format!("media root unavailable: {}", p.display()))
        }
        LibraryError::BadFilter(m) => Status::invalid_argument(m),
        LibraryError::Tags(t) => match t {
            TagError::NotFound(_) => Status::not_found(t.to_string()),
            TagError::Unsupported(_) => Status::failed_precondition(t.to_string()),
            TagError::BadValue(_) => Status::invalid_argument(t.to_string()),
            TagError::Conflict { .. } => Status::aborted(t.to_string()),
            TagError::Io(_) | TagError::NotKept(_) => Status::internal(t.to_string()),
        },
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

fn search_field(v: i32) -> crate::media_index::SearchField {
    use crate::media_index::SearchField as F;
    use library::search_media_request::Field as P;
    match P::try_from(v).unwrap_or(P::Unspecified) {
        P::Unspecified | P::Path => F::Path,
        P::Title => F::Title,
        P::Artist => F::Artist,
        P::Album => F::Album,
        P::Year => F::Year,
        P::Duration => F::Duration,
        P::Genre => F::Genre,
    }
}

#[tonic::async_trait]
impl LibraryService for LibraryGrpc {
    async fn search_media(
        &self,
        request: Request<SearchMediaRequest>,
    ) -> Result<Response<SearchMediaResponse>, Status> {
        use crate::media_index::{SearchCursor, SearchQuery};
        let r = request.into_inner();
        let cursor = if r.cursor.trim().is_empty() {
            None
        } else {
            Some(SearchCursor::decode(&r.cursor).ok_or_else(|| Status::invalid_argument("invalid cursor"))?)
        };
        let page = self
            .handle
            .search(SearchQuery {
                query: r.query,
                genres: r.genres,
                folder: r.folder,
                include_unavailable: r.include_unavailable,
                missing: r.missing.into_iter().map(search_field).collect(),
                sort: search_field(r.sort),
                descending: r.descending,
                limit: r.limit as usize,
                cursor,
            })
            .await
            .map_err(map_error)?;
        Ok(Response::new(SearchMediaResponse {
            media: page.media.into_iter().map(map_media).collect(),
            total: page.total,
            next_cursor: page.next.map(|c| c.encode()).unwrap_or_default(),
        }))
    }

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
            vanished: outcome.stats.vanished as u32,
            skips,
        }))
    }

    async fn prune(&self, request: Request<PruneRequest>) -> Result<Response<PruneResponse>, Status> {
        let older = request.into_inner().older_than;
        let seen_before = if older.trim().is_empty() {
            None
        } else {
            let secs = crate::playlist::parse_duration_secs(older.trim()).map_err(Status::invalid_argument)?;
            let now = jiff::Timestamp::now().as_second();
            Some(now - secs as i64)
        };
        let removed = self.handle.prune(seen_before).await.map_err(map_error)?;
        tracing::info!(removed, older_than = %older, "library: vanished media forgotten");
        Ok(Response::new(PruneResponse { removed }))
    }

    async fn get_tags(&self, request: Request<GetTagsRequest>) -> Result<Response<MediaTags>, Status> {
        let rel_path = request.into_inner().rel_path;
        let t = self.handle.get_tags(rel_path.clone()).await.map_err(map_error)?;
        Ok(Response::new(map_tags(&rel_path, t)))
    }

    async fn set_tags(&self, request: Request<SetTagsRequest>) -> Result<Response<SetTagsResponse>, Status> {
        let r = request.into_inner();
        let edit = edit_of(&r);
        let w = self.handle.set_tags(r.rel_path.clone(), r.revision.clone(), edit).await.map_err(map_error)?;
        Ok(Response::new(SetTagsResponse {
            conflict: w.conflict,
            tags: Some(map_tags(&r.rel_path, w.tags)),
            media: w.row.map(map_media),
        }))
    }

    async fn list_media(
        &self,
        request: Request<ListMediaRequest>,
    ) -> Result<Response<ListMediaResponse>, Status> {
        let req = request.into_inner();
        let media = self
            .handle
            .list(req.only_available, req.genres)
            .await
            .map_err(map_error)?
            .into_iter()
            .map(map_media)
            .collect();
        Ok(Response::new(ListMediaResponse { media }))
    }

    async fn list_genres(
        &self,
        request: Request<ListGenresRequest>,
    ) -> Result<Response<ListGenresResponse>, Status> {
        let only_available = request.into_inner().only_available;
        let inv = self.handle.genres(only_available).await.map_err(map_error)?;
        Ok(Response::new(ListGenresResponse {
            genres: inv
                .genres
                .into_iter()
                .map(|g| GenreCount {
                    genre: g.genre,
                    count: g.count as u32,
                    spellings: g.spellings,
                })
                .collect(),
            untagged: inv.untagged as u32,
        }))
    }
}
