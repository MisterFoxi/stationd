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

/// Aggregated statistics owned by the plugin; the TUI stays generic.
#[plugin_fn]
pub fn ui_tabs() -> FnResult<String> {
    Ok(json!([
        {
            "id": "audience",
            "title": "Audience",
            "description": "Synthèse 24 h par mount : dernier effectif, moyenne, pic et % de collectes réussies. NULL = collecte inconnue. Vérifier Dernier_UTC pour la fraîcheur. Moyennes par relevé, pas d'auditeurs uniques.",
            "sql": include_str!("ui/audience.sql")
        },
        {
            "id": "hourly",
            "title": "Audience / heure",
            "description": "Évolution sur 24 h, par heure UTC et mount : moyenne, pic, minimum, relevés valides et échecs. Zéros inclus ; échecs exclus des moyennes. Heures sans relevé absentes ; période courante partielle.",
            "sql": include_str!("ui/hourly.sql")
        },
        {
            "id": "daily",
            "title": "Audience / jour",
            "description": "Évolution sur 30 jours (selon rétention), par jour UTC et mount. Moyennes par relevé, zéros inclus et échecs exclus. Jours sans relevé absents ; périodes aux bornes partielles.",
            "sql": include_str!("ui/daily.sql")
        },
        {
            "id": "geography",
            "title": "Géographie",
            "description": "Répartition sur 24 h : moyenne, pic et part par lieu et mount. Moyenne sur tous les relevés valides, y compris les absences du lieu. Part = proportion des observations d'auditeurs, pas des personnes uniques. Lieux inconnus conservés ; top 200. GeoLite2 data created by MaxMind (https://www.maxmind.com). Historique DB-IP : IP Geolocation by DB-IP (https://db-ip.com).",
            "sql": include_str!("ui/geography.sql")
        }
    ]).to_string())
}
