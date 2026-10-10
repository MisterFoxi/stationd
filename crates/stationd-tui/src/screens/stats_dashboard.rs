//! Generic visual dataset supplied by a plugin, never inferred from its name.
use crate::{style::Styles, tr};
use ratatui_core::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    symbols::Marker,
    text::Line,
    widgets::Widget,
};
use ratatui_crossterm::crossterm::event::{KeyCode, KeyEvent};
use ratatui_widgets::{
    barchart::{Bar, BarChart},
    block::Block,
    borders::BorderType,
    chart::{Axis, Chart, Dataset, GraphType},
    paragraph::{Paragraph, Wrap},
};
use stationd_proto::plugin::{PluginDbQueryResponse, plugin_db_value::Kind};

#[derive(Clone, Debug)]
pub struct Datum {
    pub section: String,
    pub scope: String,
    pub label: String,
    pub bucket: String,
    pub value: Option<f64>,
    pub unit: String,
    pub samples: Option<f64>,
}
#[derive(Default)]
pub struct Dashboard {
    pub page: u8,
    pub point: usize,
    pub rank: usize,
    pub day: usize,
    pub hour: usize,
}
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
pub fn parse(data: &PluginDbQueryResponse) -> Result<Vec<Datum>, String> {
    let index = |name: &str| {
        data.columns
            .iter()
            .position(|c| c == name)
            .ok_or_else(|| tr!("stats-dashboard-invalid"))
    };
    let (section, scope, label, bucket, value, unit, samples) = (
        index("Section")?,
        index("Scope")?,
        index("Label")?,
        index("Bucket")?,
        index("Value")?,
        index("Unit")?,
        index("Samples")?,
    );
    let text = |r: &stationd_proto::plugin::PluginDbRow, i: usize| match r
        .values
        .get(i)
        .and_then(|v| v.kind.as_ref())
    {
        Some(Kind::Text(s)) => clean(s),
        _ => String::new(),
    };
    let num = |r: &stationd_proto::plugin::PluginDbRow, i: usize| match r
        .values
        .get(i)
        .and_then(|v| v.kind.as_ref())
    {
        Some(Kind::Integer(n)) => Some(*n as f64),
        Some(Kind::Real(n)) if n.is_finite() && *n >= 0.0 => Some(*n),
        _ => None,
    };
    Ok(data
        .rows
        .iter()
        .map(|r| Datum {
            section: text(r, section),
            scope: text(r, scope),
            label: text(r, label),
            bucket: text(r, bucket),
            value: num(r, value).filter(|v| *v >= 0.0),
            unit: text(r, unit),
            samples: num(r, samples),
        })
        .collect())
}
pub fn number(value: Option<f64>, unit: &str) -> String {
    let Some(n) = value else { return "—".into() };
    if unit == "s" {
        let s = n.round() as u64;
        return if s >= 3600 {
            format!("{} h {:02} min", s / 3600, s % 3600 / 60)
        } else if s >= 60 {
            format!("{} min {:02} s", s / 60, s % 60)
        } else {
            format!("{s} s")
        };
    }
    let v = if n.fract().abs() < 0.005 {
        format!("{n:.0}")
    } else {
        format!("{n:.2}").trim_end_matches('0').to_string()
    };
    if unit == "%" { format!("{v} %") } else { v }
}
fn panel(title: String, s: &Styles) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .border_style(s.border())
}
fn series(rows: &[Datum]) -> Vec<&Datum> {
    let mut v: Vec<_> = rows.iter().filter(|r| r.section == "series").collect();
    v.sort_by(|a, b| a.bucket.cmp(&b.bucket));
    v
}
impl Dashboard {
    pub fn handle(&mut self, k: &KeyEvent, rows: &[Datum]) -> bool {
        let points = series(rows).len();
        let ranks = rows.iter().filter(|r| r.section == "ranking").count();
        match k.code {
            KeyCode::Char('h') => self.page = (self.page + 1) % 3,
            KeyCode::Left if self.page == 1 => self.hour = self.hour.saturating_sub(1),
            KeyCode::Right if self.page == 1 => self.hour = (self.hour + 1).min(23),
            KeyCode::Up if self.page == 1 => self.day = self.day.saturating_sub(1),
            KeyCode::Down if self.page == 1 => self.day = (self.day + 1).min(6),
            KeyCode::Left => self.point = self.point.saturating_sub(1),
            KeyCode::Right => self.point = (self.point + 1).min(points.saturating_sub(1)),
            KeyCode::Up => self.rank = self.rank.saturating_sub(1),
            KeyCode::Down => self.rank = (self.rank + 1).min(ranks.saturating_sub(1)),
            KeyCode::Home => {
                self.point = 0;
                self.rank = 0;
            }
            KeyCode::End => {
                self.point = points.saturating_sub(1);
                self.rank = ranks.saturating_sub(1);
            }
            _ => return false,
        }
        true
    }
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        s: &Styles,
        rows: &[Datum],
        grouping: &str,
    ) {
        let summaries: Vec<_> = rows
            .iter()
            .filter(|r| r.section == "summary")
            .take(4)
            .collect();
        if summaries.is_empty() {
            Paragraph::new(tr!("plugins-empty"))
                .style(s.muted())
                .render(area, buf);
            return;
        }
        let cards_height = if area.width >= 76 { 3 } else { 6 };
        let [cards, main, note] = Layout::vertical([
            Constraint::Length(cards_height),
            Constraint::Fill(1),
            Constraint::Length(2),
        ])
        .spacing(1)
        .areas(area);
        let card_rows = if area.width >= 76 {
            vec![cards]
        } else {
            Layout::vertical([Constraint::Length(3), Constraint::Length(3)])
                .split(cards)
                .to_vec()
        };
        for (line, r) in card_rows.iter().enumerate() {
            let per = if area.width >= 76 { 4 } else { 2 };
            let cells = Layout::horizontal(vec![Constraint::Fill(1); per])
                .spacing(1)
                .split(*r);
            for (j, a) in cells.iter().enumerate() {
                if let Some(d) = summaries.get(line * per + j) {
                    Paragraph::new(number(d.value, &d.unit))
                        .alignment(Alignment::Center)
                        .style(s.accent())
                        .block(panel(d.label.clone(), s))
                        .render(*a, buf);
                }
            }
        }
        match self.page {
            1 => self.heatmap(main, buf, s, rows),
            2 => self.ranking(main, buf, s, rows),
            _ if main.width >= 85 => {
                let [chart, rank] =
                    Layout::horizontal([Constraint::Percentage(60), Constraint::Fill(1)])
                        .spacing(1)
                        .areas(main);
                self.chart(chart, buf, s, rows, grouping);
                self.ranking(rank, buf, s, rows);
            }
            _ if main.height >= 17 => {
                let [chart, rank] =
                    Layout::vertical([Constraint::Percentage(58), Constraint::Fill(1)])
                        .spacing(1)
                        .areas(main);
                self.chart(chart, buf, s, rows, grouping);
                self.ranking(rank, buf, s, rows);
            }
            _ => self.chart(main, buf, s, rows, grouping),
        }
        let mut lines = vec![Line::styled(tr!("stats-dashboard-keys"), s.muted())];
        if let Some(d) = rows.iter().find(|r| r.section == "note") {
            lines.push(Line::styled(d.label.clone(), s.muted()));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(note, buf);
    }
    fn chart(&mut self, area: Rect, buf: &mut Buffer, s: &Styles, rows: &[Datum], grouping: &str) {
        let points = series(rows);
        self.point = self.point.min(points.len().saturating_sub(1));
        let title = points
            .first()
            .map(|r| format!("{} · {} · UTC", tr!("stats-evolution"), r.label))
            .unwrap_or_else(|| tr!("stats-evolution"));
        let block = panel(title, s);
        let inner = block.inner(area);
        block.render(area, buf);
        if points.is_empty() {
            Paragraph::new(tr!("plugins-empty"))
                .style(s.muted())
                .render(inner, buf);
            return;
        }
        let [plot, detail] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(inner);
        let maximum = points
            .iter()
            .filter_map(|r| r.value)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        // Only adjacent observed buckets are connected. Missing/failed buckets break the line.
        let segments = segments(&points, grouping);
        let mut datasets: Vec<_> = segments
            .iter()
            .map(|p| {
                Dataset::default()
                    .marker(Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(s.accent())
                    .data(p)
            })
            .collect();
        let selected = points[self.point];
        let cursor = selected
            .value
            .map(|v| vec![(coordinate(&selected.bucket, self.point), v)])
            .unwrap_or_default();
        datasets.push(
            Dataset::default()
                .marker(Marker::Block)
                .graph_type(GraphType::Scatter)
                .style(s.warn())
                .data(&cursor),
        );
        let min = coordinate(&points[0].bucket, 0);
        let max = coordinate(&points[points.len() - 1].bucket, points.len() - 1).max(min + 1.0);
        Chart::new(datasets)
            .x_axis(Axis::default().style(s.muted()).bounds([min, max]).labels([
                short_date(&points[0].bucket),
                short_date(&points[points.len() - 1].bucket),
            ]))
            .y_axis(
                Axis::default()
                    .style(s.muted())
                    .bounds([0.0, maximum])
                    .labels(["0".to_string(), number(Some(maximum), "")]),
            )
            .render(plot, buf);
        let text = format!(
            "▸ {} · {} {} · {} {}",
            selected.bucket,
            number(selected.value, &selected.unit),
            if selected.unit == "s" || selected.unit == "%" {
                ""
            } else {
                selected.unit.as_str()
            },
            number(selected.samples, ""),
            tr!("stats-samples")
        );
        Paragraph::new(text)
            .style(s.label())
            .wrap(Wrap { trim: false })
            .render(detail, buf);
    }
    fn ranking(&mut self, area: Rect, buf: &mut Buffer, s: &Styles, rows: &[Datum]) {
        let ranks: Vec<_> = rows.iter().filter(|r| r.section == "ranking").collect();
        self.rank = self.rank.min(ranks.len().saturating_sub(1));
        let title = format!(
            "{} · {}",
            tr!("stats-ranking"),
            ranks.first().map(|r| r.unit.as_str()).unwrap_or("")
        );
        let block = panel(title, s);
        let inner = block.inner(area);
        block.render(area, buf);
        if ranks.is_empty() {
            Paragraph::new(tr!("plugins-empty"))
                .style(s.muted())
                .render(inner, buf);
            return;
        }
        let [list, detail] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(inner);
        let count = (list.height as usize / 2).max(1);
        let start = self.rank.saturating_sub(count - 1);
        let maximum = ranks
            .iter()
            .filter_map(|r| r.value)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        for (i, d) in ranks.iter().enumerate().skip(start).take(count) {
            let y = list.y + ((i - start) * 2) as u16;
            if y >= list.bottom() {
                break;
            }
            let selected = i == self.rank;
            let text = format!(
                "{} {}. {} · {}",
                if selected { "▸" } else { " " },
                i + 1,
                d.label,
                number(d.value, &d.unit)
            );
            Paragraph::new(text)
                .style(if selected { s.tab_active() } else { s.label() })
                .render(Rect::new(list.x, y, list.width, 1), buf);
            if y + 1 < list.bottom() {
                let v = (d.value.unwrap_or(0.0) / maximum * 100000.0).round() as u64;
                BarChart::horizontal(vec![Bar::new(v).text_value("")])
                    .max(100000)
                    .bar_width(1)
                    .bar_gap(0)
                    .bar_style(if selected { s.accent() } else { s.calm() })
                    .render(Rect::new(list.x, y + 1, list.width, 1), buf);
            }
        }
        let d = ranks[self.rank];
        Paragraph::new(format!(
            "{} · {} {}",
            d.label,
            number(d.value, &d.unit),
            if d.unit == "s" { "" } else { d.unit.as_str() }
        ))
        .style(s.muted())
        .wrap(Wrap { trim: false })
        .render(detail, buf);
    }
    fn heatmap(&mut self, area: Rect, buf: &mut Buffer, s: &Styles, rows: &[Datum]) {
        let block = panel(format!("{} · UTC", tr!("stats-heatmap")), s);
        let inner = block.inner(area);
        block.render(area, buf);
        let days = [
            tr!("stats-mon"),
            tr!("stats-tue"),
            tr!("stats-wed"),
            tr!("stats-thu"),
            tr!("stats-fri"),
            tr!("stats-sat"),
            tr!("stats-sun"),
        ];
        let heat: Vec<_> = rows.iter().filter(|r| r.section == "heatmap").collect();
        let max = heat
            .iter()
            .filter_map(|r| r.value)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        let cell = ((inner.width.saturating_sub(5)) / 24).max(1);
        let labels_y = inner.y;
        if inner.width >= 29 {
            for h in [0, 6, 12, 18, 23] {
                let x = inner.x + 5 + h * cell;
                if x < inner.right() {
                    buf.set_stringn(
                        x,
                        labels_y,
                        format!("{h:02}"),
                        (inner.right() - x) as usize,
                        s.muted(),
                    );
                }
            }
            let visible = inner.height.saturating_sub(3).min(7) as usize;
            let first = self.day.saturating_sub(visible.saturating_sub(1));
            for (day, label) in days.iter().enumerate().skip(first).take(visible) {
                let y = inner.y + 1 + (day - first) as u16;
                if y >= inner.bottom() {
                    break;
                }
                buf.set_stringn(inner.x, y, label, 4, s.muted());
                for hour in 0..24 {
                    let x = inner.x + 5 + hour as u16 * cell;
                    if x >= inner.right() {
                        break;
                    }
                    let d = heat.iter().find(|r| {
                        r.label.parse::<usize>().ok() == Some(day + 1)
                            && r.bucket.parse::<usize>().ok() == Some(hour)
                    });
                    let value = d.and_then(|r| r.value);
                    let symbol = match value {
                        None => "·",
                        Some(v) if v == 0.0 => "_",
                        Some(v) => ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"]
                            [((v / max * 7.0).round() as usize).min(7)],
                    };
                    let selected = day == self.day && hour == self.hour;
                    let style = if selected {
                        s.tab_active()
                    } else if value.is_none() {
                        s.muted()
                    } else {
                        s.accent()
                    };
                    buf.set_stringn(x, y, symbol, cell as usize, style);
                }
            }
        }
        let selected = heat.iter().find(|r| {
            r.label.parse::<usize>().ok() == Some(self.day + 1)
                && r.bucket.parse::<usize>().ok() == Some(self.hour)
        });
        let value = selected
            .map(|r| {
                format!(
                    "{} {} · {} {}",
                    number(r.value, &r.unit),
                    r.unit,
                    number(r.samples, ""),
                    tr!("stats-samples")
                )
            })
            .unwrap_or_else(|| tr!("stats-no-sample"));
        let hint = format!(
            "▸ {} {:02} h · {}\n{}",
            days[self.day],
            self.hour,
            value,
            tr!("stats-heatmap-legend")
        );
        let height = inner.height.min(2);
        Paragraph::new(hint)
            .style(s.label())
            .wrap(Wrap { trim: false })
            .render(
                Rect::new(
                    inner.x,
                    inner.bottom().saturating_sub(height),
                    inner.width,
                    height,
                ),
                buf,
            );
    }
}
fn short_date(bucket: &str) -> String {
    if bucket.is_ascii() && bucket.len() >= 10 && bucket.as_bytes().get(4) == Some(&b'-') {
        format!(
            "{}{}",
            &bucket[5..10],
            if bucket.len() > 10 { &bucket[10..] } else { "" }
        )
    } else {
        bucket.into()
    }
}
fn coordinate(bucket: &str, fallback: usize) -> f64 {
    if let Ok(d) = chrono::NaiveDateTime::parse_from_str(bucket, "%Y-%m-%d %H:%M") {
        return d.and_utc().timestamp() as f64;
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(bucket, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as f64;
    }
    if bucket.is_ascii() {
        if bucket.len() == 7 && bucket.as_bytes()[4] == b'-' {
            if let (Ok(year), Ok(month)) = (bucket[..4].parse::<u32>(), bucket[5..].parse::<u32>())
            {
                return (year * 12 + month) as f64;
            }
        }
        if let Ok(n) = bucket.trim_end_matches(":00").parse::<u32>() {
            return n as f64;
        }
    }
    fallback as f64
}
fn segments(points: &[&Datum], grouping: &str) -> Vec<Vec<(f64, f64)>> {
    let mut segments = Vec::new();
    let mut line = Vec::new();
    let mut last = None;
    for (i, r) in points.iter().enumerate() {
        let x = coordinate(&r.bucket, i);
        let gap = last.is_some_and(|prev: f64| {
            x - prev
                > if r.bucket.len() == 16 {
                    3600.0
                } else if grouping == "Semaine" {
                    604800.0
                } else if r.bucket.len() == 10 {
                    86400.0
                } else {
                    1.0
                }
        });
        if gap || r.value.is_none() {
            if !line.is_empty() {
                segments.push(std::mem::take(&mut line));
            }
        }
        if let Some(v) = r.value {
            line.push((x, v));
        }
        last = Some(x);
    }
    if !line.is_empty() {
        segments.push(line);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use stationd_proto::plugin::{PluginDbRow, PluginDbValue};
    fn datum(section: &str, label: &str, bucket: &str, value: Option<f64>, unit: &str) -> Datum {
        Datum {
            section: section.into(),
            scope: "/radio".into(),
            label: label.into(),
            bucket: bucket.into(),
            value,
            unit: unit.into(),
            samples: Some(120.0),
        }
    }
    fn fixture() -> Vec<Datum> {
        let mut r = vec![
            datum("summary", "Audience moyenne", "", Some(154.8), "auditeurs"),
            datum("summary", "Pic d’audience", "", Some(312.0), "auditeurs"),
            datum("summary", "Relevés valides", "", Some(8640.0), "relevés"),
            datum("summary", "Collecte réussie", "", Some(97.6), "%"),
        ];
        for h in 0..24 {
            r.push(datum(
                "series",
                "Audience moyenne",
                &format!("2026-10-09 {h:02}:00"),
                if h == 8 {
                    None
                } else {
                    Some(50.0 + (h as f64 / 3.0).sin().abs() * 190.0)
                },
                "auditeurs",
            ));
        }
        for (name, value) in [
            ("FR", 104.8),
            ("BE", 22.3),
            ("CH", 15.1),
            ("CA", 8.6),
            ("Inconnu", 4.0),
        ] {
            r.push(datum("ranking", name, "", Some(value), "auditeurs"));
        }
        for d in 1..=7 {
            for h in 0..24 {
                if h == 3 && d == 2 {
                    continue;
                }
                r.push(datum(
                    "heatmap",
                    &d.to_string(),
                    &format!("{h:02}"),
                    Some(if h < 6 { 0.0 } else { (h * d) as f64 }),
                    "auditeurs",
                ));
            }
        }
        r.push(datum(
            "note",
            "Moyennes par relevé réussi · zéro inclus",
            "",
            None,
            "",
        ));
        r
    }
    #[test]
    fn missing_samples_split_curve_and_never_become_zero() {
        let rows = vec![
            datum(
                "series",
                "Audience",
                "2024-01-01 00:00",
                Some(0.0),
                "auditeurs",
            ),
            datum("series", "Audience", "2024-01-01 01:00", None, "auditeurs"),
            datum(
                "series",
                "Audience",
                "2024-01-01 02:00",
                Some(10.0),
                "auditeurs",
            ),
            datum(
                "series",
                "Audience",
                "2024-01-01 04:00",
                Some(20.0),
                "auditeurs",
            ),
        ];
        let points = series(&rows);
        let parts = segments(&points, "Heure");
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0][0].1, 0.0);
        assert_eq!(number(None, "s"), "—");
        assert_eq!(number(Some(3660.0), "s"), "1 h 01 min");
    }
    #[test]
    fn malformed_plugin_data_is_rejected_and_text_is_sanitized() {
        assert!(parse(&PluginDbQueryResponse::default()).is_err());
        let data = PluginDbQueryResponse {
            columns: [
                "Section", "Scope", "Label", "Bucket", "Value", "Unit", "Samples",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            rows: vec![PluginDbRow {
                values: [
                    Kind::Text("summary".into()),
                    Kind::Text("/r".into()),
                    Kind::Text("a\n\u{1b}b".into()),
                    Kind::Text("éé-🥰".into()),
                    Kind::Real(f64::NAN),
                    Kind::Text("auditeurs".into()),
                    Kind::Integer(0),
                ]
                .into_iter()
                .map(|kind| PluginDbValue { kind: Some(kind) })
                .collect(),
            }],
        };
        let r = parse(&data).unwrap();
        assert_eq!(r[0].label, "a  b");
        assert!(r[0].value.is_none());
        assert_eq!(short_date(&r[0].bucket), "éé-🥰");
    }
    #[test]
    fn dashboard_renders_all_panels_at_terminal_sizes() {
        let theme = rat_theme4::create_salsa_theme("Imperial");
        let s = Styles(&theme);
        let rows = fixture();
        let mut state = Dashboard::default();
        for (w, h) in [(120, 32), (90, 24), (80, 24), (50, 18), (35, 12)] {
            for page in 0..3 {
                state.page = page;
                let area = Rect::new(0, 0, w, h);
                let mut b = Buffer::empty(area);
                state.render(area, &mut b, &s, &rows, "Heure");
                let text: String = (0..h)
                    .flat_map(|y| (0..w).map(move |x| (x, y)))
                    .map(|p| b[p].symbol())
                    .collect();
                assert!(text.contains("154.8"));
            }
        }
        if let Ok(path) = std::env::var("STATIOND_DASHBOARD_PREVIEW") {
            state.page = 0;
            let area = Rect::new(0, 0, 120, 32);
            let mut b = Buffer::empty(area);
            state.render(area, &mut b, &s, &rows, "Heure");
            let cells:Vec<_>=(0..32).flat_map(|y|(0..120).map(move|x|(x,y))).map(|p|{let c=&b[p];serde_json::json!({"x":p.0,"y":p.1,"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg)})}).collect();
            std::fs::write(
                path,
                serde_json::json!({"width":120,"height":32,"cells":cells}).to_string(),
            )
            .unwrap();
        }
    }
}
