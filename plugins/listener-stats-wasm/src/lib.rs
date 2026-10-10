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

/// Retention is editable through the generic plugin configuration screen.
#[plugin_fn]
pub fn config_schema() -> FnResult<String> {
    Ok(json!([{
        "key": "retention_days", "label": "Historique conservé (jours)",
        "kind": "integer", "default": "30", "minimum": 1, "maximum": 365
    }]).to_string())
}

#[plugin_fn]
pub fn db_migrations() -> FnResult<String> {
    Ok(serde_json::to_string(&[
        include_str!("../migrations/001_snapshots.sql"),
        include_str!("../migrations/002_regions.sql"),
        include_str!("../migrations/003_stats_indexes.sql"),
        include_str!("../migrations/004_stream_index.sql"),
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
    let common = vec![
        json!({"key":"period", "shared":true, "label":"Période", "kind":"choice", "default_value":"24 h",
            "options":["Aujourd’hui", "24 h", "7 jours", "30 jours", "90 jours", "365 jours", "Tout", "Personnalisée"]}),
        json!({"key":"from", "shared":true, "label":"Début UTC (inclus)", "kind":"datetime", "default_value":""}),
        json!({"key":"to", "shared":true, "label":"Fin UTC (exclue)", "kind":"datetime", "default_value":""}),
        json!({"key":"mount", "shared":true, "label":"Flux (vide = tous)", "kind":"text", "default_value":""}),
    ];
    let grouping = |default: &str| json!({"key":"grouping", "label":"Cumul", "kind":"choice", "default_value":default,
        "options":["Total", "Heure", "Jour", "Semaine", "Mois", "Année", "Heure du jour", "Jour de semaine"]});
    let definitions = [
        ("audience", "Audience", "Total", include_str!("ui/audience.sql"),
            "Synthèse de la période par flux : moyenne, pic, minimum, cumul des observations et qualité de collecte. Dernier_effectif = dernier relevé de la période, pas forcément le direct. UTC ; zéros inclus, échecs exclus des moyennes."),
        ("hourly", "Évolution", "Heure", include_str!("ui/hourly.sql"),
            "Cumuls chronologiques : heure, jour, semaine (lundi), mois ou année. Période réglable ; dates UTC. Observations_auditeurs = somme des effectifs relevés, pas des auditeurs uniques. Limite 1000 lignes ; bornes partielles."),
        ("daily", "Habitudes d’écoute", "Jour de semaine", include_str!("ui/daily.sql"),
            "Profil de la période : heure du jour ou jour de semaine (1=lundi, 7=dimanche), en UTC. Moyenne, pic, minimum et cumul des observations ; relevés valides et échecs. Aucun relevé = aucune ligne. Pas de sessions ni d’auditeurs uniques."),
        ("geography", "Géographie", "Total", include_str!("ui/geography.sql"),
            "Pays, région ou ville, croisés avec les cumuls temporels UTC. Moyenne sur tous les relevés valides du flux, absences incluses. Part des observations, pas des personnes. Top 1000. GeoLite2 data created by MaxMind (https://www.maxmind.com). Historique : IP Geolocation by DB-IP (https://db-ip.com)."),
    ];
    let tabs: Vec<Value> = definitions.into_iter().map(|(id, title, default, sql, description)| {
        let mut filters = common.clone();
        filters.push(grouping(default));
        if id == "daily" { filters[0]["default_value"] = json!("30 jours"); }
        if id == "geography" {
            filters.push(json!({"key":"geography", "label":"Géographie", "kind":"choice",
                "default_value":"Pays", "options":["Pays", "Région", "Ville"]}));
        }
        json!({"id":id, "title":title, "description":description,
            "sql":format!("{}{}", include_str!("ui/period.sql"), sql), "filters":filters,
            "dashboard_sql":format!("{}{}",include_str!("ui/period.sql"),
                if id == "geography" {include_str!("ui/dashboard.sql").replace("'Pays'",":geography")} else {include_str!("ui/dashboard.sql").to_string()})})
    }).collect();
    Ok(serde_json::to_string(&tabs)?)
}
