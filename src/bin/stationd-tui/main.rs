//! Optional terminal admin client; stationd continues running when this exits.
mod app;
mod client;
mod ui;

use std::io::{self, IsTerminal};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tonic::transport::Endpoint;

use app::App;

#[derive(Parser)]
#[command(name = "stationd-tui", version, about = "Terminal administration for stationd (read-only v1)")]
struct Args {
    /// Same gRPC endpoint as stationctl
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    addr: String,
    /// Automatic refresh interval in seconds (1..60)
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u64).range(1..=60))]
    refresh_seconds: u64,
    /// Start with automatic refresh disabled; r still refreshes
    #[arg(long)]
    manual: bool,
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) { restore_terminal(); }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    anyhow::ensure!(io::stdin().is_terminal() && io::stdout().is_terminal(),
        "stationd-tui needs an interactive terminal (use stationctl for scripts)");
    let endpoint = Endpoint::from_shared(args.addr.clone())?
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5));
    let runtime = tokio::runtime::Runtime::new()?;
    let (request_tx, mut request_rx) = tokio::sync::mpsc::channel::<()>(1);
    let (snapshot_tx, snapshot_rx) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        // Lazy connection permits launching the UI while stationd is offline.
        let channel = endpoint.connect_lazy();
        while request_rx.recv().await.is_some() {
            if snapshot_tx.send(client::refresh(channel.clone()).await).is_err() { break; }
        }
    });
    let stopping = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stopping);
    runtime.spawn(async move {
        #[cfg(unix)]
        {
            if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
            } else {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        signal_stop.store(true, Ordering::Relaxed);
    });

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| { restore_terminal(); previous_hook(info); }));
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut app = App { auto: !args.manual, ..App::default() };
    let interval = Duration::from_secs(args.refresh_seconds);
    let mut next_refresh = Instant::now();
    let mut requested = true;
    while !stopping.load(Ordering::Relaxed) {
        match snapshot_rx.try_recv() {
            Ok(snapshot) => {
                app.apply(snapshot);
                next_refresh = Instant::now() + interval;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {},
            Err(std::sync::mpsc::TryRecvError::Disconnected) => anyhow::bail!("RPC worker stopped"),
        }
        if !app.loading && (requested || (app.auto && Instant::now() >= next_refresh)) {
            request_tx.try_send(())?;
            app.loading = true;
            requested = false;
        }
        terminal.draw(|frame| ui::draw(frame, &mut app, &args.addr))?;
        if !event::poll(Duration::from_millis(50))? { continue; }
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release { continue; }
            if key.code == KeyCode::Char('q') || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)) {
                break;
            }
            if app.help {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) { app.help = false; }
                continue;
            }
            match key.code {
                KeyCode::Char('?') => app.help = true,
                KeyCode::Char('1') => app.select_tab(0),
                KeyCode::Char('2') => app.select_tab(1),
                KeyCode::Char('3') => app.select_tab(2),
                KeyCode::Tab | KeyCode::Right => app.select_tab(app.tab + 1),
                KeyCode::BackTab | KeyCode::Left => app.select_tab(app.tab + 2),
                KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
                KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
                KeyCode::Home => app.move_selection(isize::MIN),
                KeyCode::End => app.move_selection(isize::MAX),
                KeyCode::PageDown => app.detail_scroll = app.detail_scroll.saturating_add(5),
                KeyCode::PageUp => app.detail_scroll = app.detail_scroll.saturating_sub(5),
                KeyCode::Char('r') => { if !app.loading { requested = true; } },
                KeyCode::Char('a') => { app.auto = !app.auto; if app.auto { requested = true; } },
                _ => {},
            }
        }
    }
    Ok(())
}
