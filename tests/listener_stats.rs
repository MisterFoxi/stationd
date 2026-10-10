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
        include_str!("../plugins/listener-stats-wasm/migrations/003_stats_indexes.sql").to_string(),
        include_str!("../plugins/listener-stats-wasm/migrations/004_stream_index.sql").to_string(),
    ];
    assert_eq!(db.migrate(&migrations).unwrap().applied, 4);
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
    db.execute_batch(include_str!("../plugins/listener-stats-wasm/migrations/003_stats_indexes.sql")).unwrap();
    db.execute_batch(include_str!("../plugins/listener-stats-wasm/migrations/004_stream_index.sql")).unwrap();
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
        let sql = format!("{}{}", include_str!("../plugins/listener-stats-wasm/src/ui/period.sql"), sql).replace("unixepoch()", "345700");
        let mut stmt = db.prepare(&sql).unwrap();
        let count = stmt.column_count();
        for (key, value) in [(":period", "24 h"), (":from", ""), (":to", ""), (":mount", ""), (":grouping", "Total"), (":geography", "Ville")] {
            if let Some(i) = stmt.parameter_index(key).unwrap() { stmt.raw_bind_parameter(i, value).unwrap(); }
        }
        stmt.raw_query().mapped(|row| {
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
        }).collect::<Result<Vec<_>, _>>().unwrap()
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
        assert_eq!(&radio[2..7], &[json!(4.0), json!(9), json!(0), json!(3), json!(1)]);
        assert!(!rows.iter().any(|r| r[1] == json!("/future")));
        assert!(!rows.iter().any(|r| r[1] == json!("/old")));
    }
    db.execute_batch("DELETE FROM listener_geo; DELETE FROM listener_snapshot;").unwrap();
    assert!(query(include_str!("../plugins/listener-stats-wasm/src/ui/audience.sql")).is_empty());
    assert!(query(include_str!("../plugins/listener-stats-wasm/src/ui/geography.sql")).is_empty());
}

#[test]
fn selectable_periods_and_geographic_rollups_use_per_snapshot_totals() {
    let dir = tempfile::tempdir().unwrap();
    let db = PluginDb::open(dir.path(), "rollups", DbLimits::default()).unwrap();
    db.migrate(&[
        include_str!("../plugins/listener-stats-wasm/migrations/001_snapshots.sql").into(),
        include_str!("../plugins/listener-stats-wasm/migrations/002_regions.sql").into(), include_str!("../plugins/listener-stats-wasm/migrations/003_stats_indexes.sql").into(), include_str!("../plugins/listener-stats-wasm/migrations/004_stream_index.sql").into(),
    ]).unwrap();
    db.exec(&Statement { sql: "INSERT INTO listener_snapshot VALUES
        ('/r',1704067200,10), ('/r',1704070800,6), ('/r',1704074400,0),
        ('/r',1704078000,NULL), ('/r',1704153600,99), ('/other',1704067200,123)".into(), params: Params::default() }).unwrap();
    db.exec(&Statement { sql: "INSERT INTO listener_geo VALUES
        ('/r',1704067200,'found','FR','IDF','Paris',4),
        ('/r',1704067200,'found','FR','ARA','Lyon',6),
        ('/r',1704070800,'found','FR','IDF','Paris',3),
        ('/r',1704070800,'found','FR','ARA','Lyon',3)".into(), params: Params::default() }).unwrap();
    let mut values = serde_json::Map::new();
    for (key, value) in [("period", "Personnalisée"), ("from", "2024-01-01 00:00:00"),
        ("to", "2024-01-02 00:00:00"), ("mount", "/r"), ("grouping", "Total")] {
        values.insert(key.into(), json!(value));
    }
    let sql = |tail: &str| format!("{}{}", include_str!("../plugins/listener-stats-wasm/src/ui/period.sql"), tail);
    let summary = db.query(&sql(include_str!("../plugins/listener-stats-wasm/src/ui/audience.sql")), &Params::Named(values.clone())).unwrap();
    assert_eq!(summary.rows.len(), 1);
    assert_eq!(summary.rows[0][2], json!(5.33));
    assert_eq!(summary.rows[0][7], json!(16));
    assert_eq!(summary.rows[0][9], json!(1));
    assert_eq!(summary.rows[0][1], json!(null));
    for grouping in ["Total", "Heure", "Jour", "Semaine", "Mois", "Année", "Heure du jour", "Jour de semaine"] {
        values.insert("grouping".into(), json!(grouping));
        let rows = db.query(&sql(include_str!("../plugins/listener-stats-wasm/src/ui/hourly.sql")), &Params::Named(values.clone())).unwrap();
        assert_eq!(rows.rows.len(), if grouping == "Heure" || grouping == "Heure du jour" {4} else {1});
        assert_eq!(rows.rows.iter().filter_map(|r| r[7].as_i64()).sum::<i64>(), 16);
        if grouping == "Semaine" { assert_eq!(rows.rows[0][0], json!("2024-01-01")); }
        if grouping == "Jour de semaine" { assert_eq!(rows.rows[0][0], json!("1")); }
    }
    values.insert("grouping".into(), json!("Total"));
    values.insert("geography".into(), json!("Pays"));
    let geo_sql = sql(include_str!("../plugins/listener-stats-wasm/src/ui/geography.sql"));
    let countries = db.query(&geo_sql, &Params::Named(values.clone())).unwrap();
    assert_eq!(countries.rows.len(), 1);
    assert_eq!(&countries.rows[0][4..7], &[json!(5.33), json!(10), json!(100.0)]);
    assert_eq!(countries.rows[0][10], json!(0)); // successful zero is included
    values.insert("geography".into(), json!("Région"));
    assert_eq!(db.query(&geo_sql, &Params::Named(values.clone())).unwrap().rows.len(), 2);
    values.insert("grouping".into(), json!("Heure"));
    assert_eq!(db.query(&geo_sql, &Params::Named(values)).unwrap().rows.len(), 4);
}

#[test]
fn audience_dashboard_preserves_zero_failures_and_per_snapshot_country_totals() {
    let dir=tempfile::tempdir().unwrap();let db=PluginDb::open(dir.path(),"dashboard",DbLimits::default()).unwrap();
    db.migrate(&[include_str!("../plugins/listener-stats-wasm/migrations/001_snapshots.sql").into(),include_str!("../plugins/listener-stats-wasm/migrations/002_regions.sql").into(), include_str!("../plugins/listener-stats-wasm/migrations/003_stats_indexes.sql").into(), include_str!("../plugins/listener-stats-wasm/migrations/004_stream_index.sql").into()]).unwrap();
    db.exec(&Statement {sql:"INSERT INTO listener_snapshot VALUES ('/r',1704067200,10),('/r',1704067210,0),('/r',1704070800,NULL),('/r',1704074400,2),('/other',1704067200,999)".into(),params:Params::default()}).unwrap();
    db.exec(&Statement {sql:"INSERT INTO listener_geo VALUES ('/r',1704067200,'found','FR','IDF','Paris',4),('/r',1704067200,'found','FR','ARA','Lyon',6),('/r',1704074400,'found','FR','IDF','Paris',2)".into(),params:Params::default()}).unwrap();
    let mut values=serde_json::Map::new();for (k,v) in [("period","Personnalisée"),("from","2024-01-01 00:00:00"),("to","2024-01-02 00:00:00"),("mount","/r"),("grouping","Total")] {values.insert(k.into(),json!(v));}
    let sql=format!("{}{}",include_str!("../plugins/listener-stats-wasm/src/ui/period.sql"),include_str!("../plugins/listener-stats-wasm/src/ui/dashboard.sql"));
    let rows=db.query(&sql,&Params::Named(values.clone())).unwrap().rows;
    let summary:Vec<_>=rows.iter().filter(|r|r[0]==json!("summary")).collect();
    assert_eq!(summary.len(),4);assert_eq!(summary[0][4],json!(4.0));assert_eq!(summary[1][4],json!(10));assert_eq!(summary[2][4],json!(3));assert_eq!(summary[3][4],json!(75.0));
    assert!(rows.iter().all(|r|r[1]==json!("/r")));
    let series:Vec<_>=rows.iter().filter(|r|r[0]==json!("series")).collect();assert_eq!(series.len(),3);assert!(series.iter().any(|r|r[4].is_null()&&r[6]==json!(0)));
    let country=rows.iter().find(|r|r[0]==json!("ranking")).unwrap();assert_eq!(country[2],json!("FR"));assert_eq!(country[4],json!(4.0)); // same denominator as summary, including the zero.
    let heat=rows.iter().find(|r|r[0]==json!("heatmap")&&r[3]==json!("00")).unwrap();assert_eq!(heat[4],json!(5.0));
    for grouping in ["Heure","Jour","Semaine","Mois","Année","Heure du jour","Jour de semaine"] {
        values.insert("grouping".into(),json!(grouping));
        let rows=db.query(&sql,&Params::Named(values.clone())).unwrap().rows;
        assert_eq!(rows.iter().filter(|r|r[0]==json!("series")).filter_map(|r|r[6].as_i64()).sum::<i64>(),3);
        let audience:f64=rows.iter().filter(|r|r[0]==json!("series")).map(|r|r[4].as_f64().unwrap_or(0.0)*r[6].as_f64().unwrap()).sum();
        assert!((audience-12.0).abs()<1e-9, "hourly rollups must preserve weighted means: {grouping}");
    }
    values.insert("grouping".into(),json!("Total"));
    values.insert("geography".into(),json!("Ville"));
    let geographic=db.query(&sql.replace("'Pays'",":geography"),&Params::Named(values.clone())).unwrap().rows;
    let cities:Vec<_>=geographic.iter().filter(|r|r[0]==json!("ranking")).collect();
    assert_eq!(cities.len(),2);assert!(cities.iter().any(|r|r[2]==json!("FR · IDF · Paris")&&r[4]==json!(2.0)));
    values.remove("geography");
    values.insert("mount".into(),json!("/missing"));assert!(db.query(&sql,&Params::Named(values.clone())).unwrap().rows.is_empty());
    values.insert("mount".into(),json!(""));let rows=db.query(&sql,&Params::Named(values)).unwrap().rows;assert!(rows.iter().all(|r|r[1]==json!("/other"))); // the chosen scope is explicit, never a sum of mount averages.
}
