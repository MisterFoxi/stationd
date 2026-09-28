//! Plugin WASM de démo A2 : `stop-when-idle`, version guest.
//!
//! Observe `ListenersSampled` (export `on_event`) :
//! - à 0 auditeur, arme la mise en veille via la host function
//!   `station_control` (`stop_when_idle`) ;
//! - dès qu'un auditeur revient, réveille la station (`wake`).
//!
//! La composition « observer + agir » de Doc/plugin-host.md. Le core possède
//! le mécanisme (la veille arrive au prochain bord de piste si l'audience est
//! toujours à 0 ; `wake` ne quitte que `sleeping` — il ne dépause jamais, et
//! un arrêt opérateur est hors de portée des plugins), le plugin ne porte que
//! la politique. Armer deux fois, réveiller une station éveillée : no-op côté
//! core.
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
    let Some(count) = sample.get("count").and_then(Value::as_u64) else {
        return Ok(());
    };
    let action = if count == 0 { "stop_when_idle" } else { "wake" };
    let reply = unsafe { station_control(format!(r#"{{"action":"{action}"}}"#))? };
    // Un réveil sans effet (station déjà éveillée) répond changed=false :
    // on ne logue que ce qui a changé ou échoué.
    let quiet = serde_json::from_str::<Value>(&reply)
        .map(|r| r.get("ok") == Some(&Value::Bool(true)) && r.get("changed") == Some(&Value::Bool(false)))
        .unwrap_or(false);
    if !quiet {
        info!("stop-when-idle-wasm ({action}): {reply}");
    }
    Ok(())
}
