//! Opt-in OSC telemetry travels through the same PTY/output path as the UI.
//! Emitted by the event loop, never a background thread: a stalled loop stops it.
use std::{
    cell::RefCell,
    io::{self, Write},
    time::{Duration, Instant},
};
use anyhow::Error;
use rat_salsa::Control;
use ratatui_crossterm::crossterm::event::{Event, KeyEventKind};
use crate::app::{self, AppEvent, Global, Scenery};

#[derive(Default)]
struct Diagnostics {
    enabled: bool,
    keys: u64,
    last: Option<Instant>,
}
thread_local! {
    static DIAGNOSTICS: RefCell<Diagnostics> = RefCell::new(Diagnostics {
        enabled: std::env::var("STATIOND_WEBMIN_DIAGNOSTICS").as_deref() == Ok("1"),
        ..Diagnostics::default()
    });
}

pub fn event(event: &AppEvent, state: &mut Scenery, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
    let result = app::event(event, state, ctx);
    DIAGNOSTICS.with(|cell| {
        let mut diagnostic = cell.borrow_mut();
        if !diagnostic.enabled {
            return;
        }
        let key = matches!(event, AppEvent::Event(Event::Key(k)) if k.kind == KeyEventKind::Press);
        if key {
            diagnostic.keys = diagnostic.keys.saturating_add(1);
        }
        if key || diagnostic.last.is_none_or(|last| last.elapsed() >= Duration::from_secs(1)) {
            diagnostic.last = Some(Instant::now());
            let mut output = io::stdout().lock();
            // No keys/content are disclosed. Counter means the handler returned,
            // including a key with no action; it does not acknowledge an RPC.
            let _ = write!(output, "\x1b]777;stationd;{}\x07", diagnostic.keys);
            let _ = output.flush();
        }
    });
    result
}
