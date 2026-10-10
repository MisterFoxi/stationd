//! Plugin-owned declarative tables. No plugin-specific SQL or schema in the TUI.
use super::stats_dashboard::{self, Dashboard};
use super::{
    next_owner, ops,
    stats_period::{self, DateOutcome, DatePicker},
};
use crate::action::PluginVerb;
use crate::{
    app::{AppEvent, Global},
    k, rpc,
    screen::{Availability, KeyHelp, Screen},
    store::Store,
    style::Styles,
    tr,
};
use anyhow::Error;
use rat_salsa::{Control, SalsaContext};
use ratatui_core::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::Widget,
};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui_widgets::{
    block::Block,
    borders::BorderType,
    paragraph::{Paragraph, Wrap},
    table::{Cell, Row, Table},
};
use stationd_proto::plugin::{PluginDbQueryResponse, PluginInfo, PluginTab, plugin_db_value::Kind};
use std::time::{Duration, Instant};

pub fn declared_tabs(plugins: &[PluginInfo]) -> Vec<(String, PluginTab)> {
    plugins
        .iter()
        .flat_map(|p| p.tabs.iter().map(|t| (p.name.clone(), t.clone())))
        .collect()
}

const TABLE_KEYS: &[KeyHelp] = &[
    (k!("key-f"), k!("stats-filter-help")),
    (k!("key-plugin-next"), k!("help-plugin-next")),
    (k!("key-plugin-previous"), k!("help-plugin-previous")),
    (k!("key-up-down"), k!("help-select")),
    (k!("key-plugin-columns"), k!("help-plugin-columns")),
    (k!("key-r"), k!("help-reload")),
];
const VISUAL_KEYS: &[KeyHelp] = &[
    (k!("key-f"), k!("stats-filter-help")),
    (k!("key-v"), k!("stats-view-help")),
    (k!("key-h"), k!("stats-panels-help")),
    (k!("key-plugin-next"), k!("help-plugin-next")),
    (k!("key-plugin-previous"), k!("help-plugin-previous")),
    (k!("key-up-down"), k!("help-select")),
    (k!("key-plugin-columns"), k!("help-plugin-columns")),
    (k!("key-r"), k!("help-reload")),
];
const CATALOG_KEYS: &[KeyHelp] = &[
    (k!("key-up-down"), k!("help-select")),
    (k!("key-enter"), k!("help-plugin-open")),
    (k!("key-s"), k!("plugin-verb-start")),
    (k!("key-x"), k!("plugin-verb-stop")),
    (k!("key-r"), k!("plugin-verb-restart")),
    (k!("key-l"), k!("plugin-verb-reload")),
];

#[derive(Default)]
pub struct Catalog {
    selected: usize,
}
impl Screen for Catalog {
    fn title(&self) -> String {
        tr!("screen-plugins")
    }
    fn help(&self) -> &'static [KeyHelp] {
        CATALOG_KEYS
    }
    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        let AppEvent::Event(Event::Key(key)) = event else {
            return Ok(Control::Continue);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(Control::Continue);
        }
        let plugins = ctx.store.plugins.value.as_deref().unwrap_or_default();
        self.selected = self.selected.min(plugins.len().saturating_sub(1));
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(plugins.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(p) = plugins.get(self.selected)
                    && let Some(tab) = p.tabs.first()
                {
                    let index = declared_tabs(plugins)
                        .iter()
                        .position(|(name, t)| name == &p.name && t.id == tab.id);
                    if let Some(index) = index {
                        ctx.switch_to(super::BUILTIN_COUNT + index, None);
                    }
                }
            }
            KeyCode::Char(c @ ('s' | 'x' | 'r' | 'l')) => {
                if let Some(p) = plugins.get(self.selected) {
                    let verb = match c {
                        's' => PluginVerb::Start,
                        'x' => PluginVerb::Stop,
                        'r' => PluginVerb::Restart,
                        _ => PluginVerb::Reload,
                    };
                    ctx.open(ops::plugin(&p.name.clone(), verb));
                }
            }
            _ => return Ok(Control::Continue),
        }
        Ok(Control::Changed)
    }
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        let [notice, body] =
            Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        let message = ctx
            .store
            .plugins
            .error
            .clone()
            .unwrap_or_else(|| tr!("plugins-directory"));
        Paragraph::new(message)
            .style(if ctx.store.plugins.error.is_some() {
                s.error()
            } else {
                s.muted()
            })
            .wrap(Wrap { trim: false })
            .render(notice, buf);
        let plugins = ctx.store.plugins.value.as_deref().unwrap_or_default();
        self.selected = self.selected.min(plugins.len().saturating_sub(1));
        if plugins.is_empty() {
            Paragraph::new(tr!("control-plugins-none"))
                .style(s.muted())
                .render(body, buf);
            return Ok(());
        }
        let start = self
            .selected
            .saturating_sub(body.height.saturating_sub(2) as usize);
        let rows = plugins.iter().enumerate().skip(start).map(|(i, p)| {
            let row = Row::new(vec![
                p.name.clone(),
                p.state.clone(),
                p.capabilities.join(", "),
                p.tabs
                    .iter()
                    .map(|t| t.title.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
                p.reason.clone(),
            ]);
            if i == self.selected {
                row.style(s.tab_active())
            } else {
                row
            }
        });
        Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Length(12),
                Constraint::Percentage(20),
                Constraint::Percentage(25),
                Constraint::Fill(1),
            ],
        )
        .header(
            Row::new(vec![
                tr!("control-col-plugin"),
                tr!("control-col-state"),
                tr!("plugins-capabilities"),
                tr!("plugins-tabs"),
                tr!("control-col-reason"),
            ])
            .style(s.label()),
        )
        .column_spacing(1)
        .render(body, buf);
        Ok(())
    }
}

pub struct PluginTable {
    name: String,
    tab: PluginTab,
    owner: u64,
    request: u64,
    loading: bool,
    last: Option<Instant>,
    data: Option<PluginDbQueryResponse>,
    error: Option<String>,
    selected: usize,
    column: usize,
    filters: std::collections::HashMap<String, String>,
    filter_open: bool,
    filter_selected: usize,
    draft: Vec<String>,
    editing: Option<String>,
    filter_error: Option<String>,
    date_picker: Option<DatePicker>,
    date_defaults: Option<(String, String)>,
    last_preset: String,
    dashboard: bool,
    visual: Dashboard,
}
impl PluginTable {
    pub fn new(name: String, tab: PluginTab) -> Self {
        let dashboard = tab.has_dashboard;
        Self {
            name,
            tab,
            owner: next_owner(),
            request: 0,
            loading: false,
            last: None,
            data: None,
            error: None,
            selected: 0,
            column: 0,
            filters: Default::default(),
            filter_open: false,
            filter_selected: 0,
            draft: Vec::new(),
            editing: None,
            filter_error: None,
            date_picker: None,
            date_defaults: None,
            last_preset: "30 jours".into(),
            dashboard,
            visual: Dashboard::default(),
        }
    }
    fn draft_value(&self, key: &str) -> &str {
        self.tab
            .filters
            .iter()
            .position(|f| f.key == key)
            .map(|i| self.draft[i].as_str())
            .unwrap_or("")
    }
    fn open_date_picker(&mut self) {
        let f = &self.tab.filters[self.filter_selected];
        let defaults =
            stats_period::preset_bounds(&self.last_preset, chrono::Utc::now().naive_utc());
        let inactive = matches!(f.key.as_str(), "from" | "to")
            && !self.draft_value("period").is_empty()
            && self.draft_value("period") != "Personnalisée";
        let value = if inactive || self.draft[self.filter_selected].is_empty() {
            if f.key == "to" {
                defaults.1.as_str()
            } else {
                defaults.0.as_str()
            }
        } else {
            self.draft[self.filter_selected].as_str()
        };
        self.date_picker = Some(DatePicker::new(value, f.label.clone(), f.key == "to"));
        self.date_defaults = Some(defaults);
    }
    fn accept_date(&mut self, value: String) {
        let key = self.tab.filters[self.filter_selected].key.clone();
        let custom = self.draft_value("period") == "Personnalisée";
        if matches!(key.as_str(), "from" | "to") {
            if let Some((from, to)) = &self.date_defaults {
                for (i, f) in self.tab.filters.iter().enumerate() {
                    if f.key == "period" && f.options.iter().any(|o| o == "Personnalisée") {
                        self.draft[i] = "Personnalisée".into();
                    }
                    if !custom || self.draft[i].is_empty() {
                        if f.key == "from" {
                            self.draft[i] = from.clone();
                        }
                        if f.key == "to" {
                            self.draft[i] = to.clone();
                        }
                    }
                }
            }
        }
        self.draft[self.filter_selected] = value;
        self.date_picker = None;
        self.date_defaults = None;
        self.filter_error = None;
    }
    fn validate_draft(&self) -> Option<String> {
        for (f, v) in self.tab.filters.iter().zip(&self.draft) {
            if f.kind == "datetime"
                && !v.is_empty()
                && (v.len() != 19
                    || v.as_bytes()[10] != b' '
                    || v.parse::<jiff::civil::DateTime>().is_err())
            {
                return Some(tr!("stats-date-invalid", label = f.label.clone()));
            }
        }
        if self.draft_value("period") == "Personnalisée" {
            let (from, to) = (self.draft_value("from"), self.draft_value("to"));
            if from.is_empty() || to.is_empty() {
                return Some(tr!("stats-date-required"));
            }
            if from >= to {
                return Some(tr!("stats-date-order"));
            }
        }
        None
    }
    fn filter_key(&self, f: &stationd_proto::plugin::PluginTabFilter) -> (String, String, String) {
        (
            self.name.clone(),
            if f.shared {
                String::new()
            } else {
                self.tab.id.clone()
            },
            f.key.clone(),
        )
    }
    fn current_filters(&self, store: &Store) -> std::collections::HashMap<String, String> {
        self.tab
            .filters
            .iter()
            .map(|f| {
                (
                    f.key.clone(),
                    store
                        .plugin_filters
                        .get(&self.filter_key(f))
                        .unwrap_or(&f.default_value)
                        .clone(),
                )
            })
            .collect()
    }
    fn load(&mut self, ctx: &mut Global) {
        let values = self.current_filters(&ctx.store);
        if self.filters != values {
            self.filters = values;
            self.request += 1; // Ignore an in-flight response for the previous selection.
            self.loading = false;
            self.data = None;
            self.error = None;
            self.selected = 0;
            self.column = 0;
            self.visual = Dashboard::default();
        }
        if self.loading || self.availability(&ctx.store) != Availability::Available {
            return;
        }
        self.loading = true;
        self.request += 1;
        let (owner, request, channel, name, tab) = (
            self.owner,
            self.request,
            ctx.channel.clone(),
            self.name.clone(),
            self.tab.id.clone(),
        );
        let filters = self.filters.clone();
        let dashboard = self.dashboard;
        ctx.spawn_async(async move {
            let result = rpc::read_plugin_tab(channel, name, tab, filters, dashboard).await;
            Ok(Control::Event(AppEvent::PluginTable(
                owner, request, result,
            )))
        });
    }
}
fn cell(value: &stationd_proto::plugin::PluginDbValue) -> String {
    let text = match &value.kind {
        Some(Kind::Null(_)) | None => "—".into(),
        Some(Kind::Integer(v)) => v.to_string(),
        Some(Kind::Real(v)) => stats_dashboard::number(Some(*v), ""),
        Some(Kind::Text(v)) => v.clone(),
        Some(Kind::Blob(v)) => format!("[{} bytes]", v.len()),
    };
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
impl Screen for PluginTable {
    fn captures_text(&self) -> bool {
        self.filter_open
    }
    fn plugin_tab_key(&self) -> Option<(String, String)> {
        Some((self.name.clone(), self.tab.id.clone()))
    }
    fn title(&self) -> String {
        self.tab.title.clone()
    }
    fn help(&self) -> &'static [KeyHelp] {
        if self.tab.has_dashboard {
            VISUAL_KEYS
        } else {
            TABLE_KEYS
        }
    }
    fn availability(&self, store: &Store) -> Availability {
        if let Some(error) = &store.plugins.error {
            return Availability::Unavailable(error.clone());
        }
        match store
            .plugins
            .value
            .as_ref()
            .and_then(|p| p.iter().find(|p| p.name == self.name))
        {
            Some(p)
                if store.plugin_loaded(&self.name)
                    && p.tabs.iter().any(|t| t.id == self.tab.id) =>
            {
                Availability::Available
            }
            Some(p) => {
                Availability::Unavailable(format!("{}: {} {}", self.name, p.state, p.reason))
            }
            None => {
                Availability::Unavailable(tr!("planned-plugin-missing", plugin = self.name.clone()))
            }
        }
    }
    fn enter(&mut self, ctx: &mut Global) -> Result<(), Error> {
        self.load(ctx);
        Ok(())
    }
    fn reconnected(&mut self, ctx: &mut Global) -> Result<(), Error> {
        self.load(ctx);
        Ok(())
    }
    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        if self.filter_open {
            if let Some(picker) = &mut self.date_picker {
                if let AppEvent::Event(event) = event {
                    match picker.handle(event) {
                        DateOutcome::Selected(value) => self.accept_date(value),
                        DateOutcome::Cancel => {
                            self.date_picker = None;
                            self.date_defaults = None;
                        }
                        DateOutcome::Continue => {}
                    }
                    return Ok(Control::Changed);
                }
            }
            if let AppEvent::Event(Event::Key(key)) = event {
                if key.kind != KeyEventKind::Press {
                    return Ok(Control::Continue);
                }
                if let Some(text) = &mut self.editing {
                    match key.code {
                        KeyCode::Esc => self.editing = None,
                        KeyCode::Enter => {
                            self.draft[self.filter_selected] = text.clone();
                            self.editing = None;
                        }
                        KeyCode::Backspace => {
                            text.pop();
                        }
                        KeyCode::Char(c)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                                && text.len() + c.len_utf8() <= 256 =>
                        {
                            text.push(c)
                        }
                        _ => {}
                    }
                } else {
                    let f = &self.tab.filters[self.filter_selected];
                    match key.code {
                        KeyCode::Esc => {
                            self.filter_open = false;
                            self.filter_error = None;
                        }
                        KeyCode::BackTab | KeyCode::Up => {
                            self.filter_selected = self.filter_selected.saturating_sub(1)
                        }
                        KeyCode::Tab | KeyCode::Down => {
                            self.filter_selected =
                                (self.filter_selected + 1).min(self.tab.filters.len() - 1)
                        }
                        KeyCode::Left | KeyCode::Right if f.kind == "choice" => {
                            let count = f.options.len();
                            if count > 0 {
                                let i = f
                                    .options
                                    .iter()
                                    .position(|o| o == &self.draft[self.filter_selected])
                                    .unwrap_or(0);
                                let i = if key.code == KeyCode::Left {
                                    (i + count - 1) % count
                                } else {
                                    (i + 1) % count
                                };
                                self.draft[self.filter_selected] = f.options[i].clone();
                                if f.key == "period"
                                    && self.draft[self.filter_selected] != "Personnalisée"
                                {
                                    self.last_preset = self.draft[self.filter_selected].clone();
                                }
                                self.filter_error = None;
                            }
                        }
                        KeyCode::Delete => {
                            self.draft[self.filter_selected] = f.default_value.clone()
                        }
                        KeyCode::Char(c) if f.kind == "choice" && ('1'..='9').contains(&c) => {
                            if let Some(value) = f.options.get((c as u8 - b'1') as usize) {
                                self.draft[self.filter_selected] = value.clone();
                                if f.key == "period" && value != "Personnalisée" {
                                    self.last_preset = value.clone();
                                }
                                self.filter_error = None;
                            }
                        }
                        KeyCode::Enter if f.kind == "datetime" => self.open_date_picker(),
                        KeyCode::Enter if f.kind == "text" => {
                            self.editing = Some(self.draft[self.filter_selected].clone())
                        }
                        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            // The host still validates all declared values before binding SQL.
                            if let Some(error) = self.validate_draft() {
                                self.filter_error = Some(error);
                            } else {
                                for (f, value) in self.tab.filters.iter().zip(&self.draft) {
                                    ctx.store
                                        .plugin_filters
                                        .insert(self.filter_key(f), value.clone());
                                }
                                self.filter_open = false;
                                self.filter_error = None;
                                self.load(ctx);
                            }
                        }
                        _ => {}
                    }
                }
                return Ok(Control::Changed);
            }
        }
        match event {
            AppEvent::PluginTable(owner, request, result)
                if *owner == self.owner && *request == self.request =>
            {
                self.loading = false;
                self.last = Some(Instant::now());
                match result {
                    Ok(data) => {
                        self.data = Some(data.clone());
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error.clone()),
                }
            }
            AppEvent::Timer(_)
                if self
                    .last
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) =>
            {
                self.load(ctx)
            }
            AppEvent::Event(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if self.dashboard {
                    if let Some(rows) = self
                        .data
                        .as_ref()
                        .and_then(|d| stats_dashboard::parse(d).ok())
                    {
                        if self.visual.handle(key, &rows) {
                            return Ok(Control::Changed);
                        }
                    }
                }
                let count = self.data.as_ref().map_or(0, |d| d.rows.len());
                let cols = self.data.as_ref().map_or(0, |d| d.columns.len());
                match key.code {
                    KeyCode::Char('f') if !self.tab.filters.is_empty() => {
                        let current = self.current_filters(&ctx.store);
                        self.draft = self
                            .tab
                            .filters
                            .iter()
                            .map(|f| current[&f.key].clone())
                            .collect();
                        self.filter_selected = 0;
                        self.filter_open = true;
                        self.filter_error = None;
                        let period = self.draft_value("period").to_string();
                        if period != "Personnalisée" && !period.is_empty() {
                            self.last_preset = period;
                        }
                    }
                    KeyCode::Char('v') if self.tab.has_dashboard => {
                        self.dashboard = !self.dashboard;
                        self.request += 1;
                        self.loading = false;
                        self.data = None;
                        self.error = None;
                        self.selected = 0;
                        self.column = 0;
                        self.load(ctx);
                    }
                    KeyCode::Char('v') => {
                        ctx.set_status(tr!("stats-no-dashboard"));
                    }
                    KeyCode::Char('r') => self.load(ctx),
                    KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                    KeyCode::Down => {
                        self.selected = (self.selected + 1).min(count.saturating_sub(1))
                    }
                    KeyCode::Home => self.selected = 0,
                    KeyCode::End => self.selected = count.saturating_sub(1),
                    KeyCode::Left => self.column = self.column.saturating_sub(1),
                    KeyCode::Right => self.column = (self.column + 1).min(cols.saturating_sub(1)),
                    _ => return Ok(Control::Continue),
                }
            }
            _ => return Ok(Control::Continue),
        }
        Ok(Control::Changed)
    }
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        if self.filter_open {
            if let Some(picker) = &mut self.date_picker {
                picker.render(area, buf, &s);
                return Ok(());
            }
            let [heading, fields, preview, footer] = Layout::vertical([
                Constraint::Length(2),
                Constraint::Fill(1),
                Constraint::Length(4),
                Constraint::Length(2),
            ])
            .areas(area);
            Paragraph::new(Line::styled(
                format!("{} · {}", self.tab.title, tr!("stats-filters")),
                s.title(),
            ))
            .render(heading, buf);
            let wide = fields.width >= 70;
            let (list, detail) = if wide {
                let [l, d] = Layout::horizontal([Constraint::Percentage(52), Constraint::Fill(1)])
                    .spacing(2)
                    .areas(fields);
                (l, Some(d))
            } else {
                (fields, None)
            };
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(tr!("stats-filter-fields"))
                .border_style(s.border());
            let inner = block.inner(list);
            block.render(list, buf);
            let visible = (inner.height as usize / 2).max(1);
            let first = self.filter_selected.saturating_sub(visible - 1);
            for (i, f) in self
                .tab
                .filters
                .iter()
                .enumerate()
                .skip(first)
                .take(visible)
            {
                let selected = i == self.filter_selected;
                let value = if selected {
                    self.editing.as_ref().unwrap_or(&self.draft[i])
                } else {
                    &self.draft[i]
                };
                let inactive = matches!(f.key.as_str(), "from" | "to")
                    && self.draft_value("period") != "Personnalisée";
                let value = if f.kind == "datetime" {
                    if inactive {
                        tr!("stats-date-auto")
                    } else {
                        stats_period::display(value)
                    }
                } else if value.is_empty() {
                    tr!("stats-filter-all")
                } else {
                    value.clone()
                };
                let row = Rect::new(inner.x, inner.y + ((i - first) * 2) as u16, inner.width, 2);
                let lines = vec![
                    Line::styled(
                        format!("{} {}", if selected { "▸" } else { " " }, f.label),
                        if selected { s.accent() } else { s.muted() },
                    ),
                    Line::styled(
                        format!(
                            "  {}{}",
                            value,
                            if selected && self.editing.is_some() {
                                "▏"
                            } else {
                                ""
                            }
                        ),
                        if selected { s.tab_active() } else { s.base() },
                    ),
                ];
                Paragraph::new(lines).render(row, buf);
            }
            if let Some(detail) = detail {
                let f = &self.tab.filters[self.filter_selected];
                let block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(f.label.clone())
                    .border_style(s.accent());
                let inner = block.inner(detail);
                block.render(detail, buf);
                let lines = if f.kind == "choice" {
                    f.options
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            Line::styled(
                                format!(
                                    " {}  {} {}",
                                    i + 1,
                                    if v == &self.draft[self.filter_selected] {
                                        "●"
                                    } else {
                                        "○"
                                    },
                                    v
                                ),
                                if v == &self.draft[self.filter_selected] {
                                    s.tab_active()
                                } else {
                                    s.base()
                                },
                            )
                        })
                        .collect::<Vec<_>>()
                } else if f.kind == "datetime" {
                    vec![
                        Line::styled(tr!("stats-open-calendar"), s.accent()),
                        Line::from(""),
                        Line::from(tr!("stats-calendar-description")),
                    ]
                } else {
                    vec![Line::from(tr!("stats-open-text"))]
                };
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .render(inner, buf);
            }
            let period = self.draft_value("period");
            let (from, to) = if period == "Personnalisée" {
                (
                    self.draft_value("from").to_string(),
                    self.draft_value("to").to_string(),
                )
            } else {
                stats_period::preset_bounds(period, chrono::Utc::now().naive_utc())
            };
            let msg = self.filter_error.clone().or_else(|| self.validate_draft());
            let title = if msg.is_some() {
                tr!("stats-period-check")
            } else {
                tr!("stats-period-preview")
            };
            let lines = if let Some(msg) = msg {
                vec![Line::styled(msg, s.error())]
            } else {
                vec![
                    Line::from(format!(
                        "{} {}",
                        tr!("stats-period-from"),
                        stats_period::display(&from)
                    )),
                    Line::from(format!(
                        "{} {}",
                        tr!("stats-period-to"),
                        stats_period::display(&to)
                    )),
                ]
            };
            Paragraph::new(lines)
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .title(title)
                        .border_style(s.border()),
                )
                .render(preview, buf);
            Paragraph::new(tr!("stats-filter-keys"))
                .style(s.muted())
                .wrap(Wrap { trim: false })
                .render(footer, buf);
            return Ok(());
        }
        if self.dashboard {
            let [header, body] =
                Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(area);
            let unavailable = match self.availability(&ctx.store) {
                Availability::Unavailable(e) => Some(e),
                _ => None,
            };
            let error = unavailable.as_ref().or(self.error.as_ref());
            let mut lines = vec![Line::styled(
                format!("{} · {}", self.tab.title, tr!("stats-dashboard")),
                s.title(),
            )];
            let filters = self
                .tab
                .filters
                .iter()
                .filter(|f| !matches!(f.key.as_str(), "from" | "to"))
                .filter_map(|f| {
                    let value = self.filters.get(&f.key).unwrap_or(&f.default_value);
                    if value.is_empty() {
                        None
                    } else if f.kind == "choice" {
                        Some(value.clone())
                    } else {
                        Some(format!("{}: {}", f.label, value))
                    }
                })
                .collect::<Vec<_>>()
                .join(" · ");
            lines.push(Line::styled(format!("[f] {filters}"), s.muted()));
            let rows = self.data.as_ref().map(stats_dashboard::parse);
            if let Some(error) = error {
                lines.push(Line::styled(
                    format!("{}: {error}", tr!("plugins-stale")),
                    s.error(),
                ));
            } else if self.loading {
                lines.push(Line::styled(tr!("plugins-loading"), s.muted()));
            } else if let Some(Ok(rows)) = &rows {
                if let Some(d) = rows.iter().find(|r| r.section == "period") {
                    lines.push(Line::styled(
                        format!(
                            "{} · {} → {} UTC · {}",
                            d.scope,
                            d.label,
                            d.bucket,
                            tr!("stats-period-exclusive")
                        ),
                        s.label(),
                    ));
                }
            }
            Paragraph::new(lines).render(header, buf);
            match rows {
                Some(Ok(rows)) => self.visual.render(
                    body,
                    buf,
                    &s,
                    &rows,
                    self.filters
                        .get("grouping")
                        .map(String::as_str)
                        .unwrap_or("Total"),
                ),
                Some(Err(e)) => Paragraph::new(e).style(s.error()).render(body, buf),
                None => {}
            }
            return Ok(());
        }
        let [notice, body, detail] = Layout::vertical([
            Constraint::Length(if self.tab.filters.is_empty() { 3 } else { 6 }),
            Constraint::Fill(1),
            Constraint::Length(4),
        ])
        .areas(area);
        let unavailable = match self.availability(&ctx.store) {
            Availability::Unavailable(e) => Some(e),
            _ => None,
        };
        let error = unavailable.as_ref().or(self.error.as_ref());
        let mut lines = vec![Line::from(format!(
            "{} · {}",
            self.tab.title,
            tr!("stats-table")
        ))];
        lines.push(Line::from(self.tab.description.clone()));
        if !self.tab.filters.is_empty() {
            lines.push(Line::from(format!(
                "[f] Filtres · {}",
                self.tab
                    .filters
                    .iter()
                    .map(|f| format!(
                        "{}: {}",
                        f.label,
                        self.filters.get(&f.key).unwrap_or(&f.default_value)
                    ))
                    .collect::<Vec<_>>()
                    .join(" · ")
            )));
        }
        if let Some(error) = error {
            lines.push(Line::styled(
                format!("{}: {error}", tr!("plugins-stale")),
                s.error(),
            ));
        } else if self.loading {
            lines.push(Line::styled(tr!("plugins-loading"), s.muted()));
        } else if let Some(data) = &self.data {
            lines.push(Line::styled(
                tr!("plugins-row-count", count = data.rows.len()),
                s.muted(),
            ));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(notice, buf);
        let Some(data) = &self.data else {
            return Ok(());
        };
        self.selected = self.selected.min(data.rows.len().saturating_sub(1));
        self.column = self.column.min(data.columns.len().saturating_sub(1));
        if data.rows.is_empty() {
            Paragraph::new(tr!("plugins-empty"))
                .style(s.muted())
                .render(body, buf);
            return Ok(());
        }
        let cols =
            ((body.width as usize / 18).max(1)).min(data.columns.len().saturating_sub(self.column));
        let start = self
            .selected
            .saturating_sub(body.height.saturating_sub(2) as usize);
        let rows = data.rows.iter().enumerate().skip(start).map(|(i, row)| {
            let row = Row::new(
                row.values
                    .iter()
                    .skip(self.column)
                    .take(cols)
                    .enumerate()
                    .map(|(j, v)| {
                        let text = if data
                            .columns
                            .get(self.column + j)
                            .is_some_and(|c| c == "Duree_s")
                        {
                            match &v.kind {
                                Some(Kind::Integer(n)) => {
                                    stats_dashboard::number(Some(*n as f64), "s")
                                }
                                _ => cell(v),
                            }
                        } else {
                            cell(v)
                        };
                        let numeric = matches!(v.kind, Some(Kind::Integer(_) | Kind::Real(_)));
                        Cell::from(ratatui_core::text::Text::from(text).alignment(if numeric {
                            ratatui_core::layout::Alignment::Right
                        } else {
                            ratatui_core::layout::Alignment::Left
                        }))
                    }),
            );
            if i == self.selected {
                row.style(s.tab_active())
            } else {
                row
            }
        });
        let header = Row::new(
            data.columns
                .iter()
                .skip(self.column)
                .take(cols)
                .map(|c| c.replace("Duree_s", "Durée").replace('_', " ")),
        )
        .style(s.label());
        Table::new(rows, vec![Constraint::Fill(1); cols])
            .header(header)
            .column_spacing(2)
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(tr!("stats-table"))
                    .border_style(s.border()),
            )
            .render(body, buf);
        if let Some(row) = data.rows.get(self.selected) {
            let text = data
                .columns
                .iter()
                .zip(&row.values)
                .map(|(k, v)| format!("{k}: {}", cell(v)))
                .collect::<Vec<_>>()
                .join(" · ");
            Paragraph::new(text)
                .style(s.muted())
                .wrap(Wrap { trim: false })
                .render(detail, buf);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_tabs_by_plugin_identity_not_plugin_kind() {
        let tab = PluginTab {
            id: "same".into(),
            title: "Audience".into(),
            ..Default::default()
        };
        let plugins = vec![
            PluginInfo {
                name: "renamed".into(),
                tabs: vec![tab.clone()],
                ..Default::default()
            },
            PluginInfo {
                name: "other".into(),
                tabs: vec![tab],
                ..Default::default()
            },
        ];
        let tabs = declared_tabs(&plugins);
        assert_eq!(tabs.len(), 2);
        assert_ne!(tabs[0].0, tabs[1].0);
    }
    #[test]
    fn table_cells_do_not_pass_control_characters_to_terminal() {
        let value = stationd_proto::plugin::PluginDbValue {
            kind: Some(Kind::Text("a\n\u{1b}b".into())),
        };
        assert_eq!(cell(&value), "a  b");
    }
}

#[cfg(test)]
mod table_response_tests {
    use super::*;
    #[tokio::test]
    async fn failed_refresh_keeps_rows_and_ignores_other_requesters() {
        let args = crate::Args {
            addr: "http://127.0.0.1:50051".into(),
            lang: None,
            theme: "Imperial".into(),
            list_themes: false,
        };
        let channel = rpc::lazy_channel(&args.addr).unwrap();
        let mut ctx = Global::new(&args, channel.clone(), channel);
        let tab = PluginTab {
            id: "plays".into(),
            title: "Diffusions".into(),
            ..Default::default()
        };
        ctx.store.plugins.value = Some(vec![PluginInfo {
            name: "stats".into(),
            state: "loaded".into(),
            tabs: vec![tab.clone()],
            ..Default::default()
        }]);
        let mut screen = PluginTable::new("stats".into(), tab);
        let data = PluginDbQueryResponse {
            columns: vec!["Media".into()],
            rows: vec![stationd_proto::plugin::PluginDbRow {
                values: vec![stationd_proto::plugin::PluginDbValue {
                    kind: Some(Kind::Text("song.mp3".into())),
                }],
            }],
        };
        let _ = screen
            .event(
                &AppEvent::PluginTable(screen.owner + 1, 0, Ok(data.clone())),
                &mut ctx,
            )
            .unwrap();
        assert!(screen.data.is_none());
        let _ = screen
            .event(&AppEvent::PluginTable(screen.owner, 0, Ok(data)), &mut ctx)
            .unwrap();
        let _ = screen
            .event(
                &AppEvent::PluginTable(screen.owner, 0, Err("offline".into())),
                &mut ctx,
            )
            .unwrap();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        screen.render(area, &mut buf, &mut ctx).unwrap();
        let text: String = (0..20)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .map(|pos| buf[pos].symbol())
            .collect();
        assert!(text.contains("offline"));
        assert!(text.contains("song.mp3"));
        assert_eq!(screen.data.as_ref().unwrap().rows.len(), 1);
        // A new filter selection must not display rows from the previous selection.
        screen
            .tab
            .filters
            .push(stationd_proto::plugin::PluginTabFilter {
                key: "period".into(),
                default_value: "24 h".into(),
                shared: true,
                ..Default::default()
            });
        let old_request = screen.request;
        ctx.store.plugins.value.as_mut().unwrap()[0].state = "stopped".into();
        screen.load(&mut ctx);
        assert!(screen.data.is_none());
        let _ = screen
            .event(
                &AppEvent::PluginTable(
                    screen.owner,
                    old_request,
                    Ok(PluginDbQueryResponse {
                        columns: vec!["old".into()],
                        rows: vec![],
                    }),
                ),
                &mut ctx,
            )
            .unwrap();
        assert!(screen.data.is_none());
        let key = AppEvent::Event(Event::Key(
            ratatui_crossterm::crossterm::event::KeyEvent::new(
                KeyCode::Char('f'),
                KeyModifiers::NONE,
            ),
        ));
        let _ = screen.event(&key, &mut ctx).unwrap();
        assert!(screen.captures_text());
        let mut buf = Buffer::empty(area);
        screen.render(area, &mut buf, &mut ctx).unwrap();
        let text: String = (0..20)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .map(|pos| buf[pos].symbol())
            .collect();
        assert!(text.contains("Ctrl+S"));
        // Switching representations must reject a late response from the old view.
        let escape = AppEvent::Event(Event::Key(
            ratatui_crossterm::crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        ));
        let _ = screen.event(&escape, &mut ctx).unwrap();
        screen.tab.has_dashboard = true;
        let before = screen.request;
        let toggle = AppEvent::Event(Event::Key(
            ratatui_crossterm::crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                KeyModifiers::NONE,
            ),
        ));
        let _ = screen.event(&toggle, &mut ctx).unwrap();
        assert!(screen.dashboard);
        assert!(screen.data.is_none());
        let _ = screen
            .event(
                &AppEvent::PluginTable(
                    screen.owner,
                    before,
                    Ok(PluginDbQueryResponse {
                        columns: vec!["old table".into()],
                        rows: vec![],
                    }),
                ),
                &mut ctx,
            )
            .unwrap();
        assert!(screen.data.is_none());
        let _ = screen.event(&toggle, &mut ctx).unwrap();
        assert!(!screen.dashboard);
    }
}

#[cfg(test)]
mod filter_state_tests {
    use super::*;
    #[test]
    fn period_is_shared_but_grouping_is_owned_by_each_tab() {
        let shared = stationd_proto::plugin::PluginTabFilter {
            key: "period".into(),
            default_value: "24 h".into(),
            shared: true,
            ..Default::default()
        };
        let own = stationd_proto::plugin::PluginTabFilter {
            key: "grouping".into(),
            default_value: "Heure".into(),
            ..Default::default()
        };
        let first = PluginTable::new(
            "stats".into(),
            PluginTab {
                id: "hourly".into(),
                filters: vec![shared.clone(), own.clone()],
                ..Default::default()
            },
        );
        let second = PluginTable::new(
            "stats".into(),
            PluginTab {
                id: "geo".into(),
                filters: vec![shared.clone(), own.clone()],
                ..Default::default()
            },
        );
        let mut store = Store::new("localhost");
        store
            .plugin_filters
            .insert(first.filter_key(&shared), "7 jours".into());
        store
            .plugin_filters
            .insert(first.filter_key(&own), "Mois".into());
        assert_eq!(second.current_filters(&store)["period"], "7 jours");
        assert_eq!(second.current_filters(&store)["grouping"], "Heure");
        assert_eq!(first.current_filters(&store)["grouping"], "Mois");
    }
}

#[cfg(test)]
mod date_filter_tests {
    use super::*;
    fn screen() -> PluginTable {
        let mut screen = PluginTable::new(
            "stats".into(),
            PluginTab {
                id: "audience".into(),
                filters: vec![
                    stationd_proto::plugin::PluginTabFilter {
                        key: "period".into(),
                        kind: "choice".into(),
                        options: vec!["7 jours".into(), "Personnalisée".into()],
                        ..Default::default()
                    },
                    stationd_proto::plugin::PluginTabFilter {
                        key: "from".into(),
                        kind: "datetime".into(),
                        ..Default::default()
                    },
                    stationd_proto::plugin::PluginTabFilter {
                        key: "to".into(),
                        kind: "datetime".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        screen.draft = vec![
            "7 jours".into(),
            "2020-01-01 00:00:00".into(),
            "2020-01-02 00:00:00".into(),
        ];
        screen.last_preset = "7 jours".into();
        screen.filter_selected = 1;
        screen
    }
    #[test]
    fn selecting_date_converts_preset_and_replaces_inactive_bounds() {
        let mut s = screen();
        s.open_date_picker();
        assert_ne!(s.date_picker.as_ref().unwrap().value(), s.draft[1]);
        let end = s.date_defaults.as_ref().unwrap().1.clone();
        s.accept_date("2024-02-29 00:00:00".into());
        assert_eq!(s.draft[0], "Personnalisée");
        assert_eq!(s.draft[1], "2024-02-29 00:00:00");
        assert_eq!(s.draft[2], end);
        assert!(s.validate_draft().is_none());
        s.open_date_picker();
        s.accept_date("2024-03-01 00:00:00".into());
        assert_eq!(s.draft[2], end); // Editing an existing custom period keeps its other bound.
    }
    #[test]
    fn invalid_custom_bounds_block_apply_and_cancel_does_not_edit() {
        let mut s = screen();
        let draft = s.draft.clone();
        s.open_date_picker();
        assert_eq!(s.draft, draft);
        s.date_picker = None;
        s.date_defaults = None;
        assert_eq!(s.draft, draft);
        s.draft = vec![
            "Personnalisée".into(),
            "2024-03-01 00:00:00".into(),
            "2024-02-29 00:00:00".into(),
        ];
        assert!(s.validate_draft().is_some());
        s.draft[0] = "7 jours".into();
        assert!(s.validate_draft().is_none());
        s.draft[0] = "Personnalisée".into();
        s.draft[2].clear();
        assert!(s.validate_draft().is_some());
    }
}
