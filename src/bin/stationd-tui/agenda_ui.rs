use jiff::tz::TimeZone;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};
use stationd::{
    playlist::normalize_ref,
    proto::{
        schedule::{self, decision::Origin},
        station,
    },
};

use super::{
    agenda::{intersecting, Agenda, Entry},
    app::Resource,
};

fn block(title: &str) -> Block<'_> {
    Block::default().title(title).borders(Borders::ALL)
}

fn color(entry: &Entry) -> Color {
    match entry.origin {
        Origin::AtClockHard | Origin::Fallback => Color::LightRed,
        Origin::AtClockSoft => Color::Yellow,
        Origin::DayPart => Color::LightGreen,
        _ => Color::Cyan,
    }
}

fn ref_name(playlist_ref: &str, playlists: &Resource<Vec<station::PlaylistSummary>>) -> String {
    let key = normalize_ref(playlist_ref).ok();
    playlists
        .value
        .as_ref()
        .and_then(|items| {
            items
                .iter()
                .find(|p| key.is_some() && normalize_ref(&p.rel_path).ok() == key)
        })
        .map(|p| p.name.clone())
        .unwrap_or_else(|| playlist_ref.to_string())
}

fn source_name(entry: &Entry, playlists: &Resource<Vec<station::PlaylistSummary>>) -> String {
    if entry.origin == Origin::Fallback {
        return "Fallback".into();
    }
    ref_name(&entry.playlist, playlists)
}

pub fn draw(
    frame: &mut Frame,
    agenda: &mut Agenda,
    rules: &Resource<Vec<schedule::Rule>>,
    playlists: &Resource<Vec<station::PlaylistSummary>>,
    area: Rect,
) {
    if area.height < 10 || area.width < 40 {
        frame.render_widget(
            Paragraph::new(
                "Agenda: enlarge terminal (at least 40 x 22).\nDates and selection are preserved.",
            )
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let Some(window) = agenda.window.clone() else {
        let message = if agenda.message.is_empty() {
            "Waiting for the station timezone (Status RPC)."
        } else {
            &agenda.message
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(block("Agenda"))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };
    let selected_day = agenda.selected_day();
    // Preserve Week selection/window on resize; show its selected day when
    // seven columns cannot be read. Widening restores all columns immediately.
    let show_week = agenda.week && area.width >= 105;
    let shown: Vec<usize> = if show_week {
        (0..agenda.days.len()).collect()
    } else {
        vec![selected_day]
    };
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    let title = if agenda.week {
        format!(
            "Week {} - {} | {} | {} min{}",
            agenda.days[0].date,
            agenda.days.last().unwrap().date,
            window.zone,
            agenda.step,
            if show_week {
                ""
            } else {
                " | selected day (week needs 105 cols)"
            }
        )
    } else {
        format!(
            "Day {} | {} | {} min",
            agenda.date.unwrap(),
            window.zone,
            agenda.step
        )
    };
    frame.render_widget(
        Paragraph::new(title)
            .style(Style::default().fg(Color::Cyan))
            .wrap(Wrap { trim: false }),
        layout[0],
    );
    let notice = if !agenda.message.is_empty() {
        agenda.message.clone()
    } else {
        format!(
            "{}{}",
            agenda.data.label(),
            if agenda.in_flight.is_some() {
                " | loading Preview..."
            } else {
                ""
            }
        )
    };
    frame.render_widget(
        Paragraph::new(notice)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(
                if agenda.data.error.is_some() || !agenda.message.is_empty() {
                    Color::Yellow
                } else {
                    Color::Gray
                },
            )),
        layout[1],
    );

    let visible = usize::from(layout[2].height.saturating_sub(3)).max(1);
    if agenda.row < agenda.scroll {
        agenda.scroll = agenda.row;
    }
    if agenda.row >= agenda.scroll + visible {
        agenda.scroll = agenda.row + 1 - visible;
    }
    let mut headers = vec![Cell::from("Time")];
    for &d in &shown {
        let date = agenda.days[d].date;
        let weekday = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
            [date.weekday().to_monday_zero_offset() as usize];
        let hours =
            (agenda.days[d].to.as_second() - agenda.days[d].from.as_second()) as f64 / 3600.0;
        let text = if show_week {
            format!("{weekday} {:02}/{:02}", date.day(), date.month())
        } else {
            format!("{weekday} {date} | {hours}h day")
        };
        headers.push(Cell::from(text).style(if d == selected_day {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(Color::Cyan)
        }));
    }
    let data = agenda.data.value.as_deref().unwrap_or_default();
    let tz = TimeZone::get(&window.zone).expect("calendar validated this timezone");
    let mut rows = Vec::new();
    for r in agenda.scroll..(agenda.scroll + visible).min(agenda.axis.len()) {
        let key = agenda.axis[r];
        let repeated = agenda
            .axis
            .iter()
            .any(|k| k.minute == key.minute && k.fold > 0);
        let suffix = if repeated {
            if key.fold == 0 {
                "a"
            } else {
                "b"
            }
        } else {
            ""
        };
        let mut cells = vec![Cell::from(format!(
            "{:02}:{:02}{suffix}",
            key.minute / 60,
            key.minute % 60
        ))];
        for &d in &shown {
            let (text, style) = if let Some(slot) = agenda.slot(d, r) {
                let indices = intersecting(data, slot);
                if let Some(&i) = indices
                    .iter()
                    .find(|&&i| data[i].is_mark())
                    .or_else(|| indices.first())
                {
                    let entry = &data[i];
                    let time = entry.at.to_zoned(tz.clone());
                    let label = if entry.is_mark() {
                        format!(
                            "{}{:02}:{:02} {}",
                            entry.symbol(),
                            time.hour(),
                            time.minute(),
                            source_name(entry, playlists)
                        )
                    } else {
                        format!("{} {}", entry.symbol(), source_name(entry, playlists))
                    };
                    // Put the count first so clipping narrow week columns cannot
                    // conceal additional transitions/events inside this bucket.
                    let label = if indices.len() > 1 {
                        format!("+{} {label}", indices.len() - 1)
                    } else {
                        label
                    };
                    (label, Style::default().fg(color(entry)))
                } else {
                    (
                        if agenda.data.value.is_none() {
                            "..."
                        } else {
                            "(none)"
                        }
                        .into(),
                        Style::default().fg(Color::Gray),
                    )
                }
            } else {
                (
                    if key.fold == 0 { "DST gap" } else { "--" }.into(),
                    Style::default().fg(Color::DarkGray),
                )
            };
            let selected = d == selected_day && r == agenda.row;
            cells.push(Cell::from(text).style(if selected {
                style.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else {
                style
            }));
        }
        rows.push(Row::new(cells));
    }
    let mut widths = vec![Constraint::Length(7)];
    widths.extend(shown.iter().map(|_| Constraint::Fill(1)));
    frame.render_widget(
        Table::new(rows, widths)
            .header(Row::new(headers))
            .column_spacing(1)
            .block(block(
                "Clock projection | Up/Down: time | Left/Right: day | Enter: all events",
            )),
        layout[2],
    );

    let detail = if let Some(slot) = agenda.slot(selected_day, agenda.row) {
        let names: Vec<_> = intersecting(data, slot)
            .into_iter()
            .map(|i| format!("{} {}", data[i].symbol(), source_name(&data[i], playlists)))
            .collect();
        format!("{} | UTC {}\n{}\n! hard  * soft  D day part  B base  F fallback | +N: more entries; Enter",
            slot.from.to_zoned(tz), slot.from, names.join(" / "))
    } else {
        "Missing/repeated civil-time row: no instant for the selected day.".into()
    };
    frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), layout[3]);
    // Track-cadence `every` rules can't be placed on a clock, so they never
    // appear in the projection above — list them here so they stay visibly in
    // play. Elapsed-cadence `every` rules *are* projected, up in the grid.
    let tracks_every: Vec<String> = rules
        .value
        .as_ref()
        .map(|rs| {
            rs.iter()
                .filter(|r| r.enabled)
                .filter_map(|r| match &r.kind {
                    Some(schedule::rule::Kind::Every(e)) => match e.cadence {
                        Some(schedule::every::Cadence::Tracks(_)) => {
                            Some(ref_name(&e.playlist_ref, playlists))
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let cadence = if rules.value.is_none() {
        "Every, track-cadence: unavailable (see Grid).".to_string()
    } else {
        let stale = if rules.error.is_some() { " STALE" } else { "" };
        if tracks_every.is_empty() {
            format!("Every, track-cadence: none (elapsed cadences are projected above).{stale}")
        } else {
            format!(
                "Every, track-cadence (not projected): {}{stale}",
                tracks_every.join(", ")
            )
        }
    };
    frame.render_widget(
        Paragraph::new(cadence).style(Style::default().fg(Color::Gray)),
        layout[4],
    );

    if let Some(input) = &agenda.date_input {
        let outer = frame.area();
        let width = outer.width.min(60);
        let height = outer.height.min(7);
        let popup = Rect::new(
            outer.x + (outer.width - width) / 2,
            outer.y + (outer.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(format!(
                "Date: {input}_\nYYYY-MM-DD | station timezone\nEnter: go | Esc: cancel\n{}",
                agenda.message
            ))
            .block(block("Go to date"))
            .wrap(Wrap { trim: false }),
            popup,
        );
    }
}
