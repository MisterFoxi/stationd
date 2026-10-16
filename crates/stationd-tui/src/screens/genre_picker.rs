//! Shared, transactional genre selection for playlists and statistics filters.
use crate::{
    dialog::{centered, frame},
    style::Styles,
    tr,
};
use ratatui_core::{buffer::Buffer, layout::Rect, text::Line, widgets::Widget};
use ratatui_crossterm::crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui_widgets::{clear::Clear, paragraph::Paragraph};
use stationd_proto::library::GenreCount;
use std::collections::BTreeMap;

pub enum Pick {
    Pending,
    Cancel,
    Selected(String),
}
pub struct GenrePicker {
    pub owner: u64,
    choices: BTreeMap<String, (String, u32)>,
    selected: BTreeMap<String, String>,
    query: String,
    cursor: usize,
    rows: Rect,
    first: usize,
    error: Option<String>,
    loading: bool,
}
impl GenrePicker {
    pub fn new(value: &str) -> Self {
        let selected = value
            .split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| (s.to_lowercase(), s.to_string()))
            .collect();
        let mut p = Self {
            owner: super::next_owner(),
            choices: BTreeMap::new(),
            selected,
            query: String::new(),
            cursor: 0,
            rows: Rect::default(),
            first: 0,
            error: None,
            loading: true,
        };
        p.merge_selected();
        p
    }
    fn merge_selected(&mut self) {
        for (key, name) in &self.selected {
            self.choices.entry(key.clone()).or_insert((name.clone(), 0));
        }
    }
    pub fn set(&mut self, result: Result<Vec<GenreCount>, String>) {
        self.loading = false;
        match result {
            Ok(genres) => {
                self.error = None;
                self.choices.clear();
                for g in genres {
                    self.choices
                        .insert(g.genre.trim().to_lowercase(), (g.genre, g.count));
                }
                self.merge_selected();
            }
            Err(e) => self.error = Some(e),
        }
        self.cursor = self.cursor.min(self.visible().len().saturating_sub(1));
    }
    fn visible(&self) -> Vec<String> {
        let query = self.query.to_lowercase();
        self.choices
            .iter()
            .filter(|(_, (name, _))| name.to_lowercase().contains(&query))
            .map(|(key, _)| key.clone())
            .collect()
    }
    fn toggle(&mut self, key: &str) {
        if self.selected.remove(key).is_none() {
            if let Some((name, _)) = self.choices.get(key) {
                self.selected.insert(key.to_string(), name.clone());
            }
        }
    }
    pub fn handle(&mut self, event: &Event) -> Pick {
        let visible = self.visible();
        if let Event::Mouse(m) = event {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self.rows.contains((m.column, m.row).into()) =>
                {
                    let i = self.first + usize::from(m.row - self.rows.y);
                    if let Some(key) = visible.get(i) {
                        self.cursor = i;
                        self.toggle(key);
                    }
                }
                MouseEventKind::ScrollDown => {
                    self.cursor = (self.cursor + 3).min(visible.len().saturating_sub(1))
                }
                MouseEventKind::ScrollUp => self.cursor = self.cursor.saturating_sub(3),
                _ => {}
            }
        }
        let Event::Key(k) = event else {
            return Pick::Pending;
        };
        if k.kind != KeyEventKind::Press {
            return Pick::Pending;
        }
        match k.code {
            KeyCode::Esc => return Pick::Cancel,
            KeyCode::Enter => {
                return Pick::Selected(
                    self.selected
                        .values()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(visible.len().saturating_sub(1)),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = visible.len().saturating_sub(1),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(10),
            KeyCode::PageDown => {
                self.cursor = (self.cursor + 10).min(visible.len().saturating_sub(1))
            }
            KeyCode::Char(' ') => {
                if let Some(key) = visible.get(self.cursor) {
                    self.toggle(key)
                }
            }
            KeyCode::Char('a') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                for key in visible {
                    if let Some((name, _)) = self.choices.get(&key) {
                        self.selected.insert(key, name.clone());
                    }
                }
            }
            KeyCode::Char('d') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                for key in visible {
                    self.selected.remove(&key);
                }
            }
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.clear();
                self.cursor = 0;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !k
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(c);
                self.cursor = 0;
            }
            _ => {}
        }
        Pick::Pending
    }
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let box_a = centered(
            area,
            area.width.saturating_sub(4).min(76),
            area.height.saturating_sub(2).min(24),
        );
        Clear.render(box_a, buf);
        let block = frame(&tr!("genre-picker-title"), s, false);
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        if inner.height < 5 {
            return;
        }
        Paragraph::new(Line::styled(
            tr!("genre-picker-search", query = self.query.clone()),
            s.accent(),
        ))
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
        Paragraph::new(Line::styled(
            tr!("genre-picker-selected", n = self.selected.len()),
            s.muted(),
        ))
        .render(Rect::new(inner.x, inner.y + 1, inner.width, 1), buf);
        self.rows = Rect::new(
            inner.x,
            inner.y + 3,
            inner.width,
            inner.height.saturating_sub(6),
        );
        let visible = self.visible();
        self.first = self
            .cursor
            .saturating_sub(self.rows.height.saturating_sub(1) as usize);
        for (i, key) in visible
            .iter()
            .enumerate()
            .skip(self.first)
            .take(self.rows.height as usize)
        {
            let (name, count) = &self.choices[key];
            let line = format!(
                "{} [{}] {} ({})",
                if i == self.cursor { "▸" } else { " " },
                if self.selected.contains_key(key) {
                    "✓"
                } else {
                    " "
                },
                name,
                count
            );
            Paragraph::new(Line::styled(
                line,
                if i == self.cursor {
                    s.tab_active()
                } else {
                    s.base()
                },
            ))
            .render(
                Rect::new(
                    self.rows.x,
                    self.rows.y + (i - self.first) as u16,
                    self.rows.width,
                    1,
                ),
                buf,
            );
        }
        if visible.is_empty() {
            Paragraph::new(tr!("genre-picker-empty")).render(self.rows, buf);
        }
        let status = if self.loading {
            tr!("genre-picker-loading")
        } else {
            self.error.clone().unwrap_or_default()
        };
        Paragraph::new(Line::styled(status, s.warn())).render(
            Rect::new(inner.x, inner.y + inner.height - 3, inner.width, 1),
            buf,
        );
        Paragraph::new(tr!("genre-picker-help")).render(
            Rect::new(inner.x, inner.y + inner.height - 2, inner.width, 2),
            buf,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_crossterm::crossterm::event::KeyEvent;
    fn key(p: &mut GenrePicker, code: KeyCode) -> Pick {
        p.handle(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }
    #[test]
    fn selection_survives_search_and_unknown_genres_are_preserved() {
        let mut p = GenrePicker::new("Old, rock, ROCK");
        p.set(Ok(vec![
            GenreCount {
                genre: "Rock".into(),
                count: 2,
                ..Default::default()
            },
            GenreCount {
                genre: "TOPH".into(),
                count: 1,
                ..Default::default()
            },
        ]));
        assert_eq!(p.selected.len(), 2);
        key(&mut p, KeyCode::Char('t'));
        assert_eq!(p.visible(), ["toph"]);
        key(&mut p, KeyCode::Char(' '));
        key(&mut p, KeyCode::Backspace);
        assert_eq!(p.selected.len(), 3);
        let Pick::Selected(value) = key(&mut p, KeyCode::Enter) else {
            panic!()
        };
        assert_eq!(value, "Old, ROCK, TOPH");
    }
    #[test]
    fn cancel_keeps_original_and_errors_do_not_drop_existing_choices() {
        let mut p = GenrePicker::new("Électro");
        p.set(Err("offline".into()));
        key(&mut p, KeyCode::Char('é'));
        assert_eq!(p.visible(), ["électro"]);
        key(&mut p, KeyCode::Char(' '));
        assert!(matches!(key(&mut p, KeyCode::Esc), Pick::Cancel));
    }

    #[test]
    fn popup_keeps_checkboxes_and_search_visible_at_terminal_sizes() {
        let theme = rat_theme4::create_salsa_theme("Imperial");
        for (w, h) in [(80, 24), (50, 15), (35, 12)] {
            let mut p = GenrePicker::new("Rock");
            p.set(Ok(vec![
                GenreCount {
                    genre: "Rock".into(),
                    count: 3,
                    ..Default::default()
                },
                GenreCount {
                    genre: "TOPH".into(),
                    count: 1,
                    ..Default::default()
                },
            ]));
            let area = Rect::new(0, 0, w, h);
            let mut buffer = Buffer::empty(area);
            p.render(area, &mut buffer, &Styles(&theme));
            let text: String = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|pos| buffer[pos].symbol())
                .collect();
            assert!(text.contains("Rock"));
            assert!(text.contains("✓"));
            assert!(text.contains("TOPH"));
        }
    }
}
