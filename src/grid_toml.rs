//! Grid TOML grammar: parse a `grid.toml` into `resolver::Rule`s, validate the
//! refs against the known playlists, and serialize rules back to TOML (export).
//!
//! Counterpart of `playlist.rs` for the grid. Same discipline: strict parsing
//! (`deny_unknown_fields` → no-silent-failure), business validation lives here
//! in `stationd` (behind the gRPC contract, so every client hits the same
//! rules), and the module is DB-free / std-only so it can be unit-tested
//! without a database or a wall clock.
//!
//! One `grid.toml` holds `[[rule]]` blocks. A rule is a flat table with a
//! `kind` discriminant gating the rest of the fields (mirror of the
//! `mode`-gated playlist selection). Fields map 1:1 onto `resolver::RuleKind`
//! and onto migration 0006's columns — one model, two serializations.
//!
//! See `Doc/proposition-grammaire-grille-v1.md` for the contract.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::resolver::{
    Cadence, ClockAnchor, Date, Mode, Rule, RuleKind, Validity, WallClock, Weekday,
};

// ---------------------------------------------------------------------------
// Serde document (TOML surface)
// ---------------------------------------------------------------------------

/// The whole `grid.toml`: a schema version plus the `[[rule]]` array. Unlike a
/// playlist (one file each, referenced from the outside), the grid is a single
/// top-level object holding a container of rules — a rule is never referenced
/// by anything, so it needs no file of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridDoc {
    /// Exactly `1` for this contract. A hand-written file without it is a loud
    /// error, never a silently-assumed default.
    pub schema_version: u32,
    #[serde(default, rename = "rule", skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RuleDoc>,
}

/// One `[[rule]]`. Flat, with `kind` selecting which of the trailing fields
/// are allowed. A field belonging to another `kind` is rejected in `to_rule`
/// (no-silent-failure: a stray field is a loud error, never ignored).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDoc {
    /// Stable handle. Keys the durable playback state (family B) — a rename is
    /// an identity change, not a relabel.
    pub id: String,
    #[serde(default = "default_enabled", skip_serializing_if = "is_true")]
    pub enabled: bool,
    pub kind: KindTag,
    pub playlist_ref: String,

    // --- day_part ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,

    // --- at_clock ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeTag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,

    // --- every ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_tracks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_elapsed: Option<String>,

    // --- validity (all kinds) ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_end: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindTag {
    BaseRotation,
    DayPart,
    AtClock,
    Every,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeTag {
    Soft,
    Hard,
}

fn default_enabled() -> bool {
    true
}
#[allow(clippy::trivially_copy_pass_by_ref)] // signature required by serde
fn is_true(b: &bool) -> bool {
    *b
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GridTomlError {
    #[error("invalid grid TOML: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("unsupported grid schema_version {found} (expected 1)")]
    SchemaVersion { found: u32 },

    #[error("rule {id:?}: {message}")]
    Rule { id: String, message: String },

    #[error("grid: {0}")]
    Set(String),
}

// ---------------------------------------------------------------------------
// Parse + validate (TOML text -> resolver rules)
// ---------------------------------------------------------------------------

/// Parse and validate one `grid.toml`'s worth of rules into resolver `Rule`s.
///
/// Strict and fail-fast, in document order: an unknown field, a wrong
/// `schema_version`, a field on the wrong `kind`, or a broken XOR is a loud
/// error carrying the offending rule id. Set-level checks (duplicate ids, more
/// than one floor) run last. Playlist-ref existence is a separate step
/// ([`validate_refs`]) because it needs the whole playlist set, which this
/// DB-free function cannot see.
pub fn parse_grid(toml_str: &str) -> Result<Vec<Rule>, GridTomlError> {
    let doc: GridDoc = toml::from_str(toml_str)?;
    if doc.schema_version != 1 {
        return Err(GridTomlError::SchemaVersion {
            found: doc.schema_version,
        });
    }

    let mut rules = Vec::with_capacity(doc.rules.len());
    for rd in doc.rules {
        let id = rd.id.clone();
        let rule = to_rule(rd).map_err(|message| GridTomlError::Rule {
            id: id.clone(),
            message,
        })?;
        rules.push(rule);
    }

    check_set(&rules)?;
    Ok(rules)
}

/// Set-level checks that need every rule in hand: ids are unique, and there is
/// at most one `base_rotation` (the floor is a singleton; several is a config
/// smell the resolver tolerates but the grammar rejects).
fn check_set(rules: &[Rule]) -> Result<(), GridTomlError> {
    let mut seen: HashSet<&str> = HashSet::new();
    for r in rules {
        if !seen.insert(r.id.as_str()) {
            return Err(GridTomlError::Set(format!("duplicate rule id {:?}", r.id)));
        }
    }
    let bases = rules
        .iter()
        .filter(|r| matches!(r.kind, RuleKind::BaseRotation { .. }))
        .count();
    if bases > 1 {
        return Err(GridTomlError::Set(format!(
            "{bases} base_rotation rules; at most one is allowed (the single floor)"
        )));
    }
    Ok(())
}

/// Convert one parsed `RuleDoc` into a resolver `Rule`, enforcing the
/// per-kind field rules. Returns a human message on rejection (the caller
/// attaches the rule id).
fn to_rule(rd: RuleDoc) -> Result<Rule, String> {
    let id = rd.id.trim().to_string();
    if id.is_empty() {
        return Err("`id` must not be blank".into());
    }
    if rd.playlist_ref.trim().is_empty() {
        return Err("`playlist_ref` must not be blank".into());
    }

    // Validity (shared by every kind).
    let mut days = Vec::new();
    for d in &rd.days {
        let w = parse_weekday(d).ok_or_else(|| format!("invalid weekday {d:?} (mon..sun)"))?;
        if days.contains(&w) {
            return Err(format!("duplicate weekday {d:?}"));
        }
        days.push(w);
    }
    let date_start = rd.date_start.as_deref().map(parse_date).transpose()?;
    let date_end = rd.date_end.as_deref().map(parse_date).transpose()?;
    if let (Some(s), Some(e)) = (date_start, date_end) {
        if e < s {
            return Err("`date_end` must be on or after `date_start`".into());
        }
    }
    let validity = Validity {
        days,
        date_start,
        date_end,
    };

    // Which kind-specific field groups are present.
    let has_dp = rd.start.is_some() || rd.end.is_some();
    let has_ac =
        rd.every_minutes.is_some() || rd.at.is_some() || rd.mode.is_some() || rd.expiry.is_some();
    let has_ev = rd.min_tracks.is_some() || rd.min_elapsed.is_some();

    let playlist_ref = rd.playlist_ref;
    let kind = match rd.kind {
        KindTag::BaseRotation => {
            if has_dp || has_ac || has_ev {
                return Err(
                    "`base_rotation` takes no scheduling fields (only playlist_ref + validity)"
                        .into(),
                );
            }
            RuleKind::BaseRotation { playlist_ref }
        }
        KindTag::DayPart => {
            if has_ac || has_ev {
                return Err("`day_part` only takes `start`/`end` besides validity".into());
            }
            let start = parse_wallclock(
                rd.start.as_deref().ok_or("`day_part` requires `start`")?,
            )?;
            let end =
                parse_wallclock(rd.end.as_deref().ok_or("`day_part` requires `end`")?)?;
            if minutes(end) <= minutes(start) {
                return Err(
                    "`end` must be strictly after `start` (cross-midnight is not supported in v1)"
                        .into(),
                );
            }
            RuleKind::DayPart {
                playlist_ref,
                start,
                end,
            }
        }
        KindTag::AtClock => {
            if has_dp || has_ev {
                return Err(
                    "`at_clock` takes `every_minutes`|`at`, `mode`, `expiry` — not day_part/every fields"
                        .into(),
                );
            }
            let anchor = match (rd.every_minutes, rd.at.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err("`every_minutes` and `at` are mutually exclusive".into())
                }
                (None, None) => {
                    return Err("`at_clock` requires exactly one of `every_minutes` or `at`".into())
                }
                (Some(n), None) => {
                    if !(1..=60).contains(&n) {
                        return Err(format!("`every_minutes` must be between 1 and 60 (got {n})"));
                    }
                    ClockAnchor::EveryMinutes(n)
                }
                (None, Some(at)) => ClockAnchor::At(parse_wallclock(at)?),
            };
            let mode = match rd.mode {
                Some(ModeTag::Hard) => Mode::Hard,
                _ => Mode::Soft,
            };
            let expiry_secs = match rd.expiry.as_deref() {
                Some(s) => Some(parse_duration_secs(s)?),
                None => None,
            };
            RuleKind::AtClock {
                playlist_ref,
                anchor,
                mode,
                expiry_secs,
            }
        }
        KindTag::Every => {
            if has_dp || has_ac {
                return Err(
                    "`every` takes exactly one of `min_tracks`|`min_elapsed` — not day_part/at_clock fields"
                        .into(),
                );
            }
            let cadence = match (rd.min_tracks, rd.min_elapsed.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err("`min_tracks` and `min_elapsed` are mutually exclusive".into())
                }
                (None, None) => {
                    return Err("`every` requires exactly one of `min_tracks` or `min_elapsed`".into())
                }
                (Some(n), None) => {
                    if n < 1 {
                        return Err("`min_tracks` must be at least 1".into());
                    }
                    Cadence::Tracks(n)
                }
                (None, Some(d)) => Cadence::Elapsed(parse_duration_secs(d)?),
            };
            RuleKind::Every {
                playlist_ref,
                cadence,
            }
        }
    };

    Ok(Rule {
        id,
        enabled: rd.enabled,
        validity,
        kind,
    })
}

/// Check that every rule's `playlist_ref` resolves to a known playlist.
/// Pure: `known` is the set of canonical playlist keys (the `rel_path`s of the
/// view), supplied by the caller. Best-effort — collects all problems rather
/// than stopping at the first, so the whole grid can be reported at once.
pub fn validate_refs(rules: &[Rule], known: &HashSet<String>) -> Vec<String> {
    let mut errors = Vec::new();
    for r in rules {
        let raw = playlist_ref_of(r);
        match crate::playlist::normalize_ref(raw) {
            Ok(key) => {
                if !known.contains(&key) {
                    errors.push(format!(
                        "rule {:?}: playlist_ref `{raw}` refers to unknown playlist `{key}`",
                        r.id
                    ));
                }
            }
            Err(msg) => errors.push(format!("rule {:?}: {msg}", r.id)),
        }
    }
    errors
}

fn playlist_ref_of(r: &Rule) -> &str {
    match &r.kind {
        RuleKind::BaseRotation { playlist_ref } => playlist_ref,
        RuleKind::DayPart { playlist_ref, .. } => playlist_ref,
        RuleKind::AtClock { playlist_ref, .. } => playlist_ref,
        RuleKind::Every { playlist_ref, .. } => playlist_ref,
    }
}

// ---------------------------------------------------------------------------
// Serialize (resolver rules -> TOML text), for ExportGrid
// ---------------------------------------------------------------------------

/// Render rules back to a `grid.toml`. Stable: `to_toml` ∘ `parse_grid` is a
/// fixpoint on the field values (comments/formatting are not preserved — the
/// grid is a single generated file, unlike the lossless per-playlist round
/// trip).
pub fn to_toml(rules: &[Rule]) -> Result<String, String> {
    let doc = GridDoc {
        schema_version: 1,
        rules: rules.iter().map(rule_to_doc).collect(),
    };
    toml::to_string_pretty(&doc).map_err(|e| e.to_string())
}

fn rule_to_doc(r: &Rule) -> RuleDoc {
    let mut d = RuleDoc {
        id: r.id.clone(),
        enabled: r.enabled,
        kind: KindTag::BaseRotation, // placeholder, overwritten below
        playlist_ref: String::new(),
        start: None,
        end: None,
        every_minutes: None,
        at: None,
        mode: None,
        expiry: None,
        min_tracks: None,
        min_elapsed: None,
        days: r.validity.days.iter().map(|w| weekday_str(*w).to_string()).collect(),
        date_start: r.validity.date_start.map(date_str),
        date_end: r.validity.date_end.map(date_str),
    };
    match &r.kind {
        RuleKind::BaseRotation { playlist_ref } => {
            d.kind = KindTag::BaseRotation;
            d.playlist_ref = playlist_ref.clone();
        }
        RuleKind::DayPart { playlist_ref, start, end } => {
            d.kind = KindTag::DayPart;
            d.playlist_ref = playlist_ref.clone();
            d.start = Some(wallclock_str(*start));
            d.end = Some(wallclock_str(*end));
        }
        RuleKind::AtClock { playlist_ref, anchor, mode, expiry_secs } => {
            d.kind = KindTag::AtClock;
            d.playlist_ref = playlist_ref.clone();
            match anchor {
                ClockAnchor::EveryMinutes(n) => d.every_minutes = Some(*n),
                ClockAnchor::At(w) => d.at = Some(wallclock_str(*w)),
            }
            d.mode = Some(match mode {
                Mode::Hard => ModeTag::Hard,
                Mode::Soft => ModeTag::Soft,
            });
            d.expiry = expiry_secs.map(fmt_duration);
        }
        RuleKind::Every { playlist_ref, cadence } => {
            d.kind = KindTag::Every;
            d.playlist_ref = playlist_ref.clone();
            match cadence {
                Cadence::Tracks(n) => d.min_tracks = Some(*n),
                Cadence::Elapsed(s) => d.min_elapsed = Some(fmt_duration(*s)),
            }
        }
    }
    d
}

// ---------------------------------------------------------------------------
// Field parsers / formatters (std-only)
// ---------------------------------------------------------------------------

fn minutes(w: WallClock) -> u32 {
    w.hour as u32 * 60 + w.minute as u32
}

fn parse_wallclock(s: &str) -> Result<WallClock, String> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| format!("invalid time {s:?} (want HH:MM)"))?;
    let hour = h
        .parse::<u8>()
        .map_err(|_| format!("invalid hour in {s:?}"))?;
    let minute = m
        .parse::<u8>()
        .map_err(|_| format!("invalid minute in {s:?}"))?;
    if hour > 23 {
        return Err(format!("hour out of range in {s:?} (0..=23)"));
    }
    if minute > 59 {
        return Err(format!("minute out of range in {s:?} (0..=59)"));
    }
    Ok(WallClock { hour, minute })
}

fn wallclock_str(w: WallClock) -> String {
    format!("{:02}:{:02}", w.hour, w.minute)
}

fn parse_date(s: &str) -> Result<Date, String> {
    let bad = || format!("invalid date {s:?} (want YYYY-MM-DD)");
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || !s.bytes().all(|b| b.is_ascii_digit() || b == b'-')
    {
        return Err(bad());
    }
    let year = parts[0].parse::<i32>().map_err(|_| bad())?;
    let month = parts[1].parse::<u8>().map_err(|_| bad())?;
    let day = parts[2].parse::<u8>().map_err(|_| bad())?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(bad());
    }
    Ok(Date { year, month, day })
}

fn date_str(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

/// Duration grammar shared with the playlist contract: `[1-9][0-9]*(s|m|h|d)`,
/// a single unit, no leading zero, no zero duration.
fn parse_duration_secs(s: &str) -> Result<u64, String> {
    let bad = || format!("invalid duration {s:?} (want e.g. 30s, 15m, 2h, 1d)");
    if s.len() < 2 {
        return Err(bad());
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let mult: u64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(bad()),
    };
    let bytes = num.as_bytes();
    if bytes.is_empty() || bytes[0] == b'0' || !num.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = num.parse().map_err(|_| bad())?;
    if n == 0 {
        return Err(bad());
    }
    n.checked_mul(mult)
        .ok_or_else(|| format!("duration {s:?} is too large"))
}

/// Render seconds back as the largest exact unit (3600 → "1h", 120 → "2m").
fn fmt_duration(secs: u64) -> String {
    if secs % 86400 == 0 {
        format!("{}d", secs / 86400)
    } else if secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else if secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn weekday_str(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: &str = r#"
        schema_version = 1

        [[rule]]
        id = "floor"
        kind = "base_rotation"
        playlist_ref = "general"

        [[rule]]
        id = "morning-jazz"
        kind = "day_part"
        playlist_ref = "jazz"
        start = "08:00"
        end = "10:00"
        days = ["mon", "tue", "wed", "thu", "fri"]

        [[rule]]
        id = "news-8h"
        kind = "at_clock"
        playlist_ref = "flash-info"
        at = "08:00"
        mode = "hard"
        expiry = "2m"

        [[rule]]
        id = "station-id"
        kind = "at_clock"
        playlist_ref = "jingles"
        every_minutes = 15

        [[rule]]
        id = "sweeper"
        kind = "every"
        playlist_ref = "sweepers"
        min_tracks = 4
    "#;

    #[test]
    fn parses_the_reference_grid() {
        let rules = parse_grid(GRID).expect("reference grid parses");
        assert_eq!(rules.len(), 5);

        let jazz = rules.iter().find(|r| r.id == "morning-jazz").unwrap();
        assert_eq!(jazz.validity.days.len(), 5);
        match &jazz.kind {
            RuleKind::DayPart { playlist_ref, start, end } => {
                assert_eq!(playlist_ref, "jazz");
                assert_eq!((start.hour, end.hour), (8, 10));
            }
            _ => panic!("expected DayPart"),
        }

        let news = rules.iter().find(|r| r.id == "news-8h").unwrap();
        match &news.kind {
            RuleKind::AtClock { anchor, mode, expiry_secs, .. } => {
                assert!(matches!(anchor, ClockAnchor::At(WallClock { hour: 8, minute: 0 })));
                assert_eq!(*mode, Mode::Hard);
                assert_eq!(*expiry_secs, Some(120));
            }
            _ => panic!("expected AtClock"),
        }
    }

    #[test]
    fn rejects_missing_schema_version() {
        let toml = r#"
            [[rule]]
            id = "x"
            kind = "base_rotation"
            playlist_ref = "g"
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Parse(_))));
    }

    #[test]
    fn rejects_wrong_schema_version() {
        let toml = r#"
            schema_version = 2
            [[rule]]
            id = "x"
            kind = "base_rotation"
            playlist_ref = "g"
        "#;
        assert!(matches!(
            parse_grid(toml),
            Err(GridTomlError::SchemaVersion { found: 2 })
        ));
    }

    #[test]
    fn rejects_unknown_field() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "x"
            kind = "base_rotation"
            playlist_ref = "g"
            oops = true
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Parse(_))));
    }

    #[test]
    fn rejects_field_from_another_kind() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "x"
            kind = "base_rotation"
            playlist_ref = "g"
            start = "08:00"
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Rule { .. })));
    }

    #[test]
    fn rejects_at_clock_with_both_anchors() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "x"
            kind = "at_clock"
            playlist_ref = "g"
            every_minutes = 15
            at = "08:00"
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Rule { .. })));
    }

    #[test]
    fn rejects_at_clock_with_no_anchor() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "x"
            kind = "at_clock"
            playlist_ref = "g"
            mode = "soft"
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Rule { .. })));
    }

    #[test]
    fn rejects_every_with_both_cadences() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "x"
            kind = "every"
            playlist_ref = "g"
            min_tracks = 4
            min_elapsed = "30m"
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Rule { .. })));
    }

    #[test]
    fn rejects_cross_midnight_day_part() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "night"
            kind = "day_part"
            playlist_ref = "g"
            start = "22:00"
            end = "06:00"
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Rule { .. })));
    }

    #[test]
    fn rejects_every_minutes_out_of_range() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "x"
            kind = "at_clock"
            playlist_ref = "g"
            every_minutes = 61
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Rule { .. })));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "dup"
            kind = "base_rotation"
            playlist_ref = "a"
            [[rule]]
            id = "dup"
            kind = "every"
            playlist_ref = "b"
            min_tracks = 2
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Set(_))));
    }

    #[test]
    fn rejects_two_base_rotations() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "a"
            kind = "base_rotation"
            playlist_ref = "x"
            [[rule]]
            id = "b"
            kind = "base_rotation"
            playlist_ref = "y"
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Set(_))));
    }

    #[test]
    fn rejects_bad_duration() {
        for d in ["0m", "m", "15", "1x", "01m", "15 m"] {
            let toml = format!(
                r#"
                schema_version = 1
                [[rule]]
                id = "x"
                kind = "every"
                playlist_ref = "g"
                min_elapsed = "{d}"
            "#
            );
            assert!(
                matches!(parse_grid(&toml), Err(GridTomlError::Rule { .. })),
                "duration {d:?} should be rejected"
            );
        }
    }

    #[test]
    fn validate_refs_flags_unknown_and_passes_known() {
        let rules = parse_grid(GRID).unwrap();
        let empty = HashSet::new();
        assert_eq!(validate_refs(&rules, &empty).len(), 5);
        let known: HashSet<String> = ["general", "jazz", "flash-info", "jingles", "sweepers"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(validate_refs(&rules, &known).is_empty());
    }

    #[test]
    fn export_roundtrips_stably() {
        let rules1 = parse_grid(GRID).unwrap();
        let t2 = to_toml(&rules1).expect("serializes");
        let rules2 = parse_grid(&t2).expect("exported grid re-parses");
        assert_eq!(rules2.len(), rules1.len());
        let t3 = to_toml(&rules2).expect("serializes again");
        assert_eq!(t2, t3, "export should be stable");
    }

    #[test]
    fn export_omits_default_enabled_and_keeps_false() {
        let mut rules = parse_grid(GRID).unwrap();
        let t = to_toml(&rules).unwrap();
        assert!(!t.contains("enabled = true"));
        rules[0].enabled = false;
        let t = to_toml(&rules).unwrap();
        assert!(t.contains("enabled = false"));
    }
}