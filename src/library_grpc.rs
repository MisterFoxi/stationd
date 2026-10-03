//! gRPC transport for the media library service (library_v1.proto).
//!
//! Thin translator over `LibraryHandle`, same discipline as `grpc.rs` /
//! `schedule_grpc.rs`: map the proto to the actor and back, no metier here.
//! Served on the same tonic server as the other services (one port).

use tonic::{Request, Response, Status};

use crate::library_actor::{LibraryError, LibraryHandle, TagsRead};
use crate::media_tags::{TagEdit, TagError, CREATION_MANUAL, TEMPO_MANUAL};

// Keep the generated module reachable under a stable path for callers/tests.
pub use crate::proto::library;

use library::library_service_server::LibraryService;
use library::{
    GenreCount, ListGenresRequest, ListGenresResponse, ListMediaRequest, ListMediaResponse, Media,
    GetTagsRequest, MediaTags, PruneRequest, PruneResponse, ScanRequest, ScanResponse, SearchMediaRequest,
    SearchMediaResponse, SetTagsRequest, SetTagsResponse, Skip, TagValues,
};

fn map_tags(rel_path: &str, t: TagsRead) -> MediaTags {
    let first = |name: &str| t.tags.user_values(name).into_iter().next().unwrap_or_default();
    MediaTags {
        rel_path: rel_path.to_string(),
        title: t.tags.title.clone().unwrap_or_default(),
        artist: t.tags.artist.clone().unwrap_or_default(),
        album: t.tags.album.clone().unwrap_or_default(),
        year: t.tags.year.unwrap_or(0),
        revision: t.revision.clone(),
        genres: t.tags.genres.clone(),
        sources: t
            .genre_sources
            .iter()
            .map(|n| TagValues { name: n.clone(), values: t.tags.user_values(n) })
            .collect(),
        bpm: t.tags.bpm.unwrap_or(0),
        tempo_manual: first(TEMPO_MANUAL),
        creation_manual: first(CREATION_MANUAL),
        tempo: t.tempo.clone().unwrap_or_default(),
        creation: t.creation.clone().unwrap_or_default(),
        tempo_choices: t.tempo_choices.clone(),
    }
}

/// Proto edit → `TagEdit`: absent = unchanged, "" / 0 / empty list = removed.
fn edit_of(r: &SetTagsRequest) -> TagEdit {
    let text = |v: &Option<String>| v.as_ref().map(|s| (!s.trim().is_empty()).then(|| s.clone()));
    let mut user = std::collections::BTreeMap::new();
    for src in &r.sources {
        user.insert(src.name.clone(), src.values.clone());
    }
    if let Some(v) = &r.tempo_manual {
        user.insert(TEMPO_MANUAL.to_string(), vec![v.clone()]);
    }
    if let Some(v) = &r.creation_manual {
        user.insert(CREATION_MANUAL.to_string(), vec![v.clone()]);
    }
    TagEdit {
        title: text(&r.title),
        artist: text(&r.artist),
        album: text(&r.album),
        year: r.year.map(|y| (y != 0).then_some(y)),
        genres: r.genres.as_ref().map(|g| g.values.clone()),
        bpm: r.bpm.map(|b| (b != 0).then_some(b)),
        user,
    }
}

pub struct LibraryGrpc {
    handle: LibraryHandle,
    /// `true` once the daemon shuts down: open streams end. `None` (tests):
    /// they end when their client leaves.
    stopping: Option<tokio::sync::watch::Receiver<bool>>,
}

impl LibraryGrpc {
    pub fn new(handle: LibraryHandle) -> Self {
        Self { handle, stopping: None }
    }

    /// Streams (`WatchScan`) end when `stopping` turns `true`.
    pub fn with_stopping(mut self, stopping: tokio::sync::watch::Receiver<bool>) -> Self {
        self.stopping = Some(stopping);
        self
    }
}

/// Resolves once the daemon shuts down; never without a `stopping`.
async fn stopped(stopping: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match stopping {
        Some(rx) => {
            let _ = rx.wait_for(|s| *s).await;
        }
        None => std::future::pending().await,
    }
}

fn scan_status(s: &crate::library_actor::ScanStatus) -> library::ScanStatus {
    use crate::library_actor::ScanPhase as P;
    use library::scan_status::Phase;
    let phase = match s.phase {
        P::Idle => Phase::Idle,
        P::Listing => Phase::Listing,
        P::Reading => Phase::Reading,
        P::Analyzing => Phase::Analyzing,
        P::Plugins => Phase::Plugins,
        P::Writing => Phase::Writing,
        P::Indexing => Phase::Indexing,
    };
    library::ScanStatus {
        phase: phase as i32,
        done: s.done,
        total: s.total,
        started_at: s.started_at,
        last: s.last.as_ref().map(|e| match &e.result {
            Ok(c) => library::ScanEnd {
                finished_at: e.finished_at,
                ok: true,
                error: String::new(),
                found: c.found as u32,
                skipped: c.skipped as u32,
                present: c.present as u32,
                unavailable: c.unavailable as u32,
                vanished: c.vanished as u32,
            },
            Err(why) => library::ScanEnd { finished_at: e.finished_at, ok: false, error: why.clone(), ..Default::default() },
        }),
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
        bpm: m.bpm.unwrap_or(0) as f64,
        genre_ai: m.genre_ai.unwrap_or_default(),
        mood: m.mood.unwrap_or_default(),
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
        P::Bpm => F::Bpm,
        P::Genre => F::Genre,
    }
}

#[tonic::async_trait]
impl LibraryService for LibraryGrpc {
    async fn reorganize(&self, request: Request<library::ReorganizeRequest>) -> Result<Response<library::ReorganizeResponse>, Status> {
        let report = self.handle.reorganize(request.into_inner().dry_run).await.map_err(map_error)?;
        Ok(Response::new(library::ReorganizeResponse {
            moved: report.count("moved"),
            planned: report.count("planned"),
            unchanged: report.count("unchanged"),
            skipped: report.count("skipped"),
            failed: report.count("failed"),
            files: report.files.into_iter().map(|f| library::ReorganizeFile {
                from: f.from, to: f.to, status: f.status.into(), detail: f.detail,
            }).collect(),
        }))
    }
    async fn list_folders(&self, request: Request<library::ListFoldersRequest>) -> Result<Response<library::ListFoldersResponse>, Status> {
        let req = request.into_inner();
        let page = self.handle.folders(req.include_unavailable, req.cursor, if req.limit == 0 { 500 } else { req.limit as usize }).await.map_err(map_error)?;
        Ok(Response::new(library::ListFoldersResponse { folders: page.folders.into_iter().map(|f| library::MediaFolder { path:f.path, count:f.count }).collect(), next_cursor: page.next.unwrap_or_default() }))
    }
    async fn search_media(
        &self,
        request: Request<SearchMediaRequest>,
    ) -> Result<Response<SearchMediaResponse>, Status> {
        use crate::media_index::{SearchCursor, SearchQuery};
        let r = request.into_inner();
        // Ages resolve against the wall clock: a search is about « now ».
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let creation = r
            .age
            .iter()
            .map(|a| {
                crate::media_index::age_filter(a.op.trim(), a.value.trim(), now).map_err(|e| match e {
                    crate::media_index::AgeFilterError::Op => {
                        Status::invalid_argument(format!("age: unknown operator `{}` (<, <=, >, >=)", a.op))
                    }
                    crate::media_index::AgeFilterError::Value(why) => Status::invalid_argument(format!("age: {why}")),
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        let bpm = r
            .bpm
            .iter()
            .map(|b| {
                let op: &'static str = match b.op.trim() {
                    "<" => "<",
                    "<=" => "<=",
                    ">" => ">",
                    ">=" => ">=",
                    "=" | "==" => "=",
                    "!=" | "ne" => "!=",
                    other => {
                        return Err(Status::invalid_argument(format!(
                            "bpm: unknown operator `{other}` (<, <=, >, >=, =, !=)"
                        )))
                    }
                };
                Ok((op, b.value))
            })
            .collect::<Result<Vec<_>, Status>>()?;
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
                directory: r.directory,
                include_unavailable: r.include_unavailable,
                missing: r.missing.into_iter().map(search_field).collect(),
                sort: search_field(r.sort),
                descending: r.descending,
                limit: r.limit as usize,
                cursor,
                creation,
                bpm,
                genre_ai: r.genre_ai,
                mood: r.mood,
            })
            .await
            .map_err(map_error)?;
        Ok(Response::new(SearchMediaResponse {
            media: page.media.into_iter().map(map_media).collect(),
            total: page.total,
            next_cursor: page.next.map(|c| c.encode()).unwrap_or_default(),
        }))
    }

    type WatchScanStream = tokio_stream::wrappers::ReceiverStream<Result<library::ScanStatus, Status>>;

    async fn watch_scan(
        &self,
        _request: Request<library::WatchScanRequest>,
    ) -> Result<Response<Self::WatchScanStream>, Status> {
        let mut rx = self.handle.watch_scan();
        let mut stopping = self.stopping.clone();
        let (tx, out) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let first = scan_status(&rx.borrow_and_update());
            if tx.send(Ok(first)).await.is_err() {
                return;
            }
            loop {
                tokio::select! {
                    _ = stopped(&mut stopping) => break,
                    changed = rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let s = scan_status(&rx.borrow_and_update());
                        if tx.send(Ok(s)).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(out)))
    }

    async fn list_tag_values(
        &self,
        _request: Request<library::ListTagValuesRequest>,
    ) -> Result<Response<library::ListTagValuesResponse>, Status> {
        let origins = self.handle.tag_inventory().await.map_err(map_error)?;
        Ok(Response::new(library::ListTagValuesResponse {
            origins: origins
                .into_iter()
                .map(|o| library::TagOrigin {
                    origin: o.origin,
                    values: o
                        .values
                        .into_iter()
                        .map(|v| library::TagValueCount { value: v.value, count: v.count as u32, spellings: v.spellings })
                        .collect(),
                    without: o.without as u32,
                })
                .collect(),
        }))
    }

    type RenameTagValueStream = tokio_stream::wrappers::ReceiverStream<Result<library::RenameTagValueEvent, Status>>;

    async fn rename_tag_value(
        &self,
        request: Request<library::RenameTagValueRequest>,
    ) -> Result<Response<Self::RenameTagValueStream>, Status> {
        use crate::library_actor::RenameStep;
        use library::rename_tag_value_event::Event;
        let r = request.into_inner();
        if r.from.trim().is_empty() || r.to.trim().is_empty() {
            return Err(Status::invalid_argument("`from` and `to` must not be empty"));
        }
        let (tx, out) = tokio::sync::mpsc::channel(32);
        if r.dry_run {
            let p = self.handle.rename_preview(r.origin, r.from, r.to).await.map_err(map_error)?;
            let ev = library::RenameTagValueEvent {
                event: Some(Event::Preview(library::RenamePreview {
                    files: p.files,
                    playlists: p.playlists,
                    merges: p.merges,
                })),
            };
            let _ = tx.send(Ok(ev)).await;
            return Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(out)));
        }
        let handle = self.handle.clone();
        let (steps_tx, mut steps) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            let run = tokio::spawn(async move { handle.rename(r.origin, r.from, r.to, steps_tx).await });
            while let Some(step) = steps.recv().await {
                let event = match step {
                    RenameStep::Started { files } => Event::Started(files),
                    RenameStep::File { rel_path, error } => {
                        Event::File(library::RenameFile { rel_path, error: error.unwrap_or_default() })
                    }
                    RenameStep::Finished { changed, unchanged, failed } => {
                        Event::Done(library::RenameDone { changed, unchanged, failed })
                    }
                };
                // The client may leave: the rename goes on (stopping half-way
                // would leave the library half renamed).
                let _ = tx.send(Ok(library::RenameTagValueEvent { event: Some(event) })).await;
            }
            match run.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = tx.send(Err(map_error(e))).await;
                }
                Err(e) => {
                    let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                }
            }
        });
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(out)))
    }

    async fn scan(&self, request: Request<ScanRequest>) -> Result<Response<ScanResponse>, Status> {
        let reanalyze = request.into_inner().reanalyze;
        let outcome = self.handle.scan_with(reanalyze).await.map_err(map_error)?;
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
            genre_ai: inv
                .genre_ai
                .into_iter()
                .map(|(label, count)| library::Bucket { label, count: count as u32 })
                .collect(),
            mood: inv
                .moods
                .into_iter()
                .map(|(label, count)| library::Bucket { label, count: count as u32 })
                .collect(),
        }))
    }
}
