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
use crate::resolver::{Epoch, Origin};

// Keep the existing public path available to callers.
pub use crate::proto::schedule;

use schedule::schedule_service_server::ScheduleService;
use schedule::{
    ApplyGridRequest, ApplyGridResponse, CheckCoverageRequest, CheckCoverageResponse, ClockStatus,
    Decision, ExportGridRequest, ExportGridResponse, GridFile, ListRulesRequest, ListRulesResponse,
    PreviewRequest, PreviewResponse, ResolveNextRequest, SetClockRequest, ValidateGridResponse,
};

pub struct ScheduleGrpc {
    engine: GridEngine,
}

impl ScheduleGrpc {
    pub fn new(engine: GridEngine) -> Self {
        Self { engine }
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
        EngineError::Selection(S::PlaylistNotFound(_)) | EngineError::Selection(S::PoolEmpty) => {
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

fn map_coverage_member(
    m: crate::grid_engine::CoverageMember,
) -> Result<schedule::CoverageMember, Status> {
    Ok(schedule::CoverageMember {
        r#ref: m.r#ref,
        selected_count: m.stats.selected_count,
        total_duration: m.stats.total_duration_ms.map(ms_to_duration).transpose()?,
        verdict: map_verdict(m.verdict) as i32,
        detail: m.detail,
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

        Ok(Response::new(Decision {
            // The grid resolves a source; the selection stage turns that
            // playlist_ref into a concrete media file (empty on a fallback).
            media_path: resolved.media_path.unwrap_or_default(),
            playlist_ref: d.playlist_ref.unwrap_or_default(),
            rule_id: d.rule_id.unwrap_or_default(),
            origin: map_origin(d.origin) as i32,
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
        let applied_rule_ids = self
            .engine
            .apply_grid(&files)
            .await
            .map_err(map_grid_op_error)?;
        Ok(Response::new(ApplyGridResponse {
            ok: true,
            applied_rule_ids,
        }))
    }

    /// Dry-run of `apply_grid`: same parse + ref checks, nothing written.
    async fn validate_grid(
        &self,
        request: Request<ApplyGridRequest>,
    ) -> Result<Response<ValidateGridResponse>, Status> {
        let files = grid_files(request.into_inner().files);
        self.engine
            .validate_grid(&files)
            .await
            .map_err(map_grid_op_error)?;
        Ok(Response::new(ValidateGridResponse { ok: true }))
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
        _request: Request<ListRulesRequest>,
    ) -> Result<Response<ListRulesResponse>, Status> {
        let mut rules = self.engine.list_rules().await
            .map_err(|e| Status::internal(e.to_string()))?
            .into_iter().map(map_rule).collect::<Result<Vec<_>, _>>()?;
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
        let preview = self
            .engine
            .preview(from, window_secs)
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
        Ok(Response::new(PreviewResponse { occurrences, indicative }))
    }

    /// Sizing check (read-only): « does the grid have enough media? ». Delegates
    /// to `GridEngine::check_coverage`; a per-entry config problem is reported as
    /// an INSUFFICIENT entry, so a normal response carries the whole picture and
    /// only infrastructure surfaces as an error status.
    async fn check_coverage(
        &self,
        request: Request<CheckCoverageRequest>,
    ) -> Result<Response<CheckCoverageResponse>, Status> {
        let report = self
            .engine
            .check_coverage(&request.into_inner().rule_ids)
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
            playlist_ref, start: Some(wall(start)), end: Some(wall(end)),
        }),
        RuleKind::AtClock { playlist_ref, anchor, mode, expiry_secs } => {
            let (every_minutes, at) = match anchor {
                ClockAnchor::EveryMinutes(n) => (n, None),
                ClockAnchor::At(w) => (0, Some(wall(w))),
            };
            Kind::AtClock(schedule::AtClock {
                playlist_ref, every_minutes, at,
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
    };
    Ok(schedule::Rule { id: rule.id, enabled: rule.enabled, validity: Some(validity), kind: Some(kind) })
}
