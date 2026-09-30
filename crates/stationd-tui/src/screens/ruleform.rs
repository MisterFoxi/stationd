//! Éditeur d'une règle de grille (Agenda, lot 6b) : créer un event ou
//! modifier une règle, dans la grille regardée (active ou en préparation).
//!
//! Le formulaire n'est qu'une vue d'une table `[[rule]]` du fichier
//! (`gridraft` : commentaires gardés, valeurs écrites telles que saisies).
//! Après chaque frappe (300 ms), stationd juge le brouillon (`ValidateGrid` :
//! diagnostics par champ) et le projette sur la journée (`Preview` du
//! brouillon, rien d'appliqué) ; `Ctrl+S` l'écrit (`SaveGrid`, avec la
//! révision lue) — appliqué aussitôt si c'est la grille active. Conflit :
//! `Ctrl+R` relit le fichier et y reporte la saisie (rien d'écrasé, rien de
//! perdu).
//!
//! Les heures d'une règle sont des heures civiles de la station (la grammaire
//! de la grille), quel que soit l'affichage UTC de l'agenda.

use std::time::Duration;

use jiff::tz::TimeZone;
use rat_salsa::{Control, SalsaContext};
use rat_theme4::WidgetStyle;
use rat_widget::event::{HandleEvent, Regular};
use rat_widget::text::HasScreenCursor;
use rat_widget::text_input::{TextInput, TextInputState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::clear::Clear;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use stationd_proto::schedule::grid_diagnostic::Code;
use stationd_proto::schedule::{GetGridResponse, GridDiagnostic};

use super::agenda::AgEvent;
use super::picker::{Picked, PlaylistPicker};
use crate::agenda::{self, MarkKind, Period};
use crate::app::{AppEvent, Global};
use crate::gridraft::{self, RuleFields};
use crate::screen::KeyHelp;
use crate::style::Styles;
use crate::{fit, k, tr};

const DEBOUNCE: Duration = Duration::from_millis(300);

/// Un champ du formulaire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum F {
    Id,
    Enabled,
    Playlist,
    Dj,
    Start,
    End,
    Anchor,
    AnchorValue,
    Mode,
    Expiry,
    Cadence,
    CadenceValue,
    Days,
    DateStart,
    DateEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Choice,
    Days,
    Playlist,
}

/// Les lignes d'une nature de règle, dans l'ordre.
fn rows_of(kind: &str) -> Vec<F> {
    let mut v = vec![F::Id, F::Enabled];
    match kind {
        "base_rotation" => v.push(F::Playlist),
        "day_part" => v.extend([F::Playlist, F::Start, F::End]),
        "at_clock" => v.extend([F::Playlist, F::Anchor, F::AnchorValue, F::Mode, F::Expiry]),
        "every" => v.extend([F::Playlist, F::Cadence, F::CadenceValue]),
        "live" => v.extend([F::Dj, F::Start]),
        _ => {}
    }
    v.extend([F::Days, F::DateStart, F::DateEnd]);
    v
}

fn widget(f: F) -> Kind {
    match f {
        F::Enabled | F::Anchor | F::Mode | F::Cadence => Kind::Choice,
        F::Days => Kind::Days,
        F::Playlist => Kind::Playlist,
        _ => Kind::Text,
    }
}

/// Le champ d'une règle qu'un diagnostic désigne (`rule[n].<clé>`).
fn field_of(key: &str) -> Option<F> {
    Some(match key {
        "id" => F::Id,
        "enabled" => F::Enabled,
        "playlist_ref" => F::Playlist,
        "dj" => F::Dj,
        "start" => F::Start,
        "end" => F::End,
        "every_minutes" | "minute" | "at" => F::AnchorValue,
        "mode" => F::Mode,
        "expiry" => F::Expiry,
        "min_tracks" | "min_elapsed" => F::CadenceValue,
        "days" => F::Days,
        "date_start" => F::DateStart,
        "date_end" => F::DateEnd,
        _ => return None,
    })
}

/// Nature d'une règle, traduite.
pub fn kind_label(kind: &str) -> String {
    match kind {
        "base_rotation" => tr!("ag-origin-base"),
        "day_part" => tr!("ag-origin-daypart"),
        "at_clock" => tr!("ag-kind-at-clock"),
        "every" => tr!("ag-origin-every"),
        "live" => tr!("ag-kind-live"),
        other => other.to_string(),
    }
}

/// Un problème de grille, traduit par son code (jamais par son message ;
/// le message technique n'est repris que pour une erreur de syntaxe).
pub fn diag_text(d: &GridDiagnostic) -> String {
    let mut t = match Code::try_from(d.code).unwrap_or(Code::Unspecified) {
        Code::Syntax => tr!("diag-syntax", detail = d.message.clone()),
        Code::UnknownField => tr!("diag-unknown-field"),
        Code::MissingField => tr!("diag-missing-field"),
        Code::BadValue => tr!("diag-bad-value"),
        Code::NotAllowed => tr!("gdiag-not-allowed"),
        Code::Conflict => tr!("diag-conflict"),
        Code::BadDuration => tr!("diag-bad-duration"),
        Code::BadTime => tr!("gdiag-bad-time"),
        Code::BadDate => tr!("gdiag-bad-date"),
        Code::BadWeekday => tr!("gdiag-bad-weekday"),
        Code::SchemaVersion => tr!("gdiag-schema-version"),
        Code::DuplicateId => tr!("gdiag-duplicate-id"),
        Code::SeveralFloors => tr!("gdiag-several-floors"),
        Code::ZeroWindow => tr!("gdiag-zero-window"),
        Code::DatesReversed => tr!("gdiag-dates-reversed"),
        Code::OutOfRange => tr!("gdiag-out-of-range"),
        Code::UnknownRef => tr!("diag-unknown-ref"),
        Code::BadRef => tr!("diag-bad-ref"),
        Code::UnknownDj => tr!("gdiag-unknown-dj"),
        Code::NoLive => tr!("gdiag-no-live"),
        Code::DjFileUnreadable => tr!("gdiag-dj-file", detail = d.message.clone()),
        Code::FileUnreadable => tr!("gdiag-file", detail = d.message.clone()),
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

/// Ce que l'éditeur dit à l'agenda.
pub enum FormOutcome {
    Stay,
    /// Fermé sans rien écrire.
    Close,
    /// Écrit : message pour la ligne de statut ; l'agenda se relit.
    Saved(String),
}

pub struct RuleForm {
    pub owner: u64,
    grid: String,
    active: bool,
    exists: bool,
    revision: String,
    /// Texte de la grille à l'ouverture (ou à la relecture après conflit).
    base: String,
    /// Position de la règle dans le fichier (0…).
    index: usize,
    is_new: bool,
    /// Id de la règle à l'ouverture (pour la retrouver après un conflit).
    original_id: String,
    fields: RuleFields,
    /// Texte des champs à l'ouverture (modifié ?).
    saved: RuleFields,
    focus: usize,
    input: TextInputState,
    input_for: Option<F>,
    day_cursor: usize,
    diags: Vec<GridDiagnostic>,
    preview: Option<Result<agenda::Projection, String>>,
    /// Journée projetée, et le fuseau de l'affichage.
    day: Period,
    tz: TimeZone,
    typing: u64,
    check_req: u64,
    saving: bool,
    conflict: bool,
    confirm_quit: bool,
    message: Option<(String, bool)>,
    picker: Option<PlaylistPicker>,
}

impl RuleForm {
    /// `index` = la règle à modifier ; `None` = une nouvelle (`fields`
    /// pré-remplis), ajoutée en fin de fichier.
    pub fn open(g: &GetGridResponse, index: Option<usize>, fields: RuleFields, day: Period, tz: TimeZone, ctx: &mut Global) -> Self {
        let doc = gridraft::parse(&g.toml).ok();
        let n = doc.as_ref().map(gridraft::count).unwrap_or(0);
        let mut f = Self {
            owner: super::next_owner(),
            grid: g.name.clone(),
            active: g.active,
            exists: g.exists,
            revision: g.revision.clone(),
            base: g.toml.clone(),
            index: index.unwrap_or(n),
            is_new: index.is_none(),
            original_id: fields.id.clone(),
            saved: fields.clone(),
            fields,
            focus: 0,
            input: TextInputState::new(),
            input_for: None,
            day_cursor: 0,
            diags: Vec::new(),
            preview: None,
            day,
            tz,
            typing: 0,
            check_req: 0,
            saving: false,
            conflict: false,
            confirm_quit: false,
            message: None,
            picker: None,
        };
        if f.is_new {
            f.saved = RuleFields::default(); // une création est toujours « modifiée »
        }
        f.sync_input();
        f.check_now(ctx);
        f
    }

    fn rows(&self) -> Vec<F> {
        rows_of(&self.fields.kind)
    }

    fn current(&self) -> F {
        let r = self.rows();
        r[self.focus.min(r.len() - 1)]
    }

    fn dirty(&self) -> bool {
        self.fields != self.saved
    }

    fn text(&self, f: F) -> String {
        let x = &self.fields;
        match f {
            F::Id => x.id.clone(),
            F::Playlist => x.playlist_ref.clone(),
            F::Dj => x.dj.clone(),
            F::Start => x.start.clone(),
            F::End => x.end.clone(),
            F::AnchorValue => x.anchor_value.clone(),
            F::Expiry => x.expiry.clone(),
            F::CadenceValue => x.cadence_value.clone(),
            F::DateStart => x.date_start.clone(),
            F::DateEnd => x.date_end.clone(),
            F::Enabled => x.enabled.to_string(),
            F::Anchor => x.anchor.clone(),
            F::Mode => x.mode.clone(),
            F::Cadence => x.cadence.clone(),
            F::Days => String::new(),
        }
    }

    fn set_text(&mut self, f: F, v: String) {
        let x = &mut self.fields;
        match f {
            F::Id => x.id = v,
            F::Playlist => x.playlist_ref = v,
            F::Dj => x.dj = v,
            F::Start => x.start = v,
            F::End => x.end = v,
            F::AnchorValue => x.anchor_value = v,
            F::Expiry => x.expiry = v,
            F::CadenceValue => x.cadence_value = v,
            F::DateStart => x.date_start = v,
            F::DateEnd => x.date_end = v,
            _ => {}
        }
    }

    fn label(&self, f: F) -> String {
        match f {
            F::Id => tr!("rf-id"),
            F::Enabled => tr!("pl-f-enabled"),
            F::Playlist => tr!("rf-playlist"),
            F::Dj => tr!("rf-dj"),
            F::Start => tr!("rf-start"),
            F::End => tr!("rf-end"),
            F::Anchor => tr!("rf-anchor"),
            F::AnchorValue => {
                match self.fields.anchor.as_str() {
                    "every_minutes" => tr!("rf-every-minutes"),
                    "minute" => tr!("rf-minute"),
                    _ => tr!("rf-at"),
                }
            }
            F::Mode => tr!("rf-mode"),
            F::Expiry => tr!("rf-expiry"),
            F::Cadence => tr!("rf-cadence"),
            F::CadenceValue => {
                if self.fields.cadence == "min_tracks" { tr!("rf-min-tracks") } else { tr!("rf-min-elapsed") }
            }
            F::Days => tr!("rf-days"),
            F::DateStart => tr!("rf-date-start"),
            F::DateEnd => tr!("rf-date-end"),
        }
    }

    /// Choix d'un champ fermé : (valeur, libellé).
    fn choices(f: F) -> Vec<(&'static str, String)> {
        match f {
            F::Enabled => vec![("true", tr!("val-yes")), ("false", tr!("val-no"))],
            F::Anchor => vec![
                ("at", tr!("rf-anchor-at")),
                ("minute", tr!("rf-anchor-minute")),
                ("every_minutes", tr!("rf-anchor-every")),
            ],
            F::Mode => vec![("soft", tr!("ag-soft")), ("hard", tr!("ag-hard"))],
            F::Cadence => vec![("min_elapsed", tr!("rf-cadence-elapsed")), ("min_tracks", tr!("rf-cadence-tracks"))],
            _ => vec![],
        }
    }

    /// Aide sous un champ (format attendu).
    fn hint(&self, f: F) -> Option<String> {
        Some(match f {
            F::Start => tr!("rf-hint-time"),
            F::End => tr!("rf-hint-end"),
            F::AnchorValue if self.fields.anchor == "every_minutes" => tr!("rf-hint-every-minutes"),
            F::AnchorValue if self.fields.anchor == "minute" => tr!("rf-hint-minute"),
            F::AnchorValue => tr!("rf-hint-time"),
            F::Expiry => tr!("rf-hint-expiry"),
            F::CadenceValue if self.fields.cadence == "min_tracks" => tr!("rf-hint-tracks"),
            F::CadenceValue => tr!("rf-hint-duration"),
            F::DateStart | F::DateEnd => tr!("rf-hint-date"),
            F::Days => tr!("rf-hint-days"),
            F::Playlist => tr!("rf-hint-playlist"),
            _ => return None,
        })
    }

    fn sync_input(&mut self) {
        let f = self.current();
        if matches!(widget(f), Kind::Text | Kind::Playlist) {
            if self.input_for != Some(f) {
                self.input.set_text(self.text(f));
                self.input.move_to_line_end(false);
                self.input_for = Some(f);
            }
            self.input.focus.set(self.picker.is_none());
        } else {
            self.input_for = None;
            self.input.focus.set(false);
        }
    }

    fn draft(&self) -> Result<String, String> {
        gridraft::with_rule(&self.base, self.index, &self.fields)
    }

    fn check_now(&mut self, ctx: &mut Global) {
        let Ok(toml) = self.draft() else { return };
        self.check_req += 1;
        let (id, owner, channel, name) = (self.check_req, self.owner, ctx.channel.clone(), self.grid.clone());
        let (from, window) = self.day.request();
        ctx.spawn_async(async move {
            let r = crate::rpc::check_draft(channel, name, toml, from, window).await;
            Ok(Control::Event(AppEvent::Agenda(Box::new(AgEvent::Checked(owner, id, Box::new(r))))))
        });
    }

    fn changed(&mut self, ctx: &mut Global) {
        self.confirm_quit = false;
        self.typing += 1;
        let (owner, n) = (self.owner, self.typing);
        ctx.spawn_async(async move {
            tokio::time::sleep(DEBOUNCE).await;
            Ok(Control::Event(AppEvent::Agenda(Box::new(AgEvent::Typed(owner, n)))))
        });
    }

    fn save(&mut self, ctx: &mut Global) {
        let toml = match self.draft() {
            Ok(t) => t,
            Err(e) => {
                self.message = Some((e, true));
                return;
            }
        };
        self.saving = true;
        self.message = Some((tr!("rf-saving"), false));
        let (owner, channel, name, rev) = (self.owner, ctx.channel.clone(), self.grid.clone(), self.revision.clone());
        ctx.spawn_async(async move {
            let r = crate::rpc::save_grid(channel, name, toml, rev).await;
            Ok(Control::Event(AppEvent::Agenda(Box::new(AgEvent::Saved(owner, Box::new(r))))))
        });
    }

    /// Conflit : relire le fichier et y reporter la saisie.
    fn rebase(&mut self, ctx: &mut Global) {
        self.message = Some((tr!("rf-rebasing"), false));
        let (owner, channel, name) = (self.owner, ctx.channel.clone(), self.grid.clone());
        ctx.spawn_async(async move {
            let r = crate::rpc::grid_text(channel, name).await;
            Ok(Control::Event(AppEvent::Agenda(Box::new(AgEvent::Rebased(owner, Box::new(r))))))
        });
    }

    fn open_picker(&mut self, ctx: &mut Global) {
        self.picker = Some(PlaylistPicker::new(tr!("rf-pick-playlist"), Vec::new(), false));
        self.sync_input();
        let (owner, channel) = (self.owner, ctx.channel.clone());
        ctx.spawn_async(async move {
            let r = crate::rpc::list_playlists(channel).await;
            Ok(Control::Event(AppEvent::Agenda(Box::new(AgEvent::Playlists(owner, r)))))
        });
    }

    /// Réponses destinées à cet éditeur (`None` = pas pour lui).
    pub fn on_event(&mut self, ev: &AgEvent, ctx: &mut Global) -> Option<FormOutcome> {
        match ev {
            AgEvent::Typed(o, n) if *o == self.owner => {
                if *n == self.typing {
                    self.check_now(ctx);
                }
            }
            AgEvent::Checked(o, n, r) if *o == self.owner => {
                if *n == self.check_req {
                    match &r.validate {
                        Ok(v) => self.diags = v.diagnostics.clone(),
                        Err(e) => self.message = Some((e.clone(), true)),
                    }
                    self.preview = Some(r.preview.clone().map(|p| agenda::project(p, self.day)));
                }
            }
            AgEvent::Playlists(o, r) if *o == self.owner => {
                if let Some(p) = self.picker.as_mut() {
                    p.set_items(r.clone());
                }
            }
            AgEvent::Saved(o, r) if *o == self.owner => {
                self.saving = false;
                match &**r {
                    Ok(s) if s.ok => {
                        let msg = if s.applied {
                            tr!("rf-saved-applied", id = self.fields.id.clone(), grid = self.grid.clone())
                        } else {
                            tr!("rf-saved", id = self.fields.id.clone(), grid = self.grid.clone())
                        };
                        return Some(FormOutcome::Saved(msg));
                    }
                    Ok(s) if s.conflict => {
                        self.conflict = true;
                        self.message = Some((tr!("rf-conflict"), true));
                    }
                    Ok(s) => {
                        self.diags = s.diagnostics.clone();
                        self.message = Some((tr!("rf-invalid"), true));
                        // Le focus sur le premier champ fautif de la règle.
                        let prefix = format!("rule[{}].", self.index + 1);
                        if let Some(f) = self
                            .diags
                            .iter()
                            .filter_map(|d| d.field_path.strip_prefix(&prefix).and_then(field_of))
                            .next()
                            && let Some(i) = self.rows().iter().position(|r| *r == f)
                        {
                            self.focus = i;
                            self.sync_input();
                        }
                    }
                    Err(e) => self.message = Some((e.clone(), true)),
                }
            }
            AgEvent::Rebased(o, r) if *o == self.owner => match &**r {
                Ok(g) => {
                    self.base = g.toml.clone();
                    self.revision = g.revision.clone();
                    self.exists = g.exists;
                    let doc = gridraft::parse(&g.toml).ok();
                    let found = doc.as_ref().and_then(|d| gridraft::index_of(d, &self.original_id));
                    match (found, self.is_new) {
                        (Some(i), false) => self.index = i,
                        _ => {
                            // Règle disparue entre-temps (ou nouvelle) : ajoutée.
                            self.index = doc.as_ref().map(gridraft::count).unwrap_or(0);
                            self.is_new = true;
                        }
                    }
                    self.conflict = false;
                    self.message = Some((tr!("rf-rebased"), false));
                    self.check_now(ctx);
                }
                Err(e) => self.message = Some((e.clone(), true)),
            },
            _ => return None,
        }
        Some(FormOutcome::Stay)
    }

    pub fn on_key(&mut self, e: &Event, ctx: &mut Global) -> FormOutcome {
        if let Some(p) = self.picker.as_mut() {
            match p.handle(e) {
                Picked::Chosen(r) => {
                    self.fields.playlist_ref = r;
                    self.picker = None;
                    self.input_for = None;
                    self.sync_input();
                    self.changed(ctx);
                }
                Picked::Cancel => {
                    self.picker = None;
                    self.sync_input();
                }
                _ => {}
            }
            return FormOutcome::Stay;
        }
        let Event::Key(k) = e else { return FormOutcome::Stay };
        if k.kind != KeyEventKind::Press {
            return FormOutcome::Stay;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let rows = self.rows();
        let f = self.current();
        match k.code {
            KeyCode::Char('s') if ctrl => {
                if !self.saving {
                    self.save(ctx);
                }
                return FormOutcome::Stay;
            }
            KeyCode::Char('r') if ctrl => {
                self.rebase(ctx);
                return FormOutcome::Stay;
            }
            KeyCode::Esc => {
                if self.dirty() && !self.confirm_quit {
                    self.confirm_quit = true;
                    self.message = Some((tr!("rf-confirm-quit"), true));
                    return FormOutcome::Stay;
                }
                return FormOutcome::Close;
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.focus = (self.focus + rows.len() - 1) % rows.len();
                self.sync_input();
                return FormOutcome::Stay;
            }
            KeyCode::Down | KeyCode::Tab => {
                self.focus = (self.focus + 1) % rows.len();
                self.sync_input();
                return FormOutcome::Stay;
            }
            _ => {}
        }
        match widget(f) {
            Kind::Choice => {
                let opts = Self::choices(f);
                let cur = opts.iter().position(|o| o.0 == self.text(f)).unwrap_or(0);
                let next = match k.code {
                    KeyCode::Left => (cur + opts.len() - 1) % opts.len(),
                    KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter => (cur + 1) % opts.len(),
                    _ => return FormOutcome::Stay,
                };
                let v = opts[next].0.to_string();
                match f {
                    F::Enabled => self.fields.enabled = v == "true",
                    F::Anchor => self.fields.anchor = v,
                    F::Mode => self.fields.mode = v,
                    F::Cadence => self.fields.cadence = v,
                    _ => {}
                }
                self.changed(ctx);
            }
            Kind::Days => match k.code {
                KeyCode::Left => self.day_cursor = (self.day_cursor + 6) % 7,
                KeyCode::Right => self.day_cursor = (self.day_cursor + 1) % 7,
                KeyCode::Char(' ') | KeyCode::Enter => {
                    self.fields.days[self.day_cursor] = !self.fields.days[self.day_cursor];
                    self.changed(ctx);
                }
                _ => {}
            },
            Kind::Playlist if k.code == KeyCode::Enter => self.open_picker(ctx),
            Kind::Text | Kind::Playlist => {
                if k.code == KeyCode::Enter {
                    self.focus = (self.focus + 1) % rows.len();
                    self.sync_input();
                    return FormOutcome::Stay;
                }
                let before = self.input.text().to_string();
                self.input.handle(e, Regular);
                let after = self.input.text().to_string();
                if after != before {
                    self.set_text(f, after);
                    self.changed(ctx);
                }
            }
        }
        FormOutcome::Stay
    }

    pub fn keys(&self) -> &'static [KeyHelp] {
        &[
            (k!("key-up-down"), k!("help-rf-field")),
            (k!("key-left-right-space"), k!("help-rf-choice")),
            (k!("key-enter"), k!("help-rf-enter")),
            (k!("key-ctrl-s"), k!("help-rf-save")),
            (k!("key-ctrl-r"), k!("help-rf-rebase")),
            (k!("key-esc"), k!("help-rf-close")),
        ]
    }

    // --- rendu -----------------------------------------------------------------

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) {
        let s = Styles(&ctx.theme);
        Clear.render(area, buf);
        Block::new().style(s.base()).render(area, buf);
        let [head_a, body_a] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        self.render_head(head_a, buf, &s);
        let cursor = if area.width >= 100 {
            let [l, r] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body_a);
            let c = self.render_form(l, buf, ctx);
            self.render_side(r, buf, &Styles(&ctx.theme));
            c
        } else {
            let side_h = (body_a.height / 3).max(6);
            let [l, r] = Layout::vertical([Constraint::Fill(1), Constraint::Length(side_h)]).areas(body_a);
            let c = self.render_form(l, buf, ctx);
            self.render_side(r, buf, &Styles(&ctx.theme));
            c
        };
        if let Some(p) = self.picker.as_mut() {
            let c = p.render(area, buf, &ctx.theme);
            ctx.set_screen_cursor(c);
        } else {
            ctx.set_screen_cursor(cursor);
        }
    }

    fn render_head(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let title = if self.is_new {
            tr!("rf-title-new", kind = kind_label(&self.fields.kind))
        } else {
            tr!("rf-title-edit", id = self.original_id.clone())
        };
        let grid = if self.active { tr!("rf-grid-active", grid = self.grid.clone()) } else { tr!("rf-grid-other", grid = self.grid.clone()) };
        let mut l1 = vec![Span::styled(format!(" {title}"), s.title()), Span::styled(format!("  · {grid}"), s.muted())];
        if self.dirty() {
            l1.push(Span::styled(format!("  {}", tr!("rf-modified")), s.warn()));
        }
        let l2 = match &self.message {
            Some((m, true)) => Line::styled(format!(" {m}"), s.error()),
            Some((m, false)) => Line::styled(format!(" {m}"), s.accent()),
            None => Line::styled(format!(" {}", tr!("rf-local-times")), s.muted()),
        };
        Paragraph::new(vec![Line::from(l1), l2]).render(area, buf);
    }

    fn render_form(&mut self, area: Rect, buf: &mut Buffer, ctx: &Global) -> Option<(u16, u16)> {
        let s = Styles(&ctx.theme);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(if self.picker.is_none() { s.accent() } else { s.border() })
            .title(Span::styled(format!(" {} ", kind_label(&self.fields.kind)), s.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        let rows = self.rows();
        let prefix = format!("rule[{}]", self.index + 1);
        let mine = |d: &&GridDiagnostic| d.field_path == prefix || d.field_path.starts_with(&format!("{prefix}."));
        let label_w = (inner.width as usize / 3).clamp(12, 24);
        let mut lines: Vec<(Line, Option<usize>)> = Vec::new();
        let mut focus_line = 0;
        for (i, f) in rows.iter().enumerate() {
            let is_focus = i == self.focus;
            let row_diags: Vec<&GridDiagnostic> = self
                .diags
                .iter()
                .filter(mine)
                .filter(|d| d.field_path.strip_prefix(&format!("{prefix}.")).and_then(field_of) == Some(*f))
                .collect();
            let mark = if row_diags.is_empty() { Span::raw(" ") } else { Span::styled("✗", s.error()) };
            let label = Span::styled(
                format!(" {:<w$} ", fit::ellipsize(&self.label(*f), label_w), w = label_w),
                if is_focus { s.accent() } else { s.label() },
            );
            let value: Vec<Span> = match widget(*f) {
                Kind::Choice => {
                    let opts = Self::choices(*f);
                    let v = self.text(*f);
                    let l = opts.iter().find(|o| o.0 == v).map(|o| o.1.clone()).unwrap_or(v);
                    vec![if is_focus { Span::styled(format!("◀ {l} ▶"), s.tab_active()) } else { Span::raw(l) }]
                }
                Kind::Days => {
                    let mut v = Vec::new();
                    for (d, on) in self.fields.days.iter().enumerate() {
                        let name = tr!("ag-weekday-short", wd = d as i64 + 1);
                        let box_ = if *on { "☑" } else { "☐" };
                        let st = if is_focus && d == self.day_cursor { s.tab_active() } else if *on { s.accent() } else { s.muted() };
                        v.push(Span::styled(format!("{box_}{name}"), st));
                        v.push(Span::raw(" "));
                    }
                    if !self.fields.days.iter().any(|x| *x) {
                        v.push(Span::styled(tr!("rf-days-all"), s.muted()));
                    }
                    v
                }
                _ => {
                    let t = self.text(*f);
                    if t.is_empty() && !is_focus {
                        vec![Span::styled("—", s.muted())]
                    } else {
                        vec![Span::raw(t)]
                    }
                }
            };
            if is_focus {
                focus_line = lines.len();
            }
            let mut spans = vec![mark, label];
            spans.extend(value);
            let text_input = is_focus && matches!(widget(*f), Kind::Text | Kind::Playlist);
            lines.push((Line::from(spans), text_input.then_some(label_w + 3)));
            if is_focus {
                if let Some(h) = self.hint(*f) {
                    lines.push((Line::styled(format!("   {h}"), s.muted()), None));
                }
                for d in &row_diags {
                    lines.push((Line::styled(format!("   ↳ {}", diag_text(d)), s.error()), None));
                }
            }
        }
        // Problèmes de la règle sans champ du formulaire (kind…), puis du reste
        // de la grille (ils empêchent aussi l'enregistrement).
        let rule_level: Vec<&GridDiagnostic> = self
            .diags
            .iter()
            .filter(mine)
            .filter(|d| d.field_path.strip_prefix(&format!("{prefix}.")).and_then(field_of).is_none())
            .collect();
        let others: Vec<&GridDiagnostic> = self.diags.iter().filter(|d| !mine(d)).collect();
        if !rule_level.is_empty() || !others.is_empty() {
            lines.push((Line::default(), None));
        }
        for d in rule_level {
            lines.push((Line::styled(format!(" ✗ {}", diag_text(d)), s.error()), None));
        }
        if !others.is_empty() {
            lines.push((Line::styled(format!(" {}", tr!("rf-other-problems")), s.warn()), None));
            for d in others {
                let at = if d.rule_id.is_empty() { d.field_path.clone() } else { d.rule_id.clone() };
                lines.push((Line::styled(format!("   ✗ {at} : {}", diag_text(d)), s.error()), None));
            }
        }
        let h = inner.height as usize;
        let skip = (focus_line + 3).saturating_sub(h);
        let mut cursor = None;
        for (n, (line, input_x)) in lines.into_iter().skip(skip).take(h).enumerate() {
            let row_a = Rect::new(inner.x, inner.y + n as u16, inner.width, 1);
            Paragraph::new(line).render(row_a, buf);
            if let Some(x) = input_x
                && self.picker.is_none()
            {
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

    /// La journée avec la modification : bases, et ce que fait CETTE règle.
    fn render_side(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(s.border())
            .title(Span::styled(format!(" {} ", tr!("rf-preview")), s.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        let mut lines = Vec::new();
        match &self.preview {
            None => lines.push(Line::styled(tr!("media-loading"), s.muted())),
            // Brouillon refusé : la projection attend qu'il soit valide.
            Some(Err(_)) if !self.diags.is_empty() => lines.push(Line::styled(tr!("rf-preview-invalid"), s.warn())),
            Some(Err(e)) => lines.push(Line::styled(e.clone(), s.error())),
            Some(Ok(p)) => {
                let id = self.fields.id.trim();
                lines.push(Line::styled(tr!("rf-preview-bases"), s.label()));
                for b in &p.bands {
                    let name = if b.playlist_ref.is_empty() { tr!("ag-origin-fallback") } else { b.playlist_ref.clone() };
                    let st = if b.rule_id == id { s.accent() } else { s.base() };
                    lines.push(Line::styled(
                        format!("  {}–{}  {name}  [{}]", agenda::hm(b.start, &self.tz, false), agenda::hm(b.end, &self.tz, false), b.rule_id),
                        st,
                    ));
                }
                let mine: Vec<String> = p
                    .marks
                    .iter()
                    .filter(|m| m.rule_id == id)
                    .map(|m| format!("{}{}", m.kind.glyph(), agenda::hm(m.at, &self.tz, false)))
                    .collect();
                let live: Vec<String> = p
                    .live
                    .iter()
                    .filter(|l| l.rule_id == id)
                    .map(|l| {
                        let c = l.closes.map(|c| agenda::hm(c, &self.tz, false)).unwrap_or_else(|| "…".into());
                        format!("♪{}–{c}", agenda::hm(l.opens, &self.tz, false))
                    })
                    .collect();
                let in_bands = p.bands.iter().any(|b| b.rule_id == id);
                lines.push(Line::default());
                if mine.is_empty() && live.is_empty() && !in_bands {
                    lines.push(Line::styled(tr!("rf-preview-none"), s.warn()));
                } else if !mine.is_empty() || !live.is_empty() {
                    lines.push(Line::styled(tr!("rf-preview-marks", n = (mine.len() + live.len()) as i64), s.label()));
                    let all: Vec<String> = live.into_iter().chain(mine).collect();
                    lines.push(Line::styled(format!("  {}", all.join("  ")), s.accent()));
                }
                let others = p.marks.iter().filter(|m| m.rule_id != id && m.kind != MarkKind::Live).count();
                lines.push(Line::styled(tr!("rf-preview-others", n = others as i64), s.muted()));
            }
        }
        Paragraph::new(lines).wrap(Wrap { trim: false }).render(inner, buf);
    }
}
