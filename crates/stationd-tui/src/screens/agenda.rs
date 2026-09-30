//! Agenda (`4`) — dossier §5.4, lot 6a (lecture).
//!
//! Trois vues de la même lecture (`rpc::agenda_read` : `Preview` de la
//! période, `ListRules`, `CheckCoverage`) :
//! - **Jour** : timeline de minuit à minuit (fuseau station), bandes de base,
//!   repères (`!` hard, `*` soft, `~` every projeté, `♪` live), inspecteur du
//!   créneau choisi (règle, pool, groupe, verdict de couverture) ;
//! - **Semaine** (`v`) : 7 colonnes au pas choisi, ou la liste des 7 jours
//!   résumés sous 100 colonnes ; `Entrée` ouvre le jour ;
//! - **Couverture** (`c`) : `CheckCoverage` de toute la grille, pire en tête.
//!
//! Tout vient de stationd (projection, validités, verdicts) ; l'écran ne fait
//! que découper selon l'horloge civile (`crate::agenda`). Lire n'a aucun
//! effet : `Preview` et `CheckCoverage` ne touchent à rien.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Error;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rat_salsa::{Control, SalsaContext};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::clear::Clear;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use stationd_proto::schedule::coverage_reason::Code;
use stationd_proto::schedule::decision::Origin;
use stationd_proto::playlist::PlaylistSummary;
use stationd_proto::schedule::{
    CheckCoverageResponse, CoverageEntry, CoverageReason, GetGridResponse, ListGridsResponse, Occurrence, Rule,
    SaveGridResponse, Verdict, rule, every, at_clock,
    group_member,
};

use crate::agenda::{self, Band, Cell, LiveSpan, Mark, MarkKind, Period, Projection, Slot, STEPS};
use super::ruleform::{FormOutcome, RuleForm, kind_label};
use crate::action::Action;
use crate::app::{AppEvent, Global, Handoff};
use crate::dialog::{Confirm, Field, Form, Info, Modal};
use crate::gridraft::{self, KINDS, RuleFields};
use crate::rpc::{AgendaRead, DraftCheck, Read};
use crate::screen::{KeyHelp, Screen};
use crate::style::Styles;
use crate::{fit, k, tr};

/// Largeur à partir de laquelle l'inspecteur est affiché à côté.
const WIDE: u16 = 110;
/// Largeur à partir de laquelle la semaine est en 7 colonnes.
const WEEK_GRID: u16 = 100;

/// Réponses et minuteries de l'agenda et de son éditeur de règle (le
/// premier nombre identifie la requête ou le demandeur).
#[derive(Debug)]
pub enum AgEvent {
    /// Une lecture de la période (n° de requête).
    Read(u64, Box<AgendaRead>),
    /// Le texte d'une grille, pour ce qu'on voulait en faire.
    Text(u64, Purpose, Box<Read<GetGridResponse>>),
    /// Éditeur : fin du délai après une frappe (éditeur, n° de frappe).
    Typed(u64, u64),
    /// Éditeur : diagnostics et projection du brouillon.
    Checked(u64, u64, Box<DraftCheck>),
    Playlists(u64, Read<Vec<PlaylistSummary>>),
    Saved(u64, Box<Read<SaveGridResponse>>),
    /// Éditeur : le fichier relu après un conflit.
    Rebased(u64, Box<Read<GetGridResponse>>),
}

/// Pourquoi le texte d'une grille est lu.
#[derive(Debug, Clone)]
pub enum Purpose {
    /// Nouvelle règle de cette nature, pré-remplie au créneau choisi.
    New { kind: &'static str, time: String, date: String },
    Edit { rule_id: String },
    Delete { rule_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Day,
    Week,
    Coverage,
}

/// Ce qui a été lu, pour une période.
struct Loaded {
    /// La période lue (jour ou semaine).
    period: Period,
    proj: Projection,
    /// Lecture de la projection en échec après une première réussite : les
    /// données gardées sont anciennes (même période).
    stale: Option<String>,
    rules: Result<Vec<Rule>, String>,
    coverage: Result<CheckCoverageResponse, String>,
}

/// Un élément du créneau choisi, dans l'inspecteur.
#[derive(Debug, Clone)]
enum Item {
    Band(Band),
    Mark(Mark),
    Live(LiveSpan),
}

impl Item {
    fn playlist(&self) -> Option<&str> {
        match self {
            Item::Band(b) if !b.playlist_ref.is_empty() => Some(&b.playlist_ref),
            Item::Mark(m) if m.kind != MarkKind::Live && !m.target.is_empty() => Some(&m.target),
            _ => None,
        }
    }
}

pub struct Agenda {
    view: View,
    /// Vue à retrouver en quittant la couverture.
    back: View,
    /// Jour choisi (dans le fuseau station) ; `None` tant qu'il est inconnu.
    date: Option<Date>,
    step: usize,
    /// Créneau choisi (vue jour) ou ligne (semaine).
    cursor: usize,
    /// Première ligne affichée.
    scroll: usize,
    /// Jour choisi dans la semaine (0 = lundi).
    col: usize,
    /// Élément de l'inspecteur qui a le focus (`Tab`), pour `p`.
    item: usize,
    /// Inspecteur en plein écran (terminal étroit).
    inspector: bool,
    /// Placer le curseur sur « maintenant » à la prochaine lecture.
    follow_now: bool,
    cov_sel: usize,
    calendar: Option<Date>,
    data: Option<Loaded>,
    /// Période de la lecture en cours.
    loading: Option<Period>,
    request: u64,
    /// Erreur sans données à montrer (ou fuseau inconnu).
    error: Option<String>,
    /// Largeur du dernier rendu (l'inspecteur s'ouvre en plein écran quand
    /// il n'a pas sa place à côté).
    width: u16,
    /// Heures affichées en UTC (`u`) plutôt qu'en heure de la station.
    utc: bool,
    /// La grille regardée : `None` = la grille appliquée (l'active), sinon un
    /// fichier de grille en préparation.
    grid: Option<String>,
    /// Les grilles du nœud (dernière lecture).
    grids: Option<Result<ListGridsResponse, String>>,
    /// Sélecteur de grille (`G`) : ligne choisie.
    grid_pick: Option<usize>,
    /// Choix de la nature d'une nouvelle règle (`n`) : ligne choisie.
    new_kind: Option<usize>,
    /// Ouverture en attente d'un texte de grille (n° de requête).
    text_req: u64,
    /// Éditeur de règle ouvert.
    form: Option<RuleForm>,
    /// Instant où placer le curseur à la prochaine lecture.
    follow_to: Option<i64>,
    /// Liste des règles de la grille regardée (`l`) : ligne choisie.
    rule_pick: Option<usize>,
}

impl Default for Agenda {
    fn default() -> Self {
        Self {
            view: View::Day,
            back: View::Day,
            date: None,
            step: 1,
            cursor: 0,
            scroll: 0,
            col: 0,
            item: 0,
            inspector: false,
            follow_now: true,
            cov_sel: 0,
            calendar: None,
            data: None,
            loading: None,
            request: 0,
            error: None,
            width: 0,
            utc: false,
            grid: None,
            grids: None,
            grid_pick: None,
            new_kind: None,
            text_req: 0,
            form: None,
            follow_to: None,
            rule_pick: None,
        }
    }
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn today(tz: &TimeZone) -> Date {
    jiff::Timestamp::now().to_zoned(tz.clone()).date()
}

/// Jour de la semaine (1 = lundi) pour les traductions.
fn wd(d: Date) -> i64 {
    i64::from(d.weekday().to_monday_one_offset())
}

fn date_long(d: Date) -> String {
    tr!("ag-date-long", wd = wd(d), date = d.strftime("%d/%m/%Y").to_string())
}

fn date_short(d: Date) -> String {
    tr!("ag-date-short", wd = wd(d), date = d.strftime("%d/%m").to_string())
}

fn dur(ms: u64) -> String {
    crate::store::human_duration(Duration::from_millis(ms))
}

impl Agenda {
    /// Le fuseau de l'affichage : celui de la station, ou UTC (`u`).
    fn tz(&self, ctx: &Global) -> Option<TimeZone> {
        if self.utc { ctx.store.tz.as_ref().map(|_| TimeZone::UTC) } else { ctx.store.tz.clone() }
    }

    /// La période de la vue courante.
    fn period(&self, tz: &TimeZone) -> Option<Period> {
        let d = self.date?;
        match self.view {
            View::Day => agenda::day(d, tz),
            View::Week | View::Coverage => match self.back_or_view() {
                View::Day => agenda::day(d, tz),
                _ => agenda::week(d, tz),
            },
        }
    }

    /// La vue dont la période est lue (la couverture garde celle d'avant).
    fn back_or_view(&self) -> View {
        if self.view == View::Coverage { self.back } else { self.view }
    }

    fn load(&mut self, ctx: &mut Global) {
        let Some(tz) = self.tz(ctx) else {
            self.error = Some(tr!("ag-no-tz"));
            return;
        };
        if self.date.is_none() {
            self.date = Some(today(&tz));
        }
        let Some(period) = self.period(&tz) else {
            self.error = Some(tr!("ag-bad-date"));
            return;
        };
        self.error = None;
        self.request += 1;
        self.loading = Some(period);
        let (id, channel) = (self.request, ctx.channel.clone());
        let (from, window) = period.request();
        let grid = self.grid.clone().unwrap_or_default();
        ctx.spawn_async(async move {
            let read = crate::rpc::agenda_read(channel, from, window, grid).await;
            Ok(Control::Event(AppEvent::Agenda(Box::new(AgEvent::Read(id, Box::new(read))))))
        });
    }

    fn on_read(&mut self, read: &AgendaRead, ctx: &mut Global) {
        let Some(period) = self.loading.take() else { return };
        self.grids = Some(read.grids.clone());
        // La grille regardée est devenue l'active (activée ici ou ailleurs).
        if let (Some(g), Ok(l)) = (&self.grid, &read.grids)
            && *g == l.active
        {
            self.grid = None;
        }
        let same = self.data.as_ref().is_some_and(|d| d.period == period);
        match &read.preview {
            Ok(p) => {
                self.data = Some(Loaded {
                    period,
                    proj: agenda::project(p.clone(), period),
                    stale: None,
                    rules: read.rules.clone(),
                    coverage: read.coverage.clone(),
                });
                self.error = None;
            }
            // Même période : on garde l'ancienne lecture, marquée ancienne.
            Err(e) if same => {
                if let Some(d) = self.data.as_mut() {
                    d.stale = Some(e.clone());
                    if read.coverage.is_ok() {
                        d.coverage = read.coverage.clone();
                    }
                }
            }
            Err(e) => {
                self.data = None;
                self.error = Some(e.clone());
            }
        }
        if self.follow_now {
            self.follow_now = false;
            self.cursor_to(now(), ctx);
        }
        if let Some(t) = self.follow_to.take() {
            self.cursor_to(t, ctx);
        }
        let n = self.rows(ctx);
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    /// Place le curseur sur l'instant `t` (s'il est dans la période).
    fn cursor_to(&mut self, t: i64, ctx: &Global) {
        let Some(tz) = self.tz(ctx) else { return };
        let step = STEPS[self.step];
        match self.back_or_view() {
            View::Day => {
                if let Some(p) = self.period(&tz)
                    && let Some(i) = agenda::slots(p, step).iter().position(|s| agenda::contains(*s, t))
                {
                    self.cursor = i;
                    self.scroll = i.saturating_sub(4);
                }
            }
            _ => {
                if let Some(d) = agenda::date_of(t, &tz) {
                    if let Some(date) = self.date
                        && agenda::monday(d) == agenda::monday(date)
                    {
                        self.col = d.weekday().to_monday_zero_offset() as usize;
                    }
                    let z = jiff::Timestamp::from_second(t).map(|x| x.to_zoned(tz.clone())).ok();
                    if let Some(z) = z {
                        let minutes = i64::from(z.hour()) * 60 + i64::from(z.minute());
                        self.cursor = (minutes / step) as usize;
                        self.scroll = self.cursor.saturating_sub(4);
                    }
                }
            }
        }
    }

    /// Nombre de lignes navigables de la vue.
    fn rows(&self, ctx: &Global) -> usize {
        let Some(tz) = self.tz(ctx) else { return 0 };
        let step = STEPS[self.step];
        match self.view {
            View::Day => self.period(&tz).map(|p| agenda::slots(p, step).len()).unwrap_or(0),
            View::Week => agenda::week_rows(step).len(),
            View::Coverage => self.coverage_rows().len(),
        }
    }

    fn go_date(&mut self, d: Date, ctx: &mut Global) {
        self.date = Some(d);
        self.item = 0;
        self.load(ctx);
    }

    fn shift(&mut self, n: i64, ctx: &mut Global) {
        let Some(d) = self.date else { return };
        let days = if self.back_or_view() == View::Day { n } else { 7 * n };
        self.go_date(agenda::shift(d, days), ctx);
    }

    fn coverage_map(&self) -> HashMap<&str, &CoverageEntry> {
        match self.data.as_ref().map(|d| &d.coverage) {
            Some(Ok(c)) => c.entries.iter().map(|e| (e.rule_id.as_str(), e)).collect(),
            _ => HashMap::new(),
        }
    }

    fn rule_map(&self) -> HashMap<&str, &Rule> {
        match self.data.as_ref().map(|d| &d.rules) {
            Some(Ok(r)) => r.iter().map(|x| (x.id.as_str(), x)).collect(),
            _ => HashMap::new(),
        }
    }

    /// Entrées de couverture, pire verdict en tête.
    fn coverage_rows(&self) -> Vec<&CoverageEntry> {
        let Some(Ok(c)) = self.data.as_ref().map(|d| &d.coverage) else { return Vec::new() };
        let mut v: Vec<&CoverageEntry> = c.entries.iter().collect();
        v.sort_by(|a, b| b.verdict.cmp(&a.verdict).then(a.rule_id.cmp(&b.rule_id)));
        v
    }

    /// Les éléments du créneau choisi (vue jour).
    fn items(&self, ctx: &Global) -> Vec<Item> {
        let (Some(tz), Some(d)) = (self.tz(ctx), self.data.as_ref()) else { return Vec::new() };
        if self.view != View::Day {
            return Vec::new();
        }
        let Some(p) = self.period(&tz) else { return Vec::new() };
        let Some(slot) = agenda::slots(p, STEPS[self.step]).get(self.cursor).copied() else { return Vec::new() };
        slot_items(&d.proj, slot)
    }

    /// Couleur de chaque playlist : dans l'ordre d'apparition des bandes.
    fn colors(&self) -> HashMap<String, usize> {
        let mut out = HashMap::new();
        if let Some(d) = &self.data {
            for b in &d.proj.bands {
                let n = out.len();
                out.entry(b.playlist_ref.clone()).or_insert(n);
            }
        }
        out
    }

    /// Les règles de la grille regardée, dans l'ordre de `ListRules`.
    fn rule_ids(&self) -> Vec<String> {
        match self.data.as_ref().map(|d| &d.rules) {
            Some(Ok(r)) => r.iter().map(|x| x.id.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Les grilles du nœud : (nom, active).
    fn grid_list(&self) -> Vec<(String, bool)> {
        match &self.grids {
            Some(Ok(g)) => g.grids.iter().map(|x| (x.name.clone(), x.active)).collect(),
            _ => Vec::new(),
        }
    }

    /// Nom de la grille regardée (l'active quand `grid` est vide).
    fn grid_name(&self) -> String {
        match (&self.grid, &self.grids) {
            (Some(g), _) => g.clone(),
            (None, Some(Ok(l))) => l.active.clone(),
            _ => String::new(),
        }
    }

    /// La règle de l'élément choisi dans l'inspecteur (le filet de sécurité
    /// n'en a pas).
    fn focused_rule(&self, ctx: &Global) -> Option<String> {
        let items = self.items(ctx);
        let it = items.get(self.item % items.len().max(1))?;
        let id = match it {
            Item::Band(b) => b.rule_id.clone(),
            Item::Mark(m) => m.rule_id.clone(),
            Item::Live(l) => l.rule_id.clone(),
        };
        (!id.is_empty()).then_some(id)
    }

    /// Lit le texte de la grille regardée pour `purpose`.
    fn fetch_text(&mut self, purpose: Purpose, ctx: &mut Global) {
        self.text_req += 1;
        let (id, channel, name) = (self.text_req, ctx.channel.clone(), self.grid_name());
        ctx.spawn_async(async move {
            let r = crate::rpc::grid_text(channel, name).await;
            Ok(Control::Event(AppEvent::Agenda(Box::new(AgEvent::Text(id, purpose, Box::new(r))))))
        });
    }

    /// Nouvelle règle au créneau choisi : son heure et son jour, en heure de
    /// la STATION (la grammaire de la grille), quel que soit l'affichage.
    fn start_new(&mut self, kind: &'static str, ctx: &mut Global) {
        let Some(station) = ctx.store.tz.clone() else { return };
        let Some(t) = self.cursor_time(ctx) else { return };
        let (time, date) = match jiff::Timestamp::from_second(t) {
            Ok(ts) => {
                let z = ts.to_zoned(station);
                (z.strftime("%H:%M").to_string(), z.strftime("%Y-%m-%d").to_string())
            }
            Err(_) => return,
        };
        self.fetch_text(Purpose::New { kind, time, date }, ctx);
    }

    fn on_text(&mut self, purpose: &Purpose, r: &Read<GetGridResponse>, ctx: &mut Global) {
        let g = match r {
            Ok(g) => g,
            Err(e) => {
                ctx.open(Modal::Info(Info { title: tr!("ag-grid-unreadable"), lines: vec![e.clone()] }));
                return;
            }
        };
        let doc = match gridraft::parse(&g.toml) {
            Ok(d) => d,
            Err(e) => {
                ctx.open(Modal::Info(Info {
                    title: tr!("ag-grid-unreadable"),
                    lines: vec![tr!("ag-grid-toml-broken", grid = g.name.clone()), e],
                }));
                return;
            }
        };
        // La journée projetée dans l'éditeur : le jour choisi, quelle que soit la vue.
        let Some(tz) = self.tz(ctx) else { return };
        let Some(day) = self.date.and_then(|d| agenda::day(d, &tz)) else { return };
        match purpose {
            Purpose::New { kind, time, date } => {
                let mut f = RuleFields::new(kind);
                let base = format!("evt-{}-{}", date.replace('-', ""), time.replace(':', ""));
                f.id = gridraft::free_id(&doc, &base);
                match *kind {
                    "day_part" | "live" => f.start = time.clone(),
                    "at_clock" => f.anchor_value = time.clone(),
                    _ => {}
                }
                // Aucune date imposée : une règle sans dates vaut tous les
                // jours ; pour un event ponctuel, remplir début = fin.
                self.form = Some(RuleForm::open(g, None, f, day, tz, ctx));
            }
            Purpose::Edit { rule_id } => match gridraft::index_of(&doc, rule_id) {
                Some(i) => {
                    let f = gridraft::read_at(&doc, i).unwrap_or_default();
                    self.form = Some(RuleForm::open(g, Some(i), f, day, tz, ctx));
                }
                None => ctx.open(Modal::Info(Info {
                    title: tr!("ag-rule-missing-title"),
                    lines: vec![tr!("ag-rule-missing", rule = rule_id.clone(), grid = g.name.clone())],
                })),
            },
            Purpose::Delete { rule_id } => match gridraft::without_rule(&g.toml, rule_id) {
                Ok(toml) => ctx.open(Modal::Confirm(
                    Confirm::new(
                        tr!("ag-delete-title"),
                        vec![
                            tr!("ag-delete-body", rule = rule_id.clone(), grid = g.name.clone()),
                            if g.active { tr!("ag-delete-active") } else { tr!("ag-delete-other") },
                        ],
                        tr!("ag-delete-yes"),
                        Action::SaveGrid {
                            name: g.name.clone(),
                            toml,
                            revision: g.revision.clone(),
                            done: tr!("done-rule-deleted", rule = rule_id.clone(), grid = g.name.clone()),
                        },
                    )
                    .danger(),
                )),
                Err(_) => ctx.open(Modal::Info(Info {
                    title: tr!("ag-rule-missing-title"),
                    lines: vec![tr!("ag-rule-missing", rule = rule_id.clone(), grid = g.name.clone())],
                })),
            },
        }
    }

    fn form_outcome(&mut self, out: FormOutcome, ctx: &mut Global) {
        match out {
            FormOutcome::Stay => {}
            FormOutcome::Close => self.form = None,
            FormOutcome::Saved(msg) => {
                self.form = None;
                ctx.set_status(msg);
                self.load(ctx);
            }
        }
    }

    fn goto_playlist(&self, reference: &str, ctx: &mut Global) {
        ctx.switch_to(super::PLAYLISTS, Some(Handoff::Select { reference: reference.to_string() }));
    }
}

/// Ce qui occupe un créneau : la base à son début, les bases qui commencent
/// dedans, ses repères, les fenêtres live qui le touchent.
fn slot_items(proj: &Projection, slot: Slot) -> Vec<Item> {
    let mut out = Vec::new();
    if let Some(b) = proj.band_at(slot.start) {
        out.push(Item::Band(b.clone()));
    }
    for b in proj.bands_starting(slot.start + 1, slot.end) {
        out.push(Item::Band(b.clone()));
    }
    for m in proj.marks_in(slot.start, slot.end) {
        if m.kind != MarkKind::Live {
            out.push(Item::Mark(m.clone()));
        }
    }
    for l in proj.live_in(slot.start, slot.end) {
        out.push(Item::Live(l.clone()));
    }
    out
}

// --- textes ----------------------------------------------------------------------

fn origin_text(o: Origin) -> String {
    match o {
        Origin::DayPart => tr!("ag-origin-daypart"),
        Origin::BaseRotation => tr!("ag-origin-base"),
        Origin::AtClockHard => tr!("ag-origin-hard"),
        Origin::AtClockSoft => tr!("ag-origin-soft"),
        Origin::Every => tr!("ag-origin-every"),
        _ => tr!("ag-origin-fallback"),
    }
}

fn kind_text(kind: &str) -> String {
    match kind {
        "base_rotation" => tr!("ag-origin-base"),
        "day_part" => tr!("ag-origin-daypart"),
        "at_clock" => tr!("ag-kind-at-clock"),
        "every" => tr!("ag-origin-every"),
        "live" => tr!("ag-kind-live"),
        other => other.to_string(),
    }
}

fn wall(w: &Option<stationd_proto::schedule::WallClock>) -> String {
    w.as_ref().map(|w| format!("{:02}:{:02}", w.hour, w.minute)).unwrap_or_else(|| "—".into())
}

/// Un `every_minutes` : le pas et son premier repère (N min après minuit).
fn every_marks(n: u32) -> String {
    tr!("ag-rule-at-step", n = i64::from(n), first = format!("{:02}:{:02}", n / 60, n % 60))
}

/// Ce que dit une règle (lue par `ListRules`), en une ligne.
fn rule_text(r: &Rule) -> String {
    let mut parts = Vec::new();
    match &r.kind {
        Some(rule::Kind::BaseRotation(_)) => parts.push(tr!("ag-rule-base")),
        Some(rule::Kind::DayPart(d)) => parts.push(match &d.end {
            Some(_) => tr!("ag-rule-daypart", start = wall(&d.start), end = wall(&d.end)),
            None => tr!("ag-rule-daypart-open", start = wall(&d.start)),
        }),
        Some(rule::Kind::AtClock(a)) => {
            parts.push(match (a.every_minutes, a.minute) {
                (n @ 1.., _) => every_marks(n),
                (_, Some(m)) => tr!("ag-rule-at-hourly", marks = format!(":{m:02}")),
                _ => tr!("ag-rule-at", at = wall(&a.at)),
            });
            parts.push(if a.mode == at_clock::Mode::Hard as i32 { tr!("ag-hard") } else { tr!("ag-soft") });
            if let Some(e) = &a.expiry {
                parts.push(tr!("ag-rule-expiry", d = dur(e.seconds.max(0) as u64 * 1000)));
            }
        }
        Some(rule::Kind::Every(e)) => parts.push(match &e.cadence {
            Some(every::Cadence::Elapsed(d)) => tr!("ag-rule-every-elapsed", d = dur(d.seconds.max(0) as u64 * 1000)),
            Some(every::Cadence::Tracks(n)) => tr!("ag-rule-every-tracks", n = i64::from(*n)),
            None => "—".into(),
        }),
        Some(rule::Kind::Live(l)) => parts.push(tr!("ag-rule-live", dj = l.dj.clone(), start = wall(&l.start))),
        None => {}
    }
    if let Some(v) = &r.validity {
        if !v.days.is_empty() && v.days.len() < 7 {
            let days: Vec<String> = v.days.iter().map(|d| tr!("ag-weekday-short", wd = i64::from(*d))).collect();
            parts.push(days.join(" "));
        }
        match (v.date_start.is_empty(), v.date_end.is_empty()) {
            (false, false) => parts.push(tr!("ag-rule-dates", start = v.date_start.clone(), end = v.date_end.clone())),
            (false, true) => parts.push(tr!("ag-rule-from", start = v.date_start.clone())),
            (true, false) => parts.push(tr!("ag-rule-until", end = v.date_end.clone())),
            (true, true) => {}
        }
    }
    if !r.enabled {
        parts.push(tr!("ag-rule-disabled"));
    }
    parts.join(" · ")
}

/// Une cause de verdict (opcode de stationd), traduite.
pub fn reason_text(r: &CoverageReason) -> String {
    let ms = |v: Option<u64>| v.map(dur).unwrap_or_else(|| "—".into());
    let n = |v: Option<u64>| v.map(|x| x as i64).unwrap_or(0);
    match Code::try_from(r.code).unwrap_or(Code::Unspecified) {
        Code::PoolEmpty => tr!("cov-pool-empty"),
        Code::TrackRepeat => tr!("cov-track-repeat", window = r.window.clone(), pool = ms(r.pool_ms)),
        Code::TitleRepeat => tr!("cov-title-repeat", window = r.window.clone(), pool = ms(r.pool_ms)),
        Code::ArtistRepeat => tr!("cov-artist-repeat", n = n(r.count)),
        Code::ArtistNotEvaluated => tr!("cov-artist-not-evaluated"),
        Code::LimitUnmet => tr!("cov-limit-unmet", limit = n(r.limit), n = n(r.count)),
        Code::FiniteShort => tr!("cov-finite-short", pool = ms(r.pool_ms), need = ms(r.need_ms)),
        Code::MembersEmptyAbort => tr!("cov-members-empty-abort", list = r.refs.join(", ")),
        Code::MembersEmptySkip => tr!("cov-members-empty-skip", list = r.refs.join(", ")),
        Code::MembersLoop => tr!("cov-members-loop", list = r.refs.join(", ")),
        Code::BadRef => tr!("cov-bad-ref", reason = r.error.clone()),
        Code::UnknownPlaylist => tr!("cov-unknown-playlist"),
        Code::UnreadablePlaylist => tr!("cov-unreadable-playlist", reason = r.error.clone()),
        Code::Unresolvable => tr!("cov-unresolvable", reason = r.error.clone()),
        Code::RuntimeLoop => tr!("cov-runtime-loop", need = ms(r.need_ms), pool = ms(r.pool_ms)),
        Code::TakeRepeat => tr!("cov-take-repeat", take = n(r.limit), n = n(r.count)),
        Code::Unspecified => tr!("cov-unknown", code = r.code),
    }
}

fn verdict_span(v: i32, s: &Styles) -> Span<'static> {
    match Verdict::try_from(v).unwrap_or(Verdict::Unspecified) {
        Verdict::Ok => Span::styled(format!("✓ {}", tr!("cov-ok")), s.ok()),
        Verdict::Thin => Span::styled(format!("⚠ {}", tr!("cov-thin")), s.warn()),
        Verdict::Insufficient => Span::styled(format!("✗ {}", tr!("cov-insufficient")), s.error()),
        Verdict::Unspecified => Span::styled("—", s.muted()),
    }
}

/// Le verdict en colonne : même largeur quelle que soit la langue.
fn verdict_cell(v: i32, s: &Styles) -> Span<'static> {
    let w = [tr!("cov-ok"), tr!("cov-thin"), tr!("cov-insufficient")].iter().map(|t| t.chars().count()).max().unwrap_or(0) + 2;
    let mut span = verdict_span(v, s);
    span.content = format!("{:<w$}", span.content).into();
    span
}

fn pool_span(count: Option<u64>, duration: Option<&stationd_proto::prost_types::Duration>, s: &Styles) -> Span<'static> {
    let d = duration.map(|d| dur(d.seconds.max(0) as u64 * 1000 + u64::from(d.nanos.max(0) as u32 / 1_000_000)));
    match count {
        None => Span::styled(tr!("ag-pool-unmeasured"), s.muted()),
        Some(0) => Span::styled(tr!("ed-pool-empty"), s.error()),
        Some(n) => {
            let mut t = tr!("ed-pool-count", n = n);
            if let Some(d) = d {
                t.push_str(&format!(" · {d}"));
            }
            Span::styled(t, s.label())
        }
    }
}

/// Lignes de détail communes : règle, pool, groupe, couverture.
fn detail_lines(
    rule_id: &str,
    occ: Option<&Occurrence>,
    rules: &HashMap<&str, &Rule>,
    cov: &HashMap<&str, &CoverageEntry>,
    s: &Styles,
    out: &mut Vec<Line<'static>>,
) {
    let pad = "   ";
    if !rule_id.is_empty() {
        let desc = rules.get(rule_id).map(|r| rule_text(r)).unwrap_or_default();
        out.push(Line::from(vec![
            Span::raw(pad),
            Span::styled(format!("{} ", tr!("ag-rule")), s.label()),
            Span::styled(rule_id.to_string(), s.accent()),
            Span::styled(if desc.is_empty() { String::new() } else { format!("  {desc}") }, s.muted()),
        ]));
    }
    if let Some(o) = occ {
        out.push(Line::from(vec![
            Span::raw(pad),
            Span::styled(format!("{} ", tr!("pl-pool")), s.label()),
            pool_span(o.selected_count, o.total_duration.as_ref(), s),
        ]));
        if !o.strategy.is_empty() {
            out.push(Line::from(vec![
                Span::raw(pad),
                Span::styled(format!("{} ", tr!("ag-group")), s.label()),
                Span::raw(o.strategy.clone()),
            ]));
            let n = o.members.len();
            for (i, m) in o.members.iter().enumerate() {
                let branch = if i + 1 == n { '└' } else { '├' };
                let quota = match &m.quota {
                    Some(group_member::Quota::Take(t)) => tr!("ag-take", n = i64::from(*t)),
                    Some(group_member::Quota::Runtime(d)) => tr!("ag-runtime", d = dur(d.seconds.max(0) as u64 * 1000)),
                    None => String::new(),
                };
                let at = m.offset.as_ref().map(|d| format!(" @+{}", dur(d.seconds.max(0) as u64 * 1000))).unwrap_or_default();
                out.push(Line::from(vec![
                    Span::raw(format!("{pad}  {branch} ")),
                    Span::raw(m.r#ref.clone()),
                    Span::styled(format!("  {quota}{at}  "), s.muted()),
                    pool_span(m.selected_count, m.total_duration.as_ref(), s),
                ]));
            }
        }
    }
    if let Some(e) = cov.get(rule_id) {
        out.push(Line::from(vec![
            Span::raw(pad),
            Span::styled(format!("{} ", tr!("ag-coverage")), s.label()),
            verdict_span(e.verdict, s),
        ]));
        for r in &e.reasons {
            out.push(Line::styled(format!("{pad}  · {}", reason_text(r)), s.warn()));
        }
        for m in e.members.iter().filter(|m| !m.reasons.is_empty()) {
            for r in &m.reasons {
                out.push(Line::styled(format!("{pad}  · {} : {}", m.r#ref, reason_text(r)), s.warn()));
            }
        }
    }
}

// --- rendu -----------------------------------------------------------------------

impl Agenda {
    fn header(&self, area: Rect, buf: &mut Buffer, ctx: &Global, s: &Styles) {
        let Some(tz) = self.tz(ctx) else { return };
        let step = STEPS[self.step];
        let mut spans = Vec::new();
        let title = match (self.view, self.back_or_view(), self.date) {
            (View::Coverage, ..) => String::new(),
            (_, View::Day, Some(d)) => date_long(d),
            (_, _, Some(d)) => {
                let m = agenda::monday(d);
                tr!("ag-week-of", from = date_short(m), to = date_short(agenda::shift(m, 6)))
            }
            _ => String::new(),
        };
        let view = match self.view {
            View::Day => tr!("ag-view-day"),
            View::Week => tr!("ag-view-week"),
            View::Coverage => tr!("ag-view-coverage"),
        };
        spans.push(Span::styled(format!(" {view} "), s.tab_active()));
        spans.push(Span::styled(format!(" {title}"), s.title()));
        // La grille regardée : l'active, ou une grille en préparation (visible).
        let name = self.grid_name();
        if !name.is_empty() {
            match &self.grid {
                None => spans.push(Span::styled(format!("  · {}", tr!("ag-grid-active", grid = name)), s.muted())),
                Some(_) => spans.push(Span::styled(format!("  {} ", tr!("ag-grid-other", grid = name)), s.warn())),
            }
        }

        let station = ctx.store.tz_name.clone().unwrap_or_default();
        if self.utc {
            spans.push(Span::styled(format!("  {} ", tr!("ag-utc", tz = station.clone())), s.tab_active()));
        }
        let tz_name = if self.utc { String::new() } else { format!(" · {station}") };
        let info = if self.view == View::Coverage {
            tz_name
        } else {
            format!(" {tz_name} · {}", tr!("ag-step", n = step))
        };
        spans.push(Span::styled(info, s.muted()));
        if let Some(p) = self.data.as_ref().map(|d| d.period)
            && self.view != View::Coverage
            && agenda::has_transition(p, &tz)
        {
            let hours = (p.to - p.from) / 3600;
            let t = if self.view == View::Day { tr!("ag-dst", h = hours) } else { tr!("ag-dst-week") };
            spans.push(Span::styled(format!("  {t}"), s.warn()));
        }
        if self.loading.is_some() {
            spans.push(Span::styled(format!("  {}", tr!("media-loading")), s.muted()));
        }
        if let Some(e) = self.data.as_ref().and_then(|d| d.stale.as_ref()) {
            spans.push(Span::styled(format!("  {}", tr!("ag-stale", reason = e.clone())), s.warn()));
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }

    fn render_day(&mut self, area: Rect, buf: &mut Buffer, ctx: &Global, s: &Styles) {
        let Some(tz) = self.tz(ctx) else { return };
        let Some(d) = self.data.as_ref() else { return };
        let step = STEPS[self.step];
        let p = d.period;
        let slots = agenda::slots(p, step);
        let with_off = agenda::has_transition(p, &tz);
        let colors = self.colors();
        let h = area.height as usize;
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + h {
            self.scroll = self.cursor + 1 - h;
        }
        // Pas de lignes vides en bas tant que la journée a de quoi remplir.
        self.scroll = self.scroll.min(slots.len().saturating_sub(h));
        let t_now = now();
        let label_w: usize = if with_off { 9 } else { 6 };
        let name_w = ((area.width as usize).saturating_sub(label_w + 6) * 2 / 5).clamp(10, 28);
        let mut lines = Vec::new();
        for (i, slot) in slots.iter().enumerate().skip(self.scroll).take(h) {
            let selected = i == self.cursor;
            let is_now = agenda::contains(*slot, t_now);
            let mut spans = vec![Span::styled(if is_now { "▶" } else { " " }, s.accent())];
            let label = agenda::hm(slot.start, &tz, with_off);
            spans.push(Span::styled(
                format!("{label:<label_w$}"),
                if selected { s.tab_active() } else if is_now { s.accent() } else { s.label() },
            ));
            // Bande : couleur de la base au début du créneau.
            let band = d.proj.band_at(slot.start);
            let bstyle = match band {
                Some(b) if b.origin == Origin::Fallback || b.playlist_ref.is_empty() => s.band_fallback(),
                Some(b) => s.band(colors.get(&b.playlist_ref).copied().unwrap_or(0)),
                None => s.muted(),
            };
            spans.push(Span::styled("  ", bstyle));
            let live = !d.proj.live_in(slot.start, slot.end).is_empty();
            spans.push(Span::styled(if live { "♪" } else { " " }, s.accent()));
            // Nom de la base là où elle commence (et en tête d'écran).
            let starting: Vec<&Band> = d.proj.bands.iter().filter(|b| agenda::contains(*slot, b.start)).collect();
            let name = if let Some(b) = starting.last() {
                let mut t = band_name(b);
                if b.start != slot.start {
                    t = format!("{t} {}", agenda::hm(b.start, &tz, false));
                }
                if starting.len() > 1 {
                    t = format!("{t} +{}", starting.len() - 1);
                }
                t
            } else if i == self.scroll {
                band.map(|b| format!("│ {}", band_name(b))).unwrap_or_default()
            } else {
                String::new()
            };
            spans.push(Span::styled(format!(" {:<name_w$}", fit::ellipsize(&name, name_w)), s.base()));
            // Repères du créneau.
            let marks = d.proj.marks_in(slot.start, slot.end);
            let room = (area.width as usize).saturating_sub(label_w + name_w + 6);
            let segs: Vec<Vec<Span>> = marks
                .iter()
                .map(|m| {
                    vec![
                        Span::styled(m.kind.glyph().to_string(), mark_style(m.kind, s)),
                        Span::raw(format!("{} {}", agenda::hm(m.at, &tz, false), m.target)),
                    ]
                })
                .collect();
            let shown = fit::segments(segs.clone(), Span::raw("  "), room.saturating_sub(4));
            let n_shown = shown.spans.iter().filter(|x| ["!", "*", "~", "♪"].contains(&x.content.as_ref())).count();
            spans.extend(shown.spans);
            if marks.len() > n_shown && !marks.is_empty() {
                spans.push(Span::styled(format!(" +{}", marks.len() - n_shown), s.warn()));
            }
            let mut line = Line::from(spans);
            if selected {
                line = line.style(Style::default().add_modifier(ratatui_core::style::Modifier::BOLD));
            }
            lines.push(line);
        }
        Paragraph::new(lines).render(area, buf);
    }

    fn render_inspector(&self, area: Rect, buf: &mut Buffer, ctx: &Global, s: &Styles) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(s.border())
            .title(Span::styled(format!(" {} ", tr!("ag-inspector")), s.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        let (Some(tz), Some(d)) = (self.tz(ctx), self.data.as_ref()) else { return };
        let Some(slot) = agenda::slots(d.period, STEPS[self.step]).get(self.cursor).copied() else { return };
        let with_off = agenda::has_transition(d.period, &tz);
        let rules = self.rule_map();
        let cov = self.coverage_map();
        let mut lines = vec![Line::styled(
            format!(
                "{} – {}  ({})",
                agenda::hm(slot.start, &tz, with_off),
                agenda::hm(slot.end, &tz, with_off),
                crate::store::local_day_hm(Some(&tz), slot.start).unwrap_or_default().chars().take(5).collect::<String>()
            ),
            s.title(),
        )];
        let items = slot_items(&d.proj, slot);
        if items.is_empty() {
            lines.push(Line::styled(tr!("ag-slot-empty"), s.muted()));
        }
        for (i, it) in items.iter().enumerate() {
            lines.push(Line::default());
            let focus = if i == self.item % items.len().max(1) { "▸ " } else { "  " };
            match it {
                Item::Band(b) => {
                    lines.push(Line::from(vec![
                        Span::styled(focus, s.accent()),
                        Span::styled(format!("■ {}", band_name(b)), s.title()),
                        Span::styled(format!("  {}", origin_text(b.origin)), s.muted()),
                    ]));
                    lines.push(Line::styled(
                        format!(
                            "   {} – {}",
                            agenda::hm(b.start, &tz, with_off),
                            if b.end >= d.period.to { tr!("ag-until-end") } else { agenda::hm(b.end, &tz, with_off) }
                        ),
                        s.muted(),
                    ));
                    detail_lines(&b.rule_id, d.proj.occurrences.get(b.occ), &rules, &cov, s, &mut lines);
                }
                Item::Mark(m) => {
                    let kind = match m.kind {
                        MarkKind::Hard => tr!("ag-origin-hard"),
                        MarkKind::Soft => tr!("ag-origin-soft"),
                        MarkKind::Live => tr!("ag-kind-live"),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(focus, s.accent()),
                        Span::styled(format!("{} ", m.kind.glyph()), mark_style(m.kind, s)),
                        Span::styled(format!("{} {}", agenda::hm(m.at, &tz, with_off), m.target), s.title()),
                        Span::styled(format!("  {kind}"), s.muted()),
                    ]));
                    detail_lines(&m.rule_id, m.occ.and_then(|i| d.proj.occurrences.get(i)), &rules, &cov, s, &mut lines);
                }
                Item::Live(l) => {
                    let opens = if l.open_before { tr!("ag-live-before") } else { agenda::hm(l.opens, &tz, with_off) };
                    let closes = l.closes.map(|c| agenda::hm(c, &tz, with_off)).unwrap_or_else(|| tr!("ag-until-end"));
                    lines.push(Line::from(vec![
                        Span::styled(focus, s.accent()),
                        Span::styled(format!("♪ {}", l.dj), s.title()),
                        Span::styled(format!("  {}", tr!("ag-live-window", opens = opens, closes = closes)), s.muted()),
                    ]));
                    detail_lines(&l.rule_id, None, &rules, &cov, s, &mut lines);
                }
            }
        }
        Paragraph::new(lines).wrap(Wrap { trim: false }).render(inner, buf);
    }

    /// Hauteur de la zone « hors horloge » (0 = rien à y mettre).
    fn floating_height(&self) -> u16 {
        match &self.data {
            Some(d) if !d.proj.floating.is_empty() => d.proj.floating.len().min(FLOATING_MAX) as u16 + 2,
            _ => 0,
        }
    }

    /// Les `every` de la période, à part : ils ne sont pas fixes dans le
    /// temps (cadence au dernier passage ou au compteur de pistes), donc pas
    /// sur la ligne du temps.
    fn render_floating(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let Some(d) = &self.data else { return };
        let rules = self.rule_map();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(s.muted())
            .title(Span::styled(format!(" {} ", tr!("ag-floating-title")), s.label()));
        let inner = block.inner(area);
        block.render(area, buf);
        let name_w = d
            .proj
            .floating
            .iter()
            .map(|id| rules.get(id.as_str()).map_or(0, |r| playlist_of(r).chars().count()))
            .max()
            .unwrap_or(0)
            .min(24);
        let shown = if d.proj.floating.len() > FLOATING_MAX { FLOATING_MAX - 1 } else { FLOATING_MAX };
        let mut lines: Vec<Line> = d
            .proj
            .floating
            .iter()
            .take(shown)
            .map(|id| match rules.get(id.as_str()) {
                Some(r) => Line::from(vec![
                    Span::styled(format!(" {:<name_w$}  ", fit::ellipsize(playlist_of(r), name_w)), s.warn()),
                    Span::raw(rule_text(r)),
                    Span::styled(format!("  [{id}]"), s.muted()),
                ]),
                None => Line::styled(format!(" [{id}]"), s.muted()),
            })
            .collect();
        if d.proj.floating.len() > shown {
            lines.push(Line::styled(
                format!(" {}", tr!("ag-floating-more", n = (d.proj.floating.len() - shown) as i64)),
                s.muted(),
            ));
        }
        Paragraph::new(lines).render(inner, buf);
    }

    /// Légende, sous la vue.
    fn footer(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let legend = if self.utc { format!("{} · {}", tr!("ag-legend"), tr!("ag-utc-rules")) } else { tr!("ag-legend") };
        Paragraph::new(Line::styled(format!(" {legend}"), s.muted())).render(area, buf);
    }

    fn render_week_grid(&mut self, area: Rect, buf: &mut Buffer, ctx: &Global, s: &Styles) {
        let (Some(tz), Some(d), Some(date)) = (self.tz(ctx), self.data.as_ref(), self.date) else { return };
        let step = STEPS[self.step];
        let rows = agenda::week_rows(step);
        let monday = agenda::monday(date);
        let label_w = 6u16;
        let cw = ((area.width.saturating_sub(label_w)) / 7).max(4) as usize;
        let colors = self.colors();
        let t_now = now();
        let today_d = today(&tz);
        // En-têtes des jours.
        let mut head = vec![Span::raw(" ".repeat(label_w as usize))];
        for c in 0..7 {
            let day = agenda::shift(monday, c as i64);
            let st = if c == self.col {
                s.tab_active()
            } else if day == today_d {
                s.accent()
            } else {
                s.label()
            };
            head.push(Span::styled(format!("{:<cw$}", fit::ellipsize(&date_short(day), cw.saturating_sub(1))), st));
        }
        let h = area.height.saturating_sub(1) as usize;
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + h {
            self.scroll = self.cursor + 1 - h;
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(h));
        let mut lines = vec![Line::from(head)];
        for (ri, t) in rows.iter().enumerate().skip(self.scroll).take(h) {
            let mut spans = vec![Span::styled(
                format!("{:<6}", t.strftime("%H:%M").to_string()),
                if ri == self.cursor { s.tab_active() } else { s.label() },
            )];
            for c in 0..7 {
                let day = agenda::shift(monday, c as i64);
                let selected = ri == self.cursor && c == self.col;
                let text_w = cw.saturating_sub(1);
                let (text, st) = match agenda::cell(day, *t, step, &tz) {
                    Cell::Gap => (format!("{:<text_w$}", tr!("ag-gap")), s.muted()),
                    Cell::At(slot) => {
                        let band = d.proj.band_at(slot.start);
                        let bst = match band {
                            Some(b) if b.origin == Origin::Fallback || b.playlist_ref.is_empty() => s.band_fallback(),
                            Some(b) => s.band(colors.get(&b.playlist_ref).copied().unwrap_or(0)),
                            None => s.muted(),
                        };
                        let starts = d.proj.bands.iter().any(|b| agenda::contains(slot, b.start)) || ri == self.scroll;
                        let name = if starts { band.map(band_name).unwrap_or_default() } else { String::new() };
                        let marks = d.proj.marks_in(slot.start, slot.end);
                        let tag = match marks.len() {
                            0 => String::new(),
                            1 => marks[0].kind.glyph().to_string(),
                            n => n.to_string(),
                        };
                        let now_tag = if agenda::contains(slot, t_now) { "▶" } else { "" };
                        let right = format!("{now_tag}{tag}");
                        let room = text_w.saturating_sub(right.chars().count());
                        let body = format!("{:<room$}{right}", fit::ellipsize(&name, room));
                        (body, bst)
                    }
                };
                let st = if selected { s.tab_active() } else { st };
                spans.push(Span::styled(text, st));
                spans.push(Span::raw(" "));
            }
            lines.push(Line::from(spans));
        }
        Paragraph::new(lines).render(area, buf);
    }

    /// Semaine en liste (terminal étroit) : un résumé par jour.
    fn render_week_list(&mut self, area: Rect, buf: &mut Buffer, ctx: &Global, s: &Styles) {
        let (Some(tz), Some(d), Some(date)) = (self.tz(ctx), self.data.as_ref(), self.date) else { return };
        let monday = agenda::monday(date);
        let today_d = today(&tz);
        let mut lines = Vec::new();
        // Trois lignes par jour ; le jour choisi reste visible.
        let per_day = 3usize;
        let fits = (area.height as usize / per_day).max(1);
        let first = self.col.saturating_sub(fits - 1).min(7 - fits.min(7));
        for c in first..7usize {
            let day = agenda::shift(monday, c as i64);
            let Some(p) = agenda::day(day, &tz) else { continue };
            let head_st = if c == self.col { s.tab_active() } else if day == today_d { s.accent() } else { s.title() };
            lines.push(Line::styled(format!(" {}", date_long(day)), head_st));
            let bands: Vec<String> = d
                .proj
                .bands
                .iter()
                .filter(|b| b.end > p.from && b.start < p.to)
                .map(|b| {
                    if b.start <= p.from { band_name(b) } else { format!("{} {}", band_name(b), agenda::hm(b.start, &tz, false)) }
                })
                .collect();
            let marks = d.proj.marks_in(p.from, p.to);
            let count = |k: MarkKind| marks.iter().filter(|m| m.kind == k).count() as i64;
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::raw(fit::ellipsize(&bands.join(" → "), (area.width as usize).saturating_sub(4))),
            ]));
            lines.push(Line::styled(
                format!(
                    "   {}",
                    tr!(
                        "ag-day-summary",
                        hard = count(MarkKind::Hard),
                        soft = count(MarkKind::Soft),
                        live = count(MarkKind::Live)
                    )
                ),
                s.muted(),
            ));
        }
        Paragraph::new(lines).render(area, buf);
    }

    fn render_coverage(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let Some(d) = &self.data else { return };
        let c = match &d.coverage {
            Ok(c) => c,
            Err(e) => {
                Paragraph::new(Line::styled(format!(" {e}"), s.error())).render(area, buf);
                return;
            }
        };
        let rows = self.coverage_rows();
        let detail_h = (area.height / 2).min(12);
        let [head_a, list_a, detail_a] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(detail_h)]).areas(area);
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {} ", tr!("cov-grid")), s.label()),
            verdict_span(c.worst, s),
            Span::styled(format!("  ({})", tr!("cov-rules", n = rows.len() as i64)), s.muted()),
        ]))
        .render(head_a, buf);
        if rows.is_empty() {
            Paragraph::new(Line::styled(format!(" {}", tr!("cov-none")), s.muted())).render(list_a, buf);
            return;
        }
        let h = list_a.height as usize;
        let sel = self.cov_sel.min(rows.len() - 1);
        let start = sel.saturating_sub(h.saturating_sub(1));
        let mut lines = Vec::new();
        for (i, e) in rows.iter().enumerate().skip(start).take(h) {
            let dur_t = e.total_duration.as_ref().map(|d| dur(d.seconds.max(0) as u64 * 1000)).unwrap_or_else(|| "—".into());
            let count = e.selected_count.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
            let mut line = Line::from(vec![
                Span::raw(" "),
                verdict_cell(e.verdict, s),
                Span::raw("  "),
                Span::styled(format!("{:<18} ", fit::ellipsize(&e.rule_id, 18)), s.accent()),
                Span::styled(format!("{:<24} ", fit::ellipsize(&kind_text(&e.kind), 24)), s.muted()),
                Span::raw(format!("{:<22} ", fit::ellipsize(&e.playlist_ref, 22))),
                Span::styled(format!("{count:>6}  {dur_t}"), s.label()),
            ]);
            if i == sel {
                line = line.style(s.tab_active());
            }
            lines.push(line);
        }
        Paragraph::new(lines).render(list_a, buf);

        let e = rows[sel];
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(s.border())
            .title(Span::styled(format!(" {} ", e.rule_id), s.title()));
        let inner = block.inner(detail_a);
        block.render(detail_a, buf);
        let mut lines = Vec::new();
        let rules = self.rule_map();
        if let Some(r) = rules.get(e.rule_id.as_str()) {
            lines.push(Line::styled(rule_text(r), s.muted()));
        }
        if e.reasons.is_empty() {
            lines.push(Line::styled(tr!("cov-reason-ok"), s.ok()));
        }
        for r in &e.reasons {
            lines.push(Line::styled(format!("· {}", reason_text(r)), s.warn()));
        }
        for m in &e.members {
            let mut spans = vec![Span::raw(format!("  {} ", m.r#ref)), verdict_span(m.verdict, s)];
            // Un pool vide est déjà dit par sa cause.
            if m.selected_count != Some(0) {
                spans.push(Span::raw("  "));
                spans.push(pool_span(m.selected_count, m.total_duration.as_ref(), s));
            }
            let why: Vec<String> = m.reasons.iter().map(reason_text).collect();
            if !why.is_empty() {
                spans.push(Span::styled(format!("  {}", why.join(" ; ")), s.warn()));
            }
            lines.push(Line::from(spans));
        }
        Paragraph::new(lines).wrap(Wrap { trim: false }).render(inner, buf);
    }

    fn render_calendar(&self, area: Rect, buf: &mut Buffer, ctx: &Global, s: &Styles, cur: Date) {
        let box_a = crate::dialog::centered(area, 40, 12);
        Clear.render(box_a, buf);
        let block = crate::dialog::frame(&tr!("ag-calendar"), s, false)
            .title_bottom(Span::styled(format!(" {} ", tr!("ag-calendar-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let today_d = self.tz(ctx).map(|tz| today(&tz));
        let first = cur.first_of_month();
        let mut lines = vec![Line::styled(format!(" {}", tr!("ag-month", month = i64::from(cur.month()), year = i64::from(cur.year()))), s.title())];
        let mut head = vec![Span::raw(" ")];
        for w in 1..=7 {
            head.push(Span::styled(format!("{:<5}", fit::ellipsize(&tr!("ag-weekday-short", wd = w), 4)), s.label()));
        }
        lines.push(Line::from(head));
        let lead = first.weekday().to_monday_zero_offset() as i64;
        let mut day = agenda::shift(first, -lead);
        for _ in 0..6 {
            let mut row = vec![Span::raw(" ")];
            for _ in 0..7 {
                let st = if day == cur {
                    s.tab_active()
                } else if Some(day) == today_d {
                    s.accent()
                } else if day.month() != cur.month() {
                    s.muted()
                } else {
                    s.base()
                };
                row.push(Span::styled(format!("{:>2}", day.day()), st));
                row.push(Span::raw("   "));
                day = agenda::shift(day, 1);
            }
            lines.push(Line::from(row));
        }
        Paragraph::new(lines).render(inner, buf);
    }
}

impl Agenda {
    fn render_new_kind(&self, area: Rect, buf: &mut Buffer, s: &Styles, sel: usize) {
        let box_a = crate::dialog::centered(area, 50, KINDS.len() as u16 + 5);
        Clear.render(box_a, buf);
        let block = crate::dialog::frame(&tr!("ag-new-title"), s, false)
            .title_bottom(Span::styled(format!(" {} ", tr!("picker-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let mut lines = vec![Line::styled(tr!("ag-new-kind"), s.label()), Line::default()];
        for (i, k) in KINDS.iter().enumerate() {
            let st = if i == sel { s.tab_active() } else { s.base() };
            lines.push(Line::styled(format!(" {}", kind_label(k)), st));
        }
        Paragraph::new(lines).render(inner, buf);
    }

    fn render_rule_pick(&self, area: Rect, buf: &mut Buffer, s: &Styles, sel: usize) {
        let rules: Vec<&Rule> = match self.data.as_ref().map(|d| &d.rules) {
            Some(Ok(r)) => r.iter().collect(),
            _ => Vec::new(),
        };
        let w = area.width.saturating_sub(4).min(110);
        let h = (rules.len() as u16 + 5).clamp(8, area.height);
        let box_a = crate::dialog::centered(area, w, h);
        Clear.render(box_a, buf);
        let block = crate::dialog::frame(&tr!("ag-rules-title", grid = self.grid_name()), s, false)
            .title_bottom(Span::styled(format!(" {} ", tr!("ag-rules-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        if rules.is_empty() {
            Paragraph::new(Line::styled(tr!("ag-rules-none"), s.muted())).render(inner, buf);
            return;
        }
        let rows = inner.height as usize;
        let start = sel.saturating_sub(rows.saturating_sub(1));
        let cw = inner.width as usize;
        let mut lines = Vec::new();
        for (i, r) in rules.iter().enumerate().skip(start).take(rows) {
            let st = if i == sel { s.tab_active() } else if r.enabled { s.base() } else { s.muted() };
            let kind = match &r.kind {
                Some(rule::Kind::BaseRotation(_)) => kind_label("base_rotation"),
                Some(rule::Kind::DayPart(_)) => kind_label("day_part"),
                Some(rule::Kind::AtClock(_)) => kind_label("at_clock"),
                Some(rule::Kind::Every(_)) => kind_label("every"),
                Some(rule::Kind::Live(_)) => kind_label("live"),
                None => String::new(),
            };
            let target = match &r.kind {
                Some(rule::Kind::BaseRotation(x)) => x.playlist_ref.clone(),
                Some(rule::Kind::DayPart(x)) => x.playlist_ref.clone(),
                Some(rule::Kind::AtClock(x)) => x.playlist_ref.clone(),
                Some(rule::Kind::Every(x)) => x.playlist_ref.clone(),
                Some(rule::Kind::Live(x)) => x.dj.clone(),
                None => String::new(),
            };
            let head = format!(" {:<20} {:<24} {:<18} ", fit::ellipsize(&r.id, 20), fit::ellipsize(&kind, 24), fit::ellipsize(&target, 18));
            let rest = fit::ellipsize(&rule_text(r), cw.saturating_sub(head.chars().count()));
            lines.push(Line::from(vec![Span::styled(head, st), Span::styled(rest, if i == sel { st } else { s.muted() })]));
        }
        Paragraph::new(lines).render(inner, buf);
    }

    fn render_grid_pick(&self, area: Rect, buf: &mut Buffer, s: &Styles, sel: usize) {
        let items: Vec<&stationd_proto::schedule::GridInfo> = match &self.grids {
            Some(Ok(g)) => g.grids.iter().collect(),
            _ => Vec::new(),
        };
        let h = (items.len() as u16 * 2 + 6).min(area.height);
        let box_a = crate::dialog::centered(area, 70, h);
        Clear.render(box_a, buf);
        let block = crate::dialog::frame(&tr!("ag-grids-title"), s, false)
            .title_bottom(Span::styled(format!(" {} ", tr!("ag-grids-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let mut lines = Vec::new();
        match &self.grids {
            Some(Err(e)) => lines.push(Line::styled(e.clone(), s.error())),
            None => lines.push(Line::styled(tr!("media-loading"), s.muted())),
            Some(Ok(_)) if items.is_empty() => lines.push(Line::styled(tr!("ag-grids-none"), s.muted())),
            _ => {}
        }
        let viewed = self.grid_name();
        for (i, g) in items.iter().enumerate() {
            let st = if i == sel { s.tab_active() } else { s.base() };
            let rules = match (g.revision.is_empty(), g.rules) {
                (true, _) => tr!("ag-grids-no-file"),
                (false, Some(n)) => tr!("ag-grids-rules", n = i64::from(n)),
                (false, None) => tr!("ag-grids-unreadable"),
            };
            let mut spans = vec![
                Span::styled(if g.active { " ● " } else { "   " }, s.ok()),
                Span::styled(format!("{:<28}", fit::ellipsize(&g.name, 28)), st),
                Span::styled(format!(" {rules}"), s.muted()),
            ];
            if g.active {
                spans.push(Span::styled(format!("  {}", tr!("ag-grids-active")), s.ok()));
            }
            if g.name == viewed {
                spans.push(Span::styled(format!("  {}", tr!("ag-grids-viewed")), s.accent()));
            }
            lines.push(Line::from(spans));
            if let Some(p) = &g.problem {
                let at = if p.rule_id.is_empty() { p.field_path.clone() } else { p.rule_id.clone() };
                let t = format!("{at} : {}", super::ruleform::diag_text(p));
                lines.push(Line::styled(format!("     {}", fit::ellipsize(&t, (inner.width as usize).saturating_sub(6))), s.warn()));
            }
        }
        Paragraph::new(lines).render(inner, buf);
    }
}

/// Au plus autant de lignes dans la zone « hors horloge ».
const FLOATING_MAX: usize = 5;

/// La playlist lancée par une règle (vide pour un créneau live).
fn playlist_of(r: &Rule) -> &str {
    match &r.kind {
        Some(rule::Kind::Every(e)) => &e.playlist_ref,
        Some(rule::Kind::AtClock(a)) => &a.playlist_ref,
        Some(rule::Kind::DayPart(d)) => &d.playlist_ref,
        Some(rule::Kind::BaseRotation(b)) => &b.playlist_ref,
        _ => "",
    }
}

fn band_name(b: &Band) -> String {
    if b.playlist_ref.is_empty() { tr!("ag-origin-fallback") } else { b.playlist_ref.clone() }
}

fn mark_style(k: MarkKind, s: &Styles) -> Style {
    match k {
        MarkKind::Hard => s.error(),
        MarkKind::Soft => s.accent(),
        MarkKind::Live => s.calm(),
    }
}

// --- écran -----------------------------------------------------------------------

const GRID_KEYS: &[KeyHelp] = &[
    (k!("key-up-down"), k!("help-ag-grid-move")),
    (k!("key-enter"), k!("help-ag-grid-view")),
    (k!("key-a"), k!("help-ag-grid-activate")),
    (k!("key-c"), k!("help-ag-grid-copy")),
    (k!("key-esc"), k!("help-ag-cancel")),
];

const PICK_KEYS: &[KeyHelp] = &[
    (k!("key-up-down"), k!("help-ag-kind-move")),
    (k!("key-enter"), k!("help-ag-kind-pick")),
    (k!("key-esc"), k!("help-ag-cancel")),
];

const RULE_KEYS: &[KeyHelp] = &[
    (k!("key-up-down"), k!("help-ag-rule")),
    (k!("key-enter"), k!("help-ag-edit")),
    (k!("key-d"), k!("help-ag-delete")),
    (k!("key-n"), k!("help-ag-new")),
    (k!("key-esc"), k!("help-ag-cancel")),
];

const DAY_KEYS: &[KeyHelp] = &[
    (k!("key-n"), k!("help-ag-new")),
    (k!("key-l"), k!("help-ag-rules")),
    (k!("key-tab"), k!("help-ag-item")),
    (k!("key-e"), k!("help-ag-edit")),
    (k!("key-d"), k!("help-ag-delete")),
    (k!("key-shift-g"), k!("help-ag-grids")),
    (k!("key-u"), k!("help-ag-utc")),
    (k!("key-up-down"), k!("help-ag-slot")),
    (k!("key-brackets"), k!("help-ag-day")),
    (k!("key-t"), k!("help-ag-today")),
    (k!("key-g"), k!("help-ag-goto")),
    (k!("key-v"), k!("help-ag-week")),
    (k!("key-c"), k!("help-ag-coverage")),
    (k!("key-plus-minus"), k!("help-ag-step")),
    (k!("key-enter"), k!("help-ag-inspector")),
    (k!("key-p"), k!("help-ag-playlist")),
    (k!("key-r"), k!("help-media-reload")),
];

const WEEK_KEYS: &[KeyHelp] = &[
    (k!("key-arrows"), k!("help-ag-cell")),
    (k!("key-l"), k!("help-ag-rules")),
    (k!("key-brackets"), k!("help-ag-week-shift")),
    (k!("key-enter"), k!("help-ag-open-day")),
    (k!("key-t"), k!("help-ag-today")),
    (k!("key-g"), k!("help-ag-goto")),
    (k!("key-v"), k!("help-ag-day-view")),
    (k!("key-shift-g"), k!("help-ag-grids")),
    (k!("key-u"), k!("help-ag-utc")),
    (k!("key-c"), k!("help-ag-coverage")),
    (k!("key-plus-minus"), k!("help-ag-step")),
    (k!("key-r"), k!("help-media-reload")),
];

const COVERAGE_KEYS: &[KeyHelp] = &[
    (k!("key-up-down"), k!("help-ag-rule")),
    (k!("key-e"), k!("help-ag-edit")),
    (k!("key-p"), k!("help-ag-playlist")),
    (k!("key-c"), k!("help-ag-back")),
    (k!("key-r"), k!("help-media-reload")),
];

const CALENDAR_KEYS: &[KeyHelp] = &[
    (k!("key-arrows"), k!("help-ag-cal-move")),
    (k!("key-page"), k!("help-ag-cal-month")),
    (k!("key-enter"), k!("help-ag-cal-pick")),
    (k!("key-esc"), k!("help-ag-cancel")),
];

impl Screen for Agenda {
    fn title(&self) -> String {
        tr!("screen-agenda")
    }

    fn enter(&mut self, ctx: &mut Global) -> Result<(), Error> {
        self.load(ctx);
        Ok(())
    }

    fn reconnected(&mut self, ctx: &mut Global) -> Result<(), Error> {
        if self.data.is_none() || self.data.as_ref().is_some_and(|d| d.stale.is_some()) {
            self.load(ctx);
        }
        Ok(())
    }

    fn captures_text(&self) -> bool {
        self.form.is_some() || self.new_kind.is_some() || self.grid_pick.is_some() || self.rule_pick.is_some()
    }

    fn help(&self) -> &'static [KeyHelp] {
        if let Some(f) = &self.form {
            return f.keys();
        }
        if self.grid_pick.is_some() {
            return GRID_KEYS;
        }
        if self.rule_pick.is_some() {
            return RULE_KEYS;
        }
        if self.new_kind.is_some() {
            return PICK_KEYS;
        }
        if self.calendar.is_some() {
            return CALENDAR_KEYS;
        }
        match self.view {
            View::Day => DAY_KEYS,
            View::Week => WEEK_KEYS,
            View::Coverage => COVERAGE_KEYS,
        }
    }

    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        let e = match event {
            AppEvent::Agenda(ev) => {
                if let Some(f) = self.form.as_mut()
                    && let Some(out) = f.on_event(ev, ctx)
                {
                    self.form_outcome(out, ctx);
                    return Ok(Control::Changed);
                }
                match &**ev {
                    AgEvent::Read(id, read) if *id == self.request => self.on_read(read, ctx),
                    AgEvent::Text(id, purpose, r) if *id == self.text_req => self.on_text(purpose, r, ctx),
                    _ => return Ok(Control::Continue),
                }
                return Ok(Control::Changed);
            }
            // Une action finie (grille activée, règle supprimée…) : relire.
            AppEvent::ActionDone(Ok(_)) => {
                self.load(ctx);
                return Ok(Control::Changed);
            }
            AppEvent::Event(e) => e,
            _ => return Ok(Control::Continue),
        };
        if let Some(f) = self.form.as_mut() {
            let out = f.on_key(e, ctx);
            self.form_outcome(out, ctx);
            return Ok(Control::Changed);
        }
        let Event::Key(k) = e else { return Ok(Control::Continue) };
        if k.kind != KeyEventKind::Press {
            return Ok(Control::Continue);
        }

        // Choix de la nature d'une nouvelle règle.
        if let Some(sel) = self.new_kind {
            match k.code {
                KeyCode::Esc => self.new_kind = None,
                KeyCode::Up => self.new_kind = Some(sel.saturating_sub(1)),
                KeyCode::Down => self.new_kind = Some((sel + 1).min(KINDS.len() - 1)),
                KeyCode::Enter => {
                    self.new_kind = None;
                    self.start_new(KINDS[sel], ctx);
                }
                _ => return Ok(Control::Unchanged),
            }
            return Ok(Control::Changed);
        }

        // Liste des règles : toutes, même celles qui ne jouent pas ce jour-là
        // (au compteur, désactivées, autres jours).
        if let Some(sel) = self.rule_pick {
            let ids = self.rule_ids();
            match k.code {
                KeyCode::Esc => self.rule_pick = None,
                KeyCode::Up => self.rule_pick = Some(sel.saturating_sub(1)),
                KeyCode::Down => self.rule_pick = Some((sel + 1).min(ids.len().saturating_sub(1))),
                KeyCode::Enter | KeyCode::Char('e') => {
                    if let Some(id) = ids.get(sel) {
                        self.rule_pick = None;
                        self.fetch_text(Purpose::Edit { rule_id: id.clone() }, ctx);
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(id) = ids.get(sel) {
                        self.rule_pick = None;
                        self.fetch_text(Purpose::Delete { rule_id: id.clone() }, ctx);
                    }
                }
                KeyCode::Char('n') => {
                    self.rule_pick = None;
                    self.new_kind = Some(0);
                }
                _ => return Ok(Control::Unchanged),
            }
            return Ok(Control::Changed);
        }

        // Sélecteur de grille.
        if let Some(sel) = self.grid_pick {
            let list = self.grid_list();
            match k.code {
                KeyCode::Esc => self.grid_pick = None,
                KeyCode::Up => self.grid_pick = Some(sel.saturating_sub(1)),
                KeyCode::Down => self.grid_pick = Some((sel + 1).min(list.len().saturating_sub(1))),
                KeyCode::Enter => {
                    if let Some((name, active)) = list.get(sel) {
                        self.grid = (!active).then(|| name.clone());
                        self.grid_pick = None;
                        self.item = 0;
                        self.load(ctx);
                    }
                }
                KeyCode::Char('a') => {
                    if let Some((name, active)) = list.get(sel)
                        && !active
                    {
                        self.grid_pick = None;
                        ctx.open(Modal::Confirm(
                            Confirm::new(
                                tr!("ag-activate-title"),
                                vec![tr!("ag-activate-body", grid = name.clone())],
                                tr!("ag-activate-yes"),
                                Action::ActivateGrid { name: name.clone() },
                            )
                            .danger(),
                        ));
                    }
                }
                KeyCode::Char('c') => {
                    if let Some((name, _)) = list.get(sel) {
                        let from = name.clone();
                        self.grid_pick = None;
                        ctx.open(Modal::Form(Form::new(
                            tr!("ag-copy-title", grid = from.clone()),
                            vec![Field::text(tr!("ag-copy-name"), "")],
                            move |f| {
                                let to = f[0].value();
                                if to.is_empty() {
                                    return Err(tr!("ag-copy-need-name"));
                                }
                                Ok(Action::CopyGrid { from: from.clone(), to })
                            },
                        )));
                    }
                }
                _ => return Ok(Control::Unchanged),
            }
            return Ok(Control::Changed);
        }

        // Calendrier ouvert : il capture les touches.
        if let Some(cur) = self.calendar {
            let next = match k.code {
                KeyCode::Esc => {
                    self.calendar = None;
                    return Ok(Control::Changed);
                }
                KeyCode::Enter => {
                    self.calendar = None;
                    self.go_date(cur, ctx);
                    return Ok(Control::Changed);
                }
                KeyCode::Left => agenda::shift(cur, -1),
                KeyCode::Right => agenda::shift(cur, 1),
                KeyCode::Up => agenda::shift(cur, -7),
                KeyCode::Down => agenda::shift(cur, 7),
                KeyCode::PageUp => cur.checked_sub(jiff::Span::new().months(1)).unwrap_or(cur),
                KeyCode::PageDown => cur.checked_add(jiff::Span::new().months(1)).unwrap_or(cur),
                KeyCode::Char('t') => self.tz(ctx).map(|tz| today(&tz)).unwrap_or(cur),
                _ => return Ok(Control::Unchanged),
            };
            self.calendar = Some(next);
            return Ok(Control::Changed);
        }

        // Inspecteur plein écran (terminal étroit).
        if self.inspector && matches!(k.code, KeyCode::Esc | KeyCode::Enter) {
            self.inspector = false;
            return Ok(Control::Changed);
        }

        let rows = self.rows(ctx);
        match (self.view, k.code) {
            (_, KeyCode::Char('r')) => self.load(ctx),
            (_, KeyCode::Char('u')) => {
                // Le curseur garde son instant.
                let t = self.cursor_time(ctx);
                self.utc = !self.utc;
                if let (Some(tz), Some(t)) = (self.tz(ctx), t) {
                    self.date = agenda::date_of(t, &tz).or(self.date);
                }
                self.load(ctx);
                if let Some(t) = t {
                    self.follow_to = Some(t);
                }
            }
            (_, KeyCode::Char('G')) => {
                self.grid_pick = Some(self.grid_list().iter().position(|(n, a)| {
                    self.grid.as_deref() == Some(n.as_str()) || (self.grid.is_none() && *a)
                }).unwrap_or(0));
            }
            (View::Coverage, KeyCode::Char('c') | KeyCode::Esc) => self.view = self.back,
            (View::Coverage, KeyCode::Char('e')) => {
                if let Some(id) = self.coverage_rows().get(self.cov_sel).map(|e| e.rule_id.clone()) {
                    self.fetch_text(Purpose::Edit { rule_id: id }, ctx);
                }
            }
            (View::Coverage, KeyCode::Up) => self.cov_sel = self.cov_sel.saturating_sub(1),
            (View::Coverage, KeyCode::Down) => self.cov_sel = (self.cov_sel + 1).min(rows.saturating_sub(1)),
            (View::Coverage, KeyCode::Home) => self.cov_sel = 0,
            (View::Coverage, KeyCode::End) => self.cov_sel = rows.saturating_sub(1),
            (View::Coverage, KeyCode::Char('p')) => {
                if let Some(r) = self.coverage_rows().get(self.cov_sel).map(|e| e.playlist_ref.clone())
                    && !r.is_empty()
                {
                    self.goto_playlist(&r, ctx);
                }
            }
            (View::Coverage, _) => return Ok(Control::Continue),
            (_, KeyCode::Char('c')) => {
                self.back = self.view;
                self.view = View::Coverage;
                if self.data.is_none() {
                    self.load(ctx);
                }
            }
            (_, KeyCode::Up) => {
                self.cursor = self.cursor.saturating_sub(1);
                self.item = 0;
            }
            (_, KeyCode::Down) => {
                self.cursor = (self.cursor + 1).min(rows.saturating_sub(1));
                self.item = 0;
            }
            (_, KeyCode::PageUp) => self.cursor = self.cursor.saturating_sub(10),
            (_, KeyCode::PageDown) => self.cursor = (self.cursor + 10).min(rows.saturating_sub(1)),
            (_, KeyCode::Home) => self.cursor = 0,
            (_, KeyCode::End) => self.cursor = rows.saturating_sub(1),
            (_, KeyCode::Char('[')) => self.shift(-1, ctx),
            (_, KeyCode::Char(']')) => self.shift(1, ctx),
            (_, KeyCode::Char('t')) => {
                if let Some(tz) = self.tz(ctx) {
                    self.follow_now = true;
                    let d = today(&tz);
                    if self.date == Some(d) && self.data.is_some() {
                        self.follow_now = false;
                        self.cursor_to(now(), ctx);
                    } else {
                        self.go_date(d, ctx);
                    }
                }
            }
            (_, KeyCode::Char('g')) => self.calendar = self.date,
            (_, KeyCode::Char('+')) => {
                let t = self.cursor_time(ctx);
                self.step = self.step.saturating_sub(1);
                if let Some(t) = t {
                    self.cursor_to(t, ctx);
                }
            }
            (_, KeyCode::Char('-')) => {
                let t = self.cursor_time(ctx);
                self.step = (self.step + 1).min(STEPS.len() - 1);
                if let Some(t) = t {
                    self.cursor_to(t, ctx);
                }
            }
            (View::Day, KeyCode::Char('v')) => {
                let t = self.cursor_time(ctx);
                self.view = View::Week;
                if let Some(d) = self.date {
                    self.col = d.weekday().to_monday_zero_offset() as usize;
                }
                self.load(ctx);
                if let Some(t) = t {
                    self.cursor_to(t, ctx);
                }
            }
            (View::Day | View::Week, KeyCode::Char('l')) => self.rule_pick = Some(0),
            (View::Day | View::Week, KeyCode::Char('n')) => self.new_kind = Some(0),
            (View::Day, KeyCode::Char('e')) => {
                if let Some(id) = self.focused_rule(ctx) {
                    self.fetch_text(Purpose::Edit { rule_id: id }, ctx);
                }
            }
            (View::Day, KeyCode::Char('d')) => {
                if let Some(id) = self.focused_rule(ctx) {
                    self.fetch_text(Purpose::Delete { rule_id: id }, ctx);
                }
            }
            (View::Day, KeyCode::Tab) => self.item += 1,
            (View::Day, KeyCode::BackTab) => self.item = self.item.saturating_sub(1),
            (View::Day, KeyCode::Enter) => self.inspector = self.width < WIDE,
            (View::Day, KeyCode::Char('p')) => {
                let items = self.items(ctx);
                let focused = items.get(self.item % items.len().max(1)).and_then(|i| i.playlist().map(str::to_string));
                let any = items.iter().find_map(|i| i.playlist().map(str::to_string));
                if let Some(r) = focused.or(any) {
                    self.goto_playlist(&r, ctx);
                }
            }
            (View::Week, KeyCode::Left) => self.col = self.col.saturating_sub(1),
            (View::Week, KeyCode::Right) => self.col = (self.col + 1).min(6),
            (View::Week, KeyCode::Char('v')) | (View::Week, KeyCode::Enter) => {
                let (Some(tz), Some(d)) = (self.tz(ctx), self.date) else { return Ok(Control::Changed) };
                let day = agenda::shift(agenda::monday(d), self.col as i64);
                // Le créneau de la case choisie, dans le jour ouvert.
                let at = agenda::week_rows(STEPS[self.step]).get(self.cursor).and_then(|t| {
                    match agenda::cell(day, *t, STEPS[self.step], &tz) {
                        Cell::At(s) => Some(s.start),
                        Cell::Gap => None,
                    }
                });
                self.view = View::Day;
                self.date = Some(day);
                self.item = 0;
                self.load(ctx);
                if let Some(t) = at {
                    self.cursor_to(t, ctx);
                }
            }
            _ => return Ok(Control::Continue),
        }
        Ok(Control::Changed)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        self.width = area.width;
        if let Some(f) = self.form.as_mut() {
            f.render(area, buf, ctx);
            return Ok(());
        }
        // Le fuseau arrive avec le bandeau : première lecture dès qu'il est là.
        if self.date.is_none() && self.loading.is_none() && ctx.store.tz.is_some() {
            self.load(ctx);
        }
        let s = Styles(&ctx.theme);
        Block::new().style(s.base()).render(area, buf);
        let floating_h = if matches!(self.view, View::Coverage) { 0 } else { self.floating_height() };
        let [head_a, body_a, float_a, foot_a] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(floating_h),
            Constraint::Length(1),
        ])
        .areas(area);
        self.header(head_a, buf, ctx, &s);

        if self.data.is_none() {
            let msg = match (&self.error, ctx.store.tz.is_some()) {
                (Some(e), _) => Span::styled(e.clone(), s.error()),
                (None, false) => Span::styled(tr!("ag-no-tz"), s.muted()),
                (None, true) => Span::styled(tr!("media-loading"), s.muted()),
            };
            Paragraph::new(Line::from(vec![Span::raw(" "), msg])).wrap(Wrap { trim: false }).render(body_a, buf);
            return Ok(());
        }

        match self.view {
            View::Day => {
                if self.inspector {
                    self.render_inspector(body_a, buf, ctx, &s);
                } else if area.width >= WIDE {
                    let [l, r] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(body_a);
                    self.render_day(l, buf, ctx, &s);
                    self.render_inspector(r, buf, ctx, &s);
                } else {
                    self.render_day(body_a, buf, ctx, &s);
                }
                self.render_floating(float_a, buf, &s);
                self.footer(foot_a, buf, &s);
            }
            View::Week => {
                if area.width >= WEEK_GRID {
                    self.render_week_grid(body_a, buf, ctx, &s);
                } else {
                    self.render_week_list(body_a, buf, ctx, &s);
                }
                self.render_floating(float_a, buf, &s);
                self.footer(foot_a, buf, &s);
            }
            View::Coverage => self.render_coverage(Rect { height: body_a.height + foot_a.height, ..body_a }, buf, &s),
        }
        if let Some(cur) = self.calendar {
            self.render_calendar(area, buf, ctx, &s, cur);
        }
        if let Some(sel) = self.new_kind {
            self.render_new_kind(area, buf, &s, sel);
        }
        if let Some(sel) = self.grid_pick {
            self.render_grid_pick(area, buf, &s, sel);
        }
        if let Some(sel) = self.rule_pick {
            self.render_rule_pick(area, buf, &s, sel);
        }
        Ok(())
    }
}

impl Agenda {
    /// L'instant du créneau (ou de la case) choisi.
    fn cursor_time(&self, ctx: &Global) -> Option<i64> {
        let tz = self.tz(ctx)?;
        let step = STEPS[self.step];
        match self.back_or_view() {
            View::Day => agenda::slots(self.period(&tz)?, step).get(self.cursor).map(|s| s.start),
            _ => {
                let day = agenda::shift(agenda::monday(self.date?), self.col as i64);
                match agenda::cell(day, *agenda::week_rows(step).get(self.cursor)?, step, &tz) {
                    Cell::At(s) => Some(s.start),
                    Cell::Gap => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stationd_proto::prost_types::{Duration as PDuration, Timestamp};
    use stationd_proto::schedule::{CoverageMember, LiveWindow, PreviewResponse};

    fn ts(s: i64) -> Option<Timestamp> {
        Some(Timestamp { seconds: s, nanos: 0 })
    }

    fn occ(t: i64, origin: Origin, rule: &str, pl: &str) -> Occurrence {
        Occurrence {
            at_utc: ts(t),
            rule_id: rule.into(),
            playlist_ref: pl.into(),
            origin: origin as i32,
            selected_count: Some(12),
            total_duration: Some(PDuration { seconds: 3000, nanos: 0 }),
            ..Default::default()
        }
    }

    #[test]
    fn dates_and_weekdays_are_translated() {
        let d = Date::new(2026, 9, 28).unwrap();
        assert_eq!(date_short(d), "lun. 28/09");
        assert_eq!(date_long(agenda::shift(d, 6)), "dimanche 04/10/2026");
    }

    #[test]
    fn every_coverage_reason_has_a_translation() {
        for code in 0..=16 {
            let r = CoverageReason { code, window: "2h".into(), pool_ms: Some(60_000), ..Default::default() };
            let t = reason_text(&r);
            assert!(!t.contains('⟦') && !t.is_empty(), "code {code}: {t}");
        }
        let r = CoverageReason { code: 99, ..Default::default() };
        assert!(reason_text(&r).contains("99"), "un opcode inconnu est dit, pas avalé");
    }

    /// Une journée d'automne (25 h) à Paris : base, tranche de nuit, un
    /// rendez-vous hard, un live, puis rendu jour / semaine / couverture.
    fn fixture(ctx: &mut Global) -> Agenda {
        let tz = TimeZone::get("Europe/Paris").unwrap();
        ctx.store.tz = Some(tz.clone());
        ctx.store.tz_name = Some("Europe/Paris".into());
        let date = Date::new(2026, 10, 25).unwrap();
        let p = agenda::day(date, &tz).unwrap();
        let at = |h: i64| p.from + h * 3600;
        let preview = PreviewResponse {
            occurrences: vec![
                occ(p.from - 3600, Origin::BaseRotation, "floor", "musique"),
                occ(at(9), Origin::AtClockHard, "news", "flash"),
                occ(at(9) + 60, Origin::BaseRotation, "floor", "musique"),
                occ(at(23), Origin::DayPart, "night", "nuit"),
            ],
            live: vec![LiveWindow {
                rule_id: "dj-alex".into(),
                dj: "alex".into(),
                opens_at: ts(at(20)),
                closes_at: ts(at(23)),
                ..Default::default()
            }],
            ..Default::default()
        };
        let coverage = CheckCoverageResponse {
            entries: vec![CoverageEntry {
                rule_id: "night".into(),
                playlist_ref: "nuit".into(),
                kind: "day_part".into(),
                verdict: Verdict::Thin as i32,
                reasons: vec![CoverageReason {
                    code: Code::MembersLoop as i32,
                    refs: vec!["jazz".into()],
                    ..Default::default()
                }],
                members: vec![CoverageMember { r#ref: "jazz".into(), verdict: Verdict::Thin as i32, ..Default::default() }],
                ..Default::default()
            }],
            worst: Verdict::Thin as i32,
        };
        let mut a = Agenda { date: Some(date), follow_now: false, ..Default::default() };
        a.loading = Some(p);
        a.request = 1;
        let grids = ListGridsResponse {
            grids: vec![stationd_proto::schedule::GridInfo {
                name: "grid.toml".into(),
                revision: "sha256:x".into(),
                active: true,
                rules: Some(3),
                problem: None,
            }],
            active: "grid.toml".into(),
        };
        let read = AgendaRead { preview: Ok(preview), rules: Ok(Vec::new()), coverage: Ok(coverage), grids: Ok(grids) };
        a.on_read(&read, ctx);
        a
    }

    fn screen_text(a: &mut Agenda, ctx: &mut Global, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        a.render(area, &mut buf, ctx).unwrap();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn global() -> Global {
        use clap::Parser;
        let args = crate::Args::parse_from(["stationd-tui"]);
        let ch = crate::rpc::lazy_channel(&args.addr).unwrap();
        Global::new(&args, ch.clone(), ch)
    }

    #[tokio::test]
    async fn the_day_shows_bases_marks_live_and_the_repeated_hour() {
        let mut ctx = global();
        let mut a = fixture(&mut ctx);
        a.step = 2; // 60 min : 25 lignes
        assert_eq!(a.rows(&ctx), 25, "journée de 25 h");
        let text = screen_text(&mut a, &mut ctx, 130, 40);
        assert!(text.contains("dimanche 25/10/2026"), "{text}");
        assert!(text.contains("journée de 25 h"), "{text}");
        assert!(text.contains("02:00+02") && text.contains("02:00+01"), "{text}");
        assert!(text.contains("musique") && text.contains("nuit"), "{text}");
        assert!(text.contains("!08:00 flash"), "rendez-vous hard à 08:00 (heure d'hiver) : {text}");
        assert!(text.contains("♪"), "{text}");
        // Inspecteur sur le créneau de la nuit : règle et verdict traduits.
        a.cursor_to(agenda::day(a.date.unwrap(), ctx.store.tz.as_ref().unwrap()).unwrap().from + 23 * 3600, &ctx);
        let text = screen_text(&mut a, &mut ctx, 130, 40);
        assert!(text.contains("tranche (day_part)"), "{text}");
        assert!(text.contains("juste"), "{text}");
        assert!(text.contains("sous-dimensionné(s) : jazz"), "{text}");
        // Terminal minimal : la timeline tient, sans inspecteur à côté.
        let text = screen_text(&mut a, &mut ctx, 80, 22);
        assert!(!text.contains("tranche (day_part)"), "{text}");
    }

    #[tokio::test]
    async fn week_and_coverage_views_render_in_both_sizes() {
        let mut ctx = global();
        let mut a = fixture(&mut ctx);
        a.view = View::Week;
        let wide = screen_text(&mut a, &mut ctx, 130, 40);
        assert!(wide.contains("dim. 25/10") && wide.contains("lun. 19/10"), "{wide}");
        let narrow = screen_text(&mut a, &mut ctx, 80, 22);
        assert!(narrow.contains("dimanche 25/10/2026"), "liste des jours : {narrow}");
        a.back = View::Week;
        a.view = View::Coverage;
        let cov = screen_text(&mut a, &mut ctx, 100, 30);
        assert!(cov.contains("⚠ juste") && cov.contains("night"), "{cov}");
        a.calendar = a.date;
        let cal = screen_text(&mut a, &mut ctx, 100, 30);
        assert!(cal.contains("octobre 2026"), "{cal}");
    }

    #[tokio::test]
    async fn a_failed_reload_keeps_the_same_period_marked_old() {
        let mut ctx = global();
        let mut a = fixture(&mut ctx);
        let p = a.data.as_ref().unwrap().period;
        a.loading = Some(p);
        let failed = AgendaRead {
            preview: Err("panne".into()),
            rules: Err("panne".into()),
            coverage: Err("panne".into()),
            grids: Err("panne".into()),
        };
        a.on_read(&failed, &mut ctx);
        let d = a.data.as_ref().expect("données gardées");
        assert_eq!(d.stale.as_deref(), Some("panne"));
        // Une autre période en échec : rien n'est présenté sous de nouvelles dates.
        a.loading = Some(Period { from: p.from + 86_400, to: p.to + 86_400 });
        a.on_read(&failed, &mut ctx);
        assert!(a.data.is_none());
        assert_eq!(a.error.as_deref(), Some("panne"));
    }
}
