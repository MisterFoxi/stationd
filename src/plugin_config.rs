//! Host-owned configuration editing. Plugins declare fields; clients never send file paths.
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};
use toml_edit::{DocumentMut, Item};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub kind: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub minimum: Option<i64>,
    #[serde(default)]
    pub maximum: Option<i64>,
    #[serde(default)]
    pub secret: bool,
}
pub fn stop_fields() -> Vec<Field> {
    vec![
        Field {
            key: "min_zero_samples".into(),
            label: "Échantillons consécutifs".into(),
            kind: "integer".into(),
            optional: false,
            default: Some("1".into()),
            minimum: Some(1),
            maximum: Some(u32::MAX as i64),
            secret: false,
        },
        Field {
            key: "max_connection_age".into(),
            label: "Âge maximal des connexions (ex. 12h)".into(),
            kind: "duration".into(),
            optional: true,
            default: None,
            minimum: None,
            maximum: None,
            secret: false,
        },
    ]
}
pub fn validate_schema(fields: &[Field]) -> Result<(), String> {
    if fields.len() > 32 {
        return Err("config_schema: at most 32 fields".into());
    }
    let mut keys = HashSet::new();
    for f in fields {
        if f.key.is_empty()
            || f.key.len() > 64
            || !f
                .key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !keys.insert(&f.key)
            || f.label.is_empty()
            || f.label.len() > 256
            || f.label.chars().any(char::is_control)
            || !matches!(f.kind.as_str(), "integer" | "boolean" | "text" | "duration")
            || f.minimum.zip(f.maximum).is_some_and(|(a, b)| a > b)
            || (f.secret && f.default.is_some())
        {
            return Err("config_schema: invalid field".into());
        }
        if let Some(d) = &f.default {
            parse(f, d)?;
        }
    }
    Ok(())
}
fn parse(f: &Field, text: &str) -> Result<toml::Value, String> {
    let bad = || format!("{}: invalid {}", f.key, f.kind);
    if text.len() > 4096 || text.chars().any(char::is_control) {
        return Err(bad());
    }
    Ok(match f.kind.as_str() {
        "integer" => {
            let n = text.parse::<i64>().map_err(|_| bad())?;
            if f.minimum.is_some_and(|v| n < v) || f.maximum.is_some_and(|v| n > v) {
                return Err(format!("{}: out of bounds", f.key));
            }
            toml::Value::Integer(n)
        }
        "boolean" => toml::Value::Boolean(text.parse().map_err(|_| bad())?),
        "duration" => {
            if !text.is_ascii() {
                return Err(bad());
            }
            let seconds = crate::playlist::parse_duration_secs(text).map_err(|_| bad())?;
            if seconds == 0 {
                return Err(bad());
            }
            toml::Value::String(text.into())
        }
        "text" => toml::Value::String(text.into()),
        _ => return Err(bad()),
    })
}
pub fn patch(
    fields: &[Field],
    current: &toml::Table,
    edits: &[(String, Option<String>)],
) -> Result<toml::Table, String> {
    let mut result = current.clone();
    let mut seen = HashSet::new();
    for (key, value) in edits {
        if !seen.insert(key) {
            return Err("duplicate edit".into());
        }
        let f = fields
            .iter()
            .find(|f| &f.key == key)
            .ok_or("unknown configuration field")?;
        match value {
            Some(v) => {
                result.insert(key.clone(), parse(f, v)?);
            }
            None if f.optional || f.default.is_some() => {
                result.remove(key);
            }
            None => return Err(format!("{key}: required")),
        }
    }
    for f in fields {
        match result.get(&f.key) {
            None if !f.optional && f.default.is_none() => {
                return Err(format!("{}: required", f.key))
            }
            Some(v) => {
                let s = display(v);
                let parsed = parse(f, &s)?;
                if &parsed != v {
                    return Err(format!("{}: incorrect TOML type", f.key));
                }
            }
            _ => {}
        }
    }
    Ok(result)
}
pub fn display(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}
pub fn revision(bytes: &[u8]) -> String {
    crate::playlist_edit::revision(bytes)
}
pub fn read(path: &Path, name: &str) -> Result<(Vec<u8>, toml::Table), String> {
    let bytes = std::fs::read(path).map_err(|_| "cannot read station configuration")?;
    if bytes.len() > 1024 * 1024 {
        return Err("configuration exceeds 1 MiB".into());
    }
    let config: crate::config::Config =
        toml::from_str(std::str::from_utf8(&bytes).map_err(|_| "configuration is not UTF-8")?)
            .map_err(|_| "invalid station configuration")?;
    let mut matches = config.plugins.into_iter().filter(|p| p.name == name);
    let p = matches
        .next()
        .ok_or("plugin is absent from configuration file")?;
    if matches.next().is_some() {
        return Err("duplicate plugin declaration".into());
    }
    Ok((bytes, p.config))
}
pub fn save(
    path: &Path,
    original: &[u8],
    name: &str,
    edits: &[(String, Option<String>)],
    fields: &[Field],
) -> Result<String, String> {
    use std::io::Write;
    let mut doc = std::str::from_utf8(original)
        .map_err(|_| "invalid UTF-8")?
        .parse::<DocumentMut>()
        .map_err(|_| "invalid TOML")?;
    let plugins = doc
        .get_mut("plugin")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or("expected [[plugin]] declarations")?;
    let plugin = plugins
        .iter_mut()
        .find(|p| p.get("name").and_then(Item::as_str) == Some(name))
        .ok_or("plugin missing")?;
    if !plugin.contains_key("config") {
        plugin.insert("config", Item::Table(toml_edit::Table::new()));
    }
    let table = plugin
        .get_mut("config")
        .unwrap()
        .as_table_mut()
        .ok_or("inline plugin.config is not editable; use [plugin.config]")?;
    for (key, text) in edits {
        match text {
            None => {
                table.remove(key);
            }
            Some(text) => {
                let f = fields
                    .iter()
                    .find(|f| &f.key == key)
                    .ok_or("unknown field")?;
                let value = match parse(f, text)? {
                    toml::Value::Integer(n) => toml_edit::value(n),
                    toml::Value::Boolean(b) => toml_edit::value(b),
                    toml::Value::String(s) => toml_edit::value(s),
                    _ => return Err("unsupported value".into()),
                };
                // Preserve the field's surrounding comments.
                let decor = table
                    .get(key)
                    .and_then(Item::as_value)
                    .map(|v| v.decor().clone());
                let mut value = value;
                if let (Some(d), Some(v)) = (decor, value.as_value_mut()) {
                    *v.decor_mut() = d;
                }
                table.insert(key, value);
            }
        }
    }
    let bytes = doc.to_string().into_bytes();
    let parent = path.parent().ok_or("configuration has no parent")?;
    let tmp = parent.join(format!(".stationd-config-{}.tmp", uuid::Uuid::new_v4()));
    let metadata = std::fs::metadata(path).map_err(|_| "cannot stat configuration")?;
    let permissions = metadata.permissions();
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            opts.mode(permissions.mode());
        }
        let mut file = opts
            .open(&tmp)
            .map_err(|_| "cannot create configuration temporary file")?;
        #[cfg(unix)]
        {
            use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
            if unsafe { libc::fchown(file.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0 {
                return Err("cannot preserve configuration ownership".into());
            }
        }
        file.set_permissions(permissions)
            .map_err(|_| "cannot preserve configuration permissions")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "cannot write configuration")?;
        if std::fs::read(path).map_err(|_| "cannot recheck configuration")? != original {
            return Err("conflict: configuration changed; refresh before saving".into());
        }
        std::fs::rename(&tmp, path).map_err(|_| "cannot replace configuration")?;
        #[cfg(unix)]
        {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(revision(&bytes))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_bounds_types_and_preserves_unknown_values() {
        let f = stop_fields();
        assert!(patch(
            &f,
            &toml::Table::new(),
            &[("max_connection_age".into(), Some("日".into()))]
        )
        .is_err());
        let mut current = toml::Table::new();
        current.insert("future".into(), toml::Value::Boolean(true));
        assert!(patch(
            &f,
            &current,
            &[("min_zero_samples".into(), Some("0".into()))]
        )
        .is_err());
        assert!(patch(
            &f,
            &current,
            &[("max_connection_age".into(), Some("0h".into()))]
        )
        .is_err());
        assert!(patch(&f, &current, &[("unknown".into(), Some("1".into()))]).is_err());
        let p = patch(
            &f,
            &current,
            &[("min_zero_samples".into(), Some("3".into()))],
        )
        .unwrap();
        assert_eq!(p["future"], current["future"]);
        let mut wrong = current;
        wrong.insert("min_zero_samples".into(), toml::Value::String("3".into()));
        assert!(patch(&f, &wrong, &[]).is_err());
    }
    #[test]
    fn atomic_save_preserves_comments_other_plugins_and_detects_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stationd.toml");
        let original = b"# keep\n[[plugin]]\nname = 'stop-when-idle'\n[plugin.config]\nmin_zero_samples = 1 # retained\nunknown = 'secret'\n[[plugin]]\nname = 'logger'\n";
        std::fs::write(&path, original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let edits = vec![("min_zero_samples".into(), Some("4".into()))];
        save(&path, original, "stop-when-idle", &edits, &stop_fields()).unwrap();
        let new = std::fs::read_to_string(&path).unwrap();
        assert!(
            new.contains("# keep")
                && new.contains("# retained")
                && new.contains("unknown = 'secret'")
                && new.contains("name = 'logger'")
        );
        assert!(
            save(&path, original, "stop-when-idle", &edits, &stop_fields())
                .unwrap_err()
                .starts_with("conflict:")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), new);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
