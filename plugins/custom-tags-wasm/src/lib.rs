//! Plugin WASM `custom-tags` : hook `on_scan`.
//!
//! Au scan de la bibliothèque, le core passe pour chaque média ses tags
//! personnalisés bruts (`TXXX:<description>` en ID3v2, clés Vorbis/APE, atomes
//! MP4 freeform). Ce plugin garde ceux dont le NOM est listé dans sa config et
//! renvoie leurs VALEURS comme genres supplémentaires → ils atterrissent dans
//! `media_genre`, donc les filtres `genre` des playlists les voient tels quels :
//!
//!   [[plugin]]
//!   name = "custom-tags"
//!   wasm = "plugins/custom-tags-wasm/target/wasm32-unknown-unknown/release/custom_tags_wasm.wasm"
//!   [plugin.config]
//!   tags = ["Type"]
//!
//! Un mp3 avec `TXXX:Type = talks` reçoit le genre `talks` ; une playlist
//! dynamique le sélectionne avec un filtre `genre` `has = "talks"`. Plusieurs
//! valeurs (ID3v2.4 multi-valué, ou plusieurs tags listés) → plusieurs genres.
//!
//! Comparaison des NOMS de tag insensible à la casse (`Type` = `TYPE`, les
//! commentaires Vorbis sont souvent en majuscules). Les valeurs sont gardées
//! telles quelles (le core replie la casse pour le filtrage, `genre_key`).
//!
//! Config absente, vide ou invalide → erreur à chaque scan (échec visible dans
//! `stationctl plugin list`), jamais un plugin qui n'ajoute rien en silence.
//!
//! `ScanInput` / `ScanEnrichment` reflètent la forme JSON du host (stationd,
//! src/plugin.rs) — dupliqués en attendant un crate de types partagé.

use extism_pdk::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct CustomTag {
    pub name: String,
    pub value: String,
}

/// Seuls les champs utiles ici ; les autres (title, artist…) sont ignorés.
#[derive(Deserialize)]
pub struct ScanInput {
    pub rel_path: String,
    #[serde(default)]
    pub custom_tags: Vec<CustomTag>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct ScanEnrichment {
    pub rel_path: String,
    pub genres: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub tags: Vec<String>,
}

/// Lit et valide la config JSON (la table TOML `[plugin.config]`).
pub fn parse_config(json: &str) -> Result<Config, String> {
    let cfg: Config =
        serde_json::from_str(json).map_err(|e| format!("custom-tags: config invalide : {e}"))?;
    if cfg.tags.is_empty() || cfg.tags.iter().any(|t| t.trim().is_empty()) {
        return Err("custom-tags: `tags` doit lister au moins un nom de tag non vide".into());
    }
    Ok(cfg)
}

/// Cœur du plugin, pur (testable hors wasm).
pub fn enrich(cfg: &Config, media: Vec<ScanInput>) -> Vec<ScanEnrichment> {
    let wanted: Vec<String> = cfg.tags.iter().map(|t| t.trim().to_lowercase()).collect();
    media
        .into_iter()
        .filter_map(|m| {
            let mut genres: Vec<String> = Vec::new();
            for t in &m.custom_tags {
                if !wanted.contains(&t.name.trim().to_lowercase()) {
                    continue;
                }
                let v = t.value.trim();
                if !v.is_empty() && !genres.iter().any(|g| g.to_lowercase() == v.to_lowercase()) {
                    genres.push(v.to_string());
                }
            }
            (!genres.is_empty()).then(|| ScanEnrichment { rel_path: m.rel_path, genres })
        })
        .collect()
}

#[plugin_fn]
pub fn on_scan(input: String) -> FnResult<String> {
    let raw = config::get("config")?.unwrap_or_default();
    let cfg = parse_config(&raw).map_err(|e| Error::msg(e))?;
    let media: Vec<ScanInput> = serde_json::from_str(&input)?;
    Ok(serde_json::to_string(&enrich(&cfg, media))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(json: &str) -> Vec<ScanInput> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn listed_tag_values_become_genres_case_insensitive_name() {
        let cfg = parse_config(r#"{"tags":["Type"]}"#).unwrap();
        let out = enrich(
            &cfg,
            input(
                r#"[
                {"rel_path":"a.mp3","title":"x","custom_tags":[{"name":"Type","value":"talks"}]},
                {"rel_path":"b.flac","custom_tags":[{"name":"TYPE","value":"music"},{"name":"TYPE","value":"instrumental"}]},
                {"rel_path":"c.mp3","custom_tags":[{"name":"MusicBrainz Album Id","value":"123"}]},
                {"rel_path":"d.mp3","custom_tags":[]}
            ]"#,
            ),
        );
        assert_eq!(
            out,
            vec![
                ScanEnrichment { rel_path: "a.mp3".into(), genres: vec!["talks".into()] },
                ScanEnrichment {
                    rel_path: "b.flac".into(),
                    genres: vec!["music".into(), "instrumental".into()]
                },
            ]
        );
    }

    #[test]
    fn duplicate_and_blank_values_are_dropped() {
        let cfg = parse_config(r#"{"tags":["Type","Kind"]}"#).unwrap();
        let out = enrich(
            &cfg,
            input(
                r#"[{"rel_path":"a.mp3","custom_tags":[
                {"name":"Type","value":"ID"},{"name":"Kind","value":"id"},{"name":"Type","value":"  "}]}]"#,
            ),
        );
        assert_eq!(out, vec![ScanEnrichment { rel_path: "a.mp3".into(), genres: vec!["ID".into()] }]);
    }

    #[test]
    fn missing_or_empty_config_is_an_error() {
        assert!(parse_config("").is_err());
        assert!(parse_config("{}").is_err());
        assert!(parse_config(r#"{"tags":[]}"#).is_err());
        assert!(parse_config(r#"{"tags":[" "]}"#).is_err());
        assert!(parse_config(r#"{"tags":["Type"],"oops":1}"#).is_err());
    }
}
