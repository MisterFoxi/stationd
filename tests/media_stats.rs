//! Exercise the real media plugin's ingestion plan and queries against its confined DB.
#[path = "../plugins/play-stats-wasm/src/model.rs"]
mod model;
use serde_json::{Value, json};
use stationd::plugin_db::{DbLimits, Params, PluginDb, Statement};
fn playback(id: i64, at: i64, uuid: &str, path: &str, artist: &str) -> Value {
    json!({"play_id":id,"at":at,"media_uuid":uuid,"media_path":path,
        "title":"Titre","artist":artist,"album":"Album","playlist_ref":"music"})
}
fn ingest(db: &PluginDb, event: Value) {
    let plan = model::statements(&event, 365).unwrap();
    let statements: Vec<Statement> = plan
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect();
    if !statements.is_empty() {
        db.batch(&statements).unwrap();
    }
}
fn start(db: &PluginDb, p: &Value) {
    ingest(db, json!({"TrackStarted":{"playback":p}}));
}
fn finish(db: &PluginDb, p: &Value, at: i64, seconds: i64, verdict: Value) {
    ingest(
        db,
        json!({"TrackFinished":{"playback":p,"at":at,"aired_seconds":seconds,"played_to_end":verdict}}),
    );
}
fn sample(db: &PluginDb, at: i64, count: i64) {
    ingest(db, json!({"ListenersSampled":{"at":at,"count":count}}));
}
fn schema(db: &PluginDb) {
    db.migrate(&[
        model::LEGACY_MIGRATION.into(),
        include_str!("../plugins/play-stats-wasm/migrations/002_actual_plays.sql").into(),
    ])
    .unwrap();
}
fn values() -> serde_json::Map<String, Value> {
    let mut values = serde_json::Map::new();
    for (key, value) in [
        ("period", "Personnalisée"),
        ("from", "2024-01-01 00:00:00"),
        ("to", "2024-01-02 00:00:00"),
        ("media", ""),
        ("by", "Média"),
        ("grouping", "Total"),
        ("sorting", "Passages"),
    ] {
        values.insert(key.into(), json!(value));
    }
    values
}
fn query(db: &PluginDb, v: serde_json::Map<String, Value>) -> stationd::plugin_db::Rows {
    db.query(
        include_str!("../plugins/play-stats-wasm/src/ui/media.sql"),
        &Params::Named(v),
    )
    .unwrap()
}
#[test]
fn actual_media_history_preserves_legacy_counts_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = PluginDb::open(dir.path(), "media", DbLimits::default()).unwrap();
    db.migrate(&[model::LEGACY_MIGRATION.into()]).unwrap();
    db.exec(&Statement {
        sql: "INSERT INTO play_count VALUES ('old.mp3',7,1)".into(),
        params: Params::default(),
    })
    .unwrap();
    schema(&db);
    ingest(
        &db,
        json!({"TrackResolved":{"media_path":"selected-only.mp3"}}),
    );
    assert!(query(&db, values()).rows.is_empty());
    let p = playback(1, 1704067200, "uuid-a", "old.mp3", "A");
    start(&db, &p);
    start(&db, &p);
    sample(&db, 1704067199, 999);
    sample(&db, 1704067210, 10);
    ingest(&db, json!({"BroadcastStateChanged":{"to":"paused"}}));
    sample(&db, 1704067220, 999);
    ingest(&db, json!({"BroadcastStateChanged":{"to":"running"}}));
    sample(&db, 1704067230, 999);
    finish(&db, &p, 1704067800, 120, json!(true));
    finish(&db, &p, 1704068000, 999, json!(false));
    sample(&db, 1704068001, 999);
    let rows = query(&db, values()).rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(&rows[0][1..2], &[json!(1)]);
    assert_eq!(
        &rows[0][3..11],
        &[
            json!(120),
            json!(1),
            json!(0),
            json!(0),
            json!(0),
            json!(10.0),
            json!(10),
            json!(1)
        ]
    );
    assert_eq!(
        db.query("SELECT plays FROM play_count", &Params::default())
            .unwrap()
            .rows[0][0],
        json!(7)
    );
    let end_only = playback(2, 1704070800, "uuid-a", "renamed.mp3", "A");
    finish(&db, &end_only, 1704071000, 180, json!(false)); // missed start recovered from finish
    sample(&db, 1704070801, 999); // recovery never reopens an ended play
    let rows = query(&db, values()).rows;
    assert_eq!(rows[0][1], json!(2));
    assert_eq!(rows[0][3], json!(300));
    assert_eq!(rows[0][12], json!("renamed.mp3"));
    assert_eq!(rows[0][10], json!(1));
    assert!(
        model::statements(
            &json!({"TrackFinished":{"playback":p,"at":1,"aired_seconds":1}}),
            365
        )
        .is_err()
    );
}
#[test]
fn media_periods_rollups_and_audience_keep_identities_and_zero_samples() {
    let dir = tempfile::tempdir().unwrap();
    let db = PluginDb::open(dir.path(), "rollup", DbLimits::default()).unwrap();
    schema(&db);
    for (id, at, uuid, path, artist, seconds, verdict, counts) in [
        (1, 1704067200, "uuid-a", "old.mp3", "A", 120, true, vec![10]),
        (
            2,
            1704070800,
            "uuid-a",
            "renamed.mp3",
            "A",
            180,
            false,
            vec![20, 0],
        ),
        (
            3,
            1704074400,
            "uuid-b",
            "another.mp3",
            "A",
            240,
            true,
            vec![30],
        ),
        (4, 1704078000, "uuid-c", "third.mp3", "B", 60, true, vec![1]),
        (
            5,
            1704153600,
            "uuid-d",
            "boundary.mp3",
            "B",
            60,
            true,
            vec![100],
        ),
    ] {
        let p = playback(id, at, uuid, path, artist);
        start(&db, &p);
        for (i, count) in counts.into_iter().enumerate() {
            sample(&db, at + 1 + i as i64, count);
        }
        finish(&db, &p, at + 600, seconds, json!(verdict));
    }
    let rows = query(&db, values()).rows;
    assert_eq!(rows.len(), 3); // distinct UUIDs, same title
    let a = rows.iter().find(|r| r[16] == json!("uuid:uuid-a")).unwrap();
    assert_eq!(a[1], json!(2));
    assert_eq!(a[3], json!(300));
    assert_eq!(a[8], json!(10.0));
    assert_eq!(a[9], json!(20));
    let mut v = values();
    v.insert("by".into(), json!("Artiste"));
    let rows = query(&db, v.clone()).rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], json!("A"));
    assert_eq!(rows[0][1], json!(3));
    assert_eq!(rows[0][3], json!(540));
    assert_eq!(rows[0][8], json!(15.0));
    v.insert("by".into(), json!("Album"));
    assert_eq!(query(&db, v.clone()).rows.len(), 2); // same album name, different artists
    v.insert("by".into(), json!("Playlist"));
    assert_eq!(query(&db, v.clone()).rows[0][1], json!(4));
    for grouping in [
        "Total",
        "Heure",
        "Jour",
        "Semaine",
        "Mois",
        "Année",
        "Heure du jour",
        "Jour de semaine",
    ] {
        v.insert("grouping".into(), json!(grouping));
        let rows = query(&db, v.clone()).rows;
        assert_eq!(rows.iter().map(|r| r[1].as_i64().unwrap()).sum::<i64>(), 4);
        assert_eq!(
            rows.iter().map(|r| r[3].as_i64().unwrap()).sum::<i64>(),
            600
        );
    }
    v.insert("media".into(), json!("renamed"));
    assert_eq!(query(&db, v.clone()).rows[0][1], json!(1));
    v.insert("media".into(), json!("' OR 1=1 --"));
    assert!(query(&db, v).rows.is_empty());
}

#[test]
fn media_dashboard_uses_full_period_totals_and_separates_unknown_durations() {
    let dir = tempfile::tempdir().unwrap();
    let db = PluginDb::open(dir.path(), "dashboard", DbLimits::default()).unwrap();
    schema(&db);
    let a = playback(1, 1704067200, "a", "a.mp3", "Artist");
    start(&db, &a);
    sample(&db, 1704067210, 0);
    sample(&db, 1704067220, 10);
    finish(&db, &a, 1704067320, 120, json!(true));
    let b = playback(2, 1704070800, "b", "b.mp3", "Artist");
    start(&db, &b);
    let sql = include_str!("../plugins/play-stats-wasm/src/ui/dashboard.sql");
    let mut v = values();
    let rows = db.query(sql, &Params::Named(v.clone())).unwrap().rows;
    let summary: Vec<_> = rows.iter().filter(|r| r[0] == json!("summary")).collect();
    assert_eq!(summary.len(), 4);
    assert_eq!(summary[0][4], json!(2));
    assert_eq!(summary[1][4], json!(120));
    assert_eq!(summary[2][4], json!(5.0));
    assert_eq!(summary[3][4], json!(10));
    let rank: Vec<_> = rows.iter().filter(|r| r[0] == json!("ranking")).collect();
    assert_eq!(rank.len(), 2);
    assert_ne!(rank[0][2], rank[1][2]);
    v.insert("sorting".into(), json!("Durée"));
    let rows = db.query(sql, &Params::Named(v.clone())).unwrap().rows;
    assert!(
        rows.iter()
            .any(|r| r[0] == json!("series") && r[4].is_null())
    );
    v.insert("sorting".into(), json!("Audience"));
    let rows = db.query(sql, &Params::Named(v.clone())).unwrap().rows;
    assert!(
        rows.iter()
            .any(|r| r[0] == json!("series") && r[4] == json!(5.0))
    );
    assert!(
        rows.iter()
            .any(|r| r[0] == json!("series") && r[4].is_null())
    );
    v.insert("by".into(), json!("Artiste"));
    let rows = db.query(sql, &Params::Named(v.clone())).unwrap().rows;
    assert_eq!(rows.iter().filter(|r| r[0] == json!("ranking")).count(), 1);
    v.insert("media".into(), json!("' OR 1=1 --"));
    assert!(db.query(sql, &Params::Named(v)).unwrap().rows.is_empty());
}
