//! Antenne (`1`) — vue principale (dossier §5.1).
//!
//! Tout vient du flux `OnAirService.Watch` : morceau à l'antenne et sa
//! progression, playlist en cours et playlists à suivre, morceau préchargé
//! (certain) puis morceaux théoriques (simulés, `~`), derniers joués avec leur
//! issue, et les notes qui disent pourquoi la suite peut changer — reçues en
//! opcodes, traduites ici.
//!
//! Sans ce flux (stationd trop ancien, coupure), la vue retombe sur ce que dit
//! `Liquidsoap.GetStatus` — et le dit.

use std::time::Duration;

use anyhow::Error;
use jiff::tz::TimeZone;
use rat_salsa::Control;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::gauge::LineGauge;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use ratatui_widgets::table::{Cell, Row, Table};
use stationd_proto::onair::playlist_slot::Issue;
use stationd_proto::onair::{Note, OnAirSnapshot, PlaylistSlot, Track, note, track::Outcome};

use super::ops;
use crate::app::{AppEvent, Global};
use crate::fit;
use crate::screen::{KeyHelp, Screen};
use crate::store::{human_duration, local_hms};
use crate::style::Styles;
use crate::{k, tr};

const UPCOMING_DEFAULT: usize = 10;
const UPCOMING_MAX: usize = 30;
/// Notes affichées sous « À suivre ».
const NOTES_MAX: usize = 4;

pub struct Antenne {
    /// Morceaux à suivre affichés (préchargé compris).
    upcoming: usize,
}

impl Default for Antenne {
    fn default() -> Self {
        Self { upcoming: UPCOMING_DEFAULT }
    }
}

// --- mise en forme -------------------------------------------------------------

/// `Artiste — Titre` ; le nom de fichier quand les tags manquent (dit).
pub(super) fn label(t: &Track) -> String {
    let file = t.rel_path.rsplit('/').next().unwrap_or(&t.rel_path);
    match (t.artist.is_empty(), t.title.is_empty()) {
        (false, false) => format!("{} — {}", t.artist, t.title),
        (true, false) => t.title.clone(),
        _ if t.stream => tr!("track-relay", url = t.rel_path.clone()),
        _ if file.is_empty() => "—".into(),
        _ => tr!("track-untitled", file = file.to_string()),
    }
}

fn mmss(ms: Option<u64>) -> String {
    match ms {
        Some(ms) => {
            let s = ms / 1000;
            if s >= 3600 {
                format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
            } else {
                format!("{}:{:02}", s / 60, s % 60)
            }
        }
        None => "—".into(),
    }
}

fn hm(tz: Option<&TimeZone>, epoch: i64) -> String {
    local_hms(tz, epoch).map(|s| s.chars().take(5).collect()).unwrap_or_else(|| "—".into())
}

/// Provenance courte : playlist (› membre) · origine règle · par source.
fn provenance(t: &Track) -> String {
    let mut v = Vec::new();
    if !t.playlist_ref.is_empty() {
        v.push(t.playlist_ref.clone());
    }
    if !t.leaf_ref.is_empty() && t.leaf_ref != t.playlist_ref {
        v.push(format!("› {}", t.leaf_ref));
    }
    let origin = origin_label(&t.origin);
    if !origin.is_empty() {
        let rule = if t.rule_id.is_empty() { String::new() } else { format!(" {}", t.rule_id) };
        v.push(format!("· {origin}{rule}"));
    }
    if !t.override_source.is_empty() {
        v.push(tr!("track-pushed-by", source = t.override_source.clone()));
    }
    v.join(" ")
}

/// Origine (code envoyé par stationd) → libellé traduit.
fn origin_label(o: &str) -> String {
    match o {
        "AtClockHard" => tr!("origin-at-clock-hard"),
        "AtClockSoft" => tr!("origin-at-clock-soft"),
        "Every" => tr!("origin-every"),
        "DayPart" => tr!("origin-day-part"),
        "BaseRotation" => tr!("origin-base-rotation"),
        "Override" => tr!("origin-override"),
        "Fallback" => tr!("origin-fallback"),
        "" => String::new(),
        other => other.to_string(),
    }
}

/// Note (opcode + paramètres) → phrase traduite. La correspondance est
/// exhaustive : un opcode ajouté au contrat sans traduction ne compile pas ;
/// un opcode inconnu (stationd plus récent) reste affiché avec son numéro.
fn note_text(n: &Note, tz: Option<&TimeZone>) -> String {
    use note::Code as C;
    let at = || n.at.map(|e| hm(tz, e)).unwrap_or_else(|| "—".into());
    let Ok(code) = C::try_from(n.code) else {
        return tr!("note-unknown", code = n.code);
    };
    match code {
        C::StationPaused => tr!("note-station-paused"),
        C::StationSleeping => tr!("note-station-sleeping"),
        C::SleepAtTrackEnd => tr!("note-sleep-at-track-end"),
        C::SleepArmed => tr!("note-sleep-armed"),
        C::LiveOnAir => tr!("note-live-on-air", dj = n.dj.clone()),
        C::NoLiquidsoap => tr!("note-no-liquidsoap"),
        C::Simulated => tr!("note-simulated"),
        C::PoolEmpty => tr!("note-pool-empty"),
        C::Fallback => tr!("note-fallback"),
        C::StreamUnknownDuration => tr!("note-stream-unknown-duration", media = n.media.clone()),
        C::UnknownDuration => tr!("note-unknown-duration", media = n.media.clone()),
        C::SimulationFailed => tr!("note-simulation-failed", reason = n.reason.clone()),
        C::PluginFilterFailed => {
            tr!("note-plugin-filter-failed", plugin = n.plugin.clone(), reason = n.reason.clone())
        }
        C::GridProjectionFailed => tr!("note-grid-projection-failed", reason = n.reason.clone()),
        C::HistoryUnreadable => tr!("note-history-unreadable", reason = n.reason.clone()),
        C::RendezvousWillNotCut => tr!(
            "note-rendezvous-will-not-cut",
            rule = n.rule.clone(),
            playlist = n.playlist.clone(),
            time = at()
        ),
        C::SourceWillBeEmpty => tr!(
            "note-source-will-be-empty",
            rule = n.rule.clone(),
            playlist = n.playlist.clone(),
            time = at()
        ),
        C::RendezvousNotCut => tr!(
            "note-rendezvous-not-cut",
            rule = n.rule.clone(),
            playlist = n.playlist.clone(),
            time = at(),
            count = n.count
        ),
        C::SourceWasEmpty => tr!(
            "note-source-was-empty",
            rule = n.rule.clone(),
            playlist = n.playlist.clone(),
            time = at(),
            count = n.count
        ),
        C::Unspecified => tr!("note-unknown", code = n.code),
    }
}

/// Incident de grille (prévu ou constaté) : affiché en rouge.
fn is_incident(n: &Note) -> bool {
    use note::Code as C;
    matches!(
        C::try_from(n.code),
        Ok(C::RendezvousWillNotCut | C::SourceWillBeEmpty | C::RendezvousNotCut | C::SourceWasEmpty)
    )
}

/// Créneau fautif (ce créneau ne diffusera rien) → texte traduit.
fn slot_issue(p: &PlaylistSlot) -> Option<String> {
    match Issue::try_from(p.issue).unwrap_or(Issue::None) {
        Issue::None => None,
        Issue::PoolEmpty => Some(tr!("slot-pool-empty")),
        Issue::NothingPlayable => Some(tr!("slot-nothing-playable")),
    }
}

/// Une ligne par créneau ; un créneau fautif gagne une seconde ligne en
/// rouge qui dit pourquoi (dossier §5.1 : la ligne fautive est marquée).
fn slot_lines<'a>(p: &PlaylistSlot, tz: Option<&TimeZone>, s: &Styles, width: usize) -> Vec<Line<'a>> {
    let when = p.from.map(|e| hm(tz, e)).unwrap_or_else(|| "     ".into());
    // Le nom passe avant l'origine : c'est lui qu'on cherche.
    let room = width.saturating_sub(8);
    let name = fit::ellipsize(&p.playlist_ref, room);
    let origin = fit::ellipsize(&origin_label(&p.origin), room.saturating_sub(name.chars().count() + 2));
    let issue = slot_issue(p);
    let name_style = if issue.is_some() { s.error() } else { Style::default() };
    let mut v = vec![Line::from(vec![
        Span::styled(format!(" {when}  "), if issue.is_some() { s.error() } else { s.label() }),
        Span::styled(name, name_style),
        Span::styled(format!("  {origin}"), s.muted()),
    ])];
    if let Some(why) = issue {
        v.push(Line::styled(format!("   ⚠ {}", fit::ellipsize(&why, width.saturating_sub(5))), s.error()));
    }
    v
}

fn titled<'a>(title: String, s: &Styles) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border())
        .title(Span::styled(format!(" {title} "), s.title()))
}

// --- rendu ---------------------------------------------------------------------

impl Antenne {
    fn render_on_air(&self, area: Rect, buf: &mut Buffer, snap: &OnAirSnapshot, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let tz = ctx.store.tz.as_ref();
        let block = titled(tr!("onair-title"), &s);
        let inner = block.inner(area);
        block.render(area, buf);
        let [l1, l2, l3] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);

        let (state_txt, state_style) = match snap.state.as_str() {
            "running" => (format!("▶ {}", tr!("onair-state-running")), s.ok()),
            "paused" => (format!("❚❚ {}", tr!("onair-state-paused")), s.warn()),
            "draining" => (format!("▶ {}", tr!("onair-state-draining")), s.warn()),
            "sleeping" => (format!("■ {}", tr!("onair-state-sleeping")), s.calm()),
            _ => (tr!("state-unknown"), s.muted()),
        };
        let right = Span::styled(format!(" {state_txt} "), state_style);
        let [a, b] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(right.width() as u16)]).areas(l1);
        Paragraph::new(right).render(b, buf);

        match &snap.on_air {
            Some(t) => {
                Paragraph::new(Span::styled(fit::ellipsize(&label(t), a.width as usize), s.accent()))
                    .render(a, buf);
                // Progression, interpolée à l'horloge locale entre deux
                // instantanés ; figée en pause ; absente pour un flux.
                let now = if snap.state == "paused" {
                    snap.observed_at
                } else {
                    jiff::Timestamp::now().as_second()
                };
                let elapsed = t.started_at.map(|st| (now - st).max(0) as u64 * 1000);
                match (elapsed, t.duration_ms) {
                    (Some(e), Some(d)) if d > 0 => {
                        let ratio = (e as f64 / d as f64).clamp(0.0, 1.0);
                        let left = d.saturating_sub(e);
                        let label = format!("{} / {}  -{}", mmss(Some(e)), mmss(Some(d)), mmss(Some(left)));
                        LineGauge::default()
                            .ratio(ratio)
                            .label(Span::styled(label, s.label()))
                            .filled_style(s.accent())
                            .unfilled_style(s.muted())
                            .render(l2, buf);
                    }
                    (Some(e), _) => {
                        let txt = if t.stream { tr!("onair-stream-continuous") } else { tr!("onair-duration-unknown") };
                        Paragraph::new(Span::styled(format!("{} · {txt}", mmss(Some(e))), s.muted()))
                            .render(l2, buf);
                    }
                    _ => {}
                }
                let since = t
                    .started_at
                    .map(|e| format!("{} · ", tr!("onair-since", time = hm(tz, e))))
                    .unwrap_or_default();
                Paragraph::new(Span::styled(
                    fit::ellipsize(&format!("{since}{}", provenance(t)), l3.width as usize),
                    s.muted(),
                ))
                .render(l3, buf);
            }
            None => {
                let what = match (snap.on_air_kind.as_str(), snap.live_dj.is_empty(), snap.liquidsoap) {
                    (_, false, _) => tr!("onair-live", dj = snap.live_dj.clone()),
                    ("fallback", ..) => tr!("onair-fallback"),
                    ("halted", ..) => tr!("onair-halted"),
                    (_, _, false) => tr!("onair-no-liquidsoap"),
                    ("", ..) => tr!("onair-nothing-reported"),
                    (other, ..) => other.to_string(),
                };
                let style = if snap.on_air_kind == "fallback" { s.error() } else { s.muted() };
                Paragraph::new(Span::styled(what, style)).render(a, buf);
            }
        }
    }

    fn render_playlists(&self, area: Rect, buf: &mut Buffer, snap: &OnAirSnapshot, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let tz = ctx.store.tz.as_ref();
        let block = titled(tr!("playlists-title"), &s);
        let inner = block.inner(area);
        block.render(area, buf);
        let w = inner.width as usize;
        let mut lines: Vec<Line> = Vec::new();
        match &snap.current_playlist {
            Some(p) => {
                let issue = slot_issue(p);
                lines.push(Line::from(vec![
                    Span::styled(" ● ", if issue.is_some() { s.error() } else { s.ok() }),
                    Span::styled(
                        fit::ellipsize(&p.playlist_ref, w.saturating_sub(4)),
                        if issue.is_some() { s.error() } else { s.accent() },
                    ),
                ]));
                if let Some(why) = issue {
                    lines.push(Line::styled(format!("   ⚠ {}", fit::ellipsize(&why, w.saturating_sub(5))), s.error()));
                }
            }
            None => lines.push(Line::styled(" ● —", s.muted())),
        }
        for p in &snap.next_playlists {
            lines.extend(slot_lines(p, tz, &s, w));
        }
        if !snap.indicative.is_empty() {
            lines.push(Line::styled(format!(" {}", tr!("playlists-by-track-count")), s.label()));
            for p in &snap.indicative {
                lines.push(Line::styled(
                    format!("   {}  ({})", fit::ellipsize(&p.playlist_ref, w.saturating_sub(6)), p.rule_id),
                    s.muted(),
                ));
            }
        }
        Paragraph::new(lines).render(inner, buf);
    }

    fn render_next(&self, area: Rect, buf: &mut Buffer, snap: &OnAirSnapshot, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let tz = ctx.store.tz.as_ref();
        let block = titled(tr!("next-title"), &s);
        let inner = block.inner(area);
        block.render(area, buf);

        let notes_h = (snap.notes.len() as u16).min(NOTES_MAX as u16);
        let [table_a, notes_a] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(notes_h)]).areas(inner);

        let mut rows: Vec<Row> = Vec::new();
        // Première colonne à la largeur de ce qu'elle contient (le libellé
        // « préparé » varie selon la langue), jamais coupée.
        let prepared = tr!("next-prepared");
        let first_w = (Span::raw(prepared.as_str()).width().max("~ 00:00".len())) as u16;
        if let Some(t) = &snap.prefetched {
            rows.push(Row::new(vec![
                Cell::from(Span::styled(prepared.clone(), s.ok())),
                Cell::from(label(t)),
                Cell::from(Span::styled(mmss(t.duration_ms), s.label())),
                Cell::from(Span::styled(provenance(t), s.muted())),
            ]));
        }
        let room = self.upcoming.saturating_sub(rows.len());

        // The simulated playout may legitimately omit a hard rendez-vous when
        // its short expiry would already be missed by the projected tracks.
        // Keep the grid rendez-vous visible nevertheless: it is an important
        // operator fact, distinct from the simulated promise of what will
        // actually air. Insert it at its clock position unless the simulation
        // already contains the AtClockHard track itself.
        let hard = snap
            .next_playlists
            .iter()
            .find(|p| p.origin.as_deref() == Some("AtClockHard") && p.from.is_some())
            .filter(|p| {
                !snap
                    .upcoming
                    .iter()
                    .any(|t| t.origin.as_deref() == Some("AtClockHard") && t.rule_id == p.rule_id)
            });

        let mut hard_done = false;
        for t in snap.upcoming.iter().take(room) {
            if let Some(h) = hard
                && !hard_done
                && t.estimated_at.is_some_and(|at| at >= h.from.unwrap_or(i64::MAX))
            {
                let at = h.from.unwrap();
                rows.push(Row::new(vec![
                    Cell::from(Span::styled(format!("! {}", hm(tz, at)), s.warn())),
                    Cell::from(h.playlist_ref.clone()),
                    Cell::from(Span::styled("—", s.label())),
                    Cell::from(Span::styled(origin_label("AtClockHard"), s.warn())),
                ]));
                hard_done = true;
            }

            let when = t.estimated_at.map(|e| format!("~ {}", hm(tz, e))).unwrap_or_else(|| "~ —".into());
            let mut from = provenance(t);
            if let Some(c) = t.cut_at {
                from = format!("{} · {from}", tr!("next-cut-at", time = hm(tz, c)));
            }
            rows.push(Row::new(vec![
                Cell::from(Span::styled(when, s.warn())),
                Cell::from(label(t)),
                Cell::from(Span::styled(mmss(t.duration_ms), s.label())),
                Cell::from(Span::styled(from, s.muted())),
            ]));
        }
        if let Some(h) = hard.filter(|_| !hard_done)
            && rows.len() < self.upcoming
        {
            let at = h.from.unwrap();
            rows.push(Row::new(vec![
                Cell::from(Span::styled(format!("! {}", hm(tz, at)), s.warn())),
                Cell::from(h.playlist_ref.clone()),
                Cell::from(Span::styled("—", s.label())),
                Cell::from(Span::styled(origin_label("AtClockHard"), s.warn())),
            ]));
        }
        if rows.is_empty() {
            Paragraph::new(Span::styled(format!(" {}", tr!("next-nothing")), s.muted())).render(table_a, buf);
        } else {
            let wide = table_a.width >= 70;
            let widths: Vec<Constraint> = if wide {
                vec![Constraint::Length(first_w), Constraint::Fill(3), Constraint::Length(7), Constraint::Fill(2)]
            } else {
                vec![Constraint::Length(first_w), Constraint::Fill(1), Constraint::Length(6), Constraint::Length(0)]
            };
            Table::new(rows, widths).column_spacing(1).render(table_a, buf);
        }
        // Les incidents de grille d'abord : c'est ce qu'il faut corriger.
        let (incidents, others): (Vec<&Note>, Vec<&Note>) = snap.notes.iter().partition(|n| is_incident(n));
        let notes: Vec<Line> = incidents
            .into_iter()
            .chain(others)
            .take(NOTES_MAX)
            .map(|n| {
                Line::styled(
                    format!(" ⚠ {}", fit::ellipsize(&note_text(n, tz), notes_a.width.saturating_sub(4) as usize)),
                    if is_incident(n) { s.error() } else { s.warn() },
                )
            })
            .collect();
        Paragraph::new(notes).render(notes_a, buf);
    }

    fn render_history(&self, area: Rect, buf: &mut Buffer, snap: &OnAirSnapshot, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let tz = ctx.store.tz.as_ref();
        let block = titled(tr!("played-title"), &s);
        let inner = block.inner(area);
        block.render(area, buf);
        if snap.history.is_empty() {
            Paragraph::new(Span::styled(format!(" {}", tr!("played-nothing")), s.muted())).render(inner, buf);
            return;
        }
        // Colonne « issue » à la largeur du plus long libellé traduit.
        let end_w = [tr!("played-aired"), tr!("played-cut"), tr!("played-end-unknown")]
            .iter()
            .map(|l| Span::raw(l.as_str()).width())
            .max()
            .unwrap_or(8) as u16;
        let rows: Vec<Row> = snap
            .history
            .iter()
            .take(inner.height as usize)
            .map(|t| {
                let (end, style): (String, Style) = match Outcome::try_from(t.outcome).unwrap_or(Outcome::Unspecified) {
                    Outcome::Aired => (tr!("played-aired"), s.ok()),
                    Outcome::Cut => (tr!("played-cut"), s.warn()),
                    Outcome::Unknown | Outcome::Unspecified => (tr!("played-end-unknown"), s.muted()),
                };
                Row::new(vec![
                    Cell::from(Span::styled(
                        t.started_at.and_then(|e| crate::store::local_day_hm(tz, e)).unwrap_or_default(),
                        s.label(),
                    )),
                    Cell::from(label(t)),
                    Cell::from(Span::styled(mmss(t.duration_ms), s.label())),
                    Cell::from(Span::styled(end, style)),
                    Cell::from(Span::styled(provenance(t), s.muted())),
                ])
            })
            .collect();
        // `JJ/MM HH:MM`, suffixé ` UTC` tant que le fuseau est inconnu.
        let when_w = if tz.is_some() { 11 } else { 15 };
        let widths = if inner.width >= 90 {
            vec![
                Constraint::Length(when_w),
                Constraint::Fill(3),
                Constraint::Length(7),
                Constraint::Length(end_w),
                Constraint::Fill(2),
            ]
        } else {
            vec![
                Constraint::Length(when_w),
                Constraint::Fill(1),
                Constraint::Length(6),
                Constraint::Length(end_w),
                Constraint::Length(0),
            ]
        };
        Table::new(rows, widths).column_spacing(1).render(inner, buf);
    }

    /// Repli sans flux : ce que dit Liquidsoap, et pourquoi le flux manque.
    fn render_fallback(&self, area: Rect, buf: &mut Buffer, why: &str, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let store = &ctx.store;
        let field = |key: String| Span::styled(format!(" {:<13}", key), s.label());
        let mut lines = vec![Line::styled(format!(" {why}"), s.warn()), Line::default()];
        match store.liquidsoap.value.as_ref() {
            Some(ls) if ls.enabled => {
                let at = |e: i64| local_hms(store.tz.as_ref(), e).unwrap_or_default();
                lines.push(Line::from(vec![
                    field(tr!("fallback-on-air")),
                    Span::raw(if ls.on_air_media.is_empty() { ls.on_air_kind.clone() } else { ls.on_air_media.clone() }),
                ]));
                if ls.on_air_since > 0 {
                    let wall = (jiff::Timestamp::now().as_second() - ls.on_air_since).max(0) as u64;
                    lines.push(Line::from(vec![
                        field(tr!("fallback-since")),
                        Span::raw(tr!(
                            "fallback-since-value",
                            time = at(ls.on_air_since),
                            ago = human_duration(Duration::from_secs(wall))
                        )),
                    ]));
                }
                lines.push(Line::from(vec![
                    field(tr!("fallback-prepared")),
                    Span::raw(if ls.next_media.is_empty() { "—".to_string() } else { ls.next_media.clone() }),
                ]));
            }
            Some(_) => lines.push(Line::styled(format!(" {}", tr!("fallback-no-liquidsoap")), s.muted())),
            None => lines.push(Line::styled(format!(" {}", tr!("fallback-unknown")), s.muted())),
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(titled(tr!("fallback-title"), &s))
            .render(area, buf);
    }
}

impl Screen for Antenne {
    fn title(&self) -> String {
        tr!("screen-antenne")
    }

    fn help(&self) -> &'static [KeyHelp] {
        &[
            (k!("key-space"), k!("help-pause-resume")),
            (k!("key-n"), k!("help-skip")),
            (k!("key-o"), k!("help-override")),
            (k!("key-plus-minus"), k!("help-upcoming-count")),
        ]
    }

    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        if let AppEvent::Event(Event::Key(k)) = event
            && k.kind == KeyEventKind::Press
        {
            match k.code {
                KeyCode::Char('+') => {
                    self.upcoming = (self.upcoming + 5).min(UPCOMING_MAX);
                    return Ok(Control::Changed);
                }
                KeyCode::Char('-') => {
                    self.upcoming = self.upcoming.saturating_sub(5).max(1);
                    return Ok(Control::Changed);
                }
                KeyCode::Char(' ') => {
                    match ops::toggle_pause(&ctx.store) {
                        Some(ops::Outcome::Open(m)) => ctx.open(m),
                        Some(ops::Outcome::Run(a)) => ctx.request(a),
                        None => return Ok(Control::Continue),
                    }
                    return Ok(Control::Changed);
                }
                KeyCode::Char('n') => {
                    ctx.open(ops::skip(&ctx.store));
                    return Ok(Control::Changed);
                }
                KeyCode::Char('o') => {
                    ctx.open(ops::push_override());
                    return Ok(Control::Changed);
                }
                _ => {}
            }
        }
        Ok(Control::Continue)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        Block::new().style(s.base()).render(area, buf);
        let snap = match (&ctx.store.onair, &ctx.store.onair_link) {
            (Some(snap), _) => snap.clone(),
            (None, Err(why)) => {
                let why = why.clone();
                self.render_fallback(area, buf, &why, ctx);
                return Ok(());
            }
            (None, Ok(())) => {
                self.render_fallback(area, buf, &tr!("onair-waiting"), ctx);
                return Ok(());
            }
        };

        let [on_air_a, rest, link_a] = Layout::vertical([
            Constraint::Length(5),
            Constraint::Fill(1),
            Constraint::Length(u16::from(ctx.store.onair_link.is_err())),
        ])
        .areas(area);
        self.render_on_air(on_air_a, buf, &snap, ctx);

        if area.width >= 100 {
            let [middle, bottom] =
                Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(rest);
            let pl_w = (area.width * 3 / 10).max(34);
            let [pl, next] = Layout::horizontal([Constraint::Length(pl_w), Constraint::Fill(1)]).areas(middle);
            self.render_playlists(pl, buf, &snap, ctx);
            self.render_next(next, buf, &snap, ctx);
            self.render_history(bottom, buf, &snap, ctx);
        } else {
            // Étroit : playlists en une ligne, puis à suivre et joués empilés.
            let [pl, next, bottom] =
                Layout::vertical([Constraint::Length(1), Constraint::Percentage(55), Constraint::Fill(1)])
                    .areas(rest);
            let cur = snap.current_playlist.as_ref().map(|p| p.playlist_ref.clone()).unwrap_or_else(|| "—".into());
            let then = snap
                .next_playlists
                .first()
                .map(|p| {
                    let warn = if slot_issue(p).is_some() { " ⚠" } else { "" };
                    format!("  → {} {}{warn}", p.from.map(|e| hm(ctx.store.tz.as_ref(), e)).unwrap_or_default(), p.playlist_ref)
                })
                .unwrap_or_default();
            Paragraph::new(Line::from(vec![
                Span::styled(" ● ", s.ok()),
                Span::styled(cur, s.accent()),
                Span::styled(fit::ellipsize(&then, pl.width.saturating_sub(10) as usize), s.muted()),
            ]))
            .render(pl, buf);
            self.render_next(next, buf, &snap, ctx);
            self.render_history(bottom, buf, &snap, ctx);
        }
        if let Err(why) = &ctx.store.onair_link {
            Paragraph::new(Span::styled(format!(" ~ {}", tr!("onair-link-lost", reason = why.clone())), s.warn()))
                .render(link_a, buf);
        }
        Ok(())
    }
}
