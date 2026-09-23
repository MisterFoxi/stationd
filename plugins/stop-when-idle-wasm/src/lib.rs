//! Plugin WASM de démo A2 : `stop-when-idle`, version guest.
//!
//! Observe `ListenersSampled` (export `on_event`) et, à 0 auditeur, arme
//! l'arrêt gracieux via la host function `station_control` — la composition
//! « observer + agir » de Doc/plugin-host.md. Le core possède le mécanisme
//! (l'arrêt se fait au prochain bord de piste si l'audience est toujours à 0),
//! le plugin ne porte que la politique. Armer deux fois est un no-op côté core.
//!
//!   [[plugin]]
//!   name         = "stop-when-idle-wasm"
//!   wasm         = "plugins/stop-when-idle-wasm/target/wasm32-unknown-unknown/release/stop_when_idle_wasm.wasm"
//!   capabilities = ["control"]
//!
//! Sans la capacité `control`, l'appel hôte répond `{"ok":false,…}` (refus
//! visible dans les logs du daemon), il ne plante pas le guest.
//!
//! Forme JSON des événements (host, src/plugin.rs, enum externe) :
//!   {"ListenersSampled":{"count":0,"at":1790000000}}

use extism_pdk::*;
use serde_json::Value;

#[host_fn]
extern "ExtismHost" {
    fn station_control(input: String) -> String;
}

#[plugin_fn]
pub fn on_event(input: String) -> FnResult<()> {
    let event: Value = serde_json::from_str(&input)?;
    let Some(sample) = event.get("ListenersSampled") else {
        return Ok(()); // autre événement : ignoré (enum non exhaustif)
    };
    if sample.get("count").and_then(Value::as_u64) != Some(0) {
        return Ok(());
    }
    let reply = unsafe { station_control(r#"{"action":"stop_when_idle"}"#.to_string())? };
    info!("stop-when-idle-wasm: {reply}");
    Ok(())
}
