use p::schedule_service_server::ScheduleService;
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use stationd::{
    db,
    grid_engine::{GridEngine, Verdict},
    grid_index, group_state,
    media::ScannedMedia,
    media_index,
    playlist::Playlist,
    playlist_cursor, plugin,
    pool_inspection::{inspect_ref, PoolStats},
    proto::schedule as p,
    resolver as r,
    schedule_grpc::ScheduleGrpc,
    selection::SelectionError,
    store,
};
use tonic::Request;

async fn add(pool: &SqlitePool, reference: &str, selection: &str) {
    let toml = format!("name = {reference:?}\n[selection]\n{selection}");
    let pl = Playlist::parse(&toml).unwrap();
    pl.validate().unwrap();
    store::upsert(pool, reference, &pl, &toml, Some(reference))
        .await
        .unwrap();
}

/// Install a playlist WITHOUT the front-door `validate()` gate. Used to inject
/// a fixture that upstream validation would now reject, so the DEFENSIVE
/// downstream path (pool inspection / preview re-resolving filters) can still
/// be exercised — a bad filter that somehow reaches the view (hand-edited TOML,
/// a path that skipped validate) must be a loud error, never a silent zero.
async fn add_unvalidated(pool: &SqlitePool, reference: &str, selection: &str) {
    let toml = format!("name = {reference:?}\n[selection]\n{selection}");
    let pl = Playlist::parse(&toml).unwrap();
    store::upsert(pool, reference, &pl, &toml, Some(reference))
        .await
        .unwrap();
}

fn media(path: &str, duration_ms: u64, genre: &str) -> ScannedMedia {
    ScannedMedia {
        rel_path: path.into(),
        title: None,
        artist: None,
        album: None,
        year: None,
        genres: vec![genre.into()],
        duration_ms,
        size_bytes: 1,
        mtime_ns: 0,
    }
}

async fn fixture() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let pool = db::init(&dir.path().join("pool.db")).await.unwrap();
    media_index::replace_library(
        &pool,
        &[
            media("jazz/a.mp3", 180_250, "jazz"),
            media("jazz/b.mp3", 240_500, "jazz"),
            media("jazz/gone.mp3", 900_000, "jazz"),
            media("sting.mp3", 250, "id"),
        ],
        100,
    )
    .await
    .unwrap();
    media_index::mark_unavailable(&pool, "jazz/gone.mp3")
        .await
        .unwrap();
    add(
        &pool,
        "jazz",
        r#"
mode = "dynamic"
order = "newest"
[[selection.filter]]
field = "genre"
op = "has"
value = "jazz"
"#,
    )
    .await;
    add(
        &pool,
        "static",
        r#"
mode = "static"
order = "sequential"
files = ["/sting.mp3", "sting.mp3", "missing.mp3", "jazz/gone.mp3", "jazz/a.mp3"]
"#,
    )
    .await;
    add(
        &pool,
        "empty",
        r#"mode = "static"
files = ["missing.mp3"]"#,
    )
    .await;
    add(
        &pool,
        "remote",
        r#"mode = "remote"
url = "https://example.invalid/stream""#,
    )
    .await;
    add(
        &pool,
        "queue",
        r#"mode = "queue"
order = "fifo""#,
    )
    .await;
    (dir, pool)
}

#[tokio::test]
async fn pools_count_all_candidates_deduplicate_static_and_exclude_unavailable() {
    let (_dir, pool) = fixture().await;
    // Newest would choose one track, but inspection reports the WHOLE pool.
    assert_eq!(
        inspect_ref(&pool, "Jazz").await.unwrap().stats,
        PoolStats {
            selected_count: Some(2),
            total_duration_ms: Some(420_750),
            distinct_artists: Some(0), // fixtures carry no artist tags
        }
    );
    assert_eq!(
        inspect_ref(&pool, "STATIC").await.unwrap().stats,
        PoolStats {
            selected_count: Some(2),
            total_duration_ms: Some(180_500),
            distinct_artists: Some(0),
        }
    );
    assert_eq!(
        inspect_ref(&pool, "empty").await.unwrap().stats,
        PoolStats {
            selected_count: Some(0),
            total_duration_ms: Some(0),
            distinct_artists: Some(0),
        }
    );
    for reference in ["remote", "queue"] {
        assert_eq!(
            inspect_ref(&pool, reference).await.unwrap().stats,
            PoolStats::default()
        );
    }
}

#[tokio::test]
async fn group_pools_keep_full_local_pools_and_known_remote_queue_runtimes() {
    let (_dir, pool) = fixture().await;
    for strategy in ["sequence", "shuffle"] {
        add(
            &pool,
            "show",
            &format!(
                r#"
mode = "group"
strategy = "{strategy}"
members = [
  {{ ref = "jazz", runtime = "1m" }},
  {{ ref = "remote", runtime = "20m" }},
  {{ ref = "queue", runtime = "5m" }},
  {{ ref = "static", take = 1 }}
]
"#
            ),
        )
        .await;
        let result = inspect_ref(&pool, "show").await.unwrap();
        // Unknown count does not erase known duration. Shared jazz/a.mp3 is
        // counted once per member; take/runtime do not truncate LOCAL pools.
        assert_eq!(
            result.stats,
            PoolStats {
                selected_count: None,
                total_duration_ms: Some(2_101_250),
                distinct_artists: None, // group total: not de-duplicated across members
            }
        );
        let members = result.group.unwrap().members;
        assert_eq!(members[0].stats.total_duration_ms, Some(420_750));
        assert_eq!(
            members[1].stats,
            PoolStats {
                selected_count: None,
                total_duration_ms: Some(1_200_000),
                distinct_artists: None, // remote leaf: not measurable
            }
        );
        assert_eq!(members[2].stats.total_duration_ms, Some(300_000));
        assert_eq!(members[3].stats.selected_count, Some(2));
        assert_eq!(
            members[1].offset_secs,
            (strategy == "sequence").then_some(60)
        );
        assert_eq!(
            members[2].offset_secs,
            (strategy == "sequence").then_some(1260)
        );
        assert_eq!(members[3].offset_secs, None);
    }
    // Reusing a remote ref with a different runtime must not reuse the other
    // member's contextual duration.
    add(
        &pool,
        "twice",
        r#"mode = "group"
strategy = "sequence"
members = [{ ref = "remote", runtime = "2m" }, { ref = "remote", runtime = "3m" }]"#,
    )
    .await;
    assert_eq!(
        inspect_ref(&pool, "twice")
            .await
            .unwrap()
            .stats
            .total_duration_ms,
        Some(300_000)
    );
    add(
        &pool,
        "unknown",
        r#"mode = "group"
strategy = "sequence"
members = [{ ref = "jazz", take = 1 }, { ref = "remote", take = 1 }]"#,
    )
    .await;
    assert_eq!(
        inspect_ref(&pool, "unknown").await.unwrap().stats,
        PoolStats::default()
    );
}

#[tokio::test]
async fn weighted_and_rotate_groups_have_member_stats_without_invented_quotas() {
    let (_dir, pool) = fixture().await;
    for strategy in ["weighted", "rotate"] {
        add(
            &pool,
            "mix",
            &format!(
                r#"mode = "group"
strategy = "{strategy}"
members = [{{ ref = "jazz" }}, {{ ref = "static" }}]"#
            ),
        )
        .await;
        let result = inspect_ref(&pool, "mix").await.unwrap();
        assert_eq!(
            result.stats,
            PoolStats {
                selected_count: Some(4),
                total_duration_ms: Some(601_250),
                distinct_artists: None, // group total
            }
        );
        assert!(result
            .group
            .unwrap()
            .members
            .iter()
            .all(|m| m.quota.is_none() && m.offset_secs.is_none()));
    }
}

#[tokio::test]
async fn invalid_pools_are_errors_not_silent_zeroes() {
    let (_dir, pool) = fixture().await;
    assert!(matches!(
        inspect_ref(&pool, "missing").await,
        Err(SelectionError::PlaylistNotFound(_))
    ));
    add_unvalidated(
        &pool,
        "bad",
        r#"mode = "dynamic"
[[selection.filter]]
field = "unsupported"
op = "="
value = "x""#,
    )
    .await;
    assert!(matches!(
        inspect_ref(&pool, "bad").await,
        Err(SelectionError::UnsupportedFilter { .. })
    ));
    add(
        &pool,
        "cycle",
        r#"mode = "group"
strategy = "sequence"
members = [{ ref = "cycle" }]"#,
    )
    .await;
    assert!(matches!(
        inspect_ref(&pool, "cycle").await,
        Err(SelectionError::Unsupported(_))
    ));
}

#[tokio::test]
async fn grpc_preview_reads_pools_without_writes_or_plugin_filtering_and_refreshes_next_request() {
    use prost::Message;
    let (dir, pool) = fixture().await;
    add(
        &pool,
        "show",
        r#"mode = "group"
strategy = "sequence"
members = [{ ref = "jazz", runtime = "20m" }, { ref = "remote", runtime = "5m" }]"#,
    )
    .await;
    let rule = |id: &str, kind| r::Rule {
        id: id.into(),
        enabled: true,
        validity: r::Validity::default(),
        kind,
    };
    for item in [
        rule(
            "floor",
            r::RuleKind::BaseRotation {
                playlist_ref: "static".into(),
            },
        ),
        rule(
            "show",
            r::RuleKind::DayPart {
                playlist_ref: "show".into(),
                start: r::WallClock { hour: 9, minute: 0 },
                end: Some(r::WallClock {
                    hour: 10,
                    minute: 0,
                }),
            },
        ),
        rule(
            "mark",
            r::RuleKind::AtClock {
                playlist_ref: "empty".into(),
                anchor: r::ClockAnchor::EveryMinutes(30),
                mode: r::Mode::Soft,
                expiry_secs: None,
            },
        ),
    ] {
        grid_index::insert_rule(&pool, &item).await.unwrap();
    }
    playlist_cursor::set(&pool, "static", "sting.mp3")
        .await
        .unwrap();
    let state = group_state::GroupState {
        member_idx: 1,
        take_count: 2,
        member_started_at: Some(100),
        permutation: Some(vec![1, 0]),
    };
    group_state::set(&pool, "show", &state).await.unwrap();

    let handle = plugin::spawn(vec![plugin::PluginDecl {
        name: "blacklist".into(),
        enabled: true,
        order: 1,
        wasm: None,
        capabilities: vec![],
        config: "exclude_path_prefixes = [\"\"]".parse().unwrap(),
    }]);
    assert!(
        handle
            .filter_pool(vec![plugin::Candidate {
                rel_path: "jazz/a.mp3".into(),
                artist: None,
                title: None,
                album: None,
                year: None,
                duration_ms: 180_250,
                genres: vec![],
                mtime_ns: 0,
            }])
            .await
            .is_empty(),
        "blacklist must really filter all candidates"
    );

    // SQLite itself rejects writes on this connection. Preview still works
    // with seeded live state and a plugin that would remove the entire pool.
    let readonly = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(dir.path().join("pool.db"))
                .read_only(true),
        )
        .await
        .unwrap();
    let service = ScheduleGrpc::new(GridEngine::new(readonly, "UTC").with_plugins(handle));
    let request = || {
        Request::new(p::PreviewRequest {
            from: Some(prost_types::Timestamp {
                seconds: 8 * 3600,
                nanos: 0,
            }),
            window: Some(prost_types::Duration {
                seconds: 3 * 3600,
                nanos: 0,
            }),
        })
    };
    let first = service.preview(request()).await.unwrap().into_inner();
    assert_eq!(
        first,
        service.preview(request()).await.unwrap().into_inner()
    );
    assert_eq!(
        first,
        p::PreviewResponse::decode(first.encode_to_vec().as_slice()).unwrap()
    );
    assert!(first
        .occurrences
        .windows(2)
        .all(|w| w[0].at_utc.as_ref().unwrap().seconds < w[1].at_utc.as_ref().unwrap().seconds));
    for o in &first.occurrences {
        match o.playlist_ref.as_str() {
            "static" => {
                assert_eq!(o.selected_count, Some(2));
                assert_eq!(
                    o.total_duration,
                    Some(prost_types::Duration {
                        seconds: 180,
                        nanos: 500_000_000
                    })
                );
            }
            "show" => {
                assert_eq!(o.selected_count, None);
                assert_eq!(
                    o.total_duration,
                    Some(prost_types::Duration {
                        seconds: 720,
                        nanos: 750_000_000
                    })
                );
                assert_eq!(o.members[0].selected_count, Some(2));
                assert_eq!(o.members[1].selected_count, None);
                assert_eq!(o.members[1].total_duration.as_ref().unwrap().seconds, 300);
            }
            "empty" => {
                assert_eq!(o.selected_count, Some(0));
                assert_eq!(o.total_duration, Some(prost_types::Duration::default()));
            }
            other => panic!("unexpected playlist {other}"),
        }
    }
    assert!(first.occurrences.iter().any(|o| o.playlist_ref == "show"));
    assert_eq!(
        playlist_cursor::get(&pool, "static")
            .await
            .unwrap()
            .as_deref(),
        Some("sting.mp3")
    );
    assert_eq!(group_state::get(&pool, "show").await.unwrap(), state);

    media_index::mark_unavailable(&pool, "jazz/a.mp3")
        .await
        .unwrap();
    let refreshed = service.preview(request()).await.unwrap().into_inner();
    let base = refreshed
        .occurrences
        .iter()
        .find(|o| o.playlist_ref == "static")
        .unwrap();
    assert_eq!(base.selected_count, Some(1));
    assert_eq!(
        base.total_duration,
        Some(prost_types::Duration {
            seconds: 0,
            nanos: 250_000_000
        })
    );
}

#[tokio::test]
async fn preview_filter_error_identifies_the_leaf_playlist_and_filter() {
    let (_dir, pool) = fixture().await;
    add_unvalidated(
        &pool,
        "shows/broken",
        r#"mode = "dynamic"
[[selection.filter]]
field = "year"
op = ">="
value = 2020
[[selection.filter]]
field = "path"
op = "prefix"
value = ["stories/", "music/"]"#,
    )
    .await;
    add(
        &pool,
        "group",
        r#"mode = "group"
strategy = "sequence"
members = [{ ref = "shows/broken", take = 1 }]"#,
    )
    .await;
    let rule = r::Rule {
        id: "floor".into(),
        enabled: true,
        validity: r::Validity::default(),
        kind: r::RuleKind::BaseRotation {
            playlist_ref: "group".into(),
        },
    };
    grid_index::insert_rule(&pool, &rule).await.unwrap();
    let service = ScheduleGrpc::new(GridEngine::new(pool.clone(), "UTC"));
    let error = service
        .preview(Request::new(p::PreviewRequest {
            from: Some(prost_types::Timestamp {
                seconds: 0,
                nanos: 0,
            }),
            window: Some(prost_types::Duration {
                seconds: 60,
                nanos: 0,
            }),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    for detail in [
        "playlist `shows/broken`",
        "selection.filter[2]",
        "path",
        "prefix",
        "stories/",
        "music/",
        "expected a string",
    ] {
        assert!(
            error.message().contains(detail),
            "missing {detail}: {error}"
        );
    }
    // Direct inspection identifies the same TOML, not just group traversal.
    let direct = inspect_ref(&pool, "shows/broken").await.unwrap_err();
    assert!(direct.to_string().contains("playlist `shows/broken`"));
}

#[tokio::test]
async fn inspect_ref_sums_a_nested_group_recursively() {
    // outer = sequence[ inner , jazz ] ; inner = weighted[ jazz , static ].
    // The nested group folds its aggregate into the parent total (one level
    // deeper than the existing group tests) — exercises the recursive
    // `inspect_ref_at_depth` path, not just the flat one.
    let (_dir, pool) = fixture().await;
    add(
        &pool,
        "inner",
        r#"mode = "group"
strategy = "weighted"
members = [{ ref = "jazz", weight = 1 }, { ref = "static", weight = 1 }]"#,
    )
    .await;
    add(
        &pool,
        "outer",
        r#"mode = "group"
strategy = "sequence"
members = [{ ref = "inner", take = 1 }, { ref = "jazz", take = 1 }]"#,
    )
    .await;
    let result = inspect_ref(&pool, "outer").await.unwrap();
    // Total = inner (2 jazz + 2 static = 4 / 601_250) + jazz (2 / 420_750).
    assert_eq!(
        result.stats,
        PoolStats {
            selected_count: Some(6),
            total_duration_ms: Some(1_022_000),
            distinct_artists: None, // group total
        }
    );
    let members = result.group.unwrap().members;
    assert_eq!(members[0].r#ref, "inner");
    // The nested group appears as one member carrying its own aggregate.
    assert_eq!(members[0].stats.selected_count, Some(4));
    assert_eq!(members[0].stats.total_duration_ms, Some(601_250));
    assert_eq!(members[1].r#ref, "jazz");
    assert_eq!(members[1].stats.selected_count, Some(2));
}

#[tokio::test]
async fn inspect_ref_resolves_member_refs_relative_to_the_group_dir() {
    // `./x` / `../x` resolve from the group's directory, exactly like playout.
    let (_dir, pool) = fixture().await;
    add(
        &pool,
        "shows/jazz",
        r#"mode = "dynamic"
[[selection.filter]]
field = "genre"
op = "has"
value = "jazz""#,
    )
    .await;
    add(
        &pool,
        "shows/grp",
        r#"mode = "group"
strategy = "sequence"
members = [{ ref = "./jazz", take = 1 }, { ref = "../static", take = 1 }]"#,
    )
    .await;
    let result = inspect_ref(&pool, "Shows/Grp").await.unwrap();
    let members = result.group.unwrap().members;
    // The ref is displayed as written; the stats come from the resolved key.
    assert_eq!(members[0].r#ref, "./jazz");
    assert_eq!(members[0].stats.selected_count, Some(2));
    assert_eq!(members[1].r#ref, "../static");
    assert_eq!(members[1].stats.selected_count, Some(2));
    assert_eq!(result.stats.selected_count, Some(4));
}

// ----- CheckCoverage (sizing verdict) ------------------------------------

/// A BaseRotation rule `id` pointing at playlist `pl`, inserted into the grid.
async fn floor_rule(pool: &SqlitePool, id: &str, pl: &str) {
    grid_index::insert_rule(
        pool,
        &r::Rule {
            id: id.into(),
            enabled: true,
            validity: r::Validity::default(),
            kind: r::RuleKind::BaseRotation {
                playlist_ref: pl.into(),
            },
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn coverage_flags_empty_pool_and_missing_ref() {
    let (_dir, pool) = fixture().await;
    floor_rule(&pool, "floor", "empty").await; // static [missing.mp3] → pool vide
    floor_rule(&pool, "ghost", "does-not-exist").await; // ref cassée
    let report = GridEngine::new(pool.clone(), "UTC")
        .check_coverage(&[])
        .await
        .unwrap();
    assert_eq!(report.worst, Verdict::Insufficient);
    let by = |rid: &str| report.entries.iter().find(|e| e.rule_id == rid).unwrap();
    assert_eq!(by("floor").verdict, Verdict::Insufficient);
    assert!(by("floor").detail.contains("pool vide"), "{}", by("floor").detail);
    assert_eq!(by("ghost").verdict, Verdict::Insufficient);
    assert!(by("ghost").detail.contains("cassée"), "{}", by("ghost").detail);
}

#[tokio::test]
async fn coverage_flags_undersized_group_members() {
    let (_dir, pool) = fixture().await;
    // jazz pool = 2 tracks / ~7 min. A 1h runtime budget and a take = 5 both
    // exceed it → ⚠, each naming the culprit member.
    add(
        &pool,
        "night",
        r#"mode = "group"
strategy = "shuffle"
members = [{ ref = "jazz", runtime = "1h" }]"#,
    )
    .await;
    add(
        &pool,
        "seq",
        r#"mode = "group"
strategy = "sequence"
members = [{ ref = "jazz", take = 5 }]"#,
    )
    .await;
    floor_rule(&pool, "night", "night").await;
    floor_rule(&pool, "seq", "seq").await;
    let report = GridEngine::new(pool.clone(), "UTC")
        .check_coverage(&[])
        .await
        .unwrap();
    let by = |rid: &str| report.entries.iter().find(|e| e.rule_id == rid).unwrap();

    // shuffle group loops → axis B doesn't fire; the runtime shortfall does.
    assert_eq!(by("night").verdict, Verdict::Thin);
    assert!(by("night").detail.contains("jazz"), "{}", by("night").detail);
    let night_m = by("night")
        .members
        .iter()
        .find(|m| m.r#ref == "jazz")
        .unwrap();
    assert_eq!(night_m.verdict, Verdict::Thin);
    assert!(night_m.detail.contains("runtime"), "{}", night_m.detail);

    assert_eq!(by("seq").verdict, Verdict::Thin);
    let seq_m = by("seq").members.iter().find(|m| m.r#ref == "jazz").unwrap();
    assert_eq!(seq_m.verdict, Verdict::Thin);
    assert!(seq_m.detail.contains("take"), "{}", seq_m.detail);
}

#[tokio::test]
async fn coverage_ok_for_a_sufficient_rotation() {
    let (_dir, pool) = fixture().await;
    // jazz: dynamic (loops), no broadcast constraints/limit → non-empty = OK.
    floor_rule(&pool, "floor", "jazz").await;
    let report = GridEngine::new(pool.clone(), "UTC")
        .check_coverage(&[])
        .await
        .unwrap();
    assert_eq!(report.worst, Verdict::Ok);
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].verdict, Verdict::Ok);
}
