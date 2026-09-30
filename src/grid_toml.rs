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
    /// Every kind but `live` (which selects no playlist).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub playlist_ref: String,

    // --- live ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dj: Option<String>,

    // --- day_part (and live: `start` only) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,

    // --- at_clock ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_minutes: Option<u32>,
    /// One mark per hour at this minute (0..59): the top of the hour, offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minute: Option<u32>,
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
    Live,
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
/// Strict: an unknown field, a wrong `schema_version`, a field on the wrong
/// `kind`, or a broken XOR is a loud error carrying the offending rule id —
/// the first problem found ([`diagnose`] gives all of them, each tied to its
/// field). Set-level checks (duplicate ids, more than one floor) run last.
/// Playlist-ref existence is a separate step ([`validate_refs`]) because it
/// needs the whole playlist set, which this DB-free function cannot see.
pub fn parse_grid(toml_str: &str) -> Result<Vec<Rule>, GridTomlError> {
    let doc: GridDoc = toml::from_str(toml_str)?;
    if doc.schema_version != 1 {
        return Err(GridTomlError::SchemaVersion {
            found: doc.schema_version,
        });
    }

    let mut rules = Vec::with_capacity(doc.rules.len());
    for (i, rd) in doc.rules.into_iter().enumerate() {
        let id = rd.id.clone();
        let rule = to_rule(rd, i + 1).map_err(|diags| GridTomlError::Rule {
            id: id.clone(),
            message: diags.into_iter().next().map(|d| d.message).unwrap_or_default(),
        })?;
        rules.push(rule);
    }

    let positions: Vec<usize> = (1..=rules.len()).collect();
    if let Some(d) = check_set(&rules, &positions).into_iter().next() {
        return Err(GridTomlError::Set(d.message));
    }
    Ok(rules)
}

// ---------------------------------------------------------------------------
// Diagnostics: every problem, tied to its field
// ---------------------------------------------------------------------------

/// What is wrong, as an opcode (clients translate it; `message` is the
/// English sentence of `stationctl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridCode {
    /// Unreadable TOML.
    Syntax,
    UnknownField,
    MissingField,
    /// A value of the wrong type or shape.
    BadValue,
    /// A field that belongs to another `kind`.
    NotAllowed,
    /// Fields that exclude each other.
    Conflict,
    BadDuration,
    /// Not `HH:MM`.
    BadTime,
    /// Not `YYYY-MM-DD`.
    BadDate,
    BadWeekday,
    SchemaVersion,
    DuplicateId,
    /// More than one `base_rotation`.
    SeveralFloors,
    /// `start` = `end`.
    ZeroWindow,
    /// `date_end` before `date_start`.
    DatesReversed,
    /// A number out of its range (`every_minutes`, `min_tracks`).
    OutOfRange,
    /// A `playlist_ref` naming no playlist.
    UnknownRef,
    /// A `playlist_ref` that is not a valid ref.
    BadRef,
    /// A `dj` absent from the DJ file.
    UnknownDj,
    /// A `live` rule without `[live]` in stationd.toml.
    NoLive,
    /// The DJ file could not be read.
    DjFileUnreadable,
    /// The grid file itself could not be read.
    FileUnreadable,
}

/// One problem of a grid file. `field` is a path in the grammar:
/// `rule[3].start` (rules numbered from 1, in file order), `schema_version`;
/// empty = the file as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridDiag {
    pub code: GridCode,
    pub field: String,
    /// The rule id, when the problem is inside a rule.
    pub rule_id: String,
    pub rejected: String,
    pub expected: String,
    pub message: String,
}

impl GridDiag {
    fn new(code: GridCode, field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            field: field.into(),
            rule_id: String::new(),
            rejected: String::new(),
            expected: String::new(),
            message: message.into(),
        }
    }
    fn rejected(mut self, v: impl Into<String>) -> Self {
        self.rejected = v.into();
        self
    }
    fn expected(mut self, v: impl Into<String>) -> Self {
        self.expected = v.into();
        self
    }
}

/// Every problem of one grid file, each tied to its field, and the rules when
/// it has none. The TOML and the grammar first (a syntax error or a
/// mistyped field stops there: serde gives one), then each rule on its own,
/// then the set (ids, floor). Refs and DJs need the station: [`diagnose_refs`],
/// [`diagnose_djs`].
pub fn diagnose(text: &str) -> (Option<Vec<Rule>>, Vec<GridDiag>) {
    let (items, diags) = diagnose_partial(text);
    if diags.is_empty() { (Some(items.into_iter().map(|(_, r)| r).collect()), diags) } else { (None, diags) }
}

/// [`diagnose`], keeping the rules that are fine on their own with their
/// number in the file — so that their refs can be judged too, and every
/// problem of the file reported at once.
pub fn diagnose_partial(text: &str) -> (Vec<(usize, Rule)>, Vec<GridDiag>) {
    let doc = match toml_edit::ImDocument::parse(text) {
        Ok(d) => d,
        Err(e) => {
            let at = e
                .span()
                .map(|s| crate::playlist::line_col(text, s.start))
                .map(|(l, c)| format!(" (line {l}, column {c})"))
                .unwrap_or_default();
            let msg = format!("invalid TOML{at}: {}", e.message().trim());
            return (Vec::new(), vec![GridDiag::new(GridCode::Syntax, "", msg)]);
        }
    };
    let parsed: GridDoc = match toml::from_str(text) {
        Ok(d) => d,
        Err(e) => {
            use crate::playlist::DiagCode as P;
            let msg = e.message().trim().to_string();
            let (code, name, expected) = crate::playlist::classify_serde_error(&msg);
            let field = match (code, name.as_deref(), e.span()) {
                (P::MissingField, Some(n), Some(s)) if s.is_empty() => n.to_string(),
                (P::MissingField, Some(n), Some(s)) => {
                    crate::playlist::join_path(&crate::playlist::path_at(&doc, s.start, s.end), n)
                }
                (P::MissingField, Some(n), None) => n.to_string(),
                (_, _, Some(s)) => crate::playlist::path_at(&doc, s.start, s.start + 1),
                _ => String::new(),
            };
            let code = match code {
                P::UnknownField => GridCode::UnknownField,
                P::MissingField => GridCode::MissingField,
                _ => GridCode::BadValue,
            };
            let mut d = GridDiag::new(code, field, msg);
            if let (GridCode::UnknownField | GridCode::BadValue, Some(n)) = (code, name) {
                d = d.rejected(n);
            }
            if let Some(x) = expected {
                d = d.expected(x);
            }
            return (Vec::new(), vec![d]);
        }
    };
    if parsed.schema_version != 1 {
        let d = GridDiag::new(
            GridCode::SchemaVersion,
            "schema_version",
            format!("unsupported grid schema_version {} (expected 1)", parsed.schema_version),
        )
        .rejected(parsed.schema_version.to_string())
        .expected("1");
        return (Vec::new(), vec![d]);
    }
    let mut diags = Vec::new();
    let mut rules = Vec::new();
    let mut positions = Vec::new();
    for (i, rd) in parsed.rules.into_iter().enumerate() {
        match to_rule(rd, i + 1) {
            Ok(r) => {
                rules.push(r);
                positions.push(i + 1);
            }
            Err(mut ds) => diags.append(&mut ds),
        }
    }
    diags.extend(check_set(&rules, &positions));
    (positions.into_iter().zip(rules).collect(), diags)
}

/// Sort problems in file order (by rule number; file-level first), keeping
/// each rule's own order (grammar before refs).
pub fn in_file_order(diags: &mut [GridDiag]) {
    diags.sort_by_key(|d| {
        d.field.split(']').next().and_then(|x| x.strip_prefix("rule[")).and_then(|x| x.parse::<usize>().ok()).unwrap_or(0)
    });
}

/// Rules numbered in file order from 1.
fn numbered(rules: &[Rule]) -> Vec<(usize, &Rule)> {
    rules.iter().enumerate().map(|(i, r)| (i + 1, r)).collect()
}

/// Set-level checks that need every rule in hand: ids are unique, and there is
/// at most one `base_rotation` (the floor is a singleton; several is a config
/// smell the resolver tolerates but the grammar rejects).
/// Set-level problems of rules gathered from several files (ids, floor).
pub fn diagnose_set(rules: &[Rule]) -> Vec<GridDiag> {
    let positions: Vec<usize> = (1..=rules.len()).collect();
    check_set(rules, &positions)
}

/// `positions[i]` = the number of `rules[i]` in the file (rules with their
/// own problems are left out of `rules`).
fn check_set(rules: &[Rule], positions: &[usize]) -> Vec<GridDiag> {
    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, r) in rules.iter().enumerate() {
        if !seen.insert(r.id.as_str()) {
            let mut d = GridDiag::new(
                GridCode::DuplicateId,
                format!("rule[{}].id", positions[i]),
                format!("duplicate rule id {:?}", r.id),
            )
            .rejected(r.id.clone());
            d.rule_id = r.id.clone();
            out.push(d);
        }
    }
    let floors: Vec<usize> = rules
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r.kind, RuleKind::BaseRotation { .. }))
        .map(|(i, _)| i)
        .collect();
    if floors.len() > 1 {
        let mut d = GridDiag::new(
            GridCode::SeveralFloors,
            format!("rule[{}].kind", positions[floors[1]]),
            format!("{} base_rotation rules; at most one is allowed (the single floor)", floors.len()),
        )
        .rejected("base_rotation");
        d.rule_id = rules[floors[1]].id.clone();
        out.push(d);
    }
    out
}

/// Convert one parsed `RuleDoc` (number `n` in the file, from 1) into a
/// resolver `Rule`, enforcing the per-kind field rules. On rejection, every
/// problem found, each on its field (the first is the one `parse_grid`
/// reports).
fn to_rule(rd: RuleDoc, n: usize) -> Result<Rule, Vec<GridDiag>> {
    let id = rd.id.trim().to_string();
    let base = format!("rule[{n}]");
    let f = |name: &str| format!("{base}.{name}");
    let mut errs: Vec<GridDiag> = Vec::new();
    fn push(errs: &mut Vec<GridDiag>, code: GridCode, field: String, message: String, rejected: String) {
        errs.push(GridDiag::new(code, field, message).rejected(rejected));
    }
    fn date_of(errs: &mut Vec<GridDiag>, field: String, v: &Option<String>) -> Option<Date> {
        match v.as_deref().map(parse_date) {
            Some(Ok(d)) => Some(d),
            Some(Err(m)) => {
                push(errs, GridCode::BadDate, field, m, v.clone().unwrap_or_default());
                None
            }
            None => None,
        }
    }
    fn time_of(errs: &mut Vec<GridDiag>, field: String, v: &Option<String>) -> Option<WallClock> {
        match v.as_deref().map(parse_wallclock) {
            Some(Ok(w)) => Some(w),
            Some(Err(m)) => {
                push(errs, GridCode::BadTime, field, m, v.clone().unwrap_or_default());
                None
            }
            None => None,
        }
    }

    if id.is_empty() {
        push(&mut errs, GridCode::MissingField, f("id"), "`id` must not be blank".into(), String::new());
    }
    if rd.kind == KindTag::Live {
        if !rd.playlist_ref.is_empty() {
            push(&mut errs, GridCode::NotAllowed,
                f("playlist_ref"),
                "`live` takes no `playlist_ref` (a live slot selects no playlist)".into(),
                rd.playlist_ref.clone(),
            );
        }
    } else {
        if rd.playlist_ref.trim().is_empty() {
            push(&mut errs, GridCode::MissingField,
                f("playlist_ref"),
                "`playlist_ref` is required and must not be blank".into(),
                String::new(),
            );
        }
        if let Some(dj) = &rd.dj {
            push(&mut errs, GridCode::NotAllowed, f("dj"), "`dj` belongs to a `live` rule".into(), dj.clone());
        }
    }

    // Validity (shared by every kind).
    let mut days = Vec::new();
    for d in &rd.days {
        match parse_weekday(d) {
            Some(w) if days.contains(&w) => {
                push(&mut errs, GridCode::BadWeekday, f("days"), format!("duplicate weekday {d:?}"), d.clone())
            }
            Some(w) => days.push(w),
            None => push(&mut errs, GridCode::BadWeekday, f("days"), format!("invalid weekday {d:?} (mon..sun)"), d.clone()),
        }
    }
    let date_start = date_of(&mut errs, f("date_start"), &rd.date_start);
    let date_end = date_of(&mut errs, f("date_end"), &rd.date_end);
    if let (Some(s), Some(e)) = (date_start, date_end) {
        if e < s {
            push(&mut errs, GridCode::DatesReversed,
                f("date_end"),
                "`date_end` must be on or after `date_start`".into(),
                rd.date_end.clone().unwrap_or_default(),
            );
        }
    }
    let validity = Validity { days, date_start, date_end };


    // Fields present that this kind does not take.
    let present: [(&str, bool); 9] = [
        ("start", rd.start.is_some()),
        ("end", rd.end.is_some()),
        ("every_minutes", rd.every_minutes.is_some()),
        ("minute", rd.minute.is_some()),
        ("at", rd.at.is_some()),
        ("mode", rd.mode.is_some()),
        ("expiry", rd.expiry.is_some()),
        ("min_tracks", rd.min_tracks.is_some()),
        ("min_elapsed", rd.min_elapsed.is_some()),
    ];
    let (allowed, not_allowed_msg): (&[&str], &str) = match rd.kind {
        KindTag::BaseRotation => (&[], "`base_rotation` takes no scheduling fields (only playlist_ref + validity)"),
        KindTag::DayPart => (&["start", "end"], "`day_part` only takes `start`/`end` besides validity"),
        KindTag::AtClock => (
            &["every_minutes", "minute", "at", "mode", "expiry"],
            "`at_clock` takes `every_minutes`|`minute`|`at`, `mode`, `expiry` — not day_part/every fields",
        ),
        KindTag::Every => (
            &["min_tracks", "min_elapsed"],
            "`every` takes exactly one of `min_tracks`|`min_elapsed` — not day_part/at_clock fields",
        ),
        KindTag::Live => (&["start"], "`live` only takes `dj` and `start` besides validity"),
    };
    for (name, here) in present {
        if here && !allowed.contains(&name) {
            let msg = if rd.kind == KindTag::Live && name == "end" {
                "`live` takes no `end`: the slot lasts until the next one starts, \
                 the DJ stays on air until disconnection or silence"
                    .to_string()
            } else {
                not_allowed_msg.to_string()
            };
            errs.push(GridDiag::new(GridCode::NotAllowed, f(name), msg).rejected(name).expected(allowed.join(", ")));
        }
    }

    let playlist_ref = rd.playlist_ref.clone();
    let kind = match rd.kind {
        KindTag::BaseRotation => Some(RuleKind::BaseRotation { playlist_ref }),
        KindTag::DayPart => {
            if rd.start.is_none() {
                errs.push(GridDiag::new(GridCode::MissingField, f("start"), "`day_part` requires `start`"));
            }
            let start = time_of(&mut errs, f("start"), &rd.start);
            // No `end` = an OPEN day part: it runs until the next start of
            // another day part (resolver::open_part_covers).
            let end = time_of(&mut errs, f("end"), &rd.end);
            if let (Some(s), Some(e)) = (start, end) {
                if minutes(e) == minutes(s) {
                    errs.push(
                        GridDiag::new(
                            GridCode::ZeroWindow,
                            f("end"),
                            "`start` and `end` must differ (a zero-length window covers nothing; \
                             use a base_rotation for 24h, or omit `end` for an open day part)",
                        )
                        .rejected(rd.end.clone().unwrap_or_default()),
                    );
                }
            }
            // `end < start` is a cross-midnight window (e.g. 22:00→06:00).
            start.map(|start| RuleKind::DayPart { playlist_ref, start, end })
        }
        KindTag::AtClock => {
            let anchor = at_clock_anchor(&mut errs, &f, &rd);
            let mode = match rd.mode {
                Some(ModeTag::Hard) => Mode::Hard,
                _ => Mode::Soft,
            };
            let expiry_secs = match rd.expiry.as_deref().map(parse_duration_secs) {
                Some(Ok(s)) => Some(Some(s)),
                Some(Err(m)) => {
                    errs.push(
                        GridDiag::new(GridCode::BadDuration, f("expiry"), m).rejected(rd.expiry.clone().unwrap_or_default()),
                    );
                    None
                }
                None => Some(None),
            };
            match (anchor, expiry_secs) {
                (Some(anchor), Some(expiry_secs)) => {
                    Some(RuleKind::AtClock { playlist_ref, anchor, mode, expiry_secs })
                }
                _ => None,
            }
        }
        KindTag::Every => {
            let cadence = match (rd.min_tracks, rd.min_elapsed.as_deref()) {
                (Some(_), Some(_)) => {
                    errs.push(
                        GridDiag::new(
                            GridCode::Conflict,
                            f("min_elapsed"),
                            "`min_tracks` and `min_elapsed` are mutually exclusive",
                        )
                        .rejected(rd.min_elapsed.clone().unwrap_or_default()),
                    );
                    None
                }
                (None, None) => {
                    errs.push(
                        GridDiag::new(
                            GridCode::MissingField,
                            f("min_tracks"),
                            "`every` requires exactly one of `min_tracks` or `min_elapsed`",
                        )
                        .expected("min_tracks, min_elapsed"),
                    );
                    None
                }
                (Some(n), None) => {
                    if n < 1 {
                        errs.push(
                            GridDiag::new(GridCode::OutOfRange, f("min_tracks"), "`min_tracks` must be at least 1")
                                .rejected(n.to_string())
                                .expected("1.."),
                        );
                        None
                    } else {
                        Some(Cadence::Tracks(n))
                    }
                }
                (None, Some(d)) => match parse_duration_secs(d) {
                    Ok(s) => Some(Cadence::Elapsed(s)),
                    Err(m) => {
                        errs.push(GridDiag::new(GridCode::BadDuration, f("min_elapsed"), m).rejected(d));
                        None
                    }
                },
            };
            cadence.map(|cadence| RuleKind::Every { playlist_ref, cadence })
        }
        KindTag::Live => {
            let dj = rd.dj.as_deref().map(str::trim).unwrap_or_default().to_string();
            if dj.is_empty() {
                errs.push(GridDiag::new(GridCode::MissingField, f("dj"), "`live` requires `dj` (an id of the DJ file)"));
            }
            if rd.start.is_none() {
                errs.push(GridDiag::new(GridCode::MissingField, f("start"), "`live` requires `start`"));
            }
            let start = time_of(&mut errs, f("start"), &rd.start);
            start.filter(|_| !dj.is_empty()).map(|start| RuleKind::Live { dj, start })
        }
    };

    // Errors first found in the order of the old fail-fast checks: blank id,
    // playlist_ref / dj, validity, stray fields, then the kind's own fields.
    if !errs.is_empty() {
        for d in &mut errs {
            d.rule_id = id.clone();
        }
        return Err(errs);
    }
    let kind = kind.expect("no error means the kind was built");
    Ok(Rule { id, enabled: rd.enabled, validity, kind })
}

/// Refs of the rules against the known playlist keys, each problem on its
/// rule's `playlist_ref`.
pub fn diagnose_refs(rules: &[Rule], known: &HashSet<String>) -> Vec<GridDiag> {
    diagnose_refs_at(&numbered(rules), known)
}

/// [`diagnose_refs`] of rules given with their number in the file.
pub fn diagnose_refs_at(rules: &[(usize, &Rule)], known: &HashSet<String>) -> Vec<GridDiag> {
    let mut out = Vec::new();
    for (n, r) in rules {
        let Some(raw) = playlist_ref_of(r) else { continue };
        let field = format!("rule[{n}].playlist_ref");
        let mut d = match crate::playlist::normalize_ref(raw) {
            Ok(key) if known.contains(&key) => continue,
            Ok(key) => GridDiag::new(
                GridCode::UnknownRef,
                field,
                format!("rule {:?}: playlist_ref `{raw}` refers to unknown playlist `{key}`", r.id),
            ),
            Err(msg) => GridDiag::new(GridCode::BadRef, field, format!("rule {:?}: {msg}", r.id)),
        }
        .rejected(raw);
        d.rule_id = r.id.clone();
        out.push(d);
    }
    out
}

/// DJs of the `live` rules: `known` = the ids of the DJ file, `None` = no
/// `[live]` section (nobody could ever connect).
pub fn diagnose_djs(rules: &[Rule], known: Option<&HashSet<String>>) -> Vec<GridDiag> {
    diagnose_djs_at(&numbered(rules), known)
}

/// [`diagnose_djs`] of rules given with their number in the file.
pub fn diagnose_djs_at(rules: &[(usize, &Rule)], known: Option<&HashSet<String>>) -> Vec<GridDiag> {
    let mut out = Vec::new();
    for (n, r) in rules {
        let RuleKind::Live { dj, .. } = &r.kind else { continue };
        let field = format!("rule[{n}].dj");
        let d = match known {
            None => GridDiag::new(
                GridCode::NoLive,
                field,
                format!("rule {:?}: a `live` rule needs the `[live]` section in stationd.toml (no harbor)", r.id),
            ),
            Some(k) if !k.contains(dj) => {
                GridDiag::new(GridCode::UnknownDj, field, format!("rule {:?}: dj `{dj}` is not in the DJ file", r.id))
            }
            Some(_) => continue,
        };
        let mut d = d.rejected(dj.clone());
        d.rule_id = r.id.clone();
        out.push(d);
    }
    out
}

/// The grid file could not be read at all.
pub fn file_unreadable(message: String) -> GridDiag {
    GridDiag::new(GridCode::FileUnreadable, "", message)
}

/// A problem reading the DJ file, for the grid's `live` rules.
pub fn dj_file_unreadable(message: String) -> GridDiag {
    GridDiag::new(GridCode::DjFileUnreadable, "", format!("live rules cannot be checked: {message}"))
}

/// Check that every rule's `playlist_ref` resolves to a known playlist.
/// Pure: `known` is the set of canonical playlist keys (the `rel_path`s of the
/// view), supplied by the caller. Collects all problems ([`diagnose_refs`]).
pub fn validate_refs(rules: &[Rule], known: &HashSet<String>) -> Vec<String> {
    diagnose_refs(rules, known).into_iter().map(|d| d.message).collect()
}

/// The playlist a rule airs (`None` for a live slot).
pub fn playlist_ref_of(r: &Rule) -> Option<&str> {
    match &r.kind {
        RuleKind::BaseRotation { playlist_ref } => Some(playlist_ref),
        RuleKind::DayPart { playlist_ref, .. } => Some(playlist_ref),
        RuleKind::AtClock { playlist_ref, .. } => Some(playlist_ref),
        RuleKind::Every { playlist_ref, .. } => Some(playlist_ref),
        RuleKind::Live { .. } => None,
    }
}

/// Check that every `live` rule names a known DJ. `known` = the DJ ids of the
/// DJ file, `None` when no `[live]` section is configured (then any `live`
/// rule is an error: nobody could ever connect). Collects all problems.
pub fn validate_djs(rules: &[Rule], known: Option<&HashSet<String>>) -> Vec<String> {
    diagnose_djs(rules, known).into_iter().map(|d| d.message).collect()
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

/// The anchor of an `at_clock`: exactly one of `every_minutes` (marks N,
/// 2N… minutes after midnight), `minute` (one mark per hour, at :MM) or `at` (one fixed time
/// of day).
fn at_clock_anchor(errs: &mut Vec<GridDiag>, f: &dyn Fn(&str) -> String, rd: &RuleDoc) -> Option<ClockAnchor> {
    let given: Vec<&str> = [
        ("every_minutes", rd.every_minutes.is_some()),
        ("minute", rd.minute.is_some()),
        ("at", rd.at.is_some()),
    ]
    .into_iter()
    .filter_map(|(k, here)| here.then_some(k))
    .collect();
    match given.as_slice() {
        [] => {
            errs.push(
                GridDiag::new(
                    GridCode::MissingField,
                    f("at"),
                    "`at_clock` requires exactly one of `every_minutes`, `minute` or `at`",
                )
                .expected("every_minutes, minute, at"),
            );
            return None;
        }
        [_] => {}
        [_, rest @ ..] => {
            for k in rest {
                errs.push(
                    GridDiag::new(
                        GridCode::Conflict,
                        f(k),
                        format!("`{}` and `{k}` are mutually exclusive", given[0]),
                    )
                    .rejected(*k),
                );
            }
            return None;
        }
    }
    if let Some(n) = rd.every_minutes {
        // Marks N, 2N… minutes after midnight, within the day: from 1440 on
        // there would be none.
        if (1..1440).contains(&n) {
            return Some(ClockAnchor::EveryMinutes(n));
        }
        errs.push(
            GridDiag::new(
                GridCode::OutOfRange,
                f("every_minutes"),
                format!(
                    "`every_minutes` must be between 1 and 1439 (got {n}): its repères fall \
                     N, 2N… minutes after midnight, within the day"
                ),
            )
            .rejected(n.to_string())
            .expected("1..1439"),
        );
        return None;
    }
    if let Some(m) = rd.minute {
        if m < 60 {
            return Some(ClockAnchor::Minute(m as u8));
        }
        errs.push(
            GridDiag::new(
                GridCode::OutOfRange,
                f("minute"),
                format!("`minute` must be between 0 and 59 (got {m})"),
            )
            .rejected(m.to_string())
            .expected("0..59"),
        );
        return None;
    }
    fn time_of(errs: &mut Vec<GridDiag>, field: String, v: &Option<String>) -> Option<WallClock> {
        match v.as_deref().map(parse_wallclock) {
            Some(Ok(w)) => Some(w),
            Some(Err(m)) => {
                errs.push(GridDiag::new(GridCode::BadTime, field, m).rejected(v.clone().unwrap_or_default()));
                None
            }
            None => None,
        }
    }
    time_of(errs, f("at"), &rd.at).map(ClockAnchor::At)
}

fn rule_to_doc(r: &Rule) -> RuleDoc {
    let mut d = RuleDoc {
        id: r.id.clone(),
        enabled: r.enabled,
        kind: KindTag::BaseRotation, // placeholder, overwritten below
        playlist_ref: String::new(),
        dj: None,
        start: None,
        end: None,
        every_minutes: None,
        minute: None,
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
            d.end = end.map(wallclock_str);
        }
        RuleKind::AtClock { playlist_ref, anchor, mode, expiry_secs } => {
            d.kind = KindTag::AtClock;
            d.playlist_ref = playlist_ref.clone();
            match anchor {
                ClockAnchor::EveryMinutes(n) => d.every_minutes = Some(*n),
                ClockAnchor::Minute(m) => d.minute = Some(u32::from(*m)),
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
        RuleKind::Live { dj, start } => {
            d.kind = KindTag::Live;
            d.dj = Some(dj.clone());
            d.start = Some(wallclock_str(*start));
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
                assert_eq!((start.hour, end.unwrap().hour), (8, 10));
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
    fn diagnose_ties_every_problem_to_its_field() {
        let t = "schema_version = 1\n[[rule]]\nid = \"a\"\nkind = \"base_rotation\"\nplaylist_ref = \"m\"\n\
                 [[rule]]\nid = \"b\"\nkind = \"day_part\"\nplaylist_ref = \"n\"\nstart = \"07:00\"\nend = \"7h\"\ndays = [\"lun\"]\nmode = \"hard\"\n\
                 [[rule]]\nid = \"a\"\nkind = \"base_rotation\"\nplaylist_ref = \"m\"\n";
        let (rules, d) = diagnose(t);
        assert!(rules.is_none());
        let got: Vec<(GridCode, &str, &str)> =
            d.iter().map(|x| (x.code, x.field.as_str(), x.rejected.as_str())).collect();
        assert_eq!(
            got,
            [
                (GridCode::BadWeekday, "rule[2].days", "lun"),
                (GridCode::NotAllowed, "rule[2].mode", "mode"),
                (GridCode::BadTime, "rule[2].end", "7h"),
                (GridCode::DuplicateId, "rule[3].id", "a"),
                (GridCode::SeveralFloors, "rule[3].kind", "base_rotation"),
            ],
            "{d:?}"
        );
        assert!(d[..3].iter().all(|x| x.rule_id == "b"));
        // Set-level once the rules themselves are fine.
        let t = t.replace("end = \"7h\"\ndays = [\"lun\"]\nmode = \"hard\"\n", "");
        let (_, d) = diagnose(&t);
        let got: Vec<(GridCode, &str)> = d.iter().map(|x| (x.code, x.field.as_str())).collect();
        assert_eq!(got, [(GridCode::DuplicateId, "rule[3].id"), (GridCode::SeveralFloors, "rule[3].kind")]);
    }

    #[test]
    fn diagnose_places_grammar_errors_by_their_position() {
        let (_, d) = diagnose("schema_version = 1\n[[rule]]\nid = \"a\"\nkind = \"base_rotation\"\nplaylist_ref = \"m\"\n[[rule]]\nid = \"b\"\nkind = \"nope\"\n");
        assert_eq!((d[0].code, d[0].field.as_str()), (GridCode::BadValue, "rule[2].kind"), "{d:?}");
        assert!(d[0].expected.contains("day_part"), "{d:?}");
        let (_, d) = diagnose("schema_version = 1\n[[rule]]\nid = \"a\"\nkind = \"base_rotation\"\nplaylist = \"m\"\n");
        assert_eq!((d[0].code, d[0].field.as_str()), (GridCode::UnknownField, "rule[1].playlist"), "{d:?}");
        let (_, d) = diagnose("schema_version = 1\n[[rule]\n");
        assert_eq!(d[0].code, GridCode::Syntax);
        let (_, d) = diagnose("schema_version = 2\n");
        assert_eq!((d[0].code, d[0].field.as_str()), (GridCode::SchemaVersion, "schema_version"));
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
    fn accepts_cross_midnight_day_part() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "night"
            kind = "day_part"
            playlist_ref = "g"
            start = "22:00"
            end = "06:00"
        "#;
        let rules = parse_grid(toml).expect("cross-midnight day_part parses");
        match &rules[0].kind {
            RuleKind::DayPart { start, end, .. } => {
                assert_eq!((start.hour, start.minute), (22, 0));
                assert_eq!(end.map(|e| (e.hour, e.minute)), Some((6, 0)));
            }
            _ => panic!("expected DayPart"),
        }
    }

    #[test]
    fn rejects_zero_length_day_part() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "noop"
            kind = "day_part"
            playlist_ref = "g"
            start = "08:00"
            end = "08:00"
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
            every_minutes = 1440
        "#;
        assert!(matches!(parse_grid(toml), Err(GridTomlError::Rule { .. })));
    }

    #[test]
    fn every_minutes_takes_any_cadence_within_the_day() {
        let at = |n: u32| {
            format!("schema_version = 1\n[[rule]]\nid = \"x\"\nkind = \"at_clock\"\nplaylist_ref = \"g\"\nevery_minutes = {n}\n")
        };
        for n in [1, 7, 45, 60, 90, 1439] {
            assert!(parse_grid(&at(n)).is_ok(), "{n}");
        }
        for n in [0, 1440] {
            let (rules, diags) = diagnose(&at(n));
            assert!(rules.is_none());
            assert_eq!((diags[0].code, diags[0].field.as_str()), (GridCode::OutOfRange, "rule[1].every_minutes"));
        }
    }

    #[test]
    fn a_minute_anchor_is_one_mark_per_hour() {
        let toml = r#"
            schema_version = 1
            [[rule]]
            id = "top"
            kind = "at_clock"
            playlist_ref = "tops"
            minute = 58
        "#;
        let rules = parse_grid(toml).unwrap();
        assert!(matches!(rules[0].kind, RuleKind::AtClock { anchor: ClockAnchor::Minute(58), .. }));
        let out = to_toml(&rules).unwrap();
        assert!(out.contains("minute = 58"), "{out}");
        assert_eq!(parse_grid(&out).unwrap().len(), 1);

        let (_, diags) = diagnose(&toml.replace("58", "60"));
        assert_eq!((diags[0].code, diags[0].field.as_str()), (GridCode::OutOfRange, "rule[1].minute"));
        let (_, diags) = diagnose(&toml.replace("minute = 58", "minute = 0\n            at = \"08:00\""));
        assert_eq!((diags[0].code, diags[0].field.as_str()), (GridCode::Conflict, "rule[1].at"));
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
    #[test]
    fn a_day_part_without_end_is_open_and_exports_without_end() {
        let grid = r#"
            schema_version = 1
            [[rule]]
            id = "morning"
            kind = "day_part"
            playlist_ref = "matin"
            start = "06:00"
        "#;
        let rules = parse_grid(grid).expect("an open day part parses");
        match &rules[0].kind {
            RuleKind::DayPart { start, end, .. } => {
                assert_eq!((start.hour, start.minute), (6, 0));
                assert!(end.is_none());
            }
            _ => panic!("expected DayPart"),
        }
        let t = to_toml(&rules).unwrap();
        assert!(t.contains("start = \"06:00\"") && !t.contains("end ="), "{t}");
        let again = parse_grid(&t).unwrap();
        assert!(matches!(again[0].kind, RuleKind::DayPart { end: None, .. }));
    }

    const LIVE: &str = r#"
        schema_version = 1
        [[rule]]
        id = "marc-live"
        kind = "live"
        dj = "marc"
        start = "20:00"
        days = ["fri"]
    "#;

    #[test]
    fn a_live_rule_parses_and_round_trips() {
        let rules = parse_grid(LIVE).expect("a live rule parses");
        match &rules[0].kind {
            RuleKind::Live { dj, start } => {
                assert_eq!(dj, "marc");
                assert_eq!((start.hour, start.minute), (20, 0));
            }
            k => panic!("expected Live, got {k:?}"),
        }
        assert_eq!(rules[0].validity.days, vec![Weekday::Fri]);
        let t = to_toml(&rules).unwrap();
        assert!(t.contains("kind = \"live\"") && t.contains("dj = \"marc\""), "{t}");
        assert!(!t.contains("playlist_ref"), "a live rule has no playlist: {t}");
        assert_eq!(to_toml(&parse_grid(&t).unwrap()).unwrap(), t);
        // no playlist ref to check
        assert!(validate_refs(&rules, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_live_rule_rejects_what_it_does_not_take() {
        let with = |extra: &str| format!("{LIVE}{extra}\n");
        for (extra, want) in [
            ("end = \"22:00\"", "no `end`"),
            ("playlist_ref = \"x\"", "no `playlist_ref`"),
            ("mode = \"hard\"", "only takes `dj` and `start`"),
        ] {
            let err = parse_grid(&with(extra)).unwrap_err().to_string();
            assert!(err.contains(want), "{extra}: {err}");
        }
        let no_dj = LIVE.replace("dj = \"marc\"", "");
        assert!(parse_grid(&no_dj).unwrap_err().to_string().contains("requires `dj`"));
        let no_start = LIVE.replace("start = \"20:00\"", "");
        assert!(parse_grid(&no_start).unwrap_err().to_string().contains("requires `start`"));
        // `dj` on another kind, and a playlist rule without playlist_ref
        let stray = "schema_version = 1\n[[rule]]\nid = \"f\"\nkind = \"base_rotation\"\nplaylist_ref = \"g\"\ndj = \"marc\"\n";
        assert!(parse_grid(stray).unwrap_err().to_string().contains("`dj` belongs to a `live` rule"));
        let bare = "schema_version = 1\n[[rule]]\nid = \"f\"\nkind = \"base_rotation\"\n";
        assert!(parse_grid(bare).unwrap_err().to_string().contains("`playlist_ref` is required"));
    }

    #[test]
    fn validate_djs_needs_the_live_section_and_known_djs() {
        let rules = parse_grid(LIVE).unwrap();
        let none = validate_djs(&rules, None);
        assert_eq!(none.len(), 1);
        assert!(none[0].contains("[live]"), "{none:?}");
        let known: HashSet<String> = ["julie".to_string()].into();
        let unknown = validate_djs(&rules, Some(&known));
        assert!(unknown[0].contains("dj `marc` is not in the DJ file"), "{unknown:?}");
        let known: HashSet<String> = ["marc".to_string()].into();
        assert!(validate_djs(&rules, Some(&known)).is_empty());
        // a grid without live rule needs nothing
        assert!(validate_djs(&parse_grid(GRID).unwrap(), None).is_empty());
    }
}