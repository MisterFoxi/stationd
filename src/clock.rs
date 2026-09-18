//! Time boundary: epoch UTC -> civil local, using `jiff` with real DST rules.
//!
//! The resolver is pure and timezone-free: it takes a `LocalNow` already
//! decomposed in the station timezone. This module is the *only* place that
//! decomposition happens, so it is the one place DST ambiguity is real — and
//! the reason `jiff` is a dependency at all.
//!
//! Design (per the time doc): instants are epoch UTC on a monotone line; a
//! recurring rule is (civil time, IANA zone, recurrence). We never store a
//! frozen offset — the zone name is kept and the offset is applied per
//! occurrence, so "08:00 Europe/Paris" is 07:00 UTC in winter and 06:00 UTC
//! in summer, resolved on the fly here.

use jiff::civil::Weekday as JiffWeekday;
use jiff::tz::TimeZone;
use jiff::Timestamp;

use crate::resolver::{Date, Epoch, LocalNow, WallClock, Weekday};

#[derive(Debug, thiserror::Error)]
pub enum ClockError {
    #[error("unknown IANA timezone: {0:?}")]
    UnknownTimeZone(String),
    #[error("epoch out of representable range: {0}")]
    EpochOutOfRange(i64),
    #[error("invalid civil time: {0}")]
    BadCivilTime(String),
}

/// Decompose an epoch-UTC instant into the station's civil local time.
///
/// `tz_name` is an IANA name (e.g. `"Europe/Paris"`) — the station timezone,
/// configured once per node. The returned `LocalNow` carries both the original
/// epoch (for `Every`/ordering maths) and the civil fields (for
/// `DayPart`/`AtClock` window matching).
pub fn to_local_now(epoch: Epoch, tz_name: &str) -> Result<LocalNow, ClockError> {
    let tz = TimeZone::get(tz_name).map_err(|_| ClockError::UnknownTimeZone(tz_name.to_string()))?;
    let ts = Timestamp::from_second(epoch.0).map_err(|_| ClockError::EpochOutOfRange(epoch.0))?;
    let zoned = ts.to_zoned(tz);
    Ok(LocalNow {
        epoch,
        date: Date {
            year: zoned.year() as i32,
            month: zoned.month() as u8,
            day: zoned.day() as u8,
        },
        weekday: map_weekday(zoned.weekday()),
        wall: WallClock {
            hour: zoned.hour() as u8,
            minute: zoned.minute() as u8,
        },
    })
}

/// Convert a civil local time (in the station timezone) to an epoch-UTC
/// instant — the inverse of [`to_local_now`], used by the manual clock so a
/// human gives "20:00", not an epoch. DST is resolved by `jiff`'s compatible
/// disambiguation (a nonexistent/doubled wall time is mapped to a sensible
/// instant rather than rejected — acceptable for a test clock).
pub fn civil_to_epoch(
    tz_name: &str,
    year: i16,
    month: i8,
    day: i8,
    hour: i8,
    minute: i8,
) -> Result<Epoch, ClockError> {
    let tz = TimeZone::get(tz_name).map_err(|_| ClockError::UnknownTimeZone(tz_name.to_string()))?;
    let d = jiff::civil::Date::new(year, month, day)
        .map_err(|e| ClockError::BadCivilTime(e.to_string()))?;
    let t = jiff::civil::Time::new(hour, minute, 0, 0)
        .map_err(|e| ClockError::BadCivilTime(e.to_string()))?;
    let zoned = d
        .to_datetime(t)
        .to_zoned(tz)
        .map_err(|e| ClockError::BadCivilTime(e.to_string()))?;
    Ok(Epoch(zoned.timestamp().as_second()))
}

fn map_weekday(w: JiffWeekday) -> Weekday {
    match w {
        JiffWeekday::Monday => Weekday::Mon,
        JiffWeekday::Tuesday => Weekday::Tue,
        JiffWeekday::Wednesday => Weekday::Wed,
        JiffWeekday::Thursday => Weekday::Thu,
        JiffWeekday::Friday => Weekday::Fri,
        JiffWeekday::Saturday => Weekday::Sat,
        JiffWeekday::Sunday => Weekday::Sun,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    /// Helper: epoch seconds for a UTC civil instant (no DST in UTC, so this
    /// is an unambiguous way to name an instant in the tests).
    fn epoch_utc(y: i16, m: i8, d: i8, hh: i8, mm: i8) -> Epoch {
        let secs = date(y, m, d)
            .at(hh, mm, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_second();
        Epoch(secs)
    }

    #[test]
    fn unknown_timezone_is_an_explicit_error() {
        let err = to_local_now(Epoch(0), "Mars/Olympus_Mons").unwrap_err();
        assert!(matches!(err, ClockError::UnknownTimeZone(_)));
    }

    #[test]
    fn plain_winter_decode_paris() {
        // 2026-01-15 09:00 UTC = 10:00 CET (+1), a Thursday.
        let ln = to_local_now(epoch_utc(2026, 1, 15, 9, 0), "Europe/Paris").unwrap();
        assert_eq!((ln.wall.hour, ln.wall.minute), (10, 0));
        assert_eq!(ln.weekday, Weekday::Thu);
        assert_eq!((ln.date.year, ln.date.month, ln.date.day), (2026, 1, 15));
    }

    #[test]
    fn spring_forward_night_paris_skips_the_missing_hour() {
        // Europe/Paris springs forward 2026-03-29: 02:00 -> 03:00 local.
        // 00:30 UTC is still CET (+1) -> 01:30 local.
        let before = to_local_now(epoch_utc(2026, 3, 29, 0, 30), "Europe/Paris").unwrap();
        assert_eq!((before.wall.hour, before.wall.minute), (1, 30));
        // One real hour later, 01:30 UTC is already CEST (+2) -> 03:30 local.
        // The wall clock jumped 01:30 -> 03:30 (two hours) for one real hour:
        // that is the spring-forward, and 02:30 local never exists.
        let after = to_local_now(epoch_utc(2026, 3, 29, 1, 30), "Europe/Paris").unwrap();
        assert_eq!((after.wall.hour, after.wall.minute), (3, 30));
    }

    #[test]
    fn fall_back_night_paris_repeats_the_hour() {
        // Europe/Paris falls back 2026-10-25: 03:00 -> 02:00 local.
        // 00:30 UTC is still CEST (+2) -> 02:30 local (first pass).
        let first = to_local_now(epoch_utc(2026, 10, 25, 0, 30), "Europe/Paris").unwrap();
        assert_eq!((first.wall.hour, first.wall.minute), (2, 30));
        // One real hour later, 01:30 UTC is CET (+1) -> 02:30 local again
        // (second pass): the same wall time occurs twice, distinguished only
        // by the epoch. This is why a wall-clock rule must never be stored as
        // a frozen offset.
        let second = to_local_now(epoch_utc(2026, 10, 25, 1, 30), "Europe/Paris").unwrap();
        assert_eq!((second.wall.hour, second.wall.minute), (2, 30));
        assert_ne!(first.epoch, second.epoch, "same wall time, distinct instants");
    }
}
