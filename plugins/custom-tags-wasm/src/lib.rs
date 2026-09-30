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

#[cfg(target_arch = "wasm32")]
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
    pub metadata: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub creation: Option<Creation>,
    #[serde(default)]
    pub tempo: Option<Tempo>,
}

/// Lit et valide la config JSON (la table TOML `[plugin.config]`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Creation {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub source_tags: Vec<String>,
    #[serde(default, rename = "match")]
    pub prefix: String,
    #[serde(default = "rfc3339")]
    pub date_format: String,
}

fn rfc3339() -> String { "rfc3339".into() }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tempo {
    /// Host policy; the guest only classifies the provided BPM.
    #[serde(default = "analysis_enabled")]
    pub analyze_missing: bool,
    /// Host policy too: the window estimates are folded into, `[lo, hi]`
    /// (checked by the host).
    #[serde(default)]
    pub analyze_range: Option<[f64; 2]>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "bpm_tags")]
    pub source_tags: Vec<String>,
    #[serde(default)]
    pub range: Vec<BpmRange>,
}

fn analysis_enabled() -> bool { true }

fn bpm_tags() -> Vec<String> { vec!["BPM".into()] }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BpmRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub value: String,
}

// Bounds are inclusive, omitted bounds are open. Overlap is rejected so that
// configuration order can never silently change the meaning of a BPM value.
fn validate_rules(cfg: &Config) -> Result<(), String> {
    let names_ok = |names: &[String]| !names.is_empty() && names.iter().all(|s| !s.trim().is_empty());
    if let Some(c) = cfg.creation.as_ref().filter(|c| c.enabled) {
        if !names_ok(&c.source_tags) || c.prefix.trim().is_empty() || c.date_format != "rfc3339" {
            return Err("custom-tags: creation needs source_tags, nonblank match and rfc3339".into());
        }
    }
    if let Some(t) = cfg.tempo.as_ref().filter(|t| t.enabled) {
        if !names_ok(&t.source_tags) || t.range.is_empty() {
            return Err("custom-tags: tempo needs source_tags and ranges".into());
        }
        for (i, r) in t.range.iter().enumerate() {
            if r.value.trim().is_empty() || [r.min, r.max].into_iter().flatten().any(|v| !v.is_finite() || v < 0.0)
                || r.min.unwrap_or(0.0) > r.max.unwrap_or(f64::INFINITY)
            {
                return Err("custom-tags: invalid BPM range".into());
            }
            for previous in &t.range[..i] {
                if r.min.unwrap_or(0.0) <= previous.max.unwrap_or(f64::INFINITY)
                    && previous.min.unwrap_or(0.0) <= r.max.unwrap_or(f64::INFINITY)
                {
                    return Err("custom-tags: overlapping BPM ranges".into());
                }
            }
        }
    }
    Ok(())
}

// Only the timestamp immediately after a matching phrase is considered.
// Leading whitespace is allowed; unrelated dates elsewhere are never used.
fn creation_value(value: &str, prefix: &str) -> Option<String> {
    for (position, _) in value.match_indices(prefix) {
        let tail = value[position + prefix.len()..].trim_start();
        let token = tail.split(|c: char| c == ';' || c == ',' || c.is_whitespace()).next()?;
        if let Ok(date) = chrono::DateTime::parse_from_rfc3339(token) {
            return Some(date.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true));
        }
    }
    None
}

fn metadata(cfg: &Config, tags: &[CustomTag]) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(c) = cfg.creation.as_ref().filter(|c| c.enabled) {
        // Source order is explicit; the first valid match wins for a scalar key.
        'creation: for name in &c.source_tags {
            for tag in tags.iter().filter(|t| t.name.trim().eq_ignore_ascii_case(name.trim())) {
                if let Some(value) = creation_value(&tag.value, &c.prefix) {
                    out.insert("creation".into(), value);
                    break 'creation;
                }
            }
        }
    }
    if let Some(t) = cfg.tempo.as_ref().filter(|t| t.enabled) {
        'tempo: for name in &t.source_tags {
            for tag in tags.iter().filter(|v| v.name.trim().eq_ignore_ascii_case(name.trim())) {
                let Ok(bpm) = tag.value.trim().parse::<f64>() else { continue };
                if !bpm.is_finite() || bpm <= 0.0 { continue }
                if let Some(r) = t.range.iter().find(|r| bpm >= r.min.unwrap_or(0.0) && bpm <= r.max.unwrap_or(f64::INFINITY)) {
                    out.insert("tempo".into(), r.value.trim().to_string());
                    break 'tempo;
                }
            }
        }
    }
    out
}

pub fn parse_config(json: &str) -> Result<Config, String> {
    let cfg: Config =
        serde_json::from_str(json).map_err(|e| format!("custom-tags: config invalide : {e}"))?;
    if (cfg.tags.is_empty()
        && !cfg.creation.as_ref().is_some_and(|c| c.enabled)
        && !cfg.tempo.as_ref().is_some_and(|t| t.enabled))
        || cfg.tags.iter().any(|t| t.trim().is_empty()) {
        return Err("custom-tags: `tags` doit lister au moins un nom de tag non vide".into());
    }
    validate_rules(&cfg)?;
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
            let metadata = metadata(cfg, &m.custom_tags);
            (!genres.is_empty() || !metadata.is_empty())
                .then(|| ScanEnrichment { rel_path: m.rel_path, genres, metadata })
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
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
                ScanEnrichment { metadata: Default::default(), rel_path: "a.mp3".into(), genres: vec!["talks".into()] },
                ScanEnrichment { metadata: Default::default(),
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
        assert_eq!(out, vec![ScanEnrichment { metadata: Default::default(), rel_path: "a.mp3".into(), genres: vec!["ID".into()] }]);
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

#[cfg(test)]
mod metadata_tests {
    use super::*;

    fn run(config: &str, name: &str, value: &str) -> Vec<ScanEnrichment> {
        enrich(&parse_config(config).unwrap(), vec![ScanInput {
            rel_path: "song.mp3".into(),
            custom_tags: vec![CustomTag { name: name.into(), value: value.into() }],
        }])
    }

    const CREATION: &str = r#"{"creation":{"enabled":true,"source_tags":["Comment","Description"],"match":"created="}}"#;
    const TEMPO: &str = r#"{"tempo":{"enabled":true,"range":[{"max":79.5,"value":"slow"},{"min":80,"max":119,"value":"medium"},{"min":120,"value":"fast"}]}}"#;

    #[test]
    fn creation_is_metadata_and_requires_the_configured_phrase() {
        let out = run(CREATION, "COMMENT", "other date 2020-01-01T00:00:00Z; created=2026-06-14T06:36:48Z;");
        assert_eq!(out[0].metadata["creation"], "2026-06-14T06:36:48Z");
        assert!(out[0].genres.is_empty());
        assert!(run(CREATION, "Comment", "2026-06-14T06:36:48Z").is_empty());
        assert!(run(CREATION, "Other", "created=2026-06-14T06:36:48Z").is_empty());
        assert!(run(CREATION, "Comment", "created=not-a-date 2026-06-14T06:36:48Z").is_empty());
        assert!(run(CREATION, "Comment", "created=2026-02-30T06:36:48Z").is_empty());
        assert!(run(CREATION, "Comment", "created=2026-06-14T06:36:48").is_empty());
        assert_eq!(run(CREATION, "Description", "created=bad; created=2026-06-14T08:36:48.123+02:00;")[0].metadata["creation"], "2026-06-14T08:36:48.123+02:00");
    }

    #[test]
    fn tempo_bounds_decimals_gaps_and_invalid_values() {
        for (bpm, expected) in [("59", "slow"), ("79.5", "slow"), ("80", "medium"), ("119", "medium"), ("120", "fast"), ("300", "fast")] {
            let out = run(TEMPO, "bpm", bpm);
            assert_eq!(out[0].metadata["tempo"], expected);
            assert!(out[0].genres.is_empty());
        }
        for bpm in ["0", "-1", "NaN", "inf", "abc", "79.75"] {
            assert!(run(TEMPO, "BPM", bpm).is_empty());
        }
    }

    #[test]
    fn legacy_genres_and_both_metadata_rules_coexist() {
        let cfg = parse_config(r#"{"tags":["Type"],"creation":{"enabled":true,"source_tags":["Comment"],"match":"made="},"tempo":{"enabled":true,"range":[{"value":"any"}]}}"#).unwrap();
        let out = enrich(&cfg, vec![ScanInput {
            rel_path: "x.mp3".into(),
            custom_tags: [("Type", "talks"), ("Comment", "made=2026-01-01T00:00:00Z"), ("BPM", "100")]
                .into_iter().map(|(name, value)| CustomTag { name: name.into(), value: value.into() }).collect(),
        }]);
        assert_eq!(out[0].genres, vec!["talks"]);
        assert_eq!(out[0].metadata.len(), 2);
    }

    #[test]
    fn bad_configuration_fails_loudly() {
        for raw in [
            r#"{"creation":{"enabled":true,"source_tags":["Comment"],"match":""}}"#,
            r#"{"creation":{"enabled":true,"source_tags":[],"match":"x"}}"#,
            r#"{"creation":{"enabled":true,"source_tags":["Comment"],"match":"x","date_format":"other"}}"#,
            r#"{"tempo":{"enabled":true,"range":[]}}"#,
            r#"{"tempo":{"enabled":true,"range":[{"min":100,"max":90,"value":"x"}]}}"#,
            r#"{"tempo":{"enabled":true,"range":[{"max":100,"value":"x"},{"min":100,"value":"y"}]}}"#,
            r#"{"tempo":{"enabled":true,"range":[{"min":-1,"value":"x"}]}}"#,
        ] { assert!(parse_config(raw).is_err(), "{raw}"); }
        assert!(run(r#"{"tags":["Type"],"creation":{"enabled":false},"tempo":{"enabled":false}}"#, "Comment", "created=2026-01-01T00:00:00Z").is_empty());
    }
}

#[cfg(test)]
mod analysis_config_tests {
    use super::*;
    #[test]
    fn offline_policy_is_accepted_and_defaults_to_enabled() {
        let config = parse_config(r#"{"tempo":{"enabled":true,"range":[{"value":"any"}]}}"#).unwrap();
        assert!(config.tempo.unwrap().analyze_missing);
        let config = parse_config(r#"{"tempo":{"enabled":true,"analyze_missing":false,"range":[{"value":"any"}]}}"#).unwrap();
        assert!(!config.tempo.unwrap().analyze_missing);
        let config = parse_config(r#"{"tempo":{"enabled":true,"analyze_range":[50,100],"range":[{"value":"any"}]}}"#).unwrap();
        assert_eq!(config.tempo.unwrap().analyze_range, Some([50.0, 100.0]));
    }
}
