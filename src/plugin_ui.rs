//! Optional declarative, read-only plugin table tabs.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTab {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub sql: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub dashboard_sql: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub default_value: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub shared: bool,
}
impl Filter {
    pub fn validate_value(&self, value: &str) -> Result<(), String> {
        let valid = value.len() <= 256 && !value.chars().any(char::is_control) && match self.kind.as_str() {
            "choice" => self.options.iter().any(|o| o == value),
            "text" => true,
            "datetime" => value.is_empty() || (value.len() == 19 && value.as_bytes()[10] == b' ' && value.parse::<jiff::civil::DateTime>().is_ok()),
            _ => false,
        };
        if valid { Ok(()) } else { Err(format!("invalid filter {}: {}", self.key, self.label)) }
    }
}
/// Only declared values become bound SQL parameters; SQL never comes from the client.
pub fn bind_filters(filters: &[Filter], values: &std::collections::HashMap<String, String>)
    -> Result<crate::plugin_db::Params, String>
{
    if values.keys().any(|key| !filters.iter().any(|f| &f.key == key)) {
        return Err("unknown table filter".into());
    }
    let mut params = serde_json::Map::new();
    for filter in filters {
        let value = values.get(&filter.key).unwrap_or(&filter.default_value);
        filter.validate_value(value)?;
        params.insert(filter.key.clone(), serde_json::Value::String(value.clone()));
    }
    if params.get("period").and_then(|v| v.as_str()) == Some("Personnalisée")
        && ["from", "to"].iter().any(|key| params.get(*key).and_then(|v| v.as_str()).is_none_or(str::is_empty)) {
        return Err("custom period requires start and end (UTC YYYY-MM-DD HH:MM:SS)".into());
    }
    // Standard optional date bounds: empty means preset, otherwise a half-open interval.
    if let (Some(from), Some(to)) = (params.get("from"), params.get("to")) {
        let (from, to) = (from.as_str().unwrap(), to.as_str().unwrap());
        let custom = params.get("period").and_then(|v| v.as_str());
        if (custom.is_none() || custom == Some("Personnalisée")) && !from.is_empty() && !to.is_empty() && from >= to {
            return Err("start must precede end (UTC)".into());
        }
    }
    Ok(crate::plugin_db::Params::Named(params))
}
pub fn validate(tabs: &[UiTab], has_db: bool) -> Result<(), String> {
    if tabs.len() > 4 {
        return Err("ui_tabs: at most four tabs per plugin".into());
    }
    if tabs.iter().any(|t| t.kind.is_empty() || t.kind == "table") && !has_db {
        return Err("ui_tabs: capability db required".into());
    }
    let mut ids = std::collections::HashSet::new();
    for tab in tabs {
        if tab.id.is_empty()
            || tab.id.len() > 64
            || !tab
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("ui_tabs: invalid tab id".into());
        }
        if !ids.insert(&tab.id) {
            return Err(format!("ui_tabs: duplicate id {}", tab.id));
        }
        for (label, text, max) in [
            ("title", &tab.title, 64),
            ("description", &tab.description, 512),
        ] {
            if text.chars().count() > max
                || text.chars().any(char::is_control)
                || (label == "title" && text.trim().is_empty())
            {
                return Err(format!("ui_tabs: invalid {label}"));
            }
        }
        if tab.filters.len() > 12 { return Err("at most 12 table filters".into()); }
        let mut keys = std::collections::HashSet::new();
        for f in &tab.filters {
            if f.key.is_empty() || f.key.len() > 64
                || !f.key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || !keys.insert(&f.key) || f.label.trim().is_empty() || f.label.chars().count() > 64
                || f.label.chars().any(char::is_control)
                || f.options.len() > 32 || (f.kind != "choice" && !f.options.is_empty())
                || f.options.iter().any(|o| o.len() > 256 || o.chars().any(char::is_control)) {
                return Err("invalid table filter descriptor".into());
            }
            f.validate_value(&f.default_value)?;
        }
        if tab.kind == "plugin_config" {
            if !tab.filters.is_empty() { return Err("configuration tab cannot contain filters".into()); }
            if !tab.sql.is_empty() || !tab.dashboard_sql.is_empty() {
                return Err("configuration tab cannot contain SQL".into());
            }
            continue;
        }
        if !tab.kind.is_empty() && tab.kind != "table" {
            return Err("unknown tab kind".into());
        }
        if (!tab.dashboard_sql.is_empty() && tab.dashboard_sql.trim().is_empty()) || tab.dashboard_sql.len() > 16384 {
            return Err("ui_tabs: invalid dashboard SQL".into());
        }
        if tab.sql.trim().is_empty() || tab.sql.len() > 16384 {
            return Err("ui_tabs: empty or oversized SQL".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tab() -> UiTab {
        UiTab {
            id: "plays".into(),
            title: "Diffusions".into(),
            description: "".into(),
            sql: "SELECT 1".into(),
            kind: String::new(),
            filters: Vec::new(),
            dashboard_sql: String::new(),
        }
    }
    #[test]
    fn validates_identity_capability_and_display_bounds() {
        assert!(validate(&[], false).is_ok());
        assert!(validate(&[tab()], true).is_ok());
        assert!(validate(&[tab()], false).is_err());
        assert!(validate(&[tab(), tab()], true).is_err());
        assert!(validate(&vec![tab(); 5], true).is_err());
        for (id, title) in [("../x", "valid"), ("x", ""), ("x", "bad\u{1b}")] {
            let mut t = tab();
            t.id = id.into();
            t.title = title.into();
            assert!(validate(&[t], true).is_err());
        }
    }

    #[test]
    fn configuration_tab_needs_no_database_and_unknown_kinds_are_refused() {
        let mut t = tab();
        t.kind = "plugin_config".into();
        t.sql.clear();
        assert!(validate(&[t.clone()], false).is_ok());
        t.dashboard_sql = "SELECT 1".into();
        assert!(validate(&[t.clone()], true).is_err());
        t.dashboard_sql.clear();
        t.sql = "SELECT 1".into();
        assert!(validate(&[t.clone()], true).is_err());
        t.sql.clear();
        t.kind = "unknown".into();
        assert!(validate(&[t], true).is_err());
    }
    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<Vec<UiTab>>(
            r#"[{"id":"x","title":"X","sql":"SELECT 1","action":"stop"}]"#
        )
        .is_err());
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    #[test]
    fn filter_validation_rejects_unknown_choices_dates_and_keys_but_binds_text() {
        let filters: Vec<Filter> = serde_json::from_value(serde_json::json!([
            {"key":"period", "label":"Période", "kind":"choice", "default_value":"24 h", "options":["24 h","Personnalisée"]},
            {"key":"from", "label":"Début", "kind":"datetime", "default_value":""},
            {"key":"to", "label":"Fin", "kind":"datetime", "default_value":""},
            {"key":"mount", "label":"Flux", "kind":"text", "default_value":""}
        ])).unwrap();
        let mut values = std::collections::HashMap::new();
        assert!(bind_filters(&filters, &values).is_ok());
        values.insert("unexpected".into(), "x".into());
        assert!(bind_filters(&filters, &values).is_err()); values.clear();
        values.insert("period".into(), "bad".into());
        assert!(bind_filters(&filters, &values).is_err());
        values.insert("period".into(), "Personnalisée".into());
        assert!(bind_filters(&filters, &values).is_err());
        values.insert("from".into(), "2024-02-30 00:00:00".into());
        values.insert("to".into(), "2024-03-01 00:00:00".into());
        assert!(bind_filters(&filters, &values).is_err());
        values.insert("from".into(), "2024-02-29T00:00:00".into());
        assert!(bind_filters(&filters, &values).is_err());
        values.insert("from".into(), "2024-03-01 00:00:00".into());
        assert!(bind_filters(&filters, &values).is_err());
        values.insert("period".into(), "24 h".into());
        assert!(bind_filters(&filters, &values).is_ok()); // Inactive custom dates do not invalidate presets.
        values.insert("period".into(), "Personnalisée".into());
        values.insert("from".into(), "2024-02-29 00:00:00".into());
        values.insert("mount".into(), "' OR 1=1 --".into());
        let params = bind_filters(&filters, &values).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db = crate::plugin_db::PluginDb::open(dir.path(), "filters", Default::default()).unwrap();
        db.migrate(&["CREATE TABLE mounts (mount TEXT); INSERT INTO mounts VALUES ('/r');".into()]).unwrap();
        let sql = "SELECT mount FROM mounts WHERE mount = :mount AND :period != '' AND :from < :to";
        let rows = crate::plugin_db::query_file_filtered(&dir.path().join("filters.db"), sql, &params, 1000, 1000).unwrap();
        assert!(rows.rows.is_empty()); // Text is a literal parameter, never SQL.
    }
}
