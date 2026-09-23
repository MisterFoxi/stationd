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
    pub members: Vec<CoverageMember>,
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

/// Kind tag, referenced playlist, and temporal demand of a rule.
fn classify_rule(kind: &RuleKind) -> (&'static str, String, Demand) {
    match kind {
        RuleKind::BaseRotation { playlist_ref } => {
            // The floor must sustain a full day without forcing repeats.
            ("base_rotation", playlist_ref.clone(), Demand::Duration(86_400))
        }
        RuleKind::DayPart { playlist_ref, start, end } => {
            ("day_part", playlist_ref.clone(), Demand::Duration(wallclock_window_secs(start, end)))
        }
        RuleKind::AtClock { playlist_ref, .. } => ("at_clock", playlist_ref.clone(), Demand::Punctual),
        RuleKind::Every { playlist_ref, .. } => ("every", playlist_ref.clone(), Demand::Punctual),
    }
}

/// Window length of a DayPart in seconds, handling a cross-midnight rule
/// (`end ≤ start` → the window wraps to the next day).
fn wallclock_window_secs(start: &crate::resolver::WallClock, end: &crate::resolver::WallClock) -> i64 {
    let s = start.hour as i64 * 3600 + start.minute as i64 * 60;
    let e = end.hour as i64 * 3600 + end.minute as i64 * 60;
    if e > s { e - s } else { e + 86_400 - s }
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
    detail: String,
    members: Vec<CoverageMember>,
) -> CoverageEntry {
    CoverageEntry { rule_id, playlist_ref, kind, stats, verdict, detail, members }
}

/// Grade a resolved pool against both axes. Returns the verdict, a detail line
/// naming what fired, and the per-member breakdown (empty for a leaf).
fn verdict_for(
    playlist: &crate::playlist::Playlist,
    inspection: &PoolInspection,
    demand: Demand,
) -> (Verdict, String, Vec<CoverageMember>) {
    let stats = inspection.stats;
    let members = member_breakdown(inspection);

    // Empty pool → the rule can never produce: hard fail, nothing else matters.
    if stats.selected_count == Some(0) {
        return (Verdict::Insufficient, "pool vide".into(), members);
    }

    let mut verdict = Verdict::Ok;
    let mut reasons: Vec<String> = Vec::new();
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
                    reasons.push(format!(
                        "no_same_track_within {d} : pool {} < {d} (rejeu de piste forcé)",
                        fmt_hms(have_ms)
                    ));
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
                    reasons.push(format!(
                        "no_same_artist_within : {a} artiste(s) distinct(s) (rejeu d'artiste forcé)"
                    ));
                }
                None => reasons.push("no_same_artist_within non évalué (agrégat de groupe)".into()),
                _ => {}
            }
        }
    }
    if let Some(limit) = bc.and_then(|b| b.limit) {
        if let Some(count) = stats.selected_count {
            if count < limit as u64 {
                verdict = verdict.worst(Verdict::Thin);
                reasons.push(format!(
                    "limit {limit} : {count} média(s) distinct(s) dans le pool"
                ));
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
                    reasons.push(format!(
                        "source finie {} < créneau {} (ne remplit pas)",
                        fmt_hms(have_ms),
                        fmt_hms(need_ms)
                    ));
                }
            }
        }
    }

    // --- Group members: emptiness sinks the group; an under-sized quota loops ---
    if inspection.group.is_some() {
        let empty: Vec<&str> = members
            .iter()
            .filter(|m| m.stats.selected_count == Some(0))
            .map(|m| m.r#ref.as_str())
            .collect();
        if !empty.is_empty() {
            let policy = playlist
                .selection
                .on_member_unavailable
                .unwrap_or(crate::playlist::MemberUnavailable::Abort);
            match policy {
                crate::playlist::MemberUnavailable::Abort => {
                    verdict = verdict.worst(Verdict::Insufficient);
                    reasons.push(format!("membre(s) vide(s) [{}] → abort", empty.join(", ")));
                }
                crate::playlist::MemberUnavailable::Skip => {
                    verdict = verdict.worst(Verdict::Thin);
                    reasons.push(format!(
                        "membre(s) vide(s) [{}] → skip (dégradé)",
                        empty.join(", ")
                    ));
                }
            }
        }
        // A non-empty member whose quota exceeds its pool loops within its slot
        // (the case this check exists for). Signalled, never blocking.
        let looping: Vec<&str> = members
            .iter()
            .filter(|m| m.stats.selected_count != Some(0) && m.verdict == Verdict::Thin)
            .map(|m| m.r#ref.as_str())
            .collect();
        if !looping.is_empty() {
            verdict = verdict.worst(Verdict::Thin);
            reasons.push(format!(
                "membre(s) sous-dimensionné(s) [{}] → boucle dans le slot",
                looping.join(", ")
            ));
        }
    }

    let detail = if reasons.is_empty() { "ok".to_string() } else { reasons.join(" ; ") };
    (verdict, detail, members)
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
            let (verdict, detail) = member_verdict(m);
            CoverageMember { r#ref: m.r#ref.clone(), stats: m.stats, verdict, detail }
        })
        .collect()
}

/// Grade one group member against its own per-member quota. Empty pool → ✗. A
/// `runtime`/`take` quota larger than the member's pool means the member will
/// LOOP inside its slot before handing over — flagged ⚠ (this is the point of
/// the check: make the loops visible). Unknown pool size (remote/queue) → no
/// false alarm. A member with no quota (weighted/rotate) is judged on emptiness
/// only.
fn member_verdict(m: &crate::pool_inspection::InspectedMember) -> (Verdict, String) {
    use crate::playlist::MemberQuota;
    if m.stats.selected_count == Some(0) {
        return (Verdict::Insufficient, "pool vide".to_string());
    }
    match &m.quota {
        Some(MemberQuota::Runtime(secs)) => {
            if let Some(have_ms) = m.stats.total_duration_ms {
                let need_ms = (*secs).saturating_mul(1000);
                if have_ms < need_ms {
                    return (
                        Verdict::Thin,
                        format!(
                            "budget runtime {} > pool {} → boucle dans le slot",
                            fmt_hms(need_ms),
                            fmt_hms(have_ms)
                        ),
                    );
                }
            }
            (Verdict::Ok, "ok".to_string())
        }
        Some(MemberQuota::Take(n)) => {
            if let Some(count) = m.stats.selected_count {
                if count < *n as u64 {
                    return (
                        Verdict::Thin,
                        format!("take {n} > {count} piste(s) distincte(s) → répétition"),
                    );
                }
            }
            (Verdict::Ok, "ok".to_string())
        }
        None => (Verdict::Ok, "ok".to_string()),
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
        }
    }

    /// Share the station control (broadcast state, overrides, manual clock)
    /// with the plugin host surface and the broadcast gRPC service.
    pub fn with_control(mut self, control: StationControl) -> Self {
        self.control = control;
        self
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
        let grid = grid_index::load_grid(&self.pool).await?;
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
            if !rule.enabled {
                continue;
            }
            entries.push(self.coverage_for_rule(rule).await?);
        }
        let worst = entries.iter().fold(Verdict::Ok, |acc, e| acc.worst(e.verdict));
        Ok(CoverageReport { entries, worst })
    }

    async fn coverage_for_rule(&self, rule: &Rule) -> Result<CoverageEntry, EngineError> {
        let (kind, playlist_ref, demand) = classify_rule(&rule.kind);
        let fail = |detail: String| {
            make_entry(
                rule.id.clone(),
                playlist_ref.clone(),
                kind,
                PoolStats::default(),
                Verdict::Insufficient,
                detail,
                Vec::new(),
            )
        };

        // Broken / unsafe ref → nothing to size.
        let key = match crate::playlist::normalize_ref(&playlist_ref) {
            Ok(k) => k,
            Err(msg) => return Ok(fail(format!("ref invalide : {msg}"))),
        };
        let Some(toml) = crate::store::playlist_toml_by_ref(&self.pool, &key).await? else {
            return Ok(fail("ref cassée : playlist inconnue".into()));
        };
        let playlist = match crate::playlist::Playlist::parse(&toml) {
            Ok(p) => p,
            Err(e) => return Ok(fail(format!("playlist illisible : {e}"))),
        };

        // Size the pool (read-only). Only SQLite is infra and propagates; a
        // config-level selection error (transitive unknown ref, unsupported
        // mode, bad filter) marks THIS entry insufficient, report goes on.
        let inspection = match pool_inspection::inspect_ref(&self.pool, &key).await {
            Ok(i) => i,
            Err(crate::selection::SelectionError::Sqlx(e)) => return Err(EngineError::Sqlx(e)),
            Err(e) => return Ok(fail(format!("pool non résolvable : {e}"))),
        };

        let (verdict, detail, members) = verdict_for(&playlist, &inspection, demand);
        // `playlist_ref` is still borrowed by the `fail` closure above; clone
        // rather than move so there is no borrow/move conflict.
        Ok(make_entry(
            rule.id.clone(),
            playlist_ref.clone(),
            kind,
            inspection.stats,
            verdict,
            detail,
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
    /// and the media is known (its size/mtime are the play-once guard). The
    /// live wiring (Liquidsoap end-of-track) is not in place yet, so today this
    /// is driven by the CLI / tests.
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
            return Err(EngineError::Selection(
                crate::selection::SelectionError::Unsupported(format!(
                    "`{playlist_ref}` is not a queue playlist"
                )),
            ));
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

    pub async fn validate_grid(&self, files: &[(String, String)]) -> Result<(), GridOpError> {
        let rules = Self::parse_all(files).map_err(GridOpError::Invalid)?;
        let known = self.known_playlist_keys().await?;
        let ref_errors = grid_toml::validate_refs(&rules, &known);
        if !ref_errors.is_empty() { return Err(GridOpError::Invalid(ref_errors)); }
        Ok(())
    }

    pub async fn apply_grid(&self, files: &[(String, String)]) -> Result<Vec<String>, GridOpError> {
        let rules = Self::parse_all(files).map_err(GridOpError::Invalid)?;
        let known = self.known_playlist_keys().await?;
        let ref_errors = grid_toml::validate_refs(&rules, &known);
        if !ref_errors.is_empty() { return Err(GridOpError::Invalid(ref_errors)); }
        grid_index::replace_grid(&self.pool, &rules).await.map_err(EngineError::from)?;
        self.sync_grid().await?;
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
        let loaded = grid_index::load_grid(&self.pool).await?;

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
                        let stats = crate::pool_inspection::inspect_ref(&self.pool, &canonical).await?;
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

        Ok(GridPreview { occurrences, indicative })
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
    /// 1. the broadcast **gate** — paused/stopped → `halted`, nothing resolved
    ///    (a draining station with zero listeners stops right here);
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
            });
        }
        if let Some(resolved) = self.next_override(now).await? {
            return Ok(resolved);
        }

        let local = clock::to_local_now(now, &self.tz)?;
        let grid = grid_index::load_grid(&self.pool).await?;
        let state = grid_store::load_playback_state(&self.pool).await?;

        let ranked = resolve_ranked(local, &grid, &state);
        let had_candidates = !ranked.is_empty();

        for decision in ranked {
            let Some(playlist_ref) = decision.playlist_ref.clone() else {
                continue;
            };
            // Bounded re-pick: a chosen file that vanished from disk (between
            // scans) is flipped unavailable in the index and we re-pick from
            // the same source. Capped so a pool of dead entries can't spin.
            const MAX_DEAD_PICKS: u32 = 32;
            let mut produced: Option<crate::selection::Resolved> = None;
            for _ in 0..MAX_DEAD_PICKS {
                match crate::selection::resolve_ref_with_plugins(
                    &self.pool,
                    self.plugins.as_ref(),
                    now.0,
                    &playlist_ref,
                )
                .await
                {
                    // A remote stream: no file on disk to check, no re-pick.
                    Ok(stream @ crate::selection::Resolved::Stream(_)) => {
                        produced = Some(stream);
                        break;
                    }
                    Ok(crate::selection::Resolved::File(media)) if self.media_exists(&media) => {
                        produced = Some(crate::selection::Resolved::File(media));
                        break;
                    }
                    Ok(crate::selection::Resolved::File(missing)) => {
                        tracing::warn!(
                            media = %missing,
                            "resolved media missing on disk; marking unavailable and re-picking"
                        );
                        crate::media_index::mark_unavailable(&self.pool, &missing).await?;
                        continue;
                    }
                    Err(crate::selection::SelectionError::PoolEmpty) => break,
                    // A config error (unknown ref, unsupported order/mode, bad
                    // filter value) is not an empty pool — surface it.
                    Err(e) => return Err(EngineError::Selection(e)),
                }
            }

            if let Some(resolved) = produced {
                // Persist effects only now that this source actually produced.
                self.persist_effects(&decision, now).await?;
                let (media_path, stream) = self.log_start(resolved, now).await?;
                self.emit_resolved(&decision, Some(&media_path), format!("{:?}", decision.origin));
                return Ok(ResolvedDecision {
                    decision,
                    media_path: Some(media_path),
                    stream,
                    halted: None,
                    override_source: None,
                });
            }

            tracing::info!(
                rule = decision.rule_id.as_deref().unwrap_or("-"),
                playlist = %playlist_ref,
                origin = ?decision.origin,
                "grid source produced no usable media; falling through to lower priority"
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
        })
    }

    /// The override layer: air the head of the override queue, if any. A media
    /// is checked on disk (it may be outside the index — arbitrary path
    /// allowed); a playlist is resolved like a grid source (plugins, constraints
    /// included) and holds the air for its `tracks`. An override that can't
    /// air (missing file, empty pool, unknown ref) is DROPPED loudly and the
    /// next one / the grid takes over — never a silent gap, never a retry loop
    /// (each iteration consumes or drops one entry). Grid side effects (AtClock
    /// marks, Every resets) are not touched: the grid did not play.
    async fn next_override(&self, now: Epoch) -> Result<Option<ResolvedDecision>, EngineError> {
        use crate::selection::{Resolved, SelectionError};
        while let Some(entry) = self.control.next_override(now) {
            let produced: Result<Resolved, String> = match &entry.content {
                OverrideContent::Media(path) => {
                    if self.media_exists(path) {
                        Ok(Resolved::File(path.clone()))
                    } else {
                        Err(format!("media `{path}` not found under the media root"))
                    }
                }
                OverrideContent::Playlist(reference) => {
                    match crate::selection::resolve_ref_with_plugins(
                        &self.pool,
                        self.plugins.as_ref(),
                        now.0,
                        reference,
                    )
                    .await
                    {
                        Ok(Resolved::File(media)) if !self.media_exists(&media) => {
                            crate::media_index::mark_unavailable(&self.pool, &media).await?;
                            Err(format!("resolved media `{media}` missing on disk"))
                        }
                        Ok(resolved) => Ok(resolved),
                        Err(SelectionError::Sqlx(e)) => return Err(EngineError::Sqlx(e)),
                        Err(e) => Err(e.to_string()),
                    }
                }
            };
            match produced {
                Err(reason) => {
                    self.control.drop_override(entry.id, &reason);
                    continue;
                }
                Ok(resolved) => {
                    self.control.consume_override(entry.id);
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
                    let (media_path, stream) = self.log_start(resolved, now).await?;
                    self.emit_resolved(&decision, Some(&media_path), "Override".to_string());
                    return Ok(Some(ResolvedDecision {
                        decision,
                        media_path: Some(media_path),
                        stream,
                        halted: None,
                        override_source: Some(entry.source.clone()),
                    }));
                }
            }
        }
        Ok(None)
    }

    /// Log a track START into the station history (family B) so the
    /// anti-repetition constraints see it on the next pull (artist from the
    /// media index, `None` = untagged). A stream has no file identity / artist
    /// → no play history. Returns (media_path, is_stream).
    async fn log_start(
        &self,
        resolved: crate::selection::Resolved,
        now: Epoch,
    ) -> Result<(String, bool), EngineError> {
        Ok(match resolved {
            crate::selection::Resolved::File(media) => {
                let artist = crate::media_index::artist_of(&self.pool, &media).await?;
                crate::broadcast_log::record(&self.pool, &media, artist.as_deref(), now).await?;
                (media, false)
            }
            crate::selection::Resolved::Stream(url) => (url, true),
        })
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
                    end: crate::resolver::WallClock { hour: 10, minute: 0 },
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
                    end: WallClock { hour: 10, minute: 0 },
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
        assert!(eng.enqueue("rot", "x.mp3").await.is_err());
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
    async fn stopped_station_resolves_nothing_and_logs_nothing() {
        use crate::station_control::ControlAction;
        let (_dir, eng) = engine_with_floor().await;
        eng.control().apply(ControlAction::Stop, "cli").unwrap();
        let r = eng.next_media(at(9, 0)).await.unwrap();
        assert_eq!(r.halted, Some(BroadcastState::Stopped));
        assert!(r.media_path.is_none());
        assert!(crate::broadcast_log::tracks_since(&eng.pool, 0).await.unwrap().is_empty());
        // Resume → the grid plays again.
        eng.control().apply(ControlAction::Resume, "cli").unwrap();
        let r = eng.next_media(at(9, 1)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("music/a.mp3"));
        assert!(r.halted.is_none());
    }

    #[tokio::test]
    async fn draining_stops_at_the_boundary_once_listeners_are_zero() {
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
        assert_eq!(r.halted, Some(BroadcastState::Stopped));
        assert_eq!(eng.control().state(), BroadcastState::Stopped);
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
