//! Exercise the guest's actual SQL plan against the confined host database.
#[path = "../plugins/listener-stats-wasm/src/model.rs"]
mod model;
// Tests exercise queries and rendering, not the CLI RPC dispatcher.
#[allow(dead_code)]
#[path = "../src/bin/stationctl/listeners.rs"]
mod listeners;

use serde_json::json;
use stationd::plugin_db::{DbLimits, Params, PluginDb, Statement};

#[test]
fn snapshots_are_atomic_idempotent_and_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let db = PluginDb::open(dir.path(), "listener-stats", DbLimits::default()).unwrap();
    let migrations = vec![
        include_str!("../plugins/listener-stats-wasm/migrations/001_snapshots.sql").to_string(),
        include_str!("../plugins/listener-stats-wasm/migrations/002_regions.sql").to_string(),
    ];
    assert_eq!(db.migrate(&migrations).unwrap().applied, 2);
    assert_eq!(db.migrate(&migrations).unwrap().applied, 0);
    let sample: model::Snapshot = serde_json::from_value(json!({
        "mount": "/radio", "at": 1000, "listeners": [{"ip": "192.0.2.1"}]
    })).unwrap();
    let groups = model::aggregate(&sample, |_| Ok(model::Geo {
        status: "unavailable".into(), country: None, city: None, region: None,
    })).unwrap();
    let plan = model::statements(&sample, groups, 7);
    let statements: Vec<Statement> = plan.into_iter().map(|v| serde_json::from_value(v).unwrap()).collect();
    db.batch(&statements).unwrap();
    db.batch(&statements).unwrap();
    let query = |sql| serde_json::to_value(db.query(sql, &Params::default()).unwrap()).unwrap();
    assert_eq!(query("SELECT count(*) FROM listener_snapshot")["rows"], json!([[1]]));
    assert_eq!(query("SELECT status, country, city, listeners FROM listener_geo")["rows"], json!([["unavailable", "", "", 1]]));
    let bad = vec![
        serde_json::from_value(json!({"sql": "DELETE FROM listener_geo", "params": []})).unwrap(),
        serde_json::from_value(json!({"sql": "INSERT INTO missing_table VALUES (1)", "params": []})).unwrap(),
    ];
    assert!(db.batch(&bad).is_err());
    assert_eq!(query("SELECT count(*) FROM listener_geo")["rows"], json!([[1]]));
    for (at, listeners) in [(700000, serde_json::Value::Null), (700001, json!([]))] {
        let sample = serde_json::from_value(json!({"mount": "/radio", "at": at, "listeners": listeners})).unwrap();
        let plan = model::statements(&sample, Default::default(), 7);
        let statements: Vec<Statement> = plan.into_iter().map(|v| serde_json::from_value(v).unwrap()).collect();
        db.batch(&statements).unwrap();
    }
    assert_eq!(query("SELECT listeners FROM listener_snapshot ORDER BY at")["rows"], json!([[null], [0]]));
    assert_eq!(query("SELECT count(*) FROM listener_geo")["rows"], json!([[0]]));
}

#[test]
fn regional_migration_preserves_history_and_statistics_keep_zero_denominators() {
    let dir = tempfile::tempdir().unwrap();
    let db = PluginDb::open(dir.path(), "listener-stats", DbLimits::default()).unwrap();
    let mut migrations = vec![include_str!("../plugins/listener-stats-wasm/migrations/001_snapshots.sql").to_string()];
    db.migrate(&migrations).unwrap();
    let exec = |sql: &str, params| db.exec(&serde_json::from_value(json!({"sql":sql,"params":params})).unwrap()).unwrap();
    exec("INSERT INTO listener_snapshot VALUES ('/old', 1000, 1)", json!([]));
    exec("INSERT INTO listener_geo VALUES ('/old', 1000, 'found', 'FR', 'Paris', 1)", json!([]));
    migrations.push(include_str!("../plugins/listener-stats-wasm/migrations/002_regions.sql").to_string());
    assert_eq!(db.migrate(&migrations).unwrap().applied, 1);
    assert_eq!(db.migrate(&migrations).unwrap().applied, 0);
    let query = |sql: &str| serde_json::to_value(db.query(sql, &Params::default()).unwrap()).unwrap()["rows"].clone();
    assert_eq!(query("SELECT region, city, listeners FROM listener_geo"), json!([["", "Paris", 1]]));

    // 345600 is Monday 1970-01-05 00:00 UTC. Three successful observations
    // (including zero) and one failure. Paris mean = (4 + 0 + 2) / 3 = 2.
    for (at, count) in [(345610, Some(7)), (345620, Some(0)), (345630, Some(2)), (345640, None)] {
        exec(model::SAVE_SAMPLE, json!(["/radio", at, count]));
    }
    for (at, city, count) in [(345610, "Paris", 4), (345610, "Versailles", 3), (345630, "Paris", 2)] {
        exec(model::SAVE_GEO, json!(["/radio", at, "found", "FR", "Ile-de-France", city, count]));
    }
    exec(model::SAVE_SAMPLE, json!(["/zero", 345610, 0]));
    exec(model::SAVE_SAMPLE, json!(["/other", 345610, 1]));
    exec(model::SAVE_GEO, json!(["/other", 345610, "found", "DE", "Berlin", "Berlin", 1]));
    let rows = query(&listeners::stats_query(listeners::Period::Hour, 345600, 349199, Some("/radio")));
    assert_eq!(rows, json!([
        ["1970-01-05 00:00", "/radio", "FR", "Ile-de-France", "Paris", 2.0, 4, 3, 1, "found"],
        ["1970-01-05 00:00", "/radio", "FR", "Ile-de-France", "Versailles", 1.0, 3, 3, 1, "found"]
    ]));
    assert_eq!(query(&listeners::stats_query(listeners::Period::Day, 345600, 349199, Some("/zero"))), json!([]));
    assert_eq!(query(&listeners::stats_query(listeners::Period::Hour, 345600, 349199, Some("/x' OR 1=1 --"))), json!([]));
    // A failed latest collection must not silently reuse an earlier positive one.
    assert_eq!(query(&listeners::regions_query(Some("/radio"))), json!([["/radio",345640,"","",null,"collecte_inconnue"]]));
    exec(model::SAVE_SAMPLE, json!(["/radio", 345650, 0]));
    assert_eq!(query(&listeners::regions_query(Some("/radio"))), json!([]));
    exec(model::SAVE_SAMPLE, json!(["/region", 345650, 7]));
    for (city, count) in [("Paris", 4), ("Versailles", 3)] {
        exec(model::SAVE_GEO, json!(["/region", 345650, "found", "FR", "Ile-de-France", city, count]));
    }
    assert_eq!(query(&listeners::regions_query(Some("/region"))), json!([["/region",345650,"FR","Ile-de-France",7,"found"]]));

    // Sunday and Monday must fall in different calendar weeks.
    exec(model::SAVE_SAMPLE, json!(["/week", 345599, 1]));
    exec(model::SAVE_SAMPLE, json!(["/week", 345600, 2]));
    for (at, count) in [(345599, 1), (345600, 2)] {
        exec(model::SAVE_GEO, json!(["/week", at, "found", "FR", "Bretagne", "Rennes", count]));
    }
    let weekly = query(&listeners::stats_query(listeners::Period::Week, 300000, 400000, Some("/week")));
    assert_eq!(weekly[0][0], "1970-01-05 00:00");
    assert_eq!(weekly[1][0], "1969-12-29 00:00");
    assert_eq!(weekly[0][5], 2.0);
    assert_eq!(weekly[1][5], 1.0);
}

#[test]
fn plugin_statistics_include_zero_exclude_failures_and_bound_windows() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../plugins/listener-stats-wasm/migrations/001_snapshots.sql")).unwrap();
    db.execute_batch(include_str!("../plugins/listener-stats-wasm/migrations/002_regions.sql")).unwrap();
    db.execute_batch("INSERT INTO listener_snapshot VALUES
        ('/radio',345610,9), ('/radio',345620,0),
        ('/radio',345630,3), ('/radio',345640,NULL),
        ('/zero',345610,0), ('/failed',345610,NULL),
        ('/old',259299,8), ('/future',345701,8);
        INSERT INTO listener_geo VALUES
        ('/radio',345610,'found','FR','IDF','Paris',4),
        ('/radio',345630,'found','FR','IDF','Paris',2),
        ('/radio',345610,'unavailable','','','',5),
        ('/radio',345630,'unavailable','','','',1);").unwrap();
    let query = |sql: &str| {
        let sql = sql.replace("unixepoch()", "345700");
        let mut stmt = db.prepare(&sql).unwrap();
        let count = stmt.column_count();
        stmt.query_map([], |row| {
            let values = (0..count).map(|i| {
                use rusqlite::types::ValueRef;
                match row.get_ref(i).unwrap() {
                    ValueRef::Null => serde_json::Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(v) => json!(std::str::from_utf8(v).unwrap()),
                    ValueRef::Blob(_) => panic!("unexpected blob"),
                }
            }).collect::<Vec<_>>();
            Ok(values)
        }).unwrap().collect::<Result<Vec<_>, _>>().unwrap()
    };
    let summary = query(include_str!("../plugins/listener-stats-wasm/src/ui/audience.sql"));
    assert_eq!(summary.len(), 3); // old and future samples are excluded
    assert_eq!(&summary[0][..5], &[json!("/radio"), json!(null), json!(4.0), json!(9), json!(75.0)]);
    assert_eq!(&summary[1][..5], &[json!("/zero"), json!(0), json!(0.0), json!(0), json!(100.0)]);
    assert_eq!(&summary[2][..5], &[json!("/failed"), json!(null), json!(null), json!(null), json!(0.0)]);
    let geo = query(include_str!("../plugins/listener-stats-wasm/src/ui/geography.sql"));
    assert_eq!(geo.len(), 2);
    let paris = geo.iter().find(|r| r[3] == json!("Paris")).unwrap();
    assert_eq!(&paris[4..7], &[json!(2.0), json!(4), json!(50.0)]);
    assert!(geo.iter().any(|r| r[1] == json!("Inconnu") && r[7] == json!("unavailable")));
    for sql in [
        include_str!("../plugins/listener-stats-wasm/src/ui/hourly.sql"),
        include_str!("../plugins/listener-stats-wasm/src/ui/daily.sql"),
    ] {
        let rows = query(sql);
        let radio = rows.iter().find(|r| r[1] == json!("/radio")).unwrap();
        assert_eq!(&radio[2..], &[json!(4.0), json!(9), json!(0), json!(3), json!(1)]);
        assert!(!rows.iter().any(|r| r[1] == json!("/future")));
        if sql.contains("Jour_UTC") {
            assert!(rows.iter().any(|r| r[1] == json!("/old")));
        } else {
            assert!(!rows.iter().any(|r| r[1] == json!("/old")));
        }
    }
    db.execute_batch("DELETE FROM listener_geo; DELETE FROM listener_snapshot;").unwrap();
    assert!(query(include_str!("../plugins/listener-stats-wasm/src/ui/audience.sql")).is_empty());
    assert!(query(include_str!("../plugins/listener-stats-wasm/src/ui/geography.sql")).is_empty());
}
