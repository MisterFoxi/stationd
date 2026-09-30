//! Éditeur des tags d'un média (ou d'un lot de médias marqués), écran
//! Médias : titre, artiste, album, année, genres du fichier (`TCON`), tags
//! que `custom-tags` transforme en genres (`Type`…), BPM, tempo et date de
//! création. Les genres se choisissent dans la liste des genres connus
//! (`ListGenres`), une valeur nouvelle peut être ajoutée.
//!
//! Rien n'est jugé ici au-delà de la forme des saisies (année, BPM, date) :
//! stationd valide et écrit (`SetTags`) ; la TUI ne fait que présenter.
//!
//! Un média : les champs partent de ce qui est dans le fichier, seuls ceux
//! qui changent sont envoyés, vide = retiré. Un lot : vide = inchangé, et
//! dans une liste chaque genre est « ajouté », « retiré » ou laissé tel quel
//! sur chaque fichier. `Suppr` sur une liste la vide (dans chaque fichier
//! pour un lot). Le choix des valeurs met en tête celles que portent les
//! fichiers visés (`n/total` pour un lot) : on retire sans chercher.

use std::collections::BTreeMap;

use rat_theme4::WidgetStyle;
use rat_theme4::theme::SalsaTheme;
use rat_widget::event::{HandleEvent, Regular};
use rat_widget::text::HasScreenCursor;
use rat_widget::text_input::{TextInput, TextInputState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui_widgets::clear::Clear;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use stationd_proto::library::{GenreCount, MediaTags};

use crate::action::{Action, ListEdit, TagChanges};
use crate::dialog::{centered, frame};
use crate::style::Styles;
use crate::{fit, tr};

/// Dans un lot, ce qu'on fait d'un genre sur chaque fichier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Add,
    Remove,
}

/// Une liste de genres en cours d'édition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Values {
    /// Un média : la liste telle qu'elle sera.
    Set(Vec<String>),
    /// Un lot : genre (graphie affichée) → ajouter / retirer.
    Marks(BTreeMap<String, (String, Mark)>),
    /// Un lot : la liste vidée dans chaque fichier (`Suppr`).
    Clear,
}

impl Values {
    fn key(v: &str) -> String {
        v.trim().to_lowercase()
    }

    /// État d'un genre : `Some(true)` coché / à ajouter, `Some(false)` à
    /// retirer (lot), `None` rien.
    fn state(&self, v: &str) -> Option<bool> {
        match self {
            Values::Set(l) => l.iter().any(|x| Self::key(x) == Self::key(v)).then_some(true),
            Values::Marks(m) => m.get(&Self::key(v)).map(|(_, mark)| *mark == Mark::Add),
            Values::Clear => None,
        }
    }

    /// Espace : coché ↔ non coché (un média). Lot : une valeur que portent
    /// des fichiers du lot (`present`) va d'abord à « retirer », puis
    /// « ajouter » (la mettre sur tous), puis rien ; une valeur absente va
    /// seulement de rien à « ajouter ».
    fn toggle(&mut self, v: &str, present: bool) {
        let k = Self::key(v);
        match self {
            Values::Set(l) => match l.iter().position(|x| Self::key(x) == k) {
                Some(i) => {
                    l.remove(i);
                }
                None => l.push(v.trim().to_string()),
            },
            Values::Marks(m) => {
                let next = match (m.get(&k).map(|(_, mark)| *mark), present) {
                    (None, true) => Some(Mark::Remove),
                    (None, false) => Some(Mark::Add),
                    (Some(Mark::Remove), _) => Some(Mark::Add),
                    (Some(Mark::Add), _) => None,
                };
                match next {
                    Some(mark) => {
                        m.insert(k, (v.trim().to_string(), mark));
                    }
                    None => {
                        m.remove(&k);
                    }
                }
            }
            // Le sélecteur ne s'ouvre jamais sur une liste vidée (repartie
            // de « inchangé »).
            Values::Clear => {}
        }
    }

    /// Ce qu'on montre dans le formulaire.
    fn summary(&self) -> String {
        match self {
            Values::Set(l) if l.is_empty() => "—".into(),
            Values::Set(l) => l.join(", "),
            Values::Marks(m) if m.is_empty() => tr!("tags-unchanged"),
            Values::Marks(m) => m
                .values()
                .map(|(v, mark)| format!("{}{v}", if *mark == Mark::Add { "+" } else { "−" }))
                .collect::<Vec<_>>()
                .join(", "),
            Values::Clear => tr!("tags-cleared"),
        }
    }

    fn values(&self) -> Vec<String> {
        match self {
            Values::Set(l) => l.clone(),
            Values::Marks(m) => m.values().map(|(v, _)| v.clone()).collect(),
            Values::Clear => Vec::new(),
        }
    }

    /// La modification à envoyer, `None` si rien ne change. `before` : la
    /// liste du fichier (un média).
    fn edit(&self, before: &[String]) -> Option<ListEdit> {
        match self {
            Values::Set(l) => {
                let norm = |x: &[String]| x.iter().map(|v| Self::key(v)).collect::<Vec<_>>();
                (norm(l) != norm(before)).then(|| ListEdit::Replace(l.clone()))
            }
            Values::Marks(m) if m.is_empty() => None,
            Values::Marks(m) => Some(ListEdit::Merge {
                add: m.values().filter(|(_, k)| *k == Mark::Add).map(|(v, _)| v.clone()).collect(),
                remove: m.values().filter(|(_, k)| *k == Mark::Remove).map(|(v, _)| v.clone()).collect(),
            }),
            Values::Clear => Some(ListEdit::Replace(Vec::new())),
        }
    }
}

/// Les valeurs que portent les fichiers visés, avec le nombre de fichiers
/// qui portent chacune (casse ignorée, première graphie gardée, une fois
/// par fichier).
fn tally<'a>(lists: impl Iterator<Item = &'a [String]>) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    for list in lists {
        let mut seen: Vec<String> = Vec::new();
        for v in list.iter().filter(|v| !v.trim().is_empty()) {
            let k = Values::key(v);
            if seen.contains(&k) {
                continue;
            }
            seen.push(k.clone());
            match out.iter_mut().find(|(g, _)| Values::key(g) == k) {
                Some((_, n)) => *n += 1,
                None => out.push((v.trim().to_string(), 1)),
            }
        }
    }
    out
}

/// Choix d'une liste de genres : d'abord les valeurs que portent les
/// fichiers visés, puis les autres genres connus, filtrés en tapant, et la
/// saisie elle-même si elle n'existe pas encore.
struct GenrePicker {
    title: String,
    /// Genre, effectif dans la bibliothèque, nombre de fichiers visés qui le
    /// portent (`None` : aucun).
    pool: Vec<(String, u32, Option<u32>)>,
    /// Nombre de fichiers visés (le compte `n/total` ne se montre qu'en lot).
    total: usize,
    values: Values,
    filter: TextInputState,
    selected: usize,
}

enum PickerOutcome {
    Pending,
    Cancel,
    Done(Values),
}

impl GenrePicker {
    fn new(title: String, known: &[GenreCount], values: Values, present: &[(String, u32)], total: usize) -> Self {
        let mut pool: Vec<(String, u32, Option<u32>)> = known.iter().map(|g| (g.genre.clone(), g.count, None)).collect();
        for (v, n) in present {
            match pool.iter_mut().find(|(g, _, _)| Values::key(g) == Values::key(v)) {
                Some(row) => row.2 = Some(*n),
                None => pool.push((v.clone(), 0, Some(*n))),
            }
        }
        for v in values.values() {
            if !pool.iter().any(|(g, _, _)| Values::key(g) == Values::key(&v)) {
                pool.push((v, 0, None));
            }
        }
        Self::sort(&mut pool);
        let filter = TextInputState::new();
        filter.focus.set(true);
        Self { title, pool, total, values, filter, selected: 0 }
    }

    /// Les valeurs portées d'abord (les plus répandues dans le lot en tête),
    /// puis les autres par ordre alphabétique.
    fn sort(pool: &mut [(String, u32, Option<u32>)]) {
        pool.sort_by(|a, b| {
            b.2.is_some()
                .cmp(&a.2.is_some())
                .then_with(|| b.2.unwrap_or(0).cmp(&a.2.unwrap_or(0)))
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
    }

    /// Lignes visibles : les genres connus qui contiennent la saisie, puis
    /// la saisie elle-même en dernier si elle est nouvelle — le curseur
    /// tombe ainsi d'abord sur un genre existant (« amb » → ambient).
    /// (genre, effectif, nouveau, porté par n fichiers visés).
    fn rows(&self) -> Vec<(String, u32, bool, Option<u32>)> {
        let f = self.filter.text().trim().to_string();
        let needle = f.to_lowercase();
        let mut out: Vec<(String, u32, bool, Option<u32>)> = self
            .pool
            .iter()
            .filter(|(g, _, _)| needle.is_empty() || g.to_lowercase().contains(&needle))
            .map(|(g, n, p)| (g.clone(), *n, false, *p))
            .collect();
        if !f.is_empty() && !self.pool.iter().any(|(g, _, _)| g.to_lowercase() == needle) {
            out.push((f, 0, true, None));
        }
        out
    }

    fn handle(&mut self, e: &Event) -> PickerOutcome {
        if let Event::Key(k) = e
            && k.kind == KeyEventKind::Press
        {
            let n = self.rows().len();
            match k.code {
                KeyCode::Esc => return PickerOutcome::Cancel,
                KeyCode::Enter => return PickerOutcome::Done(self.values.clone()),
                KeyCode::Up => {
                    self.selected = self.selected.saturating_sub(1);
                    return PickerOutcome::Pending;
                }
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(n.saturating_sub(1));
                    return PickerOutcome::Pending;
                }
                KeyCode::Char(' ') | KeyCode::Tab => {
                    if let Some((g, _, new, present)) = self.rows().get(self.selected).cloned() {
                        self.values.toggle(&g, present.is_some());
                        if new {
                            // Le genre créé rejoint la liste.
                            self.pool.push((g.clone(), 0, None));
                            Self::sort(&mut self.pool);
                        }
                        if !self.filter.text().is_empty() {
                            // Filtre vidé pour saisir le genre suivant ; le
                            // curseur reste sur celui qu'on vient de cocher.
                            self.filter.set_text("");
                            self.selected = self.rows().iter().position(|(r, _, _, _)| *r == g).unwrap_or(0);
                        }
                    }
                    return PickerOutcome::Pending;
                }
                _ => {}
            }
        }
        let before = self.filter.text().to_string();
        self.filter.handle(e, Regular);
        if self.filter.text() != before {
            self.selected = 0;
        }
        PickerOutcome::Pending
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &SalsaTheme) -> Option<(u16, u16)> {
        let s = Styles(theme);
        let w = 64.min(area.width.saturating_sub(4)).max(40);
        let h = area.height.saturating_sub(2).clamp(10, 26);
        let box_a = centered(area, w, h);
        Clear.render(box_a, buf);
        let block = frame(&self.title, &s, false).title_bottom(Span::styled(format!(" {} ", tr!("tags-picker-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let [f_a, sel_a, _, list_a] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(2), Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
        let [m_a, i_a] = Layout::horizontal([Constraint::Length(2), Constraint::Fill(1)]).areas(f_a);
        Span::styled("/ ", s.accent()).render(m_a, buf);
        let style: rat_widget::text::TextStyle = theme.style(WidgetStyle::TEXT);
        TextInput::new().styles(style).render(i_a, buf, &mut self.filter);
        Paragraph::new(Span::styled(self.values.summary(), s.title())).wrap(Wrap { trim: true }).render(sel_a, buf);

        let rows = self.rows();
        let hh = list_a.height as usize;
        let start = self.selected.saturating_sub(hh.saturating_sub(1)).min(rows.len().saturating_sub(hh));
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .skip(start)
            .take(hh)
            .map(|(i, (g, n, new, present))| {
                let mark = match self.values.state(g) {
                    Some(true) if matches!(self.values, Values::Marks(_)) => "[+]",
                    Some(true) => "[x]",
                    Some(false) => "[−]",
                    None => "[ ]",
                };
                let text = if *new { tr!("tags-picker-new", genre = g.clone()) } else { g.clone() };
                let st = if i == self.selected { s.tab_active() } else { s.base() };
                let mut spans = vec![Span::styled(format!("{mark} "), s.accent()), Span::styled(text, st)];
                match present {
                    // Lot : combien des fichiers visés la portent.
                    Some(p) if self.total > 1 => spans.push(Span::styled(format!("  ({p}/{})", self.total), s.accent())),
                    _ if *n > 0 => spans.push(Span::styled(format!("  ({n})"), s.muted())),
                    _ => {}
                }
                Line::from(spans)
            })
            .collect();
        if lines.is_empty() {
            Paragraph::new(Span::styled(tr!("tags-picker-none"), s.muted())).render(list_a, buf);
        } else {
            Paragraph::new(lines).render(list_a, buf);
        }
        self.filter.screen_cursor()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Title,
    Artist,
    Album,
    Year,
    Genres,
    Source(usize),
    Bpm,
    Tempo,
    Creation,
}

/// Ce que le formulaire demande à l'écran.
pub enum TagOutcome {
    Pending,
    Cancel,
    /// Écrire : l'action et les lignes de sa confirmation.
    Submit(Box<Action>, Vec<String>),
}

/// Valeur de tempo « inchangé » (lot) : jamais un libellé réel.
const KEEP: &str = "\u{0}keep";

pub struct TagForm {
    /// Lot : les fichiers ; un média : `None`.
    batch: Option<Vec<String>>,
    orig: MediaTags,
    known: Vec<GenreCount>,
    rows: Vec<Row>,
    focus: usize,
    inputs: BTreeMap<u8, TextInputState>,
    genres: Values,
    sources: Vec<(String, Values)>,
    /// Valeurs portées par les fichiers visés : genres, puis chaque source
    /// (même ordre que `sources`).
    present_genres: Vec<(String, u32)>,
    present_sources: Vec<Vec<(String, u32)>>,
    /// Nombre de fichiers visés.
    total: usize,
    tempo_opts: Vec<(String, String)>,
    tempo: usize,
    picker: Option<(Option<usize>, GenrePicker)>,
    error: Option<String>,
}

fn text_id(r: Row) -> Option<u8> {
    Some(match r {
        Row::Title => 0,
        Row::Artist => 1,
        Row::Album => 2,
        Row::Year => 3,
        Row::Bpm => 4,
        Row::Creation => 5,
        _ => return None,
    })
}

impl TagForm {
    /// `t` : les tags du fichier (un média) ou du premier du lot (pour les
    /// noms des tags de genre et les libellés de tempo).
    pub fn new(t: MediaTags, known: Vec<GenreCount>, batch: Option<Vec<String>>) -> Self {
        let single = batch.is_none();
        let mut rows = vec![Row::Title, Row::Artist, Row::Album, Row::Year, Row::Genres];
        rows.extend((0..t.sources.len()).map(Row::Source));
        rows.extend([Row::Bpm, Row::Tempo, Row::Creation]);
        let mut inputs = BTreeMap::new();
        let init = |v: String| {
            let st = TextInputState::new();
            let mut st = st;
            st.set_text(v);
            st.move_to_line_end(false);
            st
        };
        let (year, bpm) = (if t.year == 0 { String::new() } else { t.year.to_string() }, if t.bpm == 0 { String::new() } else { t.bpm.to_string() });
        let pre = |v: &str| if single { v.to_string() } else { String::new() };
        inputs.insert(0, init(pre(&t.title)));
        inputs.insert(1, init(pre(&t.artist)));
        inputs.insert(2, init(pre(&t.album)));
        inputs.insert(3, init(pre(&year)));
        inputs.insert(4, init(pre(&bpm)));
        inputs.insert(5, init(pre(&t.creation_manual)));
        let list = |v: &[String]| if single { Values::Set(v.to_vec()) } else { Values::Marks(BTreeMap::new()) };
        let genres = list(&t.genres);
        let sources = t.sources.iter().map(|s| (s.name.clone(), list(&s.values))).collect();
        let mut tempo_opts = Vec::new();
        if !single {
            tempo_opts.push((tr!("tags-unchanged"), KEEP.to_string()));
        }
        tempo_opts.push((tr!("tags-tempo-auto"), String::new()));
        for c in &t.tempo_choices {
            tempo_opts.push((c.clone(), c.clone()));
        }
        if single && !t.tempo_manual.is_empty() && !t.tempo_choices.contains(&t.tempo_manual) {
            tempo_opts.push((t.tempo_manual.clone(), t.tempo_manual.clone()));
        }
        let tempo = if single { tempo_opts.iter().position(|o| o.1 == t.tempo_manual).unwrap_or(0) } else { 0 };
        let total = batch.as_ref().map_or(1, |b| b.len());
        let mut f = Self {
            batch,
            orig: t,
            known,
            rows,
            focus: 0,
            inputs,
            genres,
            sources,
            present_genres: Vec::new(),
            present_sources: Vec::new(),
            total,
            tempo_opts,
            tempo,
            picker: None,
            error: None,
        };
        let orig = [f.orig.clone()];
        f.tally(&orig);
        f.sync_focus();
        f
    }

    /// Les tags de tous les fichiers visés (lot) : leurs valeurs viennent en
    /// tête des listes à choisir, avec le nombre de fichiers qui les portent.
    pub fn with_present(mut self, all: &[MediaTags]) -> Self {
        if !all.is_empty() {
            self.tally(all);
        }
        self
    }

    fn tally(&mut self, all: &[MediaTags]) {
        self.present_genres = tally(all.iter().map(|t| t.genres.as_slice()));
        self.present_sources = self
            .sources
            .iter()
            .map(|(name, _)| {
                tally(all.iter().filter_map(|t| t.sources.iter().find(|s| s.name.eq_ignore_ascii_case(name)).map(|s| s.values.as_slice())))
            })
            .collect();
    }

    /// `Suppr` sur une liste : un média, vidée (ou rendue telle qu'au fichier
    /// si elle l'était déjà) ; un lot, vidée dans chaque fichier ↔ inchangée.
    fn clear_list(&mut self, which: Option<usize>) {
        let orig = match which {
            None => self.orig.genres.clone(),
            Some(i) => self.orig.sources.iter().find(|s| s.name == self.sources[i].0).map(|s| s.values.clone()).unwrap_or_default(),
        };
        let v = match which {
            None => &mut self.genres,
            Some(i) => &mut self.sources[i].1,
        };
        *v = match v {
            Values::Set(l) if l.is_empty() => Values::Set(orig),
            Values::Set(_) => Values::Set(Vec::new()),
            Values::Clear => Values::Marks(BTreeMap::new()),
            Values::Marks(_) => Values::Clear,
        };
    }

    fn sync_focus(&mut self) {
        let cur = text_id(self.rows[self.focus]);
        for (id, st) in self.inputs.iter() {
            st.focus.set(Some(*id) == cur);
        }
    }

    fn text(&self, r: Row) -> String {
        text_id(r).and_then(|i| self.inputs.get(&i)).map(|s| s.text().trim().to_string()).unwrap_or_default()
    }

    /// Les modifications demandées, ou ce qui ne va pas dans la saisie.
    pub fn changes(&self) -> Result<TagChanges, String> {
        let single = self.batch.is_none();
        let t = &self.orig;
        let text = |r: Row, old: &str| {
            let v = self.text(r);
            if single { (v != old.trim()).then_some(v) } else { (!v.is_empty()).then_some(v) }
        };
        let number = |r: Row, max: u32, label: String| -> Result<Option<u32>, String> {
            let v = self.text(r);
            if v.is_empty() {
                return Ok(None);
            }
            v.parse::<u32>().ok().filter(|n| (1..=max).contains(n)).map(Some).ok_or_else(|| tr!("tags-bad-number", field = label, max = max))
        };
        let year = number(Row::Year, 9999, tr!("media-field-year"))?;
        let bpm = number(Row::Bpm, 999, tr!("tags-bpm"))?;
        let creation = self.text(Row::Creation);
        if !creation.is_empty() && creation.parse::<jiff::Timestamp>().is_err() {
            return Err(tr!("tags-bad-creation"));
        }
        let tempo = &self.tempo_opts[self.tempo].1;
        let c = TagChanges {
            title: text(Row::Title, &t.title),
            artist: text(Row::Artist, &t.artist),
            album: text(Row::Album, &t.album),
            year: if single { (year.unwrap_or(0) != t.year).then(|| year.unwrap_or(0)) } else { year },
            genres: self.genres.edit(&t.genres),
            sources: self
                .sources
                .iter()
                .filter_map(|(name, v)| {
                    let before = t.sources.iter().find(|s| &s.name == name).map(|s| s.values.clone()).unwrap_or_default();
                    v.edit(&before).map(|e| (name.clone(), e))
                })
                .collect(),
            bpm: if single { (bpm.unwrap_or(0) != t.bpm).then(|| bpm.unwrap_or(0)) } else { bpm },
            tempo: if single { (*tempo != t.tempo_manual).then(|| tempo.clone()) } else { (tempo != KEEP).then(|| tempo.clone()) },
            creation: text(Row::Creation, &t.creation_manual),
        };
        if c.is_empty() {
            return Err(tr!("form-tags-nothing"));
        }
        Ok(c)
    }

    /// Les lignes de la confirmation : ce qui va changer.
    fn lines(&self, c: &TagChanges) -> Vec<String> {
        let mut out = vec![match &self.batch {
            None => tr!("confirm-tags-body", path = self.orig.rel_path.clone()),
            Some(p) => tr!("confirm-tags-many-body", n = p.len()),
        }];
        let mut text = |label: String, v: &Option<String>| {
            if let Some(v) = v {
                out.push(if v.is_empty() { tr!("confirm-tags-remove", field = label) } else { tr!("confirm-tags-set", field = label, value = v.clone()) });
            }
        };
        text(tr!("media-field-title"), &c.title);
        text(tr!("media-field-artist"), &c.artist);
        text(tr!("media-field-album"), &c.album);
        let num = |n: Option<u32>| n.map(|n| if n == 0 { String::new() } else { n.to_string() });
        text(tr!("media-field-year"), &num(c.year));
        text(tr!("tags-bpm"), &num(c.bpm));
        text(tr!("tags-tempo"), &c.tempo.as_ref().map(|t| if t.is_empty() { tr!("tags-tempo-auto") } else { t.clone() }));
        text(tr!("tags-creation"), &c.creation);
        let list = |label: String, e: &ListEdit| match e {
            ListEdit::Replace(v) if v.is_empty() => tr!("confirm-tags-remove", field = label),
            ListEdit::Replace(v) => tr!("confirm-tags-set", field = label, value = v.join(", ")),
            ListEdit::Merge { add, remove } => tr!("confirm-tags-merge", field = label, add = add.join(", "), remove = remove.join(", ")),
        };
        if let Some(g) = &c.genres {
            out.push(list(tr!("tags-genres"), g));
        }
        for (name, e) in &c.sources {
            out.push(list(name.clone(), e));
        }
        out
    }

    fn submit(&mut self) -> TagOutcome {
        match self.changes() {
            Err(e) => {
                self.error = Some(e);
                TagOutcome::Pending
            }
            Ok(c) => {
                let lines = self.lines(&c);
                let targets = match &self.batch {
                    None => vec![(self.orig.rel_path.clone(), self.orig.revision.clone())],
                    Some(p) => p.iter().map(|p| (p.clone(), String::new())).collect(),
                };
                TagOutcome::Submit(Box::new(Action::SetTags { targets, edit: Box::new(c) }), lines)
            }
        }
    }

    fn open_picker(&mut self, which: Option<usize>) {
        let (title, values, present) = match which {
            None => (tr!("tags-genres"), self.genres.clone(), &self.present_genres),
            Some(i) => (self.sources[i].0.clone(), self.sources[i].1.clone(), &self.present_sources[i]),
        };
        // Une liste vidée repart de « inchangé » si on choisit à nouveau.
        let values = if values == Values::Clear { Values::Marks(BTreeMap::new()) } else { values };
        let title = match &self.batch {
            None => tr!("tags-picker-title", field = title),
            Some(_) => tr!("tags-picker-title-batch", field = title),
        };
        let picker = GenrePicker::new(title, &self.known, values, present, self.total);
        self.picker = Some((which, picker));
    }

    pub fn handle(&mut self, e: &Event) -> TagOutcome {
        if let Some((which, p)) = self.picker.as_mut() {
            match p.handle(e) {
                PickerOutcome::Pending => {}
                PickerOutcome::Cancel => self.picker = None,
                PickerOutcome::Done(v) => {
                    match which {
                        None => self.genres = v,
                        Some(i) => self.sources[*i].1 = v,
                    }
                    self.picker = None;
                }
            }
            return TagOutcome::Pending;
        }
        let Event::Key(k) = e else { return TagOutcome::Pending };
        if k.kind != KeyEventKind::Press {
            return TagOutcome::Pending;
        }
        let row = self.rows[self.focus];
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => return TagOutcome::Cancel,
            KeyCode::Char('s') if ctrl => return self.submit(),
            KeyCode::Tab | KeyCode::Down => {
                self.focus = (self.focus + 1).min(self.rows.len() - 1);
                self.sync_focus();
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = self.focus.saturating_sub(1);
                self.sync_focus();
            }
            KeyCode::Delete if matches!(row, Row::Genres | Row::Source(_)) => {
                self.clear_list(if let Row::Source(i) = row { Some(i) } else { None });
                self.error = None;
            }
            KeyCode::Enter => match row {
                Row::Genres => self.open_picker(None),
                Row::Source(i) => self.open_picker(Some(i)),
                _ => {
                    self.focus = (self.focus + 1).min(self.rows.len() - 1);
                    self.sync_focus();
                }
            },
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if row == Row::Tempo => {
                let n = self.tempo_opts.len();
                self.tempo = if k.code == KeyCode::Left { (self.tempo + n - 1) % n } else { (self.tempo + 1) % n };
            }
            _ => {
                if let Some(st) = text_id(row).and_then(|i| self.inputs.get_mut(&i)) {
                    if ctrl && !matches!(k.code, KeyCode::Char('a' | 'e' | 'u' | 'k' | 'w')) {
                        return TagOutcome::Pending;
                    }
                    st.handle(e, Regular);
                    self.error = None;
                }
            }
        }
        TagOutcome::Pending
    }

    fn label(&self, r: Row) -> String {
        match r {
            Row::Title => tr!("media-field-title"),
            Row::Artist => tr!("media-field-artist"),
            Row::Album => tr!("media-field-album"),
            Row::Year => tr!("media-field-year"),
            Row::Genres => tr!("tags-genres"),
            Row::Source(i) => self.sources[i].0.clone(),
            Row::Bpm => tr!("tags-bpm"),
            Row::Tempo => tr!("tags-tempo"),
            Row::Creation => tr!("tags-creation"),
        }
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &SalsaTheme) -> Option<(u16, u16)> {
        let s = Styles(theme);
        let w = 84.min(area.width.saturating_sub(2)).max(50);
        let h = (self.rows.len() as u16 + 7).min(area.height.saturating_sub(1));
        let box_a = centered(area, w, h);
        Clear.render(box_a, buf);
        let title = match &self.batch {
            None => tr!("form-tags-title", path = self.orig.rel_path.clone()),
            Some(p) => tr!("form-tags-many-title", n = p.len()),
        };
        let block = frame(&title, &s, false).title_bottom(Span::styled(format!(" {} ", tr!("tags-form-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        // Colonne des libellés à la largeur du plus long (« date de création »,
        // noms de sources), bornée pour laisser la place aux valeurs.
        let label_w = self.rows.iter().map(|r| Span::raw(self.label(*r)).width()).max().unwrap_or(0).clamp(10, 24) as u16 + 1;
        let mut cursor = None;
        let style: rat_widget::text::TextStyle = theme.style(WidgetStyle::TEXT);
        for (i, r) in self.rows.clone().into_iter().enumerate() {
            let y = inner.y + i as u16;
            if y >= inner.y + inner.height {
                break;
            }
            let focused = i == self.focus;
            let row_a = Rect::new(inner.x, y, inner.width, 1);
            let [l_a, v_a] = Layout::horizontal([Constraint::Length(label_w), Constraint::Fill(1)]).areas(row_a);
            Paragraph::new(Span::styled(fit::ellipsize(&self.label(r), label_w as usize - 1), if focused { s.accent() } else { s.label() }))
                .render(l_a, buf);
            match r {
                Row::Genres | Row::Source(_) => {
                    let v = match r {
                        Row::Genres => &self.genres,
                        Row::Source(i) => &self.sources[i].1,
                        _ => unreachable!(),
                    };
                    let st = if focused { s.tab_active() } else { s.base() };
                    Paragraph::new(Line::from(vec![
                        Span::styled(fit::ellipsize(&v.summary(), v_a.width.saturating_sub(4) as usize), st),
                        Span::styled(if focused { " ⏎" } else { "" }, s.muted()),
                    ]))
                    .render(v_a, buf);
                }
                Row::Tempo => {
                    let mut label = self.tempo_opts[self.tempo].0.clone();
                    if self.tempo_opts[self.tempo].1.is_empty() && !self.orig.tempo.is_empty() && self.batch.is_none() {
                        label = tr!("tags-tempo-auto-now", tempo = self.orig.tempo.clone());
                    }
                    let st = if focused { s.tab_active() } else { s.base() };
                    Paragraph::new(Line::from(vec![
                        Span::styled(if focused { "◀ " } else { "" }, s.accent()),
                        Span::styled(label, st),
                        Span::styled(if focused { " ▶" } else { "" }, s.accent()),
                    ]))
                    .render(v_a, buf);
                }
                _ => {
                    if let Some(st) = text_id(r).and_then(|id| self.inputs.get_mut(&id)) {
                        if st.text().is_empty() && !focused {
                            let hint = match (r, &self.batch) {
                                (_, Some(_)) => tr!("tags-unchanged"),
                                (Row::Creation, None) if !self.orig.creation.is_empty() => tr!("tags-creation-now", date = self.orig.creation.clone()),
                                _ => "—".into(),
                            };
                            Paragraph::new(Span::styled(hint, s.muted())).render(v_a, buf);
                        } else {
                            TextInput::new().styles(style.clone()).render(v_a, buf, st);
                            if focused {
                                cursor = st.screen_cursor();
                            }
                        }
                    }
                }
            }
        }
        // Aide de la ligne qui a le focus, puis l'erreur de saisie.
        let help = match self.rows[self.focus] {
            Row::Genres | Row::Source(_) => tr!("tags-help-list"),
            Row::Tempo => tr!("tags-help-tempo"),
            Row::Creation => tr!("tags-help-creation"),
            Row::Bpm => tr!("tags-help-bpm"),
            _ if self.batch.is_some() => tr!("tags-help-batch"),
            _ => tr!("tags-help-single"),
        };
        let n = self.rows.len() as u16;
        let help_a = Rect::new(inner.x, inner.y + n + 1, inner.width, 2.min(inner.height.saturating_sub(n + 1)));
        Paragraph::new(Span::styled(help, s.muted())).wrap(Wrap { trim: true }).render(help_a, buf);
        if let Some(e) = &self.error {
            let e_a = Rect::new(inner.x, inner.y + inner.height.saturating_sub(1), inner.width, 1);
            Paragraph::new(Span::styled(fit::ellipsize(e, inner.width as usize), s.error())).render(e_a, buf);
        }
        if let Some((_, p)) = self.picker.as_mut() {
            return p.render(area, buf, theme);
        }
        cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stationd_proto::library::TagValues;

    fn tags() -> MediaTags {
        MediaTags {
            rel_path: "a.mp3".into(),
            title: "T".into(),
            year: 2001,
            revision: "tags:1".into(),
            genres: vec!["électro".into()],
            sources: vec![TagValues { name: "Type".into(), values: vec!["song".into()] }],
            bpm: 120,
            tempo: "fast".into(),
            tempo_choices: vec!["slow".into(), "fast".into()],
            ..Default::default()
        }
    }

    fn key(c: KeyCode) -> Event {
        Event::Key(ratatui_crossterm::crossterm::event::KeyEvent::new(c, KeyModifiers::NONE))
    }

    #[test]
    fn nothing_changed_is_said_and_one_change_is_sent_alone() {
        let mut f = TagForm::new(tags(), vec![], None);
        assert!(f.changes().is_err());
        // Tempo : auto → slow.
        f.focus = f.rows.iter().position(|r| *r == Row::Tempo).unwrap();
        f.handle(&key(KeyCode::Right));
        let c = f.changes().unwrap();
        assert_eq!(c, TagChanges { tempo: Some("slow".into()), ..Default::default() });
    }

    #[test]
    fn genres_are_picked_from_the_known_list_or_typed() {
        let known = vec![GenreCount { genre: "house".into(), count: 3, spellings: vec![] }];
        let mut f = TagForm::new(tags(), known, None);
        f.focus = f.rows.iter().position(|r| *r == Row::Genres).unwrap();
        f.handle(&key(KeyCode::Enter));
        // Liste : électro (déjà là), house ; « house » coché, « électro » décoché.
        f.handle(&key(KeyCode::Char(' ')));
        f.handle(&key(KeyCode::Down));
        f.handle(&key(KeyCode::Char(' ')));
        for c in "jazz".chars() {
            f.handle(&key(KeyCode::Char(c)));
        }
        f.handle(&key(KeyCode::Char(' ')));
        f.handle(&key(KeyCode::Enter));
        assert!(f.picker.is_none());
        let c = f.changes().unwrap();
        assert_eq!(c.genres, Some(ListEdit::Replace(vec!["house".into(), "jazz".into()])));
    }

    #[test]
    fn a_partial_input_picks_the_known_genre_before_creating_one() {
        let known = vec![GenreCount { genre: "ambient".into(), count: 2, spellings: vec![] }];
        let mut f = TagForm::new(tags(), known, None);
        f.focus = f.rows.iter().position(|r| *r == Row::Genres).unwrap();
        f.handle(&key(KeyCode::Enter));
        for c in "amb".chars() {
            f.handle(&key(KeyCode::Char(c)));
        }
        let rows = f.picker.as_ref().unwrap().1.rows();
        assert_eq!(rows.iter().map(|r| (r.0.as_str(), r.2)).collect::<Vec<_>>(), vec![("ambient", false), ("amb", true)]);
        f.handle(&key(KeyCode::Char(' ')));
        f.handle(&key(KeyCode::Enter));
        assert_eq!(f.changes().unwrap().genres, Some(ListEdit::Replace(vec!["électro".into(), "ambient".into()])));
    }

    #[test]
    fn a_batch_adds_or_removes_genres_and_keeps_empty_fields() {
        let known = vec![GenreCount { genre: "song".into(), count: 9, spellings: vec![] }, GenreCount { genre: "talk".into(), count: 2, spellings: vec![] }];
        let mut f = TagForm::new(tags(), known, Some(vec!["a.mp3".into(), "b.mp3".into()]));
        assert!(f.changes().is_err(), "rien de saisi");
        let i = f.rows.iter().position(|r| *r == Row::Source(0)).unwrap();
        f.focus = i;
        f.handle(&key(KeyCode::Enter));
        // « song » (porté par le lot) en tête, puis « talk ».
        f.handle(&key(KeyCode::Char(' '))); // song : retirer d'emblée
        f.handle(&key(KeyCode::Down));
        f.handle(&key(KeyCode::Char(' '))); // talk (absent) : ajouter
        for c in "news".chars() {
            f.handle(&key(KeyCode::Char(c)));
        }
        f.handle(&key(KeyCode::Char(' '))); // news : ajouter
        f.handle(&key(KeyCode::Enter));
        let c = f.changes().unwrap();
        assert_eq!(c.sources, vec![("Type".into(), ListEdit::Merge { add: vec!["news".into(), "talk".into()], remove: vec!["song".into()] })]);
        assert!(c.title.is_none() && c.genres.is_none() && c.tempo.is_none());
    }

    #[test]
    fn a_batch_puts_the_values_its_files_carry_first() {
        let known = vec![
            GenreCount { genre: "ambient".into(), count: 40, spellings: vec![] },
            GenreCount { genre: "House".into(), count: 3, spellings: vec![] },
        ];
        let mut b = tags();
        b.rel_path = "b.mp3".into();
        b.genres = vec!["house".into(), "Électro".into()];
        let mut c = tags();
        c.rel_path = "c.mp3".into();
        c.genres = vec![];
        let all = [tags(), b, c];
        let f = TagForm::new(all[0].clone(), known, Some(vec!["a.mp3".into(), "b.mp3".into(), "c.mp3".into()])).with_present(&all);
        let mut f = f;
        f.focus = f.rows.iter().position(|r| *r == Row::Genres).unwrap();
        f.handle(&key(KeyCode::Enter));
        let rows = f.picker.as_ref().unwrap().1.rows();
        let got: Vec<(&str, Option<u32>)> = rows.iter().map(|r| (r.0.as_str(), r.3)).collect();
        // électro sur 2 fichiers, House sur 1 (graphie de la bibliothèque), puis le reste.
        assert_eq!(got, vec![("électro", Some(2)), ("House", Some(1)), ("ambient", None)]);
    }

    #[test]
    fn delete_empties_a_list_in_one_file_or_in_each_file_of_a_batch() {
        // Un média : vidée, puis Suppr à nouveau rend la liste du fichier.
        let mut f = TagForm::new(tags(), vec![], None);
        f.focus = f.rows.iter().position(|r| *r == Row::Genres).unwrap();
        f.handle(&key(KeyCode::Delete));
        assert_eq!(f.changes().unwrap().genres, Some(ListEdit::Replace(vec![])));
        f.handle(&key(KeyCode::Delete));
        assert!(f.changes().is_err(), "rendue telle qu'au fichier");
        // Un lot : vidée dans chaque fichier ↔ inchangée.
        let mut f = TagForm::new(tags(), vec![], Some(vec!["a.mp3".into(), "b.mp3".into()]));
        f.focus = f.rows.iter().position(|r| *r == Row::Source(0)).unwrap();
        f.handle(&key(KeyCode::Delete));
        let c = f.changes().unwrap();
        assert_eq!(c.sources, vec![("Type".into(), ListEdit::Replace(vec![]))]);
        let mut current = tags();
        current.sources[0].values = vec!["song".into(), "talk".into()];
        let req = c.request("b.mp3", "r".into(), Some(&current));
        assert!(req.sources[0].values.is_empty(), "retiré du fichier");
        f.handle(&key(KeyCode::Delete));
        assert!(f.changes().is_err(), "de nouveau inchangée");
        // Rouvrir le choix après avoir vidé repart de « inchangé ».
        f.handle(&key(KeyCode::Delete));
        f.handle(&key(KeyCode::Enter));
        f.handle(&key(KeyCode::Enter));
        assert!(f.changes().is_err());
    }

    #[test]
    fn bad_numbers_and_dates_are_refused_before_sending() {
        let mut f = TagForm::new(tags(), vec![], None);
        f.inputs.get_mut(&4).unwrap().set_text("abc");
        assert!(f.changes().is_err());
        f.inputs.get_mut(&4).unwrap().set_text("120");
        f.inputs.get_mut(&5).unwrap().set_text("14/06/2026");
        assert!(f.changes().is_err());
        f.inputs.get_mut(&5).unwrap().set_text("2026-06-14T06:36:48Z");
        assert_eq!(f.changes().unwrap().creation.as_deref(), Some("2026-06-14T06:36:48Z"));
    }
}
