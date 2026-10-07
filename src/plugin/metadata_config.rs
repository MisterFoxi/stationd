//! Station-owned metadata target in the plugin-config editor.
use crate::{
    config::{MetadataField, MetadataRule, validate_metadata_rules},
    plugin_config as cfg,
    proto::plugin as wire,
};
use std::path::Path;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table};

pub const TARGET: &str = "@liquidsoap-metadata";

pub(super) fn read(path: &Path) -> Result<(Vec<u8>, Vec<MetadataRule>), String> {
    let bytes = std::fs::read(path).map_err(|_| "cannot read station configuration")?;
    if bytes.len() > 1024 * 1024 {
        return Err("configuration exceeds 1 MiB".into());
    }
    let config: crate::config::Config = toml::from_str(
        std::str::from_utf8(&bytes).map_err(|_| "configuration is not UTF-8")?
    ).map_err(|_| "invalid station configuration")?;
    let ls = config.liquidsoap.ok_or("Liquidsoap is not configured in this station")?;
    validate_metadata_rules(&ls.metadata_rules)?;
    Ok((bytes, ls.metadata_rules))
}

fn summary(rule: Option<&MetadataRule>) -> String {
    let Some(rule) = rule else { return "(aucune règle)".into() };
    let origin = match rule.origin.as_deref() {
        None => "Tous les tags".to_string(),
        Some("") => "Genre natif".to_string(),
        Some(source) => source.to_string(),
    };
    let display = if let Some(text) = &rule.text {
        format!("texte fixe : {text}")
    } else {
        let fields: Vec<_> = rule.fields.iter().map(|f| match f {
            MetadataField::Title => "titre",
            MetadataField::Artist => "interprète",
            MetadataField::Album => "album",
        }).collect();
        if fields.is_empty() { "affichage masqué".into() } else { fields.join(", ") }
    };
    format!("{origin} / {} : {display}", rule.tag)
}

fn save(path: &Path, bytes: &[u8], rules: &[MetadataRule]) -> Result<String, String> {
    let mut doc = std::str::from_utf8(bytes).map_err(|_| "invalid UTF-8")?
        .parse::<DocumentMut>().map_err(|_| "invalid TOML")?;
    let ls = doc.get_mut("liquidsoap").and_then(Item::as_table_mut)
        .ok_or("use a [liquidsoap] table to edit metadata rules")?;
    let old = ls.get("metadata_rule").and_then(Item::as_array_of_tables).cloned();
    let mut tables = ArrayOfTables::new();
    for (i, rule) in rules.iter().enumerate() {
        // Keep comments attached to matching rules when reordering them.
        let mut table = old.as_ref().and_then(|tables| tables.iter().find(|t| {
            t.get("tag").and_then(Item::as_str) == Some(rule.tag.as_str())
                && t.get("origin").and_then(Item::as_str) == rule.origin.as_deref()
        }).or_else(|| tables.get(i))).cloned().unwrap_or_else(Table::new);
        let mut set = |key: &str, mut value: Item| {
            if let (Some(old), Some(new)) = (table.get(key).and_then(Item::as_value), value.as_value_mut()) {
                *new.decor_mut() = old.decor().clone();
            }
            table.insert(key, value);
        };
        set("tag", toml_edit::value(&rule.tag));
        let mut fields = Array::new();
        for field in &rule.fields {
            fields.push(match field {
                MetadataField::Title => "title",
                MetadataField::Artist => "artist",
                MetadataField::Album => "album",
            });
        }
        set("fields", toml_edit::value(fields));
        if let Some(origin) = &rule.origin { set("origin", toml_edit::value(origin)); }
        if let Some(text) = &rule.text { set("text", toml_edit::value(text)); }
        if rule.origin.is_none() { table.remove("origin"); }
        if rule.text.is_none() { table.remove("text"); }
        tables.push(table);
    }
    if rules.is_empty() { ls.remove("metadata_rule"); }
    else { ls.insert("metadata_rule", Item::ArrayOfTables(tables)); }
    cfg::write_atomic(path, bytes, doc.to_string().as_bytes())
}

pub(super) fn operation(
    path: &Path,
    runtime: &[MetadataRule],
    req: wire::PluginConfigUpdateRequest,
    read_only: bool,
) -> Result<wire::PluginConfigResponse, String> {
    let (bytes, mut rules) = read(path)?;
    let revision = cfg::revision(&bytes);
    let schema_revision = cfg::revision(b"liquidsoap-metadata-rules-v1");
    let mut response = wire::PluginConfigResponse {
        name: TARGET.into(), revision: revision.clone(), schema_revision: schema_revision.clone(),
        ..Default::default()
    };
    if !read_only {
        if !(0..=2).contains(&req.mode) { return Err("invalid configuration action".into()); }
        if req.mode == 2 {
            return Err("metadata rules require restarting stationd and Liquidsoap; choose Save".into());
        }
        if req.revision != revision || req.schema_revision != schema_revision {
            return Err("conflict: configuration or schema changed; refresh before saving".into());
        }
        if req.edits.len() > 1 || req.edits.iter().any(|e| e.key != "rules") {
            return Err("unknown or duplicate metadata configuration field".into());
        }
        let candidate = match req.edits.first() {
            None => rules.clone(),
            Some(e) if !e.present => vec![],
            Some(e) => {
                if e.value.len() > 65536 { return Err("metadata rules exceed 64 KiB".into()); }
                serde_json::from_str::<Vec<MetadataRule>>(&e.value).map_err(|_| "invalid metadata rules")?
            }
        };
        validate_metadata_rules(&candidate)?;
        for i in 0..rules.len().max(candidate.len()) {
            if rules.get(i) != candidate.get(i) {
                response.changes.push(format!("Règle {} : {} → {}", i + 1,
                    summary(rules.get(i)), summary(candidate.get(i))));
            }
        }
        if req.mode == 1 {
            response.revision = save(path, &bytes, &candidate)?;
            let (stored_bytes, stored) = read(path)?;
            if cfg::revision(&stored_bytes) != response.revision || stored != candidate {
                return Err("conflict: configuration changed after saving; refresh before restarting".into());
            }
            response.saved = true;
            response.message = "Règles enregistrées ; redémarrer stationd puis Liquidsoap pour les appliquer".into();
            rules = stored;
        }
    }
    response.pending = rules != runtime;
    response.fields = vec![wire::PluginConfigField {
        key: "rules".into(), label: "Règles de métadonnées".into(), kind: "metadata_rules".into(),
        default_value: Some("[]".into()), ..Default::default()
    }];
    response.values = vec![wire::PluginConfigValue {
        key: "rules".into(), present: true,
        value: serde_json::to_string(&rules).map_err(|_| "cannot encode metadata rules")?,
        ..Default::default()
    }];
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stationd.toml");
        let text = format!("{}\n[liquidsoap]\nscript_path = 'station.liq'\napi_token = 'keep-secret' # secret comment\nnormalize = true\n[[liquidsoap.output]]\nhost = 'localhost'\nport = 8000\npassword = 'keep-password'\nmount = '/radio.mp3'\n[[liquidsoap.metadata_rule]]\ntag = 'jingle' # tag comment\norigin = 'Type'\ntext = 'Before' # text comment\n", include_str!("../../stationd.example.toml"));
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }
    fn request(data: &wire::PluginConfigResponse, rules: &[MetadataRule], mode: i32) -> wire::PluginConfigUpdateRequest {
        wire::PluginConfigUpdateRequest {
            name: TARGET.into(), revision: data.revision.clone(), schema_revision: data.schema_revision.clone(),
            edits: vec![wire::PluginConfigValue { key: "rules".into(), present: true,
                value: serde_json::to_string(rules).unwrap(), ..Default::default() }],
            mode,
        }
    }
    #[test]
    fn metadata_editor_preview_save_conflict_and_restart() {
        let (_dir, path) = setup();
        let (original, runtime) = read(&path).unwrap();
        let data = operation(&path, &runtime, Default::default(), true).unwrap();
        assert!(!data.pending);
        assert_eq!(data.fields[0].kind, "metadata_rules");
        let mut rules = runtime.clone();
        rules[0].text = Some("Ma Radio".into());
        rules.push(serde_json::from_str(r#"{"tag":"speech","fields":["title"]}"#).unwrap());
        let preview = operation(&path, &runtime, request(&data, &rules, 0), false).unwrap();
        assert_eq!(preview.changes.len(), 2);
        assert!(!preview.saved && !preview.changes.join("").contains("keep-secret"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(operation(&path, &runtime, request(&data, &rules, 2), false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let saved = operation(&path, &runtime, request(&data, &rules, 1), false).unwrap();
        assert!(saved.saved && saved.pending && !saved.applied);
        let text = std::fs::read_to_string(&path).unwrap();
        for preserved in ["# secret comment", "# text comment", "# tag comment", "normalize = true", "keep-password"] {
            assert!(text.contains(preserved), "{preserved}");
        }
        assert_eq!(read(&path).unwrap().1, rules);
        let bytes = std::fs::read(&path).unwrap();
        assert!(operation(&path, &runtime, request(&data, &runtime, 1), false).unwrap_err().starts_with("conflict:"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(operation(&path, &runtime, Default::default(), true).unwrap().pending);
        assert!(!operation(&path, &rules, Default::default(), true).unwrap().pending);
        let reset = operation(&path, &rules, Default::default(), true).unwrap();
        let mut req = request(&reset, &[], 1);
        req.edits[0].present = false;
        operation(&path, &rules, req, false).unwrap();
        assert!(read(&path).unwrap().1.is_empty());
    }
    #[test]
    fn metadata_editor_rejects_invalid_values_and_unknown_edits() {
        let (_dir, path) = setup();
        let (original, rules) = read(&path).unwrap();
        let data = operation(&path, &rules, Default::default(), true).unwrap();
        let mut bad = rules.clone();
        for text in ["", "bad\ntext"] {
            bad[0].text = Some(text.into());
            assert!(operation(&path, &rules, request(&data, &bad, 1), false).is_err());
        }
        let mut req = request(&data, &rules, 1);
        req.edits[0].key = "api_token".into();
        assert!(operation(&path, &rules, req, false).is_err());
        let mut req = request(&data, &rules, 1);
        req.edits.push(req.edits[0].clone());
        assert!(operation(&path, &rules, req, false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}
