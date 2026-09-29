//! Sélecteur de playlist (membre d'un groupe, playlist statique qui reçoit
//! des médias, playlist `queue` qui les met en file) : la liste de
//! `PlaylistService.List`, filtrée en tapant. Aucun choix fait à la place
//! de l'utilisateur ; le filtre sur le mode est une présentation (stationd
//! refusera de toute façon une playlist du mauvais mode).

use rat_theme4::WidgetStyle;
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
use stationd_proto::playlist::PlaylistSummary;

use crate::dialog::{centered, frame};
use crate::fit;
use crate::style::Styles;
use crate::tr;

pub enum Picked {
    Unchanged,
    Changed,
    Cancel,
    /// Le ref de la playlist choisie.
    Chosen(String),
    /// « Nouvelle playlist… » (proposé seulement si `allow_new`).
    New,
}

pub struct PlaylistPicker {
    pub title: String,
    /// `None` = en chargement ; `Err` = la liste n'a pas pu être lue.
    items: Option<Result<Vec<PlaylistSummary>, String>>,
    /// Modes proposés (vide = tous).
    modes: Vec<&'static str>,
    allow_new: bool,
    filter: TextInputState,
    selected: usize,
}

/// Mode affiché (traduit).
pub fn mode_label(mode: &str) -> String {
    match mode {
        "static" => tr!("mode-static"),
        "dynamic" => tr!("mode-dynamic"),
        "remote" => tr!("mode-remote"),
        "queue" => tr!("mode-queue"),
        "group" => tr!("mode-group"),
        other => other.to_string(),
    }
}

impl PlaylistPicker {
    pub fn new(title: String, modes: Vec<&'static str>, allow_new: bool) -> Self {
        let filter = TextInputState::new();
        filter.focus.set(true);
        Self { title, items: None, modes, allow_new, filter, selected: 0 }
    }

    pub fn set_items(&mut self, items: Result<Vec<PlaylistSummary>, String>) {
        self.items = Some(items);
        self.selected = 0;
    }

    /// Les entrées visibles : bon mode, ref ou nom contenant le filtre
    /// (insensible à la casse), puis « nouvelle » en dernier.
    fn visible(&self) -> Vec<&PlaylistSummary> {
        let needle = self.filter.text().trim().to_lowercase();
        match &self.items {
            Some(Ok(items)) => items
                .iter()
                .filter(|p| !p.rel_path.is_empty())
                .filter(|p| self.modes.is_empty() || self.modes.contains(&p.mode.as_str()))
                .filter(|p| {
                    needle.is_empty()
                        || p.rel_path.to_lowercase().contains(&needle)
                        || p.name.to_lowercase().contains(&needle)
                })
                .collect(),
            _ => vec![],
        }
    }

    fn count(&self) -> usize {
        self.visible().len() + usize::from(self.allow_new)
    }

    pub fn handle(&mut self, event: &Event) -> Picked {
        if let Event::Key(k) = event
            && k.kind == KeyEventKind::Press
        {
            let n = self.count();
            match k.code {
                KeyCode::Esc => return Picked::Cancel,
                KeyCode::Up => {
                    self.selected = self.selected.saturating_sub(1);
                    return Picked::Changed;
                }
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(n.saturating_sub(1));
                    return Picked::Changed;
                }
                KeyCode::Enter => {
                    let vis = self.visible();
                    return match vis.get(self.selected) {
                        Some(p) => Picked::Chosen(p.rel_path.clone()),
                        None if self.allow_new && self.selected == vis.len() => Picked::New,
                        None => Picked::Unchanged,
                    };
                }
                _ => {}
            }
        }
        let before = self.filter.text().to_string();
        self.filter.handle(event, Regular);
        if self.filter.text() != before {
            self.selected = 0;
            return Picked::Changed;
        }
        Picked::Unchanged
    }

    /// Rendu centré dans `area` ; rend la position du curseur de saisie.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &rat_theme4::theme::SalsaTheme) -> Option<(u16, u16)> {
        let s = Styles(theme);
        let w = 70.min(area.width.saturating_sub(4)).max(40);
        let h = 20.min(area.height.saturating_sub(2)).max(8);
        let box_a = centered(area, w, h);
        Clear.render(box_a, buf);
        let block = frame(&self.title, &s, false)
            .title_bottom(Span::styled(format!(" {} ", tr!("picker-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let [filter_a, _, list_a] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
        let [mark_a, input_a] = Layout::horizontal([Constraint::Length(2), Constraint::Fill(1)]).areas(filter_a);
        Span::styled("/ ", s.accent()).render(mark_a, buf);
        let style: rat_widget::text::TextStyle = theme.style(WidgetStyle::TEXT);
        TextInput::new().styles(style).render(input_a, buf, &mut self.filter);

        let lines: Vec<Line> = match &self.items {
            None => vec![Line::styled(tr!("picker-loading"), s.muted())],
            Some(Err(e)) => vec![Line::styled(e.clone(), s.error())],
            Some(Ok(_)) => {
                let vis = self.visible();
                let h = list_a.height as usize;
                let total = vis.len() + usize::from(self.allow_new);
                let start = self.selected.saturating_sub(h.saturating_sub(1)).min(total.saturating_sub(h));
                let mut out = Vec::new();
                for i in start..(start + h).min(total) {
                    let st = if i == self.selected { s.tab_active() } else { s.base() };
                    let line = match vis.get(i) {
                        Some(p) => {
                            let text = format!("{}  {}", p.rel_path, p.name);
                            Line::from(vec![
                                Span::styled(format!("{:<8} ", mode_label(&p.mode)), s.label()),
                                Span::styled(fit::ellipsize(&text, (list_a.width as usize).saturating_sub(9)), st),
                            ])
                        }
                        None => Line::from(Span::styled(format!("＋ {}", tr!("picker-new")), st)),
                    };
                    out.push(line);
                }
                if total == 0 {
                    out.push(Line::styled(tr!("picker-none"), s.muted()));
                }
                out
            }
        };
        Paragraph::new(lines).render(list_a, buf);
        self.filter.screen_cursor()
    }
}
