//! Bandeau permanent (2 lignes) — dossier §5 :
//! ligne 1 : station · état de diffusion · liaison stationd · auditeurs · live · overrides
//! ligne 2 : à l'antenne (résumé) · horloge et fuseau station · uptime

use std::time::Instant;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::paragraph::Paragraph;
use stationd_proto::broadcast::State;

use crate::store::{Link, Store, human_duration};
use crate::fit;
use crate::style::Styles;

const SEP: &str = "  │  ";

pub fn render(area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
    let now = Instant::now();
    let narrow = area.width < 100;
    let [l1, l2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    // --- ligne 1 : segments par priorité décroissante, jamais coupés ----------
    let right = link(store, now, s, narrow);
    let right_w = right.width() as u16 + 1;
    let [a, b] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_w)]).areas(l1);

    let name = store
        .status
        .value
        .as_ref()
        .map(|st| st.station_name.clone())
        .unwrap_or_else(|| "station ?".into());
    let mut segs: Vec<Vec<Span>> = vec![
        vec![Span::styled(format!(" {} ", fit::ellipsize(&name, 24)), s.title())],
        broadcast_state(store, now, s, narrow),
        vec![listeners(store, now, s, narrow)],
    ];
    if let Some(dj) = live_dj(store) {
        segs.push(vec![Span::styled(format!("LIVE {dj}"), s.warn())]);
    }
    if let Some(n) = store.overrides.value.as_ref().map(Vec::len).filter(|n| *n > 0) {
        let text = if narrow { format!("{n} ovr") } else { format!("{n} override(s) en attente") };
        segs.push(vec![Span::styled(text, s.accent())]);
    }
    let left = fit::segments(segs, Span::raw(if narrow { " │ " } else { SEP }), a.width as usize);
    Paragraph::new(left).style(s.banner()).render(a, buf);
    Paragraph::new(right).right_aligned().style(s.banner()).render(b, buf);

    // --- ligne 2 : à l'antenne (abrégé) · horloge · uptime ------------------
    let clock = clock_line(store, now, s, narrow);
    let clock_w = clock.width() as u16 + 1;
    let [a, b] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(clock_w)]).areas(l2);
    Paragraph::new(on_air(store, s, a.width as usize)).style(s.banner()).render(a, buf);
    Paragraph::new(clock).right_aligned().style(s.banner()).render(b, buf);
}

fn broadcast_state<'a>(store: &Store, now: Instant, s: &Styles, narrow: bool) -> Vec<Span<'a>> {
    let Some(b) = store.broadcast.value.as_ref() else {
        return vec![Span::styled("diffusion : inconnue", s.muted())];
    };
    let (text, style) = match State::try_from(b.state).unwrap_or(State::Unspecified) {
        State::Running => ("RUNNING", s.ok()),
        State::Paused => ("PAUSED", s.warn()),
        State::Draining if narrow => ("DRAINING", s.warn()),
        State::Draining => ("DRAINING (veille à la fin de la piste)", s.warn()),
        State::Sleeping if narrow => ("SLEEPING", s.calm()),
        State::Sleeping => ("SLEEPING (en veille)", s.calm()),
        State::Unspecified => ("état ?", s.muted()),
    };
    let mut v = vec![Span::styled(text, style)];
    if store.broadcast.is_stale(now) {
        v.push(Span::styled(" ~ancien", s.muted()));
    }
    v
}

/// Auditeurs : `—` si jamais reçu ou inconnu de stationd (jamais 0 à la place
/// d'inconnu), `~N (ancien)` si la mesure a vieilli.
fn listeners<'a>(store: &Store, now: Instant, s: &Styles, narrow: bool) -> Span<'a> {
    let label = if narrow { "aud." } else { "auditeurs" };
    match store.broadcast.value.as_ref().and_then(|b| b.listeners) {
        None => Span::styled(format!("{label} —"), s.muted()),
        Some(n) if store.broadcast.is_stale(now) => {
            Span::styled(format!("{label} ~{n} (ancien)"), s.warn())
        }
        Some(n) => Span::styled(format!("{label} {n}"), s.accent()),
    }
}

fn live_dj(store: &Store) -> Option<String> {
    let l = store.live.value.as_ref()?;
    l.on_air.as_ref().map(|sess| sess.dj.clone())
}

fn link<'a>(store: &Store, now: Instant, s: &Styles, narrow: bool) -> Line<'a> {
    let host = store.addr.trim_start_matches("http://").trim_start_matches("https://").to_string();
    match &store.link {
        Link::Connecting => Line::from(Span::styled(format!("connexion à {host}… "), s.warn())),
        Link::Connected if narrow => Line::from(Span::styled("● connecté ", s.ok())),
        Link::Connected => Line::from(vec![
            Span::styled("● ", s.ok()),
            Span::styled(format!("stationd {host} "), s.label()),
        ]),
        Link::Lost { since, .. } if narrow => Line::from(Span::styled(
            format!("✕ injoignable {} ", human_duration(now.saturating_duration_since(*since))),
            s.error(),
        )),
        Link::Lost { since, .. } => Line::from(Span::styled(
            format!("✕ stationd injoignable depuis {} ", human_duration(now.saturating_duration_since(*since))),
            s.error(),
        )),
    }
}

fn on_air<'a>(store: &Store, s: &Styles, max: usize) -> Line<'a> {
    let head = " à l'antenne : ";
    let room = max.saturating_sub(head.chars().count() + 1);
    let mut v = vec![Span::styled(head, s.label())];
    match store.liquidsoap.value.as_ref() {
        Some(ls) if ls.on_air_kind == "track" && !ls.on_air_media.is_empty() => {
            let file = ls.on_air_media.rsplit('/').next().unwrap_or(&ls.on_air_media);
            let pl = if ls.on_air_playlist.is_empty() { String::new() } else { format!("  ({})", ls.on_air_playlist) };
            // Le nom du fichier passe avant la playlist quand la place manque.
            let file_room = room.saturating_sub(pl.chars().count().min(room / 3));
            let file = fit::ellipsize(file, file_room);
            let pl = fit::ellipsize(&pl, room.saturating_sub(file.chars().count()));
            v.push(Span::raw(file));
            v.push(Span::styled(pl, s.muted()));
        }
        Some(ls) if ls.on_air_kind == "fallback" => v.push(Span::styled("FALLBACK", s.error())),
        Some(ls) if ls.on_air_kind == "halted" => v.push(Span::styled("à l'arrêt", s.calm())),
        _ => v.push(Span::styled("—", s.muted())),
    }
    Line::from(v)
}

fn clock_line<'a>(store: &Store, now: Instant, s: &Styles, narrow: bool) -> Line<'a> {
    let mut v = Vec::new();
    match (&store.tz, &store.tz_name) {
        (Some(tz), Some(name)) => {
            let t = jiff::Timestamp::now().to_zoned(tz.clone());
            v.push(Span::styled(t.strftime("%H:%M:%S").to_string(), s.title()));
            // Le fuseau reste visible même en étroit : une heure sans fuseau est ambiguë.
            v.push(Span::styled(format!(" {name}"), s.label()));
        }
        (None, Some(name)) => {
            v.push(Span::styled(format!("fuseau « {name} » introuvable"), s.error()));
        }
        _ => v.push(Span::styled("fuseau —", s.muted())),
    }
    v.push(Span::raw(if narrow { " │ " } else { SEP }));
    match store.uptime(now) {
        Some(u) if narrow => v.push(Span::styled(format!("up {} ", human_duration(u)), s.label())),
        Some(u) => v.push(Span::styled(format!("uptime {} ", human_duration(u)), s.label())),
        None => v.push(Span::styled(if narrow { "up — " } else { "uptime — " }, s.muted())),
    }
    Line::from(v)
}
