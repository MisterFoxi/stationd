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
    (k!("key-help"), k!("help-screen-help")),
    (k!("key-quit"), k!("help-quit")),
    (k!("key-force-quit"), k!("help-force-quit")),
];

/// Données accessibles partout (rendu et événements).
pub struct Global {
    ctx: SalsaAppContext<AppEvent, Error>,
    pub theme: SalsaTheme,
    pub channel: Channel,
    pub store: Store,
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
    pub fn new(args: &Args, channel: Channel) -> Self {
        Self {
            ctx: Default::default(),
            theme: create_salsa_theme(&args.theme),
            channel,
            store: Store::new(&args.addr),
        }
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
    active: usize,
    help_open: bool,
    status: StatusLineState,
    /// Tick d'une seconde (horloge, progression).
    clock: Option<TimerHandle>,
}

impl Scenery {
    pub fn new() -> Self {
        Self {
            screens: screens::registry(),
            active: 0,
            help_open: false,
            status: StatusLineState::default(),
            clock: None,
        }
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
    Ok(())
}

fn render_tabs(area: Rect, buf: &mut Buffer, state: &Scenery, ctx: &Global) {
    let s = Styles(&ctx.theme);
    // Trois densités, de la plus lisible à la plus compacte : titres complets
    // espacés, titres complets serrés, titres abrégés (sauf l'onglet actif).
    for density in 0..3 {
        let mut spans = vec![Span::raw(" ")];
        for (i, screen) in state.screens.iter().enumerate() {
            let active = i == state.active;
            let available = screen.availability(&ctx.store) == Availability::Available;
            let (key_style, title_style) = if active {
                (s.tab_active(), s.tab_active())
            } else if !available {
                (s.tab_unavailable(), s.tab_unavailable())
            } else {
                (s.tab_key(), s.tab())
            };
            let title = match density {
                2 if !active => screen.title().chars().take(4).collect::<String>(),
                _ => screen.title(),
            };
            spans.push(Span::styled(format!(" {} ", i + 1), key_style));
            spans.push(Span::styled(format!("{title} "), title_style));
            if density == 0 {
                spans.push(Span::raw(" "));
            }
        }
        if density == 2 || fit::width(&spans) <= area.width as usize {
            Paragraph::new(Line::from(spans)).style(s.base()).render(area, buf);
            return;
        }
    }
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
        AppEvent::Timer(t) if Some(t.handle) == state.clock => return Ok(Control::Changed),
        AppEvent::Timer(_) => {}
        AppEvent::Banner(read) => {
            let was = ctx.store.link.clone();
            let ok = ctx.store.apply_banner((**read).clone());
            if ok && was != ctx.store.link {
                state.status.status(0, tr!("status-connected"));
            } else if !ok && let crate::store::Link::Lost { error, .. } = &ctx.store.link {
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
        AppEvent::Rendered => {
            let mut b = FocusBuilder::new(ctx.take_focus());
            state.screens[state.active].build_focus(&mut b);
            ctx.set_focus(b.build());
            return Ok(Control::Continue);
        }
        AppEvent::Event(Event::Resize(..)) => return Ok(Control::Changed),
        AppEvent::Event(e) => {
            if let Some(k) = press(e) {
                // Quitter, toujours possible.
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('q') | KeyCode::Char('c'))
                {
                    return Ok(Control::Quit);
                }
                // L'aide ouverte capture tout (modale).
                if state.help_open {
                    if matches!(k.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
                        state.help_open = false;
                    }
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
    state.screens[active].event(event, ctx)
}
