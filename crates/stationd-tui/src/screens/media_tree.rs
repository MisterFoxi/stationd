//! Indexed directory tree; file actions stay in the media screen.
use crate::{fit, style::Styles, tr};
use ratatui_core::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    text::Line,
    widgets::Widget,
};
use ratatui_crossterm::crossterm::event::KeyCode;
use ratatui_widgets::{
    paragraph::Paragraph,
    table::{Cell, Row, Table},
};
use stationd_proto::library::{Media, MediaFolder};
use std::collections::BTreeSet;

pub struct Tree {
    pub folders: Vec<MediaFolder>,
    pub path: String,
    pub focused: bool,
    pub loading: bool,
    pub error: Option<String>,
    expanded: BTreeSet<String>,
}
impl Default for Tree {
    fn default() -> Self {
        Self {
            folders: vec![],
            path: String::new(),
            focused: true,
            loading: false,
            error: None,
            expanded: BTreeSet::from([String::new()]),
        }
    }
}
impl Tree {
    pub fn set(&mut self, folders: Vec<MediaFolder>) -> bool {
        self.folders = folders;
        self.folders
            .sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
        self.expanded
            .retain(|p| self.folders.iter().any(|f| &f.path == p));
        let before = self.path.clone();
        if !self.folders.iter().any(|f| f.path == self.path) {
            self.path = self
                .folders
                .iter()
                .find(|f| f.path.to_lowercase() == before.to_lowercase())
                .map(|f| f.path.clone())
                .unwrap_or_default();
        }
        let mut parent = self.path.as_str();
        while !parent.is_empty() {
            parent = parent.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            self.expanded.insert(parent.into());
        }
        self.expanded.insert(String::new());
        self.error = None;
        self.loading = false;
        before != self.path
    }
    fn visible(&self) -> Vec<&MediaFolder> {
        self.folders
            .iter()
            .filter(|f| {
                let mut path = f.path.as_str();
                while !path.is_empty() {
                    let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                    if !self.expanded.contains(parent) {
                        return false;
                    }
                    path = parent;
                }
                true
            })
            .collect()
    }
    /// Returns true when selection (hence the direct-file query) changed.
    pub fn navigate(&mut self, key: KeyCode) -> bool {
        let before = self.path.clone();
        let visible = self.visible();
        let index = visible
            .iter()
            .position(|f| f.path == self.path)
            .unwrap_or(0);
        let count = visible.len();
        let next = match key {
            KeyCode::Up => Some(index.saturating_sub(1)),
            KeyCode::Down => Some((index + 1).min(count.saturating_sub(1))),
            KeyCode::PageUp => Some(index.saturating_sub(10)),
            KeyCode::PageDown => Some((index + 10).min(count.saturating_sub(1))),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(count.saturating_sub(1)),
            _ => None,
        };
        if let Some(index) = next {
            if let Some(f) = visible.get(index) {
                self.path = f.path.clone();
            }
        } else {
            drop(visible);
            match key {
                KeyCode::Right => {
                    self.expanded.insert(self.path.clone());
                }
                KeyCode::Left => {
                    if !self.expanded.remove(&self.path) {
                        self.path = self
                            .path
                            .rsplit_once('/')
                            .map(|(p, _)| p.to_string())
                            .unwrap_or_default();
                    }
                }
                KeyCode::Char(' ') => {
                    if !self.expanded.remove(&self.path) {
                        self.expanded.insert(self.path.clone());
                    }
                }
                _ => {}
            }
        }
        before != self.path
    }
    pub fn render(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let visible = self.visible();
        let selected = visible
            .iter()
            .position(|f| f.path == self.path)
            .unwrap_or(0);
        let start = selected.saturating_sub(area.height.saturating_sub(1) as usize);
        let mut lines = Vec::new();
        if let Some(error) = &self.error {
            lines.push(Line::from(clean(error)).style(s.error()));
        } else if self.loading {
            lines.push(Line::from(tr!("media-loading")).style(s.muted()));
        }
        for f in visible.iter().skip(start) {
            let depth = if f.path.is_empty() {
                0
            } else {
                f.path.split('/').count()
            };
            let name = if f.path.is_empty() {
                tr!("media-tree-root")
            } else {
                clean(f.path.rsplit('/').next().unwrap_or(&f.path))
            };
            let text = format!(
                "{}{} {} ({})",
                "  ".repeat(depth.min(8)),
                if self.expanded.contains(&f.path) {
                    "▾"
                } else {
                    "▸"
                },
                name,
                f.count
            );
            let text = fit::ellipsize(&text, area.width as usize);
            lines.push(Line::from(text).style(if f.path == self.path {
                if self.focused {
                    s.tab_active()
                } else {
                    s.accent()
                }
            } else {
                s.base()
            }));
        }
        Paragraph::new(lines).render(area, buf);
    }
}
pub(super) fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn render_files(
    area: Rect,
    buf: &mut Buffer,
    s: &Styles,
    rows: &[Media],
    selected: usize,
    marks: &[String],
) {
    let h = area.height.saturating_sub(1) as usize;
    let start = selected
        .saturating_sub(h.saturating_sub(1))
        .min(rows.len().saturating_sub(h));
    let header = Row::new(vec![
        "".into(),
        tr!("media-field-path"),
        tr!("media-tree-tags"),
        tr!("media-field-genre"),
        tr!("media-field-duration"),
    ])
    .style(s.label());
    let list: Vec<Row> = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(h)
        .map(|(i, m)| {
            let filename = clean(m.rel_path.rsplit('/').next().unwrap_or(&m.rel_path));
            let tags = [m.artist.as_str(), m.title.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .map(clean)
                .collect::<Vec<_>>()
                .join(" — ");
            let duration = super::medias::mmss(m.duration_ms);
            let row = Row::new(vec![
                Cell::from(if marks.contains(&m.rel_path) {
                    "●"
                } else {
                    " "
                }),
                Cell::from(filename),
                Cell::from(tags),
                Cell::from(clean(&m.genres.join(", "))),
                Cell::from(duration),
            ]);
            if i == selected {
                row.style(s.tab_active())
            } else if !m.available {
                row.style(s.muted())
            } else {
                row
            }
        })
        .collect();
    Table::new(
        list,
        [
            Constraint::Length(1),
            Constraint::Fill(3),
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(5),
        ],
    )
    .header(header)
    .column_spacing(1)
    .render(area, buf);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descendants_follow_their_parent_and_collapse_returns_to_parent() {
        let mut t = Tree::default();
        t.set(
            ["", "A", "A-Other", "A/Sub", "Été avec espace"]
                .into_iter()
                .map(|p| MediaFolder {
                    path: p.into(),
                    count: 1,
                })
                .collect(),
        );
        assert_eq!(
            t.visible()
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["", "A", "A-Other", "Été avec espace"]
        );
        assert!(t.navigate(KeyCode::Down));
        assert_eq!(t.path, "A");
        t.navigate(KeyCode::Right);
        assert!(t.navigate(KeyCode::Down));
        assert_eq!(t.path, "A/Sub");
        assert!(t.navigate(KeyCode::Left));
        assert_eq!(t.path, "A");
        t.navigate(KeyCode::Left);
        assert!(!t.visible().iter().any(|f| f.path == "A/Sub"));
    }
    #[test]
    fn refresh_of_removed_directory_recovers_to_root() {
        let mut t = Tree::default();
        t.path = "gone".into();
        assert!(t.set(vec![MediaFolder {
            path: "".into(),
            count: 0
        }]));
        assert!(t.path.is_empty());
    }
}
