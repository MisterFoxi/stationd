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
use crate::{fit, tr};
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
        .unwrap_or_else(|| tr!("banner-station-unknown"));
    let mut segs: Vec<Vec<Span>> = vec![
        vec![Span::styled(format!(" {} ", fit::ellipsize(&name, 24)), s.title())],
        broadcast_state(store, now, s, narrow),
        vec![listeners(store, now, s, narrow)],
    ];
    if let Some(dj) = live_dj(store) {
        segs.push(vec![Span::styled(tr!("banner-live", dj = dj), s.warn())]);
    }
    if let Some(n) = store.overrides.value.as_ref().map(Vec::len).filter(|n| *n > 0) {
        let text = if narrow { tr!("banner-overrides-short", n = n) } else { tr!("banner-overrides", n = n) };
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
        return vec![Span::styled(tr!("banner-state-unknown"), s.muted())];
    };
    let (text, style) = match State::try_from(b.state).unwrap_or(State::Unspecified) {
        State::Running => (tr!("state-running"), s.ok()),
        State::Paused => (tr!("state-paused"), s.warn()),
        State::Draining if narrow => (tr!("state-draining"), s.warn()),
        State::Draining => (tr!("state-draining-long"), s.warn()),
        State::Sleeping if narrow => (tr!("state-sleeping"), s.calm()),
        State::Sleeping => (tr!("state-sleeping-long"), s.calm()),
        State::Unspecified => (tr!("state-unknown"), s.muted()),
    };
    let mut v = vec![Span::styled(text, style)];
    if store.broadcast.is_stale(now) {
        v.push(Span::styled(format!(" {}", tr!("banner-stale")), s.muted()));
    }
    v
}

/// Auditeurs : `—` si jamais reçu ou inconnu de stationd (jamais 0 à la place
/// d'inconnu), `~N (ancien)` si la mesure a vieilli.
fn listeners<'a>(store: &Store, now: Instant, s: &Styles, narrow: bool) -> Span<'a> {
    let label = if narrow { tr!("banner-listeners-short") } else { tr!("banner-listeners") };
    match store.broadcast.value.as_ref().and_then(|b| b.listeners) {
        None => Span::styled(tr!("banner-listeners-unknown", label = label), s.muted()),
        Some(n) if store.broadcast.is_stale(now) => {
            Span::styled(tr!("banner-listeners-stale", label = label, n = n), s.warn())
        }
        Some(n) => Span::styled(tr!("banner-listeners-count", label = label, n = n), s.accent()),
    }
}

fn live_dj(store: &Store) -> Option<String> {
    let l = store.live.value.as_ref()?;
    l.on_air.as_ref().map(|sess| sess.dj.clone())
}

fn link<'a>(store: &Store, now: Instant, s: &Styles, narrow: bool) -> Line<'a> {
    let host = store.addr.trim_start_matches("http://").trim_start_matches("https://").to_string();
    match &store.link {
        Link::Connecting => Line::from(Span::styled(format!("{} ", tr!("link-connecting", host = host)), s.warn())),
        Link::Connected if narrow => Line::from(Span::styled(format!("● {} ", tr!("link-connected")), s.ok())),
        Link::Connected => Line::from(vec![
            Span::styled("● ", s.ok()),
            Span::styled(format!("stationd {host} "), s.label()),
        ]),
        Link::Lost { since, .. } if narrow => Line::from(Span::styled(
            format!("✕ {} ", tr!("link-lost-short", since = human_duration(now.saturating_duration_since(*since)))),
            s.error(),
        )),
        Link::Lost { since, .. } => Line::from(Span::styled(
            format!("✕ {} ", tr!("link-lost", since = human_duration(now.saturating_duration_since(*since)))),
            s.error(),
        )),
    }
}

fn on_air<'a>(store: &Store, s: &Styles, max: usize) -> Line<'a> {
    let head = format!(" {} ", tr!("banner-on-air"));
    let room = max.saturating_sub(head.chars().count() + 1);
    let mut v = vec![Span::styled(head.clone(), s.label())];
    // Le flux de l'antenne décrit le morceau (artiste — titre) ; à défaut,
    // le nom de fichier que donne Liquidsoap.
    if let Some(t) = store.onair.as_ref().and_then(|o| o.on_air.as_ref()) {
        let name = match (t.artist.is_empty(), t.title.is_empty()) {
            (false, false) => format!("{} — {}", t.artist, t.title),
            (true, false) => t.title.clone(),
            _ => t.rel_path.rsplit('/').next().unwrap_or(&t.rel_path).to_string(),
        };
        let pl = if t.playlist_ref.is_empty() { String::new() } else { format!("  ({})", t.playlist_ref) };
        let name_room = room.saturating_sub(pl.chars().count().min(room / 3));
        let name = fit::ellipsize(&name, name_room);
        let pl = fit::ellipsize(&pl, room.saturating_sub(name.chars().count()));
        v.push(Span::raw(name));
        v.push(Span::styled(pl, s.muted()));
        return Line::from(v);
    }
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
        Some(ls) if ls.on_air_kind == "fallback" => v.push(Span::styled(tr!("kind-fallback"), s.error())),
        Some(ls) if ls.on_air_kind == "halted" => v.push(Span::styled(tr!("kind-halted"), s.calm())),
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
            v.push(Span::styled(tr!("banner-tz-unknown", tz = name.clone()), s.error()));
        }
        _ => v.push(Span::styled(tr!("banner-tz-none"), s.muted())),
    }
    v.push(Span::raw(if narrow { " │ " } else { SEP }));
    match store.uptime(now) {
        Some(u) if narrow => v.push(Span::styled(format!("{} ", tr!("banner-uptime-short", t = human_duration(u))), s.label())),
        Some(u) => v.push(Span::styled(format!("{} ", tr!("banner-uptime", t = human_duration(u))), s.label())),
        None => {
            let t = "—".to_string();
            let txt = if narrow { tr!("banner-uptime-short", t = t) } else { tr!("banner-uptime", t = t) };
            v.push(Span::styled(format!("{txt} "), s.muted()));
        }
    }
    Line::from(v)
}
