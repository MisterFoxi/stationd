//! Plugin WASM configurable : `blacklist`.
//!
//! Exporte `filter_pool` : retire les candidats dont le chemin commence par un
//! préfixe exclu, ou dont l'artiste est listé. La config vient du host via
//! extism (`config::get("config")` = le JSON de la table TOML `[plugin.config]`) :
//!
//!   [[plugin]]
//!   name = "blacklist"
//!   wasm = "plugins/blacklist-wasm/target/wasm32-unknown-unknown/release/blacklist_wasm.wasm"
//!   [plugin.config]
//!   exclude_path_prefixes = ["Podcast/Jingles/end/"]
//!   exclude_artists       = ["johannesnagy"]
//!
//! `Candidate` reflète la forme JSON du host (stationd, src/plugin.rs) —
//! dupliqué en attendant un crate de types partagé.

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

#[derive(Deserialize, Default)]
struct Cfg {
    #[serde(default)]
    exclude_path_prefixes: Vec<String>,
    #[serde(default)]
    exclude_artists: Vec<String>,
}

#[plugin_fn]
pub fn filter_pool(input: String) -> FnResult<String> {
    let cfg: Cfg = match config::get("config")? {
        Some(s) => serde_json::from_str(&s)?,
        None => Cfg::default(),
    };
    let candidates: Vec<Candidate> = serde_json::from_str(&input)?;
    let kept: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| {
            let path_excluded = cfg
                .exclude_path_prefixes
                .iter()
                .any(|p| c.rel_path.starts_with(p));
            let artist_excluded = c
                .artist
                .as_deref()
                .map(|a| cfg.exclude_artists.iter().any(|x| x == a))
                .unwrap_or(false);
            !(path_excluded || artist_excluded)
        })
        .collect();
    Ok(serde_json::to_string(&kept)?)
}
