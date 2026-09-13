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

use crate::grid_engine::GridEngine;
use crate::resolver::{Epoch, Origin};

// Keep the existing public path available to callers.
pub use crate::proto::schedule;

use schedule::schedule_service_server::ScheduleService;
use schedule::{
    ApplyGridRequest, ApplyGridResponse, Decision, ExportGridRequest, ExportGridResponse,
    ListRulesRequest, ListRulesResponse, PreviewRequest, PreviewResponse, ResolveNextRequest,
    ValidateGridResponse,
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

#[tonic::async_trait]
impl ScheduleService for ScheduleGrpc {
    /// The live resolver: what to pull at a track boundary. `now` absent means
    /// "now" (the Liquidsoap path); a supplied instant drives a deterministic
    /// query (the seed of `preview`, and of testing without the wall clock).
    async fn resolve_next(
        &self,
        request: Request<ResolveNextRequest>,
    ) -> Result<Response<Decision>, Status> {
        let now = match request.into_inner().now {
            Some(ts) => Epoch(ts.seconds),
            None => Epoch(now_epoch_seconds()),
        };
        let decision = self
            .engine
            .next(now)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(Decision {
            // The grid resolves a source; turning a playlist_ref into a concrete
            // media file is the downstream selection stage, not wired here yet.
            media_path: String::new(),
            playlist_ref: decision.playlist_ref.unwrap_or_default(),
            rule_id: decision.rule_id.unwrap_or_default(),
            origin: map_origin(decision.origin) as i32,
        }))
    }

    async fn apply_grid(
        &self,
        _request: Request<ApplyGridRequest>,
    ) -> Result<Response<ApplyGridResponse>, Status> {
        Err(Status::unimplemented(
            "grid apply awaits the grid TOML grammar",
        ))
    }

    async fn validate_grid(
        &self,
        _request: Request<ApplyGridRequest>,
    ) -> Result<Response<ValidateGridResponse>, Status> {
        Err(Status::unimplemented(
            "grid validate awaits the grid TOML grammar",
        ))
    }

    async fn export_grid(
        &self,
        _request: Request<ExportGridRequest>,
    ) -> Result<Response<ExportGridResponse>, Status> {
        Err(Status::unimplemented(
            "grid export awaits the grid TOML grammar",
        ))
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

    async fn preview(
        &self,
        _request: Request<PreviewRequest>,
    ) -> Result<Response<PreviewResponse>, Status> {
        Err(Status::unimplemented(
            "preview is a grid projection, not yet implemented",
        ))
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
