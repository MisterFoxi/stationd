//! Local TOML authoring. Reuses the daemon's parser; never writes its index.
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, ensure, Context};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use stationd::playlist::{normalize_ref, Playlist};
use toml::{Table, Value};

#[derive(Default, Clone)]
pub struct Input {
    pub text: String,
    // Byte offset, always at a UTF-8 character boundary.
    pub cursor: usize,
}

impl Input {
    fn new(text: &str) -> Self {
        Self {
            text: text.into(),
            cursor: text.len(),
        }
    }

    fn insert(&mut self, text: &str) {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .last()
            .map_or(0, |(i, _)| i);
    }

    fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace => {
                let end = self.cursor;
                self.left();
                self.text.drain(self.cursor..end);
            }
            KeyCode::Delete => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.text.drain(self.cursor..self.cursor + c.len_utf8());
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string())
            }
            _ => {}
        }
    }
}

pub struct Field {
    pub key: String,
    pub label: String,
    pub choices: &'static [&'static str],
}

fn field(key: &str, label: &str, choices: &'static [&'static str]) -> Field {
    Field {
        key: key.into(),
        label: label.into(),
        choices,
    }
}

pub enum FormAction {
    None,
    Close,
    Save { path: PathBuf, toml: String },
}

pub struct PlaylistForm {
    pub root: PathBuf,
    pub page: usize,
    pub selection: ListState,
    pub inputs: BTreeMap<String, Input>,
    counts: BTreeMap<&'static str, usize>,
    pub preview: Option<String>,
    pub preview_scroll: u16,
    pub message: String,
    pub discard: bool,
    pub saving: bool,
    dirty: bool,
}

impl PlaylistForm {
    pub fn new(root: PathBuf) -> Self {
        let mut form = Self {
            root,
            page: 0,
            selection: ListState::default().with_selected(Some(0)),
            inputs: BTreeMap::new(),
            counts: BTreeMap::new(),
            preview: None,
            preview_scroll: 0,
            message: String::new(),
            discard: false,
            saving: false,
            dirty: false,
        };
        for (key, value) in [
            ("path", ""),
            ("name", ""),
            ("enabled", "true"),
            ("mode", "static"),
            ("static_order", "sequential"),
            ("dynamic_order", "shuffle"),
            ("match", "all"),
            ("order_by", "filename"),
            ("unplayed_only", "false"),
            ("url", ""),
            ("queue_order", "fifo"),
            ("max_len", ""),
            ("strategy", "sequence"),
            ("type", "general"),
            ("weight", "15"),
            ("limit", ""),
            ("every_tracks", ""),
            ("every_time", ""),
            ("repeat", ""),
            ("on_exhausted", ""),
            ("no_same_artist_within", ""),
            ("no_same_track_within", ""),
        ] {
            form.set(key, value);
        }
        for kind in ["file", "filter", "member"] {
            form.add_row(kind);
        }
        form
    }

    fn set(&mut self, key: &str, value: &str) {
        self.inputs.insert(key.into(), Input::new(value));
    }
    pub fn get(&self, key: &str) -> &str {
        self.inputs.get(key).map_or("", |v| v.text.as_str())
    }
    fn count(&self, kind: &str) -> usize {
        self.counts.get(kind).copied().unwrap_or(0)
    }

    fn add_row(&mut self, kind: &'static str) {
        let i = self.count(kind);
        let defaults: &[(&str, &str)] = match kind {
            "file" => &[("path", "")],
            "filter" => &[
                ("field", "path"),
                ("op", "prefix"),
                ("type", "text"),
                ("value", ""),
            ],
            "member" => &[("ref", ""), ("take", "1"), ("weight", "15")],
            "schedule" => &[
                ("start", ""),
                ("end", ""),
                ("days", "mon,tue,wed,thu,fri,sat,sun"),
                ("date_start", ""),
                ("date_end", ""),
            ],
            _ => return,
        };
        for (key, value) in defaults {
            self.set(&format!("{kind}.{i}.{key}"), value);
        }
        self.counts.insert(kind, i + 1);
    }

    fn remove_row(&mut self, kind: &'static str, index: usize) {
        let count = self.count(kind);
        if index >= count {
            return;
        }
        for i in index..count {
            let prefix = format!("{kind}.{i}.");
            self.inputs.retain(|key, _| !key.starts_with(&prefix));
            if i + 1 < count {
                let next = format!("{kind}.{}.", i + 1);
                let values: Vec<_> = self
                    .inputs
                    .iter()
                    .filter_map(|(key, value)| {
                        key.strip_prefix(&next)
                            .map(|suffix| (format!("{prefix}{suffix}"), value.clone()))
                    })
                    .collect();
                self.inputs.extend(values);
            }
        }
        self.counts.insert(kind, count - 1);
    }

    pub fn fields(&self) -> Vec<Field> {
        let mut fields = Vec::new();
        let mut add = |key: &str, label: &str, choices| fields.push(field(key, label, choices));
        match self.page {
            0 => {
                add("path", "File (relative, .toml)", &[]);
                add("name", "Name", &[]);
                add("enabled", "Enabled", &["true", "false"]);
                add(
                    "mode",
                    "Selection mode",
                    &["static", "dynamic", "remote", "queue", "group"],
                );
            }
            1 => match self.get("mode") {
                "static" => {
                    add("static_order", "Order", &["sequential", "shuffle"]);
                    for i in 0..self.count("file") {
                        add(
                            &format!("file.{i}.path"),
                            &format!("File {} (under media)", i + 1),
                            &[],
                        );
                    }
                }
                "dynamic" => {
                    add(
                        "dynamic_order",
                        "Order",
                        &["shuffle", "sequential", "newest", "oldest"],
                    );
                    add("match", "Match filters", &["all", "any"]);
                    if matches!(self.get("dynamic_order"), "newest" | "oldest") {
                        add("order_by", "Order by", &["filename", "mtime", "published"]);
                        add("unplayed_only", "Unplayed only", &["false", "true"]);
                    }
                    for i in 0..self.count("filter") {
                        // Field/op remain free strings: the current parser has no closed catalogue.
                        add(
                            &format!("filter.{i}.field"),
                            &format!("Filter {}: field", i + 1),
                            &[],
                        );
                        add(&format!("filter.{i}.op"), "  Operator", &[]);
                        add(
                            &format!("filter.{i}.type"),
                            "  Value type",
                            &["text", "integer", "number", "boolean", "texts", "integers"],
                        );
                        add(
                            &format!("filter.{i}.value"),
                            "  Value (lists: comma separated)",
                            &[],
                        );
                    }
                }
                "remote" => add("url", "Stream URL", &[]),
                "queue" => {
                    add("queue_order", "Order", &["fifo", "lifo"]);
                    add("max_len", "Maximum length (optional)", &[]);
                }
                "group" => {
                    add("strategy", "Strategy", &["sequence", "rotate", "weighted"]);
                    for i in 0..self.count("member") {
                        add(
                            &format!("member.{i}.ref"),
                            &format!("Member {}: playlist ref", i + 1),
                            &[],
                        );
                        match self.get("strategy") {
                            "sequence" => add(&format!("member.{i}.take"), "  Take", &[]),
                            "weighted" => add(&format!("member.{i}.weight"), "  Weight", &[]),
                            _ => {}
                        }
                    }
                }
                _ => {}
            },
            _ => {
                add(
                    "type",
                    "Broadcast type",
                    &["general", "interval", "scheduled"],
                );
                if self.get("type") == "general" {
                    add("weight", "Weight (optional)", &[]);
                }
                add("limit", "Limit (optional)", &[]);
                if self.get("type") == "interval" {
                    add("every_tracks", "Every N tracks (optional)", &[]);
                    add("every_time", "Every duration (optional)", &[]);
                }
                add("repeat", "Repeat (blank = omitted)", &["", "false", "true"]);
                add(
                    "on_exhausted",
                    "On exhausted (optional)",
                    &["", "fallthrough", "stop", "disable", "hold"],
                );
                add("no_same_artist_within", "Artist separation (optional)", &[]);
                add("no_same_track_within", "Track separation (optional)", &[]);
                if self.get("type") == "scheduled" {
                    for i in 0..self.count("schedule") {
                        for (key, label) in [
                            ("start", "start (HH:MM)"),
                            ("end", "end (HH:MM)"),
                            ("days", "days (comma separated)"),
                            ("date_start", "from date (optional)"),
                            ("date_end", "to date (optional)"),
                        ] {
                            add(
                                &format!("schedule.{i}.{key}"),
                                &format!("Window {}: {label}", i + 1),
                                &[],
                            );
                        }
                    }
                }
            }
        }
        fields
    }

    fn row_kind(&self) -> Option<&'static str> {
        match (self.page, self.get("mode"), self.get("type")) {
            (1, "static", _) => Some("file"),
            (1, "dynamic", _) => Some("filter"),
            (1, "group", _) => Some("member"),
            (2, _, "scheduled") => Some("schedule"),
            _ => None,
        }
    }

    pub fn hint(&self) -> &'static str {
        match self.row_kind() {
            Some("file") => "F2: add file | F3: remove selected file | Paths relative to media",
            Some("filter") => {
                "F2: add filter | F3: remove selected filter | Empty path/prefix matches all"
            }
            Some("member") => {
                "F2: add member | F3: remove selected member | Refs/cycles checked at sync"
            }
            Some("schedule") => {
                "F2: add window | F3: remove selected window | Existing playlist grammar"
            }
            _ => "Tab/Up/Down: field | Left/Right: choice or cursor | Ctrl-U: clear field",
        }
    }

    fn move_field(&mut self, delta: isize) {
        let len = self.fields().len();
        let next = (self.selection.selected().unwrap_or(0) as isize + delta)
            .rem_euclid(len.max(1) as isize) as usize;
        self.selection.select(Some(next));
    }

    pub fn paste(&mut self, text: &str) {
        if self.preview.is_some() || self.discard || self.saving {
            return;
        }
        let fields = self.fields();
        if let Some(f) = fields.get(self.selection.selected().unwrap_or(0)) {
            if f.choices.is_empty() {
                self.inputs.entry(f.key.clone()).or_default().insert(text);
                self.dirty = true;
            }
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> FormAction {
        let key = if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        } else {
            key
        };
        if self.saving {
            return FormAction::None;
        }
        if self.discard {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return FormAction::Close,
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => self.discard = false,
                _ => {}
            }
            return FormAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.preview.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::F(5) => self.preview = None,
                KeyCode::Down => self.preview_scroll = self.preview_scroll.saturating_add(1),
                KeyCode::Up => self.preview_scroll = self.preview_scroll.saturating_sub(1),
                KeyCode::PageDown => self.preview_scroll = self.preview_scroll.saturating_add(10),
                KeyCode::PageUp => self.preview_scroll = self.preview_scroll.saturating_sub(10),
                KeyCode::Char('s') if ctrl => match self.relative_path() {
                    Ok(path) => {
                        self.saving = true;
                        self.message = "Saving...".into();
                        return FormAction::Save {
                            path,
                            toml: self.preview.clone().unwrap(),
                        };
                    }
                    Err(e) => self.message = e.to_string(),
                },
                _ => {}
            }
            return FormAction::None;
        }
        match key.code {
            KeyCode::Esc => {
                if !self.dirty {
                    return FormAction::Close;
                }
                self.discard = true;
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Enter => self.move_field(1),
            KeyCode::BackTab | KeyCode::Up => self.move_field(-1),
            KeyCode::F(4) => {
                self.page = (self.page
                    + if key.modifiers.contains(KeyModifiers::SHIFT) {
                        2
                    } else {
                        1
                    })
                    % 3;
                self.selection = ListState::default().with_selected(Some(0));
            }
            KeyCode::F(5) | KeyCode::Char('s') if key.code == KeyCode::F(5) || ctrl => {
                match self.validated_toml().and_then(|toml| {
                    self.relative_path()?;
                    Ok(toml)
                }) {
                    Ok(toml) => {
                        self.preview = Some(toml);
                        self.preview_scroll = 0;
                        self.message.clear();
                    }
                    Err(e) => self.message = format!("{e:#}"),
                }
            }
            KeyCode::F(2) => {
                if let Some(kind) = self.row_kind() {
                    let prefix = format!("{kind}.{}.", self.count(kind));
                    self.add_row(kind);
                    self.selection.select(
                        self.fields()
                            .iter()
                            .position(|f| f.key.starts_with(&prefix)),
                    );
                    self.dirty = true;
                }
            }
            KeyCode::F(3) => {
                if let Some(kind) = self.row_kind() {
                    let fields = self.fields();
                    if let Some(f) = fields.get(self.selection.selected().unwrap_or(0)) {
                        if let Some(index) = f
                            .key
                            .strip_prefix(&format!("{kind}."))
                            .and_then(|s| s.split('.').next())
                            .and_then(|s| s.parse().ok())
                        {
                            self.remove_row(kind, index);
                            self.selection.select(Some(
                                self.selection
                                    .selected()
                                    .unwrap_or(0)
                                    .min(self.fields().len().saturating_sub(1)),
                            ));
                            self.dirty = true;
                        }
                    }
                }
            }
            _ => {
                let fields = self.fields();
                if let Some(f) = fields.get(self.selection.selected().unwrap_or(0)) {
                    let input = self.inputs.entry(f.key.clone()).or_default();
                    let before = input.text.clone();
                    if f.choices.is_empty() {
                        input.key(key);
                    } else if matches!(
                        key.code,
                        KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                    ) && !ctrl
                    {
                        let current = f.choices.iter().position(|v| *v == input.text).unwrap_or(0);
                        let delta = if key.code == KeyCode::Left {
                            f.choices.len() - 1
                        } else {
                            1
                        };
                        *input = Input::new(f.choices[(current + delta) % f.choices.len()]);
                    }
                    self.dirty |= input.text != before;
                }
            }
        }
        FormAction::None
    }

    pub fn relative_path(&self) -> anyhow::Result<PathBuf> {
        relative_path(self.get("path"))
    }

    pub fn validated_toml(&self) -> anyhow::Result<String> {
        ensure!(!self.get("name").trim().is_empty(), "Name is required");
        let mut doc = Table::new();
        put(&mut doc, "name", self.get("name"));
        doc.insert(
            "enabled".into(),
            Value::Boolean(self.get("enabled") == "true"),
        );
        let mut sel = Table::new();
        put(&mut sel, "mode", self.get("mode"));
        match self.get("mode") {
            "static" => {
                put(&mut sel, "order", self.get("static_order"));
                let mut files = Vec::new();
                for i in 0..self.count("file") {
                    let value = self.get(&format!("file.{i}.path"));
                    ensure!(
                        !value.trim().is_empty(),
                        "File {} is empty (F3 removes a row)",
                        i + 1
                    );
                    files.push(Value::String(value.into()));
                }
                sel.insert("files".into(), Value::Array(files));
            }
            "dynamic" => {
                put(&mut sel, "order", self.get("dynamic_order"));
                put(&mut sel, "match", self.get("match"));
                if matches!(self.get("dynamic_order"), "newest" | "oldest") {
                    put(&mut sel, "order_by", self.get("order_by"));
                    sel.insert(
                        "unplayed_only".into(),
                        Value::Boolean(self.get("unplayed_only") == "true"),
                    );
                }
                let mut filters = Vec::new();
                for i in 0..self.count("filter") {
                    let get = |key| self.get(&format!("filter.{i}.{key}"));
                    ensure!(
                        !get("field").trim().is_empty() && !get("op").trim().is_empty(),
                        "Filter {} needs a field and operator",
                        i + 1
                    );
                    let mut filter = Table::new();
                    put(&mut filter, "field", get("field"));
                    put(&mut filter, "op", get("op"));
                    filter.insert(
                        "value".into(),
                        filter_value(get("type"), get("value"))
                            .with_context(|| format!("Filter {} value", i + 1))?,
                    );
                    filters.push(Value::Table(filter));
                }
                sel.insert("filter".into(), Value::Array(filters));
            }
            "remote" => {
                ensure!(!self.get("url").trim().is_empty(), "Stream URL is required");
                put(&mut sel, "url", self.get("url"));
            }
            "queue" => {
                put(&mut sel, "order", self.get("queue_order"));
                optional_number(&mut sel, "max_len", self.get("max_len"))?;
            }
            "group" => {
                put(&mut sel, "strategy", self.get("strategy"));
                let mut members = Vec::new();
                for i in 0..self.count("member") {
                    let get = |key| self.get(&format!("member.{i}.{key}"));
                    normalize_ref(get("ref"))
                        .map_err(anyhow::Error::msg)
                        .with_context(|| format!("Member {}", i + 1))?;
                    let mut member = Table::new();
                    put(&mut member, "ref", get("ref"));
                    match self.get("strategy") {
                        "sequence" => optional_number(&mut member, "take", get("take"))?,
                        "weighted" => optional_number(&mut member, "weight", get("weight"))?,
                        _ => {}
                    }
                    members.push(Value::Table(member));
                }
                sel.insert("members".into(), Value::Array(members));
            }
            _ => bail!("Unknown mode"),
        }
        doc.insert("selection".into(), Value::Table(sel));
        let mut broadcast = Table::new();
        put(&mut broadcast, "type", self.get("type"));
        optional_number(&mut broadcast, "limit", self.get("limit"))?;
        if self.get("type") == "general" {
            optional_number(&mut broadcast, "weight", self.get("weight"))?;
        }
        if self.get("type") == "interval" {
            optional_number(&mut broadcast, "every_tracks", self.get("every_tracks"))?;
            optional_string(&mut broadcast, "every_time", self.get("every_time"));
        }
        if !self.get("repeat").is_empty() {
            broadcast.insert(
                "repeat".into(),
                Value::Boolean(self.get("repeat") == "true"),
            );
        }
        optional_string(&mut broadcast, "on_exhausted", self.get("on_exhausted"));
        let mut constraints = Table::new();
        for key in ["no_same_artist_within", "no_same_track_within"] {
            optional_string(&mut constraints, key, self.get(key));
        }
        if !constraints.is_empty() {
            broadcast.insert("constraints".into(), Value::Table(constraints));
        }
        if self.get("type") == "scheduled" && self.count("schedule") > 0 {
            let mut schedules = Vec::new();
            for i in 0..self.count("schedule") {
                let get = |key| self.get(&format!("schedule.{i}.{key}"));
                let mut schedule = Table::new();
                for key in ["start", "end"] {
                    ensure!(!get(key).is_empty(), "Window {}: {key} is required", i + 1);
                    put(&mut schedule, key, get(key));
                }
                schedule.insert(
                    "days".into(),
                    Value::Array(
                        comma_list(get("days"))
                            .map(|v| Value::String(v.into()))
                            .collect(),
                    ),
                );
                for key in ["date_start", "date_end"] {
                    optional_string(&mut schedule, key, get(key));
                }
                schedules.push(Value::Table(schedule));
            }
            broadcast.insert("schedule".into(), Value::Array(schedules));
        }
        doc.insert("broadcast".into(), Value::Table(broadcast));
        let text = toml::to_string_pretty(&doc)?;
        let playlist = Playlist::parse(&text)?;
        playlist.validate()?;
        // No UUID: the normal add/sync path assigns it later.
        Ok(toml::to_string_pretty(&playlist)?)
    }
}

fn put(table: &mut Table, key: &str, value: &str) {
    table.insert(key.into(), Value::String(value.into()));
}
fn optional_string(table: &mut Table, key: &str, value: &str) {
    if !value.is_empty() {
        put(table, key, value);
    }
}
fn optional_number(table: &mut Table, key: &str, value: &str) -> anyhow::Result<()> {
    if !value.trim().is_empty() {
        let n: u32 = value
            .trim()
            .parse()
            .with_context(|| format!("{key}: expected an unsigned integer"))?;
        table.insert(key.into(), Value::Integer(i64::from(n)));
    }
    Ok(())
}
fn comma_list(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|s| !s.is_empty())
}
fn filter_value(kind: &str, value: &str) -> anyhow::Result<Value> {
    Ok(match kind {
        "text" => Value::String(value.into()),
        "integer" => Value::Integer(value.trim().parse().context("expected an integer")?),
        "number" => {
            let n: f64 = value.trim().parse().context("expected a number")?;
            ensure!(n.is_finite(), "expected a finite number");
            Value::Float(n)
        }
        "boolean" => Value::Boolean(value.trim().parse().context("expected true or false")?),
        "texts" => Value::Array(comma_list(value).map(|s| Value::String(s.into())).collect()),
        "integers" => Value::Array(
            comma_list(value)
                .map(|s| s.parse::<i64>().map(Value::Integer))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        _ => bail!("Unknown value type"),
    })
}

fn relative_path(raw: &str) -> anyhow::Result<PathBuf> {
    let raw = raw.trim().replace('\\', "/");
    ensure!(
        !raw.is_empty() && !raw.contains(':') && !raw.chars().any(char::is_control),
        "Enter a relative .toml filename"
    );
    let path = PathBuf::from(&raw);
    ensure!(
        path.components().all(|c| matches!(c, Component::Normal(_))),
        "File must stay inside the playlist directory (no absolute path or ..)"
    );
    ensure!(
        path.extension().and_then(|s| s.to_str()) == Some("toml"),
        "Filename must end with .toml"
    );
    normalize_ref(&raw).map_err(anyhow::Error::msg)?;
    Ok(path)
}

/// Configuration paths follow the daemon's existing CWD-relative semantics.
/// Called only for local authoring; browsing a remote daemon needs no config.
pub fn playlist_root(explicit: Option<&Path>, config: &Path) -> anyhow::Result<PathBuf> {
    let path = if let Some(path) = explicit {
        path.to_path_buf()
    } else {
        stationd::config::Config::load(config)
            .with_context(|| format!("Read {} or pass --playlist-root DIR", config.display()))?
            .playlist
            .path
    };
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}

/// Stage a complete file beside its destination, then publish without clobbering.
/// A temporary file has no .toml suffix, so playlist sync cannot read half a file.
pub fn save_new(root: &Path, relative: &Path, text: &str) -> anyhow::Result<PathBuf> {
    let relative = relative_path(relative.to_str().context("Filename is not UTF-8")?)?;
    let playlist = Playlist::parse(text)?;
    playlist.validate()?;
    std::fs::create_dir_all(root).with_context(|| format!("Create {}", root.display()))?;
    let root = root.canonicalize()?;
    let key = normalize_ref(relative.to_str().unwrap()).map_err(anyhow::Error::msg)?;
    // The daemon compares refs case-insensitively, even on a case-sensitive disk.
    for entry in walkdir::WalkDir::new(&root).follow_links(false) {
        let entry = entry?;
        if entry.path().extension().and_then(|v| v.to_str()) != Some("toml") {
            continue;
        }
        let rel = entry.path().strip_prefix(&root)?;
        if normalize_ref(&rel.to_string_lossy()).ok().as_deref() == Some(&key) {
            bail!("Playlist already exists: {}", entry.path().display());
        }
    }
    let mut parent = root.clone();
    if let Some(dirs) = relative.parent() {
        for component in dirs.components() {
            parent.push(component.as_os_str());
            match std::fs::symlink_metadata(&parent) {
                Ok(metadata) => ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "Not a regular directory: {}",
                    parent.display()
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&parent)?,
                Err(e) => return Err(e.into()),
            }
        }
    }
    let target = parent.join(relative.file_name().context("Missing filename")?);
    let mut builder = tempfile::Builder::new();
    builder.prefix(".stationd-").suffix(".tmp");
    // Same permissions as a normal new file, filtered by the user's umask.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut staged = builder.tempfile_in(&parent)?;
    staged.write_all(text.as_bytes())?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(&target)
        .map_err(|e| e.error)
        .with_context(|| {
            format!(
                "Create {} (existing files are never overwritten)",
                target.display()
            )
        })?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stationd::playlist::{Mode, Strategy};

    fn draft(mode: &str) -> PlaylistForm {
        let mut form = PlaylistForm::new(PathBuf::from("playlist"));
        form.set("path", "chronicles/episode.toml");
        form.set("name", "The \"HomeStone\" — été 🦊");
        form.set("mode", mode);
        form.set("file.0.path", "stories/part, 1 \"quoted\".mp3");
        form.set("url", "https://radio.example/live?name=\"test\"");
        form.set("member.0.ref", "chronicles/intro");
        form
    }

    fn parse(form: &PlaylistForm) -> Playlist {
        let text = form.validated_toml().unwrap();
        let playlist = Playlist::parse(&text).unwrap();
        playlist.validate().unwrap();
        assert!(playlist.id.is_none());
        assert_eq!(playlist.name, form.get("name"));
        playlist
    }

    #[test]
    fn all_modes_roundtrip_through_the_real_parser() {
        for (name, mode) in [
            ("static", Mode::Static),
            ("dynamic", Mode::Dynamic),
            ("remote", Mode::Remote),
            ("queue", Mode::Queue),
            ("group", Mode::Group),
        ] {
            let form = draft(name);
            let playlist = parse(&form);
            assert_eq!(playlist.selection.mode, mode);
            if mode == Mode::Static {
                assert_eq!(playlist.selection.files[0], form.get("file.0.path"));
            }
            if mode == Mode::Remote {
                assert!(playlist.selection.order.is_none());
            }
        }
    }

    #[test]
    fn dynamic_typed_filters_and_latest_episode() {
        let mut form = draft("dynamic");
        form.set("dynamic_order", "newest");
        form.set("unplayed_only", "true");
        form.set("filter.0.value", "thecronicle/");
        form.set("limit", "1");
        form.add_row("filter");
        form.set("filter.1.field", "year");
        form.set("filter.1.op", ">=");
        form.set("filter.1.type", "integer");
        form.set("filter.1.value", "2026");
        let playlist = parse(&form);
        assert_eq!(
            playlist.selection.filter[0].value.as_str(),
            Some("thecronicle/")
        );
        assert_eq!(playlist.selection.filter[1].value.as_integer(), Some(2026));
        assert_eq!(playlist.selection.unplayed_only, Some(true));
        assert_eq!(playlist.broadcast.limit, Some(1));
        form.set("filter.1.value", "not an integer");
        assert!(form.validated_toml().is_err());
    }

    #[test]
    fn changing_modes_and_strategy_omits_hidden_fields_but_preserves_draft() {
        let mut form = draft("group");
        form.add_row("member");
        form.set("member.1.ref", "chronicles/outro");
        let p = parse(&form);
        assert_eq!(p.selection.strategy, Some(Strategy::Sequence));
        assert_eq!(p.selection.members[0].take, Some(1));
        form.set("strategy", "weighted");
        let p = parse(&form);
        assert_eq!(p.selection.members[0].weight, Some(15));
        assert!(p.selection.members[0].take.is_none());
        form.set("mode", "remote");
        let p = parse(&form);
        assert!(p.selection.members.is_empty() && p.selection.strategy.is_none());
        form.set("mode", "group");
        assert_eq!(parse(&form).selection.members[1].r#ref, "chronicles/outro");
        form.remove_row("member", 0);
        assert_eq!(parse(&form).selection.members[0].r#ref, "chronicles/outro");
        form.remove_row("member", 0);
        assert!(form.validated_toml().is_err());
    }

    #[test]
    fn optional_broadcast_fields_and_windows_roundtrip() {
        let mut form = draft("queue");
        form.set("type", "interval");
        form.set("every_tracks", "4");
        form.set("max_len", "20");
        let p = parse(&form);
        assert_eq!(p.broadcast.every_tracks, Some(4));
        assert!(p.broadcast.weight.is_none());
        form.set("type", "scheduled");
        form.add_row("schedule");
        form.set("schedule.0.start", "20:00");
        form.set("schedule.0.end", "21:00");
        form.set("schedule.0.days", "mon, wed");
        let p = parse(&form);
        assert!(p.broadcast.every_tracks.is_none());
        assert_eq!(p.broadcast.schedule[0].days, vec!["mon", "wed"]);
    }

    #[test]
    fn editing_unicode_and_shortcut_letters_does_not_close_or_trigger_actions() {
        let mut form = draft("static");
        form.selection.select(Some(1));
        form.set("name", "");
        for c in "qarñs 🦊".chars() {
            assert!(matches!(
                form.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                FormAction::None
            ));
        }
        assert_eq!(form.get("name"), "qarñs 🦊");
        form.handle(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(form.get("name"), "qarñs ");
        form.handle(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        form.paste("é\n");
        assert_eq!(form.get("name"), "qarñsé ");
        form.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(form.discard);
        form.handle(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(!form.discard);
    }

    #[test]
    fn save_requires_preview_and_validation_and_can_return_to_editing() {
        let mut form = draft("static");
        let save = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(matches!(form.handle(save), FormAction::None));
        assert!(form.preview.is_some());
        form.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(form.preview.is_none());
        form.set("path", "../bad.toml");
        form.handle(save);
        assert!(form.preview.is_none());
        form.set("path", "valid.toml");
        form.handle(save);
        assert!(matches!(form.handle(save), FormAction::Save { .. }));
        assert!(form.saving);
        assert!(matches!(form.handle(save), FormAction::None));
    }

    #[test]
    fn atomic_save_keeps_existing_files_and_rejects_escaping_paths() {
        let dir = tempfile::tempdir().unwrap();
        let text = draft("static").validated_toml().unwrap();
        let root = dir.path().join("playlist");
        let path = save_new(&root, Path::new("shows/new.toml"), &text).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let different = draft("remote").validated_toml().unwrap();
        assert!(save_new(&root, Path::new("shows/new.toml"), &different).is_err());
        assert!(save_new(&root, Path::new("SHOWS/NEW.toml"), &different).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        for raw in [
            "../outside.toml",
            "/tmp/escape.toml",
            "C:\\escape.toml",
            "",
            "bad.txt",
        ] {
            assert!(save_new(&root, Path::new(raw), &text).is_err(), "{raw}");
        }
        assert!(save_new(&root, Path::new("broken.toml"), "name =").is_err());
        assert!(!root.join("broken.toml").exists());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_subdirectory_cannot_redirect_a_save() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(save_new(
            root.path(),
            Path::new("link/new.toml"),
            &draft("remote").validated_toml().unwrap()
        )
        .is_err());
        assert!(!outside.path().join("new.toml").exists());
    }
}
