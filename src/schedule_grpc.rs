//! gRPC transport for the dedicated scheduling service (schedule_v1.proto).
//!
//! Thin translator over `GridEngine`, same discipline as `grpc.rs`: no metier
//! here, just map the proto to the engine and back. Served on the same tonic
//! server as `Station` (one port, two services).
//!
//! `resolve_next` is the live path Liquidsoap hits at each track boundary.
//! `list_rules` reads the index without advancing playback. Grid mutations
//! and `preview` remain UNIMPLEMENTED pending their respective implementations.

use tonic::{Request, Response, Status};

use crate::grid_engine::{EngineError, GridEngine, GridOpError};
use crate::grid_files::{GridFileError, GridFiles};
use crate::resolver::{Epoch, Origin};

// Keep the existing public path available to callers.
pub use crate::proto::schedule;

use schedule::schedule_service_server::ScheduleService;
use schedule::{
    ApplyGridRequest, ApplyGridResponse, CheckCoverageRequest, CheckCoverageResponse, ClockStatus,
    Decision, EnqueueRequest, EnqueueResponse, ExportGridRequest, ExportGridResponse, GridFile,
    ListRulesRequest, ListRulesResponse, PreviewRequest, PreviewResponse, ResolveNextRequest,
    SetClockRequest, ValidateGridResponse,
};

pub struct ScheduleGrpc {
    engine: GridEngine,
    /// The grid files (`[grid] path`). Absent (tests): the file RPCs answer
    /// `failed_precondition`, `ApplyGrid` applies without writing a file.
    files: Option<GridFiles>,
}

impl ScheduleGrpc {
    pub fn new(engine: GridEngine) -> Self {
        Self { engine, files: None }
    }

    /// With the grid files: the engine is theirs.
    pub fn with_files(files: GridFiles) -> Self {
        Self { engine: files.engine().clone(), files: Some(files) }
    }

    #[allow(clippy::result_large_err)] // a gRPC handler's error is a Status
    fn files(&self) -> Result<&GridFiles, Status> {
        self.files.as_ref().ok_or_else(|| Status::failed_precondition("no grid directory configured"))
    }

    /// The grid to project / size: a draft, a grid file, or the applied one.
    async fn source(&self, grid: &str, draft: &str) -> Result<Option<crate::resolver::Grid>, Status> {
        if grid.trim().is_empty() && draft.is_empty() {
            return Ok(None);
        }
        match &self.files {
            Some(f) => f.source(grid, draft).await.map_err(map_file_error),
            None if !draft.is_empty() => {
                let (rules, diags) = crate::grid_toml::diagnose(draft);
                match rules {
                    Some(rules) => Ok(Some(crate::resolver::Grid { rules })),
                    None => Err(Status::invalid_argument(
                        diags.iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("; "),
                    )),
                }
            }
            None => Err(Status::failed_precondition("no grid directory configured")),
        }
    }
}

/// A grid problem as sent: opcode + field, never a sentence to show (D12).
fn map_grid_diag(file: &str, d: crate::grid_toml::GridDiag) -> schedule::GridDiagnostic {
    use crate::grid_toml::GridCode as G;
    use schedule::grid_diagnostic::Code as C;
    let code = match d.code {
        G::Syntax => C::Syntax,
        G::UnknownField => C::UnknownField,
        G::MissingField => C::MissingField,
        G::BadValue => C::BadValue,
        G::NotAllowed => C::NotAllowed,
        G::Conflict => C::Conflict,
        G::BadDuration => C::BadDuration,
        G::BadTime => C::BadTime,
        G::BadDate => C::BadDate,
        G::BadWeekday => C::BadWeekday,
        G::SchemaVersion => C::SchemaVersion,
        G::DuplicateId => C::DuplicateId,
        G::SeveralFloors => C::SeveralFloors,
        G::ZeroWindow => C::ZeroWindow,
        G::DatesReversed => C::DatesReversed,
        G::OutOfRange => C::OutOfRange,
        G::UnknownRef => C::UnknownRef,
        G::BadRef => C::BadRef,
        G::UnknownDj => C::UnknownDj,
        G::NoLive => C::NoLive,
        G::DjFileUnreadable => C::DjFileUnreadable,
        G::FileUnreadable => C::FileUnreadable,
    };
    schedule::GridDiagnostic {
        file: file.to_string(),
        field_path: d.field,
        rule_id: d.rule_id,
        rejected: d.rejected,
        expected: d.expected,
        message: d.message,
        code: code as i32,
    }
}

fn map_file_error(e: GridFileError) -> Status {
    match e {
        GridFileError::BadName(m) => Status::invalid_argument(m),
        GridFileError::NotFound(n) => Status::not_found(format!("no grid `{n}` in the grid directory")),
        e @ GridFileError::Invalid { .. } => Status::invalid_argument(e.to_string()),
        e => Status::internal(e.to_string()),
    }
}

fn now_epoch_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A rejected grid is the caller's fault → `invalid_argument`; an
/// infrastructure failure is ours → `internal`.
fn map_grid_op_error(e: GridOpError) -> Status {
    match e {
        GridOpError::Invalid(msgs) => Status::invalid_argument(msgs.join("; ")),
        GridOpError::Infra(inner) => Status::internal(inner.to_string()),
    }
}

/// Map a live-resolve error to a status. A selection problem is the caller's
/// (grid/playlist config): unknown ref or empty pool → `failed_precondition`,
/// unsupported-yet mode/order/filter → `unimplemented`, a mistyped filter or
/// unparsable stored TOML → `invalid_argument`. Infrastructure is ours →
/// `internal`.
fn map_next_error(e: EngineError) -> Status {
    use crate::selection::SelectionError as S;
    match &e {
        EngineError::Selection(S::PlaylistNotFound(_))
        | EngineError::Selection(S::PoolEmpty)
        | EngineError::Selection(S::NotAQueue(..)) => {
            Status::failed_precondition(e.to_string())
        }
        EngineError::Selection(S::UnsupportedMode(_))
        | EngineError::Selection(S::UnsupportedOrder(_))
        | EngineError::Selection(S::UnsupportedFilter { .. }) => Status::unimplemented(e.to_string()),
        EngineError::Selection(S::BadFilterValue { .. }) | EngineError::Selection(S::Parse(_)) => {
            Status::invalid_argument(e.to_string())
        }
        _ => Status::internal(e.to_string()),
    }
}

fn grid_files(files: Vec<GridFile>) -> Vec<(String, String)> {
    files.into_iter().map(|f| (f.path, f.toml)).collect()
}

fn map_origin(o: Origin) -> schedule::decision::Origin {
    use schedule::decision::Origin as P;
    match o {
        Origin::AtClockHard => P::AtClockHard,
        Origin::AtClockSoft => P::AtClockSoft,
        Origin::Every => P::Every,
        Origin::DayPart => P::DayPart,
        Origin::BaseRotation => P::BaseRotation,
        Origin::Fallback => P::Fallback,
    }
}

fn strategy_name(s: crate::playlist::Strategy) -> &'static str {
    use crate::playlist::Strategy;
    match s {
        Strategy::Sequence => "sequence",
        Strategy::Shuffle => "shuffle",
        Strategy::Weighted => "weighted",
        Strategy::Rotate => "rotate",
    }
}

fn secs_to_duration(secs: u64) -> prost_types::Duration {
    prost_types::Duration { seconds: secs as i64, nanos: 0 }
}

fn ms_to_duration(ms: u64) -> Result<prost_types::Duration, Status> {
    if ms > 315_576_000_000_000 {
        return Err(Status::internal("pool duration exceeds protobuf range"));
    }
    Ok(prost_types::Duration {
        seconds: (ms / 1000) as i64,
        nanos: ((ms % 1000) * 1_000_000) as i32,
    })
}

fn map_group_member(m: crate::pool_inspection::InspectedMember) -> Result<schedule::GroupMember, Status> {
    use crate::playlist::MemberQuota;
    let quota = m.quota.map(|q| match q {
        MemberQuota::Take(n) => schedule::group_member::Quota::Take(n),
        MemberQuota::TakeRandom { min, max } => schedule::group_member::Quota::TakeRandom(
            schedule::TakeRandomQuota { min, max }),
        MemberQuota::Runtime(secs) => schedule::group_member::Quota::Runtime(secs_to_duration(secs)),
    });
    Ok(schedule::GroupMember {
        r#ref: m.r#ref,
        quota,
        offset: m.offset_secs.map(secs_to_duration),
        selected_count: m.stats.selected_count,
        total_duration: m.stats.total_duration_ms.map(ms_to_duration).transpose()?,
    })
}

fn map_verdict(v: crate::grid_engine::Verdict) -> schedule::Verdict {
    use crate::grid_engine::Verdict as V;
    match v {
        V::Ok => schedule::Verdict::Ok,
        V::Thin => schedule::Verdict::Thin,
        V::Insufficient => schedule::Verdict::Insufficient,
    }
}

/// A coverage reason as an opcode + typed parameters (D12: no text to show).
fn map_reason(r: crate::grid_engine::Reason) -> schedule::CoverageReason {
    use crate::grid_engine::Reason as R;
    use schedule::coverage_reason::Code;
    let mut out = schedule::CoverageReason::default();
    let code = match r {
        R::PoolEmpty => Code::PoolEmpty,
        R::TrackRepeat { window, pool_ms } => {
            out.window = window;
            out.pool_ms = Some(pool_ms);
            Code::TrackRepeat
        }
        R::TitleRepeat { window, pool_ms } => {
            out.window = window;
            out.pool_ms = Some(pool_ms);
            Code::TitleRepeat
        }
        R::ArtistRepeat { artists } => {
            out.count = Some(artists);
            Code::ArtistRepeat
        }
        R::ArtistNotEvaluated => Code::ArtistNotEvaluated,
        R::LimitUnmet { limit, count } => {
            out.limit = Some(limit);
            out.count = Some(count);
            Code::LimitUnmet
        }
        R::FiniteShort { pool_ms, need_ms } => {
            out.pool_ms = Some(pool_ms);
            out.need_ms = Some(need_ms);
            Code::FiniteShort
        }
        R::MembersEmptyAbort { refs } => {
            out.refs = refs;
            Code::MembersEmptyAbort
        }
        R::MembersEmptySkip { refs } => {
            out.refs = refs;
            Code::MembersEmptySkip
        }
        R::MembersLoop { refs } => {
            out.refs = refs;
            Code::MembersLoop
        }
        R::BadRef { error } => {
            out.error = error;
            Code::BadRef
        }
        R::UnknownPlaylist => Code::UnknownPlaylist,
        R::UnreadablePlaylist { error } => {
            out.error = error;
            Code::UnreadablePlaylist
        }
        R::Unresolvable { error } => {
            out.error = error;
            Code::Unresolvable
        }
        R::RuntimeLoop { need_ms, pool_ms } => {
            out.need_ms = Some(need_ms);
            out.pool_ms = Some(pool_ms);
            Code::RuntimeLoop
        }
        R::TakeRepeat { take, count } => {
            out.limit = Some(take);
            out.count = Some(count);
            Code::TakeRepeat
        }
    };
    out.code = code as i32;
    out
}

fn map_coverage_member(
    m: crate::grid_engine::CoverageMember,
) -> Result<schedule::CoverageMember, Status> {
    Ok(schedule::CoverageMember {
        r#ref: m.r#ref,
        selected_count: m.stats.selected_count,
        total_duration: m.stats.total_duration_ms.map(ms_to_duration).transpose()?,
        verdict: map_verdict(m.verdict) as i32,
        detail: m.detail,
        reasons: m.reasons.into_iter().map(map_reason).collect(),
    })
}

fn map_coverage_entry(
    e: crate::grid_engine::CoverageEntry,
) -> Result<schedule::CoverageEntry, Status> {
    Ok(schedule::CoverageEntry {
        rule_id: e.rule_id,
        playlist_ref: e.playlist_ref,
        kind: e.kind.to_string(),
        selected_count: e.stats.selected_count,
        total_duration: e.stats.total_duration_ms.map(ms_to_duration).transpose()?,
        verdict: map_verdict(e.verdict) as i32,
        detail: e.detail,
        reasons: e.reasons.into_iter().map(map_reason).collect(),
        members: e
            .members
            .into_iter()
            .map(map_coverage_member)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

#[tonic::async_trait]
impl ScheduleService for ScheduleGrpc {
    /// The live resolver: what to pull at a track boundary. `now` absent means
    /// "now" (the Liquidsoap path); a supplied instant drives a deterministic
    /// query (the seed of `preview`, and of testing without the wall clock).
    async fn resolve_next(
        &self,
        request: Request<ResolveNextRequest>,
    ) -> Result<Response<Decision>, Status> {
        let now = self
            .engine
            .effective_now(request.into_inner().now.map(|ts| Epoch(ts.seconds)));
        let resolved = self
            .engine
            .next_media(now)
            .await
            .map_err(map_next_error)?;
        let d = resolved.decision;
        // Two layers sit above the grid origin: a halted station (nothing to
        // play, LS must not fill) and the override queue.
        let origin = if resolved.halted.is_some() {
            schedule::decision::Origin::Halted
        } else if resolved.override_source.is_some() {
            schedule::decision::Origin::Override
        } else {
            map_origin(d.origin)
        };

        Ok(Response::new(Decision {
            // The grid resolves a source; the selection stage turns that
            // playlist_ref into a concrete media file (empty on a fallback).
            media_path: resolved.media_path.unwrap_or_default(),
            playlist_ref: d.playlist_ref.unwrap_or_default(),
            rule_id: d.rule_id.unwrap_or_default(),
            origin: origin as i32,
            override_source: resolved.override_source.unwrap_or_default(),
            halted_state: resolved
                .halted
                .map(|s| s.as_str().to_string())
                .unwrap_or_default(),
        }))
    }

    /// Validate + install a grid (family A rebuilt, family B preserved). The
    /// TOML grammar and its checks live in `grid_toml`/`GridEngine`; this is a
    /// thin translator.
    async fn apply_grid(
        &self,
        request: Request<ApplyGridRequest>,
    ) -> Result<Response<ApplyGridResponse>, Status> {
        let files = grid_files(request.into_inner().files);
        let Some(gf) = &self.files else {
            let applied_rule_ids = self.engine.apply_grid(&files).await.map_err(map_grid_op_error)?;
            return Ok(Response::new(ApplyGridResponse { ok: true, applied_rule_ids, ..Default::default() }));
        };
        match gf.apply_external(&files).await {
            Ok((grid, applied_rule_ids)) => {
                Ok(Response::new(ApplyGridResponse { ok: true, applied_rule_ids, diagnostics: Vec::new(), grid }))
            }
            Err(GridFileError::Invalid { name, diags }) => Ok(Response::new(ApplyGridResponse {
                ok: false,
                diagnostics: diags.into_iter().map(|d| map_grid_diag(&name, d)).collect(),
                ..Default::default()
            })),
            Err(e) => Err(map_file_error(e)),
        }
    }

    /// Dry-run of `apply_grid`: every problem of each file, nothing written.
    async fn validate_grid(
        &self,
        request: Request<ApplyGridRequest>,
    ) -> Result<Response<ValidateGridResponse>, Status> {
        let files = grid_files(request.into_inner().files);
        let mut diagnostics = Vec::new();
        let mut all = Vec::new();
        for (path, text) in &files {
            let (items, mut diags) = crate::grid_toml::diagnose_partial(text);
            let refs: Vec<(usize, &crate::resolver::Rule)> = items.iter().map(|(n, r)| (*n, r)).collect();
            diags.extend(self.engine.diagnose_rules_at(&refs).await.map_err(|e| Status::internal(e.to_string()))?);
            // File order: a rule's grammar and refs together.
            crate::grid_toml::in_file_order(&mut diags);
            diagnostics.extend(diags.into_iter().map(|d| map_grid_diag(path, d)));
            all.extend(items.into_iter().map(|(_, r)| r));
        }
        // Across files (several files merged into one grid): ids, floor.
        if files.len() > 1 && diagnostics.is_empty() {
            diagnostics.extend(crate::grid_toml::diagnose_set(&all).into_iter().map(|mut d| {
                d.field.clear(); // a position in the merged set means nothing in a file
                map_grid_diag("", d)
            }));
        }
        Ok(Response::new(ValidateGridResponse { ok: diagnostics.is_empty(), diagnostics }))
    }

    async fn list_grids(
        &self,
        _request: Request<schedule::ListGridsRequest>,
    ) -> Result<Response<schedule::ListGridsResponse>, Status> {
        let f = self.files()?;
        let grids = f.list().await.map_err(map_file_error)?;
        let active = grids.iter().find(|g| g.active).map(|g| g.name.clone()).unwrap_or_default();
        Ok(Response::new(schedule::ListGridsResponse {
            grids: grids
                .into_iter()
                .map(|g| {
                    let problem = g.problem.map(|d| map_grid_diag(&g.name, d));
                    schedule::GridInfo {
                        name: g.name,
                        revision: g.revision,
                        active: g.active,
                        rules: g.rules.map(|n| n as u32),
                        problem,
                    }
                })
                .collect(),
            active,
        }))
    }

    async fn get_grid(
        &self,
        request: Request<schedule::GetGridRequest>,
    ) -> Result<Response<schedule::GetGridResponse>, Status> {
        let g = self.files()?.get(&request.into_inner().name).await.map_err(map_file_error)?;
        Ok(Response::new(schedule::GetGridResponse {
            name: g.name,
            toml: g.toml,
            revision: g.revision,
            active: g.active,
            exists: g.exists,
            differs_from_applied: g.differs_from_applied,
        }))
    }

    async fn save_grid(
        &self,
        request: Request<schedule::SaveGridRequest>,
    ) -> Result<Response<schedule::SaveGridResponse>, Status> {
        let req = request.into_inner();
        let s = self.files()?.save(&req.name, &req.toml, &req.expected_revision).await.map_err(map_file_error)?;
        let name = crate::grid_files::normalize_name(&req.name).unwrap_or(req.name);
        Ok(Response::new(schedule::SaveGridResponse {
            ok: s.ok,
            conflict: s.conflict,
            revision: s.revision,
            diagnostics: s.diagnostics.into_iter().map(|d| map_grid_diag(&name, d)).collect(),
            created: s.created,
            applied: s.applied,
        }))
    }

    async fn activate_grid(
        &self,
        request: Request<schedule::ActivateGridRequest>,
    ) -> Result<Response<schedule::ActivateGridResponse>, Status> {
        let f = self.files()?;
        match f.activate(&request.into_inner().name).await {
            Ok(n) => Ok(Response::new(schedule::ActivateGridResponse {
                ok: true,
                name: f.active().await.map_err(map_file_error)?,
                rules: n as u32,
                ..Default::default()
            })),
            Err(GridFileError::Invalid { name, diags }) => Ok(Response::new(schedule::ActivateGridResponse {
                ok: false,
                name: f.active().await.map_err(map_file_error)?,
                diagnostics: diags.into_iter().map(|d| map_grid_diag(&name, d)).collect(),
                ..Default::default()
            })),
            Err(e) => Err(map_file_error(e)),
        }
    }

    async fn reload_grid(
        &self,
        _request: Request<schedule::ReloadGridRequest>,
    ) -> Result<Response<schedule::ActivateGridResponse>, Status> {
        let f = self.files()?;
        let name = f.active().await.map_err(map_file_error)?;
        match f.reload().await {
            Ok(crate::grid_files::Loaded::Applied(n)) => {
                Ok(Response::new(schedule::ActivateGridResponse { ok: true, name, rules: n as u32, ..Default::default() }))
            }
            Ok(crate::grid_files::Loaded::Missing) => {
                Ok(Response::new(schedule::ActivateGridResponse { ok: false, name, missing: true, ..Default::default() }))
            }
            Err(GridFileError::Invalid { name, diags }) => Ok(Response::new(schedule::ActivateGridResponse {
                ok: false,
                diagnostics: diags.into_iter().map(|d| map_grid_diag(&name, d)).collect(),
                name,
                ..Default::default()
            })),
            Err(e) => Err(map_file_error(e)),
        }
    }

    /// Project the current index back to a single `grid.toml`.
    async fn export_grid(
        &self,
        request: Request<ExportGridRequest>,
    ) -> Result<Response<ExportGridResponse>, Status> {
        let toml = self
            .engine
            .export_grid(&request.into_inner().rule_ids)
            .await
            .map_err(map_grid_op_error)?;
        Ok(Response::new(ExportGridResponse {
            files: vec![GridFile {
                path: "grid.toml".to_string(),
                toml,
            }],
        }))
    }

    async fn list_rules(
        &self,
        request: Request<ListRulesRequest>,
    ) -> Result<Response<ListRulesResponse>, Status> {
        let req = request.into_inner();
        let rules = match self.source(&req.grid, &req.draft_toml).await? {
            Some(g) => g.rules,
            None => self.engine.list_rules().await.map_err(|e| Status::internal(e.to_string()))?,
        };
        let mut rules = rules.into_iter().map(map_rule).collect::<Result<Vec<_>, _>>()?;
        rules.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Response::new(ListRulesResponse { rules }))
    }

    /// Project the grid over a window (default 24h). A pure clock projection
    /// (`GridEngine::preview`): occurrences carry epoch UTC **and** a local
    /// rendering, so a window over a DST night shows the hole/doubling.
    async fn preview(
        &self,
        request: Request<PreviewRequest>,
    ) -> Result<Response<PreviewResponse>, Status> {
        let req = request.into_inner();
        let from = match req.from {
            Some(ts) => Epoch(ts.seconds),
            None => Epoch(now_epoch_seconds()),
        };
        // Absent window → 24h; a non-positive window yields no occurrences.
        let window_secs = req.window.map(|d| d.seconds).unwrap_or(24 * 3600);
        let grid = self.source(&req.grid, &req.draft_toml).await?;
        let preview = self
            .engine
            .preview_of(grid, from, window_secs)
            .await
            .map_err(map_next_error)?;
        let occurrences = preview
            .occurrences
            .into_iter()
            .map(|o| {
                // A group occurrence carries its member decomposition; a
                // non-group one leaves strategy empty / members absent.
                let (strategy, members) = match o.group {
                    Some(g) => (
                        strategy_name(g.strategy).to_string(),
                        g.members.into_iter().map(map_group_member).collect::<Result<Vec<_>, _>>()?,
                    ),
                    None => (String::new(), Vec::new()),
                };
                Ok(schedule::Occurrence {
                    at_utc: Some(prost_types::Timestamp { seconds: o.epoch.0, nanos: 0 }),
                    at_local: o.at_local,
                    rule_id: o.rule_id,
                    playlist_ref: o.playlist_ref,
                    origin: map_origin(o.origin) as i32,
                    strategy,
                    members,
                    selected_count: o.pool.selected_count,
                    total_duration: o.pool.total_duration_ms.map(ms_to_duration).transpose()?,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        let indicative = preview
            .indicative
            .into_iter()
            .map(|i| schedule::IndicativeRule {
                rule_id: i.rule_id,
                playlist_ref: i.playlist_ref,
            })
            .collect();
        let live = preview
            .live
            .into_iter()
            .map(|w| {
                let (closes_at, closes_local) = match w.closes {
                    Some((e, l)) => (Some(prost_types::Timestamp { seconds: e.0, nanos: 0 }), l),
                    None => (None, String::new()),
                };
                schedule::LiveWindow {
                    rule_id: w.rule_id,
                    dj: w.dj,
                    opens_at: Some(prost_types::Timestamp { seconds: w.opens.0, nanos: 0 }),
                    opens_local: w.opens_local,
                    open_before: w.open_before,
                    closes_at,
                    closes_local,
                }
            })
            .collect();
        Ok(Response::new(PreviewResponse { occurrences, indicative, live }))
    }

    /// Sizing check (read-only): « does the grid have enough media? ». Delegates
    /// to `GridEngine::check_coverage`; a per-entry config problem is reported as
    /// an INSUFFICIENT entry, so a normal response carries the whole picture and
    /// only infrastructure surfaces as an error status.
    async fn check_coverage(
        &self,
        request: Request<CheckCoverageRequest>,
    ) -> Result<Response<CheckCoverageResponse>, Status> {
        let req = request.into_inner();
        let grid = self.source(&req.grid, &req.draft_toml).await?;
        let report = self
            .engine
            .check_coverage_of(grid, &req.rule_ids)
            .await
            .map_err(map_next_error)?;
        let entries = report
            .entries
            .into_iter()
            .map(map_coverage_entry)
            .collect::<Result<Vec<_>, Status>>()?;
        Ok(Response::new(CheckCoverageResponse {
            entries,
            worst: map_verdict(report.worst) as i32,
        }))
    }

    /// Enqueue a media into a `queue` playlist's runtime buffer. Delegates to
    /// `GridEngine::enqueue`; `accepted = false` means the queue was at max_len.
    async fn enqueue(
        &self,
        request: Request<EnqueueRequest>,
    ) -> Result<Response<EnqueueResponse>, Status> {
        let req = request.into_inner();
        let out = self
            .engine
            .enqueue(&req.playlist_ref, &req.media_path)
            .await
            .map_err(map_next_error)?;
        Ok(Response::new(EnqueueResponse {
            accepted: out.accepted,
            len: out.len,
        }))
    }

    /// Manual clock (testing): freeze/release the instant `resolve_next` uses
    /// when no explicit `now` is given. No change requested → report only.
    async fn set_clock(
        &self,
        request: Request<SetClockRequest>,
    ) -> Result<Response<ClockStatus>, Status> {
        let req = request.into_inner();
        if req.real {
            self.engine.set_clock(None);
        } else if !req.at.trim().is_empty() {
            self.engine
                .set_clock_civil(&req.at)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
        }
        let effective = self.engine.effective_now(None);
        let effective_local = self.engine.render_local(effective).unwrap_or_default();
        Ok(Response::new(ClockStatus {
            frozen: self.engine.clock_override().is_some(),
            effective: Some(prost_types::Timestamp { seconds: effective.0, nanos: 0 }),
            effective_local,
        }))
    }
}

/// Transport conversion only: never evaluate a rule to display it.
fn map_rule(rule: crate::resolver::Rule) -> Result<schedule::Rule, Status> {
    use crate::resolver::{Cadence, ClockAnchor, Mode, RuleKind, Weekday};
    use schedule::rule::Kind;
    let wall = |w: crate::resolver::WallClock| schedule::WallClock {
        hour: u32::from(w.hour), minute: u32::from(w.minute),
    };
    let date = |d: crate::resolver::Date| format!("{:04}-{:02}-{:02}", d.year, d.month, d.day);
    let duration = |seconds: u64| -> Result<prost_types::Duration, Status> {
        // google.protobuf.Duration's documented maximum is 10,000 years.
        if seconds > 315_576_000_000 {
            return Err(Status::internal("rule duration exceeds protobuf range"));
        }
        Ok(prost_types::Duration { seconds: seconds as i64, nanos: 0 })
    };
    let mut days: Vec<i32> = rule.validity.days.into_iter().map(|d| {
        (match d {
            Weekday::Mon => schedule::Weekday::Mon,
            Weekday::Tue => schedule::Weekday::Tue,
            Weekday::Wed => schedule::Weekday::Wed,
            Weekday::Thu => schedule::Weekday::Thu,
            Weekday::Fri => schedule::Weekday::Fri,
            Weekday::Sat => schedule::Weekday::Sat,
            Weekday::Sun => schedule::Weekday::Sun,
        }) as i32
    }).collect();
    days.sort_unstable();
    let validity = schedule::Validity {
        days,
        date_start: rule.validity.date_start.map(date).unwrap_or_default(),
        date_end: rule.validity.date_end.map(date).unwrap_or_default(),
    };
    let kind = match rule.kind {
        RuleKind::BaseRotation { playlist_ref } => Kind::BaseRotation(schedule::BaseRotation { playlist_ref }),
        RuleKind::DayPart { playlist_ref, start, end } => Kind::DayPart(schedule::DayPart {
            playlist_ref, start: Some(wall(start)), end: end.map(wall),
        }),
        RuleKind::AtClock { playlist_ref, anchor, mode, expiry_secs } => {
            let (every_minutes, minute, at) = match anchor {
                ClockAnchor::EveryMinutes(n) => (n, None, None),
                ClockAnchor::Minute(m) => (0, Some(u32::from(m)), None),
                ClockAnchor::At(w) => (0, None, Some(wall(w))),
            };
            Kind::AtClock(schedule::AtClock {
                playlist_ref, every_minutes, minute, at,
                mode: match mode { Mode::Soft => schedule::at_clock::Mode::Soft as i32,
                                   Mode::Hard => schedule::at_clock::Mode::Hard as i32 },
                expiry: expiry_secs.map(duration).transpose()?,
            })
        }
        RuleKind::Every { playlist_ref, cadence } => Kind::Every(schedule::Every {
            playlist_ref,
            cadence: Some(match cadence {
                Cadence::Tracks(n) => schedule::every::Cadence::Tracks(n),
                Cadence::Elapsed(s) => schedule::every::Cadence::Elapsed(duration(s)?),
            }),
        }),
        RuleKind::Live { dj, start } => Kind::Live(schedule::Live { dj, start: Some(wall(start)) }),
    };
    Ok(schedule::Rule { id: rule.id, enabled: rule.enabled, validity: Some(validity), kind: Some(kind) })
}
