//! Exercise the read RPC against real migrations and prove it leaves the
//! durable playback tables untouched (including a clock mark and counters).
use stationd::{db, grid_engine::GridEngine, grid_index, grid_store, resolver as r};
use stationd::proto::schedule::{self as p, schedule_service_server::ScheduleService};
use stationd::schedule_grpc::ScheduleGrpc;
use tonic::Request;

fn rule(id: &str, kind: r::RuleKind) -> r::Rule {
    r::Rule { id: id.into(), enabled: true, validity: r::Validity::default(), kind }
}

async fn fixture() -> (tempfile::TempDir, sqlx::SqlitePool, ScheduleGrpc) {
    let dir = tempfile::tempdir().unwrap();
    let pool = db::init(&dir.path().join("rules.db")).await.unwrap();
    let service = ScheduleGrpc::new(GridEngine::new(pool.clone(), "UTC"));
    (dir, pool, service)
}

#[tokio::test]
async fn lists_every_variant_and_preserves_playback_state() {
    let (_dir, pool, service) = fixture().await;
    let mut base = rule("a-base", r::RuleKind::BaseRotation { playlist_ref: "general".into() });
    base.enabled = false;
    base.validity = r::Validity {
        days: vec![r::Weekday::Sun, r::Weekday::Mon],
        date_start: Some(r::Date { year: 2026, month: 9, day: 13 }),
        date_end: Some(r::Date { year: 2026, month: 10, day: 1 }),
    };
    let rules = vec![
        base,
        rule("b-day", r::RuleKind::DayPart { playlist_ref: "jazz".into(),
            start: r::WallClock { hour: 8, minute: 15 }, end: Some(r::WallClock { hour: 10, minute: 30 }) }),
        rule("c-clock", r::RuleKind::AtClock { playlist_ref: "news".into(),
            anchor: r::ClockAnchor::At(r::WallClock { hour: 9, minute: 0 }), mode: r::Mode::Hard, expiry_secs: Some(30) }),
        rule("d-marks", r::RuleKind::AtClock { playlist_ref: "ids".into(),
            anchor: r::ClockAnchor::EveryMinutes(15), mode: r::Mode::Soft, expiry_secs: None }),
        rule("e-tracks", r::RuleKind::Every { playlist_ref: "jingle".into(), cadence: r::Cadence::Tracks(3) }),
        rule("f-elapsed", r::RuleKind::Every { playlist_ref: "promo".into(), cadence: r::Cadence::Elapsed(600) }),
    ];
    for rule in rules.iter().rev() { grid_index::insert_rule(&pool, rule).await.unwrap(); }
    grid_store::ensure_every_rows(&pool, &["e-tracks".into(), "f-elapsed".into()]).await.unwrap();
    grid_store::reset_every(&pool, "e-tracks", r::Epoch(123)).await.unwrap();
    grid_store::bump_tracks_since(&pool).await.unwrap();
    grid_store::record_at_clock_taken(&pool, "existing-mark", r::Epoch(120)).await.unwrap();
    let before = grid_store::load_playback_state(&pool).await.unwrap();

    let first = service.list_rules(Request::new(p::ListRulesRequest {})).await.unwrap().into_inner();
    let second = service.list_rules(Request::new(p::ListRulesRequest {})).await.unwrap().into_inner();
    assert_eq!(first, second, "repeated polling has stable transport output");
    assert_eq!(first.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["a-base", "b-day", "c-clock", "d-marks", "e-tracks", "f-elapsed"]);
    assert!(!first.rules[0].enabled, "disabled rules must remain inspectable");
    let validity = first.rules[0].validity.as_ref().unwrap();
    assert_eq!(validity.days, [p::Weekday::Mon as i32, p::Weekday::Sun as i32]);
    assert_eq!(validity.date_start, "2026-09-13");
    assert_eq!(validity.date_end, "2026-10-01");
    use p::rule::Kind;
    assert!(matches!(&first.rules[0].kind, Some(Kind::BaseRotation(r)) if r.playlist_ref == "general"));
    match first.rules[1].kind.as_ref().unwrap() {
        Kind::DayPart(r) => {
            assert_eq!(r.start, Some(p::WallClock { hour: 8, minute: 15 }));
            assert_eq!(r.end, Some(p::WallClock { hour: 10, minute: 30 }));
        },
        _ => panic!("expected DayPart"),
    }
    match first.rules[2].kind.as_ref().unwrap() {
        Kind::AtClock(r) => {
            assert_eq!(r.at, Some(p::WallClock { hour: 9, minute: 0 }));
            assert_eq!(r.every_minutes, 0);
            assert_eq!(r.mode, p::at_clock::Mode::Hard as i32);
            assert_eq!(r.expiry, Some(prost_types::Duration { seconds: 30, nanos: 0 }));
        },
        _ => panic!("expected AtClock"),
    }
    match first.rules[3].kind.as_ref().unwrap() {
        Kind::AtClock(r) => {
            assert_eq!(r.every_minutes, 15);
            assert!(r.at.is_none());
            assert!(r.expiry.is_none());
            assert_eq!(r.mode, p::at_clock::Mode::Soft as i32);
        },
        _ => panic!("expected AtClock"),
    }
    assert!(matches!(&first.rules[4].kind, Some(Kind::Every(r))
        if r.cadence == Some(p::every::Cadence::Tracks(3))));
    assert!(matches!(&first.rules[5].kind, Some(Kind::Every(r))
        if r.cadence == Some(p::every::Cadence::Elapsed(prost_types::Duration { seconds: 600, nanos: 0 }))));
    let after = grid_store::load_playback_state(&pool).await.unwrap();
    assert_eq!(before.every.len(), after.every.len());
    for (id, old) in &before.every {
        let new = &after.every[id];
        assert_eq!(old.last_played, new.last_played, "{id}: last_played changed");
        assert_eq!(old.tracks_since, new.tracks_since, "{id}: counter changed");
    }
    assert_eq!(before.at_clock_taken, after.at_clock_taken, "listing must not consume clock marks");
}

#[tokio::test]
async fn empty_grid_is_success_and_storage_errors_are_explicit() {
    let (_dir, pool, service) = fixture().await;
    let reply = service.list_rules(Request::new(p::ListRulesRequest {})).await.unwrap().into_inner();
    assert!(reply.rules.is_empty());
    pool.close().await;
    let error = service.list_rules(Request::new(p::ListRulesRequest {})).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
}
