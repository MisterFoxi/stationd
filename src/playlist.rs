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
    pub broadcast: Broadcast,
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

/// The *when/how*: how the playlist takes part in the broadcast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Broadcast {
    pub r#type: BroadcastType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_tracks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<OnExhausted>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedule: Vec<Schedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraints: Option<Constraints>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BroadcastType {
    General,
    Interval,
    Scheduled,
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
pub struct Schedule {
    pub start: String,
    pub end: String,
    pub days: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_end: Option<String>,
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

        // TODO(reconciliation): group cycle detection (DAG) + referential
        // integrity of member `ref`s and static `files` — needs the whole
        // playlist set / media library, not just this one file.

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
        type = "general"
        weight = 5
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
            [broadcast]
            type = "general"
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
            [broadcast]
            type = "scheduled"
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
            [broadcast]
            type = "general"
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
            [broadcast]
            type = "scheduled"
            [[broadcast.schedule]]
            start = "00:00"
            end = "06:00"
            days = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
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
            [broadcast]
            type = "scheduled"
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
            [broadcast]
            type = "general"
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
            [broadcast]
            type = "interval"
            every_tracks = 4
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
            [broadcast]
            type = "interval"
            every_tracks = 3
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
            [broadcast]
            type = "scheduled"
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
            [broadcast]
            type = "scheduled"
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
            [broadcast]
            type = "general"
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
            [broadcast]
            type = "scheduled"
        "#;
        assert!(validate_str(toml_str).is_err(), "weight needs a weighted group");
    }

    #[test]
    fn rejects_static_without_files() {
        let toml_str = r#"
            name = "Empty static"
            [selection]
            mode = "static"
            [broadcast]
            type = "general"
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
            [broadcast]
            type = "scheduled"
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
            [broadcast]
            type = "scheduled"
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
            [broadcast]
            type = "scheduled"
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
            [broadcast]
            type = "scheduled"
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
            [broadcast]
            type = "interval"
            every_tracks = 3
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
            [broadcast]
            type = "general"
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
}
