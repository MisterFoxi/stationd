//! Administrative WASM host: same event, guest migrations and confined DB as StationD.
use crate::{
    listener_history::{stats_query, Period},
    plugin_db::{DbLimits, Params, PluginDb, Statement},
    rpc::HistoryRequest,
    Admin,
};
use anyhow::{bail, Context, Result};
use extism::{host_fn, Function, Manifest, Plugin, PluginBuilder, UserData, Wasm, PTR};
use serde::Deserialize;
use serde_json::{json, Value};
use station_icecast_config::geoip::{Geoip, Location, DATA_ATTRIBUTION};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tonic::Status;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub sample_interval_seconds: u64,
    #[serde(default)]
    pub plugin: Vec<Declaration>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub name: String,
    pub wasm: PathBuf,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub config: toml::Table,
    #[serde(default)]
    pub db: DbLimits,
}
#[derive(Clone)]
struct Host {
    db: Arc<PluginDb>,
    geoip: Option<Arc<Geoip>>,
    capabilities: HashSet<String>,
}
impl Host {
    fn check(&self, name: &str) -> Result<()> {
        if !self.capabilities.contains(name) {
            bail!("capability denied");
        }
        Ok(())
    }
}
fn reply(result: Result<Value>) -> String {
    match result {
        Ok(Value::Object(mut object)) => {
            object.insert("ok".into(), json!(true));
            Value::Object(object).to_string()
        }
        Ok(value) => json!({"ok":true,"result":value}).to_string(),
        Err(_) => json!({"ok":false,"error":"host request refused or failed"}).to_string(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    statements: Vec<Statement>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    ip: std::net::IpAddr,
}
host_fn!(db_batch(data: Host; input: String) -> String {
    let data = data.get()?;
    let host = data.lock().map_err(|_| anyhow::anyhow!("host poisoned"))?;
    Ok(reply((|| {
        host.check("db")?;
        let batch: Batch = serde_json::from_str(&input)?;
        if batch.statements.len() > 10004 { bail!("batch too large"); }
        Ok(json!({"results":host.db.batch(&batch.statements)?}))
    })()))
});
host_fn!(geoip_lookup(data: Host; input: String) -> String {
    let data = data.get()?;
    let host = data.lock().map_err(|_| anyhow::anyhow!("host poisoned"))?;
    Ok(reply((|| {
        host.check("geoip")?;
        let request: Lookup = serde_json::from_str(&input)?;
        let location = match &host.geoip {
            Some(db) => db.lookup(request.ip).map_err(anyhow::Error::msg)?,
            None => Location::unavailable(),
        };
        Ok(serde_json::to_value(location)?)
    })()))
});
struct Loaded {
    name: String,
    guest: Plugin,
    db: Arc<PluginDb>,
    last_observation: Option<u64>,
    last_success: Option<u64>,
    last_complete_collection: Option<u64>,
    failures: u64,
    problem: String,
}
pub struct History {
    settings: Settings,
    plugins: Mutex<Vec<Loaded>>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl History {
    pub fn load(path: &Path, directory: &Path, geoip: Option<Arc<Geoip>>) -> Result<Arc<Self>> {
        let text = std::fs::read_to_string(path)?;
        if text.len() > 256 * 1024 {
            bail!("admin settings too large");
        }
        let settings: Settings = toml::from_str(&text).context("invalid admin settings")?;
        if !(5..=3600).contains(&settings.sample_interval_seconds) || settings.plugin.len() > 8 {
            bail!("invalid sampling interval or plugin count");
        }
        let mut names = HashSet::new();
        let mut plugins = Vec::new();
        for declaration in &settings.plugin {
            crate::plugin_db::validate_name(&declaration.name).map_err(anyhow::Error::msg)?;
            if !names.insert(&declaration.name) {
                bail!("duplicate plugin name");
            }
            declaration.db.validate().map_err(anyhow::Error::msg)?;
            if declaration.db.max_size_mb > 256
                || declaration.db.max_rows > 1000
                || declaration.db.query_timeout_ms > 1000
            {
                bail!("admin plugin database limits exceed 256 MiB, 1000 rows or 1000 ms");
            }
            let capabilities: HashSet<_> = declaration.capabilities.iter().cloned().collect();
            if capabilities
                .iter()
                .any(|s| !["db", "geoip", "listener_details"].contains(&s.as_str()))
            {
                bail!("unsupported administrative plugin capability");
            }
            if !capabilities.contains("db") {
                bail!("history plugin requires db capability");
            }
            let bytes = std::fs::read(&declaration.wasm)
                .context("cannot read local administrative WASM")?;
            if bytes.len() > 32 * 1024 * 1024 {
                bail!("WASM exceeds 32 MiB");
            }
            let db = Arc::new(PluginDb::open(
                directory,
                &declaration.name,
                declaration.db,
            )?);
            let host = Host {
                db: db.clone(),
                geoip: geoip.clone(),
                capabilities,
            };
            let mut manifest = Manifest::new([Wasm::data(bytes)]).with_config(
                [(
                    "config".to_owned(),
                    serde_json::to_string(&declaration.config)?,
                )]
                .into_iter(),
            );
            manifest.timeout_ms = Some(2000);
            manifest.memory.max_pages = Some(1024);
            let functions = [
                Function::new(
                    "db_batch",
                    [PTR],
                    [PTR],
                    UserData::new(host.clone()),
                    db_batch,
                ),
                Function::new(
                    "geoip_lookup",
                    [PTR],
                    [PTR],
                    UserData::new(host),
                    geoip_lookup,
                ),
            ];
            let mut guest = PluginBuilder::new(manifest)
                .with_wasi(false)
                .with_cache_disabled()
                .with_functions(functions)
                .build()
                .context("cannot load administrative WASM")?;
            let migrations: Vec<String> =
                serde_json::from_str(guest.call::<_, &str>("db_migrations", "")?)?;
            if migrations.len() > 64 || migrations.iter().any(|s| s.len() > 256 * 1024) {
                bail!("plugin migrations exceed limits");
            }
            db.migrate(&migrations)?;
            plugins.push(Loaded {
                name: declaration.name.clone(),
                guest,
                db,
                last_observation: None,
                last_success: None,
                last_complete_collection: None,
                failures: 0,
                problem: String::new(),
            });
        }
        Ok(Arc::new(Self {
            settings,
            plugins: Mutex::new(plugins),
        }))
    }
    pub fn status(&self) -> Value {
        let plugins = self.plugins.lock().unwrap_or_else(|e| e.into_inner());
        json!({"interval_seconds":self.settings.sample_interval_seconds,"attribution":DATA_ATTRIBUTION,"plugins":plugins.iter().map(|p| json!({
            "name":p.name,"last_observation":p.last_observation,"last_success":p.last_success,
            "last_complete_collection":p.last_complete_collection,
            "failures":p.failures,"problem":p.problem,"database_limits":{"max_size_mb":p.db.limits().max_size_mb,"max_rows":p.db.limits().max_rows,"query_timeout_ms":p.db.limits().query_timeout_ms}
        })).collect::<Vec<_>>()})
    }
    fn record(&self, events: &[(String, Value)]) {
        let mut plugins = self.plugins.lock().unwrap_or_else(|e| e.into_inner());
        for loaded in plugins.iter_mut() {
            let declaration = self
                .settings
                .plugin
                .iter()
                .find(|p| p.name == loaded.name)
                .unwrap();
            if !declaration
                .capabilities
                .iter()
                .any(|c| c == "listener_details")
            {
                continue;
            }
            let mut ok = true;
            for (_, event) in events {
                let at = event["ListenerSnapshot"]["at"].as_u64().unwrap();
                loaded.last_observation = Some(at);
                // Never log plugin errors: guest error strings could include transient client data.
                if loaded
                    .guest
                    .call::<_, ()>("on_event", event.to_string())
                    .is_err()
                {
                    loaded.failures += 1;
                    loaded.problem = "plugin event failed; history has a gap".into();
                    ok = false;
                }
            }
            if ok {
                loaded.last_success = Some(now());
                if complete_collection(events) {
                    loaded.last_complete_collection = loaded.last_success;
                }
                loaded.problem.clear();
            }
        }
    }
    pub fn query(&self, request: HistoryRequest) -> Result<Value, Status> {
        let until = if request.until == 0 {
            now()
        } else {
            request.until
        };
        if request.since > until || until - request.since > 365 * 86400 || until > i64::MAX as u64 {
            return Err(Status::invalid_argument(
                "history window must be within 365 days",
            ));
        }
        let period = match request.period.as_str() {
            "hour" => Period::Hour,
            "day" => Period::Day,
            "week" => Period::Week,
            _ => return Err(Status::invalid_argument("period must be hour, day or week")),
        };
        let mount = (!request.mount.is_empty()).then_some(request.mount.as_str());
        let plugins = self
            .plugins
            .lock()
            .map_err(|_| Status::internal("history poisoned"))?;
        let plugin = plugins
            .iter()
            .find(|p| p.name == request.plugin)
            .ok_or_else(|| Status::not_found("history plugin not loaded"))?;
        let rows = plugin
            .db
            .query(
                &stats_query(period, request.since as i64, until as i64, mount),
                &Params::default(),
            )
            .map_err(|_| {
                Status::resource_exhausted(
                    "history query failed or exceeded limits; narrow the window",
                )
            })?;
        let coverage = plugin.db.query("SELECT mount, count(*) AS observations, sum(listeners IS NOT NULL) AS successful, sum(listeners IS NULL) AS failures, min(at) AS first_at, max(at) AS last_at FROM listener_snapshot WHERE at >= ?1 AND at <= ?2 AND (?3 = '' OR mount = ?3) GROUP BY mount", &Params::Positional(vec![json!(request.since),json!(until),json!(request.mount)]))
            .map_err(|_| Status::unavailable("history coverage unavailable"))?;
        Ok(
            json!({"since":request.since,"until":until,"period":request.period,"attribution":DATA_ATTRIBUTION,
            "statistics":rows,"coverage":coverage,"semantics":"sampled concurrent audience; successful zeros included, failures excluded from means; UTC; not unique listeners"}),
        )
    }
}
// A successful guest write of NULL is a recorded gap, not a complete collection.
fn complete_collection(events: &[(String, Value)]) -> bool {
    !events.is_empty()
        && events
            .iter()
            .all(|(_, event)| event["ListenerSnapshot"]["listeners"].is_array())
}
impl Admin {
    pub fn with_history(mut self, history: Arc<History>) -> Self {
        self.history = Some(history);
        self
    }
    /// Poll only configured public audience mounts; gaps are explicit and never backfilled.
    pub async fn collect_history(&self) {
        let Some(history) = &self.history else {
            return;
        };
        let mut interval = tokio::time::interval(Duration::from_secs(
            history.settings.sample_interval_seconds,
        ));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let mut events = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
            for mount in self.config.mounts.iter().filter(|m| m.audience) {
                let listeners = match tokio::time::timeout_at(deadline, self.listeners_snapshot(&mount.name)).await {
                    Ok(Ok(list)) if list.listeners.iter().all(|l| l.lookup_status != "error") => Some(list.listeners.into_iter().map(|l| json!({"id":l.id.to_string(),"ip":l.ip,"connected_seconds":l.connected_seconds,"user_agent":l.user_agent})).collect::<Vec<_>>()),
                    _ => None,
                };
                events.push((mount.name.clone(), json!({"ListenerSnapshot":{"mount":mount.name,"at":now(),"listeners":listeners}})));
            }
            let history = history.clone();
            if tokio::task::spawn_blocking(move || history.record(&events))
                .await
                .is_err()
            {
                eprintln!("history worker failed; collection gap");
            }
        }
    }
    pub fn history_geoip(&self) -> Option<Arc<Geoip>> {
        self.geoip.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_collection_includes_successful_zero_and_rejects_partial_gaps() {
        let event = |mount: &str, listeners| {
            (
                mount.into(),
                json!({"ListenerSnapshot":{"listeners":listeners}}),
            )
        };
        assert!(!complete_collection(&[]));
        assert!(complete_collection(&[
            event("/one", json!([])),
            event("/two", json!([{"ip":"127.0.0.1"}]))
        ]));
        assert!(!complete_collection(&[
            event("/one", json!([])),
            event("/two", Value::Null)
        ]));
    }

    #[test]
    #[ignore = "requires STATIOND_TEST_LISTENER_WASM"]
    fn existing_guest_roundtrip_retention_privacy_and_restart() {
        let wasm = std::env::var("STATIOND_TEST_LISTENER_WASM").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("admin.toml");
        std::fs::write(
            &settings,
            format!(
                r#"
sample_interval_seconds = 5
[[plugin]]
name = "listener-stats"
wasm = {wasm:?}
capabilities = ["db", "listener_details", "geoip"]
[plugin.config]
retention_days = 1
[plugin.db]
max_rows = 1000
query_timeout_ms = 1000
"#
            ),
        )
        .unwrap();
        let history = History::load(&settings, directory.path(), None).unwrap();
        let at = now();
        let event = |at, listeners| {
            vec![(
                "/radio.mp3".to_owned(),
                json!({"ListenerSnapshot":{"mount":"/radio.mp3","at":at,"listeners":listeners}}),
            )]
        };
        history.record(&event(at - 172800, json!([{"ip":"8.8.8.8"}])));
        let positive = event(at, json!([{"ip":"8.8.8.8"},{"ip":"8.8.8.8"}]));
        history.record(&positive);
        history.record(&positive);
        history.record(&event(at + 1, json!([])));
        history.record(&event(at + 2, Value::Null));
        let loaded = history.plugins.lock().unwrap();
        let p = &loaded[0];
        assert!(p.problem.is_empty());
        let query = |sql| {
            serde_json::to_value(p.db.query(sql, &Params::default()).unwrap()).unwrap()["rows"]
                .clone()
        };
        assert_eq!(
            query("SELECT listeners FROM listener_snapshot ORDER BY at"),
            json!([[2], [0], [null]])
        );
        assert_eq!(
            query("SELECT status,listeners FROM listener_geo"),
            json!([["unavailable", 2]])
        );
        let db_path = p.db.path().to_path_buf();
        // Same confined host: raw SQL cannot attach databases, mutate its migration ledger or query writes.
        assert!(p
            .db
            .query("DELETE FROM listener_geo", &Params::default())
            .is_err());
        assert!(p
            .db
            .exec(&Statement {
                sql: "ATTACH DATABASE '/tmp/escape.db' AS escape".into(),
                params: Params::default()
            })
            .is_err());
        drop(loaded);
        let result = history
            .query(HistoryRequest {
                plugin: "listener-stats".into(),
                since: at - 60,
                until: at + 2,
                period: "hour".into(),
                mount: "/radio.mp3".into(),
            })
            .unwrap();
        assert_eq!(result["coverage"]["rows"][0][1], 3);
        assert_eq!(result["statistics"]["rows"][0][5], 1.0); // (2 + 0) / 2; failure excluded
        assert!(!std::fs::read(&db_path)
            .unwrap()
            .windows(7)
            .any(|w| w == b"8.8.8.8"));
        drop(history);
        let restored = History::load(&settings, directory.path(), None).unwrap();
        assert_eq!(
            restored.plugins.lock().unwrap()[0]
                .db
                .query("SELECT count(*) FROM listener_snapshot", &Params::default())
                .unwrap()
                .rows,
            vec![vec![json!(3)]]
        );
    }
}
