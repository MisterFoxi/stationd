//! Plugin WASM de démo : `play-stats`, capacité `db`.
//!
//! Compte les passages par média dans SA base (un fichier SQLite ouvert par le
//! core pour lui, cf. Doc/plugin-host.md) :
//!
//!   [[plugin]]
//!   name         = "play-stats"
//!   enabled      = true
//!   wasm         = "plugins/play-stats-wasm/target/wasm32-unknown-unknown/release/play_stats_wasm.wasm"
//!   capabilities = ["db"]
//!   # [plugin.db]            # bornes facultatives (valeurs par défaut)
//!   # max_size_mb      = 64
//!   # query_timeout_ms = 200
//!   # max_rows         = 10000
//!
//! Lecture : `stationctl plugin db play-stats query "SELECT * FROM play_count
//! ORDER BY plays DESC LIMIT 20"`.
//!
//! - export `db_migrations` : le schéma, une migration par entrée (version =
//!   rang + 1). Appliquées par le core AVANT le chargement ; une migration déjà
//!   appliquée ne se modifie jamais (on en ajoute une nouvelle).
//! - export `on_event` : `TrackResolved` avec un média → `db_exec` (UPSERT).
//!
//! Les appels hôte répondent toujours du JSON : `{"ok":true,…}` ou
//! `{"ok":false,"error":…}` (refus, SQL invalide, délai dépassé…) — jamais un
//! trap. Ici un refus est journalisé (niveau warn côté guest).

use extism_pdk::*;
use serde_json::{json, Value};

#[host_fn]
extern "ExtismHost" {
    fn db_exec(input: String) -> String;
}

#[plugin_fn]
pub fn db_migrations() -> FnResult<String> {
    let migrations = [
        // v1
        "CREATE TABLE play_count (
             media   TEXT    PRIMARY KEY,
             plays   INTEGER NOT NULL,
             last_at INTEGER NOT NULL
         )",
    ];
    Ok(serde_json::to_string(&migrations)?)
}

#[plugin_fn]
pub fn on_event(input: String) -> FnResult<()> {
    let event: Value = serde_json::from_str(&input)?;
    let Some(media) = event
        .get("TrackResolved")
        .and_then(|t| t.get("media_path"))
        .and_then(Value::as_str)
    else {
        return Ok(()); // autre événement, ou repli sans média
    };
    let req = json!({
        "sql": "INSERT INTO play_count (media, plays, last_at) VALUES (:media, 1, unixepoch())
                ON CONFLICT(media) DO UPDATE SET plays = plays + 1, last_at = unixepoch()",
        "params": { "media": media },
    });
    let reply: Value = serde_json::from_str(&unsafe { db_exec(req.to_string())? })?;
    if reply.get("ok") != Some(&Value::Bool(true)) {
        warn!("play-stats: db_exec refused: {reply}");
    }
    Ok(())
}

/// Declarative UI owned by this plugin, discovered by any compatible TUI.
#[plugin_fn]
pub fn ui_tabs() -> FnResult<String> {
    Ok(json!([{
        "id": "plays",
        "title": "Diffusions",
        "description": "Passages par média · dernière diffusion en UTC",
        "sql": "SELECT media AS Media, plays AS Passages, datetime(last_at, 'unixepoch') AS Dernier_UTC FROM play_count ORDER BY plays DESC, media LIMIT 200"
    }]).to_string())
}