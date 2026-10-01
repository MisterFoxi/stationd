//! Pure snapshot aggregation and SQL planning, independently testable.
use std::collections::BTreeMap;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct Listener {
    pub ip: std::net::IpAddr,
}

#[derive(Deserialize)]
pub struct Snapshot {
    pub mount: String,
    pub at: i64,
    pub listeners: Option<Vec<Listener>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
pub struct Geo {
    pub status: String,
    pub country: Option<String>,
    pub city: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

impl Geo {
    pub fn validate(&self) -> Result<(), String> {
        match self.status.as_str() {
            "found" => {
                if !self.country.as_ref().is_some_and(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase())) {
                    return Err("invalid GeoIP country".into());
                }
                if self.region.as_ref().is_some_and(|r| r.len() > 256 || r.is_empty()) {
                    return Err("invalid GeoIP region".into());
                }
                if self.city.as_ref().is_some_and(|c| c.len() > 256 || c.is_empty()) {
                    return Err("invalid GeoIP city".into());
                }
            }
            "not_found" | "unavailable" if self.country.is_none() && self.city.is_none() && self.region.is_none() => {}
            _ => return Err("invalid GeoIP status".into()),
        }
        Ok(())
    }
}

pub fn aggregate<F>(sample: &Snapshot, mut lookup: F) -> Result<BTreeMap<Geo, u32>, String>
where F: FnMut(std::net::IpAddr) -> Result<Geo, String> {
    let mut cache = BTreeMap::new();
    let mut groups = BTreeMap::new();
    if let Some(listeners) = &sample.listeners {
        if listeners.len() > 10_000 {
            return Err("snapshot exceeds listener limit".into());
        }
        for listener in listeners {
            let geo = match cache.get(&listener.ip) {
                Some(geo) => geo,
                None => {
                    let geo = lookup(listener.ip)?;
                    geo.validate()?;
                    cache.entry(listener.ip).or_insert(geo)
                }
            };
            // Two clients behind the same IP still count as two listeners.
            *groups.entry(geo.clone()).or_insert(0) += 1;
        }
    }
    Ok(groups)
}

pub const SAVE_SAMPLE: &str = "INSERT INTO listener_snapshot (mount, at, listeners) VALUES (?1, ?2, ?3) ON CONFLICT(mount, at) DO UPDATE SET listeners = excluded.listeners";
pub const CLEAR_GEO: &str = "DELETE FROM listener_geo WHERE mount = ?1 AND at = ?2";
pub const SAVE_GEO: &str = "INSERT INTO listener_geo (mount, at, status, country, region, city, listeners) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
pub const PRUNE_SAMPLES: &str = "DELETE FROM listener_snapshot WHERE at < ?1";
pub const PRUNE_GEO: &str = "DELETE FROM listener_geo WHERE at < ?1";

/// All writes go through one atomic host batch. Replaying an observation
/// replaces it instead of double counting. Retention is the plugin's policy.
pub fn statements(sample: &Snapshot, groups: BTreeMap<Geo, u32>, retention_days: u32) -> Vec<Value> {
    let mut statements = vec![
        json!({"sql": SAVE_SAMPLE, "params": [&sample.mount, sample.at, sample.listeners.as_ref().map(Vec::len)]}),
        json!({"sql": CLEAR_GEO, "params": [&sample.mount, sample.at]}),
    ];
    for (geo, count) in groups {
        statements.push(json!({"sql": SAVE_GEO, "params": [
            &sample.mount, sample.at, geo.status,
            geo.country.unwrap_or_default(), geo.region.unwrap_or_default(), geo.city.unwrap_or_default(), count
        ]}));
    }
    let before = sample.at.saturating_sub(i64::from(retention_days) * 86400);
    statements.push(json!({"sql": PRUNE_GEO, "params": [before]}));
    statements.push(json!({"sql": PRUNE_SAMPLES, "params": [before]}));
    statements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_ip_is_cached_but_clients_are_not_deduplicated() {
        let sample: Snapshot = serde_json::from_value(json!({
            "mount": "/radio", "at": 100, "listeners": [{"ip": "192.0.2.1"}, {"ip": "192.0.2.1"}]
        })).unwrap();
        let mut calls = 0;
        let groups = aggregate(&sample, |_| {
            calls += 1;
            Ok(Geo {status: "found".into(), country: Some("FR".into()), city: Some("Paris".into()), region: Some("Ile-de-France".into())})
        }).unwrap();
        assert_eq!(calls, 1);
        assert_eq!(groups.values().copied().sum::<u32>(), 2);
        let sql = statements(&sample, groups, 30);
        assert_eq!(sql[2]["params"], json!(["/radio", 100, "found", "FR", "Ile-de-France", "Paris", 2]));
        assert!(!serde_json::to_string(&sql).unwrap().contains("192.0.2.1"));
    }

    #[test]
    fn failed_and_empty_snapshots_remain_distinct() {
        for (listeners, count) in [(Value::Null, Value::Null), (json!([]), json!(0))] {
            let sample: Snapshot = serde_json::from_value(json!({"mount": "/a", "at": 1, "listeners": listeners})).unwrap();
            let groups = aggregate(&sample, |_| panic!("no lookup expected")).unwrap();
            assert!(groups.is_empty());
            assert_eq!(statements(&sample, groups, 30)[0]["params"][2], count);
        }
    }

    #[test]
    fn missing_provider_is_not_a_fake_country() {
        let unknown = Geo {status: "unavailable".into(), country: None, city: None, region: None};
        assert!(unknown.validate().is_ok());
        assert!(Geo {country: Some("FR".into()), ..unknown}.validate().is_err());
    }
}
