//! Plugin WASM de démonstration (WASM-1) : `require-title`.
//!
//! Exporte `filter_pool` : ne garde que les candidats dont le tag `title` est
//! présent et non vide. Prouve l'ABI host↔wasm de bout en bout (le host
//! sérialise le pool en JSON, le wasm filtre, le host relit le pool filtré).
//!
//! La struct `Candidate` ci-dessous DOIT refléter la forme JSON du `Candidate`
//! côté host (stationd, src/plugin.rs) : mêmes noms de champs, même sémantique
//! des `Option`. Un crate de types partagé est l'étape propre suivante ; ici
//! on duplique pour garder WASM-1 sans restructuration en workspace.

use extism_pdk::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Candidate {
    rel_path: String,
    artist: Option<String>,
    title: Option<String>,
    album: Option<String>,
    year: Option<u32>,
    duration_ms: u64,
    genres: Vec<String>,
    mtime_ns: i64,
}

/// Garde uniquement les candidats ayant un titre non vide.
#[plugin_fn]
pub fn filter_pool(input: String) -> FnResult<String> {
    let candidates: Vec<Candidate> = serde_json::from_str(&input)?;
    let kept: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| c.title.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false))
        .collect();
    Ok(serde_json::to_string(&kept)?)
}
