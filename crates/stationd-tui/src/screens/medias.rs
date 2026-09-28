//! Médias (`5`) — la bibliothèque, par `LibraryService.SearchMedia`
//! (dossier §5.5). Recherche, filtres, tri stable, pages chargées à la
//! demande (curseur serveur). Lot 4a : consultation + override d'un média ;
//! Type / tags (plugin `tags`), fiche et actions en lot viendront ensuite.
//!
//! La barre de recherche accepte des mots (titre, artiste, album, chemin)
//! et deux préfixes : `genre:x` (répétable, au moins un) et `dossier:x`.

use anyhow::Error;
use rat_salsa::{Control, SalsaContext};
use rat_theme4::WidgetStyle;
use rat_widget::event::{HandleEvent, Regular};
use rat_widget::text::HasScreenCursor;
use rat_widget::text_input::{TextInput, TextInputState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::paragraph::Paragraph;
use ratatui_widgets::table::{Cell, Row, Table};
use stationd_proto::library::search_media_request::Field;
use stationd_proto::library::{Media, SearchMediaRequest, SearchMediaResponse};

use super::ops;
use crate::app::{AppEvent, Global};
use crate::fit;
use crate::screen::{KeyHelp, Screen};
use crate::style::Styles;
use crate::{k, tr};

/// Taille d'une page demandée au serveur.
const PAGE: u32 = 100;

/// Tris proposés, dans l'ordre de `s`.
const SORTS: [Field; 6] = [Field::Path, Field::Artist, Field::Title, Field::Album, Field::Year, Field::Duration];

/// Filtre « métadonnée manquante », dans l'ordre de `m`.
const MISSING: [Option<Field>; 5] = [None, Some(Field::Title), Some(Field::Artist), Some(Field::Genre), Some(Field::Year)];

/// Délai après la dernière frappe avant la recherche en direct.
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

/// Ce que la recherche en direct envoie : le texte, sans le préfixe en cours
/// de frappe (`genre:él` au bout, sans espace après) — un genre partiel ne
/// correspondrait à rien et les résultats clignoteraient à chaque lettre.
pub fn live_query(text: &str) -> String {
    let trimmed = text.trim();
    if text.ends_with(char::is_whitespace) {
        return trimmed.to_string();
    }
    match trimmed.rsplit_once(char::is_whitespace) {
        Some((head, last)) if is_prefix_token(last) => head.trim().to_string(),
        None if is_prefix_token(trimmed) => String::new(),
        _ => trimmed.to_string(),
    }
}

fn is_prefix_token(tok: &str) -> bool {
    tok.split_once(':')
        .is_some_and(|(k, _)| matches!(k.to_lowercase().as_str(), "genre" | "dossier" | "dir" | "folder"))
}

/// La barre de recherche découpée : mots, genres, dossier.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub words: String,
    pub genres: Vec<String>,
    pub folder: String,
}

/// `daft genre:électro dossier:Musique/Rock punk` → mots `daft punk`, genre
/// `électro`, dossier `Musique/Rock`. Préfixes insensibles à la casse ;
/// `dir:` / `folder:` acceptés pour `dossier:`. Un préfixe vide est ignoré.
pub fn parse_query(text: &str) -> Parsed {
    let mut p = Parsed::default();
    let mut words = Vec::new();
    for tok in text.split_whitespace() {
        let (key, val) = match tok.split_once(':') {
            Some((k, v)) => (k.to_lowercase(), v),
            None => (String::new(), tok),
        };
        match key.as_str() {
            "genre" if !val.is_empty() => p.genres.push(val.to_string()),
            "dossier" | "dir" | "folder" if !val.is_empty() => p.folder = val.to_string(),
            "genre" | "dossier" | "dir" | "folder" => {}
            _ => words.push(tok),
        }
    }
    p.words = words.join(" ");
    p
}

pub struct Medias {
    input: TextInputState,
    editing: bool,
    /// Recherche validée (texte de la barre au dernier Entrée) : Échap y
    /// revient.
    applied: String,
    /// Recherche réellement envoyée (en direct pendant la frappe).
    searched: String,
    /// N° de la dernière frappe : seule la dernière déclenche la recherche.
    typing: u64,
    sort: usize,
    descending: bool,
    missing: usize,
    include_unavailable: bool,
    rows: Vec<Media>,
    total: u64,
    next: String,
    loading: bool,
    error: Option<String>,
    selected: usize,
    /// N° de la dernière requête : une réponse plus ancienne est ignorée.
    request: u64,
    loaded_once: bool,
}

impl Default for Medias {
    fn default() -> Self {
        Self {
            input: TextInputState::new(),
            editing: false,
            applied: String::new(),
            searched: String::new(),
            typing: 0,
            sort: 0,
            descending: false,
            missing: 0,
            include_unavailable: false,
            rows: Vec::new(),
            total: 0,
            next: String::new(),
            loading: false,
            error: None,
            selected: 0,
            request: 0,
            loaded_once: false,
        }
    }
}

fn mmss(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn field_label(f: Field) -> String {
    match f {
        Field::Unspecified | Field::Path => tr!("media-field-path"),
        Field::Title => tr!("media-field-title"),
        Field::Artist => tr!("media-field-artist"),
        Field::Album => tr!("media-field-album"),
        Field::Year => tr!("media-field-year"),
        Field::Duration => tr!("media-field-duration"),
        Field::Genre => tr!("media-field-genre"),
    }
}

impl Medias {
    fn request(&self, cursor: String) -> SearchMediaRequest {
        let p = parse_query(&self.searched);
        SearchMediaRequest {
            query: p.words,
            genres: p.genres,
            folder: p.folder,
            include_unavailable: self.include_unavailable,
            missing: MISSING[self.missing].map(|f| vec![f as i32]).unwrap_or_default(),
            sort: SORTS[self.sort] as i32,
            descending: self.descending,
            limit: PAGE,
            cursor,
        }
    }

    /// Lance une recherche : depuis le début (`append = false`) ou la page
    /// suivante.
    fn load(&mut self, ctx: &mut Global, append: bool) {
        let cursor = if append { self.next.clone() } else { String::new() };
        if append && cursor.is_empty() {
            return;
        }
        self.request += 1;
        self.loading = true;
        self.loaded_once = true;
        let (id, req, channel) = (self.request, self.request(cursor), ctx.channel.clone());
        ctx.spawn_async(async move {
            let r = crate::rpc::search_media(channel, req).await;
            Ok(Control::Event(AppEvent::Media(id, r, append)))
        });
    }

    fn apply(&mut self, r: &Result<SearchMediaResponse, String>, append: bool) {
        self.loading = false;
        match r {
            Ok(page) => {
                if !append {
                    self.rows.clear();
                    self.selected = 0;
                }
                self.rows.extend(page.media.iter().cloned());
                self.total = page.total;
                self.next = page.next_cursor.clone();
                self.error = None;
            }
            Err(e) => self.error = Some(e.clone()),
        }
    }

    fn move_to(&mut self, idx: usize, ctx: &mut Global) {
        self.selected = idx.min(self.rows.len().saturating_sub(1));
        // Près de la fin de ce qui est chargé : la page suivante.
        if !self.loading && !self.next.is_empty() && self.selected + 20 >= self.rows.len() {
            self.load(ctx, true);
        }
    }

    fn summary(&self) -> String {
        let mut parts = vec![tr!(
            "media-sorted-by",
            field = field_label(SORTS[self.sort]),
            dir = if self.descending { "↓" } else { "↑" }
        )];
        if let Some(f) = MISSING[self.missing] {
            parts.push(tr!("media-missing", field = field_label(f)));
        }
        if self.include_unavailable {
            parts.push(tr!("media-with-unavailable"));
        }
        parts.join(" · ")
    }

    fn render_table(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        if self.rows.is_empty() {
            let msg = match (&self.error, self.loading, self.loaded_once) {
                (Some(e), ..) => Span::styled(e.clone(), s.error()),
                (None, true, _) | (None, false, false) => Span::styled(tr!("media-loading"), s.muted()),
                (None, false, true) => Span::styled(tr!("media-none"), s.muted()),
            };
            Paragraph::new(Line::from(vec![Span::raw(" "), msg])).render(area, buf);
            return;
        }
        let header = Row::new(vec![
            Cell::from(tr!("media-field-artist")),
            Cell::from(tr!("media-field-title")),
            Cell::from(tr!("media-field-album")),
            Cell::from(tr!("media-field-year")),
            Cell::from(tr!("media-field-duration")),
            Cell::from(tr!("media-field-genre")),
        ])
        .style(s.label());
        let h = area.height.saturating_sub(1) as usize;
        let start = self.selected.saturating_sub(h.saturating_sub(1)).min(self.rows.len().saturating_sub(h));
        let rows: Vec<Row> = self.rows[start..(start + h).min(self.rows.len())]
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let file = m.rel_path.rsplit('/').next().unwrap_or(&m.rel_path).to_string();
                let title = if m.title.is_empty() {
                    Span::styled(file, s.warn())
                } else {
                    Span::raw(m.title.clone())
                };
                let artist = if m.artist.is_empty() { Span::styled("—", s.muted()) } else { Span::raw(m.artist.clone()) };
                let year = if m.year == 0 { String::new() } else { m.year.to_string() };
                let mut row = Row::new(vec![
                    Cell::from(artist),
                    Cell::from(title),
                    Cell::from(Span::styled(m.album.clone(), s.muted())),
                    Cell::from(Span::styled(year, s.label())),
                    Cell::from(Span::styled(mmss(m.duration_ms), s.label())),
                    Cell::from(Span::styled(m.genres.join(", "), s.muted())),
                ]);
                if !m.available {
                    row = row.style(s.muted());
                }
                if start + i == self.selected { row.style(s.tab_active()) } else { row }
            })
            .collect();
        // Colonnes courtes à la largeur de leur libellé traduit, jamais coupé.
        let w = |key: String, min: usize| Span::raw(key).width().max(min) as u16;
        let (year_w, dur_w) = (w(tr!("media-field-year"), 4), w(tr!("media-field-duration"), 5));
        let widths = if area.width >= 100 {
            vec![
                Constraint::Fill(2),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Length(year_w),
                Constraint::Length(dur_w),
                Constraint::Fill(1),
            ]
        } else {
            vec![
                Constraint::Fill(1),
                Constraint::Fill(2),
                Constraint::Length(0),
                Constraint::Length(year_w),
                Constraint::Length(dur_w),
                Constraint::Length(0),
            ]
        };
        Widget::render(Table::new(rows, widths).header(header).column_spacing(1), area, buf);
    }
}

impl Screen for Medias {
    fn title(&self) -> String {
        tr!("screen-media")
    }

    fn captures_text(&self) -> bool {
        self.editing
    }

    fn enter(&mut self, ctx: &mut Global) -> Result<(), Error> {
        // Premier passage, ou une réponse partie pendant qu'un autre écran
        // était actif (elle ne nous est pas parvenue) : on recharge.
        if !self.loaded_once || self.loading {
            self.load(ctx, false);
        }
        Ok(())
    }

    fn help(&self) -> &'static [KeyHelp] {
        if self.editing {
            &[(k!("key-enter"), k!("help-media-apply")), (k!("key-esc"), k!("help-media-cancel"))]
        } else {
            &[
                (k!("key-slash"), k!("help-media-search")),
                (k!("key-s"), k!("help-media-sort")),
                (k!("key-d"), k!("help-media-desc")),
                (k!("key-m"), k!("help-media-missing")),
                (k!("key-a"), k!("help-media-unavailable")),
                (k!("key-o"), k!("help-override")),
                (k!("key-r"), k!("help-media-reload")),
            ]
        }
    }

    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        if let AppEvent::Media(id, r, append) = event {
            if *id == self.request {
                self.apply(r, *append);
                return Ok(Control::Changed);
            }
            return Ok(Control::Continue);
        }
        if let AppEvent::MediaTyped(id) = event {
            if *id == self.typing && self.editing {
                let live = live_query(self.input.text());
                if live != self.searched {
                    self.searched = live;
                    self.load(ctx, false);
                }
            }
            return Ok(Control::Changed);
        }
        let AppEvent::Event(e) = event else { return Ok(Control::Continue) };
        if self.editing {
            if let Event::Key(k) = e
                && k.kind == KeyEventKind::Press
            {
                match k.code {
                    KeyCode::Enter => {
                        self.editing = false;
                        self.input.focus.set(false);
                        self.typing += 1; // une recherche en attente n'a plus lieu d'être
                        self.applied = self.input.text().trim().to_string();
                        if self.searched != self.applied {
                            self.searched = self.applied.clone();
                            self.load(ctx, false);
                        }
                        return Ok(Control::Changed);
                    }
                    KeyCode::Esc => {
                        self.editing = false;
                        self.input.focus.set(false);
                        self.typing += 1;
                        let applied = self.applied.clone();
                        self.input.set_text(applied.clone());
                        // Retour aux résultats d'avant la saisie.
                        if self.searched != applied {
                            self.searched = applied;
                            self.load(ctx, false);
                        }
                        return Ok(Control::Changed);
                    }
                    _ => {}
                }
            }
            let before = self.input.text().to_string();
            self.input.handle(e, Regular);
            if self.input.text() != before {
                // Recherche en direct, `DEBOUNCE` après la dernière frappe.
                self.typing += 1;
                let id = self.typing;
                ctx.spawn_async(async move {
                    tokio::time::sleep(DEBOUNCE).await;
                    Ok(Control::Event(AppEvent::MediaTyped(id)))
                });
            }
            return Ok(Control::Changed);
        }
        let Event::Key(k) = e else { return Ok(Control::Continue) };
        if k.kind != KeyEventKind::Press {
            return Ok(Control::Continue);
        }
        let page = 15;
        match k.code {
            KeyCode::Char('/') => {
                self.editing = true;
                self.input.focus.set(true);
            }
            KeyCode::Char('s') => {
                self.sort = (self.sort + 1) % SORTS.len();
                self.load(ctx, false);
            }
            KeyCode::Char('d') => {
                self.descending = !self.descending;
                self.load(ctx, false);
            }
            KeyCode::Char('m') => {
                self.missing = (self.missing + 1) % MISSING.len();
                self.load(ctx, false);
            }
            KeyCode::Char('a') => {
                self.include_unavailable = !self.include_unavailable;
                self.load(ctx, false);
            }
            KeyCode::Char('r') => self.load(ctx, false),
            KeyCode::Char('o') => {
                let Some(m) = self.rows.get(self.selected) else { return Ok(Control::Continue) };
                let path = m.rel_path.clone();
                ctx.open(ops::push_override_with(Some(&path)));
            }
            KeyCode::Up => self.move_to(self.selected.saturating_sub(1), ctx),
            KeyCode::Down => self.move_to(self.selected + 1, ctx),
            KeyCode::PageUp => self.move_to(self.selected.saturating_sub(page), ctx),
            KeyCode::PageDown => self.move_to(self.selected + page, ctx),
            KeyCode::Home => self.move_to(0, ctx),
            KeyCode::End => self.move_to(usize::MAX, ctx),
            _ => return Ok(Control::Continue),
        }
        Ok(Control::Changed)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        Block::new().style(s.base()).render(area, buf);
        let [search_a, info_a, table_a] =
            Layout::vertical([Constraint::Length(3), Constraint::Length(1), Constraint::Fill(1)]).areas(area);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(if self.editing { s.accent() } else { s.border() })
            .title(Span::styled(format!(" {} ", tr!("media-search-title")), s.title()));
        let inner = block.inner(search_a);
        block.render(search_a, buf);
        if self.editing || !self.input.text().is_empty() {
            let style: rat_widget::text::TextStyle = ctx.theme.style(WidgetStyle::TEXT);
            TextInput::new().styles(style).render(inner, buf, &mut self.input);
            if self.editing {
                ctx.set_screen_cursor(self.input.screen_cursor());
            }
        } else {
            Paragraph::new(Span::styled(tr!("media-search-hint"), s.muted())).render(inner, buf);
        }

        let count = if self.loading && self.rows.is_empty() {
            String::new()
        } else {
            tr!("media-count", shown = self.rows.len(), total = self.total)
        };
        let right = Span::styled(format!("{count} "), s.label());
        let [l, r] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(right.width() as u16)]).areas(info_a);
        let mut left = self.summary();
        if let (Some(e), false) = (&self.error, self.rows.is_empty()) {
            left = format!("{left} · {e}");
        }
        Paragraph::new(Span::styled(format!(" {}", fit::ellipsize(&left, l.width.saturating_sub(1) as usize)), s.muted()))
            .render(l, buf);
        Paragraph::new(right).render(r, buf);

        self.render_table(table_a, buf, &s);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_bar_splits_words_genres_and_folder() {
        let p = parse_query("daft Genre:électro dossier:Musique/Rock punk genre:house dir:");
        assert_eq!(p.words, "daft punk");
        assert_eq!(p.genres, ["électro", "house"]);
        assert_eq!(p.folder, "Musique/Rock");
        assert_eq!(parse_query("  ").words, "");
        // Un « : » ailleurs que dans un préfixe reste un mot.
        assert_eq!(parse_query("12:30").words, "12:30");
    }

    #[test]
    fn live_search_leaves_out_the_prefix_being_typed() {
        assert_eq!(live_query("daft genre:él"), "daft");
        assert_eq!(live_query("genre:él"), "");
        assert_eq!(live_query("daft genre:électro "), "daft genre:électro");
        assert_eq!(live_query("genre:électro daf"), "genre:électro daf");
        assert_eq!(live_query("12:30"), "12:30");
    }
}
