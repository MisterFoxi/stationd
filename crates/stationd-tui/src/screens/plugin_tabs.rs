//! Plugin-owned declarative tables. No plugin-specific SQL or schema in the TUI.
use super::{next_owner, ops};
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
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_widgets::{
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
}
impl PluginTable {
    pub fn new(name: String, tab: PluginTab) -> Self {
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
        }
    }
    fn load(&mut self, ctx: &mut Global) {
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
        ctx.spawn_async(async move {
            let result = rpc::read_plugin_tab(channel, name, tab).await;
            Ok(Control::Event(AppEvent::PluginTable(
                owner, request, result,
            )))
        });
    }
}
fn cell(value: &stationd_proto::plugin::PluginDbValue) -> String {
    let text = match &value.kind {
        Some(Kind::Null(_)) | None => "NULL".into(),
        Some(Kind::Integer(v)) => v.to_string(),
        Some(Kind::Real(v)) => v.to_string(),
        Some(Kind::Text(v)) => v.clone(),
        Some(Kind::Blob(v)) => format!("[{} bytes]", v.len()),
    };
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
impl Screen for PluginTable {
    fn plugin_tab_key(&self) -> Option<(String, String)> {
        Some((self.name.clone(), self.tab.id.clone()))
    }
    fn title(&self) -> String {
        self.tab.title.clone()
    }
    fn help(&self) -> &'static [KeyHelp] {
        TABLE_KEYS
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
                let count = self.data.as_ref().map_or(0, |d| d.rows.len());
                let cols = self.data.as_ref().map_or(0, |d| d.columns.len());
                match key.code {
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
        let [notice, body, detail] = Layout::vertical([
            Constraint::Length(3),
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
            self.name, self.tab.description
        ))];
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
                    .map(|v| Cell::from(cell(v))),
            );
            if i == self.selected {
                row.style(s.tab_active())
            } else {
                row
            }
        });
        let header =
            Row::new(data.columns.iter().skip(self.column).take(cols).cloned()).style(s.label());
        Table::new(rows, vec![Constraint::Fill(1); cols])
            .header(header)
            .column_spacing(1)
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
    }
}
