//! Period-selectable media statistics based on actual Liquidsoap air starts/ends.
use extism_pdk::*;
use serde_json::{Value, json};
mod model;
#[host_fn]
extern "ExtismHost" {
    fn db_batch(input: String) -> String;
}
#[plugin_fn]
pub fn db_migrations() -> FnResult<String> {
    Ok(serde_json::to_string(&[
        model::LEGACY_MIGRATION,
        include_str!("../migrations/002_actual_plays.sql"),
        include_str!("../migrations/003_genres.sql"),
    ])?)
}
#[plugin_fn]
pub fn config_schema() -> FnResult<String> {
    Ok(
        json!([{"key":"retention_days", "label":"Historique médias conservé (jours)",
        "kind":"integer", "default":"365", "minimum":1, "maximum":365}])
        .to_string(),
    )
}
fn batch(statements: Vec<Value>) -> FnResult<()> {
    if statements.is_empty() {
        return Ok(());
    }
    let reply: Value =
        serde_json::from_str(&unsafe { db_batch(json!({"statements":statements}).to_string())? })?;
    if reply.get("ok") != Some(&Value::Bool(true)) {
        return Err(Error::msg("play-stats: database batch failed").into());
    }
    Ok(())
}
#[plugin_fn]
pub fn on_load() -> FnResult<()> {
    // A reload cannot prove the previous active media is still on air.
    batch(vec![
        json!({"sql":"UPDATE media_active SET play_id = NULL, started_at = NULL WHERE singleton = 1"}),
    ])
}
#[plugin_fn]
pub fn on_event(input: String) -> FnResult<()> {
    let event: Value = serde_json::from_str(&input)?;
    let config: Value =
        serde_json::from_str(&config::get("config")?.unwrap_or_else(|| "{}".into()))?;
    let days = match config.get("retention_days") {
        None => 365,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=365).contains(n))
            .ok_or_else(|| {
                Error::msg("play-stats: retention_days must be an integer from 1 to 365")
            })?,
    };
    batch(model::statements(&event, days as u32).map_err(Error::msg)?)
}
#[plugin_fn]
pub fn ui_tabs() -> FnResult<String> {
    let common = vec![
        json!({"key":"period", "label":"Période", "kind":"choice", "default_value":"30 jours", "shared":true,
            "options":["Aujourd’hui", "24 h", "7 jours", "30 jours", "90 jours", "365 jours", "Tout", "Personnalisée"]}),
        json!({"key":"from", "label":"Début UTC (inclus)", "kind":"datetime", "default_value":"", "shared":true}),
        json!({"key":"to", "label":"Fin UTC (exclue)", "kind":"datetime", "default_value":"", "shared":true}),
        json!({"key":"media", "label":"Recherche média/artiste", "kind":"text", "default_value":"", "shared":true}),
        json!({"key":"include_genres", "label":"Genres à inclure (virgules)", "kind":"text", "default_value":"", "shared":true}),
        json!({"key":"exclude_genres", "label":"Genres à exclure (virgules)", "kind":"text", "default_value":"", "shared":true}),
    ];
    let mut tabs = Vec::new();
    for (id, title, by, grouping) in [
        ("plays", "Diffusions médias", "Média", "Total"),
        ("timeline", "Médias / évolution", "Média", "Jour"),
        ("artists", "Artistes / albums", "Artiste", "Total"),
    ] {
        let mut filters = common.clone();
        filters.push(
            json!({"key":"by", "label":"Regrouper par", "kind":"choice", "default_value":by,
            "options":["Média","Artiste","Album","Playlist"]}),
        );
        filters.push(json!({"key":"grouping", "label":"Cumul", "kind":"choice", "default_value":grouping,
            "options":["Total","Heure","Jour","Semaine","Mois","Année","Heure du jour","Jour de semaine"]}));
        filters.push(json!({"key":"sorting", "label":"Classement", "kind":"choice", "default_value":"Passages",
            "options":["Passages","Durée","Audience"]}));
        tabs.push(json!({"id":id,"title":title,
            "description":"Diffusions réellement commencées dans la période UTC : passages, durée connue (hors pauses), complets/coupés et audience globale relevée. Durée entière rattachée au début du passage ; fins manquantes séparées. Moyenne d’audience par relevé, pas d’auditeurs uniques. 1000 lignes max. Genres exacts séparés par des virgules : inclusion = au moins un, exclusion prioritaire. Genres non connus des anciens passages : inclusion impossible. Statistiques best-effort depuis cette version.",
            "sql":include_str!("ui/media.sql"),"filters":filters,"dashboard_sql":include_str!("ui/dashboard.sql")}));
    }
    tabs.push(json!({"id":"legacy","title":"Sélections anciennes",
        "description":"Compteurs conservés de l’ancienne version : médias sélectionnés, pas forcément diffusés. Sans historique daté, période non filtrable. Les nouvelles diffusions sont dans Diffusions médias. Aucun mélange avec les statistiques réelles.",
        "sql":"SELECT media AS Media, plays AS Selections, datetime(last_at, 'unixepoch') AS Derniere_selection_UTC FROM play_count ORDER BY plays DESC, media LIMIT 1000"}));
    Ok(serde_json::to_string(&tabs)?)
}
