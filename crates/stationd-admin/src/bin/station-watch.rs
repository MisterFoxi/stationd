//! Independent, read-only central monitoring. Never controls audio services.
// tonic 0.12 uses Status as its transport error, including interceptor closures.
#![allow(clippy::result_large_err)]
use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use stationd_admin::{
    read_secret,
    rpc::{icecast_admin_client::IcecastAdminClient, Empty},
    Config,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/etc/stationd/monitor.toml")]
    settings: PathBuf,
    /// Print the last snapshot; exit 1 for active alerts, 2 for stale/unreadable state.
    #[arg(long)]
    check: bool,
    /// Run one observation (diagnostic only; does not update daemon state).
    #[arg(long)]
    once: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    bind: std::net::SocketAddr,
    #[serde(default)]
    admin_url: String,
    token_file: PathBuf,
    icecast_config: PathBuf,
    admin_config: PathBuf,
    state_file: PathBuf,
    interval_seconds: u64,
    grace_seconds: u64,
    collection_max_age_seconds: u64,
    geolite_max_age_days: u64,
    disk_min_available_bytes: u64,
    disk_min_available_percent: u64,
    disk_paths: Vec<PathBuf>,
    #[serde(default)]
    ignored_mounts: Vec<String>,
}

#[derive(Clone)]
struct MonitoringApi {
    snapshot: std::sync::Arc<tokio::sync::RwLock<Option<Value>>>,
    max_age: u64,
}
#[tonic::async_trait]
impl stationd_admin::rpc::monitoring_server::Monitoring for MonitoringApi {
    async fn get_status(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<tonic::Response<stationd_admin::rpc::JsonReply>, tonic::Status> {
        let state = self.snapshot.read().await;
        let snapshot = state
            .as_ref()
            .ok_or_else(|| tonic::Status::unavailable("first observation pending"))?;
        if stale(now(), snapshot["at"].as_u64(), self.max_age) {
            return Err(tonic::Status::unavailable("monitor observation stale"));
        }
        Ok(tonic::Response::new(stationd_admin::rpc::JsonReply {
            json: snapshot.to_string(),
        }))
    }
}
impl Settings {
    fn validate(&self) -> Result<()> {
        if !(5..=3600).contains(&self.interval_seconds)
            || self.grace_seconds > 3600
            || self.collection_max_age_seconds < self.interval_seconds
            || !(1..=365).contains(&self.geolite_max_age_days)
            || self.disk_min_available_percent > 100
            || self.disk_paths.is_empty()
        {
            bail!("invalid monitoring thresholds");
        }
        Ok(())
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Condition {
    since: u64,
    active: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct Tracker {
    conditions: BTreeMap<String, Condition>,
}
impl Tracker {
    // Missing observations mean unknown: they must never resolve an existing alert.
    fn update(&mut self, at: u64, grace: u64, checks: &BTreeMap<String, bool>) -> Vec<Value> {
        let mut checks = checks.clone();
        // Successful inventory refresh retires conditions removed by configuration.
        // An unavailable dependency leaves its previous conditions untouched.
        for key in self.conditions.keys() {
            let refresh = (key.starts_with("source:")
                && checks.get("icecast:status_unavailable") == Some(&false)
                && checks.get("configuration:icecast") == Some(&false))
                || (key.starts_with("collection:")
                    && key != "collection:status_unavailable"
                    && checks.get("collection:status_unavailable") == Some(&false)
                    && checks.get("configuration:icecast") == Some(&false)
                    && checks.get("configuration:admin") == Some(&false));
            if refresh && !checks.contains_key(key) {
                checks.insert(key.clone(), false);
            }
        }
        let mut events = Vec::new();
        for (key, failed) in &checks {
            if *failed {
                let state = self.conditions.entry(key.clone()).or_insert(Condition {
                    since: at,
                    active: false,
                });
                if !state.active && at.saturating_sub(state.since) >= grace {
                    state.active = true;
                    events
                        .push(json!({"at":at,"event":"alert","condition":key,"since":state.since}));
                }
            } else if let Some(state) = self.conditions.remove(key) {
                if state.active {
                    events.push(
                        json!({"at":at,"event":"resolved","condition":key,"since":state.since}),
                    );
                }
            }
        }
        events
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn stale(at: u64, value: Option<u64>, max_age: u64) -> bool {
    value.is_none_or(|value| value > at || at - value > max_age)
}

fn history_checks(
    at: u64,
    max_age: u64,
    expected: &[String],
    value: &Value,
    checks: &mut BTreeMap<String, bool>,
) {
    for name in expected {
        let plugin = value["plugins"]
            .as_array()
            .and_then(|ps| ps.iter().find(|p| p["name"].as_str() == Some(name)));
        let failed = plugin.is_none_or(|p| {
            stale(at, p["last_observation"].as_u64(), max_age)
                || stale(at, p["last_success"].as_u64(), max_age)
                || p.get("last_complete_collection")
                    .is_some_and(|value| stale(at, value.as_u64(), max_age))
                || p["problem"].as_str() != Some("")
        });
        checks.insert(format!("collection:{name}"), failed);
    }
}

fn parse_df(text: &str) -> Result<(u64, u64)> {
    // GNU df -Pk: filesystem, 1024-blocks, used, available, capacity, mountpoint.
    let columns: Vec<_> = text
        .lines()
        .nth(1)
        .context("missing df row")?
        .split_whitespace()
        .collect();
    if columns.len() < 6 {
        bail!("invalid df row");
    }
    let total: u64 = columns[1].parse()?;
    let available: u64 = columns[3].parse()?;
    if total == 0 {
        bail!("empty filesystem");
    }
    Ok((
        available.saturating_mul(1024),
        ((u128::from(available) * 100) / u128::from(total)) as u64,
    ))
}

async fn observe(settings: &Settings) -> (BTreeMap<String, bool>, Value) {
    let mut checks = BTreeMap::new();
    let at = now();
    let mut details = json!({"at":at,"disks":[]});
    // Local configuration and MMDB remain observable while administration is down.
    let config = std::fs::read_to_string(&settings.icecast_config)
        .ok()
        .and_then(|raw| Config::parse(&raw).ok());
    checks.insert("configuration:icecast".into(), config.is_none());
    if let Some(config) = &config {
        for name in &settings.ignored_mounts {
            checks.insert(
                format!("configuration:ignored_mount:{name}"),
                !config.mounts.iter().any(|m| &m.name == name),
            );
        }
        if config.geoip.enabled {
            match config.open_geoip() {
                Ok(Some(db)) => {
                    let epoch = db.database_build_epoch();
                    details["geolite_build_epoch"] = json!(epoch);
                    checks.insert("geolite:unavailable".into(), false);
                    checks.insert(
                        "geolite:stale".into(),
                        stale(at, Some(epoch), settings.geolite_max_age_days * 86400),
                    );
                }
                _ => {
                    checks.insert("geolite:unavailable".into(), true);
                }
            }
        } else {
            checks.insert("geolite:unavailable".into(), false);
            checks.insert("geolite:stale".into(), false);
        }
    }
    for path in &settings.disk_paths {
        let usage = std::process::Command::new("df")
            .args(["-Pk", "--"])
            .arg(path)
            .env("LC_ALL", "C")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_df(&String::from_utf8_lossy(&o.stdout)).ok());
        checks.insert(
            format!("disk:unavailable:{}", path.display()),
            usage.is_none(),
        );
        if let Some((available, percent)) = usage {
            checks.insert(
                format!("disk:low:{}", path.display()),
                available < settings.disk_min_available_bytes
                    || percent < settings.disk_min_available_percent,
            );
            details["disks"]
                .as_array_mut()
                .unwrap()
                .push(json!({"path":path,"available_bytes":available,"available_percent":percent}));
        }
    }
    let admin_settings = std::fs::read_to_string(&settings.admin_config)
        .ok()
        .and_then(|raw| toml::from_str::<stationd_admin::history::Settings>(&raw).ok());
    checks.insert("configuration:admin".into(), admin_settings.is_none());
    let connected = async {
        let token: tonic::metadata::MetadataValue<_> =
            format!("Bearer {}", read_secret(&settings.token_file)?).parse()?;
        let origin = stationd_admin::discovery::resolve(
            &settings.admin_url,
            "stationdeck.icecast.streams",
            "grpc",
        )
        .await?;
        let channel = tonic::transport::Endpoint::from_shared(origin.to_string())?
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .connect()
            .await?;
        Ok::<_, anyhow::Error>(IcecastAdminClient::with_interceptor(
            channel,
            move |mut req: tonic::Request<()>| {
                req.metadata_mut().insert("authorization", token.clone());
                req.set_timeout(Duration::from_secs(5));
                Ok(req)
            },
        ))
    }
    .await;
    match connected {
        Err(error) => {
            details["admin_connection"] = json!({"code": error.downcast_ref::<stationdeck_discovery::Error>()
                .map(|e| e.code()).unwrap_or("connection_failed")});
            checks.insert("admin:unavailable".into(), true);
        }
        Ok(mut client) => {
            // Do not conflate an available transport with an authenticated, responding API.
            let status = client.get_status(Empty {}).await;
            let history = client.get_history_status(Empty {}).await;
            checks.insert(
                "admin:unavailable".into(),
                status.is_err() && history.is_err(),
            );
            checks.insert("icecast:status_unavailable".into(), status.is_err());
            if let (Ok(status), Some(config)) = (status, config.as_ref()) {
                let mounts = status.into_inner().mounts;
                for mount in &config.mounts {
                    if !settings.ignored_mounts.contains(&mount.name) {
                        checks.insert(
                            format!("source:{}", mount.name),
                            !mounts.iter().any(|m| m.mount == mount.name && m.connected),
                        );
                    }
                }
            }
            let history = history
                .ok()
                .and_then(|reply| serde_json::from_str::<Value>(&reply.into_inner().json).ok());
            checks.insert("collection:status_unavailable".into(), history.is_none());
            if let (Some(history), Some(admin_settings), Some(config)) =
                (history, admin_settings, config)
            {
                checks.insert(
                    "configuration:collection_threshold".into(),
                    settings.collection_max_age_seconds
                        < admin_settings.sample_interval_seconds.saturating_mul(2),
                );
                let expected: Vec<_> = admin_settings
                    .plugin
                    .into_iter()
                    .filter(|_| config.mounts.iter().any(|m| m.audience))
                    .filter(|p| p.capabilities.iter().any(|c| c == "listener_details"))
                    .map(|p| p.name)
                    .collect();
                history_checks(
                    at,
                    settings.collection_max_age_seconds,
                    &expected,
                    &history,
                    &mut checks,
                );
            }
        }
    }
    (checks, details)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let settings: Settings = toml::from_str(&std::fs::read_to_string(args.settings)?)?;
    settings.validate()?;
    if args.check {
        let probe = async {
            let token = format!("Bearer {}", read_secret(&settings.token_file)?).parse()?;
            let address = std::net::SocketAddr::new(
                if settings.bind.is_ipv6() {
                    std::net::Ipv6Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv4Addr::LOCALHOST.into()
                },
                settings.bind.port(),
            );
            let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))?
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(2))
                .connect()
                .await?;
            let mut request = tonic::Request::new(Empty {});
            request.metadata_mut().insert("authorization", token);
            request.set_timeout(Duration::from_secs(2));
            let reply = stationd_admin::rpc::monitoring_client::MonitoringClient::new(channel)
                .get_status(request)
                .await?
                .into_inner();
            Ok::<Value, anyhow::Error>(serde_json::from_str(&reply.json)?)
        }
        .await;
        let snapshot = match probe {
            Ok(value) => value,
            Err(_) => {
                eprintln!("monitor gRPC unavailable or stale");
                std::process::exit(2);
            }
        };
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        if stale(
            now(),
            snapshot["at"].as_u64(),
            settings.interval_seconds * 3 + 30,
        ) {
            std::process::exit(2);
        }
        std::process::exit(
            if snapshot["active"].as_array().is_some_and(|a| a.is_empty()) {
                0
            } else {
                1
            },
        );
    }
    let mut tracker = Tracker::default();
    if args.once {
        let (checks, details) = observe(&settings).await;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"checks":checks,"details":details}))?
        );
        return Ok(());
    }
    let _announcer = stationdeck_discovery::start_from_env().await?;
    let snapshot = std::sync::Arc::new(tokio::sync::RwLock::new(None));
    let api = MonitoringApi {
        snapshot: snapshot.clone(),
        max_age: settings.interval_seconds * 3 + 30,
    };
    let expected = format!("Bearer {}", read_secret(&settings.token_file)?);
    let service = stationd_admin::rpc::monitoring_server::MonitoringServer::with_interceptor(
        api,
        move |req: tonic::Request<()>| {
            let received = req
                .metadata()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !stationd_admin::authorized(&expected, received) {
                return Err(tonic::Status::unauthenticated("admin token required"));
            }
            Ok(req)
        },
    );
    let mut server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve(settings.bind)
            .await
    });
    loop {
        let (checks, details) = observe(&settings).await;
        let at = now();
        for event in tracker.update(at, settings.grace_seconds, &checks) {
            println!("{event}");
        }
        let active: Vec<_> = tracker
            .conditions
            .iter()
            .filter(|(_, s)| s.active)
            .map(|(k, _)| k)
            .collect();
        let current = json!({"at":at,"active":active,"checks":checks,"details":details,"conditions":tracker.conditions});
        let temporary = settings.state_file.with_extension("tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&current)?)?;
        std::fs::rename(temporary, &settings.state_file)?;
        *snapshot.write().await = Some(current);
        tokio::select! {
            result = &mut server => { result??; bail!("monitor gRPC server stopped"); },
            _ = tokio::time::sleep(Duration::from_secs(settings.interval_seconds)) => {},
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn grpc_rejects_pending_and_stale_snapshots_but_returns_alerts() {
        use stationd_admin::rpc::monitoring_server::Monitoring;
        let snapshot = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let api = MonitoringApi {
            snapshot: snapshot.clone(),
            max_age: 30,
        };
        assert_eq!(
            api.get_status(tonic::Request::new(Empty {}))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::Unavailable
        );
        *snapshot.write().await = Some(json!({"at":now(),"active":["source:/seven"]}));
        let response = api
            .get_status(tonic::Request::new(Empty {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            serde_json::from_str::<Value>(&response.json).unwrap()["active"][0],
            "source:/seven"
        );
        *snapshot.write().await = Some(json!({"at":now()-31,"active":[]}));
        assert_eq!(
            api.get_status(tonic::Request::new(Empty {}))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::Unavailable
        );
    }
    #[test]
    fn grace_dedup_recovery_and_unknown() {
        let mut t = Tracker::default();
        let bad = BTreeMap::from([("source:/x".into(), true)]);
        assert!(t.update(100, 30, &bad).is_empty());
        assert!(t.update(129, 30, &bad).is_empty());
        assert_eq!(t.update(130, 30, &bad)[0]["event"], "alert");
        assert!(t.update(160, 30, &bad).is_empty());
        assert!(t.update(170, 30, &BTreeMap::new()).is_empty());
        assert!(t.conditions["source:/x"].active);
        assert_eq!(
            t.update(180, 30, &BTreeMap::from([("source:/x".into(), false)]))[0]["event"],
            "resolved"
        );
        assert!(t.conditions.is_empty());
    }
    #[test]
    fn transient_and_clock_rollback_do_not_alert() {
        let mut t = Tracker::default();
        let bad = BTreeMap::from([("x".into(), true)]);
        assert!(t.update(100, 30, &bad).is_empty());
        assert!(t.update(90, 30, &bad).is_empty());
        assert!(t
            .update(110, 30, &BTreeMap::from([("x".into(), false)]))
            .is_empty());
        assert!(t.update(200, 30, &bad).is_empty());
    }
    #[test]
    fn timestamps_and_history_are_not_fooled_by_failures_counter() {
        assert!(stale(100, None, 30));
        assert!(stale(100, Some(101), 30));
        assert!(!stale(100, Some(70), 30));
        let expected = vec!["one".into(), "two".into(), "three".into()];
        let mut checks = BTreeMap::new();
        history_checks(
            100,
            30,
            &expected,
            &json!({"plugins":[
                {"name":"one","last_observation":90,"last_success":90,"problem":"","failures":5},
                {"name":"two","last_observation":90,"last_success":50,"problem":""}
            ]}),
            &mut checks,
        );
        assert!(!checks["collection:one"]);
        assert!(checks["collection:two"]);
        assert!(checks["collection:three"]);
        history_checks(
            100,
            30,
            &["four".into()],
            &json!({"plugins":[
                {"name":"four","last_observation":90,"last_success":90,"last_complete_collection":null,"problem":""}
            ]}),
            &mut checks,
        );
        assert!(checks["collection:four"]);
    }
    #[test]
    fn df_available_and_invalid_filesystems() {
        assert_eq!(parse_df("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 1000 400 500 45% /a b\n").unwrap(), (512000, 50));
        assert!(parse_df("header\n/dev/x 0 0 0 0% /\n").is_err());
        assert!(parse_df("header\nbad\n").is_err());
    }
    #[test]
    fn dynamic_inventory_retires_only_after_successful_refresh() {
        let mut t = Tracker::default();
        let bad: BTreeMap<_, _> = (0..7)
            .map(|n| (format!("source:/station-{n}"), true))
            .collect();
        assert_eq!(t.update(100, 0, &bad).len(), 7);
        assert!(t
            .update(
                120,
                0,
                &BTreeMap::from([("icecast:status_unavailable".into(), true)])
            )
            .iter()
            .all(|event| event["event"] != "resolved"));
        let mut refreshed = BTreeMap::from([
            ("icecast:status_unavailable".into(), false),
            ("configuration:icecast".into(), false),
        ]);
        refreshed.insert("source:/station-6".into(), true);
        let events = t.update(140, 0, &refreshed);
        assert_eq!(
            events.iter().filter(|e| e["event"] == "resolved").count(),
            7
        ); // six retired sources and status
        assert!(t.conditions["source:/station-6"].active);
        assert_eq!(t.conditions.len(), 1);
    }
}
