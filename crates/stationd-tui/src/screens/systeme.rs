//! Système (`7`) — la santé de la station (dossier §5.7).
//!
//! Trois sections (`Tab`) :
//! - **État** : stationd, Liquidsoap, Icecast (mounts), live, scan — lus par
//!   le bandeau toutes les 2 s et par le flux du scan ;
//! - **Journal** : le journal de stationd (`EventService.Watch`, suivi en
//!   continu par l'application) — défile seul, se met en pause dès qu'on
//!   remonte, filtres de niveau et de composant, recherche ;
//! - **Statistiques** : `StatsService.Plays` par playlist / règle / origine /
//!   média…, sur 30 min, 24 h, 7 j ou 30 j, en barres + tableau.
//!
//! Aucune action ici : tout se lit. Les faits du journal sont des codes
//! traduits ici (D12) ; seules les lignes de journal de stationd (`LOG`)
//! gardent leur texte.

use anyhow::Error;
use rat_salsa::{Control, SalsaContext};
use rat_theme4::WidgetStyle;
use rat_widget::event::{HandleEvent, Regular};
use rat_widget::text::HasScreenCursor;
use rat_widget::text_input::{TextInput, TextInputState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use ratatui_widgets::table::{Cell, Row, Table};
use stationd_proto::events::event::{Code, Component, Level};
use stationd_proto::events::Event as JEvent;
use stationd_proto::library::scan_status::Phase;
use stationd_proto::stats::{PlaysRequest, PlaysResponse, plays_request::By};

use crate::app::{AppEvent, Global};
use crate::rpc::{self, Read};
use crate::screen::{KeyHelp, Screen};
use crate::store::{Store, human_duration, local_day_hm};
use crate::style::Styles;
use crate::{fit, k, tr};

/// Réponses propres à l'écran.
#[derive(Debug)]
pub enum SysEvent {
    /// Statistiques (n° de requête).
    Plays(u64, Read<PlaysResponse>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    State,
    Journal,
    Stats,
}

const SECTIONS: [Section; 3] = [Section::State, Section::Journal, Section::Stats];

impl Section {
    fn title(self) -> String {
        match self {
            Section::State => tr!("sys-state"),
            Section::Journal => tr!("sys-journal"),
            Section::Stats => tr!("sys-stats"),
        }
    }
}

const STATE_KEYS: &[KeyHelp] = &[(k!("key-tab"), k!("help-section"))];
const JOURNAL_KEYS: &[KeyHelp] = &[
    (k!("key-tab"), k!("help-section")),
    (k!("key-up-down"), k!("help-journal-scroll")),
    (k!("key-end"), k!("help-journal-follow")),
    (k!("key-l"), k!("help-journal-level")),
    (k!("key-c"), k!("help-journal-component")),
    (k!("key-slash"), k!("help-journal-search")),
];
const STATS_KEYS: &[KeyHelp] = &[
    (k!("key-tab"), k!("help-section")),
    (k!("key-w"), k!("help-stats-window")),
    (k!("key-b"), k!("help-stats-by")),
    (k!("key-up-down"), k!("help-select")),
    (k!("key-r"), k!("help-reload")),
];
const SEARCH_KEYS: &[KeyHelp] = &[(k!("key-enter"), k!("help-search-apply")), (k!("key-esc"), k!("help-search-clear"))];

/// Fenêtres des statistiques.
const WINDOWS: [&str; 4] = ["30m", "24h", "7d", "30d"];
/// Regroupements des statistiques.
const BYS: [By; 6] = [By::Playlist, By::Leaf, By::Rule, By::Origin, By::Media, By::Artist];

pub struct Systeme {
    section: usize,
    /// Journal : lignes remontées depuis la fin (0 = suit le flux).
    offset: usize,
    /// Nombre d'événements au moment de la pause (pour dire combien sont arrivés).
    paused_at: usize,
    /// Niveau minimal montré : 1 info, 2 avertissement, 3 erreur.
    min_level: i32,
    /// Composant montré (`None` = tous).
    component: Option<Component>,
    searching: bool,
    search: TextInputState,
    /// Statistiques.
    window: usize,
    by: usize,
    plays: Option<Read<PlaysResponse>>,
    plays_req: u64,
    selected: usize,
}

impl Default for Systeme {
    fn default() -> Self {
        Self {
            section: 0,
            offset: 0,
            paused_at: 0,
            min_level: 1,
            component: None,
            searching: false,
            search: TextInputState::new(),
            window: 1,
            by: 0,
            plays: None,
            plays_req: 0,
            selected: 0,
        }
    }
}

const COMPONENTS: [Component; 8] = [
    Component::Station,
    Component::Broadcast,
    Component::Grid,
    Component::Library,
    Component::Plugin,
    Component::Live,
    Component::Liquidsoap,
    Component::Icecast,
];

pub fn component_label(c: Component) -> String {
    match c {
        Component::Station => tr!("evc-station"),
        Component::Broadcast => tr!("evc-broadcast"),
        Component::Grid => tr!("evc-grid"),
        Component::Library => tr!("evc-library"),
        Component::Plugin => tr!("evc-plugin"),
        Component::Live => tr!("evc-live"),
        Component::Liquidsoap => tr!("evc-liquidsoap"),
        Component::Icecast => tr!("evc-icecast"),
        Component::Unspecified => tr!("evc-unknown"),
    }
}

fn by_label(b: By) -> String {
    match b {
        By::Playlist => tr!("stats-by-playlist"),
        By::Leaf => tr!("stats-by-leaf"),
        By::Rule => tr!("stats-by-rule"),
        By::Origin => tr!("stats-by-origin"),
        By::Media => tr!("stats-by-media"),
        By::Artist => tr!("stats-by-artist"),
    }
}

fn window_label(w: &str) -> String {
    match w {
        "30m" => tr!("stats-window-30m"),
        "24h" => tr!("stats-window-24h"),
        "7d" => tr!("stats-window-7d"),
        _ => tr!("stats-window-30d"),
    }
}

/// Un fait du journal, traduit (D12). Les paramètres manquants sont vides.
pub fn event_text(e: &JEvent) -> String {
    let p = |n: &str| e.params.iter().find(|x| x.name == n).map(|x| x.value.clone()).unwrap_or_default();
    match Code::try_from(e.code).unwrap_or(Code::Unspecified) {
        Code::Log => {
            let fields: Vec<String> = e
                .params
                .iter()
                .filter(|x| x.name != "message" && x.name != "target")
                .map(|x| format!("{}={}", x.name, x.value))
                .collect();
            if fields.is_empty() { p("message") } else { format!("{}  {}", p("message"), fields.join(" ")) }
        }
        Code::Started => tr!("ev-started", version = p("version"), station = p("station")),
        Code::Stopping => match p("by").as_str() {
            "request" => tr!("ev-stopping-request"),
            _ => tr!("ev-stopping-signal", signal = p("by")),
        },
        Code::BroadcastState => tr!(
            "ev-broadcast-state",
            from = state_word(&p("from")),
            to = state_word(&p("to")),
            by = p("by")
        ),
        Code::Listeners => tr!("ev-listeners", count = p("count")),
        Code::AudienceUnknown => tr!("ev-audience-unknown"),
        Code::TrackChosen => {
            let media = p("media");
            if media.is_empty() {
                tr!("ev-track-fallback", origin = p("origin"))
            } else {
                tr!("ev-track-chosen", media = media, playlist = p("playlist"), origin = p("origin"))
            }
        }
        Code::OverridePushed => {
            let what = if p("media").is_empty() { p("playlist") } else { p("media") };
            tr!("ev-override-pushed", what = what, mode = p("mode"), by = p("by"))
        }
        Code::OverrideDropped => {
            let what = if p("media").is_empty() { p("playlist") } else { p("media") };
            tr!("ev-override-dropped", what = what, by = p("by"), reason = p("reason"))
        }
        Code::GridApplied => tr!("ev-grid-applied", grid = p("grid"), rules = p("rules")),
        Code::GridRefused => {
            if p("problems").is_empty() {
                tr!("ev-grid-unreadable", grid = p("grid"), error = p("error"))
            } else {
                tr!("ev-grid-refused", grid = p("grid"), problems = p("problems"))
            }
        }
        Code::GridIncident => match p("kind").as_str() {
            "hard_not_cut" => tr!("ev-incident-hard-not-cut", rule = p("rule"), playlist = p("playlist")),
            _ => tr!("ev-incident-source-empty", rule = p("rule"), playlist = p("playlist")),
        },
        Code::LiveStarted => tr!("ev-live-started", dj = p("dj"), rule = p("rule")),
        Code::LiveEnded => tr!("ev-live-ended", dj = p("dj"), reason = p("reason")),
        Code::ScanStarted => tr!("ev-scan-started"),
        Code::ScanFinished => tr!(
            "ev-scan-finished",
            found = p("found"),
            skipped = p("skipped"),
            vanished = p("vanished")
        ),
        Code::ScanFailed => tr!("ev-scan-failed", error = p("error")),
        Code::TagsWritten => tr!("ev-tags-written", media = p("media")),
        Code::TagRenamed => tr!(
            "ev-tag-renamed",
            origin = origin_label(&p("origin")),
            from = p("from"),
            to = p("to"),
            files = p("files"),
            failed = p("failed")
        ),
        Code::PluginFailed => tr!("ev-plugin-failed", plugin = p("plugin"), phase = p("phase"), reason = p("reason")),
        Code::PluginQuarantined => tr!("ev-plugin-quarantined", plugin = p("plugin"), failures = p("failures")),
        Code::PluginState => tr!("ev-plugin-state", plugin = p("plugin"), state = p("state")),
        Code::BpmAnalyzed => {
            let why: Vec<String> = e
                .params
                .iter()
                .filter_map(|x| x.name.strip_prefix("failed_").map(|k| (k, &x.value)))
                .map(|(k, n)| format!("{} {n}", bpm_fail_label(k)))
                .collect();
            if why.is_empty() {
                tr!("ev-bpm-analyzed", estimated = p("estimated"))
            } else {
                tr!("ev-bpm-analyzed-failed", estimated = p("estimated"), failed = p("failed"), why = why.join(", "))
            }
        }
        Code::Unspecified => tr!("ev-unknown"),
    }
}

/// Pourquoi un BPM n'a pas été estimé (clé `failed_<kind>` du journal).
fn bpm_fail_label(kind: &str) -> String {
    match kind {
        "decode" => tr!("bpm-fail-decode"),
        "too_short" => tr!("bpm-fail-too-short"),
        "no_rhythm" => tr!("bpm-fail-no-rhythm"),
        "weak" => tr!("bpm-fail-weak"),
        "competing" => tr!("bpm-fail-competing"),
        "disagree" => tr!("bpm-fail-disagree"),
        other => other.to_string(),
    }
}

/// Le nom d'une origine de valeurs : genre du fichier, ou la source.
pub fn origin_label(origin: &str) -> String {
    if origin.is_empty() { tr!("tags-origin-file") } else { origin.to_string() }
}

fn state_word(s: &str) -> String {
    match s {
        "running" => tr!("state-running"),
        "paused" => tr!("state-paused"),
        "draining" => tr!("state-draining"),
        "sleeping" => tr!("state-sleeping"),
        other => other.to_string(),
    }
}

fn level_mark(level: i32, s: &Styles) -> (&'static str, Style) {
    match Level::try_from(level).unwrap_or(Level::Unspecified) {
        Level::Error => ("✕", s.error()),
        Level::Warn => ("!", s.warn()),
        _ => ("·", s.muted()),
    }
}

/// Le texte d'un scan en cours, ou `None` s'il n'y en a pas.
pub fn scan_progress(store: &Store) -> Option<String> {
    let s = store.scan.as_ref()?;
    Some(match Phase::try_from(s.phase).unwrap_or(Phase::Unspecified) {
        Phase::Idle | Phase::Unspecified => return None,
        Phase::Listing => tr!("scan-phase-listing"),
        Phase::Reading => tr!("scan-phase-reading", done = s.done, total = s.total),
        Phase::Analyzing => tr!("scan-phase-analyzing", done = s.done, total = s.total),
        Phase::Plugins => tr!("scan-phase-plugins"),
        Phase::Writing => tr!("scan-phase-writing"),
        Phase::Indexing => tr!("scan-phase-indexing"),
    })
}

/// Jauge texte `[█████░░░░░] 52 %`.
pub fn gauge(done: u64, total: u64, width: usize) -> String {
    let w = width.max(4);
    let done = done.min(total);
    let filled = (done * w as u64).checked_div(total).unwrap_or(0) as usize;
    let pct = (done * 100).checked_div(total).unwrap_or(0);
    format!("[{}{}] {pct} %", "█".repeat(filled), "░".repeat(w - filled))
}

fn titled<'a>(title: String, s: &Styles) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border())
        .title(Span::styled(format!(" {title} "), s.title()))
}

fn field<'a>(label: String, value: Span<'a>, s: &Styles) -> Line<'a> {
    Line::from(vec![Span::styled(format!("  {label:<18} "), s.label()), value])
}

impl Systeme {
    fn current(&self) -> Section {
        SECTIONS[self.section]
    }

    fn load_plays(&mut self, ctx: &mut Global) {
        self.plays_req += 1;
        let (id, channel) = (self.plays_req, ctx.channel.clone());
        let req = PlaysRequest { since: WINDOWS[self.window].into(), by: BYS[self.by] as i32, limit: 100, key: String::new() };
        ctx.spawn_async(async move {
            let r = rpc::plays(channel, req).await;
            Ok(Control::Event(AppEvent::System(Box::new(SysEvent::Plays(id, r)))))
        });
    }

    /// Les événements montrés, dans l'ordre, filtres appliqués.
    fn visible<'a>(&self, store: &'a Store) -> Vec<&'a JEvent> {
        let needle = self.search.text().trim().to_lowercase();
        store
            .journal
            .iter()
            .filter(|e| e.level >= self.min_level)
            .filter(|e| self.component.is_none_or(|c| e.component == c as i32))
            .filter(|e| {
                needle.is_empty()
                    || event_text(e).to_lowercase().contains(&needle)
                    || component_label(Component::try_from(e.component).unwrap_or(Component::Unspecified))
                        .to_lowercase()
                        .contains(&needle)
            })
            .collect()
    }

    fn render_sections(&self, area: Rect, buf: &mut Buffer, s: &Styles) {
        let lines: Vec<Line> = SECTIONS
            .iter()
            .enumerate()
            .map(|(i, sec)| {
                let st = if i == self.section { s.tab_active() } else { s.base() };
                Line::styled(format!(" {} ", sec.title()), st)
            })
            .collect();
        let block = Block::bordered().border_type(BorderType::Rounded).border_style(s.border());
        let inner = block.inner(area);
        block.render(area, buf);
        Paragraph::new(lines).render(inner, buf);
    }

    fn render_state(&self, area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
        let now = std::time::Instant::now();
        let tz = store.tz.as_ref();
        let when = |e: i64| if e == 0 { "—".to_string() } else { local_day_hm(tz, e).unwrap_or_else(|| "—".into()) };
        let mut lines: Vec<Line> = Vec::new();

        // stationd
        lines.push(Line::styled(tr!("sys-stationd"), s.title()));
        match &store.status.value {
            Some(st) => {
                lines.push(field(tr!("sys-station"), Span::raw(st.station_name.clone()), s));
                let version = store
                    .journal
                    .iter()
                    .rev()
                    .find(|e| e.code == Code::Started as i32)
                    .and_then(|e| e.params.iter().find(|p| p.name == "version").map(|p| p.value.clone()))
                    .unwrap_or_else(|| "—".into());
                lines.push(field(tr!("sys-version"), Span::raw(version), s));
                let up = store.uptime(now).map(human_duration).unwrap_or_else(|| "—".into());
                lines.push(field(tr!("sys-uptime"), Span::raw(up), s));
                lines.push(field(tr!("sys-pid"), Span::raw(st.pid.to_string()), s));
                lines.push(field(tr!("sys-timezone"), Span::raw(st.timezone.clone()), s));
            }
            None => lines.push(Line::styled(
                format!("  {}", store.status.error.clone().unwrap_or_else(|| tr!("control-not-read"))),
                s.warn(),
            )),
        }
        let journal = match &store.journal_link {
            Ok(()) => Span::styled(tr!("sys-journal-open", n = store.journal.len()), s.ok()),
            Err(e) => Span::styled(e.clone(), s.warn()),
        };
        lines.push(field(tr!("sys-journal-field"), journal, s));
        lines.push(Line::default());

        // Liquidsoap
        lines.push(Line::styled(tr!("sys-liquidsoap"), s.title()));
        match &store.liquidsoap.value {
            Some(ls) if !ls.enabled => lines.push(Line::styled(format!("  {}", tr!("sys-not-configured")), s.muted())),
            Some(ls) => {
                lines.push(field(tr!("sys-ls-air"), Span::raw(ls.air_state.clone()), s));
                lines.push(field(tr!("sys-ls-on-air"), Span::raw(format!("{} {}", ls.on_air_kind, ls.on_air_media)), s));
                lines.push(field(tr!("sys-ls-last-pull"), Span::raw(format!("{} ({})", when(ls.last_pull_at), ls.pulls)), s));
                lines.push(field(tr!("sys-ls-started"), Span::raw(ls.tracks_started.to_string()), s));
                if ls.control_error.is_empty() {
                    lines.push(field(tr!("sys-ls-control"), Span::styled(tr!("sys-ok"), s.ok()), s));
                } else {
                    lines.push(field(
                        tr!("sys-ls-control"),
                        Span::styled(format!("{} ({})", ls.control_error, when(ls.control_error_at)), s.error()),
                        s,
                    ));
                }
            }
            None => lines.push(Line::styled(format!("  {}", tr!("control-not-read")), s.muted())),
        }
        lines.push(Line::default());

        // Icecast
        lines.push(Line::styled(tr!("sys-icecast"), s.title()));
        match &store.icecast.value {
            Some(ic) if !ic.enabled => lines.push(Line::styled(format!("  {}", tr!("sys-not-configured")), s.muted())),
            Some(ic) => {
                let id = if ic.server_id.is_empty() { ic.server.clone() } else { format!("{} ({})", ic.server, ic.server_id) };
                lines.push(field(tr!("sys-ic-server"), Span::raw(id), s));
                lines.push(field(tr!("sys-ic-last-read"), Span::raw(when(ic.last_ok_at)), s));
                let audience = ic.audience.map(|a| a.to_string()).unwrap_or_else(|| "—".into());
                lines.push(field(tr!("sys-ic-audience"), Span::raw(audience), s));
                if !ic.problem.is_empty() {
                    lines.push(field(tr!("sys-ic-problem"), Span::styled(ic.problem.clone(), s.error()), s));
                }
                for m in &ic.mounts {
                    let (mark, st) = if !m.present {
                        (tr!("sys-mount-absent"), s.error())
                    } else if !m.connected {
                        (tr!("sys-mount-no-source"), s.warn())
                    } else {
                        (tr!("sys-mount-ok"), s.ok())
                    };
                    let rate = match m.read_kbps {
                        Some(r) => tr!("sys-mount-rate", declared = m.bitrate.clone(), real = format!("{r:.0}")),
                        None => tr!("sys-mount-rate-declared", declared = m.bitrate.clone()),
                    };
                    let listeners = m.listeners.map(|l| l.to_string()).unwrap_or_else(|| "—".into());
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {:<18} ", m.mount), s.label()),
                        Span::styled(mark, st),
                        Span::styled(format!("  {rate}  ♪ {listeners}"), s.muted()),
                    ]));
                }
            }
            None => lines.push(Line::styled(
                format!("  {}", store.icecast.error.clone().unwrap_or_else(|| tr!("control-not-read"))),
                s.muted(),
            )),
        }
        lines.push(Line::default());

        // Live
        lines.push(Line::styled(tr!("sys-live"), s.title()));
        match &store.live.value {
            Some(l) if !l.enabled => lines.push(Line::styled(format!("  {}", tr!("sys-not-configured")), s.muted())),
            Some(l) => {
                let on = l.on_air.as_ref().map(|o| o.dj.clone()).unwrap_or_else(|| tr!("sys-live-nobody"));
                lines.push(field(tr!("sys-live-on-air"), Span::raw(on), s));
                lines.push(field(tr!("sys-live-harbor"), Span::raw(format!("{} {}", l.harbor_port, l.mount)), s));
                if !l.last_refusal_dj.is_empty() {
                    lines.push(field(
                        tr!("sys-live-refused"),
                        Span::styled(format!("{} — {} ({})", l.last_refusal_dj, l.last_refusal_reason, when(l.last_refusal_at)), s.warn()),
                        s,
                    ));
                }
            }
            None => lines.push(Line::styled(format!("  {}", tr!("control-not-read")), s.muted())),
        }
        lines.push(Line::default());

        // Scan
        lines.push(Line::styled(tr!("sys-scan"), s.title()));
        match (&store.scan, scan_progress(store)) {
            (Some(sc), Some(txt)) => {
                lines.push(field(tr!("sys-scan-running"), Span::styled(txt, s.accent()), s));
                if sc.phase == Phase::Reading as i32 || sc.phase == Phase::Analyzing as i32 {
                    lines.push(Line::raw(format!("  {}", gauge(sc.done, sc.total, 30))));
                }
            }
            (Some(sc), None) => match &sc.last {
                Some(e) if e.ok => lines.push(field(
                    tr!("sys-scan-last"),
                    Span::raw(tr!("sys-scan-last-ok", when = when(e.finished_at), found = e.found, skipped = e.skipped)),
                    s,
                )),
                Some(e) => lines.push(field(
                    tr!("sys-scan-last"),
                    Span::styled(tr!("sys-scan-last-failed", when = when(e.finished_at), error = e.error.clone()), s.error()),
                    s,
                )),
                None => lines.push(Line::styled(format!("  {}", tr!("sys-scan-none")), s.muted())),
            },
            (None, _) => lines.push(Line::styled(format!("  {}", tr!("control-not-read")), s.muted())),
        }
        Paragraph::new(lines).wrap(Wrap { trim: false }).render(area, buf);
    }

    /// Rend le journal ; la position du curseur de la recherche en saisie.
    fn render_journal(&mut self, area: Rect, buf: &mut Buffer, ctx: &Global, s: &Styles) -> Option<(u16, u16)> {
        let [filters_a, list_a] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        // Filtres et état du défilement.
        let level = match self.min_level {
            3 => tr!("journal-level-error"),
            2 => tr!("journal-level-warn"),
            _ => tr!("journal-level-all"),
        };
        let comp = self.component.map(component_label).unwrap_or_else(|| tr!("journal-component-all"));
        let store = &ctx.store;
        let visible = self.visible(store);
        let mut head = vec![
            Span::styled(format!("{} ", tr!("journal-level")), s.label()),
            Span::raw(format!("{level}   ")),
            Span::styled(format!("{} ", tr!("journal-component")), s.label()),
            Span::raw(format!("{comp}   ")),
        ];
        if self.offset > 0 {
            let new = store.journal.len().saturating_sub(self.paused_at);
            head.push(Span::styled(tr!("journal-paused", n = new), s.warn()));
        } else {
            head.push(Span::styled(tr!("journal-following"), s.muted()));
        }
        if let Err(e) = &store.journal_link {
            head.push(Span::styled(format!("   {e}"), s.warn()));
        }
        Paragraph::new(Line::from(head)).render(filters_a, buf);

        let [search_a, rows_a] = if self.searching || !self.search.text().is_empty() {
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(list_a)
        } else {
            [Rect { height: 0, ..list_a }, list_a]
        };
        if search_a.height > 0 {
            let [label_a, input_a] =
                Layout::horizontal([Constraint::Length(Span::raw(tr!("journal-search")).width() as u16 + 2), Constraint::Fill(1)])
                    .areas(search_a);
            Paragraph::new(Span::styled(format!("{} ", tr!("journal-search")), s.label())).render(label_a, buf);
            let style: rat_widget::text::TextStyle = ctx.theme.style(WidgetStyle::TEXT);
            TextInput::new().styles(style).render(input_a, buf, &mut self.search);
        }
        let cursor = if self.searching { self.search.screen_cursor() } else { None };

        let rows = rows_a.height as usize;
        if visible.is_empty() {
            Paragraph::new(Line::styled(tr!("journal-empty"), s.muted())).render(rows_a, buf);
            return cursor;
        }
        self.offset = self.offset.min(visible.len().saturating_sub(rows));
        let end = visible.len() - self.offset;
        let start = end.saturating_sub(rows);
        let tz = store.tz.as_ref();
        let comp_w = COMPONENTS.iter().map(|c| Span::raw(component_label(*c)).width()).max().unwrap_or(10);
        let text_w = (rows_a.width as usize).saturating_sub(comp_w + 22);
        let lines: Vec<Line> = visible[start..end]
            .iter()
            .map(|e| {
                let (mark, st) = level_mark(e.level, s);
                let when = jiff::Timestamp::from_millisecond(e.at_ms)
                    .ok()
                    .map(|t| match tz {
                        Some(tz) => t.to_zoned(tz.clone()).strftime("%d/%m %H:%M:%S").to_string(),
                        None => t.strftime("%d/%m %H:%M:%SZ").to_string(),
                    })
                    .unwrap_or_default();
                let comp = component_label(Component::try_from(e.component).unwrap_or(Component::Unspecified));
                let text_style = if e.level >= Level::Warn as i32 { st } else { s.base() };
                Line::from(vec![
                    Span::styled(format!("{when} "), s.muted()),
                    Span::styled(format!("{mark} "), st),
                    Span::styled(format!("{comp:<comp_w$} "), s.label()),
                    Span::styled(fit::ellipsize(&event_text(e), text_w), text_style),
                ])
            })
            .collect();
        Paragraph::new(lines).render(rows_a, buf);
        cursor
    }

    fn render_stats(&mut self, area: Rect, buf: &mut Buffer, store: &Store, s: &Styles) {
        let [head_a, body_a] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        let head = Line::from(vec![
            Span::styled(format!("{} ", tr!("stats-window")), s.label()),
            Span::raw(format!("{}   ", window_label(WINDOWS[self.window]))),
            Span::styled(format!("{} ", tr!("stats-by")), s.label()),
            Span::raw(by_label(BYS[self.by])),
        ]);
        Paragraph::new(head).render(head_a, buf);
        let r = match &self.plays {
            None => {
                Paragraph::new(Line::styled(tr!("media-loading"), s.muted())).render(body_a, buf);
                return;
            }
            Some(Err(e)) => {
                Paragraph::new(Line::styled(e.clone(), s.error())).wrap(Wrap { trim: false }).render(body_a, buf);
                return;
            }
            Some(Ok(r)) => r,
        };
        if r.rows.is_empty() {
            Paragraph::new(Line::styled(tr!("stats-none"), s.muted())).render(body_a, buf);
            return;
        }
        self.selected = self.selected.min(r.rows.len() - 1);
        let [table_a, total_a] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(body_a);
        let max = r.rows.iter().map(|x| x.aired.max(x.picked)).max().unwrap_or(1).max(1);
        let bar_w: usize = 20;
        let rows_h = table_a.height.saturating_sub(1) as usize;
        let start = self.selected.saturating_sub(rows_h.saturating_sub(1));
        let tz = store.tz.as_ref();
        let rows: Vec<Row> = r
            .rows
            .iter()
            .enumerate()
            .skip(start)
            .take(rows_h)
            .map(|(i, x)| {
                let key = if x.key.is_empty() { tr!("stats-unknown") } else { x.key.clone() };
                let n = ((x.aired * bar_w as u64) / max) as usize;
                let st = if i == self.selected { s.tab_active() } else { s.base() };
                Row::new(vec![
                    Cell::from(Span::styled(key, st)),
                    Cell::from(Span::styled("█".repeat(n.max(usize::from(x.aired > 0))), s.accent())),
                    Cell::from(Span::raw(x.aired.to_string())),
                    Cell::from(Span::styled(x.picked.to_string(), s.muted())),
                    Cell::from(Span::styled(local_day_hm(tz, x.last_at).unwrap_or_default(), s.muted())),
                ])
            })
            .collect();
        let header = Row::new(vec![
            Cell::from(Span::styled(by_label(BYS[self.by]), s.label())),
            Cell::from(""),
            Cell::from(Span::styled(tr!("stats-aired"), s.label())),
            Cell::from(Span::styled(tr!("stats-picked"), s.label())),
            Cell::from(Span::styled(tr!("stats-last"), s.label())),
        ]);
        let table = Table::new(
            rows,
            [
                Constraint::Fill(1),
                Constraint::Length(bar_w as u16),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Length(12),
            ],
        )
        .header(header)
        .column_spacing(1);
        Widget::render(table, table_a, buf);
        Paragraph::new(Line::styled(
            tr!("stats-total", aired = r.total_aired, picked = r.total_picked),
            s.muted(),
        ))
        .render(total_a, buf);
    }
}

impl Screen for Systeme {
    fn title(&self) -> String {
        tr!("screen-system")
    }

    fn enter(&mut self, ctx: &mut Global) -> Result<(), Error> {
        if self.current() == Section::Stats {
            self.load_plays(ctx);
        }
        Ok(())
    }

    fn reconnected(&mut self, ctx: &mut Global) -> Result<(), Error> {
        self.enter(ctx)
    }

    fn captures_text(&self) -> bool {
        self.searching
    }

    fn help(&self) -> &'static [KeyHelp] {
        if self.searching {
            return SEARCH_KEYS;
        }
        match self.current() {
            Section::State => STATE_KEYS,
            Section::Journal => JOURNAL_KEYS,
            Section::Stats => STATS_KEYS,
        }
    }

    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        if let AppEvent::System(ev) = event {
            match &**ev {
                SysEvent::Plays(id, r) if *id == self.plays_req => {
                    self.plays = Some(r.clone());
                    return Ok(Control::Changed);
                }
                SysEvent::Plays(..) => return Ok(Control::Continue),
            }
        }
        let AppEvent::Event(e) = event else { return Ok(Control::Continue) };
        if self.searching {
            if let Event::Key(k) = e
                && k.kind == KeyEventKind::Press
            {
                match k.code {
                    KeyCode::Enter => {
                        self.searching = false;
                        self.search.focus.set(false);
                        self.offset = 0;
                        return Ok(Control::Changed);
                    }
                    KeyCode::Esc => {
                        self.searching = false;
                        self.search.focus.set(false);
                        self.search.set_text("");
                        self.offset = 0;
                        return Ok(Control::Changed);
                    }
                    _ => {}
                }
            }
            self.search.handle(e, Regular);
            self.offset = 0;
            return Ok(Control::Changed);
        }
        let Event::Key(k) = e else { return Ok(Control::Continue) };
        if k.kind != KeyEventKind::Press {
            return Ok(Control::Continue);
        }
        match (self.current(), k.code) {
            (_, KeyCode::Tab) => {
                self.section = (self.section + 1) % SECTIONS.len();
                self.enter(ctx)?;
            }
            (_, KeyCode::BackTab) => {
                self.section = (self.section + SECTIONS.len() - 1) % SECTIONS.len();
                self.enter(ctx)?;
            }
            (Section::Journal, KeyCode::Up) => self.scroll_up(ctx, 1),
            (Section::Journal, KeyCode::PageUp) => self.scroll_up(ctx, 10),
            (Section::Journal, KeyCode::Down) => self.offset = self.offset.saturating_sub(1),
            (Section::Journal, KeyCode::PageDown) => self.offset = self.offset.saturating_sub(10),
            (Section::Journal, KeyCode::End) => self.offset = 0,
            (Section::Journal, KeyCode::Home) => self.scroll_up(ctx, usize::MAX / 2),
            (Section::Journal, KeyCode::Char('l')) => {
                self.min_level = if self.min_level >= 3 { 1 } else { self.min_level + 1 };
                self.offset = 0;
            }
            (Section::Journal, KeyCode::Char('c')) => {
                self.component = match self.component {
                    None => Some(COMPONENTS[0]),
                    Some(c) => COMPONENTS.iter().position(|x| *x == c).and_then(|i| COMPONENTS.get(i + 1)).copied(),
                };
                self.offset = 0;
            }
            (Section::Journal, KeyCode::Char('/')) => {
                self.searching = true;
                self.search.focus.set(true);
            }
            (Section::Journal, KeyCode::Esc) if !self.search.text().is_empty() => {
                self.search.set_text("");
                self.offset = 0;
            }
            (Section::Stats, KeyCode::Char('w')) => {
                self.window = (self.window + 1) % WINDOWS.len();
                self.load_plays(ctx);
            }
            (Section::Stats, KeyCode::Char('b')) => {
                self.by = (self.by + 1) % BYS.len();
                self.selected = 0;
                self.load_plays(ctx);
            }
            (Section::Stats, KeyCode::Char('r')) => self.load_plays(ctx),
            (Section::Stats, KeyCode::Up) => self.selected = self.selected.saturating_sub(1),
            (Section::Stats, KeyCode::Down) => self.selected += 1,
            _ => return Ok(Control::Continue),
        }
        Ok(Control::Changed)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        Block::new().style(s.base()).render(area, buf);
        let side_w = SECTIONS.iter().map(|x| Span::raw(x.title()).width()).max().unwrap_or(12) as u16 + 6;
        let [side, main] = Layout::horizontal([Constraint::Length(side_w), Constraint::Fill(1)]).areas(area);
        self.render_sections(side, buf, &s);
        let sec = self.current();
        let block = titled(sec.title(), &s);
        let inner = block.inner(main);
        block.render(main, buf);
        let body = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
        let cursor = match sec {
            Section::State => {
                self.render_state(body, buf, &ctx.store, &s);
                None
            }
            Section::Journal => self.render_journal(body, buf, ctx, &s),
            Section::Stats => {
                self.render_stats(body, buf, &ctx.store, &s);
                None
            }
        };
        if cursor.is_some() {
            ctx.set_screen_cursor(cursor);
        }
        Ok(())
    }
}

impl Systeme {
    fn scroll_up(&mut self, ctx: &Global, n: usize) {
        if self.offset == 0 {
            self.paused_at = ctx.store.journal.len();
        }
        self.offset = self.offset.saturating_add(n).min(self.visible(&ctx.store).len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stationd_proto::events::Param;

    fn ev(code: Code, params: &[(&str, &str)]) -> JEvent {
        JEvent {
            seq: 1,
            at_ms: 0,
            level: Level::Info as i32,
            component: Component::Grid as i32,
            code: code as i32,
            params: params.iter().map(|(n, v)| Param { name: n.to_string(), value: v.to_string() }).collect(),
        }
    }

    #[test]
    fn every_code_reads_as_text() {
        crate::i18n::init(Some("fr"));
        let e = ev(Code::GridApplied, &[("grid", "ete.toml"), ("rules", "8")]);
        let t = event_text(&e);
        assert!(t.contains("ete.toml") && t.contains('8'), "{t}");
        let log = ev(Code::Log, &[("message", "disk slow"), ("target", "x"), ("path", "/a")]);
        assert_eq!(event_text(&log), "disk slow  path=/a");
        // Every fact code has its text (a LOG line brings its own).
        for c in 2..=22 {
            let code = Code::try_from(c).unwrap();
            assert!(!event_text(&ev(code, &[])).is_empty(), "{code:?}");
        }
    }

    #[test]
    fn the_gauge_fills_in_proportion() {
        assert_eq!(gauge(0, 0, 4), "[░░░░] 0 %");
        assert_eq!(gauge(5, 10, 4), "[██░░] 50 %");
        assert_eq!(gauge(12, 10, 4), "[████] 100 %");
    }
}
