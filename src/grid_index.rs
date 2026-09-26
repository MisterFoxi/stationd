//! Grid rule index (family A): load the grid rules from SQLite into the pure
//! `resolver::Grid`, and insert a rule into the index.
//!
//! Counterpart of `grid_store` (family B, runtime state): this module owns the
//! *reconstructible* side — the rules themselves, projected from migration
//! 0006. It maps the flat rows back into the resolver's `RuleKind` enum, one
//! detail table per variant.
//!
//! `playlist_ref` values are the human refs the rules carry; resolving a ref
//! to concrete media is a downstream concern, so nothing here touches the
//! `playlists` table.
//!
//! No TOML yet: rules are written by `insert_rule` (tests today, the grid
//! `apply` later). Once a grid TOML grammar exists, `apply` will DROP + rebuild
//! these tables from files — hence "family A".

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::resolver::{
    Cadence, ClockAnchor, Date, Grid, Mode, Rule, RuleKind, Validity, WallClock, Weekday,
};

#[derive(Debug, thiserror::Error)]
pub enum GridLoadError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("grid rule {rule_id:?} has kind {kind:?} but no matching detail row")]
    MissingDetail { rule_id: String, kind: String },
    #[error("grid rule {rule_id:?}: invalid date {value:?} (want YYYY-MM-DD)")]
    BadDate { rule_id: String, value: String },
    #[error("grid rule {rule_id:?}: invalid weekday {value:?}")]
    BadWeekday { rule_id: String, value: String },
}

/// Load every grid rule into a `Grid`. Reads each table once (not N+1) and
/// assembles in memory; grids are small.
pub async fn load_grid(pool: &SqlitePool) -> Result<Grid, GridLoadError> {
    // Base rows.
    let base: Vec<(String, i64, String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT id, enabled, kind, date_start, date_end FROM grid_rule")
            .fetch_all(pool)
            .await?;

    // Validity days, grouped by rule.
    let mut days: HashMap<String, Vec<Weekday>> = HashMap::new();
    let day_rows: Vec<(String, String)> =
        sqlx::query_as("SELECT rule_id, weekday FROM grid_rule_weekday")
            .fetch_all(pool)
            .await?;
    for (rule_id, wd) in day_rows {
        let day = parse_weekday(&wd).ok_or_else(|| GridLoadError::BadWeekday {
            rule_id: rule_id.clone(),
            value: wd.clone(),
        })?;
        days.entry(rule_id).or_default().push(day);
    }

    // Detail tables, each into a map keyed by rule_id.
    let base_rot: HashMap<String, String> =
        sqlx::query_as("SELECT rule_id, playlist_ref FROM grid_base_rotation")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();

    // (playlist_ref, start h, start m, end h, end m) — no end = an open day part.
    type DayPartRow = (String, i64, i64, Option<i64>, Option<i64>);
    let day_part: HashMap<String, DayPartRow> = sqlx::query_as(
        "SELECT rule_id, playlist_ref, start_hour, start_minute, end_hour, end_minute
         FROM grid_day_part",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(rid, r, sh, sm, eh, em): (String, String, i64, i64, Option<i64>, Option<i64>)| {
        (rid, (r, sh, sm, eh, em))
    })
    .collect();

    let at_clock: HashMap<String, (String, Option<i64>, Option<i64>, Option<i64>, String, Option<i64>)> =
        sqlx::query_as(
            "SELECT rule_id, playlist_ref, every_minutes, at_hour, at_minute, mode, expiry_secs
             FROM grid_at_clock",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(
            |(rid, r, em, ah, am, mode, exp): (
                String,
                String,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                String,
                Option<i64>,
            )| (rid, (r, em, ah, am, mode, exp)),
        )
        .collect();

    let every: HashMap<String, (String, Option<i64>, Option<i64>)> =
        sqlx::query_as("SELECT rule_id, playlist_ref, elapsed_secs, tracks FROM grid_every")
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|(rid, r, e, t): (String, String, Option<i64>, Option<i64>)| (rid, (r, e, t)))
            .collect();

    let live: HashMap<String, (String, i64, i64)> =
        sqlx::query_as("SELECT rule_id, dj, start_hour, start_minute FROM grid_live")
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|(rid, dj, h, m): (String, String, i64, i64)| (rid, (dj, h, m)))
            .collect();

    let mut rules = Vec::with_capacity(base.len());
    for (id, enabled, kind, date_start, date_end) in base {
        let validity = Validity {
            days: days.remove(&id).unwrap_or_default(),
            date_start: parse_opt_date(&id, date_start)?,
            date_end: parse_opt_date(&id, date_end)?,
        };

        let missing = || GridLoadError::MissingDetail {
            rule_id: id.clone(),
            kind: kind.clone(),
        };

        let rule_kind = match kind.as_str() {
            "base_rotation" => {
                let playlist_ref = base_rot.get(&id).ok_or_else(missing)?.clone();
                RuleKind::BaseRotation { playlist_ref }
            }
            "day_part" => {
                let (r, sh, sm, eh, em) = day_part.get(&id).ok_or_else(missing)?.clone();
                RuleKind::DayPart {
                    playlist_ref: r,
                    start: WallClock { hour: sh as u8, minute: sm as u8 },
                    end: eh.zip(em).map(|(eh, em)| WallClock { hour: eh as u8, minute: em as u8 }),
                }
            }
            "at_clock" => {
                let (r, every_min, ah, am, mode, exp) = at_clock.get(&id).ok_or_else(missing)?.clone();
                let anchor = match every_min {
                    Some(n) => ClockAnchor::EveryMinutes(n as u32),
                    None => ClockAnchor::At(WallClock {
                        hour: ah.unwrap_or(0) as u8,
                        minute: am.unwrap_or(0) as u8,
                    }),
                };
                RuleKind::AtClock {
                    playlist_ref: r,
                    anchor,
                    mode: if mode == "hard" { Mode::Hard } else { Mode::Soft },
                    expiry_secs: exp.map(|s| s as u64),
                }
            }
            "every" => {
                let (r, elapsed, tracks) = every.get(&id).ok_or_else(missing)?.clone();
                let cadence = match (elapsed, tracks) {
                    (Some(s), _) => Cadence::Elapsed(s as u64),
                    (_, Some(t)) => Cadence::Tracks(t as u32),
                    // The 0006 CHECK guarantees exactly one; treat the
                    // impossible case as a missing detail rather than guessing.
                    (None, None) => return Err(missing()),
                };
                RuleKind::Every { playlist_ref: r, cadence }
            }
            "live" => {
                let (dj, h, m) = live.get(&id).ok_or_else(missing)?.clone();
                RuleKind::Live { dj, start: WallClock { hour: h as u8, minute: m as u8 } }
            }
            _ => return Err(missing()),
        };

        rules.push(Rule {
            id,
            enabled: enabled != 0,
            validity,
            kind: rule_kind,
        });
    }

    Ok(Grid { rules })
}

/// Insert one rule into the index (base row + validity days + the one detail
/// row), transactionally. Used by tests today and by `replace_grid`.
pub async fn insert_rule(pool: &SqlitePool, rule: &Rule) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    insert_rule_in_tx(&mut tx, rule).await?;
    tx.commit().await
}

/// Replace the whole rule index (family A) with `rules`, transactionally —
/// what a grid `apply` runs: DROP-then-rebuild of the reconstructible side.
/// Family B (migration 0005) is deliberately NOT touched. FK cascade is not
/// relied upon (no `PRAGMA foreign_keys`), so every table is cleared explicitly.
pub async fn replace_grid(pool: &SqlitePool, rules: &[Rule]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for table in [
        "grid_rule_weekday",
        "grid_base_rotation",
        "grid_day_part",
        "grid_at_clock",
        "grid_every",
        "grid_live",
        "grid_rule",
    ] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&mut *tx)
            .await?;
    }
    for rule in rules {
        insert_rule_in_tx(&mut tx, rule).await?;
    }
    tx.commit().await
}

/// Shared body: write one rule's rows onto an already-open transaction.
async fn insert_rule_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    rule: &Rule,
) -> Result<(), sqlx::Error> {
    let kind = match &rule.kind {
        RuleKind::BaseRotation { .. } => "base_rotation",
        RuleKind::DayPart { .. } => "day_part",
        RuleKind::AtClock { .. } => "at_clock",
        RuleKind::Every { .. } => "every",
        RuleKind::Live { .. } => "live",
    };
    sqlx::query("INSERT INTO grid_rule (id, enabled, kind, date_start, date_end) VALUES (?1,?2,?3,?4,?5)")
        .bind(&rule.id)
        .bind(rule.enabled as i64)
        .bind(kind)
        .bind(rule.validity.date_start.map(fmt_date))
        .bind(rule.validity.date_end.map(fmt_date))
        .execute(&mut **tx)
        .await?;

    for day in &rule.validity.days {
        sqlx::query("INSERT INTO grid_rule_weekday (rule_id, weekday) VALUES (?1, ?2)")
            .bind(&rule.id)
            .bind(weekday_str(*day))
            .execute(&mut **tx)
            .await?;
    }

    match &rule.kind {
        RuleKind::BaseRotation { playlist_ref } => {
            sqlx::query("INSERT INTO grid_base_rotation (rule_id, playlist_ref) VALUES (?1,?2)")
                .bind(&rule.id)
                .bind(playlist_ref)
                .execute(&mut **tx)
                .await?;
        }
        RuleKind::DayPart { playlist_ref, start, end } => {
            sqlx::query(
                "INSERT INTO grid_day_part
                 (rule_id, playlist_ref, start_hour, start_minute, end_hour, end_minute)
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )
            .bind(&rule.id)
            .bind(playlist_ref)
            .bind(start.hour as i64)
            .bind(start.minute as i64)
            .bind(end.map(|e| e.hour as i64))
            .bind(end.map(|e| e.minute as i64))
            .execute(&mut **tx)
            .await?;
        }
        RuleKind::AtClock { playlist_ref, anchor, mode, expiry_secs } => {
            let (every_min, at_h, at_m) = match anchor {
                ClockAnchor::EveryMinutes(n) => (Some(*n as i64), None, None),
                ClockAnchor::At(wc) => (None, Some(wc.hour as i64), Some(wc.minute as i64)),
            };
            sqlx::query(
                "INSERT INTO grid_at_clock
                 (rule_id, playlist_ref, every_minutes, at_hour, at_minute, mode, expiry_secs)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
            )
            .bind(&rule.id)
            .bind(playlist_ref)
            .bind(every_min)
            .bind(at_h)
            .bind(at_m)
            .bind(if *mode == Mode::Hard { "hard" } else { "soft" })
            .bind(expiry_secs.map(|s| s as i64))
            .execute(&mut **tx)
            .await?;
        }
        RuleKind::Every { playlist_ref, cadence } => {
            let (elapsed, tracks) = match cadence {
                Cadence::Elapsed(s) => (Some(*s as i64), None),
                Cadence::Tracks(t) => (None, Some(*t as i64)),
            };
            sqlx::query(
                "INSERT INTO grid_every (rule_id, playlist_ref, elapsed_secs, tracks)
                 VALUES (?1,?2,?3,?4)",
            )
            .bind(&rule.id)
            .bind(playlist_ref)
            .bind(elapsed)
            .bind(tracks)
            .execute(&mut **tx)
            .await?;
        }
        RuleKind::Live { dj, start } => {
            sqlx::query(
                "INSERT INTO grid_live (rule_id, dj, start_hour, start_minute) VALUES (?1,?2,?3,?4)",
            )
            .bind(&rule.id)
            .bind(dj)
            .bind(start.hour as i64)
            .bind(start.minute as i64)
            .execute(&mut **tx)
            .await?;
        }
    }

    Ok(())
}

// --- small mappings -------------------------------------------------------

fn weekday_str(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "mon",
        Weekday::Tue => "tue",
        Weekday::Wed => "wed",
        Weekday::Thu => "thu",
        Weekday::Fri => "fri",
        Weekday::Sat => "sat",
        Weekday::Sun => "sun",
    }
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => return None,
    })
}

fn fmt_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

fn parse_opt_date(rule_id: &str, s: Option<String>) -> Result<Option<Date>, GridLoadError> {
    match s {
        None => Ok(None),
        Some(v) => parse_date(&v)
            .map(Some)
            .ok_or_else(|| GridLoadError::BadDate { rule_id: rule_id.to_string(), value: v }),
    }
}

fn parse_date(s: &str) -> Option<Date> {
    let mut it = s.split('-');
    let year = it.next()?.parse::<i32>().ok()?;
    let month = it.next()?.parse::<u8>().ok()?;
    let day = it.next()?.parse::<u8>().ok()?;
    if it.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(Date { year, month, day })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::resolver::{resolve_next, Epoch, LocalNow, Origin, PlaybackState};

    async fn fresh_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.db");
        let pool = db::init(&path).await.expect("init + migrations");
        (dir, pool)
    }

    fn rule(id: &str, kind: RuleKind) -> Rule {
        Rule { id: id.into(), enabled: true, validity: Validity::default(), kind }
    }

    #[tokio::test]
    async fn empty_grid_loads_empty() {
        let (_dir, pool) = fresh_db().await;
        let grid = load_grid(&pool).await.unwrap();
        assert!(grid.rules.is_empty());
    }

    #[tokio::test]
    async fn every_kind_roundtrips_through_sqlite() {
        let (_dir, pool) = fresh_db().await;

        insert_rule(&pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        let mut jazz = rule(
            "morning",
            RuleKind::DayPart {
                playlist_ref: "jazz".into(),
                start: WallClock { hour: 8, minute: 0 },
                end: Some(WallClock { hour: 10, minute: 0 }),
            },
        );
        jazz.validity.days = vec![Weekday::Mon, Weekday::Fri];
        insert_rule(&pool, &jazz).await.unwrap();
        insert_rule(
            &pool,
            &rule(
                "news",
                RuleKind::AtClock {
                    playlist_ref: "flash".into(),
                    anchor: ClockAnchor::At(WallClock { hour: 8, minute: 0 }),
                    mode: Mode::Hard,
                    expiry_secs: Some(120),
                },
            ),
        )
        .await
        .unwrap();
        insert_rule(
            &pool,
            &rule("jingle", RuleKind::Every { playlist_ref: "ids".into(), cadence: Cadence::Tracks(4) }),
        )
        .await
        .unwrap();

        let grid = load_grid(&pool).await.unwrap();
        assert_eq!(grid.rules.len(), 4);

        let morning = grid.rules.iter().find(|r| r.id == "morning").unwrap();
        assert_eq!(morning.validity.days.len(), 2);
        match &morning.kind {
            RuleKind::DayPart { playlist_ref, start, end } => {
                assert_eq!(playlist_ref, "jazz");
                assert_eq!((start.hour, end.unwrap().hour), (8, 10));
            }
            _ => panic!("expected DayPart"),
        }
        let news = grid.rules.iter().find(|r| r.id == "news").unwrap();
        match &news.kind {
            RuleKind::AtClock { mode, expiry_secs, anchor, .. } => {
                assert_eq!(*mode, Mode::Hard);
                assert_eq!(*expiry_secs, Some(120));
                assert!(matches!(anchor, ClockAnchor::At(WallClock { hour: 8, minute: 0 })));
            }
            _ => panic!("expected AtClock"),
        }
    }

    #[tokio::test]
    async fn resolver_runs_on_a_db_loaded_grid() {
        let (_dir, pool) = fresh_db().await;
        insert_rule(&pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        insert_rule(
            &pool,
            &rule(
                "morning",
                RuleKind::DayPart {
                    playlist_ref: "jazz".into(),
                    start: WallClock { hour: 8, minute: 0 },
                    end: Some(WallClock { hour: 10, minute: 0 }),
                },
            ),
        )
        .await
        .unwrap();

        let grid = load_grid(&pool).await.unwrap();
        let now = LocalNow {
            epoch: Epoch(9 * 3600),
            date: Date { year: 2026, month: 3, day: 15 },
            weekday: Weekday::Sun,
            wall: WallClock { hour: 9, minute: 0 },
        };
        // 09:00 is inside the DayPart window → jazz, loaded straight from SQLite.
        let d = resolve_next(now, &grid, &PlaybackState::default());
        assert_eq!(d.origin, Origin::DayPart);
        assert_eq!(d.playlist_ref.as_deref(), Some("jazz"));
    }

    #[tokio::test]
    async fn replace_grid_swaps_the_whole_index() {
        let (_dir, pool) = fresh_db().await;
        insert_rule(&pool, &rule("old", RuleKind::BaseRotation { playlist_ref: "a".into() }))
            .await.unwrap();
        insert_rule(&pool, &rule("stale", RuleKind::Every { playlist_ref: "x".into(), cadence: Cadence::Tracks(3) }))
            .await.unwrap();
        replace_grid(&pool, &[rule("new", RuleKind::BaseRotation { playlist_ref: "b".into() })])
            .await.unwrap();
        let grid = load_grid(&pool).await.unwrap();
        assert_eq!(grid.rules.len(), 1, "old rules must be gone");
        assert_eq!(grid.rules[0].id, "new");
    }

    #[tokio::test]
    async fn a_live_rule_is_stored_loaded_and_replaced() {
        let (_dir, pool) = fresh_db().await;
        let mut r = rule("marc-live", RuleKind::Live { dj: "marc".into(), start: WallClock { hour: 20, minute: 30 } });
        r.validity.days = vec![Weekday::Fri];
        insert_rule(&pool, &r).await.unwrap();
        let grid = load_grid(&pool).await.unwrap();
        match &grid.rules[0].kind {
            RuleKind::Live { dj, start } => assert_eq!((dj.as_str(), start.hour, start.minute), ("marc", 20, 30)),
            k => panic!("expected Live, got {k:?}"),
        }
        assert_eq!(grid.rules[0].validity.days, vec![Weekday::Fri]);
        replace_grid(&pool, &[rule("floor", RuleKind::BaseRotation { playlist_ref: "a".into() })]).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM grid_live").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0, "the live detail row is cleared by an apply");
    }
    #[tokio::test]
    async fn an_open_day_part_is_stored_and_loaded_without_end() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        insert_rule(
            &pool,
            &Rule {
                id: "morning".into(),
                enabled: true,
                validity: Validity::default(),
                kind: RuleKind::DayPart {
                    playlist_ref: "matin".into(),
                    start: WallClock { hour: 6, minute: 0 },
                    end: None,
                },
            },
        )
        .await
        .unwrap();
        let grid = load_grid(&pool).await.unwrap();
        assert!(matches!(
            grid.rules[0].kind,
            RuleKind::DayPart { start: WallClock { hour: 6, minute: 0 }, end: None, .. }
        ));
    }

}
