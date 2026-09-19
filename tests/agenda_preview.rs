//! The agenda uses Preview, never ResolveNext. Exercise the served contract
//! with real persisted counters/tokens and repeated day/week requests.
use stationd::proto::schedule::{self as p, schedule_service_server::ScheduleService};
use stationd::schedule_grpc::ScheduleGrpc;
use stationd::{db, grid_engine::GridEngine, grid_index, grid_store, resolver as r};
use tonic::Request;

#[tokio::test]
async fn agenda_preview_preserves_live_state_across_day_and_week_reads() {
    let dir = tempfile::tempdir().unwrap();
    let pool = db::init(&dir.path().join("preview.db")).await.unwrap();
    for reference in ["general", "news"] {
        let toml = r#"name = "Preview fixture"
[selection]
mode = "dynamic""#;
        let pl = stationd::playlist::Playlist::parse(toml).unwrap();
        stationd::store::upsert(&pool, reference, &pl, toml, Some(reference)).await.unwrap();
    }
    let make = |id: &str, kind| r::Rule {
        id: id.into(),
        enabled: true,
        validity: r::Validity::default(),
        kind,
    };
    for rule in [
        make(
            "base",
            r::RuleKind::BaseRotation {
                playlist_ref: "general".into(),
            },
        ),
        make(
            "news",
            r::RuleKind::AtClock {
                playlist_ref: "news".into(),
                anchor: r::ClockAnchor::EveryMinutes(30),
                mode: r::Mode::Soft,
                expiry_secs: None,
            },
        ),
        make(
            "cadence",
            r::RuleKind::Every {
                playlist_ref: "jingles".into(),
                cadence: r::Cadence::Tracks(3),
            },
        ),
    ] {
        grid_index::insert_rule(&pool, &rule).await.unwrap();
    }
    grid_store::ensure_every_rows(&pool, &["cadence".into()])
        .await
        .unwrap();
    grid_store::reset_every(&pool, "cadence", r::Epoch(123))
        .await
        .unwrap();
    grid_store::bump_tracks_since(&pool).await.unwrap();
    grid_store::record_at_clock_taken(&pool, "news@2026-10-25T02:30", r::Epoch(1792888200))
        .await
        .unwrap();
    let before = grid_store::load_playback_state(&pool).await.unwrap();
    let service = ScheduleGrpc::new(GridEngine::new(pool.clone(), "Europe/Paris"));
    let mut first = None;
    for seconds in [25 * 3600, 169 * 3600, 25 * 3600] {
        let from: jiff::Timestamp = "2026-10-24T22:00:00Z".parse().unwrap();
        let result = service
            .preview(Request::new(p::PreviewRequest {
                from: Some(prost_types::Timestamp {
                    seconds: from.as_second(),
                    nanos: 0,
                }),
                window: Some(prost_types::Duration { seconds, nanos: 0 }),
            }))
            .await
            .unwrap()
            .into_inner();
        // `occurrences` is real clock instants only — a track-counted `every`
        // is never placed on it...
        assert!(result
            .occurrences
            .iter()
            .all(|o| o.origin != p::decision::Origin::Every as i32));
        // ...it is surfaced apart, once, as an indicative rule in play.
        assert!(result
            .indicative
            .iter()
            .any(|r| r.playlist_ref == "jingles"));
        assert!(result
            .occurrences
            .iter()
            .any(|o| o.origin == p::decision::Origin::AtClockSoft as i32));
        // The timeline is strictly ordered by instant.
        assert!(result
            .occurrences
            .windows(2)
            .all(|w| w[0].at_utc.as_ref().unwrap().seconds
                < w[1].at_utc.as_ref().unwrap().seconds));
        if seconds == 25 * 3600 {
            if let Some(first) = &first {
                assert_eq!(first, &result);
            } else {
                first = Some(result);
            }
        }
    }
    let after = grid_store::load_playback_state(&pool).await.unwrap();
    assert_eq!(before.at_clock_taken, after.at_clock_taken);
    assert_eq!(before.every.len(), after.every.len());
    for (id, old) in before.every {
        assert_eq!(old.last_played, after.every[&id].last_played);
        assert_eq!(old.tracks_since, after.every[&id].tracks_since);
    }
}
