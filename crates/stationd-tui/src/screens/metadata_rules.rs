//! Structured metadata rules inside the host-owned configuration editor.
use crate::{style::Styles, tr};
use ratatui_core::{buffer::Buffer, layout::{Constraint, Layout, Rect}, text::Line, widgets::Widget};
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use ratatui_crossterm::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

pub const TARGET: &str = "@liquidsoap-metadata";

#[derive(Clone, Debug)]
struct Rule {
    tag: String,
    origin: u8,
    source: String,
    fields: Vec<String>,
    fixed: bool,
    text: String,
}
impl Default for Rule {
    fn default() -> Self {
        Self { tag: String::new(), origin: 0, source: "Type".into(),
            fields: vec!["title".into(), "artist".into(), "album".into()], fixed: false, text: String::new() }
    }
}
impl Rule {
    fn from_value(v: &Value) -> Result<Self, String> {
        let origin = v.get("origin").and_then(Value::as_str);
        let text = v.get("text").and_then(Value::as_str);
        Ok(Self {
            tag: v.get("tag").and_then(Value::as_str).ok_or_else(|| tr!("metadata-invalid"))?.into(),
            origin: match origin { None => 0, Some("") => 1, Some(_) => 2 },
            source: origin.filter(|s| !s.is_empty()).unwrap_or("Type").into(),
            fields: match v.get("fields") {
                None => Self::default().fields,
                Some(v) => v.as_array().ok_or_else(|| tr!("metadata-invalid"))?.iter()
                    .map(|v| v.as_str().map(String::from).ok_or_else(|| tr!("metadata-invalid")))
                    .collect::<Result<_, _>>()?,
            },
            fixed: text.is_some(), text: text.unwrap_or_default().into(),
        })
    }
    fn value(&self) -> Value {
        json!({"tag": self.tag,
            "origin": match self.origin { 0 => None, 1 => Some(""), _ => Some(self.source.as_str()) },
            "fields": self.fields,
            "text": self.fixed.then_some(self.text.as_str())})
    }
    fn origin_label(&self) -> String {
        match self.origin {
            0 => tr!("metadata-origin-any"),
            1 => tr!("metadata-origin-native"),
            _ => self.source.clone(),
        }
    }
    fn shown_fields(&self) -> String {
        if self.fields.is_empty() { return tr!("metadata-hidden"); }
        self.fields.iter().map(|f| match f.as_str() {
            "title" => tr!("media-field-title"),
            "artist" => tr!("media-field-artist"),
            "album" => tr!("media-field-album"),
            _ => f.clone(),
        }).collect::<Vec<_>>().join(", ")
    }
    fn toggle(&mut self, field: &str) {
        if self.fields.iter().any(|f| f == field) { self.fields.retain(|f| f != field); }
        else { self.fields.push(field.into()); }
    }
}

pub enum Outcome { Pending, Cancel, Submit(String) }

pub struct Editor {
    rules: Vec<Rule>,
    selected: usize,
    detail: bool,
    row: usize,
    input: Option<String>,
    message: String,
}
impl Editor {
    pub fn new(text: &str) -> Result<Self, String> {
        let values: Vec<Value> = serde_json::from_str(text).map_err(|_| tr!("metadata-invalid"))?;
        if values.len() > 128 { return Err(tr!("metadata-limit")); }
        let rules = values.iter().map(Rule::from_value).collect::<Result<_, _>>()?;
        Ok(Self { rules, selected: 0, detail: false, row: 0, input: None, message: String::new() })
    }
    fn submit(&mut self) -> Outcome {
        if self.rules.iter().any(|r| r.tag.trim().is_empty()
            || (r.origin == 2 && r.source.trim().is_empty())
            || (r.fixed && r.text.trim().is_empty())) {
            self.message = tr!("metadata-required");
            return Outcome::Pending;
        }
        let value = serde_json::to_string(&self.rules.iter().map(Rule::value).collect::<Vec<_>>()).unwrap();
        if value.len() > 65536 { self.message = tr!("metadata-limit"); return Outcome::Pending; }
        Outcome::Submit(value)
    }
    pub fn handle(&mut self, key: &KeyEvent) -> Outcome {
        if let Some(input) = self.input.as_mut() {
            match key.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Enter => {
                    let value = self.input.take().unwrap();
                    let rule = &mut self.rules[self.selected];
                    match self.row { 0 => rule.tag = value, 2 => rule.source = value, 7 => rule.text = value, _ => {} }
                }
                KeyCode::Backspace => { input.pop(); }
                KeyCode::Char(c) if !c.is_control()
                    && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && input.len() + c.len_utf8() <= 4096 => input.push(c),
                _ => {}
            }
            return Outcome::Pending;
        }
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.submit();
        }
        self.message.clear();
        if self.detail {
            match key.code {
                KeyCode::Esc => self.detail = false,
                KeyCode::Up => self.row = self.row.saturating_sub(1),
                KeyCode::Down => self.row = (self.row + 1).min(7),
                KeyCode::Enter => {
                    let rule = &mut self.rules[self.selected];
                    match self.row {
                        0 => self.input = Some(rule.tag.clone()),
                        1 => rule.origin = (rule.origin + 1) % 3,
                        2 if rule.origin == 2 => self.input = Some(rule.source.clone()),
                        3 => rule.fixed = !rule.fixed,
                        4 if !rule.fixed => rule.toggle("title"),
                        5 if !rule.fixed => rule.toggle("artist"),
                        6 if !rule.fixed => rule.toggle("album"),
                        7 if rule.fixed => self.input = Some(rule.text.clone()),
                        _ => {}
                    }
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Esc => return Outcome::Cancel,
                KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) && self.selected > 0 => {
                    self.rules.swap(self.selected, self.selected - 1); self.selected -= 1;
                }
                KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) && self.selected + 1 < self.rules.len() => {
                    self.rules.swap(self.selected, self.selected + 1); self.selected += 1;
                }
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => self.selected = (self.selected + 1).min(self.rules.len().saturating_sub(1)),
                KeyCode::Insert | KeyCode::Char('a') => {
                    if self.rules.len() == 128 { self.message = tr!("metadata-limit"); }
                    else { self.rules.push(Rule::default()); self.selected = self.rules.len() - 1; self.row = 0; self.detail = true; }
                }
                KeyCode::Delete if !self.rules.is_empty() => {
                    self.rules.remove(self.selected);
                    self.selected = self.selected.min(self.rules.len().saturating_sub(1));
                }
                KeyCode::Enter if !self.rules.is_empty() => { self.detail = true; self.row = 0; }
                _ => {}
            }
        }
        Outcome::Pending
    }
    pub fn render(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3), Constraint::Fill(1), Constraint::Length(5),
        ]).areas(area);
        Paragraph::new(format!("{}\n{}", tr!("metadata-target"), tr!("metadata-first-match")))
            .style(s.label()).wrap(Wrap { trim: false }).render(header, buf);
        let lines: Vec<Line> = if self.detail {
            let r = &self.rules[self.selected];
            let yes = |field: &str| if r.fields.iter().any(|f| f == field) { tr!("metadata-show") } else { tr!("metadata-hide") };
            let rows = [
                (tr!("metadata-tag"), r.tag.clone(), true),
                (tr!("metadata-origin"), match r.origin { 0 => tr!("metadata-origin-any"), 1 => tr!("metadata-origin-native"), _ => tr!("metadata-origin-custom") }, true),
                (tr!("metadata-source"), r.source.clone(), r.origin == 2),
                (tr!("metadata-display"), if r.fixed { tr!("metadata-fixed") } else { tr!("metadata-fields") }, true),
                (tr!("media-field-title"), yes("title"), !r.fixed),
                (tr!("media-field-artist"), yes("artist"), !r.fixed),
                (tr!("media-field-album"), yes("album"), !r.fixed),
                (tr!("metadata-text"), r.text.clone(), r.fixed),
            ];
            let start = self.row.saturating_sub(body.height.saturating_sub(1) as usize);
            rows.into_iter().enumerate().skip(start).map(|(i, (label, value, enabled))| {
                let value = if i == self.row {
                    self.input.as_ref().map(|v| format!("{v}▏")).unwrap_or(value)
                } else { value };
                let line = Line::from(format!("{} {label} : {value}", if i == self.row { "›" } else { " " }));
                if enabled { line.style(s.label()) } else { line.style(s.muted()) }
            }).collect()
        } else if self.rules.is_empty() {
            vec![Line::from(tr!("metadata-empty"))]
        } else {
            let start = self.selected.saturating_sub(body.height.saturating_sub(1) as usize);
            self.rules.iter().enumerate().skip(start).map(|(i, r)| Line::from(format!(
                "{} {}. {} / {} : {}", if i == self.selected { "›" } else { " " }, i + 1,
                r.origin_label(), r.tag,
                if r.fixed { format!("{} : {}", tr!("metadata-fixed"), r.text) } else { r.shown_fields() }
            ))).collect()
        };
        Paragraph::new(lines).style(s.label()).render(body, buf);
        Paragraph::new(format!("{}\n{}\n{}", self.message,
            if self.detail { tr!("metadata-detail-keys") } else { tr!("metadata-list-keys") },
            tr!("metadata-restart")))
            .style(s.muted()).wrap(Wrap { trim: false }).render(footer, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }
    #[test]
    fn metadata_form_adds_edits_masks_reorders_and_deletes_rules() {
        let mut e = Editor::new("[]").unwrap();
        e.handle(&key(KeyCode::Char('a')));
        assert!(e.detail && e.rules.len() == 1);
        e.handle(&key(KeyCode::Enter));
        for c in "jingle".chars() { e.handle(&key(KeyCode::Char(c))); }
        e.handle(&key(KeyCode::Enter));
        e.row = 3;
        e.handle(&key(KeyCode::Enter));
        assert!(e.rules[0].fixed);
        assert!(matches!(e.submit(), Outcome::Pending));
        e.row = 7;
        e.handle(&key(KeyCode::Enter));
        for c in "Ma Radio".chars() { e.handle(&key(KeyCode::Char(c))); }
        e.handle(&key(KeyCode::Enter));
        e.handle(&key(KeyCode::Esc));
        e.handle(&key(KeyCode::Char('a')));
        e.rules[1].tag = "speech".into();
        e.row = 5; e.handle(&key(KeyCode::Enter));
        e.row = 6; e.handle(&key(KeyCode::Enter));
        assert_eq!(e.rules[1].fields, vec!["title"]);
        e.handle(&key(KeyCode::Esc));
        e.handle(&KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(e.rules[0].tag, "speech");
        let Outcome::Submit(text) = e.submit() else { panic!("valid form"); };
        let parsed: Vec<Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed[1]["text"], "Ma Radio");
        assert_eq!(parsed[0]["fields"], json!(["title"]));
        assert!(Editor::new(&text).unwrap().rules[1].fixed);
        e.handle(&key(KeyCode::Delete));
        assert_eq!(e.rules.len(), 1);
        assert_eq!(e.rules[0].tag, "jingle");
    }
    #[test]
    fn metadata_form_keeps_input_keys_and_preserves_all_origin_modes() {
        let mut e = Editor::new(r#"[{"tag":"song","origin":"","fields":[]},{"tag":"jingle","origin":"Type","text":"Radio"}]"#).unwrap();
        assert_eq!(e.rules[0].origin, 1);
        assert_eq!(e.rules[1].origin, 2);
        e.handle(&key(KeyCode::Enter));
        e.handle(&key(KeyCode::Enter));
        // 'a' is text while typing, not a request to add another rule.
        e.handle(&key(KeyCode::Char('a')));
        e.handle(&key(KeyCode::Esc));
        assert_eq!(e.rules.len(), 2);
        assert_eq!(e.rules[0].tag, "song");
        e.row = 1;
        e.handle(&key(KeyCode::Enter));
        assert_eq!(e.rules[0].origin, 2);
        e.handle(&key(KeyCode::Enter));
        assert_eq!(e.rules[0].origin, 0);
        let Outcome::Submit(text) = e.submit() else { panic!("valid form"); };
        let values: Vec<Value> = serde_json::from_str(&text).unwrap();
        assert!(values[0]["origin"].is_null());
        assert_eq!(values[0]["fields"], json!([]));
        assert_eq!(values[1]["origin"], "Type");
    }
}
