//! Playlist model and TOML (de)serialization.
//!
//! Per the architecture docs: playlists are file-first (one TOML file per
//! playlist is the source of truth; SQLite is a rebuildable view). This
//! module lives in `stationd` — NOT in the CLI — because all business logic
//! (validation, id generation) must sit behind the gRPC contract so every
//! client (stationctl today, the web frontend later) hits the same code.
//!
//! `stationctl` only does a syntactic pre-check (is this well-formed TOML?)
//! then ships the content here. stationd parses, validates, assigns the id,
//! and rewrites — losslessly — before the CLI writes the file back to disk.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One playlist = one TOML file. Root-level `id`/`name`/`enabled`, with
/// `selection` (the *what*) and `broadcast` (the *when/how*) as sub-tables.
///
/// `id` is `Option` on purpose: a freshly hand-written file has no id yet
/// (the human never picks it — "too important to leave to the user"). It is
/// absent = "not yet added", present = "already registered". `playlist add`
/// generates it. `deny_unknown_fields` enforces no-silent-failure: an
/// unknown key is a loud error, never quietly ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Playlist {
    /// System-generated UUID (v4). Absent in a hand-written file, filled in
    /// by `playlist add`. Referenced by groups, the CLI, SQLite history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,

    /// Display name only. May change, may be translated.
    pub name: String,

    #[serde(default = "default_enabled")]
    pub enabled: bool,

    pub selection: Selection,
    /// The playlist's own consumption policy — optional, NOT scheduling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broadcast: Option<Broadcast>,
}

fn default_enabled() -> bool {
    true
}

/// The *what*: nature of the source and how it's selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub mode: Mode,

    /// Traversal discipline. Valid values depend on `mode` (validated in
    /// `validate`, not by the type system). Omitted for `group` (there
    /// `strategy` governs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Order>,

    // --- dynamic ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#match: Option<Match>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter: Vec<Filter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_by: Option<OrderBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unplayed_only: Option<bool>,

    // --- static ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,

    // --- remote ---
    /// URL of a remote stream to relay (another Icecast, an HTTP stream).
    /// The playlist's `name` (root level) is its programming name, e.g.
    /// url `http://nightmusic.live` + name "goodnight".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    // --- queue ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u32>,

    // --- group ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Strategy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
    /// What a `sequence` group does when a member yields no media (group mode
    /// only). Default `abort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_member_unavailable: Option<MemberUnavailable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Static,
    Dynamic,
    Remote,
    Queue,
    Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Order {
    Shuffle,
    Sequential,
    Newest,
    Oldest,
    Fifo,
    Lifo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Match {
    All,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderBy {
    Mtime,
    Filename,
    Published,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    Weighted,
    Rotate,
    Sequence,
    Shuffle,
}

/// What a `sequence` group does when a member produces no media:
/// - `abort` (default): the whole group fails → the grid falls through to a
///   lower-priority source (down to the BaseRotation floor);
/// - `skip`: drop the unavailable member and continue the sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberUnavailable {
    Abort,
    Skip,
}

/// A dynamic-selection filter: structured `field`/`op`/`value`, no string
/// DSL (keeps the parsing surface small, per the doc). `value` is a free
/// TOML value (string, number, or array); its field/op/value shape is checked
/// in `Playlist::validate` — via the same pure catalogue check the resolver
/// uses — so a malformed filter is a loud error at apply/validate, not on air.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub field: String,
    pub op: String,
    pub value: toml::Value,
}

/// A group member: a reference to another playlist, plus an optional quota.
/// In a `weighted` group a member may carry a `weight`. In a `sequence` or
/// `shuffle` group a member may carry a per-member quota, either in tracks
/// (`take`) or in wall-clock time (`runtime`, e.g. "20m") — the two are
/// mutually exclusive.
///
/// `ref` is relative to the playlist root (`shows/intro`), unless it starts
/// with `./` or `../`: then it is relative to the GROUP's own directory
/// (`./intro` from `shows/main` → `shows/intro`). See [`resolve_member_ref`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take: Option<u32>,
    /// Per-member time budget (e.g. "20m"), alternative to `take`. Soft: the
    /// member's last track may overrun the budget; the switch happens at the
    /// next track boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// The playlist's own **consumption policy** — NOT scheduling. When (and
/// whether) a playlist is on air is the GRID's job (`grid.toml`); a playlist
/// only says how to consume its own source once the grid activates it. This
/// deliberately no longer carries `type`/`weight`/`schedule`/`every_*`: those
/// were ordering, they duplicated the grid, and are now a loud parse error
/// (`deny_unknown_fields`). Optional — a playlist with no policy omits it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Broadcast {
    /// Max tracks emitted per activation before yielding the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Re-parse the pool within one activation once exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<bool>,
    /// Behaviour when a finite source is exhausted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<OnExhausted>,
    /// Anti-repetition constraints applied while sequencing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraints: Option<Constraints>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnExhausted {
    Fallthrough,
    Stop,
    Disable,
    Hold,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_same_artist_within: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_same_track_within: Option<String>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PlaylistError {
    #[error("invalid playlist TOML: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("validation failed: {0}")]
    Validation(String),

    #[error("could not rewrite playlist TOML: {0}")]
    Rewrite(#[from] toml_edit::TomlError),
}

// ---------------------------------------------------------------------------
// Parse / validate / assign id / rewrite
// ---------------------------------------------------------------------------

impl Playlist {
    /// Parse a playlist from TOML text. Strict: unknown fields are rejected.
    pub fn parse(toml_str: &str) -> Result<Self, PlaylistError> {
        let playlist: Playlist = toml::from_str(toml_str)?;
        Ok(playlist)
    }

    /// Business-logic validation. Lives here (in stationd) so every client
    /// gets the same rules. Covers the clear-cut rules from the doc; the
    /// heavier cross-playlist checks (group cycle detection, referential
    /// integrity of `members`/`files`) need the whole set loaded and are
    /// left as explicit TODOs for the reconciliation pass.
    pub fn validate(&self) -> Result<(), PlaylistError> {
        let err = |msg: &str| PlaylistError::Validation(msg.to_string());

        // `order` valid values depend on `mode`.
        match self.selection.mode {
            Mode::Static => {
                self.check_order_in(&[Order::Shuffle, Order::Sequential], "static")?;
            }
            Mode::Dynamic => {
                self.check_order_in(
                    &[Order::Shuffle, Order::Sequential, Order::Newest, Order::Oldest],
                    "dynamic",
                )?;
            }
            Mode::Remote => {
                if self.selection.order.is_some() {
                    return Err(err("`order` is not allowed for a remote (a relayed stream has no internal order)"));
                }
            }
            Mode::Queue => {
                self.check_order_in(&[Order::Fifo, Order::Lifo], "queue")?;
            }
            Mode::Group => {
                if self.selection.order.is_some() {
                    return Err(err("`order` is not allowed for a group (use `strategy`)"));
                }
                if self.selection.strategy.is_none() {
                    return Err(err("a group requires `strategy`"));
                }
            }
        }

        // `unplayed_only` (play-once) only makes sense on a dated order: it
        // dequeues a growing series oldest/newest-first. On any other order it
        // is a loud error, never silently ignored.
        if self.selection.unplayed_only == Some(true)
            && !matches!(self.selection.order, Some(Order::Newest) | Some(Order::Oldest))
        {
            return Err(err("`unplayed_only` requires `order = newest` or `order = oldest`"));
        }

        // Per-member quotas. `weight` is for a `weighted` group; `take`
        // (tracks) and `runtime` (time budget) are per-member quotas for a
        // `sequence` or `shuffle` group, and are mutually exclusive.
        let quota_group = matches!(
            self.selection.strategy,
            Some(Strategy::Sequence) | Some(Strategy::Shuffle)
        );
        for m in &self.selection.members {
            if m.take.is_some() && m.runtime.is_some() {
                return Err(err(
                    "a member cannot have both `take` and `runtime` (tracks XOR time budget)",
                ));
            }
            if m.take.is_some() && !quota_group {
                return Err(err("`take` on a member requires a `sequence` or `shuffle` group"));
            }
            if m.runtime.is_some() && !quota_group {
                return Err(err(
                    "`runtime` on a member requires a `sequence` or `shuffle` group",
                ));
            }
            if let Some(r) = &m.runtime {
                parse_duration_secs(r).map_err(|e| {
                    PlaylistError::Validation(format!(
                        "member `{}` has an invalid `runtime`: {e}",
                        m.r#ref
                    ))
                })?;
            }
            if m.weight.is_some() && self.selection.strategy != Some(Strategy::Weighted) {
                return Err(err("`weight` on a member requires a `weighted` group"));
            }
        }

        // Mode-shape sanity: the right fields for the right mode, and no
        // field that belongs to another mode (no-silent-failure: a stray
        // field must be a loud error, never quietly ignored).
        match self.selection.mode {
            Mode::Static if self.selection.files.is_empty() => {
                return Err(err("a static playlist needs `files`"));
            }
            Mode::Remote if self.selection.url.is_none() => {
                return Err(err("a remote playlist needs `url`"));
            }
            Mode::Group if self.selection.members.is_empty() => {
                return Err(err("a group needs `members`"));
            }
            _ => {}
        }

        // `url` only belongs to a remote.
        if self.selection.url.is_some() && self.selection.mode != Mode::Remote {
            return Err(err("`url` is only valid for mode `remote`"));
        }

        // Dynamic-selection filters: each `field`/`op`/`value` must be a valid
        // catalogue entry. Checked here with the same pure check the resolver
        // uses, so a malformed filter — e.g. `has_any` with a bare string
        // instead of a list — is a loud error at apply/validate, never on air.
        for (i, f) in self.selection.filter.iter().enumerate() {
            crate::selection::validate_filter(f).map_err(|e| {
                PlaylistError::Validation(format!("selection.filter[{}]: {e}", i + 1))
            })?;
        }

        // Cross-playlist checks (group cycle detection, referential
        // integrity of member `ref`s) are NOT done here — a single file
        // cannot see the whole set. They live in `validate_set`, called by
        // the sync pass once every file is loaded.

        Ok(())
    }

    fn check_order_in(&self, allowed: &[Order], mode_name: &str) -> Result<(), PlaylistError> {
        if let Some(order) = self.selection.order {
            if !allowed.contains(&order) {
                return Err(PlaylistError::Validation(format!(
                    "`order = {order:?}` is not valid for mode `{mode_name}`"
                )));
            }
        }
        Ok(())
    }
}

/// Parse a duration in the playlist/grid grammar `[1-9][0-9]*(s|m|h|d)` into
/// seconds: a single unit, no leading zero, no zero duration. Loud error on
/// anything else. (Deliberately duplicated from `grid_toml` — same grammar,
/// separate scope — so this module stays free-standing, like the assumed
/// `parse_date`/`parse_weekday` duplication.)
pub fn parse_duration_secs(s: &str) -> Result<u64, String> {
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

// ---------------------------------------------------------------------------
// Group projection (for the preview): decompose a sequence/shuffle group into
// its members with their quota and, when computable, a start offset. Pure and
// DB-free — track durations are unknown, so a `take` member never gets an
// offset and breaks the offset chain for everything after it; a `shuffle` gets
// no offsets at all (its order is drawn at runtime).
// ---------------------------------------------------------------------------

/// A member's quota as projected: tracks (`take`) or a time budget (`runtime`,
/// in seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberQuota {
    Take(u32),
    Runtime(u64),
}

/// One group member, projected for the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedMember {
    pub r#ref: String,
    pub quota: MemberQuota,
    /// Seconds from the group's activation start, when the preview can place
    /// it (a `runtime` member whose predecessors in a `sequence` are all
    /// `runtime`); `None` otherwise.
    pub offset_secs: Option<u64>,
}

/// A group's members as projected for the preview, plus its strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupProjection {
    pub strategy: Strategy,
    pub members: Vec<ProjectedMember>,
}

impl Playlist {
    /// Decompose a `sequence`/`shuffle` group for the preview. `None` for any
    /// other playlist — a leaf, a `remote`/`queue`, or a `weighted`/`rotate`
    /// group (those have no per-member `take`/`runtime` timeline). Pure: a
    /// `take` member never gets an offset and breaks the offset chain for
    /// everything after it (its track duration is unknown); a `shuffle` gets no
    /// offsets at all. A `runtime` that somehow fails to parse (validation
    /// should have caught it) is treated as a 0s budget rather than panicking.
    pub fn project_group_members(&self) -> Option<GroupProjection> {
        let sel = &self.selection;
        if sel.mode != Mode::Group {
            return None;
        }
        let strategy = match sel.strategy {
            Some(s @ (Strategy::Sequence | Strategy::Shuffle)) => s,
            _ => return None,
        };
        // `sequence` accumulates offsets from the top; `shuffle` never has a
        // known order, so the chain starts "broken" (`None`).
        let mut cumulative: Option<u64> = (strategy == Strategy::Sequence).then_some(0);
        let members = sel
            .members
            .iter()
            .map(|m| {
                if let Some(r) = &m.runtime {
                    let secs = parse_duration_secs(r).unwrap_or(0);
                    let offset = cumulative;
                    cumulative = cumulative.map(|c| c.saturating_add(secs));
                    ProjectedMember {
                        r#ref: m.r#ref.clone(),
                        quota: MemberQuota::Runtime(secs),
                        offset_secs: offset,
                    }
                } else {
                    // Unknown duration → no offset, and the chain is broken for
                    // every later member.
                    cumulative = None;
                    ProjectedMember {
                        r#ref: m.r#ref.clone(),
                        quota: MemberQuota::Take(m.take.unwrap_or(1).max(1)),
                        offset_secs: None,
                    }
                }
            })
            .collect();
        Some(GroupProjection { strategy, members })
    }
}

/// Assign a freshly generated UUID v4 to a playlist's TOML, losslessly:
/// only the `id` line is injected, all comments/formatting are preserved
/// (`toml_edit`). If the document already has an `id`, it is kept as-is and
/// returned unchanged — `add` is idempotent on an already-registered file.
///
/// Returns the (possibly rewritten) TOML text and the effective id.
pub fn assign_id(toml_str: &str) -> Result<(String, Uuid), PlaylistError> {
    let mut doc = toml_str.parse::<toml_edit::DocumentMut>()?;

    if let Some(existing) = doc.get("id").and_then(|v| v.as_str()) {
        if let Ok(id) = Uuid::parse_str(existing) {
            return Ok((doc.to_string(), id));
        }
        // An `id` that is present but not a valid UUID is a loud error, not
        // something we silently overwrite.
        return Err(PlaylistError::Validation(
            "`id` present but not a valid UUID".to_string(),
        ));
    }

    let id = Uuid::new_v4();
    doc["id"] = toml_edit::value(id.to_string());
    Ok((doc.to_string(), id))
}

// ---------------------------------------------------------------------------
// Cross-playlist validation (the whole set): reference resolution + cycle
// detection. Done on the files alone — no database needed — which is why it
// lives here as a pure function the sync handler calls after loading every
// file. `add` (one file) cannot do this; only `sync` sees the whole set.
// ---------------------------------------------------------------------------

/// Normalize a playlist reference / relative path into a canonical key used
/// both for `ref`s and for the scanned files' relative paths, so the two
/// compare reliably across OSes:
///
/// - `\` → `/` (portable: a ref written on Windows must resolve on Linux)
/// - a trailing `.toml` is stripped (refs name the playlist, not the file)
/// - lowercased (case-insensitive matching, decided for portability)
/// - a leading `/` and empty/`.` segments are rejected or dropped
/// - `..` is rejected outright (no escaping the playlist root)
///
/// Returns the canonical key, or an error describing why the ref is unsafe.
pub fn normalize_ref(raw: &str) -> Result<String, String> {
    let unified = raw.replace('\\', "/");
    let trimmed = unified.strip_suffix(".toml").unwrap_or(&unified);

    if trimmed.starts_with('/') {
        return Err(format!("`{raw}` must be relative to the playlist root (no leading `/`)"));
    }

    let mut segments = Vec::new();
    for seg in trimmed.split('/') {
        match seg {
            "" | "." => continue, // collapse `a//b` and `./a`
            ".." => return Err(format!("`{raw}` must not contain `..` (cannot escape the root)")),
            s => segments.push(s.to_lowercase()),
        }
    }

    if segments.is_empty() {
        return Err(format!("`{raw}` is an empty reference"));
    }

    Ok(segments.join("/"))
}

/// Resolve a group member's `ref` to its canonical key, given the group's own
/// canonical key (`group_key`, e.g. `shows/main`).
///
/// - A ref starting with `./` or `../` (or exactly `.`/`..`) is **relative to
///   the group's directory**: `./intro` from `shows/main` → `shows/intro`,
///   `../jingles/id` from `shows/main` → `jingles/id`. `..` may climb up to
///   the playlist root, never above it (loud error).
/// - Any other ref is **root-relative**, exactly as before ([`normalize_ref`]).
///   No implicit fallback between the two: the syntax says which one it is,
///   so a ref never silently resolves to a different playlist.
///
/// Same canonicalization as `normalize_ref` otherwise (`\` → `/`, `.toml`
/// stripped, lowercased, empty/`.` segments dropped).
pub fn resolve_member_ref(group_key: &str, raw: &str) -> Result<String, String> {
    let unified = raw.replace('\\', "/");
    let relative = unified == "."
        || unified == ".."
        || unified.starts_with("./")
        || unified.starts_with("../");
    if !relative {
        return normalize_ref(raw);
    }
    let trimmed = unified.strip_suffix(".toml").unwrap_or(&unified);

    // A member ref must NAME a playlist, not designate a directory: `./`,
    // `.`, `..`, `./x/..` all end on a directory (the group's own, or a
    // parent) → loud error, never silently resolved to a same-named playlist.
    let last = trimmed
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .last();
    if !matches!(last, Some(s) if s != "..") {
        return Err(format!(
            "`{raw}` designates a directory, not a playlist (relative to group `{group_key}`)"
        ));
    }

    // Base = the group's directory: its key minus its own last segment.
    let mut segments: Vec<String> = group_key
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect();
    segments.pop();

    for seg in trimmed.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                if segments.pop().is_none() {
                    return Err(format!(
                        "`{raw}` climbs above the playlist root (relative to group `{group_key}`)"
                    ));
                }
            }
            s => segments.push(s.to_lowercase()),
        }
    }

    if segments.is_empty() {
        return Err(format!(
            "`{raw}` is an empty reference (relative to group `{group_key}`)"
        ));
    }
    Ok(segments.join("/"))
}

/// One entry to validate as part of the whole set: its canonical key (the
/// normalized relative path, from the file's location) and the parsed
/// playlist.
pub struct SetEntry {
    pub key: String,
    pub playlist: Playlist,
}

/// An error tied to one playlist within the set.
pub struct SetError {
    pub key: String,
    pub message: String,
}

/// Validate the whole set of playlists together: every group member `ref`
/// resolves to a known playlist, and the group graph has no cycle. Pure and
/// database-free — operates only on what was scanned from the files.
///
/// Best-effort spirit: collects *all* problems rather than stopping at the
/// first, so `sync` can report them together.
pub fn validate_set(entries: &[SetEntry]) -> Vec<SetError> {
    use std::collections::{HashMap, HashSet};

    let mut errors = Vec::new();

    // Index by canonical key for ref resolution.
    let known: HashSet<String> = entries.iter().map(|e| e.key.clone()).collect();

    // Build the group graph (edges = normalized member refs), reporting
    // unresolvable or unsafe refs as we go. Keys are owned Strings to keep
    // the recursive DFS below free of lifetime gymnastics.
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for e in entries {
        if e.playlist.selection.mode != Mode::Group {
            continue;
        }
        let mut targets = Vec::new();
        for m in &e.playlist.selection.members {
            match resolve_member_ref(&e.key, &m.r#ref) {
                Ok(key) => {
                    if !known.contains(&key) {
                        errors.push(SetError {
                            key: e.key.clone(),
                            message: format!("member `{}` refers to unknown playlist `{key}`", m.r#ref),
                        });
                    }
                    targets.push(key);
                }
                Err(msg) => {
                    errors.push(SetError { key: e.key.clone(), message: msg });
                }
            }
        }
        edges.insert(e.key.clone(), targets);
    }

    // Cycle detection over the group graph (DFS with a recursion stack).
    // Reports each node that sits on a cycle. Covers self-reference
    // (a group listing itself) and transitive loops (A→B→C→A).
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn dfs(
        node: &str,
        edges: &HashMap<String, Vec<String>>,
        marks: &mut HashMap<String, Mark>,
        on_cycle: &mut HashSet<String>,
    ) {
        marks.insert(node.to_string(), Mark::Visiting);
        if let Some(targets) = edges.get(node).cloned() {
            for t in &targets {
                match marks.get(t).copied() {
                    Some(Mark::Visiting) => {
                        // Back-edge: both ends are on a cycle.
                        on_cycle.insert(node.to_string());
                        on_cycle.insert(t.clone());
                    }
                    Some(Mark::Done) => {}
                    None => {
                        // Only recurse into targets that are themselves
                        // groups (present as graph nodes). A ref to a
                        // non-group leaf can't extend a cycle.
                        if edges.contains_key(t) {
                            dfs(t, edges, marks, on_cycle);
                        }
                    }
                }
            }
        }
        marks.insert(node.to_string(), Mark::Done);
    }

    let mut marks: HashMap<String, Mark> = HashMap::new();
    let mut on_cycle: HashSet<String> = HashSet::new();
    let node_keys: Vec<String> = edges.keys().cloned().collect();
    for node in &node_keys {
        if marks.get(node).is_none() {
            dfs(node, &edges, &mut marks, &mut on_cycle);
        }
    }
    for key in on_cycle {
        errors.push(SetError {
            key: key.clone(),
            message: "is part of a group cycle (a group cannot reference itself, directly or transitively)".to_string(),
        });
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    const DYNAMIC: &str = r#"
        name = "Hits récents"
        enabled = true
        [selection]
        mode = "dynamic"
        order = "shuffle"
        match = "all"
        [[selection.filter]]
        field = "year"
        op = ">="
        value = 2018
        [broadcast]
        limit = 15
        [broadcast.constraints]
        no_same_artist_within = "30m"
    "#;

    #[test]
    fn parses_and_validates_dynamic() {
        let pl = Playlist::parse(DYNAMIC).expect("should parse");
        assert_eq!(pl.name, "Hits récents");
        assert_eq!(pl.selection.mode, Mode::Dynamic);
        assert!(pl.id.is_none(), "hand-written file has no id yet");
        pl.validate().expect("should validate");
    }

    #[test]
    fn rejects_unknown_field() {
        let toml_str = r#"
            name = "Bad"
            oops = "unknown"
            [selection]
            mode = "dynamic"
        "#;
        assert!(Playlist::parse(toml_str).is_err());
    }

    #[test]
    fn rejects_take_outside_sequence_group() {
        let toml_str = r#"
            name = "Bad group"
            [selection]
            mode = "group"
            strategy = "weighted"
            members = [{ ref = "a", take = 3 }]
        "#;
        let pl = Playlist::parse(toml_str).expect("parses");
        assert!(pl.validate().is_err(), "take needs a sequence group");
    }

    #[test]
    fn rejects_bad_order_for_mode() {
        let toml_str = r#"
            name = "Bad order"
            [selection]
            mode = "static"
            order = "newest"
            files = ["a.mp3"]
        "#;
        let pl = Playlist::parse(toml_str).expect("parses");
        assert!(pl.validate().is_err(), "newest is not valid for static");
    }

    #[test]
    fn assign_id_injects_uuid_once() {
        let (rewritten, id1) = assign_id(DYNAMIC).expect("assigns id");
        assert!(rewritten.contains(&id1.to_string()));
        // Comments/structure preserved: name still there.
        assert!(rewritten.contains("Hits récents"));
        // Idempotent: running again keeps the same id.
        let (_again, id2) = assign_id(&rewritten).expect("idempotent");
        assert_eq!(id1, id2);
    }

    #[test]
    fn parses_and_validates_remote() {
        let toml_str = r#"
            name = "goodnight"
            [selection]
            mode = "remote"
            url = "http://nightmusic.live"
        "#;
        let pl = Playlist::parse(toml_str).expect("should parse");
        assert_eq!(pl.selection.mode, Mode::Remote);
        assert_eq!(pl.selection.url.as_deref(), Some("http://nightmusic.live"));
        pl.validate().expect("should validate");
    }

    #[test]
    fn rejects_remote_without_url() {
        let toml_str = r#"
            name = "goodnight"
            [selection]
            mode = "remote"
        "#;
        let pl = Playlist::parse(toml_str).expect("parses");
        assert!(pl.validate().is_err(), "remote needs a url");
    }

    #[test]
    fn rejects_url_on_non_remote() {
        let toml_str = r#"
            name = "Bad"
            [selection]
            mode = "dynamic"
            url = "http://nope.live"
        "#;
        let pl = Playlist::parse(toml_str).expect("parses");
        assert!(pl.validate().is_err(), "url only valid for remote");
    }

    // ----- dynamic filter shape validated upstream (at validate) -------

    #[test]
    fn validates_a_wellformed_genre_has_any() {
        let toml_str = r#"
            name = "Ok"
            [selection]
            mode = "dynamic"
            [[selection.filter]]
            field = "genre"
            op = "has_any"
            value = ["80s", "disco"]
        "#;
        validate_str(toml_str).expect("has_any with a string list is valid");
    }

    #[test]
    fn rejects_has_any_with_a_bare_string() {
        // The bug this guards: `has_any` wants a list; a bare string must fail
        // at validate time, not surface on air.
        let toml_str = r#"
            name = "Bad"
            [selection]
            mode = "dynamic"
            [[selection.filter]]
            field = "genre"
            op = "has_any"
            value = "80s,disco"
        "#;
        assert!(validate_str(toml_str).is_err(), "has_any needs an array, not a string");
    }

    #[test]
    fn rejects_unknown_filter_field_at_validate() {
        let toml_str = r#"
            name = "Bad"
            [selection]
            mode = "dynamic"
            [[selection.filter]]
            field = "rating"
            op = ">="
            value = 3
        "#;
        assert!(validate_str(toml_str).is_err(), "unknown filter field is rejected upstream");
    }

    #[test]
    fn rejects_unsupported_op_for_field_at_validate() {
        // genre only takes has/has_any/has_all/has_none — `eq` is a loud error.
        let toml_str = r#"
            name = "Bad"
            [selection]
            mode = "dynamic"
            [[selection.filter]]
            field = "genre"
            op = "eq"
            value = "jazz"
        "#;
        assert!(validate_str(toml_str).is_err(), "genre/eq is not a valid catalogue entry");
    }

    // ----- helpers -----------------------------------------------------

    /// Parse then validate in one go. Panics if parsing fails (that's a
    /// test bug, not the case under test); returns the validation Result.
    fn validate_str(toml_str: &str) -> Result<(), PlaylistError> {
        Playlist::parse(toml_str)
            .expect("test input should be parseable TOML")
            .validate()
    }

    // ----- valid cases, one per remaining mode -------------------------

    #[test]
    fn valid_static() {
        let toml_str = r#"
            name = "Jingles"
            [selection]
            mode = "static"
            order = "sequential"
            files = ["jingles/id-01.wav", "jingles/id-02.wav"]
        "#;
        validate_str(toml_str).expect("static with files is valid");
    }

    #[test]
    fn valid_queue() {
        let toml_str = r#"
            name = "Demandes"
            [selection]
            mode = "queue"
            order = "fifo"
            max_len = 20
        "#;
        validate_str(toml_str).expect("queue with fifo is valid");
    }

    #[test]
    fn valid_group_weighted() {
        let toml_str = r#"
            name = "Matinée"
            [selection]
            mode = "group"
            strategy = "weighted"
            members = [{ ref = "a", weight = 5 }, { ref = "b", weight = 2 }]
        "#;
        validate_str(toml_str).expect("weighted group with weights is valid");
    }

    #[test]
    fn valid_group_sequence_with_take() {
        let toml_str = r#"
            name = "Séquence"
            [selection]
            mode = "group"
            strategy = "sequence"
            members = [{ ref = "a", take = 3 }, { ref = "b", take = 1 }]
        "#;
        validate_str(toml_str).expect("sequence group with take is valid");
    }

    #[test]
    fn valid_group_shuffle_with_runtime() {
        let toml_str = r#"
            name = "Shuffle budgets"
            [selection]
            mode = "group"
            strategy = "shuffle"
            members = [{ ref = "rock", runtime = "20m" }, { ref = "pop", runtime = "20m" }]
        "#;
        validate_str(toml_str).expect("shuffle group with per-member runtime is valid");
    }

    #[test]
    fn valid_group_sequence_mixing_take_and_runtime() {
        let toml_str = r#"
            name = "Sequence budgets"
            [selection]
            mode = "group"
            strategy = "sequence"
            members = [{ ref = "rock", runtime = "20m" }, { ref = "jingle", take = 1 }]
        "#;
        validate_str(toml_str).expect("a sequence group may mix runtime and take across members");
    }

    #[test]
    fn rejects_take_and_runtime_on_same_member() {
        let toml_str = r#"
            name = "Both quotas"
            [selection]
            mode = "group"
            strategy = "sequence"
            members = [{ ref = "a", take = 2, runtime = "20m" }]
        "#;
        assert!(validate_str(toml_str).is_err(), "take and runtime are mutually exclusive");
    }

    #[test]
    fn rejects_runtime_outside_quota_group() {
        let toml_str = r#"
            name = "Runtime on weighted"
            [selection]
            mode = "group"
            strategy = "weighted"
            members = [{ ref = "a", runtime = "20m" }]
        "#;
        assert!(validate_str(toml_str).is_err(), "runtime needs a sequence or shuffle group");
    }

    #[test]
    fn rejects_bad_runtime_duration() {
        let toml_str = r#"
            name = "Bad runtime"
            [selection]
            mode = "group"
            strategy = "shuffle"
            members = [{ ref = "a", runtime = "20" }]
        "#;
        assert!(validate_str(toml_str).is_err(), "runtime must be a valid duration");
    }

    #[test]
    fn rejects_unplayed_only_without_dated_order() {
        let toml_str = r#"
            name = "Bad unplayed"
            [selection]
            mode = "dynamic"
            order = "shuffle"
            unplayed_only = true
        "#;
        assert!(
            validate_str(toml_str).is_err(),
            "unplayed_only needs order newest or oldest"
        );
    }

    #[test]
    fn valid_unplayed_only_with_oldest() {
        let toml_str = r#"
            name = "Feuilleton"
            [selection]
            mode = "dynamic"
            order = "oldest"
            order_by = "filename"
            unplayed_only = true
        "#;
        validate_str(toml_str).expect("oldest + unplayed_only is valid");
    }

    #[test]
    fn parse_duration_secs_grammar() {
        assert_eq!(parse_duration_secs("30s").unwrap(), 30);
        assert_eq!(parse_duration_secs("15m").unwrap(), 900);
        assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
        assert_eq!(parse_duration_secs("1d").unwrap(), 86_400);
        for bad in ["0m", "m", "15", "1x", "01m", "20 m", ""] {
            assert!(parse_duration_secs(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    // ----- group projection (preview) ----------------------------------

    fn project(toml_str: &str) -> Option<GroupProjection> {
        Playlist::parse(toml_str)
            .expect("test input parses")
            .project_group_members()
    }

    #[test]
    fn projects_sequence_runtime_offsets_and_breaks_on_take() {
        // runtime 20m → take 1 → runtime 20m : offsets [Some(0), None, None]
        // (the `take` in the middle breaks the chain — unknown duration).
        let g = project(
            r#"
                name = "Seq"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [
                  { ref = "rock",    runtime = "20m" },
                  { ref = "jingle",  take = 1 },
                  { ref = "pop",     runtime = "20m" },
                ]
            "#,
        )
        .expect("a sequence group projects");
        assert_eq!(g.strategy, Strategy::Sequence);
        assert_eq!(g.members.len(), 3);
        assert_eq!(g.members[0].quota, MemberQuota::Runtime(1200));
        assert_eq!(g.members[0].offset_secs, Some(0));
        assert_eq!(g.members[1].quota, MemberQuota::Take(1));
        assert_eq!(g.members[1].offset_secs, None);
        assert_eq!(g.members[2].quota, MemberQuota::Runtime(1200));
        assert_eq!(g.members[2].offset_secs, None, "a take breaks the offset chain");
    }

    #[test]
    fn projects_sequence_runtime_accumulates() {
        let g = project(
            r#"
                name = "Seq"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [
                  { ref = "a", runtime = "20m" },
                  { ref = "b", runtime = "10m" },
                  { ref = "c", runtime = "5m" },
                ]
            "#,
        )
        .unwrap();
        assert_eq!(g.members[0].offset_secs, Some(0));
        assert_eq!(g.members[1].offset_secs, Some(1200));
        assert_eq!(g.members[2].offset_secs, Some(1800));
    }

    #[test]
    fn projects_shuffle_without_any_offset() {
        let g = project(
            r#"
                name = "Shuf"
                [selection]
                mode = "group"
                strategy = "shuffle"
                members = [{ ref = "a", runtime = "20m" }, { ref = "b", runtime = "20m" }]
            "#,
        )
        .unwrap();
        assert_eq!(g.strategy, Strategy::Shuffle);
        assert!(g.members.iter().all(|m| m.offset_secs.is_none()), "shuffle order is unknown");
        assert_eq!(g.members[0].quota, MemberQuota::Runtime(1200));
    }

    #[test]
    fn projects_bare_member_as_take_one() {
        let g = project(
            r#"
                name = "Seq"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [{ ref = "a" }, { ref = "b" }]
            "#,
        )
        .unwrap();
        assert_eq!(g.members[0].quota, MemberQuota::Take(1));
        assert_eq!(g.members[0].offset_secs, None);
    }

    #[test]
    fn non_sequence_shuffle_groups_do_not_project() {
        // weighted group → no per-member take/runtime timeline.
        assert!(project(
            r#"
                name = "W"
                [selection]
                mode = "group"
                strategy = "weighted"
                members = [{ ref = "a", weight = 5 }]
            "#,
        )
        .is_none());
        // a leaf never projects.
        assert!(project(
            r#"
                name = "Leaf"
                [selection]
                mode = "dynamic"
            "#,
        )
        .is_none());
    }

    // ----- defaults ----------------------------------------------------

    #[test]
    fn enabled_defaults_to_true() {
        let toml_str = r#"
            name = "No enabled field"
            [selection]
            mode = "dynamic"
        "#;
        let pl = Playlist::parse(toml_str).expect("parses");
        assert!(pl.enabled, "enabled should default to true when absent");
    }

    // ----- validation failures we didn't cover yet ---------------------

    #[test]
    fn rejects_weight_outside_weighted_group() {
        let toml_str = r#"
            name = "Bad group"
            [selection]
            mode = "group"
            strategy = "sequence"
            members = [{ ref = "a", weight = 5 }]
        "#;
        assert!(validate_str(toml_str).is_err(), "weight needs a weighted group");
    }

    #[test]
    fn rejects_static_without_files() {
        let toml_str = r#"
            name = "Empty static"
            [selection]
            mode = "static"
        "#;
        assert!(validate_str(toml_str).is_err(), "static needs files");
    }

    #[test]
    fn rejects_group_without_members() {
        let toml_str = r#"
            name = "Empty group"
            [selection]
            mode = "group"
            strategy = "weighted"
        "#;
        assert!(validate_str(toml_str).is_err(), "group needs members");
    }

    #[test]
    fn rejects_group_without_strategy() {
        let toml_str = r#"
            name = "No strategy"
            [selection]
            mode = "group"
            members = [{ ref = "a" }]
        "#;
        assert!(validate_str(toml_str).is_err(), "group needs a strategy");
    }

    #[test]
    fn rejects_order_on_group() {
        let toml_str = r#"
            name = "Group with order"
            [selection]
            mode = "group"
            order = "shuffle"
            strategy = "weighted"
            members = [{ ref = "a", weight = 1 }]
        "#;
        assert!(validate_str(toml_str).is_err(), "a group must not carry order");
    }

    #[test]
    fn rejects_order_on_remote() {
        let toml_str = r#"
            name = "Remote with order"
            [selection]
            mode = "remote"
            order = "shuffle"
            url = "http://nightmusic.live"
        "#;
        assert!(validate_str(toml_str).is_err(), "a remote must not carry order");
    }

    #[test]
    fn rejects_bad_order_for_queue() {
        let toml_str = r#"
            name = "Bad queue order"
            [selection]
            mode = "queue"
            order = "shuffle"
        "#;
        assert!(validate_str(toml_str).is_err(), "shuffle is not valid for queue");
    }

    // ----- parse-level failures ----------------------------------------

    #[test]
    fn parse_rejects_malformed_toml() {
        // Syntactically broken (dangling `=`): must be a Parse error, and
        // crucially must NOT panic.
        let toml_str = r#"
            name =
            [selection]
            mode = "dynamic"
        "#;
        let res = Playlist::parse(toml_str);
        assert!(matches!(res, Err(PlaylistError::Parse(_))));
    }

    #[test]
    fn parse_rejects_unknown_enum_variant() {
        // `mode = "remoote"` is well-formed TOML but not a known Mode.
        let toml_str = r#"
            name = "Typo"
            [selection]
            mode = "remoote"
        "#;
        assert!(Playlist::parse(toml_str).is_err(), "unknown mode variant is rejected");
    }

    // ----- assign_id edge cases ----------------------------------------

    #[test]
    fn assign_id_preserves_existing_valid_uuid() {
        let with_id = r#"
            id = "b3c55b4e-168d-449e-9bd2-046b814cbbd9"
            name = "Already added"
            [selection]
            mode = "dynamic"
            [broadcast]
            type = "general"
        "#;
        let (rewritten, id) = assign_id(with_id).expect("keeps existing id");
        assert_eq!(id.to_string(), "b3c55b4e-168d-449e-9bd2-046b814cbbd9");
        // Idempotent: content unchanged when an id is already present.
        assert_eq!(rewritten, with_id);
    }

    #[test]
    fn assign_id_rejects_present_but_invalid_uuid() {
        let bad_id = r#"
            id = "not-a-uuid"
            name = "Bad id"
            [selection]
            mode = "dynamic"
            [broadcast]
            type = "general"
        "#;
        assert!(assign_id(bad_id).is_err(), "a present non-UUID id is a loud error");
    }

    // ----- normalize_ref ----------------------------------------------

    #[test]
    fn normalize_ref_canonicalizes() {
        assert_eq!(normalize_ref("nuit/goodnight").unwrap(), "nuit/goodnight");
        assert_eq!(normalize_ref("nuit/goodnight.toml").unwrap(), "nuit/goodnight");
        // Windows separator folded to `/`.
        assert_eq!(normalize_ref("nuit\\goodnight").unwrap(), "nuit/goodnight");
        // Case folded.
        assert_eq!(normalize_ref("Nuit/GoodNight").unwrap(), "nuit/goodnight");
        // Redundant segments collapsed.
        assert_eq!(normalize_ref("./nuit//goodnight").unwrap(), "nuit/goodnight");
    }

    #[test]
    fn normalize_ref_rejects_unsafe() {
        assert!(normalize_ref("../secret").is_err(), "no escaping the root");
        assert!(normalize_ref("/etc/passwd").is_err(), "no absolute path");
        assert!(normalize_ref("").is_err(), "no empty ref");
        assert!(normalize_ref("nuit/../../x").is_err(), "no `..` anywhere");
    }

    // ----- validate_set: refs + cycles --------------------------------

    /// Build a minimal group entry referring to the given members.
    fn group_entry(key: &str, refs: &[&str]) -> SetEntry {
        let members = refs
            .iter()
            .map(|r| format!("{{ ref = \"{r}\", weight = 1 }}"))
            .collect::<Vec<_>>()
            .join(", ");
        let toml_str = format!(
            r#"
                name = "{key}"
                [selection]
                mode = "group"
                strategy = "weighted"
                members = [{members}]
            "#
        );
        SetEntry {
            key: key.to_string(),
            playlist: Playlist::parse(&toml_str).expect("group parses"),
        }
    }

    /// A trivial leaf (non-group) entry, so refs can resolve to it.
    fn leaf_entry(key: &str) -> SetEntry {
        let toml_str = r#"
            name = "leaf"
            [selection]
            mode = "dynamic"
        "#;
        SetEntry {
            key: key.to_string(),
            playlist: Playlist::parse(toml_str).expect("leaf parses"),
        }
    }

    #[test]
    fn set_valid_group_resolves() {
        let entries = vec![
            leaf_entry("a"),
            leaf_entry("b"),
            group_entry("morning", &["a", "b"]),
        ];
        assert!(validate_set(&entries).is_empty(), "all refs resolve, no cycle");
    }

    #[test]
    fn set_reports_unknown_ref() {
        let entries = vec![leaf_entry("a"), group_entry("morning", &["a", "ghost"])];
        let errors = validate_set(&entries);
        assert!(
            errors.iter().any(|e| e.message.contains("ghost")),
            "the missing member must be reported"
        );
    }

    #[test]
    fn set_detects_self_reference() {
        let entries = vec![group_entry("loop", &["loop"])];
        let errors = validate_set(&entries);
        assert!(
            errors.iter().any(|e| e.message.contains("cycle")),
            "a group referencing itself is a cycle"
        );
    }

    #[test]
    fn set_detects_transitive_cycle() {
        // a -> b -> c -> a
        let entries = vec![
            group_entry("a", &["b"]),
            group_entry("b", &["c"]),
            group_entry("c", &["a"]),
        ];
        let errors = validate_set(&entries);
        assert!(
            errors.iter().any(|e| e.message.contains("cycle")),
            "A->B->C->A must be detected"
        );
    }

    // ----- resolve_member_ref (relative member refs) -------------------

    #[test]
    fn member_ref_without_dot_prefix_stays_root_relative() {
        // Backward compatible: a plain ref ignores the group's location.
        assert_eq!(resolve_member_ref("shows/main", "jingles/id").unwrap(), "jingles/id");
        assert_eq!(resolve_member_ref("shows/main", "Shows/Intro.toml").unwrap(), "shows/intro");
    }

    #[test]
    fn member_ref_dot_slash_is_relative_to_the_group_dir() {
        assert_eq!(resolve_member_ref("shows/main", "./intro").unwrap(), "shows/intro");
        assert_eq!(
            resolve_member_ref("homestone-chronicles/homestone-chronicles", "./Homestone-Chronicles-Intro.toml")
                .unwrap(),
            "homestone-chronicles/homestone-chronicles-intro"
        );
        // Windows separator folded.
        assert_eq!(resolve_member_ref("shows/main", ".\\sub\\x").unwrap(), "shows/sub/x");
        // A group at the root: `./x` is just `x`.
        assert_eq!(resolve_member_ref("main", "./x").unwrap(), "x");
    }

    #[test]
    fn member_ref_dot_dot_climbs_but_never_above_root() {
        assert_eq!(resolve_member_ref("a/b/g", "../jingles/id").unwrap(), "a/jingles/id");
        assert_eq!(resolve_member_ref("a/b/g", "../../x").unwrap(), "x");
        assert_eq!(resolve_member_ref("a/b/g", "./c/../d").unwrap(), "a/b/d");
        assert!(resolve_member_ref("a/g", "../../x").is_err(), "above the root");
        assert!(resolve_member_ref("g", "../x").is_err(), "above the root");
    }

    #[test]
    fn member_ref_relative_empty_is_rejected() {
        assert!(resolve_member_ref("shows/main", "./").is_err());
        assert!(resolve_member_ref("shows/main", ".").is_err());
        assert!(resolve_member_ref("shows/main", "..").is_err());
        // Ends on a directory even though it passes through a name.
        assert!(resolve_member_ref("a/b/g", "./c/..").is_err());
    }

    #[test]
    fn set_resolves_relative_member_refs_from_the_group_dir() {
        let entries = vec![
            leaf_entry("shows/intro"),
            leaf_entry("jingles/id"),
            group_entry("shows/main", &["./intro", "../jingles/id"]),
        ];
        assert!(validate_set(&entries).is_empty(), "relative refs resolve from shows/");
    }

    #[test]
    fn set_reports_unknown_relative_ref_with_its_resolved_key() {
        // `./intro` from shows/main is shows/intro — NOT the root `intro`.
        let entries = vec![leaf_entry("intro"), group_entry("shows/main", &["./intro"])];
        let errors = validate_set(&entries);
        assert!(
            errors.iter().any(|e| e.message.contains("shows/intro")),
            "no silent fallback to the root playlist"
        );
    }

    #[test]
    fn set_detects_a_cycle_through_a_relative_ref() {
        let entries = vec![
            group_entry("shows/a", &["./b"]),
            group_entry("shows/b", &["./a"]),
        ];
        let errors = validate_set(&entries);
        assert!(errors.iter().any(|e| e.message.contains("cycle")));
    }

    #[test]
    fn set_reports_a_relative_ref_climbing_above_root() {
        let entries = vec![group_entry("main", &["../x"])];
        let errors = validate_set(&entries);
        assert!(errors.iter().any(|e| e.message.contains("above the playlist root")));
    }

    #[test]
    fn set_case_insensitive_ref_resolves() {
        // ref written with different case than the key must still resolve.
        let entries = vec![leaf_entry("nuit/goodnight"), group_entry("g", &["Nuit/GoodNight"])];
        assert!(
            validate_set(&entries).is_empty(),
            "case-folded ref should resolve to the lowercased key"
        );
    }
}
