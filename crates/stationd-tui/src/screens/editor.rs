//! Éditeur d'une playlist (dossier §5.3) : formulaire par mode à gauche, le
//! TOML produit et les diagnostics de stationd à droite, l'aperçu du pool en
//! direct. Le formulaire n'est qu'une présentation du TOML (`draft`) :
//! stationd seul valide (`PreviewPool`, 300 ms après la dernière frappe) et
//! enregistre (`Save`, avec la révision lue à l'ouverture). La TUI n'écrit
//! jamais de fichier.
//!
//! `Ctrl+T` bascule sur le TOML brut (éditeur de texte) ; un TOML illisible
//! y reste modifiable, le formulaire attend qu'il se relise.

use std::time::Duration;

use rat_salsa::{Control, SalsaContext};
use rat_theme4::WidgetStyle;
use rat_widget::event::{HandleEvent, Regular, TextOutcome};
use rat_widget::text::HasScreenCursor;
use rat_widget::text_input::{TextInput, TextInputState};
use rat_widget::textarea::{TextArea, TextAreaState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::clear::Clear;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use stationd_proto::library::GenreCount;
use stationd_proto::playlist::diagnostic::{Code, Severity};
use stationd_proto::playlist::{Diagnostic, ExportResponse, PlaylistSummary, PreviewPoolResponse, SaveResponse};

use super::medias::{Medias, Picked as PickedMedia};
use super::picker::{PlaylistPicker, Picked as PickedPlaylist, mode_label};
use super::PlEvent;
use crate::app::{AppEvent, Global};
use crate::dialog::{Choice, ChoiceBox, centered, frame};
use crate::draft::{Draft, FILTER_FIELDS, FilterPart, Key, MODES, MemberPart, filter_ops, orders};
use crate::screen::KeyHelp;
use crate::style::Styles;
use crate::{fit, k, tr};

/// Délai après la dernière modification avant l'aperçu du pool.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Taille de l'échantillon demandé.
const SAMPLE: u32 = 20;

/// Ce qu'une ligne du formulaire modifie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Ref d'une nouvelle playlist (où stationd écrira le fichier).
    Ref,
    Key(Key),
    Filter(usize, FilterPart),
    Member(usize, MemberPart),
    File(usize),
    AddFilter,
    AddMember,
    AddFiles,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Header,
    /// Information non modifiable.
    Note,
    Text,
    /// Choix fermé : (libellé, valeur) ; valeur vide = absent du TOML.
    Choice(Vec<(String, String)>),
    /// Ligne d'action (ajouter…).
    Action,
}

#[derive(Debug, Clone)]
struct Row {
    target: Option<Target>,
    label: String,
    kind: Kind,
    value: String,
    /// `field_path` des diagnostics qui la concernent.
    path: String,
}

impl Row {
    fn focusable(&self) -> bool {
        self.target.is_some() && matches!(self.kind, Kind::Text | Kind::Choice(_) | Kind::Action)
    }
}

/// Ligne du formulaire qui porte un diagnostic : le chemin exact, sinon la
/// première ligne sous ce chemin (`selection.filter[2]` → son champ), sinon
/// la ligne la plus proche au-dessus (`selection.filter[2].value.x` → la
/// valeur).
fn diag_row(paths: &[&str], diag: &str) -> Option<usize> {
    if diag.is_empty() {
        return None;
    }
    if let Some(i) = paths.iter().position(|p| *p == diag) {
        return Some(i);
    }
    let under = |p: &str, base: &str| p.len() > base.len() && p.starts_with(base) && matches!(p.as_bytes()[base.len()], b'.' | b'[');
    if let Some(i) = paths.iter().position(|p| under(p, diag)) {
        return Some(i);
    }
    paths
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.is_empty() && under(diag, p))
        .max_by_key(|(_, p)| p.len())
        .map(|(i, _)| i)
}

/// Libellé d'une valeur de la grammaire (le jeton TOML reste visible).
fn val_label(v: &str) -> String {
    let t = match v {
        "shuffle" => tr!("val-shuffle"),
        "sequential" => tr!("val-sequential"),
        "newest" => tr!("val-newest"),
        "oldest" => tr!("val-oldest"),
        "fifo" => tr!("val-fifo"),
        "lifo" => tr!("val-lifo"),
        "all" => tr!("val-all"),
        "any" => tr!("val-any"),
        "filename" => tr!("val-filename"),
        "mtime" => tr!("val-mtime"),
        "published" => tr!("val-published"),
        "weighted" => tr!("val-weighted"),
        "rotate" => tr!("val-rotate"),
        "sequence" => tr!("val-sequence"),
        "abort" => tr!("val-abort"),
        "skip" => tr!("val-skip"),
        "fallthrough" => tr!("val-fallthrough"),
        "stop" => tr!("val-stop"),
        "disable" => tr!("val-disable"),
        "hold" => tr!("val-hold"),
        "true" => return tr!("val-yes"),
        "false" => return tr!("val-no"),
        "path" => tr!("media-field-path"),
        "genre" => tr!("media-field-genre"),
        "genre_ai" => tr!("val-genre-ai"),
        "mood" => tr!("val-mood"),
        "artist" => tr!("media-field-artist"),
        "title" => tr!("media-field-title"),
        "album" => tr!("media-field-album"),
        "year" => tr!("media-field-year"),
        "duration" => tr!("val-duration-s"),
        "age" => tr!("val-age"),
        "creation" => tr!("val-creation"),
        "tempo" => tr!("val-tempo"),
        "play_count" => tr!("val-play-count"),
        "last_played" => tr!("val-last-played"),
        "prefix" => tr!("val-prefix"),
        "eq" => tr!("val-eq"),
        "ne" => tr!("val-ne"),
        "contains" => tr!("val-contains"),
        "has" => tr!("val-has"),
        "has_any" => tr!("val-has-any"),
        "has_all" => tr!("val-has-all"),
        "has_none" => tr!("val-has-none"),
        _ => return v.to_string(),
    };
    if t == v { t } else { format!("{t} ({v})") }
}

/// Options d'un choix : « absent » en tête si le champ est facultatif, la
/// valeur actuelle ajoutée si elle n'est pas proposée (jamais écrasée en
/// silence).
fn options(values: &[&str], optional: bool, current: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if optional {
        out.push((tr!("pl-absent"), String::new()));
    }
    out.extend(values.iter().map(|v| (val_label(v), v.to_string())));
    if !current.is_empty() && !values.contains(&current) {
        out.push((val_label(current), current.to_string()));
    }
    out
}

/// Texte d'un diagnostic, traduit par son code (jamais par son message).
pub fn diag_text(d: &Diagnostic) -> String {
    let code = Code::try_from(d.code).unwrap_or(Code::Unspecified);
    let mut t = match code {
        Code::Syntax => tr!("diag-syntax", detail = d.message.clone()),
        Code::UnknownField => tr!("diag-unknown-field"),
        Code::MissingField => tr!("diag-missing-field"),
        Code::BadValue => tr!("diag-bad-value"),
        Code::NotAllowed => tr!("diag-not-allowed"),
        Code::RequiredForMode => tr!("diag-required-for-mode"),
        Code::Conflict => tr!("diag-conflict"),
        Code::BadFilter => tr!("diag-bad-filter"),
        Code::BadDuration => tr!("diag-bad-duration"),
        Code::UnknownRef => tr!("diag-unknown-ref"),
        Code::BadRef => tr!("diag-bad-ref"),
        Code::Cycle => tr!("diag-cycle"),
        Code::IdChanged => tr!("diag-id-changed"),
        Code::EmptyPool => tr!("diag-empty-pool"),
        Code::Unspecified => tr!("diag-unknown", code = d.code),
    };
    if !d.rejected.is_empty() {
        t.push_str(&tr!("diag-rejected", value = d.rejected.clone()));
    }
    if !d.expected.is_empty() {
        t.push_str(&tr!("diag-expected", values = d.expected.clone()));
    }
    t
}

/// Coupe `text` en lignes d'au plus `width` cellules, entre les éléments
/// d'une liste (« a (1), b (2) ») : un élément n'est jamais coupé en deux,
/// un élément plus long que la ligne reste entier sur la sienne.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let w = |s: &str| Span::raw(s).width();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let items: Vec<&str> = text.split(", ").collect();
    for (i, item) in items.iter().enumerate() {
        let piece = if i + 1 < items.len() { format!("{item},") } else { item.to_string() };
        let need = if cur.is_empty() { w(&piece) } else { w(&cur) + 1 + w(&piece) };
        if need > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&piece);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Un nom de fichier simple, sans chemin, disponible dans la liste connue.
fn suggested_filename(name: &str, playlists: &[PlaylistSummary]) -> String {
    let name = name.trim();
    let name = name.strip_suffix(".toml").unwrap_or(name);
    let mut base = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            base.push(c);
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let base = base.trim_end_matches('-');
    if base.is_empty() {
        return String::new();
    }
    let taken = |candidate: &str| playlists.iter().any(|p| {
        p.rel_path.trim_end_matches(".toml").eq_ignore_ascii_case(candidate)
    });
    let mut candidate = base.to_string();
    let mut suffix = 2;
    while taken(&candidate) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    format!("{candidate}.toml")
}

fn is_error(d: &Diagnostic) -> bool {
    d.severity == Severity::Error as i32
}

fn mmss(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Ce qui recouvre l'éditeur.
enum Overlay {
    /// Conflit de révision : garder / comparer / recharger.
    Conflict(ChoiceBox),
    /// Échap sur un brouillon modifié : continuer / abandonner.
    Discard(ChoiceBox),
    /// Fichier du nœud (révision `revision`) à côté du brouillon.
    Compare { file: String, revision: String, scroll: usize },
    /// Sélecteur de médias (playlist statique).
    Media(Box<Medias>),
    /// Sélecteur de playlist (membre d'un groupe).
    Members(Box<PlaylistPicker>),
}

/// Ce que l'éditeur dit à l'écran Playlists.
pub enum Outcome {
    Stay,
    /// Fermé (enregistré ou abandonné).
    Close,
    /// Enregistré : la liste est à relire.
    Saved,
}

pub struct Editor {
    /// Identifie les réponses de CET éditeur.
    owner: u64,
    /// Ref de la playlist existante (vide pendant la création).
    reference: String,
    is_new: bool,
    ref_text: String,
    /// Suit le nom tant que le fichier n'a pas été modifié manuellement.
    suggest_ref: bool,
    /// Révision du fichier lue à l'ouverture (vide = création).
    revision: String,
    draft: Draft,
    /// Texte au dernier chargement / enregistrement.
    saved_text: String,
    focus: Target,
    input: TextInputState,
    input_for: Option<Target>,
    raw: Option<TextAreaState>,
    preview: Option<Result<PreviewPoolResponse, String>>,
    preview_pending: bool,
    /// Diagnostics affichés : ceux du dernier aperçu, ou du dernier `Save`.
    diags: Vec<Diagnostic>,
    typing: u64,
    preview_req: u64,
    saving: bool,
    overlay: Option<Overlay>,
    genres: Vec<GenreCount>,
    /// Liste des playlists (sélecteur de membres).
    playlists: Vec<PlaylistSummary>,
    /// Message en tête (résultat d'un enregistrement…), `true` = erreur.
    message: Option<(String, bool)>,
    form_scroll: usize,
    toml_scroll: usize,
}

impl Editor {
    /// Ouvre une playlist existante depuis son `Export` : le fichier (avec
    /// ses commentaires) s'il existe, sinon ce qui est appliqué.
    pub fn open(x: &ExportResponse, playlists: Vec<PlaylistSummary>, ctx: &mut Global) -> Self {
        let text = if x.file_toml.is_empty() { x.applied_toml.clone() } else { x.file_toml.clone() };
        let mut e = Self::with(x.rel_path.clone(), false, x.revision.clone(), Draft::parse(&text), playlists, ctx);
        if x.file_differs {
            e.message = Some((tr!("ed-file-differs", file = x.file.clone()), false));
        } else if x.file_toml.is_empty() {
            e.message = Some((tr!("ed-no-file"), false));
        }
        e
    }

    /// Nouvelle playlist, du mode donné.
    pub fn new_playlist(mode: &str, playlists: Vec<PlaylistSummary>, ctx: &mut Global) -> Self {
        let mut e = Self::with(String::new(), true, String::new(), Draft::template(mode, ""), playlists, ctx);
        e.focus = Target::Key(Key::Name);
        e.saved_text = String::new(); // rien n'existe encore : tout est à enregistrer
        e.bind_input();
        e
    }

    fn with(
        reference: String,
        is_new: bool,
        revision: String,
        draft: Draft,
        playlists: Vec<PlaylistSummary>,
        ctx: &mut Global,
    ) -> Self {
        let mut e = Self {
            owner: super::next_owner(),
            ref_text: reference.clone(),
            suggest_ref: is_new,
            reference,
            is_new,
            revision,
            saved_text: draft.text().to_string(),
            draft,
            focus: Target::Key(Key::Name),
            input: TextInputState::new(),
            input_for: None,
            raw: None,
            preview: None,
            preview_pending: false,
            diags: Vec::new(),
            typing: 0,
            preview_req: 0,
            saving: false,
            overlay: None,
            genres: Vec::new(),
            playlists,
            message: None,
            form_scroll: 0,
            toml_scroll: 0,
        };
        e.bind_input();
        e.run_preview(ctx);
        let (owner, channel) = (e.owner, ctx.channel.clone());
        ctx.spawn_async(async move {
            let r = crate::rpc::list_genres(channel).await;
            Ok(Control::Event(AppEvent::Playlists(Box::new(PlEvent::Genres(owner, r)))))
        });
        e
    }

    /// Ref sous laquelle le brouillon est jugé et enregistré.
    fn target_ref(&self) -> String {
        if self.is_new { self.ref_text.trim().to_string() } else { self.reference.clone() }
    }

    /// Ce brouillon est-il celui de la playlist `reference` (casse et
    /// `.toml` ignorés, comme les refs de stationd) ?
    pub fn reference_is(&self, reference: &str) -> bool {
        let norm = |r: &str| r.trim().trim_end_matches(".toml").to_lowercase();
        !self.target_ref().is_empty() && norm(&self.target_ref()) == norm(reference)
    }

    pub fn dirty(&self) -> bool {
        self.is_new || self.draft.text() != self.saved_text
    }

    /// Ajoute des médias au brouillon (médias confiés par l'écran Médias).
    pub fn add_files(&mut self, files: &[String], ctx: &mut Global) {
        let n = self.draft.add_files(files);
        self.message = Some((tr!("ed-files-added", n = n, total = files.len()), false));
        if let Some(i) = self.draft.files().len().checked_sub(1) {
            self.focus = Target::File(i);
        }
        self.structure_changed(ctx);
    }

    // --- lignes du formulaire ---------------------------------------------------

    fn rows(&self) -> Vec<Row> {
        let d = &self.draft;
        let mut out = Vec::new();
        let header = |out: &mut Vec<Row>, label: String| {
            out.push(Row { target: None, label, kind: Kind::Header, value: String::new(), path: String::new() })
        };
        let text = |out: &mut Vec<Row>, t: Target, label: String, value: String, path: String| {
            out.push(Row { target: Some(t), label, kind: Kind::Text, value, path })
        };
        let choice = |out: &mut Vec<Row>, key: Key, label: String, values: &[&str], optional: bool| {
            let current = d.get(key).unwrap_or_default();
            let opts = options(values, optional, &current);
            out.push(Row { target: Some(Target::Key(key)), label, kind: Kind::Choice(opts), value: current, path: key.path() })
        };
        let action = |out: &mut Vec<Row>, t: Target, label: String, path: &str| {
            out.push(Row { target: Some(t), label, kind: Kind::Action, value: String::new(), path: path.to_string() })
        };

        header(&mut out, tr!("pl-h-identity"));
        text(&mut out, Target::Key(Key::Name), tr!("pl-f-name"), d.get(Key::Name).unwrap_or_default(), Key::Name.path());
        if self.is_new {
            text(&mut out, Target::Ref, tr!("pl-f-ref"), self.ref_text.clone(), String::new());
        } else {
            out.push(Row {
                target: None,
                label: tr!("pl-f-ref"),
                kind: Kind::Note,
                value: self.reference.clone(),
                path: String::new(),
            });
        }
        let enabled = d.get(Key::Enabled).unwrap_or_else(|| "true".into());
        out.push(Row {
            target: Some(Target::Key(Key::Enabled)),
            label: tr!("pl-f-enabled"),
            kind: Kind::Choice(options(&["true", "false"], false, &enabled)),
            value: enabled,
            path: Key::Enabled.path(),
        });

        header(&mut out, tr!("pl-h-selection"));
        let mode = d.mode();
        let mode_opts: Vec<(String, String)> = {
            let mut o: Vec<(String, String)> = MODES
                .iter()
                .map(|m| {
                    let l = mode_label(m);
                    (if l == *m { l } else { format!("{l} ({m})") }, m.to_string())
                })
                .collect();
            if !MODES.contains(&mode.as_str()) {
                o.push((mode.clone(), mode.clone()));
            }
            o
        };
        out.push(Row {
            target: Some(Target::Key(Key::Mode)),
            label: tr!("pl-f-mode"),
            kind: Kind::Choice(mode_opts),
            value: mode.clone(),
            path: Key::Mode.path(),
        });
        match mode.as_str() {
            "static" => {
                choice(&mut out, Key::Order, tr!("pl-f-order"), orders("static"), true);
                let files = d.files();
                header(&mut out, tr!("pl-h-files", n = files.len()));
                for (i, f) in files.iter().enumerate() {
                    text(&mut out, Target::File(i), format!("{:>3}.", i + 1), f.clone(), format!("selection.files[{}]", i + 1));
                }
                action(&mut out, Target::AddFiles, tr!("pl-add-files"), "selection.files");
            }
            "dynamic" => {
                choice(&mut out, Key::Match, tr!("pl-f-match"), &["all", "any"], true);
                choice(&mut out, Key::Order, tr!("pl-f-order"), orders("dynamic"), true);
                let dated = matches!(d.get(Key::Order).as_deref(), Some("newest" | "oldest"));
                if dated || d.get(Key::OrderBy).is_some() {
                    choice(&mut out, Key::OrderBy, tr!("pl-f-order-by"), &["filename", "mtime", "published"], true);
                }
                if dated || d.get(Key::UnplayedOnly).is_some() {
                    choice(&mut out, Key::UnplayedOnly, tr!("pl-f-unplayed"), &["true", "false"], true);
                }
                let filters = d.filters();
                header(&mut out, tr!("pl-h-filters", n = filters.len()));
                for (i, f) in filters.iter().enumerate() {
                    let base = format!("selection.filter[{}]", i + 1);
                    out.push(Row {
                        target: Some(Target::Filter(i, FilterPart::Field)),
                        label: tr!("pl-f-filter", n = i + 1),
                        kind: Kind::Choice(options(&FILTER_FIELDS, false, &f.field)),
                        value: f.field.clone(),
                        path: format!("{base}.field"),
                    });
                    out.push(Row {
                        target: Some(Target::Filter(i, FilterPart::Op)),
                        label: format!("  {}", tr!("pl-f-op")),
                        kind: Kind::Choice(options(filter_ops(&f.field), false, &f.op)),
                        value: f.op.clone(),
                        path: format!("{base}.op"),
                    });
                    text(&mut out, Target::Filter(i, FilterPart::Value), format!("  {}", tr!("pl-f-value")), f.value.clone(), format!("{base}.value"));
                    // `play_count` porte une fenêtre glissante `within`.
                    if f.field == "play_count" {
                        text(&mut out, Target::Filter(i, FilterPart::Within), format!("  {}", tr!("pl-f-within")), f.within.clone(), format!("{base}.within"));
                    }
                }
                action(&mut out, Target::AddFilter, tr!("pl-add-filter"), "selection.filter");
            }
            "remote" => {
                text(&mut out, Target::Key(Key::Url), tr!("pl-f-url"), d.get(Key::Url).unwrap_or_default(), Key::Url.path());
            }
            "queue" => {
                choice(&mut out, Key::Order, tr!("pl-f-order"), orders("queue"), true);
                text(&mut out, Target::Key(Key::MaxLen), tr!("pl-f-max-len"), d.get(Key::MaxLen).unwrap_or_default(), Key::MaxLen.path());
            }
            "group" => {
                choice(&mut out, Key::Strategy, tr!("pl-f-strategy"), &["weighted", "rotate", "sequence", "shuffle"], true);
                let strategy = d.get(Key::Strategy).unwrap_or_default();
                let quota = matches!(strategy.as_str(), "sequence" | "shuffle");
                if quota || d.get(Key::OnMemberUnavailable).is_some() {
                    choice(&mut out, Key::OnMemberUnavailable, tr!("pl-f-on-member-unavailable"), &["abort", "skip"], true);
                }
                let members = d.members();
                header(&mut out, tr!("pl-h-members", n = members.len()));
                for (i, m) in members.iter().enumerate() {
                    let base = format!("selection.members[{}]", i + 1);
                    text(&mut out, Target::Member(i, MemberPart::Ref), tr!("pl-f-member", n = i + 1), m.r#ref.clone(), format!("{base}.ref"));
                    if strategy == "weighted" || !m.weight.is_empty() {
                        text(&mut out, Target::Member(i, MemberPart::Weight), format!("  {}", tr!("pl-f-weight")), m.weight.clone(), format!("{base}.weight"));
                    }
                    if quota || !m.take.is_empty() {
                        text(&mut out, Target::Member(i, MemberPart::Take), format!("  {}", tr!("pl-f-take")), m.take.clone(), format!("{base}.take"));
                    }
                    if quota || !m.take_random_min.is_empty() {
                        text(&mut out, Target::Member(i, MemberPart::TakeRandomMin), format!("  {}", tr!("pl-f-take-random-min")), m.take_random_min.clone(), format!("{base}.take_random_min"));
                    }
                    if quota || !m.take_random_max.is_empty() {
                        text(&mut out, Target::Member(i, MemberPart::TakeRandomMax), format!("  {}", tr!("pl-f-take-random-max")), m.take_random_max.clone(), format!("{base}.take_random_max"));
                    }
                    if quota || !m.runtime.is_empty() {
                        text(&mut out, Target::Member(i, MemberPart::Runtime), format!("  {}", tr!("pl-f-runtime")), m.runtime.clone(), format!("{base}.runtime"));
                    }
                }
                action(&mut out, Target::AddMember, tr!("pl-add-member"), "selection.members");
            }
            _ => {}
        }

        header(&mut out, tr!("pl-h-broadcast"));
        text(&mut out, Target::Key(Key::Limit), tr!("pl-f-limit"), d.get(Key::Limit).unwrap_or_default(), Key::Limit.path());
        choice(&mut out, Key::Repeat, tr!("pl-f-repeat"), &["true", "false"], true);
        choice(&mut out, Key::OnExhausted, tr!("pl-f-on-exhausted"), &["fallthrough", "stop", "disable", "hold"], true);
        for (key, label) in [
            (Key::NoSameArtist, tr!("pl-f-no-same-artist")),
            (Key::NoSameTrack, tr!("pl-f-no-same-track")),
            (Key::NoSameTitle, tr!("pl-f-no-same-title")),
        ] {
            text(&mut out, Target::Key(key), label, d.get(key).unwrap_or_default(), key.path());
        }
        out
    }

    fn focusables(rows: &[Row]) -> Vec<Target> {
        rows.iter().filter(|r| r.focusable()).filter_map(|r| r.target).collect()
    }

    /// Garde le focus sur une ligne qui existe (après un changement de mode,
    /// un membre retiré…).
    fn settle_focus(&mut self) {
        let f = Self::focusables(&self.rows());
        if !f.contains(&self.focus) {
            self.focus = match self.focus {
                Target::Filter(i, _) if i > 0 => Target::Filter(i - 1, FilterPart::Field),
                Target::Member(i, _) if i > 0 => Target::Member(i - 1, MemberPart::Ref),
                Target::File(i) if i > 0 => Target::File(i - 1),
                Target::Filter(..) => Target::AddFilter,
                Target::Member(..) => Target::AddMember,
                Target::File(_) => Target::AddFiles,
                _ => Target::Key(Key::Mode),
            };
            if !f.contains(&self.focus) {
                self.focus = f.first().copied().unwrap_or(Target::Key(Key::Name));
            }
        }
        self.bind_input();
    }

    fn move_focus(&mut self, delta: isize) {
        let f = Self::focusables(&self.rows());
        if f.is_empty() {
            return;
        }
        let at = f.iter().position(|t| *t == self.focus).unwrap_or(0) as isize;
        let next = (at + delta).clamp(0, f.len() as isize - 1) as usize;
        self.focus = f[next];
        self.bind_input();
    }

    fn text_value(&self, t: Target) -> Option<String> {
        self.rows().into_iter().find(|r| r.target == Some(t) && r.kind == Kind::Text).map(|r| r.value)
    }

    /// Le champ de saisie suit la ligne qui a le focus.
    fn bind_input(&mut self) {
        match self.text_value(self.focus) {
            Some(v) => {
                if self.input_for != Some(self.focus) {
                    self.input.set_text(v);
                    // Curseur en fin de valeur : on complète ou on efface.
                    self.input.move_to_line_end(false);
                    self.input_for = Some(self.focus);
                }
                self.input.focus.set(self.raw.is_none());
            }
            None => {
                self.input_for = None;
                self.input.focus.set(false);
            }
        }
    }

    /// Le brouillon a changé de forme (ajout, retrait, mode…) : le champ de
    /// saisie est relu et l'aperçu relancé.
    fn structure_changed(&mut self, ctx: &mut Global) {
        self.input_for = None;
        self.settle_focus();
        self.changed(ctx);
    }

    /// Une modification : aperçu relancé après `DEBOUNCE`.
    fn changed(&mut self, ctx: &mut Global) {
        if self.is_new && self.suggest_ref && self.draft.readable() {
            self.ref_text = suggested_filename(&self.draft.get(Key::Name).unwrap_or_default(), &self.playlists);
            if self.input_for == Some(Target::Ref) {
                self.input.set_text(&self.ref_text);
                self.input.move_to_line_end(false);
            }
        }
        self.typing += 1;
        self.preview_pending = true;
        let (owner, id) = (self.owner, self.typing);
        ctx.spawn_async(async move {
            tokio::time::sleep(DEBOUNCE).await;
            Ok(Control::Event(AppEvent::Playlists(Box::new(PlEvent::Typed(owner, id)))))
        });
    }

    fn run_preview(&mut self, ctx: &mut Global) {
        self.preview_req += 1;
        self.preview_pending = true;
        let (owner, id, channel) = (self.owner, self.preview_req, ctx.channel.clone());
        let (toml, reference) = (self.draft.text().to_string(), self.target_ref());
        ctx.spawn_async(async move {
            let r = crate::rpc::preview_pool(channel, toml, reference, SAMPLE).await;
            Ok(Control::Event(AppEvent::Playlists(Box::new(PlEvent::Preview(owner, id, r)))))
        });
    }

    fn save(&mut self, ctx: &mut Global) {
        if self.saving {
            return;
        }
        let reference = self.target_ref();
        if reference.is_empty() {
            self.focus = Target::Ref;
            self.bind_input();
            self.message = Some((tr!("ed-ref-required"), true));
            return;
        }
        self.saving = true;
        self.message = Some((tr!("ed-saving"), false));
        let (owner, channel) = (self.owner, ctx.channel.clone());
        let (toml, revision) = (self.draft.text().to_string(), self.revision.clone());
        ctx.spawn_async(async move {
            let r = crate::rpc::save_playlist(channel, reference, toml, revision).await;
            Ok(Control::Event(AppEvent::Playlists(Box::new(PlEvent::Saved(owner, r)))))
        });
    }

    fn fetch_file(&mut self, ctx: &mut Global, compare: bool) {
        let (owner, channel, reference) = (self.owner, ctx.channel.clone(), self.target_ref());
        ctx.spawn_async(async move {
            let r = crate::rpc::export_playlist(channel, reference).await;
            let ev = if compare { PlEvent::Compare(owner, r) } else { PlEvent::Reloaded(owner, r) };
            Ok(Control::Event(AppEvent::Playlists(Box::new(ev))))
        });
    }

    fn on_saved(&mut self, r: &Result<SaveResponse, String>, ctx: &mut Global) -> Outcome {
        self.saving = false;
        match r {
            Err(e) => {
                self.message = Some((tr!("ed-save-failed", reason = e.clone()), true));
                Outcome::Stay
            }
            Ok(r) if r.conflict => {
                self.message = Some((tr!("ed-conflict-short"), true));
                let mut b = ChoiceBox::new(
                    tr!("ed-conflict-title"),
                    vec![tr!("ed-conflict-body", file = r.file.clone()), tr!("ed-conflict-never")],
                    vec![tr!("ed-conflict-keep"), tr!("ed-conflict-compare"), tr!("ed-conflict-reload")],
                );
                b.danger = true;
                self.overlay = Some(Overlay::Conflict(b));
                Outcome::Stay
            }
            Ok(r) if !r.ok => {
                self.diags = r.diagnostics.clone();
                let n = self.diags.iter().filter(|d| is_error(d)).count();
                self.message = Some((tr!("ed-not-saved", n = n), true));
                self.focus_diag(0);
                Outcome::Stay
            }
            Ok(r) => {
                let file = r.file.clone();
                self.reference = file.strip_suffix(".toml").unwrap_or(&file).to_string();
                self.is_new = false;
                self.revision = r.revision.clone();
                self.draft = Draft::parse(&r.toml);
                self.saved_text = r.toml.clone();
                self.sync_raw();
                self.diags = r.diagnostics.clone();
                self.message = Some((
                    if r.created { tr!("ed-created", file = file) } else { tr!("ed-saved", file = file) },
                    false,
                ));
                self.structure_changed(ctx);
                Outcome::Saved
            }
        }
    }

    /// Met le focus sur la ligne du `n`-ième diagnostic qui en a une.
    fn focus_diag(&mut self, n: usize) {
        let rows = self.rows();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        let mut hits: Vec<Target> = Vec::new();
        let mut ordered: Vec<&Diagnostic> = self.diags.iter().filter(|d| is_error(d)).collect();
        ordered.extend(self.diags.iter().filter(|d| !is_error(d)));
        for d in ordered {
            if let Some(t) = diag_row(&paths, &d.field_path).and_then(|i| rows[i].target)
                && !hits.contains(&t)
            {
                hits.push(t);
            }
        }
        if hits.is_empty() {
            return;
        }
        let at = hits.iter().position(|t| *t == self.focus);
        let pick = match at {
            Some(i) if n > 0 => (i + n) % hits.len(),
            _ => 0,
        };
        self.focus = hits[pick];
        if let Some(r) = rows.iter().find(|r| r.target == Some(self.focus))
            && !r.focusable()
        {
            return;
        }
        self.bind_input();
    }

    fn sync_raw(&mut self) {
        if let Some(raw) = self.raw.as_mut() {
            raw.set_text(self.draft.text());
        }
    }

    fn set_choice(&mut self, delta: isize, ctx: &mut Global) {
        let rows = self.rows();
        let Some(row) = rows.iter().find(|r| r.target == Some(self.focus)) else { return };
        let Kind::Choice(opts) = &row.kind else { return };
        let n = opts.len() as isize;
        if n == 0 {
            return;
        }
        let at = opts.iter().position(|o| o.1 == row.value).unwrap_or(0) as isize;
        let next = &opts[((at + delta) % n + n) as usize % n as usize].1;
        match self.focus {
            Target::Key(k) => self.draft.set(k, next),
            Target::Filter(i, part) => self.draft.set_filter(i, part, next),
            _ => return,
        }
        self.structure_changed(ctx);
    }

    fn apply_text(&mut self, v: &str, ctx: &mut Global) {
        match self.focus {
            Target::Ref => {
                self.ref_text = v.to_string();
                self.suggest_ref = false;
            },
            Target::Key(k) => self.draft.set(k, v),
            Target::Filter(i, part) => self.draft.set_filter(i, part, v),
            Target::Member(i, part) => self.draft.set_member(i, part, v),
            Target::File(i) => self.draft.set_file(i, v),
            _ => return,
        }
        self.sync_raw();
        self.changed(ctx);
    }

    /// `Ctrl+N` / Entrée sur une ligne « ajouter ».
    fn add(&mut self, ctx: &mut Global) {
        match self.focus {
            Target::Filter(..) | Target::AddFilter => {
                self.draft.add_filter();
                self.focus = Target::Filter(self.draft.filters().len() - 1, FilterPart::Field);
                self.structure_changed(ctx);
            }
            Target::Member(..) | Target::AddMember => {
                let mut p = PlaylistPicker::new(tr!("ed-pick-member"), vec![], false);
                p.set_items(Ok(self.playlists.clone()));
                self.overlay = Some(Overlay::Members(Box::new(p)));
            }
            Target::File(_) | Target::AddFiles => {
                let mut m = Medias::new(true);
                m.ensure_loaded(ctx);
                self.overlay = Some(Overlay::Media(Box::new(m)));
            }
            _ => match self.draft.mode().as_str() {
                "dynamic" => {
                    self.focus = Target::AddFilter;
                    self.add(ctx);
                }
                "group" => {
                    self.focus = Target::AddMember;
                    self.add(ctx);
                }
                "static" => {
                    self.focus = Target::AddFiles;
                    self.add(ctx);
                }
                _ => {}
            },
        }
    }

    fn remove(&mut self, ctx: &mut Global) {
        match self.focus {
            Target::Filter(i, _) => self.draft.remove_filter(i),
            Target::Member(i, _) => self.draft.remove_member(i),
            Target::File(i) => self.draft.remove_file(i),
            _ => return,
        }
        self.structure_changed(ctx);
    }

    fn move_item(&mut self, up: bool, ctx: &mut Global) {
        self.focus = match self.focus {
            Target::Filter(i, p) => match self.draft.move_filter(i, up) {
                Some(j) => Target::Filter(j, p),
                None => return,
            },
            Target::Member(i, p) => match self.draft.move_member(i, up) {
                Some(j) => Target::Member(j, p),
                None => return,
            },
            Target::File(i) => match self.draft.move_file(i, up) {
                Some(j) => Target::File(j),
                None => return,
            },
            _ => return,
        };
        self.structure_changed(ctx);
    }

    fn close_or_ask(&mut self) -> Outcome {
        if !self.dirty() {
            return Outcome::Close;
        }
        self.overlay = Some(Overlay::Discard(ChoiceBox::new(
            tr!("ed-discard-title"),
            vec![tr!("ed-discard-body")],
            vec![tr!("ed-discard-keep"), tr!("ed-discard-yes")],
        )));
        Outcome::Stay
    }

    // --- événements -------------------------------------------------------------

    /// Réponses et minuteries adressées à cet éditeur.
    pub fn on_event(&mut self, ev: &PlEvent, ctx: &mut Global) -> Option<Outcome> {
        match ev {
            PlEvent::Typed(o, id) if *o == self.owner => {
                if *id == self.typing {
                    self.run_preview(ctx);
                }
            }
            PlEvent::Preview(o, id, r) if *o == self.owner => {
                if *id == self.preview_req {
                    self.preview_pending = false;
                    if let Ok(p) = r {
                        self.diags = p.diagnostics.clone();
                    }
                    self.preview = Some(r.clone());
                }
            }
            PlEvent::Saved(o, r) if *o == self.owner => return Some(self.on_saved(r, ctx)),
            PlEvent::Genres(o, r) if *o == self.owner => {
                if let Ok(g) = r {
                    self.genres = g.genres.clone();
                }
            }
            PlEvent::Compare(o, r) if *o == self.owner => match r {
                Ok(x) => {
                    self.overlay =
                        Some(Overlay::Compare { file: x.file_toml.clone(), revision: x.revision.clone(), scroll: 0 })
                }
                Err(e) => self.message = Some((e.clone(), true)),
            },
            PlEvent::Reloaded(o, r) if *o == self.owner => match r {
                Ok(x) => {
                    let text = if x.file_toml.is_empty() { x.applied_toml.clone() } else { x.file_toml.clone() };
                    self.draft = Draft::parse(&text);
                    self.saved_text = text;
                    self.revision = x.revision.clone();
                    self.is_new = false;
                    self.reference = x.rel_path.clone();
                    self.sync_raw();
                    self.message = Some((tr!("ed-reloaded"), false));
                    self.structure_changed(ctx);
                }
                Err(e) => self.message = Some((e.clone(), true)),
            },
            _ => return None,
        }
        Some(Outcome::Stay)
    }

    /// Recherche de médias du sélecteur ouvert (réponses, frappe).
    pub fn on_media_event(&mut self, ev: &AppEvent, ctx: &mut Global) -> bool {
        if let Some(Overlay::Media(m)) = self.overlay.as_mut() {
            return m.handle(ev, ctx) != Control::Continue;
        }
        false
    }

    pub fn on_key(&mut self, e: &Event, ctx: &mut Global) -> Outcome {
        // Recouvrements d'abord : ils capturent tout.
        if let Some(ov) = self.overlay.as_mut() {
            match ov {
                Overlay::Conflict(b) => match b.handle(e) {
                    Choice::Picked(0) | Choice::Cancel => self.overlay = None,
                    Choice::Picked(1) => {
                        self.overlay = None;
                        self.fetch_file(ctx, true);
                    }
                    Choice::Picked(_) => {
                        self.overlay = None;
                        self.fetch_file(ctx, false);
                    }
                    _ => {}
                },
                Overlay::Discard(b) => match b.handle(e) {
                    Choice::Picked(1) => return Outcome::Close,
                    Choice::Picked(_) | Choice::Cancel => self.overlay = None,
                    _ => {}
                },
                Overlay::Compare { scroll, .. } => {
                    if let Event::Key(k) = e
                        && k.kind == KeyEventKind::Press
                    {
                        match k.code {
                            KeyCode::Esc | KeyCode::Enter => self.overlay = None,
                            KeyCode::Up => *scroll = scroll.saturating_sub(1),
                            KeyCode::Down => *scroll += 1,
                            KeyCode::PageUp => *scroll = scroll.saturating_sub(15),
                            KeyCode::PageDown => *scroll += 15,
                            _ => {}
                        }
                    }
                }
                Overlay::Media(m) => {
                    let _ = m.handle(&AppEvent::Event(e.clone()), ctx);
                    match m.take_picked() {
                        Some(PickedMedia::Chosen(files)) => {
                            self.overlay = None;
                            self.add_files(&files, ctx);
                            self.sync_raw();
                        }
                        Some(PickedMedia::Cancel) => self.overlay = None,
                        None => {}
                    }
                }
                Overlay::Members(p) => match p.handle(e) {
                    PickedPlaylist::Chosen(r) => {
                        self.overlay = None;
                        self.draft.add_member(&r);
                        self.focus = Target::Member(self.draft.members().len() - 1, MemberPart::Ref);
                        self.sync_raw();
                        self.structure_changed(ctx);
                    }
                    PickedPlaylist::Cancel => self.overlay = None,
                    _ => {}
                },
            }
            return Outcome::Stay;
        }

        let Event::Key(k) = e else { return Outcome::Stay };
        if k.kind != KeyEventKind::Press {
            return Outcome::Stay;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        // Touches communes aux deux vues.
        match k.code {
            KeyCode::Char('s') if ctrl => {
                self.save(ctx);
                return Outcome::Stay;
            }
            KeyCode::Char('t') if ctrl => {
                self.toggle_raw();
                return Outcome::Stay;
            }
            KeyCode::F(2) => {
                self.toggle_raw();
                return Outcome::Stay;
            }
            KeyCode::F(8) => {
                if self.raw.is_none() {
                    self.focus_diag(1);
                }
                return Outcome::Stay;
            }
            KeyCode::Esc if self.raw.is_some() => {
                self.toggle_raw();
                return Outcome::Stay;
            }
            KeyCode::Esc => return self.close_or_ask(),
            _ => {}
        }

        if let Some(raw) = self.raw.as_mut() {
            if raw.handle(e, Regular) == TextOutcome::TextChanged {
                let text = raw.text();
                self.draft.set_text(&text);
                self.input_for = None;
                self.changed(ctx);
            }
            return Outcome::Stay;
        }
        if !self.draft.readable() {
            return Outcome::Stay;
        }

        let rows = self.rows();
        let kind = rows.iter().find(|r| r.target == Some(self.focus)).map(|r| r.kind.clone());
        match k.code {
            KeyCode::Char('n') if ctrl => self.add(ctx),
            KeyCode::Char('d') if ctrl => self.remove(ctx),
            KeyCode::Up if alt || ctrl => self.move_item(true, ctx),
            KeyCode::Down if alt || ctrl => self.move_item(false, ctx),
            KeyCode::Tab | KeyCode::Down => self.move_focus(1),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(-1),
            KeyCode::PageDown => self.move_focus(10),
            KeyCode::PageUp => self.move_focus(-10),
            KeyCode::Enter => match kind {
                Some(Kind::Action) => self.add(ctx),
                _ => self.move_focus(1),
            },
            _ => match kind {
                Some(Kind::Choice(_)) => match k.code {
                    KeyCode::Left => self.set_choice(-1, ctx),
                    KeyCode::Right | KeyCode::Char(' ') => self.set_choice(1, ctx),
                    _ => {}
                },
                Some(Kind::Text) => self.type_text(e, k, ctx),
                _ => {}
            },
        }
        Outcome::Stay
    }

    fn type_text(&mut self, e: &Event, k: &KeyEvent, ctx: &mut Global) {
        // Ctrl+… n'est pas du texte (sauf l'édition de ligne).
        if k.modifiers.contains(KeyModifiers::CONTROL) && !matches!(k.code, KeyCode::Char('a' | 'e' | 'u' | 'k' | 'w')) {
            return;
        }
        self.bind_input();
        let before = self.input.text().to_string();
        self.input.handle(e, Regular);
        let after = self.input.text().to_string();
        if after != before {
            self.apply_text(&after, ctx);
        }
    }

    fn toggle_raw(&mut self) {
        match self.raw.take() {
            Some(_) => {
                self.input_for = None;
                if self.draft.readable() {
                    self.settle_focus();
                }
            }
            None => {
                let mut st = TextAreaState::new();
                st.set_text(self.draft.text());
                st.focus.set(true);
                self.input.focus.set(false);
                self.raw = Some(st);
            }
        }
    }

    // --- rendu --------------------------------------------------------------------

    /// Ligne du TOML qui porte la ligne `t` du formulaire (repère visuel).
    fn toml_line(&self, t: Target) -> Option<usize> {
        let lines: Vec<&str> = self.draft.text().lines().collect();
        let is_key = |l: &str, key: &str| {
            let l = l.trim_start();
            l.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='))
        };
        let section = |name: &str| -> Option<usize> {
            if name.is_empty() {
                return Some(0);
            }
            let h = format!("[{name}]");
            lines.iter().position(|l| l.trim() == h)
        };
        let in_section = |name: &str, key: &str| -> Option<usize> {
            let start = section(name)?;
            let from = if name.is_empty() { 0 } else { start + 1 };
            lines
                .iter()
                .enumerate()
                .skip(from)
                .take_while(|(i, l)| name.is_empty() && *i < usize::MAX || !l.trim_start().starts_with('['))
                .find(|(_, l)| is_key(l, key))
                .map(|(i, _)| i)
        };
        match t {
            Target::Key(k) => {
                let path = k.path();
                let (table, key) = path.rsplit_once('.').unwrap_or(("", path.as_str()));
                in_section(table, key)
            }
            Target::Filter(i, _) => {
                lines.iter().enumerate().filter(|(_, l)| l.trim() == "[[selection.filter]]").nth(i).map(|(n, _)| n)
            }
            Target::Member(i, _) => {
                let start = lines.iter().position(|l| is_key(l, "members"))?;
                lines.iter().enumerate().skip(start).filter(|(_, l)| l.contains("ref")).nth(i).map(|(n, _)| n)
            }
            Target::File(i) => {
                let f = self.draft.files().get(i)?.clone();
                lines.iter().position(|l| l.contains(&format!("\"{f}\"")))
            }
            _ => None,
        }
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) {
        let s = Styles(&ctx.theme);
        Block::new().style(s.base()).render(area, buf);
        let [top_a, body_a] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        self.render_top(top_a, buf, &s);

        // Hauteur des diagnostics : leurs lignes une fois coupées à la largeur
        // de leur cadre (la moitié droite, ou tout en étroit).
        let diag_w = if body_a.width >= 110 { body_a.width / 2 } else { body_a.width }.saturating_sub(2) as usize;
        let n_diags: u16 = self
            .diags
            .iter()
            .map(|d| crate::dialog::wrapped_lines(&format!("✗ {} : {}", d.field_path, diag_text(d)), diag_w) as u16)
            .sum();
        let cursor;
        if body_a.width >= 110 {
            let [left, right] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body_a);
            let pool_h = 9.min(left.height / 2);
            let [form_a, pool_a] = Layout::vertical([Constraint::Fill(1), Constraint::Length(pool_h)]).areas(left);
            let diag_h = (n_diags + 2).clamp(3, 10).min(right.height / 2);
            let [toml_a, diag_a] = Layout::vertical([Constraint::Fill(1), Constraint::Length(diag_h)]).areas(right);
            let c1 = self.render_form(form_a, buf, ctx);
            let c2 = self.render_toml(toml_a, buf, ctx);
            cursor = c1.or(c2);
            self.render_diags(diag_a, buf, &s);
            self.render_pool(pool_a, buf, &s);
        } else {
            let diag_h = (n_diags + 2).clamp(3, 6);
            let pool_h = if body_a.height < 24 { 4 } else { 6 };
            let [main_a, diag_a, pool_a] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(diag_h), Constraint::Length(pool_h)]).areas(body_a);
            cursor = if self.raw.is_some() { self.render_toml(main_a, buf, ctx) } else { self.render_form(main_a, buf, ctx) };
            self.render_diags(diag_a, buf, &s);
            self.render_pool(pool_a, buf, &s);
        }
        ctx.set_screen_cursor(cursor);

        let s = Styles(&ctx.theme);
        match self.overlay.as_mut() {
            Some(Overlay::Conflict(b)) | Some(Overlay::Discard(b)) => b.render(area, buf, &s),
            Some(Overlay::Compare { file, revision, scroll }) => {
                let (file, revision) = (file.clone(), revision.clone());
                let mut sc = *scroll;
                self.render_compare(area, buf, &s, &file, &revision, &mut sc);
                if let Some(Overlay::Compare { scroll, .. }) = self.overlay.as_mut() {
                    *scroll = sc;
                }
            }
            Some(Overlay::Media(m)) => {
                let w = area.width.saturating_sub(4);
                let h = area.height.saturating_sub(2);
                let box_a = centered(area, w, h);
                Clear.render(box_a, buf);
                let block = frame(&tr!("ed-pick-media"), &s, false);
                let inner = block.inner(box_a);
                block.style(s.base()).render(box_a, buf);
                ctx.set_screen_cursor(None);
                m.draw(inner, buf, ctx);
            }
            Some(Overlay::Members(p)) => {
                let c = p.render(area, buf, &ctx.theme);
                ctx.set_screen_cursor(c);
            }
            None => {}
        }
    }

    fn render_top(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let who = if self.is_new {
            tr!("ed-title-new", reference = if self.ref_text.trim().is_empty() { "…".into() } else { self.ref_text.trim().to_string() })
        } else {
            tr!("ed-title", reference = self.reference.clone())
        };
        let mut spans = vec![Span::styled(format!(" {who} "), s.title())];
        if self.dirty() {
            spans.push(Span::styled(format!("● {} ", tr!("ed-modified")), s.warn()));
        }
        if !self.revision.is_empty() {
            let short: String = self.revision.trim_start_matches("sha256:").chars().take(8).collect();
            spans.push(Span::styled(format!("{} ", tr!("ed-revision", rev = short)), s.muted()));
        }
        if self.raw.is_some() {
            spans.push(Span::styled(format!("[{}] ", tr!("ed-raw-mode")), s.accent()));
        }
        if let Some((m, err)) = &self.message {
            spans.push(Span::styled(format!("· {m}"), if *err { s.error() } else { s.ok() }));
        }
        Paragraph::new(fit::segments(vec![spans], Span::raw(""), area.width as usize)).render(area, buf);
    }

    fn render_form(&mut self, area: Rect, buf: &mut Buffer, ctx: &Global) -> Option<(u16, u16)> {
        let s = Styles(&ctx.theme);
        let focused = self.raw.is_none() && self.overlay.is_none();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(if focused { s.accent() } else { s.border() })
            .title(Span::styled(format!(" {} ", tr!("ed-form")), s.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        if !self.draft.readable() {
            Paragraph::new(vec![Line::styled(tr!("ed-unreadable"), s.error()), Line::styled(tr!("ed-unreadable-hint"), s.muted())])
                .wrap(Wrap { trim: true })
                .render(inner, buf);
            return None;
        }
        let rows = self.rows();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        // Diagnostics par ligne.
        let mut per_row: Vec<Vec<&Diagnostic>> = vec![Vec::new(); rows.len()];
        for d in &self.diags {
            if let Some(i) = diag_row(&paths, &d.field_path) {
                per_row[i].push(d);
            }
        }
        // Lignes affichées : une par ligne du formulaire, plus, sous la ligne
        // qui a le focus, ses diagnostics et les genres proposés.
        let label_w = (inner.width as usize / 3).clamp(12, 26);
        let mut lines: Vec<(Line, Option<usize>)> = Vec::new();
        let mut focus_line = 0;
        let mut focus_tail = 0;
        for (i, r) in rows.iter().enumerate() {
            let is_focus = focused && r.target == Some(self.focus);
            if r.kind == Kind::Header {
                if i > 0 {
                    lines.push((Line::default(), None));
                }
                lines.push((Line::styled(format!(" {}", r.label), s.title()), None));
                continue;
            }
            let mark = match per_row[i].iter().any(|d| is_error(d)) {
                true => Span::styled("✗", s.error()),
                false if !per_row[i].is_empty() => Span::styled("⚠", s.warn()),
                false => Span::raw(" "),
            };
            let label = Span::styled(
                format!(" {:<w$} ", fit::ellipsize(&r.label, label_w), w = label_w),
                if is_focus { s.accent() } else { s.label() },
            );
            let value = match &r.kind {
                Kind::Choice(opts) => {
                    let l = opts.iter().find(|o| o.1 == r.value).map(|o| o.0.clone()).unwrap_or_else(|| r.value.clone());
                    if is_focus {
                        Span::styled(format!("◀ {l} ▶"), s.tab_active())
                    } else {
                        Span::raw(l)
                    }
                }
                Kind::Action => Span::styled(format!("＋ {}", r.label), if is_focus { s.tab_active() } else { s.accent() }),
                Kind::Note => Span::styled(r.value.clone(), s.muted()),
                Kind::Text if r.value.is_empty() && !is_focus => Span::styled("—", s.muted()),
                _ => Span::raw(r.value.clone()),
            };
            if is_focus {
                focus_line = lines.len();
            }
            let line = if r.kind == Kind::Action {
                Line::from(vec![mark, Span::raw(" "), value])
            } else {
                Line::from(vec![mark, label, value])
            };
            lines.push((line, if is_focus && r.kind == Kind::Text { Some(label_w + 3) } else { None }));
            if is_focus {
                let before = lines.len();
                for d in &per_row[i] {
                    let st = if is_error(d) { s.error() } else { s.warn() };
                    lines.push((Line::styled(format!("   ↳ {}", diag_text(d)), st), None));
                }
                if let Target::Filter(fi, FilterPart::Value) = self.focus
                    && self.draft.filters().get(fi).is_some_and(|f| f.field == "genre")
                {
                    // Tous les genres proposés, sur autant de lignes qu'il faut.
                    for l in wrap(&self.genre_hint(), (inner.width as usize).saturating_sub(3)) {
                        lines.push((Line::styled(format!("   {l}"), s.muted()), None));
                    }
                }
                focus_tail = lines.len() - before;
            }
        }
        // La ligne qui a le focus et ce qui la suit (diagnostics, genres)
        // restent visibles, le focus d'abord si tout ne tient pas.
        let h = inner.height as usize;
        let tail = focus_tail;
        if focus_line < self.form_scroll {
            self.form_scroll = focus_line.saturating_sub(1);
        } else if focus_line + 1 + tail > self.form_scroll + h {
            self.form_scroll = (focus_line + 1 + tail).saturating_sub(h).min(focus_line);
        }
        let mut cursor = None;
        for (n, (line, input_x)) in lines.into_iter().skip(self.form_scroll).take(h).enumerate() {
            let row_a = Rect::new(inner.x, inner.y + n as u16, inner.width, 1);
            Paragraph::new(line).render(row_a, buf);
            if let Some(x) = input_x {
                let x = x as u16;
                let field_a = Rect::new(inner.x + x, row_a.y, inner.width.saturating_sub(x), 1);
                Clear.render(field_a, buf);
                let style: rat_widget::text::TextStyle = ctx.theme.style(WidgetStyle::TEXT);
                TextInput::new().styles(style).render(field_a, buf, &mut self.input);
                cursor = self.input.screen_cursor();
            }
        }
        cursor
    }

    /// Genres connus qui commencent par le mot en cours de frappe (tous).
    fn genre_hint(&self) -> String {
        let typed = self.input.text();
        let last = typed.rsplit(',').next().unwrap_or("").trim().to_lowercase();
        let list: Vec<String> = self
            .genres
            .iter()
            .filter(|g| last.is_empty() || g.genre.to_lowercase().starts_with(&last))
            .map(|g| format!("{} ({})", g.genre, g.count))
            .collect();
        if list.is_empty() { tr!("ed-genres-none") } else { tr!("ed-genres", list = list.join(", ")) }
    }

    fn render_toml(&mut self, area: Rect, buf: &mut Buffer, ctx: &Global) -> Option<(u16, u16)> {
        let s = Styles(&ctx.theme);
        let editing = self.raw.is_some() && self.overlay.is_none();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(if editing { s.accent() } else { s.border() })
            .title(Span::styled(format!(" {} ", tr!("ed-toml")), s.title()))
            .title_bottom(Span::styled(
                format!(" {} ", if self.raw.is_some() { tr!("ed-toml-keys-raw") } else { tr!("ed-toml-keys") }),
                s.muted(),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        if let Some(raw) = self.raw.as_mut() {
            let style: rat_widget::text::TextStyle = ctx.theme.style(WidgetStyle::TEXTAREA);
            TextArea::new().styles(style).render(inner, buf, raw);
            return if editing { raw.screen_cursor() } else { None };
        }
        let text = self.draft.text().to_string();
        let lines: Vec<&str> = text.lines().collect();
        let mark = if self.overlay.is_none() { self.toml_line(self.focus) } else { None };
        let h = inner.height as usize;
        if let Some(m) = mark {
            if m < self.toml_scroll {
                self.toml_scroll = m;
            } else if m >= self.toml_scroll + h {
                self.toml_scroll = m + 1 - h;
            }
        }
        self.toml_scroll = self.toml_scroll.min(lines.len().saturating_sub(h));
        let gutter = lines.len().to_string().len();
        let out: Vec<Line> = lines
            .iter()
            .enumerate()
            .skip(self.toml_scroll)
            .take(h)
            .map(|(i, l)| {
                let st = if Some(i) == mark {
                    s.tab_active()
                } else if l.trim_start().starts_with('#') {
                    s.muted()
                } else if l.trim_start().starts_with('[') {
                    s.accent()
                } else {
                    Style::default()
                };
                Line::from(vec![Span::styled(format!("{:>gutter$} ", i + 1), s.muted()), Span::styled(l.to_string(), st)])
            })
            .collect();
        Paragraph::new(out).render(inner, buf);
        None
    }

    fn render_diags(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let errors = self.diags.iter().filter(|d| is_error(d)).count();
        let title = if self.diags.is_empty() {
            tr!("ed-diags-none")
        } else {
            tr!("ed-diags", errors = errors, warnings = self.diags.len() - errors)
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(if errors > 0 { s.error() } else { s.border() })
            .title(Span::styled(format!(" {title} "), if errors > 0 { s.error() } else { s.title() }));
        let inner = block.inner(area);
        block.render(area, buf);
        let rows = self.rows();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        let me = self.target_ref();
        let mut ordered: Vec<&Diagnostic> = self.diags.iter().filter(|d| is_error(d)).collect();
        ordered.extend(self.diags.iter().filter(|d| !is_error(d)));
        let lines: Vec<Line> = ordered
            .into_iter()
            .map(|d| {
                let (sym, st) = if is_error(d) { ("✗", s.error()) } else { ("⚠", s.warn()) };
                let label = match diag_row(&paths, &d.field_path) {
                    Some(i) => rows[i].label.trim().to_string(),
                    None if d.field_path.is_empty() => tr!("ed-diag-file"),
                    None => d.field_path.clone(),
                };
                let file = if !d.file.is_empty() && d.file != me { format!("[{}] ", d.file) } else { String::new() };
                Line::from(vec![
                    Span::styled(format!("{sym} "), st),
                    Span::styled(format!("{file}{} ", tr!("ed-diag-label", label = label)), s.label()),
                    Span::styled(diag_text(d), st),
                ])
            })
            .collect();
        if lines.is_empty() {
            Paragraph::new(Span::styled(tr!("ed-diags-ok"), s.ok())).render(inner, buf);
        } else {
            Paragraph::new(lines).wrap(Wrap { trim: false }).render(inner, buf);
        }
    }

    fn render_pool(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(s.border())
            .title(Span::styled(format!(" {} ", tr!("ed-pool")), s.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        let mut lines = Vec::new();
        if self.preview_pending {
            lines.push(Line::styled(tr!("ed-pool-pending"), s.muted()));
        }
        match &self.preview {
            None => {}
            Some(Err(e)) => lines.push(Line::styled(e.clone(), s.error())),
            Some(Ok(p)) if !p.ok => lines.push(Line::styled(tr!("ed-pool-invalid"), s.warn())),
            Some(Ok(p)) => match p.count {
                None => lines.push(Line::styled(tr!("ed-pool-unmeasured"), s.muted())),
                Some(0) => lines.push(Line::styled(tr!("ed-pool-empty"), s.error())),
                Some(n) => {
                    let mut head = tr!("ed-pool-count", n = n);
                    if let Some(ms) = p.duration_ms {
                        head.push_str(&format!(" · {}", crate::store::human_duration(Duration::from_millis(ms))));
                    }
                    if let Some(a) = p.artists {
                        head.push_str(&format!(" · {}", tr!("ed-pool-artists", n = a)));
                    }
                    lines.push(Line::styled(head, s.ok()));
                    for m in &p.members {
                        let what = match (m.count, m.error.is_empty()) {
                            (_, false) => Span::styled(m.error.clone(), s.error()),
                            (Some(0), _) => Span::styled(tr!("ed-pool-member-empty"), s.error()),
                            (Some(n), _) => Span::raw(format!(
                                "{} · {}",
                                tr!("ed-pool-count", n = n),
                                m.duration_ms.map(|d| crate::store::human_duration(Duration::from_millis(d))).unwrap_or_default()
                            )),
                            (None, _) => Span::styled(tr!("ed-pool-unmeasured-short"), s.muted()),
                        };
                        let name = if m.resolved.is_empty() || m.resolved == m.r#ref { m.r#ref.clone() } else { format!("{} → {}", m.r#ref, m.resolved) };
                        lines.push(Line::from(vec![Span::styled(format!("  {name} : "), s.label()), what]));
                    }
                    for m in &p.sample {
                        let who = match (m.artist.is_empty(), m.title.is_empty()) {
                            (false, false) => format!("{} — {}", m.artist, m.title),
                            (true, false) => m.title.clone(),
                            _ => m.rel_path.clone(),
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!("  {:>6} ", mmss(m.duration_ms)), s.muted()),
                            Span::raw(fit::ellipsize(&who, (inner.width as usize).saturating_sub(10))),
                        ]));
                    }
                }
            },
        }
        Paragraph::new(lines).render(inner, buf);
    }

    fn render_compare(&self, area: Rect, buf: &mut Buffer, s: &Styles, file: &str, revision: &str, scroll: &mut usize) {
        let box_a = centered(area, area.width.saturating_sub(2), area.height.saturating_sub(1));
        Clear.render(box_a, buf);
        let block = frame(&tr!("ed-compare-title"), s, true)
            .title_bottom(Span::styled(format!(" {} ", tr!("ed-compare-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let [l, _, r] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(2), Constraint::Fill(1)]).areas(inner);
        let short: String = revision.trim_start_matches("sha256:").chars().take(8).collect();
        let left: Vec<&str> = file.lines().collect();
        let draft = self.draft.text().to_string();
        let right: Vec<&str> = draft.lines().collect();
        let h = inner.height.saturating_sub(1) as usize;
        *scroll = (*scroll).min(left.len().max(right.len()).saturating_sub(h));
        let col = |a: Rect, title: String, mine: &[&str], other: &[&str], buf: &mut Buffer| {
            let [t, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(a);
            Paragraph::new(Span::styled(title, s.title())).render(t, buf);
            let lines: Vec<Line> = mine
                .iter()
                .skip(*scroll)
                .take(h)
                .map(|l| {
                    let st = if other.contains(l) { Style::default() } else { s.warn() };
                    Line::styled(l.to_string(), st)
                })
                .collect();
            Paragraph::new(lines).render(body, buf);
        };
        col(l, tr!("ed-compare-disk", rev = short), &left, &right, buf);
        col(r, tr!("ed-compare-draft"), &right, &left, buf);
    }

    pub fn keys(&self) -> &'static [KeyHelp] {
        match &self.overlay {
            Some(Overlay::Media(m)) => return m.keys(),
            Some(Overlay::Members(_)) => {
                return &[(k!("key-enter"), k!("help-ed-add")), (k!("key-esc"), k!("help-close"))];
            }
            Some(_) => return &[(k!("key-esc"), k!("help-close"))],
            None => {}
        }
        if self.raw.is_some() {
            &[
                (k!("key-ctrl-s"), k!("help-ed-save")),
                (k!("key-ctrl-t"), k!("help-ed-form")),
                (k!("key-esc"), k!("help-ed-form")),
            ]
        } else {
            &[
                (k!("key-ctrl-s"), k!("help-ed-save")),
                (k!("key-tab"), k!("help-ed-next")),
                (k!("key-left-right"), k!("help-ed-choice")),
                (k!("key-ctrl-n"), k!("help-ed-add")),
                (k!("key-ctrl-d"), k!("help-ed-remove")),
                (k!("key-alt-updown"), k!("help-ed-move")),
                (k!("key-ctrl-t"), k!("help-ed-raw")),
                (k!("key-f8"), k!("help-ed-next-diag")),
                (k!("key-esc"), k!("help-close")),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_files_are_safe_and_avoid_existing_refs() {
        assert_eq!(suggested_filename("  Ma Playlist / Rock!  ", &[]), "ma-playlist-rock.toml");
        assert_eq!(suggested_filename("Été.toml", &[]), "été.toml");
        assert_eq!(suggested_filename("../", &[]), "");
        let playlists = ["Rock", "rock-2.toml", "shows/rock-3"].map(|reference| PlaylistSummary {
            rel_path: reference.into(),
            ..Default::default()
        });
        assert_eq!(suggested_filename("Rock", &playlists), "rock-3.toml");
    }
    #[test]
    fn a_diagnostic_lands_on_its_row_or_the_nearest_one() {
        let paths = [
            "",
            "name",
            "selection.mode",
            "selection.filter[1].field",
            "selection.filter[1].op",
            "selection.filter[1].value",
            "selection.filter",
            "selection.members",
        ];
        assert_eq!(diag_row(&paths, "selection.filter[1].value"), Some(5), "exact");
        assert_eq!(diag_row(&paths, "selection.filter[1]"), Some(3), "sous ce chemin : le champ du filtre");
        assert_eq!(diag_row(&paths, "selection.filter"), Some(6), "exact avant « sous »");
        assert_eq!(diag_row(&paths, "selection.filter[1].value.x"), Some(5), "au-dessus");
        assert_eq!(diag_row(&paths, "selection"), Some(2), "première ligne de la sélection");
        assert_eq!(diag_row(&paths, "broadcast.limit"), None);
        assert_eq!(diag_row(&paths, ""), None, "le fichier entier : pas de ligne");
    }

    #[test]
    fn a_long_genre_list_wraps_without_losing_any() {
        let text = (0..40).map(|i| format!("genre{i} ({i})")).collect::<Vec<_>>().join(", ");
        let lines = wrap(&text, 30);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| Span::raw(l.as_str()).width() <= 30), "{lines:?}");
        assert_eq!(lines.join(" "), text, "rien de perdu");
        let named = wrap("genres : bossa nova (1), big beat (2), drum and bass (3)", 26);
        assert!(named.iter().all(|l| !l.ends_with("bossa") && !l.ends_with("big") && !l.ends_with("drum")), "{named:?}");
    }

    #[test]
    fn a_choice_never_drops_an_unknown_value() {
        let o = options(&["fifo", "lifo"], true, "shuffle");
        assert_eq!(o.first().map(|x| x.1.as_str()), Some(""), "absent en tête");
        assert!(o.iter().any(|x| x.1 == "shuffle"), "valeur actuelle gardée");
        assert_eq!(options(&["a"], false, "").len(), 1);
    }

    #[test]
    fn a_diagnostic_is_translated_by_code_with_its_values() {
        let d = Diagnostic {
            code: Code::BadValue as i32,
            rejected: "lifo".into(),
            expected: "shuffle, sequential".into(),
            message: "english text never shown".into(),
            ..Default::default()
        };
        let t = diag_text(&d);
        assert!(t.contains("lifo") && t.contains("shuffle, sequential"), "{t}");
        assert!(!t.contains("english"), "{t}");
        let syntax = Diagnostic { code: Code::Syntax as i32, message: "line 3, col 2".into(), ..Default::default() };
        assert!(diag_text(&syntax).contains("line 3"), "la position d'une erreur de syntaxe est relayée");
    }
}
