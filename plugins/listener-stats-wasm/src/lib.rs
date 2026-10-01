//! V1: sampled concurrent audience by mount and geography, not unique people
//! or exact sessions. The host owns Icecast access and the SQLite handle.
use extism_pdk::*;
use serde_json::{json, Value};

mod model;

#[host_fn]
extern "ExtismHost" {
    fn db_batch(input: String) -> String;
    fn geoip_lookup(input: String) -> String;
}

#[plugin_fn]
pub fn db_migrations() -> FnResult<String> {
    Ok(serde_json::to_string(&[
        include_str!("../migrations/001_snapshots.sql"),
        include_str!("../migrations/002_regions.sql"),
    ])?)
}

#[plugin_fn]
pub fn on_event(input: String) -> FnResult<()> {
    let event: Value = serde_json::from_str(&input)?;
    let Some(sample) = event.get("ListenerSnapshot") else { return Ok(()) };
    let sample: model::Snapshot = serde_json::from_value(sample.clone())?;
    let groups = model::aggregate(&sample, |ip| {
        let reply = unsafe { geoip_lookup(json!({"ip": ip}).to_string()) }
            .map_err(|_| "GeoIP host call failed".to_string())?;
        let value: Value = serde_json::from_str(&reply).map_err(|_| "invalid GeoIP reply")?;
        if value.get("ok") != Some(&Value::Bool(true)) {
            return Err("GeoIP lookup refused".into());
        }
        serde_json::from_value(value).map_err(|_| "invalid GeoIP result".into())
    }).map_err(|e| Error::msg(e))?;
    let config: Value = serde_json::from_str(&config::get("config")?.unwrap_or_else(|| "{}".into()))?;
    let retention = match config.get("retention_days") {
        None => 30,
        Some(value) => value.as_u64().filter(|days| (1..=365).contains(days))
            .ok_or_else(|| Error::msg("listener-stats: retention_days must be an integer from 1 to 365"))?,
    };
    let request = json!({"statements": model::statements(&sample, groups, retention as u32)});
    let reply: Value = serde_json::from_str(&unsafe { db_batch(request.to_string())? })?;
    if reply.get("ok") != Some(&Value::Bool(true)) {
        // No raw reply/input: an error must not accidentally log client data.
        return Err(Error::msg("listener-stats: database batch failed").into());
    }
    Ok(())
}
