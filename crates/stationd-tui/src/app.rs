//! Application : état global, boucle d'événements rat-salsa, registre d'écrans,
//! mise en page commune (bandeau · onglets · zone de travail · statut ·
//! raccourcis) et touches globales.

use std::time::Duration;

use anyhow::Error;
use rat_focus::FocusBuilder;
use rat_salsa::event::RenderedEvent;
use rat_salsa::timer::{TimeOut, TimerDef, TimerHandle};
use rat_salsa::{Control, SalsaAppContext, SalsaContext};
use rat_theme4::theme::SalsaTheme;
use rat_theme4::{WidgetStyle, create_salsa_theme};
use rat_widget::statusline::{StatusLine, StatusLineState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::clear::Clear;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use tonic::transport::Channel;

use crate::action::{Action, Done};
use crate::dialog::{Modal, Outcome};
use crate::rpc::{self, BannerRead};
use crate::screen::{Availability, KeyHelp, Screen};
use crate::store::Store;
use crate::style::Styles;
use crate::{Args, banner, fit, k, screens, tr};

/// Taille minimale utilisable (dossier §2).
const MIN_W: u16 = 80;
const MIN_H: u16 = 24;

/// Touches communes à tous les écrans (hors saisie).
const GLOBAL_KEYS: &[KeyHelp] = &[
    (k!("key-digits"), k!("help-switch-screen")),
    (k!("key-cycle-tabs"), k!("help-cycle-tabs")),
    (k!("key-help"), k!("help-screen-help")),
    (k!("key-quit"), k!("help-quit")),
    (k!("key-force-quit"), k!("help-force-quit")),
];

/// Données accessibles partout (rendu et événements).
pub struct Global {
    ctx: SalsaAppContext<AppEvent, Error>,
    pub theme: SalsaTheme,
    pub channel: Channel,
    /// Même adresse, sans délai maximal : opérations longues (scan).
    pub long_channel: Channel,
    pub store: Store,
    /// Modale demandée par un écran (ouverte par l'application).
    pending_modal: Option<Modal>,
    /// Action demandée par un écran sans confirmation (lecture, scan…).
    pending_action: Option<Action>,
    /// Un écran demande d'en ouvrir un autre (n° du registre).
    pending_switch: Option<usize>,
    /// Message d'un écran pour la ligne de statut.
    pending_status: Option<String>,
    /// Ce qu'un écran confie à celui qu'il ouvre (médias à ajouter à une
    /// playlist…) ; l'écran ouvert le prend à son entrée.
    pub handoff: Option<Handoff>,
}

/// Travail confié d'un écran à un autre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handoff {
    /// Ouvrir le brouillon de la playlist statique `reference` (ou d'une
    /// nouvelle, `None`) avec ces médias ajoutés — rien n'est enregistré
    /// avant `Ctrl+S`.
    AddFiles { reference: Option<String>, files: Vec<String> },
    /// Montrer la playlist `reference` dans la liste (depuis l'agenda).
    Select { reference: String },
}

impl SalsaContext<AppEvent, Error> for Global {
    fn set_salsa_ctx(&mut self, app_ctx: SalsaAppContext<AppEvent, Error>) {
        self.ctx = app_ctx;
    }

    fn salsa_ctx(&self) -> &SalsaAppContext<AppEvent, Error> {
        &self.ctx
    }
}

impl Global {
    pub fn new(args: &Args, channel: Channel, long_channel: Channel) -> Self {
        Self {
            ctx: Default::default(),
            theme: create_salsa_theme(&args.theme),
            channel,
            long_channel,
            store: Store::new(&args.addr),
            pending_modal: None,
            pending_action: None,
            pending_switch: None,
            pending_status: None,
            handoff: None,
        }
    }

    /// Un écran ouvre une modale (confirmation, formulaire).
    pub fn open(&mut self, modal: Modal) {
        self.pending_modal = Some(modal);
    }

    /// Un écran écrit dans la ligne de statut.
    pub fn set_status(&mut self, msg: String) {
        self.pending_status = Some(msg);
    }

    /// Un écran lance une action sans confirmation.
    pub fn request(&mut self, action: Action) {
        self.pending_action = Some(action);
    }

    /// Un écran en ouvre un autre, en lui confiant `handoff`.
    pub fn switch_to(&mut self, screen: usize, handoff: Option<Handoff>) {
        self.pending_switch = Some(screen);
        self.handoff = handoff;
    }
}

/// Messages de l'application.
#[derive(Debug)]
pub enum AppEvent {
    Timer(TimeOut),
    Event(Event),
    Rendered,
    /// Une lecture du bandeau est arrivée.
    Banner(Box<BannerRead>),
    /// Un instantané de l'antenne ; `true` = premier d'un flux (re)ouvert.
    OnAir(Box<stationd_proto::onair::OnAirSnapshot>, bool),
    /// Le flux de l'antenne est fermé ou n'a pas pu s'ouvrir.
    OnAirLost(String),
    /// Une action est terminée (message traduit, ou erreur).
    ActionDone(Result<Done, String>),
    /// Une page de la recherche de médias (propriétaire, n° de requête, page
    /// ou erreur, `true` = page suivante à ajouter). Le propriétaire
    /// distingue l'écran Médias des sélecteurs qui réutilisent sa recherche.
    Media(u64, u64, Result<stationd_proto::library::SearchMediaResponse, String>, bool),
    /// Recherche de médias : fin du délai après une frappe (propriétaire,
    /// n° de la frappe).
    MediaTyped(u64, u64),
    /// Fiche d'un média (propriétaire, n° de requête).
    MediaCard(u64, u64, Box<crate::rpc::MediaCard>),
    /// Tags des fichiers visés (dans l'ordre) et genres connus, lus pour les
    /// modifier (propriétaire, n° de requête).
    MediaTags(u64, u64, Box<crate::rpc::Read<crate::rpc::TagFormData>>),
    /// Liste des playlists pour un sélecteur (propriétaire, n° de requête).
    PlaylistChoices(u64, u64, Result<Vec<stationd_proto::playlist::PlaylistSummary>, String>),
    /// Écran Playlists : réponses et minuteries (voir `screens::playlists`).
    Playlists(Box<crate::screens::PlEvent>),
    /// Agenda et éditeur de règle : réponses et minuteries.
    Agenda(Box<crate::screens::AgEvent>),
    /// Un événement du journal ; `true` = premier d'un flux (re)ouvert.
    Journal(Box<stationd_proto::events::Event>, bool),
    /// Le flux du journal est fermé ou n'a pas pu s'ouvrir.
    JournalLost(String),
    /// Où en est le scan.
    Scan(Box<stationd_proto::library::ScanStatus>),
    /// Le flux du scan est fermé : son état n'est plus connu.
    ScanLost,
    /// Écrans Tags et Système : réponses (voir `screens::tags`, `screens::systeme`).
    Tags(Box<crate::screens::TagsEvent>),
    System(Box<crate::screens::SysEvent>),
    PluginTable(u64, u64, crate::rpc::Read<stationd_proto::plugin::PluginDbQueryResponse>),
}

impl From<RenderedEvent> for AppEvent {
    fn from(_: RenderedEvent) -> Self {
        Self::Rendered
    }
}

impl From<TimeOut> for AppEvent {
    fn from(t: TimeOut) -> Self {
        Self::Timer(t)
    }
}

impl From<Event> for AppEvent {
    fn from(e: Event) -> Self {
        Self::Event(e)
    }
}

/// État de l'interface.
pub struct Scenery {
    screens: Vec<Box<dyn Screen>>,
    plugin_tabs: Vec<(String, stationd_proto::plugin::PluginTab)>,
    active: usize,
    help_open: bool,
    /// stationd s'arrête à notre demande : la perte de liaison est attendue.
    expect_exit: bool,
    status: StatusLineState,
    /// Tick d'une seconde (horloge, progression).
    clock: Option<TimerHandle>,
    /// Modale ouverte : elle capture toutes les entrées.
    modal: Option<Modal>,
}

impl Scenery {
    pub fn new() -> Self {
        Self {
            screens: screens::registry(),
            plugin_tabs: Vec::new(),
            active: 0,
            help_open: false,
            expect_exit: false,
            status: StatusLineState::default(),
            clock: None,
            modal: None,
        }
    }

    fn sync_plugin_tabs(&mut self, ctx: &mut Global) -> Result<(), Error> {
        let desired = screens::declared_tabs(ctx.store.plugins.value.as_deref().unwrap_or_default());
        if desired == self.plugin_tabs { return Ok(()); }
        let key = self.screens[self.active].plugin_tab_key();
        let mut old = self.screens.split_off(screens::BUILTIN_COUNT);
        for (name, tab) in &desired {
            let keep = self.plugin_tabs.iter().position(|(n, t)| n == name && t == tab);
            let screen = keep.and_then(|i| {
                let key = self.plugin_tabs[i].clone();
                old.iter().position(|s| s.plugin_tab_key() == Some((key.0.clone(), key.1.id.clone())))
                    .map(|i| old.remove(i))
            }).unwrap_or_else(|| Box::new(screens::PluginTable::new(name.clone(), tab.clone())));
            self.screens.push(screen);
        }
        self.plugin_tabs = desired;
        if let Some(key) = key {
            self.active = self.screens.iter().position(|s| s.plugin_tab_key().as_ref() == Some(&key)).unwrap_or(screens::PLUGINS);
            self.active().enter(ctx)?;
        }
        Ok(())
    }

    fn active(&mut self) -> &mut dyn Screen {
        self.screens[self.active].as_mut()
    }
}

// --- cycle de vie ----------------------------------------------------------------

pub fn init(state: &mut Scenery, ctx: &mut Global) -> Result<(), Error> {
    // Horloge et progression : un tick par seconde.
    state.clock = Some(ctx.add_timer(TimerDef::new().repeat_forever().timer(Duration::from_secs(1))));
    spawn_banner_poll(ctx);
    spawn_onair_watch(ctx);
    spawn_watch(ctx, rpc::watch_events, |e, fresh| AppEvent::Journal(Box::new(e), fresh), AppEvent::JournalLost);
    spawn_watch(ctx, rpc::watch_scan, |s, _| AppEvent::Scan(Box::new(s)), |_| AppEvent::ScanLost);
    state.status.status(0, tr!("status-connecting"));
    state.active().enter(ctx)?;
    Ok(())
}

/// Tâche de fond : lit le bandeau en continu et envoie chaque lecture à la
/// boucle. S'arrête d'elle-même quand la TUI se ferme (canal fermé).
fn spawn_banner_poll(ctx: &Global) {
    let channel = ctx.channel.clone();
    ctx.spawn_async_ext(move |chan| async move {
        let mut delay = rpc::POLL_OK;
        loop {
            let read = rpc::read_banner(channel.clone()).await;
            let ok = read.status.is_ok();
            if chan.send(Ok(Control::Event(AppEvent::Banner(Box::new(read))))).await.is_err() {
                break;
            }
            delay = rpc::next_delay(delay, ok);
            tokio::time::sleep(delay).await;
        }
        Ok(Control::Continue)
    });
}

/// Tâche de fond : suit le flux de l'antenne et le rouvre après une coupure
/// (délai croissant plafonné). Chaque instantané part vers la boucle.
fn spawn_onair_watch(ctx: &Global) {
    let channel = ctx.channel.clone();
    ctx.spawn_async_ext(move |chan| async move {
        let mut delay = rpc::POLL_OK;
        loop {
            match rpc::watch_onair(channel.clone()).await {
                Ok(mut stream) => {
                    let mut fresh = true;
                    loop {
                        match stream.message().await {
                            Ok(Some(snap)) => {
                                delay = rpc::POLL_OK;
                                let ev = AppEvent::OnAir(Box::new(snap), fresh);
                                fresh = false;
                                if chan.send(Ok(Control::Event(ev))).await.is_err() {
                                    return Ok(Control::Continue);
                                }
                            }
                            Ok(None) => break,
                            Err(status) => {
                                let ev = AppEvent::OnAirLost(tr!("onair-stream-error", reason = rpc::status_text(&status)));
                                if chan.send(Ok(Control::Event(ev))).await.is_err() {
                                    return Ok(Control::Continue);
                                }
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    let ev = AppEvent::OnAirLost(tr!("onair-stream-error", reason = e));
                    if chan.send(Ok(Control::Event(ev))).await.is_err() {
                        return Ok(Control::Continue);
                    }
                }
            }
            delay = rpc::next_delay(delay, false);
            tokio::time::sleep(delay).await;
        }
    });
}

/// Tâche de fond : suit un flux serveur et le rouvre après une coupure
/// (délai croissant plafonné). `item(x, premier)` et `lost(raison)` font
/// l'événement envoyé à la boucle.
fn spawn_watch<T, O, F>(ctx: &Global, open: O, item: fn(T, bool) -> AppEvent, lost: fn(String) -> AppEvent)
where
    T: Send + 'static,
    O: Fn(Channel) -> F + Send + 'static,
    F: std::future::Future<Output = Result<tonic::Streaming<T>, String>> + Send,
{
    let channel = ctx.channel.clone();
    ctx.spawn_async_ext(move |chan| async move {
        let mut delay = rpc::POLL_OK;
        loop {
            let why = match open(channel.clone()).await {
                Ok(mut stream) => {
                    let mut fresh = true;
                    loop {
                        match stream.message().await {
                            Ok(Some(x)) => {
                                delay = rpc::POLL_OK;
                                let ev = item(x, fresh);
                                fresh = false;
                                if chan.send(Ok(Control::Event(ev))).await.is_err() {
                                    return Ok(Control::Continue);
                                }
                            }
                            Ok(None) => break tr!("stream-closed"),
                            Err(status) => break rpc::status_text(&status),
                        }
                    }
                }
                Err(e) => e,
            };
            if chan.send(Ok(Control::Event(lost(why)))).await.is_err() {
                return Ok(Control::Continue);
            }
            delay = rpc::next_delay(delay, false);
            tokio::time::sleep(delay).await;
        }
    });
}

/// Lance une action en tâche de fond ; son résultat revient en `ActionDone`.
fn spawn_action(ctx: &Global, state: &mut Scenery, action: Action) {
    let channel = if action.is_long() { ctx.long_channel.clone() } else { ctx.channel.clone() };
    let tz = ctx.store.tz.clone();
    state.status.status(0, tr!("status-action-running"));
    ctx.spawn_async(async move {
        let r = crate::action::run(action, channel, tz).await;
        Ok(Control::Event(AppEvent::ActionDone(r)))
    });
}

pub fn error(err: Error, state: &mut Scenery, _ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
    state.status.status(0, tr!("status-error", reason = format!("{err:#}")));
    Ok(Control::Changed)
}

// --- rendu -----------------------------------------------------------------------

pub fn render(area: Rect, buf: &mut Buffer, state: &mut Scenery, ctx: &mut Global) -> Result<(), Error> {
    let s = Styles(&ctx.theme);
    Block::new().style(s.base()).render(area, buf);

    if area.width < MIN_W || area.height < MIN_H {
        let msg = tr!(
            "terminal-too-small",
            width = area.width,
            height = area.height,
            min_width = MIN_W,
            min_height = MIN_H
        );
        Paragraph::new(msg).wrap(Wrap { trim: true }).style(s.warn()).render(area, buf);
        return Ok(());
    }

    let [banner_a, tabs_a, work_a, status_a, keys_a] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    banner::render(banner_a, buf, &ctx.store, &s);
    render_tabs(tabs_a, buf, state, ctx);
    // Un écran avec un champ de saisie place lui-même le curseur.
    ctx.set_screen_cursor(None);
    state.screens[state.active].render(work_a, buf, ctx)?;

    let s = Styles(&ctx.theme);
    StatusLine::new()
        .layout([Constraint::Fill(1), Constraint::Length(28)])
        .styles(ctx.theme.style(WidgetStyle::STATUSLINE))
        .render(status_a, buf, &mut state.status);
    render_keys(keys_a, buf, state.screens[state.active].help(), &s);

    if state.help_open {
        render_help(work_a, buf, state.screens[state.active].as_ref(), &s);
    }
    if let Some(m) = state.modal.as_mut() {
        ctx.set_screen_cursor(None);
        m.render(work_a, buf, ctx);
    }
    Ok(())
}

/// Plain keys for plugin views: terminals such as VS Code can intercept F6.
fn plugin_tab_direction(key: &KeyEvent) -> Option<bool> {
    match (key.code, key.modifiers) {
        (KeyCode::Tab | KeyCode::Char('n'), KeyModifiers::NONE) => Some(false),
        (KeyCode::BackTab, KeyModifiers::NONE | KeyModifiers::SHIFT)
        | (KeyCode::Char('p'), KeyModifiers::NONE) => Some(true),
        _ => None,
    }
}

fn cycle_screen(active: usize, count: usize, previous: bool) -> usize {
    if previous { (active + count - 1) % count } else { (active + 1) % count }
}

fn render_tabs(area: Rect, buf: &mut Buffer, state: &Scenery, ctx: &Global) {
    let s = Styles(&ctx.theme);
    let labels: Vec<String> = state.screens.iter().enumerate().map(|(i, screen)| {
        if i < 9 { format!(" {} {} ", i + 1, screen.title()) } else { format!(" {} ", screen.title()) }
    }).collect();
    // Keep the active tab visible at any terminal width, with overflow markers.
    let mut start = state.active;
    let mut end = state.active + 1;
    let mut width = ratatui_core::text::Span::raw(&labels[state.active]).width() + 4;
    while start > 0 {
        let extra = ratatui_core::text::Span::raw(&labels[start - 1]).width();
        if width + extra > area.width as usize { break; }
        start -= 1; width += extra;
    }
    while end < labels.len() {
        let extra = ratatui_core::text::Span::raw(&labels[end]).width();
        if width + extra > area.width as usize { break; }
        end += 1; width += extra;
    }
    let mut spans = vec![Span::styled(if start > 0 { "‹ " } else { "  " }, s.muted())];
    for (i, label) in labels.iter().enumerate().take(end).skip(start) {
        let style = if i == state.active { s.tab_active() }
            else if state.screens[i].availability(&ctx.store) != Availability::Available { s.tab_unavailable() }
            else { s.tab() };
        spans.push(Span::styled(label.clone(), style));
    }
    if end < labels.len() { spans.push(Span::styled(" ›", s.muted())); }
    Paragraph::new(Line::from(spans)).style(s.base()).render(area, buf);
}
/// Raccourcis de l'écran puis raccourcis globaux, tant qu'ils tiennent
/// entiers (un raccourci à moitié affiché ne sert à rien).
fn render_keys(area: Rect, buf: &mut Buffer, screen_keys: &[KeyHelp], s: &Styles) {
    let segs: Vec<Vec<Span>> = screen_keys
        .iter()
        .chain(GLOBAL_KEYS.iter())
        .map(|(key, what)| {
            vec![
                Span::styled(format!(" {} ", crate::i18n::text(key, &[])), s.tab_key()),
                Span::styled(format!(" {}", crate::i18n::text(what, &[])), s.muted()),
            ]
        })
        .collect();
    let line = fit::segments(segs, Span::raw("  "), area.width as usize);
    Paragraph::new(line).style(s.base()).render(area, buf);
}

fn render_help(work: Rect, buf: &mut Buffer, screen: &dyn Screen, s: &Styles) {
    let keys: Vec<&KeyHelp> = screen.help().iter().chain(GLOBAL_KEYS.iter()).collect();
    let h = (keys.len() as u16 + 6).min(work.height);
    let w = 60.min(work.width);
    let area = Rect::new(work.x + (work.width - w) / 2, work.y + (work.height - h) / 2, w, h);
    let mut lines = vec![
        Line::styled(tr!("help-title-screen", screen = screen.title()), s.title()),
        Line::default(),
    ];
    for (key, what) in keys {
        lines.push(Line::from(vec![
            Span::styled(format!("{:>10}  ", crate::i18n::text(key, &[])), s.accent()),
            Span::raw(crate::i18n::text(what, &[])),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::styled(tr!("help-legend"), s.muted()));
    Clear.render(area, buf);
    Paragraph::new(lines)
        .style(s.base())
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(s.accent())
                .title(Span::styled(format!(" {} ", tr!("help-box-title")), s.title())),
        )
        .render(area, buf);
}

// --- événements ------------------------------------------------------------------

fn press(e: &Event) -> Option<&KeyEvent> {
    match e {
        Event::Key(k) if k.kind == KeyEventKind::Press => Some(k),
        _ => None,
    }
}

pub fn event(event: &AppEvent, state: &mut Scenery, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
    match event {
        AppEvent::Timer(t) if Some(t.handle) == state.clock => {
            let _ = state.active().event(event, ctx)?;
            return Ok(Control::Changed);
        }
        AppEvent::PluginTable(..) => {
            // Deliver replies even to inactive tabs; no stranded in-flight requests.
            for screen in &mut state.screens { let _ = screen.event(event, ctx)?; }
            return Ok(Control::Changed);
        }
        AppEvent::Timer(_) => {}
        AppEvent::Banner(read) => {
            let was = ctx.store.link.clone();
            let ok = ctx.store.apply_banner((**read).clone());
            state.sync_plugin_tabs(ctx)?;
            let was_lost = matches!(was, crate::store::Link::Lost { .. });
            if ok && was != ctx.store.link {
                state.expect_exit = false;
                state.status.status(0, tr!("status-connected"));
                let active = state.active;
                state.screens[active].reconnected(ctx)?;
            } else if !ok
                && !was_lost
                && !state.expect_exit
                && let crate::store::Link::Lost { error, .. } = &ctx.store.link
            {
                // Une fois, à la perte : le bandeau dit ensuite depuis quand.
                state.status.status(0, error.clone());
            }
            state.status.status(1, tr!("status-screen", screen = state.screens[state.active].title()));
            return Ok(Control::Changed);
        }
        AppEvent::OnAir(snap, fresh) => {
            ctx.store.apply_onair((**snap).clone(), *fresh);
            return Ok(Control::Changed);
        }
        AppEvent::OnAirLost(why) => {
            ctx.store.onair_link = Err(why.clone());
            return Ok(Control::Changed);
        }
        AppEvent::Journal(ev, fresh) => {
            ctx.store.apply_journal((**ev).clone(), *fresh);
            return Ok(Control::Changed);
        }
        AppEvent::JournalLost(why) => {
            ctx.store.journal_link = Err(why.clone());
            return Ok(Control::Changed);
        }
        AppEvent::Scan(s) => {
            ctx.store.scan = Some((**s).clone());
            return Ok(Control::Changed);
        }
        AppEvent::ScanLost => {
            ctx.store.scan = None;
            return Ok(Control::Changed);
        }
        AppEvent::ActionDone(r) => {
            match r {
                Ok(done) => {
                    state.expect_exit = done.exits;
                    state.status.status(0, done.message.clone());
                    if let Some(scan) = &done.scan {
                        ctx.store.last_scan = Some(scan.clone());
                    }
                }
                Err(e) => state.status.status(0, tr!("status-action-failed", reason = e.clone())),
            }
            // L'écran actif peut vouloir relire ce que l'action a changé.
            let active = state.active;
            let _ = state.screens[active].event(event, ctx)?;
            return Ok(Control::Changed);
        }
        AppEvent::Rendered => {
            let mut b = FocusBuilder::new(ctx.take_focus());
            state.screens[state.active].build_focus(&mut b);
            ctx.set_focus(b.build());
            return Ok(Control::Continue);
        }
        // Réponse à la liste ouverte d'un formulaire (modale) ; sinon destinée
        // à l'écran qui l'a demandée (plus bas).
        AppEvent::Media(..) | AppEvent::MediaTyped(..) | AppEvent::PlaylistChoices(..)
            if state.modal.as_mut().is_some_and(|m| m.on_event(event, ctx)) =>
        {
            return Ok(Control::Changed);
        }
        AppEvent::Media(..)
        | AppEvent::MediaTyped(..)
        | AppEvent::MediaCard(..)
        | AppEvent::MediaTags(..)
        | AppEvent::PlaylistChoices(..)
        | AppEvent::Playlists(..)
        | AppEvent::Agenda(..)
        | AppEvent::Tags(..)
        | AppEvent::System(..) => {}
        AppEvent::Event(Event::Resize(..)) => return Ok(Control::Changed),
        AppEvent::Event(e) => {
            if let Some(k) = press(e) {
                // Quitter, toujours possible.
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('q') | KeyCode::Char('c'))
                {
                    return Ok(Control::Quit);
                }
                // Une modale capture tout.
                if let Some(m) = state.modal.as_mut() {
                    match m.handle(e, ctx) {
                        Outcome::Unchanged => return Ok(Control::Unchanged),
                        Outcome::Changed => return Ok(Control::Changed),
                        Outcome::Cancel => {
                            state.modal = None;
                            state.status.status(0, tr!("status-cancelled"));
                        }
                        Outcome::Submit(action) => {
                            state.modal = None;
                            spawn_action(ctx, state, action);
                        }
                        Outcome::Replace(next) => state.modal = Some(*next),
                        Outcome::Close => state.modal = None,
                    }
                    return Ok(Control::Changed);
                }
                // L'aide ouverte capture tout (modale).
                if state.help_open {
                    if matches!(k.code, KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?') | KeyCode::Char('q')) {
                        state.help_open = false;
                    }
                    return Ok(Control::Changed);
                }
                // F1 : l'aide, même pendant une saisie.
                if k.code == KeyCode::F(1) {
                    state.help_open = true;
                    return Ok(Control::Changed);
                }
                if state.screens[state.active].plugin_tab_key().is_some()
                    && !state.screens[state.active].captures_text()
                    && let Some(previous) = plugin_tab_direction(k)
                {
                    let count = state.screens.len() - screens::BUILTIN_COUNT;
                    state.active = screens::BUILTIN_COUNT
                        + cycle_screen(state.active - screens::BUILTIN_COUNT, count, previous);
                    state.status.status(1, tr!("status-screen", screen = state.screens[state.active].title()));
                    state.active().enter(ctx)?;
                    return Ok(Control::Changed);
                }
                if !state.screens[state.active].captures_text()
                    && ((k.modifiers == KeyModifiers::CONTROL && matches!(k.code, KeyCode::PageUp | KeyCode::PageDown))
                        || (k.code == KeyCode::F(6) && (k.modifiers.is_empty() || k.modifiers == KeyModifiers::SHIFT)))
                {
                    let previous = k.code == KeyCode::PageUp || k.modifiers == KeyModifiers::SHIFT;
                    state.active = cycle_screen(state.active, state.screens.len(), previous);
                    state.status.status(1, tr!("status-screen", screen = state.screens[state.active].title()));
                    state.active().enter(ctx)?;
                    return Ok(Control::Changed);
                }
                if !state.screens[state.active].captures_text() && k.modifiers.is_empty() {
                    match k.code {
                        KeyCode::Char('q') => return Ok(Control::Quit),
                        KeyCode::Char('?') => {
                            state.help_open = true;
                            return Ok(Control::Changed);
                        }
                        KeyCode::Char(c @ '1'..='9') => {
                            let idx = (c as usize) - ('1' as usize);
                            if idx < state.screens.len() && idx != state.active {
                                state.active = idx;
                                state.status.status(1, tr!("status-screen", screen = state.screens[idx].title()));
                                state.active().enter(ctx)?;
                            }
                            return Ok(Control::Changed);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let active = state.active;
    let r = state.screens[active].event(event, ctx)?;
    if let Some(msg) = ctx.pending_status.take() {
        state.status.status(0, msg);
    }
    if let Some(m) = ctx.pending_modal.take() {
        state.modal = Some(m);
        return Ok(Control::Changed);
    }
    if let Some(a) = ctx.pending_action.take() {
        spawn_action(ctx, state, a);
        return Ok(Control::Changed);
    }
    if let Some(idx) = ctx.pending_switch.take()
        && idx < state.screens.len()
    {
        state.active = idx;
        state.status.status(1, tr!("status-screen", screen = state.screens[idx].title()));
        state.active().enter(ctx)?;
        return Ok(Control::Changed);
    }
    Ok(r)
}

#[cfg(test)]
mod plugin_tab_tests {
    use super::*;
    use stationd_proto::plugin::{PluginInfo, PluginTab};
    fn plugin(name: &str, title: &str) -> PluginInfo {
        PluginInfo { name: name.into(), state: "disabled".into(),
            tabs: vec![PluginTab { id: "table".into(), title: title.into(), ..Default::default() }],
            ..Default::default() }
    }
    #[tokio::test]
    async fn dynamic_tabs_preserve_active_identity_on_reorder_and_return_to_catalog_on_removal() {
        let args = crate::Args { addr: "http://127.0.0.1:50051".into(), lang: None, theme: "Imperial".into(), list_themes: false };
        let channel = rpc::lazy_channel(&args.addr).unwrap();
        let mut ctx = Global::new(&args, channel.clone(), channel);
        let mut state = Scenery::new();
        assert_eq!(state.screens.len(), screens::BUILTIN_COUNT);
        ctx.store.plugins.value = Some(vec![plugin("a", "A"), plugin("b", "B")]);
        state.sync_plugin_tabs(&mut ctx).unwrap();
        state.active = screens::BUILTIN_COUNT + 1;
        let key = state.screens[state.active].plugin_tab_key();
        ctx.store.plugins.value.as_mut().unwrap().reverse();
        state.sync_plugin_tabs(&mut ctx).unwrap();
        assert_eq!(state.active, screens::BUILTIN_COUNT);
        assert_eq!(state.screens[state.active].plugin_tab_key(), key);
        ctx.store.plugins.value = Some(vec![plugin("a", "A")]);
        state.sync_plugin_tabs(&mut ctx).unwrap();
        assert_eq!(state.active, screens::PLUGINS);
    }
    #[tokio::test]
    async fn plugin_navigation_keys_reach_the_event_handler_and_wrap_between_views() {
        let args = crate::Args { addr: "http://127.0.0.1:50051".into(), lang: None, theme: "Imperial".into(), list_themes: false };
        let channel = rpc::lazy_channel(&args.addr).unwrap();
        let mut ctx = Global::new(&args, channel.clone(), channel);
        let mut state = Scenery::new();
        let mut audience = plugin("listener-stats", "Audience");
        audience.tabs.push(PluginTab { id: "geography".into(), title: "Géographie".into(), ..Default::default() });
        ctx.store.plugins.value = Some(vec![audience, plugin("play-stats", "Diffusions")]);
        state.sync_plugin_tabs(&mut ctx).unwrap();
        state.active = screens::BUILTIN_COUNT;
        for (code, modifiers, index) in [
            (KeyCode::Tab, KeyModifiers::NONE, 9),
            (KeyCode::Char('n'), KeyModifiers::NONE, 10),
            (KeyCode::Char('n'), KeyModifiers::NONE, 8),
            (KeyCode::BackTab, KeyModifiers::SHIFT, 10),
            (KeyCode::Char('p'), KeyModifiers::NONE, 9),
            (KeyCode::F(6), KeyModifiers::NONE, 10),
        ] {
            let key = AppEvent::Event(Event::Key(KeyEvent::new(code, modifiers)));
            let _ = event(&key, &mut state, &mut ctx).unwrap();
            assert_eq!(state.active, index, "{code:?} {modifiers:?}");
        }
        assert_eq!(plugin_tab_direction(&KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)), None);
    }

    #[test]
    fn tab_navigation_wraps_beyond_digit_shortcuts() {
        assert_eq!(cycle_screen(12, 13, false), 0);
        assert_eq!(cycle_screen(0, 13, true), 12);
        assert_eq!(cycle_screen(9, 13, false), 10);
    }
    #[test]
    fn narrow_tab_bar_keeps_the_active_plugin_visible() {
        let args = crate::Args { addr: "".into(), lang: None, theme: "Imperial".into(), list_themes: false };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let channel = rpc::lazy_channel("http://127.0.0.1:50051").unwrap();
        let mut ctx = Global::new(&args, channel.clone(), channel);
        let mut state = Scenery::new();
        state.screens.push(Box::new(screens::PluginTable::new("stats".into(),
            PluginTab { id: "x".into(), title: "Audience".into(), ..Default::default() })));
        state.active = screens::BUILTIN_COUNT;
        ctx.store.plugins.value = Some(vec![PluginInfo { name: "stats".into(), ..Default::default() }]);
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        render_tabs(area, &mut buf, &state, &ctx);
        let text: String = (0..80).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(text.contains("Audience"), "{text}");
        assert!(text.contains('‹'));
    }
}