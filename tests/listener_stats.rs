//! Exercise the guest's actual SQL plan against the confined host database.
#[path = "../plugins/listener-stats-wasm/src/model.rs"]
mod model;

use serde_json::json;
use stationd::plugin_db::{DbLimits, Params, PluginDb, Statement};

#[test]
fn snapshots_are_atomic_idempotent_and_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let db = PluginDb::open(dir.path(), "listener-stats", DbLimits::default()).unwrap();
    let migrations = vec![include_str!("../plugins/listener-stats-wasm/migrations/001_snapshots.sql").to_string()];
    assert_eq!(db.migrate(&migrations).unwrap().applied, 1);
    assert_eq!(db.migrate(&migrations).unwrap().applied, 0);
    let sample: model::Snapshot = serde_json::from_value(json!({
        "mount": "/radio", "at": 1000, "listeners": [{"ip": "192.0.2.1"}]
    })).unwrap();
    let groups = model::aggregate(&sample, |_| Ok(model::Geo {
        status: "unavailable".into(), country: None, city: None,
    })).unwrap();
    let plan = model::statements(&sample, groups);
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
        let plan = model::statements(&sample, Default::default());
        let statements: Vec<Statement> = plan.into_iter().map(|v| serde_json::from_value(v).unwrap()).collect();
        db.batch(&statements).unwrap();
    }
    assert_eq!(query("SELECT listeners FROM listener_snapshot ORDER BY at")["rows"], json!([[null], [0]]));
    assert_eq!(query("SELECT count(*) FROM listener_geo")["rows"], json!([[0]]));
}
