//! Veille automatique, version WASM de stop-when-idle.
//! Par défaut : zéro auditeur. Avec max_connection_age = "12h" : toutes
//! les connexions sont anciennes. Le core recontrôle au bord de piste
//! et réveille sur une nouvelle connexion. Aucune IP n'est lue par ce guest.
//! Nécessite control ; le mode âge nécessite icecast.listener_snapshots.

use extism_pdk::*;
use serde_json::{json, Value};

#[host_fn]
extern "ExtismHost" {
    fn station_control(input: String) -> String;
    fn listener_connections(input: String) -> String;
}

fn settings() -> FnResult<(u32, Option<u64>)> {
    let value: Value = serde_json::from_str(&config::get("config")?.unwrap_or_else(|| "{}".into()))?;
    let min = match value.get("min_zero_samples") {
        None => 1,
        Some(v) => v.as_u64().filter(|n| *n >= 1 && *n <= u32::MAX as u64)
            .ok_or_else(|| Error::msg("min_zero_samples must be an integer >= 1"))? as u32,
    };
    let age = match value.get("max_connection_age") {
        None => None,
        Some(v) => {
            let text = v.as_str().ok_or_else(|| Error::msg("max_connection_age must be a duration such as 12h"))?;
            let bad = || Error::msg("max_connection_age must be a positive duration in s, m, h or d");
            if text.len() < 2 || !text.is_ascii() { return Err(bad().into()); }
            let (number, unit) = text.split_at(text.len() - 1);
            let multiplier = match unit { "s" => 1, "m" => 60, "h" => 3600, "d" => 86400, _ => return Err(bad().into()) };
            if number.starts_with('0') || !number.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad().into());
            }
            Some(number.parse::<u64>().ok().and_then(|n| n.checked_mul(multiplier))
                .filter(|n| *n > 0).ok_or_else(bad)?)
        }
    };
    Ok((min, age))
}

fn control(request: Value) -> FnResult<()> {
    let reply: Value = serde_json::from_str(&unsafe { station_control(request.to_string())? })?;
    if reply.get("ok") != Some(&Value::Bool(true)) {
        return Err(Error::msg("stop-when-idle-wasm: station control refused").into());
    }
    if reply.get("changed") == Some(&Value::Bool(true)) {
        info!("stop-when-idle-wasm: {}", reply);
    }
    Ok(())
}

#[plugin_fn]
pub fn on_load(_input: String) -> FnResult<()> {
    let (_, age) = settings()?;
    var::set("idle_streak", 0u32)?;
    if age.is_some() {
        let available: Value = serde_json::from_str(&unsafe { listener_connections("{}".into())? })?;
        if available.get("ok") != Some(&Value::Bool(true)) || available.get("enabled") != Some(&Value::Bool(true)) {
            return Err(Error::msg("max_connection_age requires [icecast] listener_snapshots = true").into());
        }
        // No persisted baseline after a load/reload: same conservative recovery
        // as the native plugin. Wake never overrides an operator's pause.
        control(json!({"action": "wake"}))?;
    }
    Ok(())
}

fn eligible(clients: &Value, age: u64) -> bool {
    clients.as_array().is_some_and(|clients| clients.iter().all(|c|
        c.get("connected_seconds").and_then(Value::as_u64).is_some_and(|seconds| seconds >= age)))
}

#[plugin_fn]
pub fn on_event(input: String) -> FnResult<()> {
    let event: Value = serde_json::from_str(&input)?;
    let (min, age) = settings()?;
    if let Some(age) = age {
        let Some(sample) = event.get("ConnectionsSampled") else { return Ok(()) };
        // Both the observed event and the latest host view must be eligible:
        // unknown/young queued samples reset the streak; old events cannot
        // arm a drain using a stale snapshot.
        if !eligible(&sample["connections"], age) {
            var::set("idle_streak", 0u32)?;
            return Ok(());
        }
        let current: Value = serde_json::from_str(&unsafe { listener_connections("{}".into())? })?;
        if current.get("ok") != Some(&Value::Bool(true)) || !eligible(&current["connections"], age) {
            var::set("idle_streak", 0u32)?;
            return Ok(());
        }
    } else {
        let Some(count) = event.get("ListenersSampled").and_then(|s| s.get("count")).and_then(Value::as_u64) else {
            return Ok(());
        };
        if count > 0 {
            var::set("idle_streak", 0u32)?;
            return control(json!({"action": "wake"}));
        }
    }
    let streak = var::get::<u32>("idle_streak")?.unwrap_or(0).saturating_add(1);
    var::set("idle_streak", streak)?;
    if streak == min {
        let mut request = json!({"action": "stop_when_idle"});
        if let Some(age) = age {
            request["max_connection_age"] = json!(age);
        }
        control(request)?;
    }
    Ok(())
}
