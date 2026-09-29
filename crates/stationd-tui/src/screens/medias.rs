//! Médias (`5`) — la bibliothèque, par `LibraryService.SearchMedia`
//! (dossier §5.5). Recherche, filtres, tri stable, pages chargées à la
//! demande (curseur serveur). Fiche d'un média (`Entrée` : métadonnées,
//! playlists qui peuvent le diffuser, diffusions), sélection multiple
//! (`Espace`) et actions sur la sélection : ajout à une playlist statique
//! (brouillon ouvert dans Playlists), mise en file, override. Type / tags
//! (plugin `tags`) viendront au lot 8.
//!
//! La même recherche sert de sélecteur de médias au brouillon d'une playlist
//! statique (`Medias::new(true)`).
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
use ratatui_widgets::clear::Clear;
use ratatui_widgets::table::{Cell, Row, Table};
use stationd_proto::library::search_media_request::Field;
use stationd_proto::library::{Media, SearchMediaRequest, SearchMediaResponse};

use super::ops;
use super::tagform::{TagForm, TagOutcome};
use super::picker::{PlaylistPicker, Picked as PickedPlaylist, mode_label};
use crate::action::Action;
use crate::app::{AppEvent, Global, Handoff};
use crate::dialog::{centered, frame};
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
        .is_some_and(|(k, _)| matches!(k.to_lowercase().as_str(), "genre" | "dossier" | "dir" | "folder" | "age" | "âge"))
}

/// La barre de recherche découpée : mots, genres, dossier, âges.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub words: String,
    pub genres: Vec<String>,
    pub folder: String,
    /// `âge:<10d` → (`<`, `10d`) : âge de la date de création. Envoyé tel
    /// quel, stationd refuse (et dit pourquoi) un opérateur ou une durée faux.
    pub age: Vec<(String, String)>,
}

/// `daft genre:électro dossier:Musique/Rock âge:<10d punk` → mots `daft
/// punk`, genre `électro`, dossier `Musique/Rock`, créé il y a moins de 10 jours. Préfixes insensibles à la casse ;
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
            "age" | "âge" if !val.is_empty() => {
                let n = val.chars().take_while(|c| matches!(c, '<' | '>' | '=')).count();
                let (op, dur) = val.split_at(n);
                p.age.push((op.to_string(), dur.to_string()));
            }
            "genre" | "dossier" | "dir" | "folder" | "age" | "âge" => {}
            _ => words.push(tok),
        }
    }
    p.words = words.join(" ");
    p
}

/// Ce que le sélecteur de playlist va recevoir (fiche ou lot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// Ajouter à une playlist statique (brouillon ouvert dans Playlists).
    AddToStatic,
    /// Mettre en file d'une playlist `queue`.
    Enqueue,
}

/// Fiche d'un média (`Entrée`).
struct Card {
    media: Media,
    request: u64,
    data: Option<crate::rpc::MediaCard>,
}

/// Réponse d'un sélecteur de médias (mode `picker`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    Cancel,
    Chosen(Vec<String>),
}

pub struct Medias {
    /// Identifie les réponses de CETTE recherche (l'écran Médias et chaque
    /// sélecteur ont la leur).
    owner: u64,
    /// Sélecteur de médias (brouillon de playlist statique) : `Entrée`
    /// choisit au lieu d'ouvrir la fiche.
    picker: bool,
    picked: Option<Picked>,
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
    /// Médias marqués (`Espace`), dans l'ordre où ils l'ont été.
    marks: Vec<String>,
    /// Compteur des requêtes de l'écran (liste, fiche, tags, choix de
    /// playlist) : chacune garde le n° de SA dernière requête.
    request: u64,
    /// N° de la dernière requête de LA LISTE : seule sa réponse est gardée
    /// (une fiche ou un formulaire ouverts entre-temps ne la périment pas).
    list_req: u64,
    loaded_once: bool,
    card: Option<Card>,
    /// Sélecteur de playlist ouvert pour une action en lot.
    chooser: Option<(Purpose, PlaylistPicker, u64)>,
    /// Relecture après une action : la sélection revient sur ce média.
    keep: Option<String>,
    /// Lecture des tags en cours (n° de requête) avant le formulaire.
    tags_req: Option<u64>,
    /// Les médias dont on modifie les tags.
    tags_targets: Vec<String>,
    /// Éditeur des tags ouvert.
    tagform: Option<TagForm>,
}

impl Default for Medias {
    fn default() -> Self {
        Self::new(false)
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
    pub fn new(picker: bool) -> Self {
        Self {
            owner: super::next_owner(),
            picker,
            picked: None,
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
            marks: Vec::new(),
            request: 0,
            list_req: 0,
            loaded_once: false,
            card: None,
            chooser: None,
            keep: None,
            tags_req: None,
            tags_targets: Vec::new(),
            tagform: None,
        }
    }

    /// Sélecteur : la réponse, une fois donnée (`Entrée` ou `Échap`).
    pub fn take_picked(&mut self) -> Option<Picked> {
        self.picked.take()
    }

    /// Lance la première recherche si rien n'est chargé, si une réponse est
    /// partie pendant qu'un autre écran était actif, ou si la dernière a
    /// échoué (stationd injoignable alors).
    pub fn ensure_loaded(&mut self, ctx: &mut Global) {
        if !self.loaded_once || self.loading || self.error.is_some() {
            self.load(ctx, false);
        }
    }

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
            age: p
                .age
                .into_iter()
                .map(|(op, value)| stationd_proto::library::search_media_request::AgeFilter { op, value })
                .collect(),
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
        self.list_req = self.request;
        self.loading = true;
        self.loaded_once = true;
        let (owner, id, req, channel) = (self.owner, self.request, self.request(cursor), ctx.channel.clone());
        ctx.spawn_async(async move {
            let r = crate::rpc::search_media(channel, req).await;
            Ok(Control::Event(AppEvent::Media(owner, id, r, append)))
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
                if let Some(k) = self.keep.take()
                    && let Some(i) = self.rows.iter().position(|m| m.rel_path == k)
                {
                    self.selected = i;
                }
                self.total = page.total;
                self.next = page.next_cursor.clone();
                self.error = None;
                // La fiche ouverte montre la ligne relue (tags écrits…).
                if let Some(c) = self.card.as_mut()
                    && let Some(m) = self.rows.iter().find(|m| m.rel_path == c.media.rel_path)
                {
                    c.media = m.clone();
                }
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

    fn toggle_mark(&mut self) {
        let Some(m) = self.rows.get(self.selected) else { return };
        match self.marks.iter().position(|p| *p == m.rel_path) {
            Some(i) => {
                self.marks.remove(i);
            }
            None => self.marks.push(m.rel_path.clone()),
        }
    }

    /// Les médias visés par une action : les marqués, sinon celui de la
    /// ligne (ou de la fiche ouverte).
    fn targets(&self) -> Vec<String> {
        if let Some(c) = &self.card {
            return vec![c.media.rel_path.clone()];
        }
        if !self.marks.is_empty() {
            return self.marks.clone();
        }
        self.rows.get(self.selected).map(|m| vec![m.rel_path.clone()]).unwrap_or_default()
    }

    fn open_card(&mut self, ctx: &mut Global) {
        let Some(m) = self.rows.get(self.selected).cloned() else { return };
        self.request += 1;
        let (owner, id, channel, path) = (self.owner, self.request, ctx.channel.clone(), m.rel_path.clone());
        self.card = Some(Card { media: m, request: id, data: None });
        ctx.spawn_async(async move {
            let card = crate::rpc::media_card(channel, path).await;
            Ok(Control::Event(AppEvent::MediaCard(owner, id, Box::new(card))))
        });
    }

    /// `e` : modifier les tags du média (ou des marqués). Les tags du fichier
    /// (du premier du lot) et les genres connus sont lus d'abord ; le
    /// formulaire s'ouvre à leur arrivée.
    fn edit_tags(&mut self, ctx: &mut Global) {
        let targets = self.targets();
        let Some(first) = targets.first().cloned() else { return };
        self.request += 1;
        self.tags_req = Some(self.request);
        self.tags_targets = targets;
        let (owner, id, channel) = (self.owner, self.request, ctx.channel.clone());
        ctx.spawn_async(async move {
            let r = crate::rpc::tag_form_data(channel, first).await;
            Ok(Control::Event(AppEvent::MediaTags(owner, id, Box::new(r))))
        });
    }

    /// Après une action (tags écrits…) : relit la page et la fiche, la
    /// sélection reste sur son média.
    fn refresh(&mut self, ctx: &mut Global) {
        if !self.loaded_once {
            return;
        }
        self.keep = self.rows.get(self.selected).map(|m| m.rel_path.clone());
        self.load(ctx, false);
        if let Some(c) = self.card.take() {
            let path = c.media.rel_path.clone();
            self.request += 1;
            let (owner, id, channel) = (self.owner, self.request, ctx.channel.clone());
            let mut media = c.media;
            if let Some(m) = self.rows.iter().find(|m| m.rel_path == path) {
                media = m.clone();
            }
            self.card = Some(Card { media, request: id, data: None });
            ctx.spawn_async(async move {
                let card = crate::rpc::media_card(channel, path).await;
                Ok(Control::Event(AppEvent::MediaCard(owner, id, Box::new(card))))
            });
        }
    }

    fn open_chooser(&mut self, purpose: Purpose, ctx: &mut Global) {
        let n = self.targets().len();
        if n == 0 {
            return;
        }
        let (title, modes, allow_new) = match purpose {
            Purpose::AddToStatic => (tr!("media-choose-static", n = n), vec!["static"], true),
            Purpose::Enqueue => (tr!("media-choose-queue", n = n), vec!["queue"], false),
        };
        self.request += 1;
        let (owner, id, channel) = (self.owner, self.request, ctx.channel.clone());
        self.chooser = Some((purpose, PlaylistPicker::new(title, modes, allow_new), id));
        ctx.spawn_async(async move {
            let r = crate::rpc::list_playlists(channel).await;
            Ok(Control::Event(AppEvent::PlaylistChoices(owner, id, r)))
        });
    }

    fn chosen(&mut self, purpose: Purpose, target: Option<String>, ctx: &mut Global) {
        let files = self.targets();
        match purpose {
            Purpose::AddToStatic => {
                ctx.switch_to(super::PLAYLISTS, Some(Handoff::AddFiles { reference: target, files }));
            }
            Purpose::Enqueue => {
                let Some(playlist) = target else { return };
                ctx.request(match files.len() {
                    1 => Action::Enqueue { playlist, media: files[0].clone() },
                    _ => Action::EnqueueMany { playlist, media: files },
                });
            }
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
        if !self.marks.is_empty() {
            parts.push(tr!("media-marked", n = self.marks.len()));
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
            Cell::from(""),
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
                let mark = if self.marks.contains(&m.rel_path) { Span::styled("●", s.accent()) } else { Span::raw(" ") };
                let mut row = Row::new(vec![
                    Cell::from(mark),
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
                Constraint::Length(1),
                Constraint::Fill(2),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Length(year_w),
                Constraint::Length(dur_w),
                Constraint::Fill(1),
            ]
        } else {
            vec![
                Constraint::Length(1),
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

    fn render_card(&self, area: Rect, buf: &mut Buffer, ctx: &Global) {
        let Some(card) = &self.card else { return };
        let s = Styles(&ctx.theme);
        let tz = ctx.store.tz.as_ref();
        let m = &card.media;
        let w = 84.min(area.width.saturating_sub(2)).max(40);
        let h = area.height.saturating_sub(2).clamp(12, 34);
        let box_a = centered(area, w, h);
        Clear.render(box_a, buf);
        let block = frame(&tr!("card-title"), &s, false)
            .title_bottom(Span::styled(format!(" {} ", tr!("card-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);

        let kv = |k: String, v: Span<'static>| {
            Line::from(vec![Span::styled(format!(" {:<16} ", fit::ellipsize(&k, 16)), s.label()), v])
        };
        let or_dash = |v: &str| if v.is_empty() { Span::styled("—", s.muted()) } else { Span::raw(v.to_string()) };
        let mut lines = vec![
            kv(tr!("media-field-title"), if m.title.is_empty() { Span::styled(tr!("card-no-title"), s.warn()) } else { Span::styled(m.title.clone(), s.title()) }),
            kv(tr!("media-field-artist"), or_dash(&m.artist)),
            kv(tr!("media-field-album"), or_dash(&m.album)),
            kv(tr!("media-field-year"), if m.year == 0 { Span::styled("—", s.muted()) } else { Span::raw(m.year.to_string()) }),
            kv(tr!("media-field-duration"), Span::raw(mmss(m.duration_ms))),
            kv(tr!("media-field-genre"), or_dash(&m.genres.join(", "))),
            kv(tr!("card-size"), Span::raw(tr!("card-size-mb", mb = format!("{:.1}", m.size_bytes as f64 / 1_048_576.0)))),
            kv(tr!("media-field-path"), Span::raw(m.rel_path.clone())),
            kv(
                tr!("card-state"),
                if m.available { Span::styled(tr!("card-available"), s.ok()) } else { Span::styled(tr!("card-unavailable"), s.error()) },
            ),
            Line::default(),
            Line::styled(format!(" {}", tr!("card-tags")), s.title()),
        ];
        // Ce que l'index ne porte pas : lu dans le fichier (GetTags).
        match card.data.as_ref().map(|d| &d.tags) {
            None => lines.push(Line::styled(format!("   {}", tr!("media-loading")), s.muted())),
            Some(Err(e)) => lines.push(Line::styled(format!("   {e}"), s.muted())),
            Some(Ok(t)) => {
                let how = |manual: &str| {
                    Span::styled(
                        format!("  {}", if manual.is_empty() { tr!("card-tags-auto") } else { tr!("card-tags-manual") }),
                        s.muted(),
                    )
                };
                let bpm = if t.bpm == 0 { Span::styled("—", s.muted()) } else { Span::raw(t.bpm.to_string()) };
                lines.push(kv(tr!("tags-bpm"), bpm));
                let mut tempo = vec![Span::styled(format!(" {:<16} ", fit::ellipsize(&tr!("tags-tempo"), 16)), s.label()), or_dash(&t.tempo)];
                if !t.tempo.is_empty() {
                    tempo.push(how(&t.tempo_manual));
                }
                lines.push(Line::from(tempo));
                let mut creation =
                    vec![Span::styled(format!(" {:<16} ", fit::ellipsize(&tr!("tags-creation"), 16)), s.label()), or_dash(&t.creation)];
                if !t.creation.is_empty() {
                    creation.push(how(&t.creation_manual));
                }
                lines.push(Line::from(creation));
                for src in &t.sources {
                    lines.push(kv(src.name.clone(), or_dash(&src.values.join(", "))));
                }
            }
        }
        lines.push(Line::default());
        lines.push(Line::styled(format!(" {}", tr!("card-playlists")), s.title()));
        match card.data.as_ref().map(|d| &d.playlists) {
            None => lines.push(Line::styled(format!("   {}", tr!("media-loading")), s.muted())),
            Some(Err(e)) => lines.push(Line::styled(format!("   {e}"), s.error())),
            Some(Ok(ps)) if ps.is_empty() => lines.push(Line::styled(format!("   {}", tr!("card-no-playlist")), s.warn())),
            Some(Ok(ps)) => {
                for p in ps {
                    let mut spans = vec![
                        Span::raw("   "),
                        Span::styled(p.rel_path.clone(), if p.enabled { s.base() } else { s.muted() }),
                        Span::styled(format!("  {}", mode_label(&p.mode)), s.label()),
                    ];
                    if !p.enabled {
                        spans.push(Span::styled(format!("  {}", tr!("pl-disabled")), s.muted()));
                    }
                    if !p.rules.is_empty() {
                        spans.push(Span::styled(format!("  · {}", tr!("pl-used-rules", list = p.rules.join(", "))), s.muted()));
                    }
                    if !p.groups.is_empty() {
                        spans.push(Span::styled(format!("  · {}", tr!("pl-used-groups", list = p.groups.join(", "))), s.muted()));
                    }
                    lines.push(Line::from(spans));
                }
            }
        }
        lines.push(Line::default());
        lines.push(Line::styled(format!(" {}", tr!("card-plays")), s.title()));
        match &card.data {
            None => lines.push(Line::styled(format!("   {}", tr!("media-loading")), s.muted())),
            Some(d) => {
                let labels = [tr!("card-24h"), tr!("card-7d"), tr!("card-30d"), tr!("card-all")];
                let mut spans = vec![Span::raw("   ")];
                let mut last = None;
                for (i, (label, r)) in labels.iter().zip(&d.plays).enumerate() {
                    if i > 0 {
                        spans.push(Span::styled("   ", s.muted()));
                    }
                    spans.push(Span::styled(format!("{label} "), s.label()));
                    match r {
                        Ok(Some(row)) => {
                            spans.push(Span::raw(format!("{} / {}", row.aired, row.picked)));
                            last = Some(row.last_at);
                        }
                        Ok(None) => spans.push(Span::raw("0 / 0")),
                        Err(e) => spans.push(Span::styled(fit::ellipsize(e, 30), s.error())),
                    }
                }
                lines.push(Line::from(spans));
                lines.push(Line::styled(format!("   {}", tr!("card-plays-legend")), s.muted()));
                let last = last.and_then(|t| crate::store::local_day_hm(tz, t));
                lines.push(Line::from(vec![
                    Span::styled(format!("   {} ", tr!("card-last")), s.label()),
                    match last {
                        Some(t) => Span::raw(t),
                        None => Span::styled(tr!("card-never"), s.muted()),
                    },
                ]));
            }
        }
        Paragraph::new(lines).render(inner, buf);
    }

    /// Touches d'un sélecteur ou de la table (hors saisie, hors fiche).
    fn key(&mut self, k: &ratatui_crossterm::crossterm::event::KeyEvent, ctx: &mut Global) -> bool {
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
            KeyCode::Char(' ') => {
                self.toggle_mark();
                self.move_to(self.selected + 1, ctx);
            }
            KeyCode::Char('c') => self.marks.clear(),
            KeyCode::Up => self.move_to(self.selected.saturating_sub(1), ctx),
            KeyCode::Down => self.move_to(self.selected + 1, ctx),
            KeyCode::PageUp => self.move_to(self.selected.saturating_sub(page), ctx),
            KeyCode::PageDown => self.move_to(self.selected + page, ctx),
            KeyCode::Home => self.move_to(0, ctx),
            KeyCode::End => self.move_to(usize::MAX, ctx),
            _ => return false,
        }
        true
    }

    /// Événements d'une recherche (écran ou sélecteur) : réponses, frappe.
    pub fn handle(&mut self, event: &AppEvent, ctx: &mut Global) -> Control<AppEvent> {
        match event {
            AppEvent::Media(owner, id, r, append) if *owner == self.owner => {
                if *id == self.list_req {
                    self.apply(r, *append);
                    return Control::Changed;
                }
                return Control::Continue;
            }
            AppEvent::MediaTyped(owner, id) if *owner == self.owner => {
                if *id == self.typing && self.editing {
                    let live = live_query(self.input.text());
                    if live != self.searched {
                        self.searched = live;
                        self.load(ctx, false);
                    }
                }
                return Control::Changed;
            }
            AppEvent::MediaCard(owner, id, card) if *owner == self.owner => {
                if let Some(c) = self.card.as_mut()
                    && c.request == *id
                {
                    c.data = Some((**card).clone());
                }
                return Control::Changed;
            }
            AppEvent::MediaTags(owner, id, r) if *owner == self.owner => {
                if self.tags_req == Some(*id) {
                    self.tags_req = None;
                    match &**r {
                        Ok((t, known)) => {
                            let batch = (self.tags_targets.len() > 1).then(|| self.tags_targets.clone());
                            self.tagform = Some(TagForm::new(t.clone(), known.clone(), batch));
                        }
                        Err(e) => ctx.open(crate::dialog::Modal::Info(crate::dialog::Info {
                            title: tr!("form-tags-read-failed"),
                            lines: vec![e.clone()],
                        })),
                    }
                }
                return Control::Changed;
            }
            AppEvent::ActionDone(Ok(_)) if !self.picker => {
                self.refresh(ctx);
                return Control::Changed;
            }
            AppEvent::PlaylistChoices(owner, id, r) if *owner == self.owner => {
                if let Some((_, p, req)) = self.chooser.as_mut()
                    && req == id
                {
                    p.set_items(r.clone());
                }
                return Control::Changed;
            }
            AppEvent::Event(_) => {}
            _ => return Control::Continue,
        }
        let AppEvent::Event(e) = event else { return Control::Continue };

        // Éditeur de tags ouvert : il capture tout.
        if let Some(f) = self.tagform.as_mut() {
            match f.handle(e) {
                TagOutcome::Pending => {}
                TagOutcome::Cancel => self.tagform = None,
                TagOutcome::Submit(action, lines) => {
                    self.tagform = None;
                    let yes = tr!("confirm-tags-yes");
                    let c = crate::dialog::Confirm::new(tr!("confirm-tags-title"), lines, yes, *action);
                    let c = if self.tags_targets.len() > 1 { c.danger() } else { c };
                    ctx.open(crate::dialog::Modal::Confirm(c));
                }
            }
            return Control::Changed;
        }

        // Sélecteur de playlist ouvert : il capture tout.
        if let Some((purpose, p, _)) = self.chooser.as_mut() {
            let purpose = *purpose;
            match p.handle(e) {
                PickedPlaylist::Unchanged => return Control::Unchanged,
                PickedPlaylist::Changed => return Control::Changed,
                PickedPlaylist::Cancel => self.chooser = None,
                PickedPlaylist::Chosen(r) => {
                    self.chooser = None;
                    self.chosen(purpose, Some(r), ctx);
                }
                PickedPlaylist::New => {
                    self.chooser = None;
                    self.chosen(purpose, None, ctx);
                }
            }
            return Control::Changed;
        }

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
                        return Control::Changed;
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
                        return Control::Changed;
                    }
                    _ => {}
                }
            }
            let before = self.input.text().to_string();
            self.input.handle(e, Regular);
            if self.input.text() != before {
                // Recherche en direct, `DEBOUNCE` après la dernière frappe.
                self.typing += 1;
                let (owner, id) = (self.owner, self.typing);
                ctx.spawn_async(async move {
                    tokio::time::sleep(DEBOUNCE).await;
                    Ok(Control::Event(AppEvent::MediaTyped(owner, id)))
                });
            }
            return Control::Changed;
        }
        let Event::Key(k) = e else { return Control::Continue };
        if k.kind != KeyEventKind::Press {
            return Control::Continue;
        }

        // Fiche ouverte.
        if self.card.is_some() {
            match k.code {
                KeyCode::Esc | KeyCode::Enter => self.card = None,
                KeyCode::Up | KeyCode::Down => {
                    let next = if k.code == KeyCode::Up { self.selected.saturating_sub(1) } else { self.selected + 1 };
                    self.move_to(next, ctx);
                    self.open_card(ctx);
                }
                KeyCode::Char('o') => {
                    let path = self.card.as_ref().map(|c| c.media.rel_path.clone()).unwrap_or_default();
                    ctx.open(ops::push_override_with(Some(&path)));
                }
                KeyCode::Char('p') => self.open_chooser(Purpose::AddToStatic, ctx),
                KeyCode::Char('f') => self.open_chooser(Purpose::Enqueue, ctx),
                KeyCode::Char('e') => self.edit_tags(ctx),
                _ => return Control::Unchanged,
            }
            return Control::Changed;
        }

        if self.picker {
            match k.code {
                KeyCode::Esc => {
                    self.picked = Some(Picked::Cancel);
                    return Control::Changed;
                }
                KeyCode::Enter => {
                    let chosen = self.targets();
                    if !chosen.is_empty() {
                        self.picked = Some(Picked::Chosen(chosen));
                    }
                    return Control::Changed;
                }
                _ => {}
            }
            return if self.key(k, ctx) { Control::Changed } else { Control::Unchanged };
        }

        match k.code {
            KeyCode::Enter => self.open_card(ctx),
            KeyCode::Char('o') => {
                let Some(m) = self.rows.get(self.selected) else { return Control::Continue };
                let path = m.rel_path.clone();
                ctx.open(ops::push_override_with(Some(&path)));
            }
            KeyCode::Char('p') => self.open_chooser(Purpose::AddToStatic, ctx),
            KeyCode::Char('f') => self.open_chooser(Purpose::Enqueue, ctx),
            KeyCode::Char('e') => self.edit_tags(ctx),
            _ => {
                if !self.key(k, ctx) {
                    return Control::Continue;
                }
            }
        }
        Control::Changed
    }

    pub fn draw(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) {
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
        if self.card.is_some() {
            self.render_card(area, buf, ctx);
        }
        if let Some((_, p, _)) = self.chooser.as_mut() {
            let cursor = p.render(area, buf, &ctx.theme);
            ctx.set_screen_cursor(cursor);
        }
        if let Some(f) = self.tagform.as_mut() {
            let cursor = f.render(area, buf, &ctx.theme);
            ctx.set_screen_cursor(cursor);
        }
    }

    /// Raccourcis du moment (aide et ligne du bas).
    pub fn keys(&self) -> &'static [KeyHelp] {
        if self.tagform.is_some() {
            &[
                (k!("key-ctrl-s"), k!("help-tags-write")),
                (k!("key-enter"), k!("help-tags-list")),
                (k!("key-esc"), k!("help-close")),
            ]
        } else if self.editing {
            &[(k!("key-enter"), k!("help-media-apply")), (k!("key-esc"), k!("help-media-cancel"))]
        } else if self.card.is_some() {
            &[
                (k!("key-esc"), k!("help-close")),
                (k!("key-up-down"), k!("help-card-prev-next")),
                (k!("key-e"), k!("help-media-edit-tags")),
                (k!("key-o"), k!("help-override")),
                (k!("key-p"), k!("help-media-to-playlist")),
                (k!("key-f"), k!("help-media-enqueue")),
            ]
        } else if self.picker {
            &[
                (k!("key-enter"), k!("help-picker-add")),
                (k!("key-space"), k!("help-media-mark")),
                (k!("key-slash"), k!("help-media-search")),
                (k!("key-esc"), k!("help-close")),
                (k!("key-s"), k!("help-media-sort")),
                (k!("key-c"), k!("help-media-clear-marks")),
            ]
        } else {
            &[
                (k!("key-slash"), k!("help-media-search")),
                (k!("key-enter"), k!("help-media-card")),
                (k!("key-space"), k!("help-media-mark")),
                (k!("key-e"), k!("help-media-edit-tags")),
                (k!("key-p"), k!("help-media-to-playlist")),
                (k!("key-f"), k!("help-media-enqueue")),
                (k!("key-o"), k!("help-override")),
                (k!("key-s"), k!("help-media-sort")),
                (k!("key-d"), k!("help-media-desc")),
                (k!("key-m"), k!("help-media-missing")),
                (k!("key-a"), k!("help-media-unavailable")),
                (k!("key-c"), k!("help-media-clear-marks")),
                (k!("key-r"), k!("help-media-reload")),
            ]
        }
    }

    /// Une saisie est en cours (texte de recherche ou filtre du sélecteur).
    pub fn typing_text(&self) -> bool {
        self.editing || self.chooser.is_some() || self.card.is_some() || self.tagform.is_some()
    }
}

impl Screen for Medias {
    fn title(&self) -> String {
        tr!("screen-media")
    }

    fn captures_text(&self) -> bool {
        self.typing_text()
    }

    fn enter(&mut self, ctx: &mut Global) -> Result<(), Error> {
        self.ensure_loaded(ctx);
        Ok(())
    }

    fn reconnected(&mut self, ctx: &mut Global) -> Result<(), Error> {
        if self.error.is_some() {
            self.load(ctx, false);
        }
        Ok(())
    }

    fn help(&self) -> &'static [KeyHelp] {
        self.keys()
    }

    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        Ok(self.handle(event, ctx))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        self.draw(area, buf, ctx);
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
        let a = parse_query("âge:<10d daft Age:>=30d age:");
        assert_eq!(a.age, [("<".to_string(), "10d".to_string()), (">=".to_string(), "30d".to_string())]);
        assert_eq!(a.words, "daft");
        assert_eq!(live_query("daft âge:<1"), "daft");
        // Un « : » ailleurs que dans un préfixe reste un mot.
        assert_eq!(parse_query("12:30").words, "12:30");
    }

    fn media(path: &str, title: &str) -> Media {
        Media { rel_path: path.into(), title: title.into(), available: true, ..Default::default() }
    }

    fn page(media: Vec<Media>) -> Result<SearchMediaResponse, String> {
        Ok(SearchMediaResponse { total: media.len() as u64, media, ..Default::default() })
    }

    #[tokio::test]
    async fn the_list_read_after_a_write_reaches_the_list_and_the_open_card() {
        use clap::Parser;
        let args = crate::Args::parse_from(["stationd-tui"]);
        let ch = crate::rpc::lazy_channel(&args.addr).unwrap();
        let mut ctx = Global::new(&args, ch.clone(), ch);
        let mut m = Medias::new(false);
        let owner = m.owner;
        // Liste chargée, fiche ouverte sur a.mp3.
        m.list_req = 1;
        m.request = 1;
        let _ = m.handle(&AppEvent::Media(owner, 1, page(vec![media("a.mp3", "Veridis")]), false), &mut ctx);
        m.request = 2;
        m.card = Some(Card { media: m.rows[0].clone(), request: 2, data: None });
        // Après l'écriture (refresh) : liste relue (3) PUIS fiche relue (4).
        m.list_req = 3;
        m.request = 4;
        m.card.as_mut().unwrap().request = 4;
        let _ = m.handle(&AppEvent::Media(owner, 3, page(vec![media("a.mp3", "Veridis Quo")]), false), &mut ctx);
        assert_eq!(m.rows[0].title, "Veridis Quo", "la réponse de la liste n'est pas périmée par la fiche");
        assert_eq!(m.card.as_ref().unwrap().media.title, "Veridis Quo", "la fiche suit la ligne relue");
        // Une réponse de liste vraiment ancienne reste ignorée.
        let _ = m.handle(&AppEvent::Media(owner, 1, page(vec![media("a.mp3", "Veridis")]), false), &mut ctx);
        assert_eq!(m.rows[0].title, "Veridis Quo");
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
