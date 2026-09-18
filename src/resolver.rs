//! Grid resolver runtime: `resolve_next(now, grid, state) -> GridDecision`.
//!
//! This is the core of what distinguishes the rewrite from AzuraCast: how the
//! station decides *which source* to pull from at instant `now`. It is a pure
//! function of (a decomposed `now` + the grid rules + persisted playback
//! state). No wall-clock, no I/O, no Liquidsoap — so it is fully testable and
//! `stationctl schedule preview --at …` can run it on any synthetic instant.
//!
//! TWO-STAGE RESOLUTION. This module answers only the *grid* question: which
//! playlist `ref` is active and why. Turning that `ref` into a concrete media
//! file is the downstream *selection* step (the playlist engine). The grid
//! decides the source; selection decides the track.
//!
//! TIME BOUNDARY. Instants live as epoch UTC internally (`Epoch`), never a raw
//! `i64` (per the time doc). DayPart/AtClock compare against *civil local*
//! time, so the caller must convert `now` (epoch) into `LocalNow` using the
//! station timezone. That conversion — the only place DST ambiguity is real —
//! is where the still-undecided time crate (`jiff` vs `chrono` + `chrono-tz`)
//! will live. This module stays crate-free on purpose.
//!
//! COLLISION ORDER (documented, fixed):
//!   AtClock hard > AtClock soft > Every > base (DayPart → BaseRotation) > fallback

use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Time inputs (minimal, std-only; map onto the chosen time crate at the edge)
// ---------------------------------------------------------------------------

/// An instant on the monotone epoch line, in seconds UTC. Wrapped so two
/// timestamps can't be silently added and s/ms can't be mixed up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Epoch(pub i64);

/// A civil calendar date in the station timezone. Used only for validity
/// windows (date_start/date_end) and the AtClock occurrence token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

/// A civil wall-clock time-of-day in the station timezone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallClock {
    pub hour: u8,
    pub minute: u8,
}

impl WallClock {
    /// Minutes since local midnight — the comparison currency for windows.
    fn minutes(self) -> u32 {
        self.hour as u32 * 60 + self.minute as u32
    }
}

/// `now`, already converted to the station's civil local time by the caller.
/// Carrying both the epoch and the decomposed civil fields keeps this module
/// free of any timezone database: the DST-aware conversion happened upstream.
#[derive(Debug, Clone, Copy)]
pub struct LocalNow {
    pub epoch: Epoch,
    pub date: Date,
    pub weekday: Weekday,
    pub wall: WallClock,
}

// ---------------------------------------------------------------------------
// Rules (the domain mirror of schedule_v1.proto; hand-written like playlist.rs)
// ---------------------------------------------------------------------------

/// One grid rule. Distinguished by its anchor point (`kind`), gated by a
/// validity scope. A rule references a playlist by its `ref` (ref_effective).
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub enabled: bool,
    pub validity: Validity,
    pub kind: RuleKind,
}

/// When a rule is allowed to apply. "Fixed weekly" and "dated override" are
/// the same mechanism, differing only here: recurrent (days) vs bounded
/// (date_start/date_end). Empty `days` = every day; absent dates = unbounded.
#[derive(Debug, Clone, Default)]
pub struct Validity {
    pub days: Vec<Weekday>,
    pub date_start: Option<Date>,
    pub date_end: Option<Date>,
}

#[derive(Debug, Clone)]
pub enum RuleKind {
    /// The floor: no anchor, always resolvable, lowest priority.
    BaseRotation { playlist_ref: String },
    /// A window that *selects which base is active*. `end` is the bound of a
    /// predicate on `now` (start <= now < end), evaluated at a track boundary
    /// — never a content cut. The last track overruns; the change happens at
    /// the next track (soft boundary).
    DayPart {
        playlist_ref: String,
        start: WallClock,
        end: WallClock,
    },
    /// An absolute clock rendez-vous that *punctuates* the base.
    AtClock {
        playlist_ref: String,
        anchor: ClockAnchor,
        mode: Mode,
        /// Tolerance for a missed rendez-vous (restart at XX:04). Past this
        /// the occurrence is stale and skipped. Per-rule, not global: a stale
        /// news id → bin it; a station jingle → still fine. `None` = never
        /// stale.
        expiry_secs: Option<u64>,
    },
    /// A sliding cooldown that *punctuates* the base. Always soft. Its state
    /// is persisted (family B) or a restart breaks the cadence.
    Every {
        playlist_ref: String,
        cadence: Cadence,
    },
}

/// Where an AtClock's rendez-vous falls within the hour.
#[derive(Debug, Clone, Copy)]
pub enum ClockAnchor {
    /// Marks at minutes 0, N, 2N… < 60 (N divides the hour cleanly, 1..=60).
    /// e.g. 15 → :00 :15 :30 :45. Reuses the `alignment = "hour"` semantics.
    EveryMinutes(u32),
    /// A single fixed time-of-day (e.g. legal top-of-hour news at 08:00).
    At(WallClock),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Waits for the next track boundary.
    Soft,
    /// Preempts (duck/fade) to hit the target instant exactly. NB: the actual
    /// mid-track preemption is driven by a wall timer outside this function;
    /// here `Hard` only affects *priority* when several rules are due at the
    /// same boundary.
    Hard,
}

/// A sliding cadence. Exactly one anchor — the two are never fused (the
/// AzuraCast trap of a single "interval" field read two ways).
#[derive(Debug, Clone, Copy)]
pub enum Cadence {
    /// At least this many seconds since the last play.
    Elapsed(u64),
    /// At least this many station tracks since the last play.
    Tracks(u32),
}

// ---------------------------------------------------------------------------
// Persisted playback state (family B). Read here; the caller persists deltas.
// ---------------------------------------------------------------------------

/// Runtime state that must survive a restart. Keyed by rule id.
#[derive(Debug, Default)]
pub struct PlaybackState {
    /// Per-`Every` cooldown state.
    pub every: HashMap<String, EveryState>,
    /// AtClock occurrences already consumed, by their occurrence token, so a
    /// mark is never played twice (during its own activation, or after a
    /// restart within the same minute).
    pub at_clock_taken: HashSet<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct EveryState {
    pub last_played: Option<Epoch>,
    pub tracks_since: u32,
}

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    AtClockHard,
    AtClockSoft,
    Every,
    DayPart,
    BaseRotation,
    /// No rule produced a source — the Liquidsoap safety fallback fills in.
    /// Never a silent gap.
    Fallback,
}

/// The grid's answer at a track boundary: which source, why, and (for an
/// AtClock) the occurrence token the caller must persist as taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridDecision {
    pub origin: Origin,
    pub rule_id: Option<String>,
    pub playlist_ref: Option<String>,
    /// Present only for an AtClock win: persist it into `at_clock_taken` so
    /// the same rendez-vous does not fire again.
    pub mark_taken: Option<String>,
}

impl GridDecision {
    fn fallback() -> Self {
        Self {
            origin: Origin::Fallback,
            rule_id: None,
            playlist_ref: None,
            mark_taken: None,
        }
    }
}

/// The whole grid.
#[derive(Debug, Clone, Default)]
pub struct Grid {
    pub rules: Vec<Rule>,
}

// ---------------------------------------------------------------------------
// resolve_next
// ---------------------------------------------------------------------------

/// Rank every applicable source at `now`, highest priority first, for the grid
/// fallthrough: the engine tries them in order and keeps the first that yields
/// a concrete media, so an empty pool at one level falls through to the next
/// (down to the BaseRotation floor) instead of a silent gap. Pure.
///
/// Order = the fixed collision order, flattened:
///   AtClock (hard, then soft, ties by id) > Every (by id) > DayPart (narrowest
///   first) > BaseRotation. An empty result means no rule covers `now`.
pub fn resolve_ranked(now: LocalNow, grid: &Grid, state: &PlaybackState) -> Vec<GridDecision> {
    let mut out = Vec::new();

    // 1. AtClock due now — hard before soft, then id.
    let mut due: Vec<(&Rule, &str, Mode, String)> = Vec::new();
    for rule in grid.rules.iter().filter(|r| r.enabled) {
        if !rule.validity.applies(now) {
            continue;
        }
        if let RuleKind::AtClock {
            playlist_ref,
            anchor,
            mode,
            expiry_secs,
        } = &rule.kind
        {
            if let Some(token) = at_clock_due(&rule.id, *anchor, *expiry_secs, now) {
                if !state.at_clock_taken.contains(&token) {
                    due.push((rule, playlist_ref, *mode, token));
                }
            }
        }
    }
    due.sort_by(|a, b| {
        let rank = |m: Mode| if m == Mode::Hard { 0 } else { 1 };
        rank(a.2).cmp(&rank(b.2)).then_with(|| a.0.id.cmp(&b.0.id))
    });
    for (rule, playlist_ref, mode, token) in due {
        out.push(GridDecision {
            origin: if mode == Mode::Hard {
                Origin::AtClockHard
            } else {
                Origin::AtClockSoft
            },
            rule_id: Some(rule.id.clone()),
            playlist_ref: Some(playlist_ref.to_string()),
            mark_taken: Some(token),
        });
    }

    // 2. Every satisfied (sliding cooldown), by id.
    let mut every: Vec<&Rule> = grid
        .rules
        .iter()
        .filter(|r| r.enabled && r.validity.applies(now) && matches!(r.kind, RuleKind::Every { .. }))
        .collect();
    every.sort_by(|a, b| a.id.cmp(&b.id));
    for rule in every {
        if let RuleKind::Every { playlist_ref, cadence } = &rule.kind {
            if every_due(&rule.id, *cadence, now, state) {
                out.push(GridDecision {
                    origin: Origin::Every,
                    rule_id: Some(rule.id.clone()),
                    playlist_ref: Some(playlist_ref.clone()),
                    mark_taken: None,
                });
            }
        }
    }

    // 3. Base: covering DayParts (narrowest first), then the BaseRotation.
    let mut dayparts: Vec<(&Rule, &str, u32)> = Vec::new();
    let mut base_rotation: Option<(&Rule, &str)> = None;
    for rule in grid.rules.iter().filter(|r| r.enabled) {
        if !rule.validity.applies(now) {
            continue;
        }
        match &rule.kind {
            RuleKind::DayPart { playlist_ref, start, end } => {
                if let Some(span) = window_covers(*start, *end, now.wall) {
                    dayparts.push((rule, playlist_ref, span));
                }
            }
            RuleKind::BaseRotation { playlist_ref } => match base_rotation {
                Some((cur, _)) if cur.id <= rule.id => {}
                _ => base_rotation = Some((rule, playlist_ref)),
            },
            _ => {}
        }
    }
    dayparts.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.id.cmp(&b.0.id)));
    for (rule, playlist_ref, _) in dayparts {
        out.push(GridDecision {
            origin: Origin::DayPart,
            rule_id: Some(rule.id.clone()),
            playlist_ref: Some(playlist_ref.to_string()),
            mark_taken: None,
        });
    }
    if let Some((rule, playlist_ref)) = base_rotation {
        out.push(GridDecision {
            origin: Origin::BaseRotation,
            rule_id: Some(rule.id.clone()),
            playlist_ref: Some(playlist_ref.to_string()),
            mark_taken: None,
        });
    }

    out
}

/// Resolve the single winning source at a track boundary — the top of
/// [`resolve_ranked`], or the fallback if nothing applies. Pure.
pub fn resolve_next(now: LocalNow, grid: &Grid, state: &PlaybackState) -> GridDecision {
    resolve_ranked(now, grid, state)
        .into_iter()
        .next()
        .unwrap_or_else(GridDecision::fallback)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

impl Validity {
    /// Does this rule apply on `now`'s civil day?
    fn applies(&self, now: LocalNow) -> bool {
        if !self.days.is_empty() && !self.days.contains(&now.weekday) {
            return false;
        }
        if let Some(start) = self.date_start {
            if now.date < start {
                return false;
            }
        }
        if let Some(end) = self.date_end {
            if now.date > end {
                return false;
            }
        }
        true
    }
}

/// If `now` falls in `[start, end)` (local minutes), return the window width
/// in minutes (used to prefer the narrowest overlapping DayPart). `None` if
/// not covered. Cross-midnight windows (end <= start) are left as a TODO for
/// when the grid needs them — flagged rather than silently mishandled.
fn window_covers(start: WallClock, end: WallClock, now: WallClock) -> Option<u32> {
    let (s, e, n) = (start.minutes(), end.minutes(), now.minutes());
    if e <= s {
        // TODO: cross-midnight window not modelled yet (would need day carry).
        return None;
    }
    if s <= n && n < e {
        Some(e - s)
    } else {
        None
    }
}

/// Build the occurrence token identifying a specific AtClock rendez-vous, so
/// it is consumed at most once: rule id + local date + the repère HH:MM.
fn occurrence_token(rule_id: &str, date: Date, mark: WallClock) -> String {
    format!(
        "{rule_id}@{:04}-{:02}-{:02}T{:02}:{:02}",
        date.year, date.month, date.day, mark.hour, mark.minute
    )
}

/// Is an AtClock due at `now`, within tolerance? Returns the occurrence token
/// of the repère it would satisfy, or `None`. Minute granularity (boundary
/// evaluation): the most recent repère at or before `now` is considered.
fn at_clock_due(
    rule_id: &str,
    anchor: ClockAnchor,
    expiry_secs: Option<u64>,
    now: LocalNow,
) -> Option<String> {
    let now_min = now.wall.minutes();
    let mark_min = match anchor {
        ClockAnchor::EveryMinutes(n) if n >= 1 && n <= 60 => (now.wall.minute as u32 / n) * n,
        ClockAnchor::EveryMinutes(_) => return None, // invalid N (validated upstream)
        ClockAnchor::At(at) => {
            // Only within the same hour as the fixed mark.
            if at.hour as u32 != now.wall.hour as u32 {
                return None;
            }
            at.minute as u32
        }
    };
    // For EveryMinutes the mark is within the current hour by construction.
    let mark_abs = now.wall.hour as u32 * 60 + mark_min;
    if now_min < mark_abs {
        return None; // repère not reached yet this hour
    }
    let delay_secs = (now_min - mark_abs) as u64 * 60;
    if let Some(expiry) = expiry_secs {
        if delay_secs > expiry {
            return None; // stale: past its tolerance window
        }
    }
    let mark = WallClock {
        hour: now.wall.hour,
        minute: mark_min as u8,
    };
    Some(occurrence_token(rule_id, now.date, mark))
}

/// Is an `Every` cooldown satisfied at `now`?
fn every_due(rule_id: &str, cadence: Cadence, now: LocalNow, state: &PlaybackState) -> bool {
    let s = state.every.get(rule_id).copied().unwrap_or_default();
    match cadence {
        Cadence::Elapsed(secs) => match s.last_played {
            None => true, // never played → due immediately
            Some(last) => (now.epoch.0 - last.0) >= secs as i64,
        },
        Cadence::Tracks(n) => s.tracks_since >= n,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn now_at(hour: u8, minute: u8) -> LocalNow {
        LocalNow {
            epoch: Epoch(hour as i64 * 3600 + minute as i64 * 60),
            date: Date { year: 2026, month: 3, day: 15 },
            weekday: Weekday::Sun,
            wall: WallClock { hour, minute },
        }
    }

    fn base(id: &str, r: &str) -> Rule {
        Rule {
            id: id.into(),
            enabled: true,
            validity: Validity::default(),
            kind: RuleKind::BaseRotation { playlist_ref: r.into() },
        }
    }
    fn daypart(id: &str, r: &str, s: (u8, u8), e: (u8, u8)) -> Rule {
        Rule {
            id: id.into(),
            enabled: true,
            validity: Validity::default(),
            kind: RuleKind::DayPart {
                playlist_ref: r.into(),
                start: WallClock { hour: s.0, minute: s.1 },
                end: WallClock { hour: e.0, minute: e.1 },
            },
        }
    }
    fn at_clock(id: &str, r: &str, n: u32, mode: Mode, expiry: Option<u64>) -> Rule {
        Rule {
            id: id.into(),
            enabled: true,
            validity: Validity::default(),
            kind: RuleKind::AtClock {
                playlist_ref: r.into(),
                anchor: ClockAnchor::EveryMinutes(n),
                mode,
                expiry_secs: expiry,
            },
        }
    }
    fn every_tracks(id: &str, r: &str, n: u32) -> Rule {
        Rule {
            id: id.into(),
            enabled: true,
            validity: Validity::default(),
            kind: RuleKind::Every { playlist_ref: r.into(), cadence: Cadence::Tracks(n) },
        }
    }

    #[test]
    fn empty_grid_falls_back() {
        let d = resolve_next(now_at(9, 0), &Grid::default(), &PlaybackState::default());
        assert_eq!(d.origin, Origin::Fallback);
        assert!(d.playlist_ref.is_none());
    }

    #[test]
    fn base_rotation_is_the_floor() {
        let grid = Grid { rules: vec![base("floor", "general")] };
        let d = resolve_next(now_at(3, 0), &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::BaseRotation);
        assert_eq!(d.playlist_ref.as_deref(), Some("general"));
    }

    #[test]
    fn daypart_selects_base_inside_window_then_falls_through() {
        let grid = Grid {
            rules: vec![base("floor", "general"), daypart("morning", "jazz", (8, 0), (10, 0))],
        };
        // Inside 08:00–10:00 → jazz.
        let d = resolve_next(now_at(9, 0), &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::DayPart);
        assert_eq!(d.playlist_ref.as_deref(), Some("jazz"));
        // At 10:00 exactly → window is [8,10), so back to the floor.
        let d = resolve_next(now_at(10, 0), &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::BaseRotation);
        assert_eq!(d.playlist_ref.as_deref(), Some("general"));
    }

    #[test]
    fn narrowest_overlapping_daypart_wins() {
        let grid = Grid {
            rules: vec![
                daypart("wide", "a", (8, 0), (12, 0)),
                daypart("narrow", "b", (9, 0), (10, 0)),
            ],
        };
        let d = resolve_next(now_at(9, 30), &grid, &PlaybackState::default());
        assert_eq!(d.playlist_ref.as_deref(), Some("b"), "narrower window is more specific");
    }

    #[test]
    fn atclock_punctuates_over_base() {
        let grid = Grid {
            rules: vec![base("floor", "general"), at_clock("top", "jingle", 15, Mode::Soft, None)],
        };
        // 09:15 is a :15 repère → jingle wins over the base.
        let d = resolve_next(now_at(9, 15), &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::AtClockSoft);
        assert_eq!(d.playlist_ref.as_deref(), Some("jingle"));
        assert!(d.mark_taken.is_some(), "the occurrence must be reported for persistence");
    }

    #[test]
    fn soft_atclock_catches_up_to_next_boundary_then_yields() {
        // Soft = "at the next track boundary after the mark". The :00 mark
        // passed while a track was still playing; the boundary lands at 09:07,
        // so the still-pending :00 mark fires here (catch-up) — NOT only when
        // `now` is exactly on a mark minute.
        let grid = Grid {
            rules: vec![base("floor", "general"), at_clock("top", "jingle", 15, Mode::Soft, None)],
        };
        let now = now_at(9, 7);
        let d = resolve_next(now, &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::AtClockSoft, "the pending :00 mark fires at this boundary");
        // Once consumed, a later boundary still inside the same :00–:15 slot
        // falls to the base: overdue marks coalesce, no burst catch-up.
        let mut state = PlaybackState::default();
        state.at_clock_taken.insert(d.mark_taken.unwrap());
        let later = resolve_next(now_at(9, 10), &grid, &state);
        assert_eq!(later.origin, Origin::BaseRotation, "same mark already taken → base");
    }

    #[test]
    fn hard_outranks_soft_at_same_boundary() {
        let grid = Grid {
            rules: vec![
                at_clock("soft", "jingle", 15, Mode::Soft, None),
                at_clock("hard", "news", 15, Mode::Hard, None),
            ],
        };
        let d = resolve_next(now_at(9, 15), &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::AtClockHard);
        assert_eq!(d.playlist_ref.as_deref(), Some("news"));
    }

    #[test]
    fn atclock_expiry_makes_a_late_mark_stale() {
        // A :00 mark, tolerance 120s. At 09:04 we are 4 min late → stale.
        let grid = Grid {
            rules: vec![base("floor", "general"), at_clock("news", "flash", 60, Mode::Hard, Some(120))],
        };
        let d = resolve_next(now_at(9, 4), &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::BaseRotation, "4 min late exceeds the 120s tolerance");
    }

    #[test]
    fn atclock_already_taken_is_not_refired() {
        let grid = Grid {
            rules: vec![base("floor", "general"), at_clock("top", "jingle", 15, Mode::Soft, None)],
        };
        let now = now_at(9, 15);
        let first = resolve_next(now, &grid, &PlaybackState::default());
        // Persist the mark, then re-evaluate the same boundary.
        let mut state = PlaybackState::default();
        state.at_clock_taken.insert(first.mark_taken.unwrap());
        let second = resolve_next(now, &grid, &state);
        assert_eq!(second.origin, Origin::BaseRotation, "consumed mark must not refire");
    }

    #[test]
    fn every_due_by_track_count_then_yields_to_base() {
        let grid = Grid {
            rules: vec![base("floor", "general"), every_tracks("jingle", "id", 4)],
        };
        // 3 tracks since → not yet due → base.
        let mut state = PlaybackState::default();
        state.every.insert("jingle".into(), EveryState { last_played: None, tracks_since: 3 });
        let d = resolve_next(now_at(9, 7), &grid, &state);
        assert_eq!(d.origin, Origin::BaseRotation);
        // 4 tracks since → due → Every punctuates.
        state.every.insert("jingle".into(), EveryState { last_played: None, tracks_since: 4 });
        let d = resolve_next(now_at(9, 7), &grid, &state);
        assert_eq!(d.origin, Origin::Every);
        assert_eq!(d.playlist_ref.as_deref(), Some("id"));
    }

    #[test]
    fn every_outranked_by_atclock() {
        let grid = Grid {
            rules: vec![
                base("floor", "general"),
                every_tracks("cool", "a", 1), // due (>=1)
                at_clock("top", "b", 15, Mode::Soft, None),
            ],
        };
        let mut state = PlaybackState::default();
        state.every.insert("cool".into(), EveryState { last_played: None, tracks_since: 5 });
        let d = resolve_next(now_at(9, 15), &grid, &state);
        assert_eq!(d.origin, Origin::AtClockSoft, "AtClock beats a due Every");
    }

    #[test]
    fn validity_days_gate_a_rule() {
        let mut r = daypart("weekday-jazz", "jazz", (8, 0), (10, 0));
        r.validity.days = vec![Weekday::Mon, Weekday::Tue];
        let grid = Grid { rules: vec![base("floor", "general"), r] };
        // now_at() is a Sunday → the DayPart does not apply → base.
        let d = resolve_next(now_at(9, 0), &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::BaseRotation);
    }

    #[test]
    fn disabled_rule_is_ignored() {
        let mut r = daypart("morning", "jazz", (8, 0), (10, 0));
        r.enabled = false;
        let grid = Grid { rules: vec![base("floor", "general"), r] };
        let d = resolve_next(now_at(9, 0), &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::BaseRotation);
    }

    #[test]
    fn resolve_ranked_orders_by_priority() {
        let grid = Grid {
            rules: vec![
                base("floor", "general"),
                daypart("morning", "jazz", (8, 0), (10, 0)),
                at_clock("top", "jingle", 15, Mode::Soft, None),
            ],
        };
        // At a :15 mark inside the morning window: AtClock, then DayPart, then
        // the floor — exactly the fallthrough order the engine will try.
        let ranked = resolve_ranked(now_at(9, 15), &grid, &PlaybackState::default());
        let refs: Vec<_> = ranked.iter().map(|d| d.playlist_ref.as_deref().unwrap()).collect();
        assert_eq!(refs, vec!["jingle", "jazz", "general"]);
    }
}
