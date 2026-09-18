//! Calendar presentation of Preview. No rule evaluation and no playback writes.
use std::time::{Duration, Instant};

use anyhow::Context;
use crossterm::event::{KeyCode, KeyEvent};
use jiff::{civil::Date, tz::TimeZone, SignedDuration, Span, Timestamp};
use stationd::proto::schedule::{self, decision::Origin};

use super::{app::Resource, client::ReadResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub from: Timestamp,
    pub to: Timestamp,
    pub zone: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotKey {
    pub minute: u16,
    // A second passage through this wall-clock bucket after a backward shift.
    pub fold: u8,
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub key: SlotKey,
    pub from: Timestamp,
    pub to: Timestamp,
}

pub struct Day {
    pub date: Date,
    pub from: Timestamp,
    pub to: Timestamp,
    pub slots: Vec<Slot>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub at: Timestamp,
    pub origin: Origin,
    pub rule_id: String,
    pub playlist: String,
}

impl Entry {
    pub fn is_mark(&self) -> bool {
        matches!(self.origin, Origin::AtClockHard | Origin::AtClockSoft)
    }
    pub fn symbol(&self) -> &'static str {
        match self.origin {
            Origin::AtClockHard => "!",
            Origin::AtClockSoft => "*",
            Origin::DayPart => "D",
            Origin::BaseRotation => "B",
            Origin::Fallback => "F",
            _ => "?",
        }
    }
}

/// Parse transport timestamps once. An invalid response is never a blank success.
pub fn entries(window: &Window, response: schedule::PreviewResponse) -> ReadResult<Vec<Entry>> {
    let mut result = Vec::new();
    for occurrence in response.occurrences {
        let ts = occurrence
            .at_utc
            .ok_or("Preview occurrence has no UTC timestamp")?;
        let at = Timestamp::new(ts.seconds, ts.nanos).map_err(|e| e.to_string())?;
        if at < window.from || at >= window.to {
            return Err("Preview occurrence is outside the requested window".into());
        }
        let origin = Origin::try_from(occurrence.origin).map_err(|e| e.to_string())?;
        result.push(Entry {
            at,
            origin,
            rule_id: occurrence.rule_id,
            playlist: occurrence.playlist_ref,
        });
    }
    result.sort_by_key(|entry| entry.at);
    Ok(result)
}

/// All transitions and point events intersecting a displayed bucket. A mark has
/// no inferred duration, even if the next response entry arrives a minute later.
pub fn intersecting(data: &[Entry], slot: &Slot) -> Vec<usize> {
    let first = data.partition_point(|e| e.at < slot.from).saturating_sub(1);
    let mut found = Vec::new();
    for i in first..data.len() {
        let entry = &data[i];
        if entry.at >= slot.to {
            break;
        }
        if entry.is_mark() {
            if entry.at >= slot.from {
                found.push(i);
            }
        } else if data.get(i + 1).map_or(true, |next| next.at > slot.from) {
            found.push(i);
        }
    }
    found
}

fn shift(date: Date, days: i64) -> anyhow::Result<Date> {
    Ok(date.checked_add(Span::new().days(days))?)
}

/// Build calendar cells from real instants. Missing wall hours have no cell;
/// repeated hours have separate cells with different UTC bounds.
fn calendar(
    date: Date,
    week: bool,
    zone: &str,
    step: u16,
) -> anyhow::Result<(Window, Vec<Day>, Vec<SlotKey>)> {
    let tz = TimeZone::get(zone)?;
    let first = if week {
        shift(date, -i64::from(date.weekday().to_monday_zero_offset()))?
    } else {
        date
    };
    let count = if week { 7 } else { 1 };
    let mut days = Vec::new();
    let mut axis: Vec<_> = (0..1440)
        .step_by(usize::from(step))
        .map(|minute| SlotKey { minute, fold: 0 })
        .collect();
    for d in 0..count {
        let date = shift(first, d)?;
        let from = date.to_zoned(tz.clone())?.timestamp();
        let to = shift(date, 1)?.to_zoned(tz.clone())?.timestamp();
        let mut slots: Vec<Slot> = Vec::new();
        let mut seen = [0u8; 1440];
        let mut at = from;
        while at < to {
            let local = at.to_zoned(tz.clone());
            let minute = local.hour() as u16 * 60 + local.minute() as u16;
            let key = SlotKey {
                minute: minute / step * step,
                fold: seen[minute as usize],
            };
            seen[minute as usize] += 1;
            let end = at.checked_add(SignedDuration::from_secs(60))?.min(to);
            if let Some(previous) = slots.last_mut().filter(|s| s.key == key) {
                previous.to = end;
            } else {
                slots.push(Slot {
                    key,
                    from: at,
                    to: end,
                });
            }
            at = end;
        }
        // Insert repeated buckets in chronological order, after their preceding
        // bucket. Normal days retain empty cells in these extra axis rows.
        let mut previous = None;
        for slot in &slots {
            if !axis.contains(&slot.key) {
                let index = previous
                    .and_then(|key| axis.iter().position(|k| *k == key))
                    .map_or(0, |i| i + 1);
                axis.insert(index, slot.key);
            }
            previous = Some(slot.key);
        }
        days.push(Day {
            date,
            from,
            to,
            slots,
        });
    }
    let window = Window {
        from: days.first().unwrap().from,
        to: days.last().unwrap().to,
        zone: zone.into(),
    };
    Ok((window, days, axis))
}

pub enum Action {
    None,
    Detail(String),
}

pub struct Agenda {
    pub date: Option<Date>,
    pub week: bool,
    pub step: u16,
    pub days: Vec<Day>,
    pub axis: Vec<SlotKey>,
    pub row: usize,
    pub scroll: usize,
    pub window: Option<Window>,
    pub data: Resource<Vec<Entry>>,
    pub in_flight: Option<Window>,
    pub date_input: Option<String>,
    pub message: String,
    zone: Option<String>,
    requested: bool,
    attempted: Option<Instant>,
}

impl Default for Agenda {
    fn default() -> Self {
        Self {
            date: None,
            week: false,
            step: 60,
            days: Vec::new(),
            axis: Vec::new(),
            row: 0,
            scroll: 0,
            window: None,
            data: Resource::default(),
            in_flight: None,
            date_input: None,
            message: String::new(),
            zone: None,
            requested: true,
            attempted: None,
        }
    }
}

impl Agenda {
    pub fn set_zone(&mut self, zone: &str) {
        if self.zone.as_deref() == Some(zone) {
            return;
        }
        let result = (|| {
            let now = Timestamp::now().to_zoned(TimeZone::get(zone)?);
            self.zone = Some(zone.into());
            self.date = Some(now.date());
            self.rebuild()?;
            self.row = self
                .axis
                .iter()
                .position(|k| k.minute == now.hour() as u16 * 60 / self.step * self.step)
                .unwrap_or(0);
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(e) = result {
            self.message = e.to_string();
        }
    }

    fn rebuild(&mut self) -> anyhow::Result<()> {
        let Some(date) = self.date else {
            return Ok(());
        };
        let Some(zone) = &self.zone else {
            return Ok(());
        };
        let selected = self.axis.get(self.row).copied();
        let (window, days, axis) = calendar(date, self.week, zone, self.step)?;
        if self.window.as_ref() != Some(&window) {
            // Never display the previous week's data underneath the new dates.
            self.data = Resource::default();
            self.requested = true;
        }
        self.window = Some(window);
        self.days = days;
        self.axis = axis;
        self.row = selected
            .and_then(|key| {
                self.axis.iter().position(|k| {
                    k.minute == key.minute / self.step * self.step && k.fold == key.fold
                })
            })
            .unwrap_or(self.row.min(self.axis.len().saturating_sub(1)));
        self.scroll = self.scroll.min(self.row);
        self.message.clear();
        Ok(())
    }

    pub fn request(&mut self) {
        self.requested = true;
    }

    pub fn take_request(&mut self, auto: bool, interval: Duration) -> Option<Window> {
        if self.in_flight.is_some() {
            return None;
        }
        let due = auto && self.attempted.map_or(true, |at| at.elapsed() >= interval);
        if !self.requested && !due {
            return None;
        }
        let window = self.window.clone()?;
        self.in_flight = Some(window.clone());
        self.requested = false;
        Some(window)
    }

    pub fn finish(&mut self, window: Window, result: ReadResult<Vec<Entry>>) {
        if self.in_flight.as_ref() != Some(&window) {
            return;
        }
        self.in_flight = None;
        if self.window.as_ref() == Some(&window) {
            self.data.apply(result);
            self.attempted = Some(Instant::now());
        }
    }

    pub fn selected_day(&self) -> usize {
        self.days
            .iter()
            .position(|day| Some(day.date) == self.date)
            .unwrap_or(0)
    }

    pub fn slot(&self, day: usize, row: usize) -> Option<&Slot> {
        let key = self.axis.get(row)?;
        self.days.get(day)?.slots.iter().find(|s| s.key == *key)
    }

    pub fn details(&self) -> String {
        let Some(window) = &self.window else {
            return "Waiting for the station timezone...".into();
        };
        let Some(slot) = self.slot(self.selected_day(), self.row) else {
            return "No civil time in this cell (clock change or skipped date).".into();
        };
        let Ok(tz) = TimeZone::get(&window.zone) else {
            return "Unknown station timezone".into();
        };
        let local = |at: Timestamp| at.to_zoned(tz.clone()).to_string();
        let mut lines = vec![
            format!("{} - {}", local(slot.from), local(slot.to)),
            self.data.label(),
        ];
        let Some(data) = &self.data.value else {
            lines.push(self.data.label());
            return lines.join("\n");
        };
        for i in intersecting(data, slot) {
            let entry = &data[i];
            lines.push(format!(
                "\n{} {} | {}",
                entry.symbol(),
                entry.rule_id,
                if entry.playlist.is_empty() {
                    "fallback"
                } else {
                    &entry.playlist
                }
            ));
            lines.push(format!("Local: {}\nUTC: {}", local(entry.at), entry.at));
            if entry.is_mark() {
                lines.push(
                    "Clock mark; duration unknown. Soft marks wait for a track boundary.".into(),
                );
            } else {
                let end = data.get(i + 1).map_or(window.to, |e| e.at);
                lines.push(format!(
                    "Next projected change: {}\nUTC: {}",
                    local(end),
                    end
                ));
                lines.push("Source interval in Preview; not a track duration or a cut.".into());
            }
        }
        if intersecting(data, slot).is_empty() {
            lines.push("No projected entry in this cell.".into());
        }
        lines.join("\n")
    }

    pub fn paste_date(&mut self, text: &str) {
        if let Some(input) = &mut self.date_input {
            input.extend(
                text.chars()
                    .filter(|c| c.is_ascii_digit() || *c == '-')
                    .take(10usize.saturating_sub(input.len())),
            );
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        if let Some(input) = &mut self.date_input {
            match key.code {
                KeyCode::Esc => {
                    self.date_input = None;
                    self.message.clear();
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if (c.is_ascii_digit() || c == '-') && input.len() < 10 => {
                    input.push(c)
                }
                KeyCode::Enter => {
                    let parsed = input.parse::<Date>().context("Expected YYYY-MM-DD");
                    match parsed.and_then(|date| self.goto(date)) {
                        Ok(()) => self.date_input = None,
                        Err(e) => self.message = e.to_string(),
                    }
                }
                _ => {}
            }
            return Action::None;
        }
        let original_view = (self.week, self.step);
        let result = (|| {
            let Some(date) = self.date else {
                return Ok(());
            };
            match key.code {
                KeyCode::Char('d') | KeyCode::Char('w') => {
                    self.week = key.code == KeyCode::Char('w');
                    self.rebuild()?;
                }
                KeyCode::Left => self.goto(shift(date, -1)?)?,
                KeyCode::Right => self.goto(shift(date, 1)?)?,
                KeyCode::PageUp | KeyCode::Char('[') => {
                    self.goto(shift(date, if self.week { -7 } else { -1 })?)?
                }
                KeyCode::PageDown | KeyCode::Char(']') => {
                    self.goto(shift(date, if self.week { 7 } else { 1 })?)?
                }
                KeyCode::Up | KeyCode::Char('k') => self.row = self.row.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.row = (self.row + 1).min(self.axis.len().saturating_sub(1))
                }
                KeyCode::Home => self.row = 0,
                KeyCode::End => self.row = self.axis.len().saturating_sub(1),
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.step = match self.step {
                        60 => 30,
                        _ => 15,
                    };
                    self.rebuild()?;
                }
                KeyCode::Char('-') => {
                    self.step = match self.step {
                        15 => 30,
                        _ => 60,
                    };
                    self.rebuild()?;
                }
                KeyCode::Char('g') => {
                    self.date_input = Some(String::new());
                    self.message.clear();
                }
                KeyCode::Char('t') => {
                    let now =
                        Timestamp::now().to_zoned(TimeZone::get(self.zone.as_deref().unwrap())?);
                    self.goto(now.date())?;
                    self.row = self
                        .axis
                        .iter()
                        .position(|k| {
                            k.minute
                                == (now.hour() as u16 * 60 + now.minute() as u16) / self.step
                                    * self.step
                        })
                        .unwrap_or(0);
                }
                _ => {}
            }
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(e) = result {
            self.week = original_view.0;
            self.step = original_view.1;
            self.message = e.to_string();
        }
        if key.code == KeyCode::Enter {
            Action::Detail(self.details())
        } else {
            Action::None
        }
    }

    fn goto(&mut self, date: Date) -> anyhow::Result<()> {
        // Prove the complete window is representable before committing selection.
        if let Some(zone) = &self.zone {
            calendar(date, self.week, zone, self.step)?;
        }
        self.date = Some(date);
        self.rebuild()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> Date {
        s.parse().unwrap()
    }
    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }
    fn entry(time: &str, origin: Origin) -> Entry {
        Entry {
            at: at(time),
            origin,
            rule_id: "test".into(),
            playlist: "test".into(),
        }
    }
    fn agenda() -> Agenda {
        let mut a = Agenda::default();
        a.set_zone("Europe/Paris");
        a.goto(date("2026-10-25")).unwrap();
        a
    }

    #[test]
    fn spring_gap_is_not_a_24_hour_day() {
        let (window, days, axis) = calendar(date("2026-03-29"), false, "Europe/Paris", 30).unwrap();
        assert_eq!(window.to.as_second() - window.from.as_second(), 23 * 3600);
        assert_eq!(axis.len(), 48);
        assert_eq!(days[0].slots.len(), 46);
        assert!(!days[0]
            .slots
            .iter()
            .any(|s| s.key.minute == 120 || s.key.minute == 150));
        assert_eq!(window.from, at("2026-03-28T23:00:00Z"));
        assert_eq!(window.to, at("2026-03-29T22:00:00Z"));
    }

    #[test]
    fn repeated_hour_rows_follow_utc_order_and_have_distinct_instants() {
        let (window, days, axis) = calendar(date("2026-10-25"), false, "Europe/Paris", 30).unwrap();
        assert_eq!(window.to.as_second() - window.from.as_second(), 25 * 3600);
        assert_eq!(axis.len(), 50);
        assert_eq!(
            &axis[4..8],
            &[
                SlotKey {
                    minute: 120,
                    fold: 0
                },
                SlotKey {
                    minute: 150,
                    fold: 0
                },
                SlotKey {
                    minute: 120,
                    fold: 1
                },
                SlotKey {
                    minute: 150,
                    fold: 1
                }
            ]
        );
        let twice: Vec<_> = days[0]
            .slots
            .iter()
            .filter(|s| s.key.minute == 150)
            .collect();
        assert_eq!(twice.len(), 2);
        assert_eq!(twice[1].from.as_second() - twice[0].from.as_second(), 3600);
        for slots in days[0].slots.windows(2) {
            assert_eq!(slots[0].to, slots[1].from);
        }
    }

    #[test]
    fn week_is_monday_to_monday_and_includes_dst_extra_hour() {
        let (window, days, axis) = calendar(date("2026-10-25"), true, "Europe/Paris", 60).unwrap();
        assert_eq!(days[0].date, date("2026-10-19"));
        assert_eq!(days[6].date, date("2026-10-25"));
        assert_eq!(window.to.as_second() - window.from.as_second(), 169 * 3600);
        assert_eq!(axis.len(), 25);
        assert!(!days[0].slots.iter().any(|s| s.key.fold == 1));
    }

    #[test]
    fn half_hour_transition_and_non_european_timezone() {
        let (window, days, _) =
            calendar(date("2026-04-05"), false, "Australia/Lord_Howe", 30).unwrap();
        assert_eq!(
            window.to.as_second() - window.from.as_second(),
            24 * 3600 + 1800
        );
        assert_eq!(days[0].slots.len(), 49);
        let (window, _, _) =
            calendar(date("2026-09-14"), false, "America/Los_Angeles", 60).unwrap();
        assert_eq!(window.from, at("2026-09-14T07:00:00Z"));
    }

    #[test]
    fn point_events_are_not_extended_until_the_next_transition() {
        let data = vec![
            entry("2026-09-14T09:00:00Z", Origin::AtClockSoft),
            entry("2026-09-14T09:01:00Z", Origin::BaseRotation),
            entry("2026-09-14T09:15:00Z", Origin::AtClockHard),
            entry("2026-09-14T09:16:00Z", Origin::DayPart),
        ];
        let slot = |from, to| Slot {
            key: SlotKey {
                minute: 540,
                fold: 0,
            },
            from: at(from),
            to: at(to),
        };
        assert!(
            intersecting(&data, &slot("2026-09-14T09:00:30Z", "2026-09-14T09:01:00Z")).is_empty()
        );
        assert_eq!(
            intersecting(&data, &slot("2026-09-14T09:00:00Z", "2026-09-14T09:15:00Z")),
            vec![0, 1]
        );
        assert_eq!(
            intersecting(&data, &slot("2026-09-14T09:15:00Z", "2026-09-14T09:30:00Z")),
            vec![2, 3]
        );
        assert_eq!(
            intersecting(&data, &slot("2026-09-14T09:30:00Z", "2026-09-14T10:00:00Z")),
            vec![3]
        );
    }

    #[test]
    fn navigation_drops_old_period_responses_and_preserves_same_period_on_error() {
        let mut a = agenda();
        let first = a.take_request(false, Duration::from_secs(3)).unwrap();
        assert!(a.take_request(true, Duration::ZERO).is_none());
        a.goto(date("2026-10-26")).unwrap();
        a.finish(first, Ok(vec![]));
        assert!(a.data.value.is_none(), "old day must not fill new day");
        let current = a.take_request(false, Duration::ZERO).unwrap();
        a.finish(
            current.clone(),
            Ok(vec![entry("2026-10-26T00:00:00Z", Origin::Fallback)]),
        );
        a.request();
        let same = a.take_request(false, Duration::ZERO).unwrap();
        a.finish(same, Err("Unimplemented: old daemon".into()));
        assert_eq!(a.data.value.as_ref().unwrap().len(), 1);
        assert!(a.data.label().contains("STALE"));
        a.request();
        let same = a.take_request(false, Duration::ZERO).unwrap();
        a.finish(same, Ok(vec![]));
        assert!(a.data.error.is_none());
    }

    #[test]
    fn transport_rejects_missing_timestamp_and_out_of_window_occurrences() {
        let window = agenda().window.unwrap();
        assert!(entries(
            &window,
            schedule::PreviewResponse {
                occurrences: vec![schedule::Occurrence::default()],
                ..Default::default()
            }
        )
        .is_err());
        let outside = schedule::Occurrence {
            at_utc: Some(prost_types::Timestamp {
                seconds: 0,
                nanos: 0,
            }),
            ..Default::default()
        };
        assert!(entries(
            &window,
            schedule::PreviewResponse {
                occurrences: vec![outside],
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn date_entry_and_week_navigation_preserve_selected_day() {
        let mut a = agenda();
        a.handle(KeyEvent::from(KeyCode::Char('w')));
        assert_eq!(a.selected_day(), 6);
        a.handle(KeyEvent::from(KeyCode::Right));
        assert_eq!(a.date, Some(date("2026-10-26")));
        assert_eq!(a.selected_day(), 0);
        a.handle(KeyEvent::from(KeyCode::Char('g')));
        a.paste_date("2026-02-30");
        a.handle(KeyEvent::from(KeyCode::Enter));
        assert!(a.date_input.is_some());
        assert_eq!(a.date, Some(date("2026-10-26")));
        a.handle(KeyEvent::from(KeyCode::Esc));
        a.handle(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(a.days.len(), 1);
        a.row = 10;
        a.handle(KeyEvent::from(KeyCode::Char('+')));
        assert_eq!(a.step, 30);
        assert_eq!(a.axis[a.row].minute, 600);
    }
}
