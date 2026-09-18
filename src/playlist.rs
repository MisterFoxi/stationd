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
/// TOML value (string, number, or array) — validated against `field`/`op`
/// later, not here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub field: String,
    pub op: String,
    pub value: toml::Value,
}

/// A group member: a reference to another playlist, plus an optional
/// `weight` (weighted strategy) or `take` (sequence strategy).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take: Option<u32>,
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

        // `take` only makes sense in a `sequence` group; `weight` only in a
        // `weighted` group.
        for m in &self.selection.members {
            if m.take.is_some() && self.selection.strategy != Some(Strategy::Sequence) {
                return Err(err("`take` on a member requires a `sequence` group"));
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
            match normalize_ref(&m.r#ref) {
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
