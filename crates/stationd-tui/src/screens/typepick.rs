//! Affecter une valeur de source (`Type`…) aux médias visés, depuis Médias
//! (`t`) : la liste des valeurs déjà employées (les plus fréquentes en
//! tête), « autre valeur… » pour en saisir une nouvelle, « retirer ». C'est
//! l'action la plus fréquente de l'écran : un seul média s'écrit sans autre
//! question ; un lot passe par une confirmation qui dit combien de fichiers.

use rat_theme4::WidgetStyle;
use rat_theme4::theme::SalsaTheme;
use rat_widget::event::{HandleEvent, Regular};
use rat_widget::text::HasScreenCursor;
use rat_widget::text_input::{TextInput, TextInputState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_widgets::clear::Clear;
use ratatui_widgets::paragraph::Paragraph;
use stationd_proto::library::TagOrigin;

use crate::dialog::{centered, frame};
use crate::rpc::Read;
use crate::style::Styles;
use crate::{fit, tr};

pub enum TypeOutcome {
    Pending,
    Cancel,
    /// La valeur choisie (`None` = retirer).
    Chosen(Option<String>),
}

/// La source (`Type`) et ses valeurs avec leur effectif.
type Source = (String, Vec<(String, u32)>);

pub struct TypePick {
    pub request: u64,
    /// La source (`Type`), et ses valeurs par fréquence ; `None` = en lecture.
    data: Option<Read<Source>>,
    targets: usize,
    sel: usize,
    input: Option<TextInputState>,
}

impl TypePick {
    pub fn new(request: u64, targets: usize) -> Self {
        Self { request, data: None, targets, sel: 0, input: None }
    }

    /// La première source déclarée (après le genre du fichier), ses valeurs
    /// les plus fréquentes en tête. `Err` s'il n'y en a aucune.
    pub fn set(&mut self, origins: Read<Vec<TagOrigin>>) {
        self.data = Some(origins.and_then(|os| {
            let o = os.into_iter().find(|o| !o.origin.is_empty()).ok_or_else(|| tr!("media-type-no-source"))?;
            let mut v: Vec<(String, u32)> = o.values.into_iter().map(|v| (v.value, v.count)).collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
            Ok((o.origin, v))
        }));
    }

    /// La source visée, une fois lue.
    pub fn source(&self) -> Option<&str> {
        match &self.data {
            Some(Ok((s, _))) => Some(s.as_str()),
            _ => None,
        }
    }

    fn len(&self) -> usize {
        match &self.data {
            // Les valeurs, « autre valeur… », « retirer ».
            Some(Ok((_, v))) => v.len() + 2,
            _ => 0,
        }
    }

    pub fn typing(&self) -> bool {
        self.input.is_some()
    }

    pub fn handle(&mut self, e: &Event) -> TypeOutcome {
        let key = match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => Some(k.code),
            _ => None,
        };
        if let Some(input) = self.input.as_mut() {
            match key {
                Some(KeyCode::Esc) => self.input = None,
                Some(KeyCode::Enter) => {
                    let v = input.text().trim().to_string();
                    if !v.is_empty() {
                        return TypeOutcome::Chosen(Some(v));
                    }
                }
                _ => {
                    input.handle(e, Regular);
                }
            }
            return TypeOutcome::Pending;
        }
        let n = self.len();
        match key {
            Some(KeyCode::Esc) => return TypeOutcome::Cancel,
            Some(KeyCode::Up) => self.sel = self.sel.saturating_sub(1),
            Some(KeyCode::Down) => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            Some(KeyCode::Enter) => {
                let Some(Ok((_, values))) = &self.data else { return TypeOutcome::Pending };
                if self.sel < values.len() {
                    return TypeOutcome::Chosen(Some(values[self.sel].0.clone()));
                }
                if self.sel == values.len() {
                    let i = TextInputState::new();
                    i.focus.set(true);
                    self.input = Some(i);
                } else {
                    return TypeOutcome::Chosen(None);
                }
            }
            _ => {}
        }
        TypeOutcome::Pending
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &SalsaTheme) -> Option<(u16, u16)> {
        let s = Styles(theme);
        let title = match self.source() {
            Some(src) => tr!("media-type-title", source = src.to_string(), n = self.targets),
            None => tr!("media-type-title-loading"),
        };
        let mut lines: Vec<Line> = Vec::new();
        match &self.data {
            None => lines.push(Line::styled(tr!("media-loading"), s.muted())),
            Some(Err(e)) => lines.push(Line::styled(e.clone(), s.warn())),
            Some(Ok((_, values))) => {
                let row = |i: usize, text: String, st| {
                    let st = if i == self.sel { s.tab_active() } else { st };
                    Line::styled(format!(" {} ", fit::ellipsize(&text, 50)), st)
                };
                for (i, (v, n)) in values.iter().enumerate() {
                    lines.push(row(i, format!("{v}  ({n})"), s.base()));
                }
                lines.push(row(values.len(), tr!("media-type-other"), s.accent()));
                lines.push(row(values.len() + 1, tr!("media-type-remove"), s.muted()));
            }
        }
        let w = 60.min(area.width.saturating_sub(4));
        // Un message (erreur, pas de source) peut tenir sur plusieurs lignes.
        let body = match &self.data {
            Some(Err(e)) => crate::dialog::wrapped_lines(e, w.saturating_sub(2) as usize) as u16,
            _ => lines.len() as u16,
        };
        let h = (body + if self.input.is_some() { 4 } else { 2 }).min(area.height.saturating_sub(2));
        let box_a = centered(area, w, h);
        Clear.render(box_a, buf);
        let block = frame(&title, &s, self.targets > 1);
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let [list_a, input_a] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(if self.input.is_some() { 2 } else { 0 }),
        ])
        .areas(inner);
        let rows = list_a.height as usize;
        let start = self.sel.saturating_sub(rows.saturating_sub(1));
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>())
            .wrap(ratatui_widgets::paragraph::Wrap { trim: false })
            .render(list_a, buf);
        if let Some(input) = self.input.as_mut() {
            let [label_a, field_a] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(input_a);
            Paragraph::new(Span::styled(tr!("media-type-new"), s.label())).render(label_a, buf);
            let style: rat_widget::text::TextStyle = theme.style(WidgetStyle::TEXT);
            TextInput::new().styles(style).render(field_a, buf, input);
            return input.screen_cursor();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stationd_proto::library::TagValueCount;

    fn origin(name: &str, values: &[(&str, u32)]) -> TagOrigin {
        TagOrigin {
            origin: name.into(),
            values: values.iter().map(|(v, n)| TagValueCount { value: v.to_string(), count: *n, spellings: vec![] }).collect(),
            without: 0,
        }
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(ratatui_crossterm::crossterm::event::KeyEvent::new(code, ratatui_crossterm::crossterm::event::KeyModifiers::NONE))
    }

    #[test]
    fn the_first_source_is_offered_most_used_first() {
        crate::i18n::init(Some("fr"));
        let mut p = TypePick::new(1, 3);
        p.set(Ok(vec![origin("", &[("rock", 9)]), origin("Type", &[("talk", 1), ("music", 5)])]));
        assert_eq!(p.source(), Some("Type"));
        // music (5) first: Enter picks it.
        assert!(matches!(p.handle(&key(KeyCode::Enter)), TypeOutcome::Chosen(Some(v)) if v == "music"));
        // Past the values: « autre valeur… » asks for one, « retirer » is None.
        let mut p = TypePick::new(1, 1);
        p.set(Ok(vec![origin("Type", &[("talk", 1)])]));
        p.handle(&key(KeyCode::Down));
        p.handle(&key(KeyCode::Enter));
        assert!(p.typing());
        for c in "news".chars() {
            p.handle(&key(KeyCode::Char(c)));
        }
        assert!(matches!(p.handle(&key(KeyCode::Enter)), TypeOutcome::Chosen(Some(v)) if v == "news"));
        let mut p = TypePick::new(1, 1);
        p.set(Ok(vec![origin("Type", &[("talk", 1)])]));
        p.handle(&key(KeyCode::Down));
        p.handle(&key(KeyCode::Down));
        assert!(matches!(p.handle(&key(KeyCode::Enter)), TypeOutcome::Chosen(None)));
        // No source: said, nothing to choose.
        let mut p = TypePick::new(1, 1);
        p.set(Ok(vec![origin("", &[("rock", 9)])]));
        assert_eq!(p.source(), None);
        assert!(matches!(p.handle(&key(KeyCode::Enter)), TypeOutcome::Pending));
    }
}
