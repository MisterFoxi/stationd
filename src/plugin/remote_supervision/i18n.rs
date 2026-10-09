//! Browser language shared by HTML and the console reservation.
use axum::http::{header, HeaderMap};
use serde_json::Value;
use std::sync::OnceLock;

pub(super) fn language(headers: &HeaderMap) -> &'static str {
    let mut selected = "fr";
    let mut best = 0.0_f32;
    for value in headers.get_all(header::ACCEPT_LANGUAGE) {
        for item in value.to_str().unwrap_or_default().split(',') {
            let mut parts = item.trim().split(';');
            let base = parts
                .next()
                .unwrap_or_default()
                .split('-')
                .next()
                .unwrap_or_default();
            let quality = parts.next().map_or(Some(1.0), |part| {
                part.trim()
                    .strip_prefix("q=")
                    .and_then(|q| q.parse::<f32>().ok())
            });
            let Some(quality) = quality.filter(|q| q.is_finite() && *q > 0.0 && *q <= 1.0) else {
                continue;
            };
            if quality > best {
                if let Some(code) = ["fr", "en", "de"]
                    .into_iter()
                    .find(|code| base.eq_ignore_ascii_case(code))
                {
                    selected = code;
                    best = quality;
                }
            }
        }
    }
    selected
}
fn catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("translations.json")).expect("web translations")
    })
}
pub(super) fn text<'a>(source: &'a str, language: &str) -> &'a str {
    catalog()[source][language].as_str().unwrap_or(source)
}
// Templates contain only external scripts. Translate before injecting dynamic data.
pub(super) fn page(template: &str, headers: &HeaderMap) -> String {
    let language = language(headers);
    let mut result = String::new();
    for (index, segment) in template.split('<').enumerate() {
        if index > 0 {
            result.push('<');
        }
        if let Some((tag, content)) = segment.split_once('>') {
            let mut tag = tag.to_owned();
            for attribute in ["aria-label", "lang"] {
                let prefix = format!("{attribute}=\"");
                if let Some(start) = tag.find(&prefix) {
                    let start = start + prefix.len();
                    if let Some(end) = tag[start..].find('"') {
                        let source = &tag[start..start + end];
                        let translated = if attribute == "lang" {
                            language
                        } else {
                            text(source, language)
                        };
                        let escaped = translated.replace('&', "&amp;").replace('"', "&quot;");
                        tag.replace_range(start..start + end, &escaped);
                    }
                }
            }
            result.push_str(&tag);
            result.push('>');
            let source = content.trim();
            if source.is_empty() {
                result.push_str(content);
            } else {
                let translated = text(source, language)
                    .replace('&', "&amp;")
                    .replace('<', "&lt;");
                result.push_str(&content.replacen(source, &translated, 1));
            }
        } else {
            result.push_str(segment);
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webmin_language_negotiates_preferences_and_fallback() {
        for (value, expected) in [
            ("en-US,en;q=0.9,fr;q=0.8", "en"),
            ("es,DE-de;q=0.7,en;q=0.5", "de"),
            ("en;q=0.3,fr;q=0.9", "fr"),
            ("en;q=0,de;q=bad", "fr"),
            ("es,*;q=0.5", "fr"),
            ("de;q=NaN,en;q=1.5", "fr"),
            ("de,en", "de"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT_LANGUAGE, value.parse().unwrap());
            assert_eq!(language(&headers), expected, "{value}");
        }
        assert_eq!(language(&HeaderMap::new()), "fr");
    }
    #[test]
    fn webmin_language_translates_templates_and_accessible_labels() {
        for language in ["fr", "en", "de"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT_LANGUAGE, language.parse().unwrap());
            let html = page(include_str!("station.html"), &headers);
            assert!(html.contains(&format!("lang=\"{language}\"")));
            assert!(html.contains(text("À l’antenne", language)));
            assert!(html.contains(&format!(
                "aria-label=\"{}\"",
                text("Progression du média", language)
            )));
            assert!(html.contains("{{STATION_ID}}"));
        }
        for (_, entry) in catalog().as_object().unwrap() {
            for language in ["en", "de"] {
                assert!(!entry[language].as_str().unwrap().is_empty());
            }
        }
    }
}
