//! Native rat-widget date calendar with explicit UTC time controls.
use crate::{style::Styles, tr};
use chrono::{Datelike, Duration, Locale, Months, NaiveDate, NaiveDateTime, Timelike, Utc};
use rat_widget::calendar::{CalendarState, Month, selection::SingleSelection};
use ratatui_core::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    text::Line,
    widgets::{StatefulWidget, Widget},
};
use ratatui_crossterm::crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui_widgets::{
    block::Block,
    borders::BorderType,
    paragraph::{Paragraph, Wrap},
};

pub enum DateOutcome {
    Continue,
    Cancel,
    Selected(String),
}
pub struct DatePicker {
    calendar: CalendarState<1, SingleSelection>,
    hour: u32,
    minute: u32,
    second: u32,
    focus: usize,
    digits: String,
    error: Option<String>,
    pub title: String,
    end: bool,
}
pub fn locale() -> Locale {
    match crate::i18n::language() {
        "en" => Locale::en_GB,
        "de" => Locale::de_DE,
        _ => Locale::fr_FR,
    }
}
pub fn display(value: &str) -> String {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .map(|d| {
            d.and_utc()
                .format_localized("%d %b %Y · %H:%M:%S UTC", locale())
                .to_string()
        })
        .unwrap_or_else(|_| tr!("stats-date-auto"))
}
pub fn preset_bounds(period: &str, now: NaiveDateTime) -> (String, String) {
    let from = match period {
        "Aujourd’hui" => now.date().and_hms_opt(0, 0, 0).unwrap(),
        "7 jours" => now - Duration::days(7),
        "30 jours" => now - Duration::days(30),
        "90 jours" => now - Duration::days(90),
        "365 jours" => now - Duration::days(365),
        "Tout" => NaiveDate::from_ymd_opt(1970, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
        _ => now - Duration::days(1),
    };
    (
        from.format("%Y-%m-%d %H:%M:%S").to_string(),
        (now + Duration::seconds(1))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
}
impl DatePicker {
    pub fn new(value: &str, title: String, end: bool) -> Self {
        let seed = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
            .unwrap_or_else(|_| Utc::now().naive_utc());
        let mut calendar = CalendarState::<1, SingleSelection>::default();
        calendar.set_start_date(seed.date());
        calendar.selection.borrow_mut().select(seed.date());
        calendar.focus.set(true);
        calendar.months[0].focus.set(true);
        Self {
            calendar,
            hour: seed.hour(),
            minute: seed.minute(),
            second: seed.second(),
            focus: 0,
            digits: String::new(),
            error: None,
            title,
            end,
        }
    }
    fn date(&self) -> NaiveDate {
        self.calendar.selection.borrow().selected().unwrap()
    }
    fn set_date(&mut self, date: NaiveDate) {
        if !(0..=9999).contains(&date.year()) {
            return;
        }
        self.calendar.selection.borrow_mut().select(date);
        self.calendar.set_start_date(date);
        self.digits.clear();
        self.error = None;
    }
    pub fn value(&self) -> String {
        self.date()
            .and_hms_opt(self.hour, self.minute, self.second)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }
    fn move_days(&mut self, n: i64) {
        if let Some(d) = self.date().checked_add_signed(Duration::days(n)) {
            self.set_date(d);
        }
    }
    fn move_months(&mut self, n: i32) {
        let d = if n < 0 {
            self.date()
                .checked_sub_months(Months::new(n.unsigned_abs()))
        } else {
            self.date().checked_add_months(Months::new(n as u32))
        };
        if let Some(d) = d {
            self.set_date(d);
        }
    }
    fn field(&mut self) -> (&mut u32, u32) {
        match self.focus {
            1 => (&mut self.hour, 24),
            2 => (&mut self.minute, 60),
            _ => (&mut self.second, 60),
        }
    }
    pub fn handle(&mut self, event: &Event) -> DateOutcome {
        if let Event::Mouse(m) = event {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                let day = self.calendar.months[0].area_days.iter().position(|r| {
                    m.column >= r.x && m.column < r.right() && m.row >= r.y && m.row < r.bottom()
                });
                if let Some(i) = day {
                    if let Some(d) = self.calendar.start_date().with_day(i as u32 + 1) {
                        self.set_date(d);
                        self.focus = 0;
                    }
                }
            }
            return DateOutcome::Continue;
        }
        let Event::Key(k) = event else {
            return DateOutcome::Continue;
        };
        if k.kind != KeyEventKind::Press {
            return DateOutcome::Continue;
        }
        match k.code {
            KeyCode::Esc => return DateOutcome::Cancel,
            KeyCode::Enter if self.error.is_none() => return DateOutcome::Selected(self.value()),
            KeyCode::Tab => {
                self.focus = (self.focus + 1) % 4;
                self.digits.clear();
            }
            KeyCode::BackTab => {
                self.focus = (self.focus + 3) % 4;
                self.digits.clear();
            }
            KeyCode::PageUp => self.move_months(if k.modifiers.contains(KeyModifiers::CONTROL) {
                -12
            } else {
                -1
            }),
            KeyCode::PageDown => self.move_months(if k.modifiers.contains(KeyModifiers::CONTROL) {
                12
            } else {
                1
            }),
            KeyCode::Home if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_date(Utc::now().date_naive())
            }
            KeyCode::Home if self.focus == 0 => self.set_date(self.date().with_day(1).unwrap()),
            KeyCode::End if self.focus == 0 => {
                if let Some(next) = self
                    .date()
                    .with_day(1)
                    .unwrap()
                    .checked_add_months(Months::new(1))
                {
                    self.set_date(next - Duration::days(1));
                }
            }
            KeyCode::Char('t') => self.set_date(Utc::now().date_naive()),
            KeyCode::Char('n') => {
                let now = Utc::now();
                self.set_date(now.date_naive());
                self.hour = now.hour();
                self.minute = now.minute();
                self.second = now.second();
            }
            KeyCode::Char('0') if self.focus == 0 => {
                self.hour = 0;
                self.minute = 0;
                self.second = 0;
            }
            KeyCode::Char('e') if self.end => {
                self.move_days(1);
                self.hour = 0;
                self.minute = 0;
                self.second = 0;
            }
            KeyCode::Left if self.focus == 0 => self.move_days(-1),
            KeyCode::Right if self.focus == 0 => self.move_days(1),
            KeyCode::Up if self.focus == 0 => self.move_days(-7),
            KeyCode::Down if self.focus == 0 => self.move_days(7),
            KeyCode::Left => {
                self.focus = self.focus.saturating_sub(1);
                self.digits.clear();
            }
            KeyCode::Right => {
                self.focus = (self.focus + 1).min(3);
                self.digits.clear();
            }
            KeyCode::Up | KeyCode::Down => {
                let up = k.code == KeyCode::Up;
                let (v, limit) = self.field();
                *v = if up {
                    (*v + 1) % limit
                } else {
                    (*v + limit - 1) % limit
                };
                self.digits.clear();
                self.error = None;
            }
            KeyCode::Char(c)
                if self.focus > 0
                    && c.is_ascii_digit()
                    && !k
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if self.digits.len() == 2 {
                    self.digits.clear();
                }
                self.digits.push(c);
                let n: u32 = self.digits.parse().unwrap();
                let (v, limit) = self.field();
                if n < limit {
                    *v = n;
                    self.error = None;
                } else {
                    self.error = Some(tr!("stats-time-invalid", max = limit - 1));
                }
            }
            _ => {}
        }
        self.calendar.focus.set(self.focus == 0);
        self.calendar.months[0].focus.set(self.focus == 0);
        DateOutcome::Continue
    }
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let [heading, body, preview, help] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .areas(area);
        Paragraph::new(Line::styled(format!("{} · UTC", self.title), s.title()))
            .render(heading, buf);
        let [cal, time] = Layout::horizontal([
            Constraint::Length(34.min(body.width.saturating_sub(16))),
            Constraint::Fill(1),
        ])
        .spacing(2)
        .areas(body);
        let today = std::collections::HashMap::from([(Utc::now().date_naive(), s.accent())]);
        if cal.width >= 28 && cal.height >= 9 {
            Month::new()
                .locale(locale())
                .show_weekdays()
                .style(s.base())
                .title_style(s.title())
                .weekday_style(s.muted())
                .day_style(s.base())
                .day_styles(&today)
                .select_style(s.tab_active())
                .focus_style(s.tab_active())
                .title_align(Alignment::Center)
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(if self.focus == 0 {
                            s.accent()
                        } else {
                            s.border()
                        }),
                )
                .render(cal, buf, &mut self.calendar.months[0]);
        } else {
            Paragraph::new(self.date().format("%Y-%m-%d").to_string())
                .style(s.accent())
                .render(cal, buf);
        }
        let labels = [
            tr!("stats-hours"),
            tr!("stats-minutes"),
            tr!("stats-seconds"),
        ];
        let clock = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Fill(1),
        ])
        .split(time);
        for (i, v) in [self.hour, self.minute, self.second]
            .into_iter()
            .enumerate()
        {
            let focused = self.focus == i + 1;
            Paragraph::new(format!("{:02}  {}", v, if focused { "▲ ▼" } else { "" }))
                .alignment(Alignment::Center)
                .style(if focused { s.accent() } else { s.base() })
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .title(labels[i].clone())
                        .border_style(if focused { s.accent() } else { s.border() }),
                )
                .render(clock[i], buf);
        }
        Paragraph::new(display(&self.value()))
            .alignment(Alignment::Center)
            .style(s.accent())
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(tr!("stats-date-selected"))
                    .border_style(s.border()),
            )
            .render(preview, buf);
        let hint = self.error.clone().unwrap_or_else(|| {
            if self.end {
                tr!("stats-calendar-end-help")
            } else {
                tr!("stats-calendar-help")
            }
        });
        Paragraph::new(hint)
            .style(if self.error.is_some() {
                s.error()
            } else {
                s.muted()
            })
            .wrap(Wrap { trim: false })
            .render(help, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> Event {
        Event::Key(ratatui_crossterm::crossterm::event::KeyEvent::new(
            code,
            KeyModifiers::NONE,
        ))
    }
    #[test]
    fn calendar_handles_leap_month_and_preserves_time() {
        let mut p = DatePicker::new("2024-01-31 12:34:56", "Date".into(), false);
        p.handle(&key(KeyCode::PageDown));
        assert_eq!(p.value(), "2024-02-29 12:34:56");
        p.handle(&key(KeyCode::Right));
        assert_eq!(p.value(), "2024-03-01 12:34:56");
        p.handle(&key(KeyCode::Left));
        assert_eq!(p.value(), "2024-02-29 12:34:56");
    }
    #[test]
    fn clock_wraps_and_invalid_digits_cannot_be_accepted() {
        let mut p = DatePicker::new("2024-02-29 23:59:00", "Date".into(), false);
        p.handle(&key(KeyCode::Tab));
        p.handle(&key(KeyCode::Up));
        assert_eq!(p.value(), "2024-02-29 00:59:00");
        p.handle(&key(KeyCode::Char('2')));
        p.handle(&key(KeyCode::Char('4')));
        assert!(matches!(
            p.handle(&key(KeyCode::Enter)),
            DateOutcome::Continue
        ));
        p.handle(&key(KeyCode::Up));
        assert!(matches!(
            p.handle(&key(KeyCode::Enter)),
            DateOutcome::Selected(_)
        ));
    }
    #[test]
    fn whole_day_uses_next_midnight_as_exclusive_bound() {
        let mut p = DatePicker::new("2024-02-29 12:00:00", "Date".into(), true);
        p.handle(&key(KeyCode::Char('e')));
        assert_eq!(p.value(), "2024-03-01 00:00:00");
        assert!(matches!(p.handle(&key(KeyCode::Esc)), DateOutcome::Cancel));
    }
    #[test]
    fn renders_with_native_calendar_at_terminal_sizes() {
        let theme = rat_theme4::create_salsa_theme("Imperial");
        let s = Styles(&theme);
        let mut p = DatePicker::new("2024-02-29 12:34:56", "Date".into(), true);
        for (w, h) in [(100, 24), (80, 24), (50, 18), (35, 12)] {
            let a = Rect::new(0, 0, w, h);
            let mut b = Buffer::empty(a);
            p.render(a, &mut b, &s);
            let text: String = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|pos| b[pos].symbol())
                .collect();
            assert!(text.contains("UTC"));
        }
    }
}
