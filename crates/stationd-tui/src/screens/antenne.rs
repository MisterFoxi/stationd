//! Antenne (`1`) — vue principale (dossier §5.1).
//!
//! Tout vient du flux `OnAirService.Watch` : morceau à l'antenne et sa
//! progression, playlist en cours et playlists à suivre, morceau préchargé
//! (certain) puis morceaux théoriques (simulés, `~`), derniers joués avec leur
//! issue, et les notes qui disent pourquoi la suite peut changer.
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
use stationd_proto::onair::{track::Outcome, OnAirSnapshot, PlaylistSlot, Track};

use crate::app::{AppEvent, Global};
use crate::fit;
use crate::screen::{KeyHelp, Screen};
use crate::store::{human_duration, local_hms};
use crate::style::Styles;

const UPCOMING_DEFAULT: usize = 10;
const UPCOMING_MAX: usize = 30;

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
fn label(t: &Track) -> String {
    let file = t.rel_path.rsplit('/').next().unwrap_or(&t.rel_path);
    match (t.artist.is_empty(), t.title.is_empty()) {
        (false, false) => format!("{} — {}", t.artist, t.title),
        (true, false) => t.title.clone(),
        _ if t.stream => format!("relais {}", t.rel_path),
        _ if file.is_empty() => "—".into(),
        _ => format!("{file} (titre non renseigné)"),
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

/// Provenance courte : playlist (› membre) · origine règle.
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
        v.push(format!("par {}", t.override_source));
    }
    v.join(" ")
}

fn origin_label(o: &str) -> &'static str {
    match o {
        "AtClockHard" => "rendez-vous (coupe)",
        "AtClockSoft" => "rendez-vous",
        "Every" => "every",
        "DayPart" => "tranche",
        "BaseRotation" => "base",
        "Override" => "override",
        "Fallback" => "FALLBACK",
        "" => "",
        _ => "?",
    }
}

fn slot_line<'a>(p: &PlaylistSlot, tz: Option<&TimeZone>, s: &Styles, width: usize) -> Line<'a> {
    let when = p.from.map(|e| hm(tz, e)).unwrap_or_else(|| "     ".into());
    let origin = origin_label(&p.origin);
    let name = fit::ellipsize(&p.playlist_ref, width.saturating_sub(8 + origin.len() + 2));
    Line::from(vec![
        Span::styled(format!(" {when}  "), s.label()),
        Span::raw(name),
        Span::styled(format!("  {origin}"), s.muted()),
    ])
}

fn titled<'a>(title: &'a str, s: &Styles) -> Block<'a> {
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
        let block = titled("À l'antenne", &s);
        let inner = block.inner(area);
        block.render(area, buf);
        let [l1, l2, l3] = Layout::vertical([Constraint::Length(1); 3]).areas(inner);

        let (state_txt, state_style) = match snap.state.as_str() {
            "running" => ("▶ EN COURS", s.ok()),
            "paused" => ("❚❚ PAUSE", s.warn()),
            "draining" => ("▶ VEILLE ARMÉE", s.warn()),
            "sleeping" => ("■ VEILLE", s.calm()),
            _ => ("état ?", s.muted()),
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
                        let txt = if t.stream { "flux continu" } else { "durée inconnue" };
                        Paragraph::new(Span::styled(format!("{} · {txt}", mmss(Some(e))), s.muted()))
                            .render(l2, buf);
                    }
                    _ => {}
                }
                let since = t.started_at.map(|e| format!("depuis {} · ", hm(tz, e))).unwrap_or_default();
                Paragraph::new(Span::styled(
                    fit::ellipsize(&format!("{since}{}", provenance(t)), l3.width as usize),
                    s.muted(),
                ))
                .render(l3, buf);
            }
            None => {
                let what = match (snap.on_air_kind.as_str(), snap.live_dj.is_empty(), snap.liquidsoap) {
                    (_, false, _) => format!("LIVE — {}", snap.live_dj),
                    ("fallback", ..) => "FALLBACK (filet de sécurité)".into(),
                    ("halted", ..) => "à l'arrêt (bruit de fond)".into(),
                    (_, _, false) => "pas de [liquidsoap] : rien n'est diffusé".into(),
                    ("", ..) => "rien reçu de Liquidsoap".into(),
                    (k, ..) => k.to_string(),
                };
                let style = if snap.on_air_kind == "fallback" { s.error() } else { s.muted() };
                Paragraph::new(Span::styled(what, style)).render(a, buf);
            }
        }
    }

    fn render_playlists(&self, area: Rect, buf: &mut Buffer, snap: &OnAirSnapshot, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let tz = ctx.store.tz.as_ref();
        let block = titled("Playlists", &s);
        let inner = block.inner(area);
        block.render(area, buf);
        let w = inner.width as usize;
        let mut lines: Vec<Line> = Vec::new();
        match &snap.current_playlist {
            Some(p) => lines.push(Line::from(vec![
                Span::styled(" ● ", s.ok()),
                Span::styled(fit::ellipsize(&p.playlist_ref, w.saturating_sub(4)), s.accent()),
            ])),
            None => lines.push(Line::styled(" ● —", s.muted())),
        }
        for p in &snap.next_playlists {
            lines.push(slot_line(p, tz, &s, w));
        }
        if !snap.indicative.is_empty() {
            lines.push(Line::styled(" au compteur de titres :", s.label()));
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
        let block = titled("À suivre", &s);
        let inner = block.inner(area);
        block.render(area, buf);

        let notes_h = if snap.notes.is_empty() { 0 } else { (snap.notes.len() as u16).min(3) };
        let [table_a, notes_a] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(notes_h)]).areas(inner);

        let mut rows: Vec<Row> = Vec::new();
        if let Some(t) = &snap.prefetched {
            rows.push(Row::new(vec![
                Cell::from(Span::styled("préparé", s.ok())),
                Cell::from(label(t)),
                Cell::from(Span::styled(mmss(t.duration_ms), s.label())),
                Cell::from(Span::styled(provenance(t), s.muted())),
            ]));
        }
        let room = self.upcoming.saturating_sub(rows.len());
        for t in snap.upcoming.iter().take(room) {
            let when = t.estimated_at.map(|e| format!("~ {}", hm(tz, e))).unwrap_or_else(|| "~ —".into());
            let mut from = provenance(t);
            if let Some(c) = t.cut_at {
                from = format!("coupé {} · {from}", hm(tz, c));
            }
            rows.push(Row::new(vec![
                Cell::from(Span::styled(when, s.warn())),
                Cell::from(label(t)),
                Cell::from(Span::styled(mmss(t.duration_ms), s.label())),
                Cell::from(Span::styled(from, s.muted())),
            ]));
        }
        if rows.is_empty() {
            Paragraph::new(Span::styled(" rien", s.muted())).render(table_a, buf);
        } else {
            let wide = table_a.width >= 70;
            let widths: Vec<Constraint> = if wide {
                vec![Constraint::Length(8), Constraint::Fill(3), Constraint::Length(7), Constraint::Fill(2)]
            } else {
                vec![Constraint::Length(8), Constraint::Fill(1), Constraint::Length(6), Constraint::Length(0)]
            };
            Table::new(rows, widths).column_spacing(1).render(table_a, buf);
        }
        let notes: Vec<Line> = snap
            .notes
            .iter()
            .take(3)
            .map(|n| Line::styled(format!(" ⚠ {}", fit::ellipsize(n, notes_a.width.saturating_sub(4) as usize)), s.warn()))
            .collect();
        Paragraph::new(notes).render(notes_a, buf);
    }

    fn render_history(&self, area: Rect, buf: &mut Buffer, snap: &OnAirSnapshot, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let tz = ctx.store.tz.as_ref();
        let block = titled("Joués", &s);
        let inner = block.inner(area);
        block.render(area, buf);
        if snap.history.is_empty() {
            Paragraph::new(Span::styled(" rien encore", s.muted())).render(inner, buf);
            return;
        }
        let rows: Vec<Row> = snap
            .history
            .iter()
            .take(inner.height as usize)
            .map(|t| {
                let (end, style): (&str, Style) = match Outcome::try_from(t.outcome).unwrap_or(Outcome::Unspecified) {
                    Outcome::Aired => ("diffusé", s.ok()),
                    Outcome::Cut => ("coupé", s.warn()),
                    Outcome::Unknown | Outcome::Unspecified => ("fin ?", s.muted()),
                };
                Row::new(vec![
                    Cell::from(Span::styled(t.started_at.map(|e| hm(tz, e)).unwrap_or_default(), s.label())),
                    Cell::from(label(t)),
                    Cell::from(Span::styled(mmss(t.duration_ms), s.label())),
                    Cell::from(Span::styled(end, style)),
                    Cell::from(Span::styled(provenance(t), s.muted())),
                ])
            })
            .collect();
        let widths = if inner.width >= 90 {
            vec![
                Constraint::Length(5),
                Constraint::Fill(3),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Fill(2),
            ]
        } else {
            vec![
                Constraint::Length(5),
                Constraint::Fill(1),
                Constraint::Length(6),
                Constraint::Length(8),
                Constraint::Length(0),
            ]
        };
        Table::new(rows, widths).column_spacing(1).render(inner, buf);
    }

    /// Repli sans flux : ce que dit Liquidsoap, et pourquoi le flux manque.
    fn render_fallback(&self, area: Rect, buf: &mut Buffer, why: &str, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let store = &ctx.store;
        let mut lines = vec![Line::styled(format!(" {why}"), s.warn()), Line::default()];
        match store.liquidsoap.value.as_ref() {
            Some(ls) if ls.enabled => {
                let at = |e: i64| local_hms(store.tz.as_ref(), e).unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(" À l'antenne  ", s.label()),
                    Span::raw(if ls.on_air_media.is_empty() { ls.on_air_kind.clone() } else { ls.on_air_media.clone() }),
                ]));
                if ls.on_air_since > 0 {
                    let wall = (jiff::Timestamp::now().as_second() - ls.on_air_since).max(0) as u64;
                    lines.push(Line::from(vec![
                        Span::styled(" Depuis       ", s.label()),
                        Span::raw(format!("{} (il y a {})", at(ls.on_air_since), human_duration(Duration::from_secs(wall)))),
                    ]));
                }
                lines.push(Line::from(vec![
                    Span::styled(" Préparé      ", s.label()),
                    Span::raw(if ls.next_media.is_empty() { "—".to_string() } else { ls.next_media.clone() }),
                ]));
            }
            Some(_) => lines.push(Line::styled(" Liquidsoap non configuré", s.muted())),
            None => lines.push(Line::styled(" Antenne inconnue", s.muted())),
        }
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(titled("À l'antenne (repli)", &s)).render(area, buf);
    }
}

impl Screen for Antenne {
    fn title(&self) -> &'static str {
        "Antenne"
    }

    fn help(&self) -> &'static [KeyHelp] {
        &[("+ / -", "morceaux à suivre affichés")]
    }

    fn event(&mut self, event: &AppEvent, _ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
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
                self.render_fallback(area, buf, why, ctx);
                return Ok(());
            }
            (None, Ok(())) => {
                self.render_fallback(area, buf, "en attente du premier instantané…", ctx);
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
            let [pl, next] = Layout::horizontal([Constraint::Length(34), Constraint::Fill(1)]).areas(middle);
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
                .map(|p| format!("  → {} {}", p.from.map(|e| hm(ctx.store.tz.as_ref(), e)).unwrap_or_default(), p.playlist_ref))
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
            Paragraph::new(Span::styled(format!(" ~ {why} — dernier instantané affiché"), s.warn())).render(link_a, buf);
        }
        Ok(())
    }
}
