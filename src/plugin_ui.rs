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
        if tab.kind == "plugin_config" {
            if !tab.sql.is_empty() {
                return Err("configuration tab cannot contain SQL".into());
            }
            continue;
        }
        if !tab.kind.is_empty() && tab.kind != "table" {
            return Err("unknown tab kind".into());
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
