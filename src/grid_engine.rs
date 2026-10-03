//! Grid engine: the loop that turns the pure resolver into a live decision.
//!
//! `resolver::resolve_next` is pure and timezone-free; this is the I/O shell
//! around it. On each call it hydrates the grid (family A) and the playback
//! state (family B) from SQLite, converts `now` to civil local time via
//! `clock`, asks the resolver, then persists the decision's side effects.
//!
//! The resolver being catch-up by construction (it computes from `now` + state,
//! never from a running timer), start-up reconciliation is just `sync_grid`
//! followed by normal `next` calls — no special replay path.
//!
//! Threading note (single writer): today this reloads from SQLite each call,
//! which is correct and simplest. The eventual shape is an owning tokio task
//! holding the grid in memory, invalidated on apply/reload, mutations sent over
//! an mpsc channel — same actor model as the rest of stationd.

use std::collections::HashSet;
use std::path::PathBuf;

use sqlx::SqlitePool;

use crate::clock::{self, ClockError};
use crate::grid_index::{self, GridLoadError};
use crate::grid_store;
use crate::grid_toml;
use crate::selection::TurnStart;
use crate::resolver::{
    resolve_next, resolve_ranked, Cadence, Epoch, EveryState, Grid, GridDecision, LocalNow,
    Origin, PlaybackState, Rule, RuleKind,
};
use crate::station_control::{BroadcastState, Gate, OverrideContent, StationControl};
use crate::store;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Grid(#[from] GridLoadError),
    #[error(transparent)]
    Clock(#[from] ClockError),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Selection(#[from] crate::selection::SelectionError),
}

/// Splits infrastructure failure (→ gRPC `internal`) from a rejected grid
/// (→ `invalid_argument`, and — no-silent-failure — never replaces the last
/// valid grid).
#[derive(Debug, thiserror::Error)]
pub enum GridOpError {
    #[error(transparent)]
    Infra(#[from] EngineError),
    #[error("grid rejected: {}", .0.join("; "))]
    Invalid(Vec<String>),
}

/// Live resolver over a SQLite-backed grid, in the station timezone.
/// Cheap to clone (pool, handles): the gRPC service and the Liquidsoap bridge
/// each hold one; all mutable state lives in SQLite / `StationControl`.
#[derive(Clone)]
pub struct GridEngine {
    pool: SqlitePool,
    /// IANA name of the station timezone (config), e.g. "Europe/Paris".
    tz: String,
    /// Plugins to notify of decisions (best-effort, fire-and-forget). `None`
    /// when no plugin system is wired (e.g. in unit tests).
    plugins: Option<crate::plugin::PluginHandle>,
    /// Media root for the on-resolution existence check. `None` (tests) turns
    /// the check off.
    media_root: Option<PathBuf>,
    /// Station runtime control: broadcast state (the gate before any
    /// resolution), the override queue (consulted before the grid) and the
    /// manual clock. Shared with the plugin host surface and gRPC.
    control: StationControl,
    /// `[live] djs_path`: the DJ file `live` rules are checked against at
    /// validate/apply. `None` = no `[live]` section: a `live` rule is refused.
    live_djs: Option<PathBuf>,
}

/// One entry of a grid projection ([`GridEngine::preview`]): the instant a
/// resolved decision *starts*, in epoch UTC and rendered in station-local
/// time. Empty `rule_id`/`playlist_ref` = the fallback filled in.
#[derive(Debug, Clone)]
pub struct PreviewOccurrence {
    pub epoch: Epoch,
    pub at_local: String,
    pub origin: Origin,
    pub rule_id: String,
    pub playlist_ref: String,
    /// Read-only statistics of the active source, before any plugin filtering.
    pub pool: crate::pool_inspection::PoolStats,
    /// Member pools plus the existing take/runtime offset projection.
    pub group: Option<crate::pool_inspection::InspectedGroup>,
}

/// A grid rule taken into account but not placeable on the clock (a
/// track-counted `Every`, whose cadence rides real playback). Returned
/// alongside the timeline, never injected into the ordered occurrences.
#[derive(Debug, Clone)]
pub struct IndicativeRule {
    pub rule_id: String,
    pub playlist_ref: String,
}

/// A grid projection ([`GridEngine::preview`]): the ordered, clock-driven
/// `occurrences` (real instants only), plus the `indicative` rules that are in
/// play but can't be timed on a clock.
#[derive(Debug, Clone, Default)]
pub struct GridPreview {
    pub occurrences: Vec<PreviewOccurrence>,
    pub indicative: Vec<IndicativeRule>,
    /// Live connection windows over the projection (the grid's `live`
    /// rules, `resolver::live_window`), in opening order. They lay over the
    /// programme, never inside `occurrences`.
    pub live: Vec<LiveProjection>,
}

/// A live DJ connection window seen by a preview: when DJ `dj` may connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveProjection {
    pub rule_id: String,
    pub dj: String,
    /// First minute of the window inside the projection (the window start
    /// when `open_before` is false).
    pub opens: Epoch,
    pub opens_local: String,
    /// Already open at the start of the projection: it opened earlier.
    pub open_before: bool,
    /// First minute it is no longer open; `None` = still open at the end.
    pub closes: Option<(Epoch, String)>,
}

/// A grid decision plus the concrete media it resolves to (see
/// [`GridEngine::next_media`]). `media_path` is `None` only for a fallback
/// decision (no active source → Liquidsoap's safety net fills the air).
#[derive(Debug, Clone)]
pub struct ResolvedDecision {
    pub decision: GridDecision,
    pub media_path: Option<String>,
    /// True when `media_path` is a remote stream URL (a `remote` playlist), not
    /// a local file: the LS wiring relays it via `input.http` rather than
    /// playing a file. `false` for a file or a fallback.
    pub stream: bool,
    /// The station is paused/stopped: nothing to play (media `None`). Distinct
    /// from a fallback — Liquidsoap must NOT fill the air here.
    pub halted: Option<BroadcastState>,
    /// This media came from the override queue, pushed by this source (a
    /// plugin name or `cli`); the grid was bypassed for this pull.
    pub override_source: Option<String>,
    /// Canonical key of the LEAF playlist that produced `media_path` (the
    /// member of a group, not the group). `None` for a stream, a fallback, a
    /// halt, or a media override. Carried to the end of the track so the
    /// `unplayed_only` mark lands on the right playlist.
    pub leaf_ref: Option<String>,
    /// Row of this track in `broadcast_log`, to stamp its real air start
    /// (`mark_aired`). `None` when nothing was logged (stream, halt, fallback).
    pub log_id: Option<i64>,
}

// ───────────────────────────────────────────────────────────────────────────
// Coverage check: « does the grid have enough media? » ([`GridEngine::check_coverage`])
// A read-only sizing pass, NOT a playout: for each rule it inspects the pool of
// the playlist it references and grades it. Two axes, worst wins:
//   A. the playlist's own demands   — anti-repetition window, `limit`;
//   B. the grid's temporal demand   — a FINITE source shorter than its slot.
// « Signalé, pas jugé »: it qualifies, it never blocks a commit.
// ───────────────────────────────────────────────────────────────────────────

use crate::pool_inspection::{self, PoolInspection, PoolStats};

/// Sizing verdict for one grid entry (or group member). Ordered by severity;
/// the grid's overall verdict is the worst across its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verdict {
    #[default]
    Ok,
    Thin,
    Insufficient,
}

impl Verdict {
    fn rank(self) -> u8 {
        match self {
            Verdict::Ok => 0,
            Verdict::Thin => 1,
            Verdict::Insufficient => 2,
        }
    }
    /// The more severe of the two.
    pub fn worst(self, other: Verdict) -> Verdict {
        if other.rank() > self.rank() { other } else { self }
    }
}

/// One group member, sized on its own — locates the weak link inside a group.
#[derive(Debug, Clone)]
pub struct CoverageMember {
    pub r#ref: String,
    pub stats: PoolStats,
    pub verdict: Verdict,
    pub detail: String,
    /// What made the verdict, as opcodes (a client translates them; `detail`
    /// is their French rendering for `stationctl`). Empty = ok.
    pub reasons: Vec<Reason>,
}

/// One grid rule and the sizing of the pool its playlist resolves to.
#[derive(Debug, Clone)]
pub struct CoverageEntry {
    pub rule_id: String,
    pub playlist_ref: String,
    pub kind: &'static str,
    pub stats: PoolStats,
    pub verdict: Verdict,
    pub detail: String,
    /// What made the verdict, as opcodes ([`CoverageMember::reasons`]).
    pub reasons: Vec<Reason>,
    pub members: Vec<CoverageMember>,
}

/// One cause of a coverage verdict — an opcode with typed parameters, never
/// a sentence: clients translate (dossier TUI, D12). [`Reason::text`] is the
/// French line `stationctl schedule check` has always printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The pool holds no media.
    PoolEmpty,
    /// `no_same_track_within` longer than the pool: a track replays.
    TrackRepeat { window: String, pool_ms: u64 },
    /// `no_same_title_within` longer than the pool: a song replays.
    TitleRepeat { window: String, pool_ms: u64 },
    /// `no_same_artist_within` with fewer than two distinct artists.
    ArtistRepeat { artists: u64 },
    /// `no_same_artist_within` not evaluated (group aggregate). Not a fault.
    ArtistNotEvaluated,
    /// `limit` above the number of distinct media.
    LimitUnmet { limit: u64, count: u64 },
    /// A finite source shorter than the slot it must fill.
    FiniteShort { pool_ms: u64, need_ms: u64 },
    /// Empty group members, policy `abort`: the group fails.
    MembersEmptyAbort { refs: Vec<String> },
    /// Empty group members, policy `skip`: degraded.
    MembersEmptySkip { refs: Vec<String> },
    /// Members whose quota exceeds their pool: they loop in their slot.
    MembersLoop { refs: Vec<String> },
    /// The rule's playlist ref is not a valid ref.
    BadRef { error: String },
    /// The rule's playlist is not known.
    UnknownPlaylist,
    /// The playlist's TOML no longer parses.
    UnreadablePlaylist { error: String },
    /// The pool could not be resolved (transitive ref, filter…).
    Unresolvable { error: String },
    /// A member `runtime` budget above its pool: it loops in its slot.
    RuntimeLoop { need_ms: u64, pool_ms: u64 },
    /// A member `take` above its distinct tracks: it repeats.
    TakeRepeat { take: u64, count: u64 },
}

impl Reason {
    /// The French line of `stationctl schedule check` (unchanged wording).
    pub fn text(&self) -> String {
        match self {
            Reason::PoolEmpty => "pool vide".into(),
            Reason::TrackRepeat { window, pool_ms } => format!(
                "no_same_track_within {window} : pool {} < {window} (rejeu de piste forcé)",
                fmt_hms(*pool_ms)
            ),
            Reason::TitleRepeat { window, pool_ms } => format!(
                "no_same_title_within {window} : pool {} < {window} (rejeu de morceau forcé)",
                fmt_hms(*pool_ms)
            ),
            Reason::ArtistRepeat { artists } => {
                format!("no_same_artist_within : {artists} artiste(s) distinct(s) (rejeu d'artiste forcé)")
            }
            Reason::ArtistNotEvaluated => "no_same_artist_within non évalué (agrégat de groupe)".into(),
            Reason::LimitUnmet { limit, count } => {
                format!("limit {limit} : {count} média(s) distinct(s) dans le pool")
            }
            Reason::FiniteShort { pool_ms, need_ms } => format!(
                "source finie {} < créneau {} (ne remplit pas)",
                fmt_hms(*pool_ms),
                fmt_hms(*need_ms)
            ),
            Reason::MembersEmptyAbort { refs } => format!("membre(s) vide(s) [{}] → abort", refs.join(", ")),
            Reason::MembersEmptySkip { refs } => {
                format!("membre(s) vide(s) [{}] → skip (dégradé)", refs.join(", "))
            }
            Reason::MembersLoop { refs } => {
                format!("membre(s) sous-dimensionné(s) [{}] → boucle dans le slot", refs.join(", "))
            }
            Reason::BadRef { error } => format!("ref invalide : {error}"),
            Reason::UnknownPlaylist => "ref cassée : playlist inconnue".into(),
            Reason::UnreadablePlaylist { error } => format!("playlist illisible : {error}"),
            Reason::Unresolvable { error } => format!("pool non résolvable : {error}"),
            Reason::RuntimeLoop { need_ms, pool_ms } => format!(
                "budget runtime {} > pool {} → boucle dans le slot",
                fmt_hms(*need_ms),
                fmt_hms(*pool_ms)
            ),
            Reason::TakeRepeat { take, count } => {
                format!("take {take} > {count} piste(s) distincte(s) → répétition")
            }
        }
    }
}

/// `detail` of an entry or member: its reasons in French, or « ok ».
fn detail_of(reasons: &[Reason]) -> String {
    if reasons.is_empty() {
        "ok".to_string()
    } else {
        reasons.iter().map(Reason::text).collect::<Vec<_>>().join(" ; ")
    }
}

/// The whole grid's sizing report ([`GridEngine::check_coverage`]).
#[derive(Debug, Clone, Default)]
pub struct CoverageReport {
    pub entries: Vec<CoverageEntry>,
    pub worst: Verdict,
}

/// What the grid asks a rule's source to cover. `Punctual` = a single track
/// (AtClock/Every marks) — any non-empty pool satisfies it.
enum Demand {
    Duration(i64), // seconds
    Punctual,
}

/// Kind tag, referenced playlist, and temporal demand of a rule (`grid` = the
/// whole grid: an open day part lasts until the next start of another one).
fn classify_rule(rule: &Rule, grid: &Grid) -> (&'static str, String, Demand) {
    match &rule.kind {
        RuleKind::BaseRotation { playlist_ref } => {
            // The floor must sustain a full day without forcing repeats.
            ("base_rotation", playlist_ref.clone(), Demand::Duration(86_400))
        }
        RuleKind::DayPart { playlist_ref, start, end: Some(end) } => {
            ("day_part", playlist_ref.clone(), Demand::Duration(wallclock_window_secs(start, end)))
        }
        RuleKind::DayPart { playlist_ref, start, end: None } => {
            ("day_part", playlist_ref.clone(), Demand::Duration(open_part_nominal_secs(rule, start, grid)))
        }
        RuleKind::AtClock { playlist_ref, .. } => ("at_clock", playlist_ref.clone(), Demand::Punctual),
        RuleKind::Every { playlist_ref, .. } => ("every", playlist_ref.clone(), Demand::Punctual),
        // Not sized (no playlist): `check_coverage` skips live slots.
        RuleKind::Live { dj, .. } => ("live", dj.clone(), Demand::Punctual),
    }
}

/// Window length of a DayPart in seconds, handling a cross-midnight rule
/// (`end ≤ start` → the window wraps to the next day).
fn wallclock_window_secs(start: &crate::resolver::WallClock, end: &crate::resolver::WallClock) -> i64 {
    let s = start.hour as i64 * 3600 + start.minute as i64 * 60;
    let e = end.hour as i64 * 3600 + end.minute as i64 * 60;
    if e > s { e - s } else { e + 86_400 - s }
}

/// Nominal length of an OPEN day part, for sizing: from its start to the next
/// start (on the clock, wrapping past midnight) of another enabled day part.
/// `days` are ignored — the longest gap on a given day may be longer (e.g. a
/// show whose successor does not run every day). No other day part: 24 h.
fn open_part_nominal_secs(rule: &Rule, start: &crate::resolver::WallClock, grid: &Grid) -> i64 {
    let s = start.hour as i64 * 60 + start.minute as i64;
    grid.rules
        .iter()
        .filter(|r| r.enabled && r.id != rule.id)
        .filter_map(|r| match &r.kind {
            RuleKind::DayPart { start: o, .. } => {
                let o = o.hour as i64 * 60 + o.minute as i64;
                Some(if o > s { o - s } else { o + 1440 - s })
            }
            _ => None,
        })
        .min()
        .unwrap_or(1440)
        * 60
}

/// Does the source fill an arbitrary window by looping, or is its playtime
/// bounded? Confirmed rule: `dynamic`/`remote`/rotation groups and any
/// `repeat = true` loop; `static`/`queue` without repeat and a `sequence`
/// group are finite. Only a finite source can under-fill its slot (axis B).
fn source_loops(playlist: &crate::playlist::Playlist) -> bool {
    use crate::playlist::{Mode, Strategy};
    if playlist.broadcast.as_ref().and_then(|b| b.repeat) == Some(true) {
        return true;
    }
    match playlist.selection.mode {
        Mode::Dynamic | Mode::Remote => true,
        Mode::Static | Mode::Queue => false,
        Mode::Group => !matches!(playlist.selection.strategy, Some(Strategy::Sequence)),
    }
}

fn make_entry(
    rule_id: String,
    playlist_ref: String,
    kind: &'static str,
    stats: PoolStats,
    verdict: Verdict,
    reasons: Vec<Reason>,
    members: Vec<CoverageMember>,
) -> CoverageEntry {
    let detail = detail_of(&reasons);
    CoverageEntry { rule_id, playlist_ref, kind, stats, verdict, detail, reasons, members }
}

/// Grade a resolved pool against both axes. Returns the verdict, the reasons
/// that fired, and the per-member breakdown (empty for a leaf).
fn verdict_for(
    playlist: &crate::playlist::Playlist,
    inspection: &PoolInspection,
    demand: Demand,
) -> (Verdict, Vec<Reason>, Vec<CoverageMember>) {
    let stats = inspection.stats;
    let members = member_breakdown(inspection);

    // Empty pool → the rule can never produce: hard fail, nothing else matters.
    if stats.selected_count == Some(0) {
        return (Verdict::Insufficient, vec![Reason::PoolEmpty], members);
    }

    let mut verdict = Verdict::Ok;
    let mut reasons: Vec<Reason> = Vec::new();
    let bc = playlist.broadcast.as_ref();

    // --- Axis A: the playlist's own demands (anti-repetition + limit) ---
    if let Some(c) = bc.and_then(|b| b.constraints.as_ref()) {
        if let Some(d) = &c.no_same_track_within {
            if let (Ok(need_s), Some(have_ms)) =
                (crate::playlist::parse_duration_secs(d), stats.total_duration_ms)
            {
                let need_ms = need_s.saturating_mul(1000);
                if have_ms < need_ms {
                    verdict = verdict.worst(Verdict::Thin);
                    reasons.push(Reason::TrackRepeat { window: d.clone(), pool_ms: have_ms });
                }
            }
        }
        if let Some(d) = &c.no_same_title_within {
            if let (Ok(need_s), Some(have_ms)) =
                (crate::playlist::parse_duration_secs(d), stats.total_duration_ms)
            {
                // Pool duration is an upper bound: copies of one song count
                // once here, so the real margin is smaller.
                let need_ms = need_s.saturating_mul(1000);
                if have_ms < need_ms {
                    verdict = verdict.worst(Verdict::Thin);
                    reasons.push(Reason::TitleRepeat { window: d.clone(), pool_ms: have_ms });
                }
            }
        }
        if c.no_same_artist_within.is_some() {
            match stats.distinct_artists {
                // A pool with fewer than 2 distinct artists can never satisfy a
                // no-same-artist window. (A tighter bound would need the number
                // of tracks per window; this is the honest lower guard.)
                Some(a) if a < 2 => {
                    verdict = verdict.worst(Verdict::Thin);
                    reasons.push(Reason::ArtistRepeat { artists: a });
                }
                None => reasons.push(Reason::ArtistNotEvaluated),
                _ => {}
            }
        }
    }
    if let Some(limit) = bc.and_then(|b| b.limit) {
        if let Some(count) = stats.selected_count {
            if count < limit as u64 {
                verdict = verdict.worst(Verdict::Thin);
                reasons.push(Reason::LimitUnmet { limit: limit as u64, count });
            }
        }
    }

    // --- Axis B: temporal demand of the grid, for FINITE sources only ---
    if !source_loops(playlist) {
        if let Demand::Duration(secs) = demand {
            if let Some(have_ms) = stats.total_duration_ms {
                let need_ms = (secs.max(0) as u64).saturating_mul(1000);
                if have_ms < need_ms {
                    verdict = verdict.worst(Verdict::Thin);
                    reasons.push(Reason::FiniteShort { pool_ms: have_ms, need_ms });
                }
            }
        }
    }

    // --- Group members: emptiness sinks the group; an under-sized quota loops ---
    if inspection.group.is_some() {
        let empty: Vec<String> = members
            .iter()
            .filter(|m| m.stats.selected_count == Some(0))
            .map(|m| m.r#ref.clone())
            .collect();
        if !empty.is_empty() {
            let policy = playlist
                .selection
                .on_member_unavailable
                .unwrap_or(crate::playlist::MemberUnavailable::Abort);
            match policy {
                crate::playlist::MemberUnavailable::Abort => {
                    verdict = verdict.worst(Verdict::Insufficient);
                    reasons.push(Reason::MembersEmptyAbort { refs: empty });
                }
                crate::playlist::MemberUnavailable::Skip => {
                    verdict = verdict.worst(Verdict::Thin);
                    reasons.push(Reason::MembersEmptySkip { refs: empty });
                }
            }
        }
        // A non-empty member whose quota exceeds its pool loops within its slot
        // (the case this check exists for). Signalled, never blocking.
        let looping: Vec<String> = members
            .iter()
            .filter(|m| m.stats.selected_count != Some(0) && m.verdict == Verdict::Thin)
            .map(|m| m.r#ref.clone())
            .collect();
        if !looping.is_empty() {
            verdict = verdict.worst(Verdict::Thin);
            reasons.push(Reason::MembersLoop { refs: looping });
        }
    }

    (verdict, reasons, members)
}

/// Per-member breakdown of a group inspection (empty for a leaf). Each member
/// is graded against its OWN quota ([`member_verdict`]) — the group-level axes
/// carry the rest.
fn member_breakdown(inspection: &PoolInspection) -> Vec<CoverageMember> {
    let Some(g) = &inspection.group else {
        return Vec::new();
    };
    g.members
        .iter()
        .map(|m| {
            let (verdict, reasons) = member_verdict(m);
            let detail = detail_of(&reasons);
            CoverageMember { r#ref: m.r#ref.clone(), stats: m.stats, verdict, detail, reasons }
        })
        .collect()
}

/// Grade one group member against its own per-member quota. Empty pool → ✗. A
/// `runtime`/`take` quota larger than the member's pool means the member will
/// LOOP inside its slot before handing over — flagged ⚠ (this is the point of
/// the check: make the loops visible). Unknown pool size (remote/queue) → no
/// false alarm. A member with no quota (weighted/rotate) is judged on emptiness
/// only.
fn member_verdict(m: &crate::pool_inspection::InspectedMember) -> (Verdict, Vec<Reason>) {
    use crate::playlist::MemberQuota;
    if m.stats.selected_count == Some(0) {
        return (Verdict::Insufficient, vec![Reason::PoolEmpty]);
    }
    match &m.quota {
        Some(MemberQuota::Runtime(secs)) => {
            if let Some(have_ms) = m.stats.total_duration_ms {
                let need_ms = (*secs).saturating_mul(1000);
                if have_ms < need_ms {
                    return (Verdict::Thin, vec![Reason::RuntimeLoop { need_ms, pool_ms: have_ms }]);
                }
            }
            (Verdict::Ok, Vec::new())
        }
        Some(MemberQuota::Take(n) | MemberQuota::TakeRandom { max: n, .. }) => {
            if let Some(count) = m.stats.selected_count {
                if count < *n as u64 {
                    return (Verdict::Thin, vec![Reason::TakeRepeat { take: *n as u64, count }]);
                }
            }
            (Verdict::Ok, Vec::new())
        }
        None => (Verdict::Ok, Vec::new()),
    }
}

/// Milliseconds → "HH:MM:SS" (durations are estimates; seconds are enough).
fn fmt_hms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

impl GridEngine {
    pub fn new(pool: SqlitePool, tz: impl Into<String>) -> Self {
        Self {
            pool,
            tz: tz.into(),
            plugins: None,
            media_root: None,
            control: StationControl::new_in_memory(),
            live_djs: None,
        }
    }

    /// Enable `live` rules: their `dj` must be in this DJ file (`[live]`).
    pub fn with_live_djs(mut self, djs_path: impl Into<PathBuf>) -> Self {
        self.live_djs = Some(djs_path.into());
        self
    }

    /// The connection window of DJ `dj` open at `now`, if any (the grid's
    /// `live` rules, in the station timezone — `resolver::live_window`).
    pub async fn live_window(
        &self,
        dj: &str,
        now: Epoch,
    ) -> Result<Option<crate::resolver::LiveWindow>, EngineError> {
        let local = clock::to_local_now(now, &self.tz)?;
        let grid = grid_index::load_grid(&self.pool).await?;
        Ok(crate::resolver::live_window(&grid, dj, local))
    }

    /// Share the station control (broadcast state, overrides, manual clock)
    /// with the plugin host surface and the broadcast gRPC service.
    pub fn with_control(mut self, control: StationControl) -> Self {
        self.control = control;
        self
    }

    /// The database pool (tests of the modules built on the engine).
    #[cfg(test)]
    pub(crate) fn pool_for_tests(&self) -> SqlitePool {
        self.pool.clone()
    }

    pub fn control(&self) -> &StationControl {
        &self.control
    }

    /// Attach the plugin system so decisions are broadcast to plugins.
    pub fn with_plugins(mut self, plugins: crate::plugin::PluginHandle) -> Self {
        self.plugins = Some(plugins);
        self
    }

    /// Attach the media root so resolution can verify a chosen file still
    /// exists on disk (and flip it unavailable if not). Without it, the check
    /// is skipped.
    pub fn with_media_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.media_root = Some(root.into());
        self
    }

    /// Manual clock: freeze the instant `resolve_next` uses when the request
    /// gives no explicit `now` (`Some`), or return to real time (`None`).
    pub fn set_clock(&self, frozen: Option<Epoch>) {
        self.control.set_clock(frozen);
    }

    /// The current manual-clock override, if any.
    pub fn clock_override(&self) -> Option<Epoch> {
        self.control.clock_override()
    }

    /// The instant to resolve at: an explicit request `now`, else the manual
    /// clock override, else real wall-clock time.
    pub fn effective_now(&self, explicit: Option<Epoch>) -> Epoch {
        explicit
            .or_else(|| self.control.clock_override())
            .unwrap_or_else(real_now)
    }

    /// Does the chosen media still exist under the media root? `true` when no
    /// root is configured (check off, e.g. in tests).
    fn media_exists(&self, rel_path: &str) -> bool {
        match &self.media_root {
            None => true,
            Some(root) => root.join(rel_path).exists(),
        }
    }

    /// Freeze the manual clock at a civil local time spec: "HH:MM" (today, in
    /// the station timezone) or "YYYY-MM-DD HH:MM". The daemon owns the zone,
    /// so the caller never computes an epoch.
    pub fn set_clock_civil(&self, spec: &str) -> Result<(), ClockError> {
        let epoch = self.parse_civil_spec(spec)?;
        self.set_clock(Some(epoch));
        Ok(())
    }

    fn parse_civil_spec(&self, spec: &str) -> Result<Epoch, ClockError> {
        let spec = spec.trim();
        let (date_part, time_part) = match spec.split_once(' ') {
            Some((d, t)) => (Some(d.trim()), t.trim()),
            None => (None, spec),
        };
        let (hour, minute) = parse_hh_mm(time_part)?;
        let (year, month, day) = match date_part {
            Some(dp) => parse_ymd(dp)?,
            None => {
                let today = clock::to_local_now(real_now(), &self.tz)?;
                (
                    today.date.year as i16,
                    today.date.month as i8,
                    today.date.day as i8,
                )
            }
        };
        clock::civil_to_epoch(&self.tz, year, month, day, hour, minute)
    }

    /// Render an epoch as station-local civil text (for display).
    pub fn render_local(&self, epoch: Epoch) -> Result<String, ClockError> {
        let ln = clock::to_local_now(epoch, &self.tz)?;
        Ok(fmt_local(&ln, &self.tz))
    }

    /// Read the configured rules without resolving or touching playback state.
    pub async fn list_rules(&self) -> Result<Vec<crate::resolver::Rule>, EngineError> {
        Ok(grid_index::load_grid(&self.pool).await?.rules)
    }

    /// Sizing report: for each enabled grid rule, inspect the pool of the
    /// playlist it references (read-only, no playout, no family-B mutation)
    /// and grade whether it has enough media for what the grid asks. `rule_ids`
    /// empty → the whole grid; otherwise only those rules. A per-entry config
    /// problem (broken ref, unreadable playlist, unresolvable pool) becomes an
    /// `INSUFFICIENT` entry rather than aborting the report — the point is to
    /// surface every problem at once. Only infrastructure (SQLite) propagates.
    pub async fn check_coverage(&self, rule_ids: &[String]) -> Result<CoverageReport, EngineError> {
        self.check_coverage_of(None, rule_ids).await
    }

    /// [`GridEngine::check_coverage`] of `grid` (a draft, or a grid that is
    /// not applied); `None` = the applied grid.
    pub async fn check_coverage_of(
        &self,
        grid: Option<Grid>,
        rule_ids: &[String],
    ) -> Result<CoverageReport, EngineError> {
        let grid = match grid {
            Some(g) => g,
            None => grid_index::load_grid(&self.pool).await?,
        };
        let want: Option<HashSet<&str>> =
            (!rule_ids.is_empty()).then(|| rule_ids.iter().map(String::as_str).collect());

        let mut entries = Vec::new();
        for rule in &grid.rules {
            if let Some(w) = &want {
                if !w.contains(rule.id.as_str()) {
                    continue;
                }
            }
            // A disabled rule is not in play — it can't cause a gap, skip it.
            // A live slot selects no playlist: nothing to size.
            if !rule.enabled || matches!(rule.kind, RuleKind::Live { .. }) {
                continue;
            }
            entries.push(self.coverage_for_rule(rule, &grid).await?);
        }
        let worst = entries.iter().fold(Verdict::Ok, |acc, e| acc.worst(e.verdict));
        Ok(CoverageReport { entries, worst })
    }

    async fn coverage_for_rule(&self, rule: &Rule, grid: &Grid) -> Result<CoverageEntry, EngineError> {
        let (kind, playlist_ref, demand) = classify_rule(rule, grid);
        let fail = |reason: Reason| {
            make_entry(
                rule.id.clone(),
                playlist_ref.clone(),
                kind,
                PoolStats::default(),
                Verdict::Insufficient,
                vec![reason],
                Vec::new(),
            )
        };

        // Broken / unsafe ref → nothing to size.
        let key = match crate::playlist::normalize_ref(&playlist_ref) {
            Ok(k) => k,
            Err(msg) => return Ok(fail(Reason::BadRef { error: msg.to_string() })),
        };
        let Some(toml) = crate::store::playlist_toml_by_ref(&self.pool, &key).await? else {
            return Ok(fail(Reason::UnknownPlaylist));
        };
        let playlist = match crate::playlist::Playlist::parse(&toml) {
            Ok(p) => p,
            Err(e) => return Ok(fail(Reason::UnreadablePlaylist { error: e.to_string() })),
        };

        // Size the pool (read-only). Only SQLite is infra and propagates; a
        // config-level selection error (transitive unknown ref, unsupported
        // mode, bad filter) marks THIS entry insufficient, report goes on.
        let inspection = match pool_inspection::inspect_ref_at(&self.pool, &key, self.effective_now(None).0).await {
            Ok(i) => i,
            Err(crate::selection::SelectionError::Sqlx(e)) => return Err(EngineError::Sqlx(e)),
            Err(e) => return Ok(fail(Reason::Unresolvable { error: e.to_string() })),
        };

        let (verdict, reasons, members) = verdict_for(&playlist, &inspection, demand);
        // `playlist_ref` is still borrowed by the `fail` closure above; clone
        // rather than move so there is no borrow/move conflict.
        Ok(make_entry(
            rule.id.clone(),
            playlist_ref.clone(),
            kind,
            inspection.stats,
            verdict,
            reasons,
            members,
        ))
    }

    /// Ensure every `Every` rule has a counter row, so its cadence advances
    /// from the first track on. Call at start-up and after each grid apply /
    /// reload. Idempotent; never resets an existing counter (family B).
    pub async fn sync_grid(&self) -> Result<(), EngineError> {
        let grid = grid_index::load_grid(&self.pool).await?;
        let every_ids: Vec<String> = grid
            .rules
            .iter()
            .filter(|r| matches!(r.kind, RuleKind::Every { .. }))
            .map(|r| r.id.clone())
            .collect();
        grid_store::ensure_every_rows(&self.pool, &every_ids).await?;
        Ok(())
    }

    /// One completed station track: advance every `Every` cooldown by one.
    /// Called on a track-end event, distinct from asking for the next source.
    pub async fn on_track_completed(&self) -> Result<(), EngineError> {
        grid_store::bump_tracks_since(&self.pool).await?;
        Ok(())
    }

    /// Mark an episode as fully played for its `unplayed_only` playlist (family
    /// B, infinite-expiry). Called by the track-COMPLETION path — never on
    /// start: an interrupted or failed episode must stay eligible. No-op unless
    /// the playlist actually declares `unplayed_only` (keeps the history scoped)
    /// and the media is known (its size/mtime are the play-once guard). Driven
    /// live by [`GridEngine::on_track_left`] (Liquidsoap bridge, a track that
    /// played to its end).
    pub async fn on_episode_finished(
        &self,
        playlist_ref: &str,
        media: &str,
        now: Epoch,
    ) -> Result<(), EngineError> {
        let Ok(key) = crate::playlist::normalize_ref(playlist_ref) else {
            tracing::warn!(playlist = %playlist_ref, "on_episode_finished: unnormalizable ref, skipping");
            return Ok(());
        };
        let Some(toml) = store::playlist_toml_by_ref(&self.pool, &key).await? else {
            return Ok(()); // unknown playlist — nothing to mark
        };
        let playlist = crate::playlist::Playlist::parse(&toml)
            .map_err(|e| EngineError::Selection(crate::selection::SelectionError::Parse(e)))?;
        if playlist.selection.unplayed_only != Some(true) {
            return Ok(()); // only unplayed_only playlists keep a play history
        }
        let Some((size, mtime)) = crate::media_index::size_mtime_of(&self.pool, media).await? else {
            return Ok(()); // media unknown — can't capture the guard
        };
        crate::episode_play::mark(&self.pool, &key, media, size, mtime, now).await?;
        Ok(())
    }

    /// A track left the air after `aired_s` seconds really on air (pauses
    /// excluded) — reported by the Liquidsoap bridge whatever ended it (its
    /// natural end, a skip, a hard override, a Liquidsoap restart). If it
    /// played to the end ([`played_to_end`] against the indexed duration), the
    /// `unplayed_only` mark of its LEAF playlist is recorded; a cut track, a
    /// media override (no leaf) or an unknown duration never marks. Returns
    /// whether the track counted as played to the end.
    pub async fn on_track_left(
        &self,
        media: &str,
        leaf_ref: Option<&str>,
        aired_s: i64,
        now: Epoch,
    ) -> Result<bool, EngineError> {
        Ok(self.track_left(None, media, leaf_ref, aired_s, now).await? == Some(true))
    }

    /// [`GridEngine::on_track_left`], plus the end written on the track's
    /// `broadcast_log` row (`log_id`, migration 0022) for the on-air history.
    /// Returns `Some(true)` played to the end, `Some(false)` cut, `None`
    /// duration unknown.
    pub async fn track_left(
        &self,
        log_id: Option<i64>,
        media: &str,
        leaf_ref: Option<&str>,
        aired_s: i64,
        now: Epoch,
    ) -> Result<Option<bool>, EngineError> {
        let duration_ms = crate::media_index::duration_ms_of(&self.pool, media).await?;
        let verdict = duration_ms.filter(|d| *d > 0).map(|d| played_to_end(aired_s, d));
        if let Some(id) = log_id {
            crate::broadcast_log::record_left(&self.pool, id, now, verdict).await?;
        }
        match verdict {
            None => {
                tracing::info!(%media, aired_s, "track left the air; duration unknown: not counted as played to the end");
            }
            Some(false) => {
                tracing::info!(%media, aired_s, "track cut short: not counted as played to the end");
            }
            Some(true) => {
                tracing::debug!(%media, aired_s, "track played to the end");
                if let Some(leaf) = leaf_ref {
                    self.on_episode_finished(leaf, media, now).await?;
                }
            }
        }
        Ok(verdict)
    }

    /// Enqueue a media into a `queue` playlist's runtime buffer (audience
    /// request / DJ injection). Errors if the ref is unknown or the playlist is
    /// not a queue; refused (`accepted = false`) when the queue is at its
    /// `max_len`. The pushed `media_path` is taken as-is (a media rel_path).
    pub async fn enqueue(
        &self,
        playlist_ref: &str,
        media_path: &str,
    ) -> Result<crate::queue_state::PushOutcome, EngineError> {
        let key = crate::playlist::normalize_ref(playlist_ref).map_err(|_| {
            EngineError::Selection(crate::selection::SelectionError::PlaylistNotFound(
                playlist_ref.to_string(),
            ))
        })?;
        let Some(toml) = store::playlist_toml_by_ref(&self.pool, &key).await? else {
            return Err(EngineError::Selection(
                crate::selection::SelectionError::PlaylistNotFound(playlist_ref.to_string()),
            ));
        };
        let pl = crate::playlist::Playlist::parse(&toml)
            .map_err(|e| EngineError::Selection(crate::selection::SelectionError::Parse(e)))?;
        if pl.selection.mode != crate::playlist::Mode::Queue {
            return Err(EngineError::Selection(crate::selection::SelectionError::NotAQueue(
                playlist_ref.to_string(),
                pl.selection.mode,
            )));
        }
        crate::queue_state::push(&self.pool, &key, media_path, pl.selection.max_len, real_now())
            .await
            .map_err(EngineError::from)
    }

    fn parse_all(files: &[(String, String)]) -> Result<Vec<Rule>, Vec<String>> {
        let mut rules = Vec::new();
        let mut errors = Vec::new();
        for (path, toml) in files {
            match grid_toml::parse_grid(toml) {
                Ok(mut rs) => rules.append(&mut rs),
                Err(e) => errors.push(format!("{path}: {e}")),
            }
        }
        if !errors.is_empty() { return Err(errors); }
        let mut seen: HashSet<&str> = HashSet::new();
        for r in &rules {
            if !seen.insert(r.id.as_str()) {
                errors.push(format!("duplicate rule id {:?} across grid files", r.id));
            }
        }
        let bases = rules.iter().filter(|r| matches!(r.kind, RuleKind::BaseRotation { .. })).count();
        if bases > 1 {
            errors.push(format!("{bases} base_rotation rules across grid files; at most one is allowed"));
        }
        if errors.is_empty() { Ok(rules) } else { Err(errors) }
    }

    async fn known_playlist_keys(&self) -> Result<HashSet<String>, EngineError> {
        Ok(store::list(&self.pool).await?.into_iter().filter_map(|r| r.rel_path).collect())
    }

    /// Playlist refs, then the DJs of `live` rules (the DJ file is read only
    /// when the grid has a `live` rule). Every problem at once.
    async fn check_refs(&self, rules: &[Rule]) -> Result<(), GridOpError> {
        let known = self.known_playlist_keys().await?;
        let mut errors = grid_toml::validate_refs(rules, &known);
        if rules.iter().any(|r| matches!(r.kind, RuleKind::Live { .. })) {
            match &self.live_djs {
                None => errors.extend(grid_toml::validate_djs(rules, None)),
                Some(path) => match crate::live::load_djs(path) {
                    Ok(djs) => {
                        let ids: HashSet<String> = djs.into_iter().map(|d| d.id).collect();
                        errors.extend(grid_toml::validate_djs(rules, Some(&ids)));
                    }
                    Err(e) => errors.push(format!("live rules cannot be checked: {e}")),
                },
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(GridOpError::Invalid(errors)) }
    }

    /// Problems of `rules` against the station: unknown or invalid playlist
    /// refs, `live` rules without `[live]` or with a DJ absent from the DJ
    /// file — each on its rule's field.
    pub async fn diagnose_rules(&self, rules: &[Rule]) -> Result<Vec<grid_toml::GridDiag>, EngineError> {
        let items: Vec<(usize, &Rule)> = rules.iter().enumerate().map(|(i, r)| (i + 1, r)).collect();
        self.diagnose_rules_at(&items).await
    }

    /// [`GridEngine::diagnose_rules`] of rules given with their number in
    /// the file (the ones that parsed, when others did not).
    pub async fn diagnose_rules_at(&self, rules: &[(usize, &Rule)]) -> Result<Vec<grid_toml::GridDiag>, EngineError> {
        let known = self.known_playlist_keys().await?;
        let mut out = grid_toml::diagnose_refs_at(rules, &known);
        if rules.iter().any(|(_, r)| matches!(r.kind, RuleKind::Live { .. })) {
            match &self.live_djs {
                None => out.extend(grid_toml::diagnose_djs_at(rules, None)),
                Some(path) => match crate::live::load_djs(path) {
                    Ok(djs) => {
                        let ids: HashSet<String> = djs.into_iter().map(|d| d.id).collect();
                        out.extend(grid_toml::diagnose_djs_at(rules, Some(&ids)));
                    }
                    Err(e) => out.push(grid_toml::dj_file_unreadable(e.to_string())),
                },
            }
        }
        Ok(out)
    }

    /// Replace the applied grid by `rules` (already judged valid): index
    /// rebuilt (family A), `Every` counters reconciled, never reset (family B).
    pub async fn replace_rules(&self, rules: &[Rule]) -> Result<(), EngineError> {
        grid_index::replace_grid(&self.pool, rules).await?;
        self.sync_grid().await?;
        self.control.bump_air();
        Ok(())
    }

    pub async fn validate_grid(&self, files: &[(String, String)]) -> Result<(), GridOpError> {
        let rules = Self::parse_all(files).map_err(GridOpError::Invalid)?;
        self.check_refs(&rules).await
    }

    pub async fn apply_grid(&self, files: &[(String, String)]) -> Result<Vec<String>, GridOpError> {
        let rules = Self::parse_all(files).map_err(GridOpError::Invalid)?;
        self.check_refs(&rules).await?;
        grid_index::replace_grid(&self.pool, &rules).await.map_err(EngineError::from)?;
        self.sync_grid().await?;
        self.control.bump_air();
        Ok(rules.iter().map(|r| r.id.clone()).collect())
    }

    pub async fn export_grid(&self, rule_ids: &[String]) -> Result<String, GridOpError> {
        let grid = grid_index::load_grid(&self.pool).await.map_err(EngineError::from)?;
        let rules: Vec<Rule> = if rule_ids.is_empty() {
            grid.rules
        } else {
            let want: HashSet<&str> = rule_ids.iter().map(String::as_str).collect();
            grid.rules.into_iter().filter(|r| want.contains(r.id.as_str())).collect()
        };
        grid_toml::to_toml(&rules).map_err(|m| GridOpError::Invalid(vec![m]))
    }

    /// Project the grid over `[from, from + window)` **without** touching
    /// playback state or the wall clock — the `stationctl schedule preview`
    /// path, and the DST test (a window over a transition night reveals a
    /// hole or a doubled hour in the local column).
    ///
    /// It is a *projection*, not a track-by-track simulation: pool statistics
    /// describe the current media index, without choosing tracks or predicting
    /// future playback. We walk minute by minute (every DayPart/AtClock boundary is
    /// minute-aligned) and emit an occurrence whenever the resolved decision
    /// changes. Consumed AtClock marks are carried forward exactly as the live
    /// loop persists them, so a mark punctuates a single instant instead of
    /// swallowing its whole slot.
    ///
    /// `Every` rules split by cadence:
    /// - **elapsed** (a time cooldown) *is* clock-projectable, so it is folded
    ///   into the walk and appears in `occurrences`. We seed it as if it had
    ///   just played at `from`, so its first projected mark lands one cadence
    ///   in (`from + cadence`), and advance its last-played on each fire — it
    ///   then punctuates like an AtClock (instant, not segment), and keeps the
    ///   `AtClock > Every > base` priority for free (it goes through
    ///   `resolve_next`).
    /// - **track-counted** can't be placed on a clock (its cadence rides real
    ///   playback, which we don't simulate here), so it is returned once in
    ///   `indicative` — never injected into the ordered `occurrences` timeline.
    pub async fn preview(
        &self,
        from: Epoch,
        window_secs: i64,
    ) -> Result<GridPreview, EngineError> {
        self.preview_of(None, from, window_secs).await
    }

    /// [`GridEngine::preview`] of `grid` (a draft, or a grid that is not
    /// applied) instead of the applied one — nothing applied, nothing stored.
    /// `None` = the applied grid.
    pub async fn preview_of(
        &self,
        grid: Option<Grid>,
        from: Epoch,
        window_secs: i64,
    ) -> Result<GridPreview, EngineError> {
        let loaded = match grid {
            Some(g) => g,
            None => grid_index::load_grid(&self.pool).await?,
        };

        // Track-counted Every rules: not projectable on the clock → returned
        // once as indicative (enabled only — a disabled rule isn't in play).
        let indicative: Vec<IndicativeRule> = loaded
            .rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| match &r.kind {
                RuleKind::Every { playlist_ref, cadence: Cadence::Tracks(_) } => {
                    Some(IndicativeRule {
                        rule_id: r.id.clone(),
                        playlist_ref: playlist_ref.clone(),
                    })
                }
                _ => None,
            })
            .collect();
        // Elapsed Every rules: seed each so its first mark is one cadence in.
        let elapsed_every_ids: Vec<String> = loaded
            .rules
            .iter()
            .filter_map(|r| match &r.kind {
                RuleKind::Every { cadence: Cadence::Elapsed(_), .. } => Some(r.id.clone()),
                _ => None,
            })
            .collect();

        // The walk grid keeps everything except track-counted Every rules —
        // they can never fire on a clock (and a `Tracks(0)` left in would
        // spuriously fire every minute). Elapsed Every rules stay in.
        let grid = Grid {
            rules: loaded
                .rules
                .into_iter()
                .filter(|r| {
                    !matches!(
                        r.kind,
                        RuleKind::Every { cadence: Cadence::Tracks(_), .. }
                    )
                })
                .collect(),
        };

        // Bound the walk: ignore a non-positive window, cap at 31 days so a
        // pathological request can't spin for ages (minute granularity).
        let window = window_secs.clamp(0, 31 * 86_400);
        let end = from.0.saturating_add(window);

        let mut sim = PlaybackState::default();
        for id in &elapsed_every_ids {
            sim.every.insert(
                id.clone(),
                EveryState { last_played: Some(from), tracks_since: 0 },
            );
        }
        let mut occurrences: Vec<PreviewOccurrence> = Vec::new();
        let mut prev_key: Option<(Origin, String, String)> = None;
        // Live windows: one open occurrence per DJ at most (the most recent).
        let mut djs: Vec<String> = grid
            .rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| match &r.kind {
                RuleKind::Live { dj, .. } => Some(dj.clone()),
                _ => None,
            })
            .collect();
        djs.sort();
        djs.dedup();
        let mut live: Vec<LiveProjection> = Vec::new();
        // Per DJ: (occurrence token, index in `live`) of the window open now.
        let mut live_open: std::collections::HashMap<String, (String, usize)> =
            std::collections::HashMap::new();
        // Current pool predicates are time-independent: reuse an inspection
        // within this request, never across previews (a rescan must be visible).
        let mut pool_memo: std::collections::HashMap<String, crate::pool_inspection::PoolInspection> =
            std::collections::HashMap::new();

        let mut secs = from.0;
        while secs < end {
            let epoch = Epoch(secs);
            let local = clock::to_local_now(epoch, &self.tz)?;
            let decision = resolve_next(local, &grid, &sim);

            // Consume the occurrence so the next minute in this slot yields the
            // base, mirroring the live loop (an instant, not a segment): an
            // AtClock via its token, an elapsed Every by advancing last-played.
            if let Some(token) = &decision.mark_taken {
                sim.at_clock_taken.insert(token.clone());
            }
            if decision.origin == Origin::Every {
                if let Some(id) = &decision.rule_id {
                    sim.every.entry(id.clone()).or_default().last_played = Some(epoch);
                }
            }

            for dj in &djs {
                let now_open = crate::resolver::live_window(&grid, dj, local);
                let token = now_open.as_ref().map(|w| w.occurrence.clone());
                let before = live_open.get(dj).map(|(t, _)| t.clone());
                if token == before {
                    continue;
                }
                if let Some((_, i)) = live_open.remove(dj) {
                    live[i].closes = Some((epoch, fmt_local(&local, &self.tz)));
                }
                if let Some(w) = now_open {
                    live_open.insert(dj.clone(), (w.occurrence.clone(), live.len()));
                    live.push(LiveProjection {
                        rule_id: w.rule_id,
                        dj: dj.clone(),
                        opens: epoch,
                        opens_local: fmt_local(&local, &self.tz),
                        open_before: secs == from.0 && w.opened_ago_min > 0,
                        closes: None,
                    });
                }
            }

            let rule_id = decision.rule_id.clone().unwrap_or_default();
            let playlist_ref = decision.playlist_ref.clone().unwrap_or_default();
            let key = (decision.origin.clone(), rule_id.clone(), playlist_ref.clone());
            if prev_key.as_ref() != Some(&key) {
                let inspection = if playlist_ref.is_empty() {
                    crate::pool_inspection::PoolInspection::default()
                } else {
                    let canonical = crate::playlist::normalize_ref(&playlist_ref)
                        .map_err(|_| crate::selection::SelectionError::PlaylistNotFound(playlist_ref.clone()))?;
                    if let Some(stats) = pool_memo.get(&canonical) {
                        stats.clone()
                    } else {
                        let stats = crate::pool_inspection::inspect_ref_at(&self.pool, &canonical, epoch.0).await?;
                        pool_memo.insert(canonical, stats.clone());
                        stats
                    }
                };
                occurrences.push(PreviewOccurrence {
                    epoch,
                    at_local: fmt_local(&local, &self.tz),
                    origin: decision.origin.clone(),
                    rule_id,
                    playlist_ref,
                    pool: inspection.stats,
                    group: inspection.group,
                });
                prev_key = Some(key);
            }
            secs += 60;
        }

        Ok(GridPreview { occurrences, indicative, live })
    }

    /// Resolve the source to pull at a track boundary for instant `now`, and
    /// persist the decision's side effects (a consumed AtClock occurrence, an
    /// `Every` cooldown reset). Pure resolution, durable bookkeeping. This is
    /// the source-only primitive (no media); the live path is `next_media`.
    pub async fn next(&self, now: Epoch) -> Result<GridDecision, EngineError> {
        let local = clock::to_local_now(now, &self.tz)?;
        let grid = grid_index::load_grid(&self.pool).await?;
        let state = grid_store::load_playback_state(&self.pool).await?;

        let decision = resolve_next(local, &grid, &state);
        self.persist_effects(&decision, now).await?;
        Ok(decision)
    }

    /// Resolve the source AND the concrete media to pull, with **grid
    /// fallthrough**: ask the resolver for the applicable sources in priority
    /// order and keep the first that actually yields a media. A source whose
    /// pool is empty is skipped and we fall through to the next (down to the
    /// BaseRotation floor) — no silent gap. Side effects (a consumed AtClock
    /// mark, an `Every` reset) are persisted ONLY for the source that produced,
    /// so an empty AtClock does not burn its occurrence.
    ///
    /// - every applicable source (incl. the floor) empty → `PoolEmpty` surfaced
    ///   (legitimate dead air → Liquidsoap on-empty covers it);
    /// - no rule covers `now` at all → a `Fallback` decision, media `None`.
    ///
    /// Two layers come BEFORE the grid (the pure resolver is untouched):
    /// 1. the broadcast **gate** — paused/sleeping → `halted`, nothing
    ///    resolved (a draining station with zero listeners falls asleep right
    ///    here). Right after a wake, a held group is released: the station
    ///    airs the slot of NOW, not a cycle cut when it fell asleep;
    /// 2. the **override queue** — the highest priority of the architecture
    ///    (`override > one-shot > grid > fallback`).
    pub async fn next_media(&self, now: Epoch) -> Result<ResolvedDecision, EngineError> {
        if let Gate::Halt(state) = self.control.gate() {
            tracing::debug!(state = state.as_str(), "broadcast halted: nothing resolved");
            return Ok(ResolvedDecision {
                decision: GridDecision {
                    origin: Origin::Fallback,
                    rule_id: None,
                    playlist_ref: None,
                    mark_taken: None,
                },
                media_path: None,
                stream: false,
                halted: Some(state),
                override_source: None,
                leaf_ref: None,
                log_id: None,
            });
        }
        if self.control.take_woken() {
            if let Some(hold) = grid_store::get_hold(&self.pool).await? {
                tracing::info!(playlist = %hold.playlist_ref, "woke from sleep: held group released");
                self.end_hold(&hold.playlist_ref, true).await?;
            }
        }
        if let Some(resolved) = self.next_override(now).await? {
            return Ok(resolved);
        }

        let local = clock::to_local_now(now, &self.tz)?;
        let grid = grid_index::load_grid(&self.pool).await?;
        let state = grid_store::load_playback_state(&self.pool).await?;

        let mut ranked = resolve_ranked(local, &grid, &state);
        // A group started by an `every` / `at_clock` keeps the air until its
        // cycle ends: right after the hard rendez-vous, ahead of everything
        // else (the override layer is already above).
        let hold = self.current_hold(&grid).await?;
        let held_at = hold.as_ref().map(|h| {
            let origin = match h.origin.as_str() {
                "at_clock_hard" => Origin::AtClockHard,
                "at_clock_soft" => Origin::AtClockSoft,
                _ => Origin::Every,
            };
            let at = ranked
                .iter()
                .position(|d| d.origin != Origin::AtClockHard)
                .unwrap_or(ranked.len());
            ranked.insert(
                at,
                GridDecision {
                    origin,
                    rule_id: Some(h.rule_id.clone()),
                    playlist_ref: Some(h.playlist_ref.clone()),
                    mark_taken: None,
                },
            );
            at
        });
        let had_candidates = !ranked.is_empty();

        for (i, decision) in ranked.into_iter().enumerate() {
            let Some(playlist_ref) = decision.playlist_ref.clone() else {
                continue;
            };
            let held = held_at == Some(i);
            let start = if held {
                TurnStart::Continue
            } else if matches!(decision.origin, Origin::Every | Origin::AtClockHard | Origin::AtClockSoft) {
                TurnStart::Fresh
            } else {
                TurnStart::Resume
            };
            let produced = self.produce(&playlist_ref, now, start).await?;

            if let Some(turn) = produced {
                if held {
                    // The rule's effects were persisted when the group started.
                    if !turn.holds {
                        grid_store::clear_hold(&self.pool).await?;
                        tracing::info!(playlist = %playlist_ref, "held group: cycle complete");
                    }
                } else {
                    // Persist effects only now that this source actually produced.
                    self.persist_effects(&decision, now).await?;
                    self.start_hold_if_group(&decision, turn.holds, now).await?;
                }
                let origin = format!("{:?}", decision.origin);
                let (media_path, stream, leaf_ref, log_id) =
                    self.log_start(turn.resolved, now, &decision, &origin).await?;
                self.emit_resolved(&decision, Some(&media_path), origin);
                return Ok(ResolvedDecision {
                    decision,
                    media_path: Some(media_path),
                    stream,
                    halted: None,
                    override_source: None,
                    leaf_ref,
                    log_id,
                });
            }

            if held {
                // Cycle over (`skip` past the last members), or a member empty
                // under `abort`: the hold ends, the grid takes over.
                tracing::info!(playlist = %playlist_ref, "held group: nothing left in its cycle, hold released");
                self.end_hold(&playlist_ref, true).await?;
                continue;
            }
            tracing::info!(
                rule = decision.rule_id.as_deref().unwrap_or("-"),
                playlist = %playlist_ref,
                origin = ?decision.origin,
                "grid source produced no usable media; falling through to lower priority"
            );
            self.control.record_incident(
                crate::station_control::IncidentKind::SourceEmpty,
                decision.rule_id.as_deref(),
                &playlist_ref,
                &format!("{:?}", decision.origin),
                now,
            );
        }

        if had_candidates {
            // Everything applicable, down to the floor, produced nothing:
            // legitimate dead air → surface it (Liquidsoap on-empty covers).
            return Err(EngineError::Selection(
                crate::selection::SelectionError::PoolEmpty,
            ));
        }

        // No rule covered `now` at all → fallback (Liquidsoap safety net).
        let decision = GridDecision {
            origin: Origin::Fallback,
            rule_id: None,
            playlist_ref: None,
            mark_taken: None,
        };
        self.emit_resolved(&decision, None, format!("{:?}", decision.origin));
        Ok(ResolvedDecision {
            decision,
            media_path: None,
            stream: false,
            halted: None,
            override_source: None,
            leaf_ref: None,
            log_id: None,
        })
    }

    /// Resolve one grid source to something playable NOW: plugins,
    /// constraints, and a bounded re-pick when the chosen file vanished from
    /// disk (flipped unavailable in the index). `None` = the source produced
    /// nothing usable (empty pool) — the caller falls through. A config error
    /// (unknown ref, unsupported order/mode, bad filter) is surfaced.
    /// `start`: how a group takes the turn (`Fresh` when its rule triggers
    /// it, `Continue` when it holds the air — `None` then also when its cycle
    /// is over; see `selection::TurnStart`).
    async fn produce(
        &self,
        playlist_ref: &str,
        now: Epoch,
        start: crate::selection::TurnStart,
    ) -> Result<Option<crate::selection::Turn>, EngineError> {
        use crate::selection::{Resolved, SelectionError, Turn};
        // Capped so a pool of dead entries can't spin.
        const MAX_DEAD_PICKS: u32 = 32;
        for _ in 0..MAX_DEAD_PICKS {
            match crate::selection::resolve_turn(
                &self.pool,
                self.plugins.as_ref(),
                now.0,
                playlist_ref,
                start,
            )
            .await
            {
                // A remote stream: no file on disk to check, no re-pick.
                Ok(turn @ Turn { resolved: Resolved::Stream(_), .. }) => return Ok(Some(turn)),
                Ok(turn @ Turn { resolved: Resolved::File { .. }, .. })
                    if matches!(&turn.resolved, Resolved::File { path, .. } if self.media_exists(path)) =>
                {
                    return Ok(Some(turn));
                }
                Ok(Turn { resolved: Resolved::File { path: missing, .. }, .. }) => {
                    tracing::warn!(
                        media = %missing,
                        "resolved media missing on disk; marking unavailable and re-picking"
                    );
                    crate::media_index::mark_unavailable(&self.pool, &missing).await?;
                }
                Err(SelectionError::PoolEmpty) | Err(SelectionError::CycleComplete) => return Ok(None),
                Err(e) => return Err(EngineError::Selection(e)),
            }
        }
        Ok(None)
    }

    /// The group holding the air, if it still stands: its rule must still be
    /// in the grid, enabled, with the same playlist. Otherwise the hold is
    /// dropped (logged) and the group restarts from the top next time.
    async fn current_hold(&self, grid: &Grid) -> Result<Option<grid_store::Hold>, EngineError> {
        let Some(hold) = grid_store::get_hold(&self.pool).await? else {
            return Ok(None);
        };
        let stands = grid.rules.iter().any(|r| {
            r.enabled
                && r.id == hold.rule_id
                && match &r.kind {
                    RuleKind::Every { playlist_ref, .. } | RuleKind::AtClock { playlist_ref, .. } => {
                        *playlist_ref == hold.playlist_ref
                    }
                    _ => false,
                }
        });
        if stands {
            return Ok(Some(hold));
        }
        tracing::warn!(
            rule = %hold.rule_id,
            playlist = %hold.playlist_ref,
            "held group dropped: its rule is gone, disabled or points elsewhere"
        );
        self.end_hold(&hold.playlist_ref, true).await?;
        Ok(None)
    }

    /// End the hold; `reset` also puts the group back at the top of its
    /// cycle (it was cut short), so its next activation starts cleanly.
    async fn end_hold(&self, playlist_ref: &str, reset: bool) -> Result<(), EngineError> {
        grid_store::clear_hold(&self.pool).await?;
        if reset {
            if let Ok(key) = crate::playlist::normalize_ref(playlist_ref) {
                crate::group_state::set(&self.pool, &key, &crate::group_state::GroupState::default())
                    .await?;
            }
        }
        Ok(())
    }

    /// After `decision` produced `turn`: a group started by an `every` /
    /// `at_clock` rule, with its cycle unfinished, holds the air from now on
    /// (a day part or the base rotation stays in force by itself: no hold).
    /// A hold still in force for ANOTHER group (a hard rendez-vous group cut
    /// in) is replaced, and that group reset to the top.
    async fn start_hold_if_group(
        &self,
        decision: &GridDecision,
        turn_holds: bool,
        now: Epoch,
    ) -> Result<(), EngineError> {
        let origin = match decision.origin {
            Origin::Every => "every",
            Origin::AtClockHard => "at_clock_hard",
            Origin::AtClockSoft => "at_clock_soft",
            _ => return Ok(()),
        };
        let (Some(rule_id), Some(playlist_ref), true) =
            (decision.rule_id.as_ref(), decision.playlist_ref.as_ref(), turn_holds)
        else {
            return Ok(());
        };
        let previous = grid_store::get_hold(&self.pool).await?;
        if let Some(prev) = previous.filter(|p| p.playlist_ref != *playlist_ref) {
            tracing::info!(
                rule = %prev.rule_id,
                playlist = %prev.playlist_ref,
                "held group interrupted by another group: it will restart from the top"
            );
            self.end_hold(&prev.playlist_ref, true).await?;
        }
        grid_store::set_hold(
            &self.pool,
            &grid_store::Hold {
                rule_id: rule_id.clone(),
                playlist_ref: playlist_ref.clone(),
                origin: origin.to_string(),
                started_at: now,
            },
        )
        .await?;
        tracing::info!(rule = %rule_id, playlist = %playlist_ref, "group holds the air until its cycle ends");
        Ok(())
    }

    /// The next `AtClock` **hard** rendez-vous at or after `now` — or one
    /// that fell at most [`HARD_CUT_LATE_S`] ago and is still untaken (a
    /// timer that woke a little late must still cut). Walks the minute
    /// boundaries with the resolver itself, restricted to the enabled hard
    /// AtClock rules and the real playback state (a consumed occurrence is
    /// skipped), over at most [`HARD_MARK_LOOKAHEAD_MIN`] minutes: timezone,
    /// DST and tokens are the resolver's, never recomputed here. `None` =
    /// no hard mark in that window (the ticker re-plans later).
    pub async fn next_hard_mark(&self, now: Epoch) -> Result<Option<Epoch>, EngineError> {
        let loaded = grid_index::load_grid(&self.pool).await?;
        let hard = Grid {
            rules: loaded
                .rules
                .into_iter()
                .filter(|r| {
                    r.enabled
                        && matches!(
                            r.kind,
                            RuleKind::AtClock { mode: crate::resolver::Mode::Hard, .. }
                        )
                })
                .collect(),
        };
        if hard.rules.is_empty() {
            return Ok(None);
        }
        let state = grid_store::load_playback_state(&self.pool).await?;
        let this_minute = now.0 - now.0.rem_euclid(60);
        let first = if now.0 - this_minute <= HARD_CUT_LATE_S { this_minute } else { this_minute + 60 };
        for i in 0..HARD_MARK_LOOKAHEAD_MIN {
            let at = Epoch(first + i * 60);
            let local = clock::to_local_now(at, &self.tz)?;
            // A rendez-vous FALLS on this minute — not an older one still due
            // (within its expiry): its occurrence token names this HH:MM.
            let this_mark = format!("T{:02}:{:02}", local.wall.hour, local.wall.minute);
            let due = resolve_ranked(local, &hard, &state).into_iter().any(|d| {
                d.origin == Origin::AtClockHard
                    && d.mark_taken.as_deref().is_some_and(|t| t.ends_with(&this_mark))
            });
            if due {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }

    /// Air the `AtClock` hard rendez-vous `mark` NOW, mid-track (the caller
    /// cuts it in). Cuts only when it is really time: `now` within
    /// [`HARD_CUT_LATE_S`] after `mark`, the station on air (not paused /
    /// stopped), and the resolver giving an untaken `AtClockHard` due now.
    /// Its source is resolved like a pull (plugins, constraints, re-pick) and
    /// its occurrence token consumed, so the next pull does not air it again.
    /// `None` = no cut: the token stays free and the rule airs **soft** at the
    /// next track boundary, within its `expiry` (a paused station, a restart
    /// or a clock jump past the mark, an empty pool…).
    pub async fn air_at_clock_hard(
        &self,
        mark: Epoch,
        now: Epoch,
    ) -> Result<Option<ResolvedDecision>, EngineError> {
        let late = now.0 - mark.0;
        if !(0..=HARD_CUT_LATE_S).contains(&late) {
            tracing::info!(mark = mark.0, late, "AtClock hard: not on time, no cut (soft at the next boundary)");
            return Ok(None);
        }
        if let Gate::Halt(state) = self.control.gate() {
            tracing::info!(state = state.as_str(), "AtClock hard: station halted, no cut (soft once it airs again)");
            return Ok(None);
        }
        if let Some(dj) = self.control.live_dj() {
            tracing::info!(%dj, "AtClock hard: a DJ is on air, no cut (soft after the live, within its expiry)");
            return Ok(None);
        }
        let local = clock::to_local_now(now, &self.tz)?;
        let mark_local = clock::to_local_now(mark, &self.tz)?;
        let this_mark = format!("T{:02}:{:02}", mark_local.wall.hour, mark_local.wall.minute);
        let grid = grid_index::load_grid(&self.pool).await?;
        let state = grid_store::load_playback_state(&self.pool).await?;
        // The hard rendez-vous of THIS mark (not an older one still due).
        let Some(decision) = resolve_ranked(local, &grid, &state).into_iter().find(|d| {
            d.origin == Origin::AtClockHard
                && d.mark_taken.as_deref().is_some_and(|t| t.ends_with(&this_mark))
        }) else {
            return Ok(None); // already aired by a pull at this boundary, or not due
        };
        let Some(playlist_ref) = decision.playlist_ref.clone() else {
            return Ok(None);
        };
        let Some(turn) = self.produce(&playlist_ref, now, TurnStart::Fresh).await? else {
            tracing::warn!(
                rule = decision.rule_id.as_deref().unwrap_or("-"),
                playlist = %playlist_ref,
                "AtClock hard: its source produced nothing, no cut"
            );
            self.control.record_incident(
                crate::station_control::IncidentKind::HardNotCut,
                decision.rule_id.as_deref(),
                &playlist_ref,
                "AtClockHard",
                mark,
            );
            return Ok(None);
        };
        self.persist_effects(&decision, now).await?;
        self.start_hold_if_group(&decision, turn.holds, now).await?;
        let origin = format!("{:?}", decision.origin);
        let (media_path, stream, leaf_ref, log_id) =
            self.log_start(turn.resolved, now, &decision, &origin).await?;
        self.emit_resolved(&decision, Some(&media_path), origin);
        Ok(Some(ResolvedDecision {
            decision,
            media_path: Some(media_path),
            stream,
            halted: None,
            override_source: None,
            leaf_ref,
            log_id,
        }))
    }

    /// The override layer: air the head of the override queue, if any. A media
    /// is checked on disk (it may be outside the index — arbitrary path
    /// allowed); a playlist is resolved like a grid source (plugins, constraints
    /// included) and holds the air for its `tracks` — a `sequence` / `shuffle`
    /// group starts its cycle from the top and, without `tracks`, holds the
    /// air until that cycle ends (as when a rule triggers it). An override that can't
    /// air (missing file, empty pool, unknown ref) is DROPPED loudly and the
    /// next one / the grid takes over — never a silent gap, never a retry loop
    /// (each iteration consumes or drops one entry). Grid side effects (AtClock
    /// marks, Every resets) are not touched: the grid did not play.
    async fn next_override(&self, now: Epoch) -> Result<Option<ResolvedDecision>, EngineError> {
        while let Some(entry) = self.control.next_override(now) {
            if let Some(resolved) = self.air_override_entry(entry, now).await? {
                return Ok(Some(resolved));
            }
        }
        Ok(None)
    }

    /// Air override `id` NOW, out of queue order (a `hard` cut): resolved like
    /// any override (one track consumed; dropped loudly if it can't air).
    /// `None` = gone (already aired by a pull, expired, or unplayable).
    pub async fn air_override_now(
        &self,
        id: u64,
        now: Epoch,
    ) -> Result<Option<ResolvedDecision>, EngineError> {
        match self.control.override_by_id(id, now) {
            None => Ok(None),
            Some(entry) => self.air_override_entry(entry, now).await,
        }
    }

    /// Resolve one override entry: `Some` = it produced (one track consumed),
    /// `None` = it could not air and was dropped (logged), or its group cycle
    /// had nothing left (ended quietly).
    async fn air_override_entry(
        &self,
        entry: crate::station_control::OverrideEntry,
        now: Epoch,
    ) -> Result<Option<ResolvedDecision>, EngineError> {
        use crate::selection::{Resolved, SelectionError, Turn, TurnStart};
        let produced: Result<Turn, String> = match &entry.content {
            OverrideContent::Media(path) => {
                if self.media_exists(path) {
                    // No playlist produced it: no leaf, never an unplayed_only mark.
                    Ok(Turn { resolved: Resolved::File { path: path.clone(), leaf: None }, holds: false, cycles: false })
                } else {
                    // Keep the index honest: it is no longer offered as available.
                    crate::media_index::mark_unavailable(&self.pool, path).await?;
                    Err(format!("media `{path}` not found under the media root"))
                }
            }
            OverrideContent::Playlist(reference) => {
                // First track: a group cycle starts from the top; then it
                // carries on in that same cycle (other playlists ignore it).
                let start = if entry.holding { TurnStart::Continue } else { TurnStart::Fresh };
                match crate::selection::resolve_turn(&self.pool, self.plugins.as_ref(), now.0, reference, start)
                    .await
                {
                    Ok(Turn { resolved: Resolved::File { path: media, .. }, .. }) if !self.media_exists(&media) => {
                        crate::media_index::mark_unavailable(&self.pool, &media).await?;
                        Err(format!("resolved media `{media}` missing on disk"))
                    }
                    Ok(turn) => Ok(turn),
                    Err(SelectionError::CycleComplete) => {
                        self.control.end_override(entry.id);
                        return Ok(None);
                    }
                    Err(SelectionError::Sqlx(e)) => return Err(EngineError::Sqlx(e)),
                    Err(e) => Err(e.to_string()),
                }
            }
        };
        match produced {
            Err(reason) => {
                self.control.drop_override(entry.id, &reason);
                Ok(None)
            }
            Ok(Turn { resolved, holds, cycles }) => {
                self.control.consume_override(entry.id, holds, cycles);
                let playlist_ref = match &entry.content {
                    OverrideContent::Playlist(r) => Some(r.clone()),
                    OverrideContent::Media(_) => None,
                };
                let decision = GridDecision {
                    origin: Origin::Fallback, // not a grid origin; see override_source
                    rule_id: None,
                    playlist_ref,
                    mark_taken: None,
                };
                let (media_path, stream, leaf_ref, log_id) =
                    self.log_start(resolved, now, &decision, "Override").await?;
                self.emit_resolved(&decision, Some(&media_path), "Override".to_string());
                Ok(Some(ResolvedDecision {
                    decision,
                    media_path: Some(media_path),
                    stream,
                    halted: None,
                    override_source: Some(entry.source.clone()),
                    leaf_ref,
                    log_id,
                }))
            }
        }
    }

    /// Log a track START into the station history (family B) so the
    /// anti-repetition constraints see it on the next pull (artist from the
    /// media index, `None` = untagged). A stream has no file identity / artist
    /// → no play history. Returns (media_path, is_stream, leaf playlist key).
    async fn log_start(
        &self,
        resolved: crate::selection::Resolved,
        now: Epoch,
        decision: &GridDecision,
        origin: &str,
    ) -> Result<(String, bool, Option<String>, Option<i64>), EngineError> {
        Ok(match resolved {
            crate::selection::Resolved::File { path: media, leaf } => {
                let artist = crate::media_index::artist_of(&self.pool, &media).await?;
                let from = crate::broadcast_log::Provenance {
                    rule_id: decision.rule_id.as_deref(),
                    origin: Some(origin),
                    playlist_ref: decision.playlist_ref.as_deref(),
                    leaf_ref: leaf.as_deref(),
                };
                let id =
                    crate::broadcast_log::record(&self.pool, &media, artist.as_deref(), now, from).await?;
                (media, false, leaf, Some(id))
            }
            crate::selection::Resolved::Stream(url) => (url, true, None, None),
        })
    }

    /// Liquidsoap really started the track logged as `log_id` (stats: aired
    /// vs merely chosen).
    pub async fn mark_aired(&self, log_id: i64, at: Epoch) -> Result<(), EngineError> {
        crate::broadcast_log::mark_aired(&self.pool, log_id, at).await?;
        Ok(())
    }

    /// Persist a decision's side effects — a consumed AtClock occurrence and an
    /// `Every` cooldown reset. Called only once a source has actually produced
    /// (or, from `next`, for the chosen source).
    async fn persist_effects(
        &self,
        decision: &GridDecision,
        now: Epoch,
    ) -> Result<(), EngineError> {
        if let Some(token) = &decision.mark_taken {
            grid_store::record_at_clock_taken(&self.pool, token, now).await?;
        }
        if decision.origin == Origin::Every {
            if let Some(rule_id) = &decision.rule_id {
                grid_store::reset_every(&self.pool, rule_id, now).await?;
            }
        }
        Ok(())
    }

    /// Notify plugins of a decision (best-effort, fire-and-forget). `origin`
    /// is the short label (`BaseRotation`, …, or `Override`).
    fn emit_resolved(&self, decision: &GridDecision, media_path: Option<&str>, origin: String) {
        if let Some(plugins) = &self.plugins {
            plugins.emit(crate::plugin::PluginEvent::TrackResolved {
                media_path: media_path.map(|s| s.to_string()),
                playlist_ref: decision.playlist_ref.clone(),
                rule_id: decision.rule_id.clone(),
                origin,
            });
        }
    }
}

/// How late (seconds) an `AtClock` hard rendez-vous may still be cut in:
/// the ticker waking a little after the mark. Later (a restart, a clock
/// jump), no cut — the rule airs soft at the next boundary instead.
pub const HARD_CUT_LATE_S: i64 = 10;

/// How far ahead (minutes) the next hard rendez-vous is searched; the
/// ticker re-plans at least every minute anyway.
pub const HARD_MARK_LOOKAHEAD_MIN: i64 = 60;

/// Slack between the time a track really spent on air and its indexed
/// duration for it to count as played to the end: covers the crossfade (the
/// next track starts before this one ends — 3 s by default) and the
/// one-second granularity of the bridge's clock.
pub const PLAYED_TO_END_TOLERANCE_S: i64 = 15;

/// Did a track that spent `aired_s` seconds on air play to its end, given its
/// indexed duration? (`aired + tolerance >= duration`.)
pub fn played_to_end(aired_s: i64, duration_ms: i64) -> bool {
    aired_s.saturating_add(PLAYED_TO_END_TOLERANCE_S).saturating_mul(1000) >= duration_ms
}

fn real_now() -> Epoch {
    use std::time::{SystemTime, UNIX_EPOCH};
    Epoch(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

fn parse_hh_mm(s: &str) -> Result<(i8, i8), ClockError> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| ClockError::BadCivilTime(format!("expected HH:MM, got {s:?}")))?;
    let hour = h
        .trim()
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad hour in {s:?}")))?;
    let minute = m
        .trim()
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad minute in {s:?}")))?;
    Ok((hour, minute))
}

fn parse_ymd(s: &str) -> Result<(i16, i8, i8), ClockError> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(ClockError::BadCivilTime(format!("expected YYYY-MM-DD, got {s:?}")));
    }
    let year = parts[0]
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad year in {s:?}")))?;
    let month = parts[1]
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad month in {s:?}")))?;
    let day = parts[2]
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad day in {s:?}")))?;
    Ok((year, month, day))
}

/// Render a `LocalNow` as `YYYY-MM-DD HH:MM <IANA zone>` — named zone, not a
/// frozen offset (per the time doc). Enough to read a preview and to spot a
/// DST hole/doubling when paired with the epoch-UTC column.
fn fmt_local(local: &LocalNow, tz: &str) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} {tz}",
        local.date.year, local.date.month, local.date.day, local.wall.hour, local.wall.minute
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::grid_index::insert_rule;
    use crate::resolver::{Cadence, ClockAnchor, Mode, Rule, RuleKind, Validity};

    async fn engine() -> (tempfile::TempDir, GridEngine) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("test.db")).await.unwrap();
        (dir, GridEngine::new(pool, "UTC")) // UTC → epoch maps straight to wall time
    }

    fn rule(id: &str, kind: RuleKind) -> Rule {
        Rule { id: id.into(), enabled: true, validity: Validity::default(), kind }
    }

    /// epoch for a given UTC hour:minute (tz is "UTC" in these tests).
    fn at(h: i64, m: i64) -> Epoch {
        Epoch(h * 3600 + m * 60)
    }

    #[tokio::test]
    async fn unknown_timezone_surfaces_as_error() {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        let eng = GridEngine::new(pool, "Nowhere/Nope");
        assert!(matches!(eng.next(at(9, 0)).await, Err(EngineError::Clock(_))));
    }

    #[tokio::test]
    async fn every_cadence_survives_across_calls() {
        let (_dir, eng) = engine().await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule("jingle", RuleKind::Every { playlist_ref: "ids".into(), cadence: Cadence::Tracks(2) }),
        )
        .await
        .unwrap();
        eng.sync_grid().await.unwrap();

        // tracks_since = 0 → not due → base.
        assert_eq!(eng.next(at(9, 0)).await.unwrap().origin, Origin::BaseRotation);
        eng.on_track_completed().await.unwrap(); // 1
        assert_eq!(eng.next(at(9, 0)).await.unwrap().origin, Origin::BaseRotation);
        eng.on_track_completed().await.unwrap(); // 2
        // Now due → Every fires and resets the counter.
        let d = eng.next(at(9, 0)).await.unwrap();
        assert_eq!(d.origin, Origin::Every);
        assert_eq!(d.playlist_ref.as_deref(), Some("ids"));
        // Reset persisted → immediately after, back to base.
        eng.on_track_completed().await.unwrap(); // 1
        assert_eq!(eng.next(at(9, 0)).await.unwrap().origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn atclock_fires_once_then_is_consumed() {
        let (_dir, eng) = engine().await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "top",
                RuleKind::AtClock {
                    playlist_ref: "jingle".into(),
                    anchor: ClockAnchor::EveryMinutes(15),
                    mode: Mode::Soft,
                    expiry_secs: None,
                },
            ),
        )
        .await
        .unwrap();

        // 09:15 → the :15 mark fires.
        let first = eng.next(at(9, 15)).await.unwrap();
        assert_eq!(first.origin, Origin::AtClockSoft);
        // Same mark again (09:16 still floors to :15) → already consumed → base.
        let second = eng.next(at(9, 16)).await.unwrap();
        assert_eq!(second.origin, Origin::BaseRotation);
    }

    // ----- a group started by every / at_clock holds the air -------------

    /// `music` floor + an `every` 45 min on the group `onehit` (sequence:
    /// `jhit` then `hit`, one track each) + an hourly hard `top`. `hit_prefix`
    /// lets a test empty the second member; `policy` = on_member_unavailable.
    async fn hold_fixture(hit_prefix: &str, policy: &str) -> (tempfile::TempDir, GridEngine) {
        let (dir, eng) = engine().await;
        let m = |p: &str| crate::media::ScannedMedia {
            rel_path: p.into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 60_000,
            size_bytes: 1,
            mtime_ns: 0,
        };
        crate::media_index::replace_library(
            &eng.pool,
            &[m("music/a.mp3"), m("jhit/j.mp3"), m("hit/h.mp3"), m("top/t.mp3")],
            1000,
        )
        .await
        .unwrap();
        for (r, prefix) in [("music", "music/"), ("jhit", "jhit/"), ("hit", hit_prefix), ("top", "top/")] {
            let toml = format!(
                "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                 [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{prefix}\"\n"
            );
            let pl = crate::playlist::Playlist::parse(&toml).unwrap();
            crate::store::upsert(&eng.pool, r, &pl, &toml, Some(r)).await.unwrap();
        }
        let group = format!(
            "name = \"onehit\"\n[selection]\nmode = \"group\"\nstrategy = \"sequence\"\n\
             on_member_unavailable = \"{policy}\"\n\
             members = [ {{ ref = \"jhit\", take = 1 }}, {{ ref = \"hit\", take = 1 }} ]\n"
        );
        let pl = crate::playlist::Playlist::parse(&group).unwrap();
        crate::store::upsert(&eng.pool, "onehit", &pl, &group, Some("onehit")).await.unwrap();
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "music".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule("OneHit", RuleKind::Every { playlist_ref: "onehit".into(), cadence: Cadence::Elapsed(2700) }),
        )
        .await
        .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "top",
                RuleKind::AtClock {
                    playlist_ref: "top".into(),
                    anchor: ClockAnchor::EveryMinutes(60),
                    mode: Mode::Hard,
                    expiry_secs: Some(300),
                },
            ),
        )
        .await
        .unwrap();
        eng.sync_grid().await.unwrap();
        (dir, eng)
    }

    async fn media_at(eng: &GridEngine, t: Epoch) -> (String, Origin) {
        let r = eng.next_media(t).await.unwrap();
        (r.media_path.unwrap(), r.decision.origin)
    }

    #[tokio::test]
    async fn an_every_group_holds_the_air_until_its_cycle_ends() {
        let (_d, eng) = hold_fixture("hit/", "skip").await;
        assert_eq!(media_at(&eng, at(9, 10)).await, ("jhit/j.mp3".into(), Origin::Every));
        assert!(grid_store::get_hold(&eng.pool).await.unwrap().is_some());
        // The rule is no longer due (reset at the first track): the group
        // keeps the air all the same.
        assert_eq!(media_at(&eng, at(9, 11)).await, ("hit/h.mp3".into(), Origin::Every));
        // Provenance logged for the stats: the group, its leaf, the rule.
        let rows: Vec<(String, Option<String>, Option<String>, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT rel_path, rule_id, origin, playlist_ref, leaf_ref FROM broadcast_log ORDER BY id")
                .fetch_all(&eng.pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![
                ("jhit/j.mp3".into(), Some("OneHit".into()), Some("Every".into()), Some("onehit".into()), Some("jhit".into())),
                ("hit/h.mp3".into(), Some("OneHit".into()), Some("Every".into()), Some("onehit".into()), Some("hit".into())),
            ]
        );
        // Cycle complete: hold released, back to the floor.
        assert!(grid_store::get_hold(&eng.pool).await.unwrap().is_none());
        assert_eq!(media_at(&eng, at(9, 15)).await.1, Origin::BaseRotation);
        // Next activation starts from the top.
        assert_eq!(media_at(&eng, at(9, 56)).await, ("jhit/j.mp3".into(), Origin::Every));
    }

    #[tokio::test]
    async fn a_wake_releases_the_held_group_and_airs_the_slot_of_now() {
        use crate::station_control::ControlAction;
        let (_d, eng) = hold_fixture("hit/", "skip").await;
        assert_eq!(media_at(&eng, at(9, 10)).await.0, "jhit/j.mp3");
        // Asleep mid-cycle, woken 30 min later.
        eng.control().sleep_now();
        assert!(eng.next_media(at(9, 20)).await.unwrap().halted.is_some());
        eng.control().apply(ControlAction::Wake, "stop-when-idle").unwrap();
        assert_eq!(media_at(&eng, at(9, 40)).await, ("music/a.mp3".into(), Origin::BaseRotation));
        assert!(grid_store::get_hold(&eng.pool).await.unwrap().is_none());
        // Reset: its next activation starts from the top.
        assert_eq!(media_at(&eng, at(9, 56)).await, ("jhit/j.mp3".into(), Origin::Every));
    }

    #[tokio::test]
    async fn a_pause_keeps_the_held_group() {
        use crate::station_control::ControlAction;
        let (_d, eng) = hold_fixture("hit/", "skip").await;
        assert_eq!(media_at(&eng, at(9, 10)).await.0, "jhit/j.mp3");
        eng.control().apply(ControlAction::Pause, "cli").unwrap();
        eng.control().apply(ControlAction::Resume, "cli").unwrap();
        assert_eq!(media_at(&eng, at(9, 12)).await.0, "hit/h.mp3");
    }

    #[tokio::test]
    async fn its_rule_restarts_the_group_from_the_top() {
        let (_d, eng) = hold_fixture("hit/", "skip").await;
        // An interrupted activation left the cursor on the second member.
        let mid = crate::group_state::GroupState { member_idx: 1, ..Default::default() };
        crate::group_state::set(&eng.pool, "onehit", &mid).await.unwrap();
        assert_eq!(media_at(&eng, at(9, 10)).await, ("jhit/j.mp3".into(), Origin::Every));
        assert_eq!(media_at(&eng, at(9, 11)).await, ("hit/h.mp3".into(), Origin::Every));
    }

    #[tokio::test]
    async fn the_hold_survives_a_restart() {
        let (_d, eng) = hold_fixture("hit/", "skip").await;
        assert_eq!(media_at(&eng, at(9, 10)).await.0, "jhit/j.mp3");
        let again = GridEngine::new(eng.pool.clone(), "UTC");
        assert_eq!(media_at(&again, at(9, 12)).await.0, "hit/h.mp3");
    }

    #[tokio::test]
    async fn an_empty_member_under_skip_ends_the_cycle_without_restarting_it() {
        let (_d, eng) = hold_fixture("nothing/", "skip").await;
        assert_eq!(media_at(&eng, at(9, 10)).await.0, "jhit/j.mp3");
        // `hit` is empty → skipped → end of the cycle: no second jingle.
        assert_eq!(media_at(&eng, at(9, 11)).await.1, Origin::BaseRotation);
        assert!(grid_store::get_hold(&eng.pool).await.unwrap().is_none());
        assert_eq!(
            crate::group_state::get(&eng.pool, "onehit").await.unwrap(),
            crate::group_state::GroupState::default()
        );
    }

    #[tokio::test]
    async fn an_empty_member_under_abort_releases_the_hold_and_resets_the_group() {
        let (_d, eng) = hold_fixture("nothing/", "abort").await;
        assert_eq!(media_at(&eng, at(9, 10)).await.0, "jhit/j.mp3");
        assert_eq!(media_at(&eng, at(9, 11)).await.1, Origin::BaseRotation);
        assert!(grid_store::get_hold(&eng.pool).await.unwrap().is_none());
        // Next activation starts with the first member again.
        assert_eq!(media_at(&eng, at(9, 56)).await.0, "jhit/j.mp3");
    }

    #[tokio::test]
    async fn a_hard_rendez_vous_cuts_in_and_the_held_group_resumes() {
        let (_d, eng) = hold_fixture("hit/", "skip").await;
        assert_eq!(media_at(&eng, at(9, 59)).await.0, "jhit/j.mp3");
        let cut = eng.air_at_clock_hard(at(10, 0), at(10, 0)).await.unwrap().unwrap();
        assert_eq!(cut.media_path.as_deref(), Some("top/t.mp3"));
        assert_eq!(media_at(&eng, at(10, 1)).await, ("hit/h.mp3".into(), Origin::Every));
        assert_eq!(media_at(&eng, at(10, 5)).await.1, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn a_hold_whose_rule_is_disabled_is_dropped() {
        let (_d, eng) = hold_fixture("hit/", "skip").await;
        assert_eq!(media_at(&eng, at(9, 10)).await.0, "jhit/j.mp3");
        sqlx::query("UPDATE grid_rule SET enabled = 0 WHERE id = 'OneHit'")
            .execute(&eng.pool)
            .await
            .unwrap();
        assert_eq!(media_at(&eng, at(9, 11)).await.1, Origin::BaseRotation);
        assert!(grid_store::get_hold(&eng.pool).await.unwrap().is_none());
    }

    // ----- AtClock hard: the timer's side ---------------------------------

    /// `music` floor + `news` AtClock HARD every 15 min (+ an optional soft
    /// jingle rule), each playlist one file.
    async fn hard_fixture(with_soft: bool) -> (tempfile::TempDir, GridEngine) {
        let (dir, eng) = engine().await;
        let m = |p: &str| crate::media::ScannedMedia {
            rel_path: p.into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 60_000,
            size_bytes: 1,
            mtime_ns: 0,
        };
        crate::media_index::replace_library(&eng.pool, &[m("music/a.mp3"), m("news/n.mp3"), m("jingle/j.mp3")], 1000)
            .await
            .unwrap();
        for r in ["music", "news", "jingle"] {
            let toml = format!(
                "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                 [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{r}/\"\n"
            );
            let pl = crate::playlist::Playlist::parse(&toml).unwrap();
            crate::store::upsert(&eng.pool, r, &pl, &toml, Some(r)).await.unwrap();
        }
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "music".into() }))
            .await
            .unwrap();
        let clock_rule = |id: &str, r: &str, n: u32, mode: Mode| {
            rule(
                id,
                RuleKind::AtClock {
                    playlist_ref: r.into(),
                    anchor: ClockAnchor::EveryMinutes(n),
                    mode,
                    expiry_secs: Some(600),
                },
            )
        };
        insert_rule(&eng.pool, &clock_rule("news", "news", 15, Mode::Hard)).await.unwrap();
        if with_soft {
            insert_rule(&eng.pool, &clock_rule("jingle", "jingle", 5, Mode::Soft)).await.unwrap();
        }
        (dir, eng)
    }

    #[tokio::test]
    async fn next_hard_mark_is_the_next_hard_rendez_vous_only() {
        let (_d, eng) = hard_fixture(true).await;
        // 09:07:30 → 09:15:00 (the soft :10 jingle mark is not a hard mark).
        assert_eq!(eng.next_hard_mark(Epoch(at(9, 7).0 + 30)).await.unwrap(), Some(at(9, 15)));
        // Exactly on the mark, and a few seconds after it: still this mark.
        assert_eq!(eng.next_hard_mark(at(9, 15)).await.unwrap(), Some(at(9, 15)));
        assert_eq!(eng.next_hard_mark(Epoch(at(9, 15).0 + 8)).await.unwrap(), Some(at(9, 15)));
        // Past the grace: the next one.
        assert_eq!(eng.next_hard_mark(Epoch(at(9, 15).0 + 30)).await.unwrap(), Some(at(9, 30)));
        // Once the 09:15 occurrence is consumed, it is skipped.
        eng.air_at_clock_hard(at(9, 15), at(9, 15)).await.unwrap().unwrap();
        assert_eq!(eng.next_hard_mark(at(9, 15)).await.unwrap(), Some(at(9, 30)));
    }

    #[tokio::test]
    async fn next_hard_mark_is_none_without_hard_rules() {
        let (_d, eng) = engine().await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "music".into() }))
            .await
            .unwrap();
        assert_eq!(eng.next_hard_mark(at(9, 0)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_hard_mark_on_time_airs_now_and_is_consumed() {
        let (_d, eng) = hard_fixture(false).await;
        let r = eng.air_at_clock_hard(at(9, 15), Epoch(at(9, 15).0 + 2)).await.unwrap().unwrap();
        assert_eq!(r.media_path.as_deref(), Some("news/n.mp3"));
        assert_eq!(r.decision.origin, Origin::AtClockHard);
        assert_eq!(r.leaf_ref.as_deref(), Some("news"));
        // The next pull at the same mark does not air it again.
        let pull = eng.next_media(Epoch(at(9, 15).0 + 5)).await.unwrap();
        assert_eq!(pull.media_path.as_deref(), Some("music/a.mp3"));
        // Nor does a second cut.
        assert!(eng.air_at_clock_hard(at(9, 15), Epoch(at(9, 15).0 + 5)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_late_hard_mark_is_not_cut_and_stays_soft() {
        let (_d, eng) = hard_fixture(false).await;
        // 30 s late (restart, clock jump): no cut…
        assert!(eng.air_at_clock_hard(at(9, 15), Epoch(at(9, 15).0 + 30)).await.unwrap().is_none());
        // …the occurrence is still free: the next pull airs it (soft path).
        let pull = eng.next_media(Epoch(at(9, 16).0)).await.unwrap();
        assert_eq!(pull.media_path.as_deref(), Some("news/n.mp3"));
        // Too early is not a cut either.
        assert!(eng.air_at_clock_hard(at(9, 30), Epoch(at(9, 30).0 - 1)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_paused_station_is_not_cut_and_airs_the_mark_once_resumed() {
        use crate::station_control::ControlAction;
        let (_d, eng) = hard_fixture(false).await;
        eng.control().apply(ControlAction::Pause, "cli").unwrap();
        assert!(eng.air_at_clock_hard(at(9, 15), at(9, 15)).await.unwrap().is_none());
        eng.control().apply(ControlAction::Resume, "cli").unwrap();
        let pull = eng.next_media(Epoch(at(9, 17).0)).await.unwrap();
        assert_eq!(pull.media_path.as_deref(), Some("news/n.mp3"), "within expiry: soft");
    }

    async fn preview_leaves(pool: &SqlitePool, refs: &[&str]) {
        let toml = r#"name = "Preview fixture"
[selection]
mode = "dynamic""#;
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        for reference in refs {
            crate::store::upsert(pool, reference, &pl, toml, Some(reference)).await.unwrap();
        }
    }

    #[tokio::test]
    async fn preview_projects_base_daypart_and_marks() {
        let (_dir, eng) = engine().await; // tz = UTC
        preview_leaves(&eng.pool, &["general", "jazz", "jingle"]).await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "mid",
                RuleKind::DayPart {
                    playlist_ref: "jazz".into(),
                    start: crate::resolver::WallClock { hour: 9, minute: 0 },
                    end: Some(crate::resolver::WallClock { hour: 10, minute: 0 }),
                },
            ),
        )
        .await
        .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "top",
                RuleKind::AtClock {
                    playlist_ref: "jingle".into(),
                    anchor: ClockAnchor::EveryMinutes(30),
                    mode: Mode::Soft,
                    expiry_secs: None,
                },
            ),
        )
        .await
        .unwrap();
        // A track-counted Every is NOT placed on the timeline, only noted.
        insert_rule(
            &eng.pool,
            &rule("cool", RuleKind::Every { playlist_ref: "never".into(), cadence: Cadence::Tracks(1) }),
        )
        .await
        .unwrap();

        // Window 08:00 → 11:00 (UTC, so wall == epoch hour).
        let pv = eng.preview(at(8, 0), 3 * 3600).await.unwrap();
        let occ = &pv.occurrences;

        // First occurrence starts exactly at `from`.
        assert_eq!(occ.first().unwrap().epoch, at(8, 0));
        // No two consecutive occurrences share the same decision (change-only).
        for w in occ.windows(2) {
            assert_ne!(
                (&w[0].origin, &w[0].playlist_ref),
                (&w[1].origin, &w[1].playlist_ref),
                "consecutive occurrences must differ"
            );
        }
        // The DayPart shows up as jazz, the marks as jingle.
        assert!(occ.iter().any(|o| o.origin == Origin::DayPart && o.playlist_ref == "jazz"));
        assert!(occ.iter().any(|o| o.origin == Origin::AtClockSoft && o.playlist_ref == "jingle"));
        // A track-counted Every never lands on the timeline...
        assert!(occ.iter().all(|o| o.origin != Origin::Every));
        // ...but is surfaced once, apart, as an indicative rule.
        assert_eq!(pv.indicative.len(), 1);
        assert_eq!(pv.indicative[0].playlist_ref, "never");
        // A mark is an instant, not a segment: the 08:30 jingle is followed by
        // a return to the floor at 08:31.
        let mark = occ.iter().position(|o| o.epoch == at(8, 30)).expect("08:30 mark");
        assert_eq!(occ[mark].origin, Origin::AtClockSoft);
        assert_eq!(occ[mark + 1].epoch, at(8, 31));
        assert_eq!(occ[mark + 1].origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn preview_projects_an_elapsed_every_at_its_cadence() {
        let (_dir, eng) = engine().await; // tz = UTC
        preview_leaves(&eng.pool, &["general", "flash"]).await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        // A 30-minute elapsed cooldown.
        insert_rule(
            &eng.pool,
            &rule(
                "news",
                RuleKind::Every { playlist_ref: "flash".into(), cadence: Cadence::Elapsed(1800) },
            ),
        )
        .await
        .unwrap();

        // Window 08:00 → 10:00.
        let pv = eng.preview(at(8, 0), 2 * 3600).await.unwrap();
        let occ = &pv.occurrences;

        // Seeded at the window start → no mark on the very edge.
        assert!(!occ.iter().any(|o| o.epoch == at(8, 0) && o.origin == Origin::Every));
        // First mark one cadence in (08:30), then back to the floor at 08:31 —
        // an instant, not a segment.
        let m = occ.iter().position(|o| o.epoch == at(8, 30)).expect("08:30 mark");
        assert_eq!(occ[m].origin, Origin::Every);
        assert_eq!(occ[m].playlist_ref, "flash");
        assert_eq!(occ[m + 1].epoch, at(8, 31));
        assert_eq!(occ[m + 1].origin, Origin::BaseRotation);
        // And it recurs at the cadence: 09:00, 09:30.
        assert!(occ.iter().any(|o| o.epoch == at(9, 0) && o.origin == Origin::Every));
        assert!(occ.iter().any(|o| o.epoch == at(9, 30) && o.origin == Origin::Every));
        // An elapsed Every is projected onto the timeline, not indicative.
        assert!(pv.indicative.is_empty());
    }

    #[tokio::test]
    async fn preview_projects_live_windows_apart_from_the_timeline() {
        use crate::resolver::WallClock;
        let (_dir, eng) = engine().await; // tz = UTC
        preview_leaves(&eng.pool, &["general", "evening"]).await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        // A live slot at 20:00, until the next slot starts (the 22:00 day part).
        insert_rule(
            &eng.pool,
            &rule("dj-alex", RuleKind::Live { dj: "alex".into(), start: WallClock { hour: 20, minute: 0 } }),
        )
        .await
        .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "late",
                RuleKind::DayPart {
                    playlist_ref: "evening".into(),
                    start: WallClock { hour: 22, minute: 0 },
                    end: Some(WallClock { hour: 23, minute: 0 }),
                },
            ),
        )
        .await
        .unwrap();

        let pv = eng.preview(at(19, 0), 4 * 3600).await.unwrap();
        assert_eq!(pv.live.len(), 1, "{:?}", pv.live);
        let w = &pv.live[0];
        assert_eq!((w.rule_id.as_str(), w.dj.as_str()), ("dj-alex", "alex"));
        assert_eq!(w.opens, at(20, 0));
        assert!(!w.open_before);
        assert_eq!(w.closes.as_ref().map(|c| c.0), Some(at(22, 0)));
        // Never on the ordered timeline: it selects no playlist.
        assert!(pv.occurrences.iter().all(|o| o.rule_id != "dj-alex"));

        // A projection that starts inside the window says it opened earlier.
        let pv = eng.preview(at(21, 0), 3600).await.unwrap();
        assert_eq!(pv.live.len(), 1);
        assert!(pv.live[0].open_before);
        assert_eq!(pv.live[0].opens, at(21, 0));
        assert!(pv.live[0].closes.is_none(), "still open at the end of the projection");
    }

    #[tokio::test]
    async fn preview_of_a_bare_floor_is_a_single_segment() {
        let (_dir, eng) = engine().await;
        preview_leaves(&eng.pool, &["general"]).await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        let pv = eng.preview(at(0, 0), 6 * 3600).await.unwrap();
        assert_eq!(pv.occurrences.len(), 1, "nothing changes → one segment");
        assert_eq!(pv.occurrences[0].origin, Origin::BaseRotation);
        assert_eq!(pv.occurrences[0].playlist_ref, "general");
        assert!(pv.indicative.is_empty());
    }

    #[tokio::test]
    async fn next_media_falls_through_empty_daypart_to_floor() {
        use crate::resolver::WallClock;
        let (_dir, eng) = engine().await; // tz = UTC

        // Media only under music/ — the floor can produce, the daypart cannot.
        crate::media_index::replace_library(
            &eng.pool,
            &[crate::media::ScannedMedia {
                rel_path: "music/a.mp3".into(),
                title: Some("t".into()),
                artist: None,
                album: None,
                year: None,
                genres: vec![],
                duration_ms: 1000,
                size_bytes: 1,
                mtime_ns: 0,
            }],
            1000,
        )
        .await
        .unwrap();

        for (r, prefix) in [("music", "music/"), ("jazz", "jazz/")] {
            let toml = format!(
                "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                 [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{prefix}\"\n"
            );
            let pl = crate::playlist::Playlist::parse(&toml).unwrap();
            crate::store::upsert(&eng.pool, r, &pl, &toml, Some(r)).await.unwrap();
        }

        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "music".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "jazz",
                RuleKind::DayPart {
                    playlist_ref: "jazz".into(),
                    start: WallClock { hour: 8, minute: 0 },
                    end: Some(WallClock { hour: 10, minute: 0 }),
                },
            ),
        )
        .await
        .unwrap();

        // 09:00: the jazz daypart outranks the floor but its pool is empty →
        // fall through to the music floor rather than erroring.
        let r = eng.next_media(at(9, 0)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("music/a.mp3"));
        assert_eq!(r.decision.origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn preview_decomposes_a_sequence_group_occurrence() {
        let (_dir, eng) = engine().await; // tz = UTC
        preview_leaves(&eng.pool, &["rock", "jingle"]).await;
        let toml = r#"
            name = "Matinee"
            [selection]
            mode = "group"
            strategy = "sequence"
            members = [{ ref = "rock", runtime = "20m" }, { ref = "jingle", take = 1 }]
        "#;
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        crate::store::upsert(&eng.pool, "matinee", &pl, toml, Some("matinee"))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule("floor", RuleKind::BaseRotation { playlist_ref: "matinee".into() }),
        )
        .await
        .unwrap();

        let pv = eng.preview(at(0, 0), 3600).await.unwrap();
        let occ = pv.occurrences.first().expect("one segment");
        let g = occ.group.as_ref().expect("group decomposition present");
        assert_eq!(g.strategy, crate::playlist::Strategy::Sequence);
        assert_eq!(g.members.len(), 2);
        assert_eq!(g.members[0].r#ref, "rock");
        assert_eq!(g.members[0].quota, Some(crate::playlist::MemberQuota::Runtime(1200)));
        assert_eq!(g.members[0].offset_secs, Some(0));
        assert_eq!(g.members[1].quota, Some(crate::playlist::MemberQuota::Take(1)));
        assert_eq!(g.members[1].offset_secs, None);
    }

    #[tokio::test]
    async fn next_media_logs_starts_and_then_excludes_within_the_window() {
        let (_dir, eng) = engine().await; // tz = UTC
        crate::media_index::replace_library(
            &eng.pool,
            &[
                crate::media::ScannedMedia {
                    rel_path: "a.mp3".into(),
                    title: None,
                    artist: Some("X".into()),
                    album: None,
                    year: None,
                    genres: vec![],
                    duration_ms: 1000,
                    size_bytes: 1,
                    mtime_ns: 0,
                },
                crate::media::ScannedMedia {
                    rel_path: "b.mp3".into(),
                    title: None,
                    artist: Some("Y".into()),
                    album: None,
                    year: None,
                    genres: vec![],
                    duration_ms: 1000,
                    size_bytes: 1,
                    mtime_ns: 0,
                },
            ],
            1000,
        )
        .await
        .unwrap();
        let toml = "name = \"Rot\"\n[selection]\nmode = \"dynamic\"\norder = \"sequential\"\n\
                    [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"\"\n\
                    [broadcast.constraints]\nno_same_track_within = \"1h\"\n";
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        crate::store::upsert(&eng.pool, "rot", &pl, toml, Some("rot"))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule("floor", RuleKind::BaseRotation { playlist_ref: "rot".into() }),
        )
        .await
        .unwrap();

        // First pull logs a track; 30 min later the 1h window bars it → the
        // next pull must pick the other track.
        let first = eng.next_media(Epoch(10_000)).await.unwrap().media_path.unwrap();
        let second = eng
            .next_media(Epoch(10_000 + 1800))
            .await
            .unwrap()
            .media_path
            .unwrap();
        assert_ne!(first, second, "anti-repetition excludes the just-played track");
        // Both starts are recorded in the station history.
        let logged = crate::broadcast_log::tracks_since(&eng.pool, 0).await.unwrap();
        assert!(logged.contains(&first) && logged.contains(&second));
    }

    #[tokio::test]
    async fn on_episode_finished_marks_only_unplayed_only_playlists() {
        let (_dir, eng) = engine().await;
        crate::media_index::replace_library(
            &eng.pool,
            &[crate::media::ScannedMedia {
                rel_path: "pod/ep1.mp3".into(),
                title: None,
                artist: None,
                album: None,
                year: None,
                genres: vec![],
                duration_ms: 1000,
                size_bytes: 42,
                mtime_ns: 7,
            }],
            1000,
        )
        .await
        .unwrap();
        let feu = "name = \"F\"\n[selection]\nmode = \"dynamic\"\norder = \"oldest\"\n\
                   order_by = \"filename\"\nunplayed_only = true\n[[selection.filter]]\n\
                   field = \"path\"\nop = \"prefix\"\nvalue = \"pod/\"\n";
        let pl = crate::playlist::Playlist::parse(feu).unwrap();
        crate::store::upsert(&eng.pool, "feu", &pl, feu, Some("feu")).await.unwrap();
        let rot = "name = \"R\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                   [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"pod/\"\n";
        let pl2 = crate::playlist::Playlist::parse(rot).unwrap();
        crate::store::upsert(&eng.pool, "rot", &pl2, rot, Some("rot")).await.unwrap();

        // Finishing under the unplayed_only playlist records it (guard from index).
        eng.on_episode_finished("feu", "pod/ep1.mp3", Epoch(5000)).await.unwrap();
        assert!(crate::episode_play::played_matching(&eng.pool, "feu")
            .await
            .unwrap()
            .contains("pod/ep1.mp3"));
        // The same media under a plain rotation keeps no play history.
        eng.on_episode_finished("rot", "pod/ep1.mp3", Epoch(5000)).await.unwrap();
        assert!(crate::episode_play::played_matching(&eng.pool, "rot")
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_open_day_part_is_sized_until_the_next_start() {
        let open = |id: &str, h: u8| {
            rule(
                id,
                RuleKind::DayPart {
                    playlist_ref: id.into(),
                    start: crate::resolver::WallClock { hour: h, minute: 0 },
                    end: None,
                },
            )
        };
        let grid = Grid { rules: vec![open("morning", 6), open("day", 12), open("night", 20)] };
        let secs = |id: &str| match classify_rule(grid.rules.iter().find(|r| r.id == id).unwrap(), &grid).2 {
            Demand::Duration(s) => s,
            Demand::Punctual => panic!("a day part has a duration"),
        };
        assert_eq!(secs("morning"), 6 * 3600);
        assert_eq!(secs("night"), 10 * 3600, "wraps past midnight to 06:00");
        let lone = Grid { rules: vec![open("only", 6)] };
        match classify_rule(&lone.rules[0], &lone).2 {
            Demand::Duration(s) => assert_eq!(s, 86_400),
            Demand::Punctual => panic!(),
        }
    }

    #[test]
    fn played_to_end_allows_the_crossfade_but_not_a_cut() {
        assert!(played_to_end(600, 600_000));
        assert!(played_to_end(597, 600_000), "the next track starts during the crossfade");
        assert!(played_to_end(585, 600_000), "15 s of slack");
        assert!(!played_to_end(584, 600_000));
        assert!(!played_to_end(100, 600_000));
        assert!(played_to_end(0, 10_000), "a sting shorter than the slack");
    }

    #[tokio::test]
    async fn track_left_writes_the_end_on_its_log_row() {
        let (_d, eng) = hard_fixture(false).await;
        let r = eng.next_media(at(9, 3)).await.unwrap();
        let id = r.log_id.expect("a logged file");
        let media = r.media_path.clone().unwrap();
        assert_eq!(eng.track_left(Some(id), &media, None, 20, at(9, 4)).await.unwrap(), Some(false));
        let row = crate::broadcast_log::row(&eng.pool, id).await.unwrap().unwrap();
        assert_eq!((row.left_at, row.played_to_end), (Some(at(9, 4).0), Some(false)));
        // Unindexed media: end written, verdict unknown.
        assert_eq!(eng.track_left(None, "ghost.mp3", None, 999, at(9, 5)).await.unwrap(), None);
    }

    #[tokio::test]
    async fn on_track_left_marks_only_a_full_play_of_a_leaf() {
        let (_dir, eng) = engine().await;
        crate::media_index::replace_library(
            &eng.pool,
            &[crate::media::ScannedMedia {
                rel_path: "pod/ep1.mp3".into(),
                title: None,
                artist: None,
                album: None,
                year: None,
                genres: vec![],
                duration_ms: 600_000,
                size_bytes: 1,
                mtime_ns: 0,
            }],
            1000,
        )
        .await
        .unwrap();
        let feu = "name = \"F\"\n[selection]\nmode = \"dynamic\"\norder = \"oldest\"\n\
                   order_by = \"filename\"\nunplayed_only = true\n[[selection.filter]]\n\
                   field = \"path\"\nop = \"prefix\"\nvalue = \"pod/\"\n";
        let pl = crate::playlist::Playlist::parse(feu).unwrap();
        crate::store::upsert(&eng.pool, "feu", &pl, feu, Some("feu")).await.unwrap();
        let played = || async {
            crate::episode_play::played_matching(&eng.pool, "feu").await.unwrap()
        };

        // Cut short: not a full play, nothing marked.
        assert!(!eng.on_track_left("pod/ep1.mp3", Some("feu"), 100, Epoch(5000)).await.unwrap());
        assert!(played().await.is_empty());
        // A media override (no leaf): played to the end, but nothing to mark.
        assert!(eng.on_track_left("pod/ep1.mp3", None, 600, Epoch(5000)).await.unwrap());
        assert!(played().await.is_empty());
        // Unknown media: no duration, never counted.
        assert!(!eng.on_track_left("ghost.mp3", Some("feu"), 9999, Epoch(5000)).await.unwrap());
        // A full play of the leaf: marked.
        assert!(eng.on_track_left("pod/ep1.mp3", Some("feu"), 598, Epoch(5000)).await.unwrap());
        assert!(played().await.contains("pod/ep1.mp3"));
    }

    #[tokio::test]
    async fn next_media_relays_a_remote_without_touching_disk() {
        let (_dir, eng) = engine().await; // tz = UTC
        // No media on disk; the base is a remote relay.
        let toml = "name = \"night\"\n[selection]\nmode = \"remote\"\nurl = \"http://nightmusic.live\"\n";
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        crate::store::upsert(&eng.pool, "night", &pl, toml, Some("night")).await.unwrap();
        insert_rule(
            &eng.pool,
            &rule("floor", RuleKind::BaseRotation { playlist_ref: "night".into() }),
        )
        .await
        .unwrap();

        let r = eng.next_media(Epoch(1000)).await.unwrap();
        assert!(r.stream, "a remote resolves to a stream");
        assert_eq!(r.media_path.as_deref(), Some("http://nightmusic.live"));
        assert_eq!(r.decision.origin, Origin::BaseRotation);
        // A stream is never logged into the file-oriented broadcast history.
        assert!(crate::broadcast_log::tracks_since(&eng.pool, 0)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn enqueue_then_next_media_pops_the_queue() {
        let (_dir, eng) = engine().await; // tz = UTC
        let toml = "name = \"Req\"\n[selection]\nmode = \"queue\"\norder = \"fifo\"\nmax_len = 2\n";
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        crate::store::upsert(&eng.pool, "req", &pl, toml, Some("req")).await.unwrap();
        insert_rule(
            &eng.pool,
            &rule("floor", RuleKind::BaseRotation { playlist_ref: "req".into() }),
        )
        .await
        .unwrap();

        // Empty queue → the base produces nothing → PoolEmpty surfaces.
        assert!(matches!(
            eng.next_media(Epoch(1)).await,
            Err(EngineError::Selection(crate::selection::SelectionError::PoolEmpty))
        ));
        // Enqueue, then it plays and is consumed.
        let out = eng.enqueue("req", "req/a.mp3").await.unwrap();
        assert!(out.accepted);
        assert_eq!(out.len, 1);
        let r = eng.next_media(Epoch(2)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("req/a.mp3"));
        // Consumed → empty again.
        assert!(matches!(
            eng.next_media(Epoch(3)).await,
            Err(EngineError::Selection(crate::selection::SelectionError::PoolEmpty))
        ));
        // Enqueue to an unknown ref, and to a non-queue playlist → errors.
        assert!(eng.enqueue("nope", "x.mp3").await.is_err());
        let rtoml = "name = \"R\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n";
        let rpl = crate::playlist::Playlist::parse(rtoml).unwrap();
        crate::store::upsert(&eng.pool, "rot", &rpl, rtoml, Some("rot")).await.unwrap();
        assert!(matches!(
            eng.enqueue("rot", "x.mp3").await,
            Err(EngineError::Selection(crate::selection::SelectionError::NotAQueue(r, crate::playlist::Mode::Dynamic)))
                if r == "rot"
        ));
    }

    // ----- A2: broadcast gate + override layer ----------------------------

    /// Engine with one media `music/a.mp3` and a `music` floor over it.
    async fn engine_with_floor() -> (tempfile::TempDir, GridEngine) {
        let (dir, eng) = engine().await;
        let m = |p: &str| crate::media::ScannedMedia {
            rel_path: p.into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 1000,
            size_bytes: 1,
            mtime_ns: 0,
        };
        crate::media_index::replace_library(
            &eng.pool,
            &[m("music/a.mp3"), m("news/flash.mp3"), m("news/flash2.mp3")],
            1000,
        )
        .await
        .unwrap();
        for (r, prefix, order) in [("music", "music/", "shuffle"), ("news", "news/", "sequential")] {
            let toml = format!(
                "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"{order}\"\n\
                 [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{prefix}\"\n"
            );
            let pl = crate::playlist::Playlist::parse(&toml).unwrap();
            crate::store::upsert(&eng.pool, r, &pl, &toml, Some(r)).await.unwrap();
        }
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "music".into() }))
            .await
            .unwrap();
        (dir, eng)
    }

    fn media_override(p: &str) -> crate::station_control::OverrideRequest {
        crate::station_control::OverrideRequest {
            content: OverrideContent::Media(p.into()),
            mode: Default::default(),
            expiry: None,
            tracks: None,
        }
    }

    #[tokio::test]
    async fn sleeping_station_resolves_nothing_and_logs_nothing() {
        use crate::station_control::ControlAction;
        let (_dir, eng) = engine_with_floor().await;
        eng.control().sleep_now();
        let r = eng.next_media(at(9, 0)).await.unwrap();
        assert_eq!(r.halted, Some(BroadcastState::Sleeping));
        assert!(r.media_path.is_none());
        assert!(crate::broadcast_log::tracks_since(&eng.pool, 0).await.unwrap().is_empty());
        // Wake → the grid plays again.
        eng.control().apply(ControlAction::Wake, "stop-when-idle").unwrap();
        let r = eng.next_media(at(9, 1)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("music/a.mp3"));
        assert!(r.halted.is_none());
    }

    #[tokio::test]
    async fn draining_sleeps_at_the_boundary_once_listeners_are_zero() {
        use crate::station_control::ControlAction;
        let (_dir, eng) = engine_with_floor().await;
        eng.control().apply(ControlAction::StopWhenIdle, "cli").unwrap();
        eng.control().sample_listeners(2);
        assert_eq!(
            eng.next_media(at(9, 0)).await.unwrap().media_path.as_deref(),
            Some("music/a.mp3"),
            "audience present → keeps playing while armed"
        );
        eng.control().sample_listeners(0);
        let r = eng.next_media(at(9, 3)).await.unwrap();
        assert_eq!(r.halted, Some(BroadcastState::Sleeping));
        assert_eq!(eng.control().state(), BroadcastState::Sleeping);
    }

    #[tokio::test]
    async fn a_media_override_preempts_the_grid_once() {
        let (_dir, eng) = engine_with_floor().await;
        // Arbitrary media: not required to be indexed (media_root off in tests).
        eng.control().push_override(media_override("jingles/unindexed.mp3"), "cli").unwrap();
        let r = eng.next_media(at(9, 0)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("jingles/unindexed.mp3"));
        assert_eq!(r.override_source.as_deref(), Some("cli"));
        // Consumed → back to the grid.
        let r = eng.next_media(at(9, 1)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("music/a.mp3"));
        assert!(r.override_source.is_none());
        assert_eq!(r.decision.origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn a_media_override_missing_on_disk_is_dropped_and_marked_unavailable() {
        let (dir, eng) = engine_with_floor().await;
        let root = dir.path().join("media");
        std::fs::create_dir_all(root.join("music")).unwrap();
        std::fs::write(root.join("music/a.mp3"), b"x").unwrap();
        let eng = eng.with_media_root(&root);
        // Indexed, but gone from the disk since the last scan.
        eng.control().push_override(media_override("news/flash.mp3"), "cli").unwrap();
        let r = eng.next_media(at(9, 0)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("music/a.mp3"), "dropped: the grid airs");
        assert!(eng.control().list_overrides().is_empty());
        let available: Vec<_> =
            crate::media_index::list(&eng.pool, true, &[]).await.unwrap().into_iter().map(|m| m.rel_path).collect();
        assert!(!available.contains(&"news/flash.mp3".to_string()), "{available:?}");
    }

    #[tokio::test]
    async fn a_playlist_override_holds_the_air_for_its_tracks() {
        let (_dir, eng) = engine_with_floor().await;
        eng.control()
            .push_override(
                crate::station_control::OverrideRequest {
                    content: OverrideContent::Playlist("News".into()),
                    mode: crate::station_control::OverrideMode::Hard, // degraded to soft
                    expiry: None,
                    tracks: Some(2),
                },
                "urgence",
            )
            .unwrap();
        let one = eng.next_media(at(9, 0)).await.unwrap();
        let two = eng.next_media(at(9, 1)).await.unwrap();
        assert_eq!(one.media_path.as_deref(), Some("news/flash.mp3"));
        assert_eq!(two.media_path.as_deref(), Some("news/flash2.mp3"));
        assert_eq!(one.decision.playlist_ref.as_deref(), Some("news"));
        assert_eq!(two.override_source.as_deref(), Some("urgence"));
        // Exhausted → the grid.
        assert_eq!(
            eng.next_media(at(9, 2)).await.unwrap().media_path.as_deref(),
            Some("music/a.mp3")
        );
    }

    /// `block` = sequence group: `news` (2 tracks) then `music` (1).
    async fn with_block_group(eng: &GridEngine) {
        let group = "name = \"block\"\n[selection]\nmode = \"group\"\nstrategy = \"sequence\"\n\
                     members = [ { ref = \"news\", take = 2 }, { ref = \"music\", take = 1 } ]\n";
        let pl = crate::playlist::Playlist::parse(group).unwrap();
        crate::store::upsert(&eng.pool, "block", &pl, group, Some("block")).await.unwrap();
    }

    fn playlist_override(r: &str, tracks: Option<u32>) -> crate::station_control::OverrideRequest {
        crate::station_control::OverrideRequest {
            content: OverrideContent::Playlist(r.into()),
            mode: Default::default(),
            expiry: None,
            tracks,
        }
    }

    #[tokio::test]
    async fn a_group_override_airs_its_whole_cycle_from_the_top() {
        let (_dir, eng) = engine_with_floor().await;
        with_block_group(&eng).await;
        // The group's cycle was left mid-way (a grid turn): the override
        // restarts it from the top anyway.
        crate::group_state::set(
            &eng.pool,
            "block",
            &crate::group_state::GroupState { member_idx: 1, ..Default::default() },
        )
        .await
        .unwrap();
        eng.control().push_override(playlist_override("block", None), "cli").unwrap();
        let mut aired = Vec::new();
        for m in 0..3 {
            let r = eng.next_media(at(9, m)).await.unwrap();
            assert_eq!(r.override_source.as_deref(), Some("cli"), "track {m}");
            aired.push(r.media_path.unwrap());
        }
        assert_eq!(aired, ["news/flash.mp3", "news/flash2.mp3", "music/a.mp3"]);
        assert!(eng.control().list_overrides().is_empty(), "cycle over: override gone");
        let r = eng.next_media(at(9, 3)).await.unwrap();
        assert!(r.override_source.is_none());
        assert_eq!(r.decision.origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn tracks_caps_a_group_override() {
        let (_dir, eng) = engine_with_floor().await;
        with_block_group(&eng).await;
        eng.control().push_override(playlist_override("block", Some(2)), "cli").unwrap();
        assert_eq!(eng.next_media(at(9, 0)).await.unwrap().media_path.as_deref(), Some("news/flash.mp3"));
        assert_eq!(eng.next_media(at(9, 1)).await.unwrap().media_path.as_deref(), Some("news/flash2.mp3"));
        let r = eng.next_media(at(9, 2)).await.unwrap();
        assert!(r.override_source.is_none(), "capped at 2");
        assert_eq!(r.decision.origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn a_hard_group_override_carries_on_after_the_cut() {
        let (_dir, eng) = engine_with_floor().await;
        with_block_group(&eng).await;
        let id = eng.control().push_override(playlist_override("block", None), "cli").unwrap().id;
        // The cut airs the first track out of queue order…
        let r = eng.air_override_now(id, at(9, 0)).await.unwrap().unwrap();
        assert_eq!(r.media_path.as_deref(), Some("news/flash.mp3"));
        // …the rest of the cycle follows at the next boundaries.
        assert_eq!(eng.next_media(at(9, 1)).await.unwrap().media_path.as_deref(), Some("news/flash2.mp3"));
        assert_eq!(eng.next_media(at(9, 2)).await.unwrap().media_path.as_deref(), Some("music/a.mp3"));
        assert!(eng.next_media(at(9, 3)).await.unwrap().override_source.is_none());
    }

    #[tokio::test]
    async fn an_override_that_cannot_air_is_dropped_and_the_grid_takes_over() {
        let (_dir, eng) = engine_with_floor().await;
        eng.control()
            .push_override(
                crate::station_control::OverrideRequest {
                    content: OverrideContent::Playlist("ghost".into()),
                    mode: Default::default(),
                    expiry: None,
                    tracks: None,
                },
                "cli",
            )
            .unwrap();
        let r = eng.next_media(at(9, 0)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("music/a.mp3"));
        assert!(r.override_source.is_none());
        assert!(eng.control().list_overrides().is_empty(), "dropped, not retried");
    }

    #[tokio::test]
    async fn an_expired_override_is_abandoned_on_the_engine_clock() {
        let (_dir, eng) = engine_with_floor().await;
        eng.set_clock(Some(at(9, 0)));
        let mut req = media_override("news/flash.mp3");
        req.expiry = Some("1m".into());
        eng.control().push_override(req, "cli").unwrap();
        // Next boundary comes 5 minutes later: stale → grid.
        let r = eng.next_media(at(9, 5)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("music/a.mp3"));
        assert!(eng.control().list_overrides().is_empty());
    }
}
