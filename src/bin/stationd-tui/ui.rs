use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap},
    Frame,
};
use stationd::proto::schedule::{self, rule::Kind};

use super::app::App;

fn block(title: impl Into<String>) -> Block<'static> {
    Block::default().title(title.into()).borders(Borders::ALL)
}

fn empty(value: &str) -> &str { if value.is_empty() { "-" } else { value } }
fn enabled(value: bool) -> &'static str { if value { "enabled" } else { "disabled" } }

fn uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = seconds / 3_600 % 24;
    let minutes = seconds / 60 % 60;
    let seconds = seconds % 60;
    if days > 0 {
        format!("{days}d {hours:02}h {minutes:02}m {seconds:02}s")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}
fn wall(value: &Option<schedule::WallClock>) -> String {
    value.as_ref().map(|w| format!("{:02}:{:02}", w.hour, w.minute)).unwrap_or_else(|| "-".into())
}

fn rule_summary(rule: &schedule::Rule) -> (&str, &str) {
    match &rule.kind {
        Some(Kind::BaseRotation(r)) => ("BaseRotation", &r.playlist_ref),
        Some(Kind::DayPart(r)) => ("DayPart", &r.playlist_ref),
        Some(Kind::AtClock(r)) => ("AtClock", &r.playlist_ref),
        Some(Kind::Every(r)) => ("Every", &r.playlist_ref),
        Some(Kind::Live(r)) => ("Live (DJ)", &r.dj),
        None => ("Unknown", "-"),
    }
}

fn rule_detail(rule: &schedule::Rule) -> String {
    let (kind, playlist) = rule_summary(rule);
    let mut lines = vec![format!("Rule: {}", rule.id), format!("State: {}", enabled(rule.enabled)),
        format!("Kind: {kind}"), format!("Playlist: {playlist}")];
    if let Some(validity) = &rule.validity {
        let days = if validity.days.is_empty() { "Every day".into() } else {
            validity.days.iter().map(|day| schedule::Weekday::try_from(*day)
                .map(|d| d.as_str_name().to_owned()).unwrap_or_else(|_| format!("Unknown ({day})")))
                .collect::<Vec<_>>().join(", ")
        };
        lines.extend([format!("Days: {days}"), format!("From date: {}", empty(&validity.date_start)),
            format!("To date: {}", empty(&validity.date_end))]);
    }
    match &rule.kind {
        Some(Kind::DayPart(r)) => lines.extend([format!("Window: {} - {}", wall(&r.start), wall(&r.end)),
            "End bounds the window; it does not cut the track.".into()]),
        Some(Kind::AtClock(r)) => {
            if r.every_minutes > 0 { lines.push(format!("Clock marks: every {} min from the hour", r.every_minutes)); }
            else { lines.push(format!("At: {}", wall(&r.at))); }
            let mode = schedule::at_clock::Mode::try_from(r.mode)
                .map(|m| m.as_str_name().to_owned()).unwrap_or_else(|_| format!("Unknown ({})", r.mode));
            lines.push(format!("Mode: {mode}"));
            lines.push(format!("Expiry: {}", r.expiry.as_ref().map(|d| format!("{}s", d.seconds))
                .unwrap_or_else(|| "None".into())));
        }
        Some(Kind::Every(r)) => lines.push(match &r.cadence {
            Some(schedule::every::Cadence::Tracks(n)) => format!("Cadence: {n} completed station tracks"),
            Some(schedule::every::Cadence::Elapsed(d)) => format!("Cadence: {}s since last play", d.seconds),
            None => "Cadence: unspecified".into(),
        }),
        _ => {},
    }
    lines.join("\n")
}

fn detail(frame: &mut Frame, area: Rect, text: String, scroll: u16) {
    let paragraph = Paragraph::new(text).block(block("Detail | PgUp/PgDn"))
        .wrap(Wrap { trim: false }).scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

pub fn draw(frame: &mut Frame, app: &mut App, addr: &str) {
    let area = frame.area();
    if let Some(form) = &mut app.form {
        draw_form(frame, form);
        return;
    }
    if area.width < 40 || area.height < 12 {
        frame.render_widget(Paragraph::new("StationD\nResize terminal to at least 40 x 12.\nq: quit")
            .wrap(Wrap { trim: false }), area);
        return;
    }
    let connected = if app.status.error.is_some() {
        if app.status.value.is_some() { "UNAVAILABLE / STALE" } else { "UNAVAILABLE" }
    } else if app.status.value.is_some() { "CONNECTED" } else { "CONNECTING" };
    let refresh = if app.loading { "refreshing" } else if app.auto { "auto" } else { "manual" };
    let mut header = vec![Line::from(format!("StationD | {connected} | {refresh}"))];
    if let Some(s) = &app.status.value {
        header.push(Line::from(format!("Station: {}", s.station_name)));
        let summary = format!("Timezone: {} | Uptime: {} | PID: {}",
            s.timezone, uptime(s.uptime_seconds), s.pid);
        // Fit the status on one line when possible; keep each field on its
        // own line in a narrow terminal. Values come from the Status RPC.
        if Line::from(summary.as_str()).width() <= usize::from(area.width) {
            header.push(Line::from(summary));
        } else {
            header.extend([
                Line::from(format!("Timezone: {}", s.timezone)),
                Line::from(format!("Uptime: {}", uptime(s.uptime_seconds))),
                Line::from(format!("PID: {}", s.pid)),
            ]);
        }
    } else {
        header.push(Line::from("Station status: unavailable"));
    }
    header.push(Line::from(addr.to_owned()));
    let layout = Layout::vertical([Constraint::Length(header.len() as u16), Constraint::Length(3),
        Constraint::Min(1), Constraint::Length(2)]).split(area);
    frame.render_widget(Paragraph::new(header).style(Style::default().fg(Color::Cyan)), layout[0]);
    frame.render_widget(Tabs::new(["1 Status", "2 Playlists", "3 Grid", "4 Agenda"])
        .select(app.tab).block(block("Administration"))
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)), layout[1]);
    match app.tab {
        1 => playlists(frame, app, layout[2]),
        2 => rules(frame, app, layout[2]),
        3 => super::agenda_ui::draw(frame, &mut app.agenda, &app.rules, &app.playlists, layout[2]),
        _ => status(frame, app, layout[2]),
    }
    let footer = if app.tab == 3 {
        "d/w: day/week  PgUp/PgDn: period  +/-: zoom  g: date\nt: today  Enter: detail  r: refresh  Tab/1-4: view  ?: help"
    } else if app.tab == 1 {
        "n: new playlist  s: sync  r: refresh\nTab/1-4: view  ?: help  q: quit"
    } else { "Tab/1-4: view  Up/Down: select  r: refresh\na: auto/manual  ?: help  q: quit" };
    frame.render_widget(Paragraph::new(footer), layout[3]);
    if app.help {
        let width = area.width.min(76);
        let height = area.height.min(24);
        let popup = Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 2, width, height);
        frame.render_widget(Clear, popup);
        frame.render_widget(Paragraph::new(Text::from(
            "Tab / Shift-Tab or 1-4: change view\nUp / Down or k / j: select item\nHome/End: first/last item or hour\nPgUp/PgDn: scroll detail (Agenda: period)\nr: refresh; a: auto/manual\nn: new local TOML (Playlists)\ns: sync daemon playlists (Playlists)\n\nAgenda: d/w day/week; t today; g YYYY-MM-DD\nLeft/Right: day; Up/Down: hour\nPgUp/PgDn or [ / ]: previous/next period\n+/-: 15/30/60-minute buckets; Enter: details\n! hard mark; * soft mark; D day part; B base; F fallback\nEvery cadences depend on playback and are not projected.\nRepeated civil hours have a/b rows; missing hours show DST.\n\nq / Ctrl-C: close this client\nEsc / ?: close help\nClosing this client leaves stationd running."
        )).block(block("Keyboard help")).wrap(Wrap { trim: false }), popup);
    }
    if let Some(report) = &app.report {
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(report.as_str())
            .block(block("Details / report | Esc/Enter: close | PgUp/PgDn"))
            .wrap(Wrap { trim: false }).scroll((app.report_scroll, 0)), area);
    }
}

fn draw_form(frame: &mut Frame, form: &mut super::playlist_form::PlaylistForm) {
    let area = frame.area();
    if area.width < 40 || area.height < 16 {
        frame.render_widget(Paragraph::new("Playlist draft preserved.\nResize to at least 40 x 16.\nEsc: cancel / return to form").wrap(Wrap { trim: false }), area);
        return;
    }
    if let Some(text) = &form.preview {
        let layout = Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(3), Constraint::Length(2)]).split(area);
        frame.render_widget(Paragraph::new(format!("Create: {}", form.root.join(form.get("path")).display())).wrap(Wrap { trim: false }), layout[0]);
        frame.render_widget(Paragraph::new(text.as_str()).block(block("TOML preview"))
            .wrap(Wrap { trim: false }).scroll((form.preview_scroll, 0)), layout[1]);
        let notice = if form.message.is_empty() { "Validated with StationD's playlist parser. Group references/cycles are checked during sync." } else { &form.message };
        frame.render_widget(Paragraph::new(notice).style(Style::default().fg(Color::Yellow)).wrap(Wrap { trim: false }), layout[2]);
        frame.render_widget(Paragraph::new("Ctrl-S: save new file  Esc: edit\nUp/Down/PgUp/PgDn: scroll"), layout[3]);
        return;
    }
    let layout = Layout::vertical([Constraint::Length(2), Constraint::Length(1), Constraint::Min(3), Constraint::Length(2), Constraint::Length(3), Constraint::Length(2)]).split(area);
    frame.render_widget(Paragraph::new(format!("New playlist | Local directory: {}", form.root.display()))
        .style(Style::default().fg(Color::Cyan)).wrap(Wrap { trim: false }), layout[0]);
    frame.render_widget(Tabs::new(["General", "Selection", "Broadcast"]).select(form.page)
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)), layout[1]);
    let selected = form.selection.selected().unwrap_or(0);
    let fields = form.fields();
    let rows: Vec<ListItem> = fields.iter().enumerate().map(|(i, f)| {
        let value = form.get(&f.key);
        let line = if !f.choices.is_empty() {
            Line::from(format!("  < {} >", if value.is_empty() { "default" } else { value }))
        } else if i == selected {
            let input = &form.inputs[&f.key];
            input_line(input, layout[2].width.saturating_sub(6) as usize)
        } else {
            Line::from(format!("  {}", if value.is_empty() { "(empty)" } else { value }))
        };
        ListItem::new(vec![Line::from(f.label.clone()), line])
    }).collect();
    frame.render_stateful_widget(List::new(rows).block(block("Fields | Tab: next | Left/Right: choice or cursor"))
        .highlight_symbol("> ").highlight_style(Style::default().fg(Color::Cyan)), layout[2], &mut form.selection);
    frame.render_widget(Paragraph::new(form.hint()).wrap(Wrap { trim: false }), layout[3]);
    frame.render_widget(Paragraph::new(form.message.as_str()).style(Style::default().fg(Color::Yellow))
        .wrap(Wrap { trim: false }), layout[4]);
    frame.render_widget(Paragraph::new("F4: section  F5/Ctrl-S: preview\nEsc: cancel  Ctrl-U: clear field"), layout[5]);
    if form.discard {
        let popup = Rect::new(area.x + 2, area.y + area.height / 2 - 2, area.width - 4, 5);
        frame.render_widget(Clear, popup);
        frame.render_widget(Paragraph::new("Discard this unsaved playlist?\ny: discard   n / Esc: keep editing")
            .block(block("Unsaved draft")).wrap(Wrap { trim: false }), popup);
    }
}

fn input_line(input: &super::playlist_form::Input, width: usize) -> Line<'static> {
    let before = &input.text[..input.cursor];
    let mut start = input.cursor;
    let mut used = 0;
    for (index, c) in before.char_indices().rev() {
        let w = Line::from(c.to_string()).width();
        if used + w > width.saturating_sub(3) { break; }
        used += w;
        start = index;
    }
    let after = &input.text[input.cursor..];
    let cursor = after.chars().next();
    let rest = &after[cursor.map_or(0, char::len_utf8)..];
    Line::from(vec![Span::raw(if start > 0 { " <" } else { "  " }),
        Span::raw(before[start..].to_owned()),
        Span::styled(cursor.unwrap_or(' ').to_string(), Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw(rest.to_owned())])
}

fn status(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = vec![app.status.label(), String::new()];
    if let Some(s) = &app.status.value {
        lines.extend([format!("Station: {}", s.station_name), format!("Timezone: {}", s.timezone),
            format!("Uptime: {}", uptime(s.uptime_seconds)),
            format!("PID: {}", s.pid)]);
    }
    lines.extend([String::new(), "Playlists".into(), app.playlists.label(), "Grid".into(), app.rules.label()]);
    frame.render_widget(Paragraph::new(lines.join("\n")).block(block("Station status"))
        .wrap(Wrap { trim: false }).scroll((app.detail_scroll, 0)), area);
}

fn panels(area: Rect) -> (Rect, Rect, Rect) {
    let outer = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(area);
    let direction = if area.width >= 90 { Direction::Horizontal } else { Direction::Vertical };
    let inner = Layout::default().direction(direction)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)]).split(outer[1]);
    (outer[0], inner[0], inner[1])
}

fn playlists(frame: &mut Frame, app: &mut App, area: Rect) {
    let (notice, list, details) = panels(area);
    frame.render_widget(Paragraph::new(app.playlists.label()).wrap(Wrap { trim: false }), notice);
    let items = app.playlists.value.as_deref().unwrap_or_default();
    if items.is_empty() {
        let label = if app.playlists.value.is_some() { "No playlists in the view." } else { "No data yet." };
        frame.render_widget(Paragraph::new(label).block(block("Playlists")), list);
    } else {
        let rows: Vec<ListItem> = items.iter().map(|p| ListItem::new(format!("{} [{}] {}", p.name, p.mode, enabled(p.enabled)))).collect();
        frame.render_stateful_widget(List::new(rows).block(block(format!("Playlists ({})", items.len())))
            .highlight_symbol("> ").highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            list, &mut app.playlist_selection);
    }
    let text = app.playlist_selection.selected().and_then(|i| items.get(i))
        .map(|p| format!("Name: {}\nID: {}\nPath: {}\nMode: {}\nState: {}", p.name, p.id,
            empty(&p.rel_path), p.mode, enabled(p.enabled)))
        .unwrap_or_else(|| "Select a playlist.".into());
    detail(frame, details, text, app.detail_scroll);
}

fn rules(frame: &mut Frame, app: &mut App, area: Rect) {
    let (notice, list, details) = panels(area);
    let tz = app.status.value.as_ref().map(|s| s.timezone.as_str()).unwrap_or("unknown");
    frame.render_widget(Paragraph::new(format!("{}\nCivil times in station timezone: {tz}", app.rules.label()))
        .wrap(Wrap { trim: false }), notice);
    let items = app.rules.value.as_deref().unwrap_or_default();
    if items.is_empty() {
        let label = if app.rules.value.is_some() { "No rules in the view." } else { "No data yet." };
        frame.render_widget(Paragraph::new(label).block(block("Grid rules")), list);
    } else {
        let rows: Vec<ListItem> = items.iter().map(|r| {
            let (kind, playlist) = rule_summary(r);
            ListItem::new(format!("{} [{}] {}\n  {}", r.id, kind, enabled(r.enabled), playlist))
        }).collect();
        frame.render_stateful_widget(List::new(rows).block(block(format!("Grid rules ({})", items.len())))
            .highlight_symbol("> ").highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            list, &mut app.rule_selection);
    }
    let text = app.rule_selection.selected().and_then(|i| items.get(i)).map(rule_detail)
        .unwrap_or_else(|| "Select a rule.".into());
    detail(frame, details, text, app.detail_scroll);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use crate::client::Snapshot;

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app, "http://127.0.0.1:50051")).unwrap();
        terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn unavailable_rules_are_not_presented_as_an_empty_grid() {
        let mut app = App { tab: 2, ..App::default() };
        app.apply(Snapshot { status: Err("offline".into()), playlists: Ok(vec![]),
            rules: Err("UNIMPLEMENTED: old daemon".into()) });
        let rendered = screen(&mut app, 120, 30);
        assert!(rendered.contains("UNIMPLEMENTED"));
        assert!(!rendered.contains("No rules in the view"));
    }

    #[test]
    fn views_handle_small_and_resized_terminals() {
        let mut app = App::default();
        for (width, height) in [(0, 0), (20, 5), (40, 12), (80, 24), (120, 36)] {
            for tab in 0..4 {
                app.tab = tab;
                screen(&mut app, width, height);
                app.help = true;
                screen(&mut app, width, height);
                app.help = false;
            }
        }
    }

    #[test]
    fn local_form_renders_offline_in_all_sections_and_keeps_long_input_visible() {
        use crate::playlist_form::PlaylistForm;
        let mut app = App { form: Some(PlaylistForm::new("/tmp/playlist".into())), ..App::default() };
        app.form.as_mut().unwrap().paste("a/very/long/directory/with/unicode/été/last-file.toml");
        for (width, height) in [(20, 5), (40, 16), (80, 24), (120, 36)] {
            for page in 0..3 {
                app.form.as_mut().unwrap().page = page;
                let rendered = screen(&mut app, width, height);
                if width >= 40 { assert!(rendered.contains("F4: section")); }
            }
        }
        app.form.as_mut().unwrap().page = 0;
        assert!(screen(&mut app, 40, 16).contains("last-file.toml"));
        app.form.as_mut().unwrap().discard = true;
        assert!(screen(&mut app, 80, 24).contains("Discard this unsaved playlist?"));
        let form = app.form.as_mut().unwrap();
        form.discard = false;
        form.preview = Some("name = \"Demo\"".into());
        let rendered = screen(&mut app, 80, 24);
        assert!(rendered.contains("TOML preview"));
        assert!(rendered.contains("Ctrl-S: save new file"));
    }

    #[test]
    fn agenda_renders_dst_rows_resize_fallback_and_rpc_failures() {
        use crate::agenda::Entry;
        use stationd::proto::schedule::decision::Origin;
        use crossterm::event::{KeyCode, KeyEvent};
        let mut app = App { tab: 3, ..App::default() };
        app.agenda.set_zone("Europe/Paris");
        app.agenda.handle(KeyEvent::from(KeyCode::Char('g')));
        app.agenda.paste_date("2026-10-25");
        app.agenda.handle(KeyEvent::from(KeyCode::Enter));
        let from = app.agenda.window.as_ref().unwrap().from;
        app.agenda.data.apply(Ok(vec![
            Entry { at: from, origin: Origin::BaseRotation, rule_id: "base".into(), playlist: "Hits".into() },
            Entry { at: "2026-10-25T00:30:00Z".parse().unwrap(), origin: Origin::AtClockSoft, rule_id: "news".into(), playlist: "News".into() },
            Entry { at: "2026-10-25T00:31:00Z".parse().unwrap(), origin: Origin::BaseRotation, rule_id: "base".into(), playlist: "Hits".into() },
            Entry { at: "2026-10-25T01:30:00Z".parse().unwrap(), origin: Origin::AtClockSoft, rule_id: "news".into(), playlist: "News".into() },
        ]));
        app.agenda.row = 2;
        app.agenda.scroll = 0;
        let rendered = screen(&mut app, 120, 36);
        assert!(rendered.contains("02:00a") && rendered.contains("02:00b"));
        assert!(rendered.contains("*02:30 News"));
        assert!(app.agenda.details().contains("UTC: 2026-10-25T00:30:00Z"));
        app.agenda.handle(KeyEvent::from(KeyCode::Char('w')));
        let narrow = screen(&mut app, 80, 30);
        assert!(narrow.contains("selected day"));
        let wide = screen(&mut app, 120, 36);
        assert!(wide.contains("Mon 19/10") && wide.contains("Sun 25/10"));
        assert!(!wide.contains("selected day (week needs"));
        app.agenda.data.apply(Err("Unimplemented: Preview unavailable".into()));
        assert!(screen(&mut app, 120, 36).contains("Unimplemented"));
        app.agenda.handle(KeyEvent::from(KeyCode::Char('g')));
        assert!(screen(&mut app, 80, 30).contains("Go to date"));
        for size in [(0, 0), (20, 5), (40, 12), (40, 22), (80, 24)] { screen(&mut app, size.0, size.1); }
    }
}
