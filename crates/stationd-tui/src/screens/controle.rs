//! Contrôle (`2`) — tout ce qu'on commande sur la station (dossier §5.2).
//!
//! Sections à gauche (Tab / Maj+Tab), détail à droite, touches propres à la
//! section. Chaque action est l'appel gRPC de `stationctl` ; celles qui
//! touchent l'antenne passent par une confirmation qui nomme l'objet. Les
//! données viennent du bandeau (lu toutes les 2 s) : l'effet d'une action
//! s'y lit au tour suivant.

use anyhow::Error;
use rat_salsa::Control;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use ratatui_widgets::table::{Cell, Row, Table};
use stationd_proto::broadcast::State;
use stationd_proto::library::skip::Reason;

use super::ops;
use crate::action::PluginVerb;
use crate::app::{AppEvent, Global};
use crate::fit;
use crate::screen::{KeyHelp, Screen};
use crate::store::{Store, local_hms};
use crate::style::Styles;
use crate::{k, tr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Broadcast,
    Overrides,
    Live,
    Queue,
    Library,
    Plugins,
    Station,
}

const SECTIONS: [Section; 7] = [
    Section::Broadcast,
    Section::Overrides,
    Section::Live,
    Section::Queue,
    Section::Library,
    Section::Plugins,
    Section::Station,
];

impl Section {
    fn title(self) -> String {
        match self {
            Section::Broadcast => tr!("control-broadcast"),
            Section::Overrides => tr!("control-overrides"),
            Section::Live => tr!("control-live"),
            Section::Queue => tr!("control-queue"),
            Section::Library => tr!("control-library"),
            Section::Plugins => tr!("control-plugins"),
            Section::Station => tr!("control-station"),
        }
    }

    fn keys(self) -> &'static [KeyHelp] {
        match self {
            Section::Broadcast => &[
                (k!("key-tab"), k!("help-section")),
                (k!("key-space"), k!("help-pause-resume")),
                (k!("key-n"), k!("help-skip")),
                (k!("key-v"), k!("help-drain")),
                (k!("key-w"), k!("help-wake")),
            ],
            Section::Overrides => &[
                (k!("key-tab"), k!("help-section")),
                (k!("key-o"), k!("help-override")),
                (k!("key-up-down"), k!("help-select")),
                (k!("key-d"), k!("help-override-remove")),
                (k!("key-shift-d"), k!("help-override-clear")),
            ],
            Section::Live => &[
                (k!("key-tab"), k!("help-section")),
                (k!("key-k"), k!("help-live-kick")),
                (k!("key-o"), k!("help-live-open")),
                (k!("key-up-down"), k!("help-select")),
                (k!("key-c"), k!("help-live-close")),
            ],
            Section::Queue => &[(k!("key-tab"), k!("help-section")), (k!("key-e"), k!("help-enqueue"))],
            Section::Library => &[(k!("key-tab"), k!("help-section")), (k!("key-s"), k!("help-scan"))],
            Section::Plugins => &[
                (k!("key-tab"), k!("help-section")),
                (k!("key-up-down"), k!("help-select")),
                (k!("key-s"), k!("plugin-verb-start")),
                (k!("key-x"), k!("plugin-verb-stop")),
                (k!("key-r"), k!("plugin-verb-restart")),
                (k!("key-l"), k!("plugin-verb-reload")),
            ],
            Section::Station => &[
                (k!("key-tab"), k!("help-section")),
                (k!("key-a"), k!("help-shutdown")),
                (k!("key-shift-a"), k!("help-shutdown-force")),
            ],
        }
    }
}

#[derive(Default)]
pub struct Controle {
    section: usize,
    /// Ligne choisie dans la liste de la section (overrides, ouvertures, plugins).
    selected: usize,
}

fn titled<'a>(title: String, s: &Styles) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border())
        .title(Span::styled(format!(" {title} "), s.title()))
}

fn hms(store: &Store, epoch: i64) -> String {
    local_hms(store.tz.as_ref(), epoch).unwrap_or_else(|| "—".into())
}

/// `libellé   valeur`, libellés alignés.
fn field<'a>(label: String, value: Span<'a>, s: &Styles) -> Line<'a> {
    Line::from(vec![Span::styled(format!("{label:<16} "), s.label()), value])
}

/// Une valeur jamais reçue est « — » ; une erreur de lecture se dit.
fn unread<'a>(error: Option<&String>, s: &Styles) -> Line<'a> {
    match error {
        Some(e) => Line::styled(tr!("control-read-failed", reason = e.clone()), s.warn()),
        None => Line::styled(tr!("control-not-read"), s.muted()),
    }
}

/// Fait défiler pour que `selected` reste visible dans `height` lignes.
fn window(len: usize, selected: usize, height: usize) -> std::ops::Range<usize> {
    if height == 0 || len == 0 {
        return 0..0;
    }
    let start = selected.saturating_sub(height - 1).min(len.saturating_sub(height));
    start..(start + height).min(len)
}

impl Controle {
    fn current(&self) -> Section {
        SECTIONS[self.section]
    }

    /// Nombre de lignes sélectionnables dans la section.
    fn list_len(&self, store: &Store) -> usize {
        match self.current() {
            Section::Overrides => store.overrides.value.as_ref().map_or(0, Vec::len),
            Section::Live => store.live.value.as_ref().map_or(0, |l| l.openings.len()),
            Section::Plugins => store.plugins.value.as_ref().map_or(0, Vec::len),
            _ => 0,
        }
    }

    fn render_sections(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let block = titled(tr!("screen-control"), s);
        let inner = block.inner(area);
        block.render(area, buf);
        let lines: Vec<Line> = SECTIONS
            .iter()
            .enumerate()
            .map(|(i, sec)| {
                let text = fit::ellipsize(&sec.title(), inner.width.saturating_sub(3) as usize);
                if i == self.section {
                    Line::styled(format!(" ▸ {text}"), s.tab_active())
                } else {
                    Line::styled(format!("   {text}"), s.label())
                }
            })
            .collect();
        Paragraph::new(lines).render(inner, buf);
    }

    fn render_broadcast(&self, area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
        let mut lines = Vec::new();
        let (txt, style) = match ops::state(store) {
            State::Running => (tr!("state-running"), s.ok()),
            State::Paused => (tr!("state-paused"), s.warn()),
            State::Draining => (tr!("state-draining-long"), s.warn()),
            State::Sleeping => (tr!("state-sleeping-long"), s.calm()),
            State::Unspecified => (tr!("state-unknown"), s.muted()),
        };
        lines.push(field(tr!("control-state"), Span::styled(txt, style), s));
        let listeners = match store.broadcast.value.as_ref().and_then(|b| b.listeners) {
            Some(n) => Span::raw(n.to_string()),
            None => Span::styled("—", s.muted()),
        };
        lines.push(field(tr!("control-listeners"), listeners, s));
        lines.push(field(tr!("control-on-air"), Span::styled(ops::on_air_label(store), s.accent()), s));
        if let Some(e) = &store.broadcast.error {
            lines.push(Line::default());
            lines.push(unread(Some(e), s));
        }
        lines.push(Line::default());
        lines.push(Line::styled(tr!("control-broadcast-hint"), s.muted()));
        Paragraph::new(lines).wrap(Wrap { trim: false }).render(area, buf);
    }

    fn render_overrides(&self, area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
        let Some(list) = &store.overrides.value else {
            Paragraph::new(unread(store.overrides.error.as_ref(), s)).render(area, buf);
            return;
        };
        if list.is_empty() {
            Paragraph::new(vec![
                Line::styled(tr!("control-overrides-none"), s.muted()),
                Line::default(),
                Line::styled(tr!("control-overrides-hint"), s.muted()),
            ])
            .wrap(Wrap { trim: false })
            .render(area, buf);
            return;
        }
        let header = Row::new(vec![
            Cell::from("#"),
            Cell::from(tr!("control-col-content")),
            Cell::from(tr!("control-col-mode")),
            Cell::from(tr!("control-col-left")),
            Cell::from(tr!("control-col-expires")),
            Cell::from(tr!("control-col-source")),
        ])
        .style(s.label());
        let range = window(list.len(), self.selected, area.height.saturating_sub(1) as usize);
        let rows: Vec<Row> = list[range.clone()]
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let what = if o.playlist_ref.is_empty() { o.media_path.clone() } else { format!("▤ {}", o.playlist_ref) };
                let expires = if o.expires_at == 0 { "—".into() } else { hms(store, o.expires_at) };
                let row = Row::new(vec![
                    Cell::from(o.id.to_string()),
                    Cell::from(what),
                    Cell::from(Span::styled(o.mode.clone(), if o.mode == "hard" { s.warn() } else { Style::default() })),
                    Cell::from(o.remaining.to_string()),
                    Cell::from(expires),
                    Cell::from(Span::styled(o.source.clone(), s.muted())),
                ]);
                if range.start + i == self.selected { row.style(s.tab_active()) } else { row }
            })
            .collect();
        Table::new(
            rows,
            [
                Constraint::Length(5),
                Constraint::Fill(1),
                Constraint::Length(5),
                Constraint::Length(6),
                Constraint::Length(9),
                Constraint::Length(10),
            ],
        )
        .header(header)
        .column_spacing(1)
        .render(area, buf);
    }

    fn render_live(&self, area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
        let Some(l) = &store.live.value else {
            Paragraph::new(unread(store.live.error.as_ref(), s)).render(area, buf);
            return;
        };
        if !l.enabled {
            Paragraph::new(Line::styled(tr!("control-live-disabled"), s.muted())).render(area, buf);
            return;
        }
        let mut lines = Vec::new();
        match &l.on_air {
            Some(sess) => {
                lines.push(field(tr!("control-live-on-air"), Span::styled(sess.dj.clone(), s.ok()), s));
                lines.push(field(
                    String::new(),
                    Span::styled(
                        tr!(
                            "control-live-session",
                            access = sess.access.clone(),
                            since = hms(store, sess.since),
                            address = sess.address.clone()
                        ),
                        s.muted(),
                    ),
                    s,
                ));
            }
            None => lines.push(field(tr!("control-live-on-air"), Span::styled(tr!("control-live-nobody"), s.muted()), s)),
        }
        let djs = if l.djs_error.is_empty() {
            Span::raw(tr!("control-live-djs", n = l.djs_count))
        } else {
            Span::styled(tr!("control-live-djs-error", reason = l.djs_error.clone()), s.error())
        };
        lines.push(field(tr!("control-live-accounts"), djs, s));
        if !l.urgent_djs.is_empty() {
            lines.push(field(tr!("control-live-urgent"), Span::raw(l.urgent_djs.join(", ")), s));
        }
        for c in &l.cooldowns {
            lines.push(field(
                tr!("control-live-cooldown"),
                Span::styled(tr!("control-live-until", dj = c.dj.clone(), time = hms(store, c.until)), s.warn()),
                s,
            ));
        }
        for r in &l.refused {
            lines.push(field(tr!("control-live-refused"), Span::styled(r.dj.clone(), s.warn()), s));
        }
        if !l.last_refusal_dj.is_empty() {
            lines.push(field(
                tr!("control-live-last-refusal"),
                Span::styled(
                    tr!(
                        "control-live-refusal",
                        dj = l.last_refusal_dj.clone(),
                        time = hms(store, l.last_refusal_at),
                        reason = l.last_refusal_reason.clone()
                    ),
                    s.muted(),
                ),
                s,
            ));
        }
        lines.push(Line::default());
        lines.push(Line::styled(tr!("control-live-openings"), s.label()));
        let head_h = lines.len() as u16;
        let [head_a, list_a] = Layout::vertical([Constraint::Length(head_h), Constraint::Fill(1)]).areas(area);
        Paragraph::new(lines).render(head_a, buf);
        if l.openings.is_empty() {
            Paragraph::new(Line::styled(format!("  {}", tr!("control-live-no-opening")), s.muted())).render(list_a, buf);
            return;
        }
        let range = window(l.openings.len(), self.selected, list_a.height as usize);
        let lines: Vec<Line> = l.openings[range.clone()]
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let mut txt = tr!("control-live-until", dj = o.dj.clone(), time = hms(store, o.until));
                if o.cut {
                    txt = format!("{txt} · {}", tr!("control-live-cut"));
                }
                if range.start + i == self.selected {
                    Line::styled(format!("▸ {txt}"), s.tab_active())
                } else {
                    Line::raw(format!("  {txt}"))
                }
            })
            .collect();
        Paragraph::new(lines).render(list_a, buf);
    }

    fn render_queue(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        Paragraph::new(vec![Line::styled(tr!("control-queue-hint"), s.muted())])
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_library(&self, area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
        let Some(r) = &store.last_scan else {
            Paragraph::new(vec![
                Line::styled(tr!("control-scan-none"), s.muted()),
                Line::default(),
                Line::styled(tr!("control-scan-hint"), s.muted()),
            ])
            .wrap(Wrap { trim: false })
            .render(area, buf);
            return;
        };
        let mut lines = vec![
            Line::styled(tr!("control-scan-last"), s.label()),
            field(tr!("control-scan-found"), Span::raw(r.found.to_string()), s),
            field(tr!("control-scan-present"), Span::raw(r.present.to_string()), s),
            field(
                tr!("control-scan-vanished"),
                Span::styled(r.vanished.to_string(), if r.vanished > 0 { s.warn() } else { Style::default() }),
                s,
            ),
            field(tr!("control-scan-unavailable"), Span::styled(r.unavailable.to_string(), s.muted()), s),
            field(
                tr!("control-scan-skipped"),
                Span::styled(r.skipped.to_string(), if r.skipped > 0 { s.warn() } else { Style::default() }),
                s,
            ),
        ];
        if !r.skips.is_empty() {
            lines.push(Line::default());
            let room = area.height.saturating_sub(lines.len() as u16 + 1) as usize;
            for sk in r.skips.iter().take(room) {
                let why = match Reason::try_from(sk.reason).unwrap_or(Reason::Unspecified) {
                    Reason::Unreadable => tr!("scan-skip-unreadable"),
                    Reason::ZeroDuration => tr!("scan-skip-zero-duration"),
                    Reason::WalkError => tr!("scan-skip-walk-error"),
                    Reason::Unspecified => tr!("scan-skip-unknown"),
                };
                let detail = if sk.detail.is_empty() { String::new() } else { format!(" ({})", sk.detail) };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {why} "), s.warn()),
                    Span::raw(fit::ellipsize(&format!("{}{detail}", sk.path), area.width.saturating_sub(20) as usize)),
                ]));
            }
            if r.skips.len() > room {
                lines.push(Line::styled(format!("  {}", tr!("control-more", n = r.skips.len() - room)), s.muted()));
            }
        }
        Paragraph::new(lines).render(area, buf);
    }

    fn render_plugins(&self, area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
        let Some(list) = &store.plugins.value else {
            Paragraph::new(unread(store.plugins.error.as_ref(), s)).render(area, buf);
            return;
        };
        if list.is_empty() {
            Paragraph::new(Line::styled(tr!("control-plugins-none"), s.muted())).render(area, buf);
            return;
        }
        let header = Row::new(vec![
            Cell::from(tr!("control-col-plugin")),
            Cell::from(tr!("control-col-state")),
            Cell::from(tr!("control-col-failures")),
            Cell::from(tr!("control-col-reason")),
        ])
        .style(s.label());
        let range = window(list.len(), self.selected, area.height.saturating_sub(1) as usize);
        let rows: Vec<Row> = list[range.clone()]
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let style = match p.state.as_str() {
                    "loaded" => s.ok(),
                    "failed" | "quarantined" => s.error(),
                    _ => s.muted(),
                };
                let name = if p.enabled { p.name.clone() } else { format!("{} ({})", p.name, tr!("control-plugin-off")) };
                let row = Row::new(vec![
                    Cell::from(name),
                    Cell::from(Span::styled(p.state.clone(), style)),
                    Cell::from(p.failures.to_string()),
                    Cell::from(Span::styled(p.reason.clone(), s.muted())),
                ]);
                if range.start + i == self.selected { row.style(s.tab_active()) } else { row }
            })
            .collect();
        Table::new(rows, [Constraint::Length(20), Constraint::Length(12), Constraint::Length(6), Constraint::Fill(1)])
            .header(header)
            .column_spacing(1)
            .render(area, buf);
    }

    fn render_station(&self, area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
        let mut lines = vec![Line::styled(tr!("control-shutdown-hint"), s.muted())];
        if let Some(sess) = store.live.value.as_ref().and_then(|l| l.on_air.as_ref()) {
            lines.push(Line::default());
            lines.push(Line::styled(tr!("control-shutdown-live", dj = sess.dj.clone()), s.warn()));
        }
        Paragraph::new(lines).wrap(Wrap { trim: false }).render(area, buf);
    }

    /// Touche propre à la section. `true` = traitée.
    fn section_key(&mut self, code: KeyCode, ctx: &mut Global) -> bool {
        let store = &ctx.store;
        match (self.current(), code) {
            (Section::Broadcast, KeyCode::Char(' ')) => match ops::toggle_pause(store) {
                Some(ops::Outcome::Open(m)) => ctx.open(m),
                Some(ops::Outcome::Run(a)) => ctx.request(a),
                None => return false,
            },
            (Section::Broadcast, KeyCode::Char('n')) => ctx.open(ops::skip(store)),
            (Section::Broadcast, KeyCode::Char('v')) => ctx.open(ops::stop_when_idle()),
            (Section::Broadcast, KeyCode::Char('w')) => ctx.request(crate::action::Action::Wake),
            (Section::Overrides, KeyCode::Char('o')) => ctx.open(ops::push_override()),
            (Section::Overrides, KeyCode::Char('d')) => {
                let Some(o) = store.overrides.value.as_ref().and_then(|l| l.get(self.selected)) else { return false };
                let what = if o.playlist_ref.is_empty() { o.media_path.clone() } else { o.playlist_ref.clone() };
                ctx.open(ops::clear_override(o.id, what));
            }
            (Section::Overrides, KeyCode::Char('D')) => {
                let n = store.overrides.value.as_ref().map_or(0, Vec::len);
                if n == 0 {
                    return false;
                }
                ctx.open(ops::clear_all_overrides(n));
            }
            (Section::Live, KeyCode::Char('k')) => {
                let Some(dj) = store.live.value.as_ref().and_then(|l| l.on_air.as_ref()).map(|s| s.dj.clone()) else {
                    return false;
                };
                ctx.open(ops::kick(&dj));
            }
            (Section::Live, KeyCode::Char('o')) => ctx.open(ops::live_open()),
            (Section::Live, KeyCode::Char('c')) => {
                let Some(o) = store.live.value.as_ref().and_then(|l| l.openings.get(self.selected)) else {
                    return false;
                };
                let dj = o.dj.clone();
                ctx.open(ops::live_close(&dj));
            }
            (Section::Queue, KeyCode::Char('e')) => ctx.open(ops::enqueue()),
            (Section::Library, KeyCode::Char('s')) => ctx.open(ops::scan()),
            (Section::Plugins, KeyCode::Char(c @ ('s' | 'x' | 'r' | 'l'))) => {
                let Some(p) = store.plugins.value.as_ref().and_then(|l| l.get(self.selected)) else { return false };
                let verb = match c {
                    's' => PluginVerb::Start,
                    'x' => PluginVerb::Stop,
                    'r' => PluginVerb::Restart,
                    _ => PluginVerb::Reload,
                };
                let name = p.name.clone();
                ctx.open(ops::plugin(&name, verb));
            }
            (Section::Station, KeyCode::Char(c @ ('a' | 'A'))) => {
                let dj = store.live.value.as_ref().and_then(|l| l.on_air.as_ref()).map(|s| s.dj.clone());
                ctx.open(ops::shutdown(c == 'A', dj.as_deref()));
            }
            _ => return false,
        }
        true
    }
}

impl Screen for Controle {
    fn title(&self) -> String {
        tr!("screen-control")
    }

    fn help(&self) -> &'static [KeyHelp] {
        self.current().keys()
    }

    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        let AppEvent::Event(Event::Key(k)) = event else { return Ok(Control::Continue) };
        if k.kind != KeyEventKind::Press {
            return Ok(Control::Continue);
        }
        match k.code {
            KeyCode::Tab => {
                self.section = (self.section + 1) % SECTIONS.len();
                self.selected = 0;
                return Ok(Control::Changed);
            }
            KeyCode::BackTab => {
                self.section = (self.section + SECTIONS.len() - 1) % SECTIONS.len();
                self.selected = 0;
                return Ok(Control::Changed);
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                return Ok(Control::Changed);
            }
            KeyCode::Down => {
                let n = self.list_len(&ctx.store);
                self.selected = (self.selected + 1).min(n.saturating_sub(1));
                return Ok(Control::Changed);
            }
            _ => {}
        }
        if self.section_key(k.code, ctx) {
            return Ok(Control::Changed);
        }
        Ok(Control::Continue)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        Block::new().style(s.base()).render(area, buf);
        // La liste a pu raccourcir depuis le dernier tour (override passé…).
        let n = self.list_len(&ctx.store);
        self.selected = self.selected.min(n.saturating_sub(1));

        let side_w = SECTIONS.iter().map(|x| Span::raw(x.title()).width()).max().unwrap_or(12) as u16 + 6;
        let [side, main] = Layout::horizontal([Constraint::Length(side_w), Constraint::Fill(1)]).areas(area);
        self.render_sections(side, buf, &s);

        let sec = self.current();
        let block = titled(sec.title(), &s);
        let inner = block.inner(main);
        block.render(main, buf);
        // Les touches de la section sont sur la ligne du bas (aide de l'écran).
        // Une colonne de marge de chaque côté, texte coupé compris.
        let body = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
        let store = &ctx.store;
        match sec {
            Section::Broadcast => self.render_broadcast(body, buf, store, &s),
            Section::Overrides => self.render_overrides(body, buf, store, &s),
            Section::Live => self.render_live(body, buf, store, &s),
            Section::Queue => self.render_queue(body, buf, &s),
            Section::Library => self.render_library(body, buf, store, &s),
            Section::Plugins => self.render_plugins(body, buf, store, &s),
            Section::Station => self.render_station(body, buf, store, &s),
        }
        Ok(())
    }
}
