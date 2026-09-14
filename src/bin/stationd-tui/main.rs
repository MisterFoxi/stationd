//! Optional terminal admin client; stationd continues running when this exits.
mod app;
mod agenda;
mod agenda_ui;
mod client;
mod playlist_form;
mod ui;

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::{
    cursor::Show,
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tonic::transport::Endpoint;

use app::App;
use playlist_form::{FormAction, PlaylistForm};

enum JobResult {
    Preview(agenda::Window, client::ReadResult<Vec<agenda::Entry>>),
    Saved(Result<PathBuf, String>),
    Synced(client::ReadResult<stationd::proto::station::PlaylistSyncReply>),
}

#[derive(Parser)]
#[command(name = "stationd-tui", version, about = "StationD terminal client and local playlist authoring")]
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
    /// Local playlist directory for TOML creation (overrides --config)
    #[arg(long)]
    playlist_root: Option<PathBuf>,
    /// Local daemon configuration, used only to locate the playlist directory
    #[arg(long, default_value = "stationd.toml")]
    config: PathBuf,
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen, Show);
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
    let sync_endpoint = endpoint.clone().timeout(Duration::from_secs(60));
    let (job_tx, job_rx) = std::sync::mpsc::channel();
    let (preview_tx, mut preview_rx) = tokio::sync::mpsc::channel::<agenda::Window>(1);
    let preview_endpoint = endpoint.clone();
    let preview_result_tx = job_tx.clone();
    runtime.spawn(async move {
        let channel = preview_endpoint.connect_lazy();
        while let Some(window) = preview_rx.recv().await {
            let result = client::preview(channel.clone(), &window).await;
            if preview_result_tx.send(JobResult::Preview(window, result)).is_err() { break; }
        }
    });
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
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut app = App { auto: !args.manual, ..App::default() };
    let interval = Duration::from_secs(args.refresh_seconds);
    let mut next_refresh = Instant::now();
    let mut requested = true;
    while !stopping.load(Ordering::Relaxed) {
        while let Ok(result) = job_rx.try_recv() {
            match result {
                JobResult::Preview(window, result) => app.agenda.finish(window, result),
                JobResult::Saved(Ok(path)) => {
                    app.form = None;
                    app.report = Some(format!("Saved: {}\n\nThe TOML is on disk; it has not been synchronized.\nClose this message, then press s in Playlists to sync.\nSync scans the daemon's configured directory; for a remote daemon,\nthe local directory must be shared with it.", path.display()));
                    app.report_scroll = 0;
                }
                JobResult::Saved(Err(error)) => {
                    if let Some(form) = &mut app.form { form.saving = false; form.message = error; }
                }
                JobResult::Synced(result) => {
                    app.syncing = false;
                    app.report = Some(match result {
                        Ok(reply) => {
                            let mut report = format!("Synchronized: {} playlist(s)\nErrors: {}", reply.added, reply.errors.len());
                            for error in reply.errors { report.push_str(&format!("\n\n{}\n{}", error.path, error.message)); }
                            report
                        }
                        Err(error) => format!("Sync failed: {error}\n\nLocal TOML files are still available.\nIf the connection was lost, check the daemon before retrying."),
                    });
                    app.report_scroll = 0;
                    requested = true;
                }
            }
        }
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
        if app.tab == 3 {
            if let Some(window) = app.agenda.take_request(app.auto, interval) {
                preview_tx.try_send(window)?;
            }
        }
        terminal.draw(|frame| ui::draw(frame, &mut app, &args.addr))?;
        if !event::poll(Duration::from_millis(50))? { continue; }
        let event = event::read()?;
        if let Event::Paste(text) = &event {
            if let Some(form) = &mut app.form { form.paste(text); }
            else if app.tab == 3 { app.agenda.paste_date(text); }
        }
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release { continue; }
            // Editor keys are handled first: q, r, a, n and s are normal text.
            if let Some(form) = &mut app.form {
                match form.handle(key) {
                    FormAction::None => {},
                    FormAction::Close => app.form = None,
                    FormAction::Save { path, toml } => {
                        let root = form.root.clone();
                        let tx = job_tx.clone();
                        runtime.spawn_blocking(move || {
                            let result = playlist_form::save_new(&root, &path, &toml).map_err(|e| format!("{e:#}"));
                            let _ = tx.send(JobResult::Saved(result));
                        });
                    }
                }
                continue;
            }
            if app.report.is_some() {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => app.report = None,
                    KeyCode::PageDown | KeyCode::Down => app.report_scroll = app.report_scroll.saturating_add(5),
                    KeyCode::PageUp | KeyCode::Up => app.report_scroll = app.report_scroll.saturating_sub(5),
                    _ => {},
                }
                continue;
            }
            if app.tab == 3 && app.agenda.date_input.is_some() {
                app.agenda.handle(key);
                continue;
            }
            if key.code == KeyCode::Char('q') || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)) {
                break;
            }
            if app.help {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) { app.help = false; }
                continue;
            }
            if app.tab == 3 && matches!(key.code,
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down |
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End | KeyCode::Enter |
                KeyCode::Char('d' | 'w' | 't' | 'g' | '[' | ']' | '+' | '=' | '-' | 'j' | 'k')) {
                if let agenda::Action::Detail(text) = app.agenda.handle(key) {
                    app.report = Some(text); app.report_scroll = 0;
                }
                continue;
            }
            match key.code {
                KeyCode::Char('n') if app.tab == 1 => {
                    match playlist_form::playlist_root(args.playlist_root.as_deref(), &args.config) {
                        Ok(root) => app.form = Some(PlaylistForm::new(root)),
                        Err(e) => { app.report = Some(format!("{e:#}")); app.report_scroll = 0; }
                    }
                }
                KeyCode::Char('s') if app.tab == 1 && !app.syncing => {
                    app.syncing = true;
                    app.report = Some("Synchronizing the daemon's playlist directory...".into());
                    app.report_scroll = 0;
                    let endpoint = sync_endpoint.clone();
                    let tx = job_tx.clone();
                    runtime.spawn(async move {
                        let result = client::sync_playlists(endpoint.connect_lazy()).await;
                        let _ = tx.send(JobResult::Synced(result));
                    });
                }
                KeyCode::Char('?') => app.help = true,
                KeyCode::Char('1') => app.select_tab(0),
                KeyCode::Char('2') => app.select_tab(1),
                KeyCode::Char('3') => app.select_tab(2),
                KeyCode::Char('4') => app.select_tab(3),
                KeyCode::Tab | KeyCode::Right => app.select_tab(app.tab + 1),
                KeyCode::BackTab | KeyCode::Left => app.select_tab(app.tab + 3),
                KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
                KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
                KeyCode::Home => app.move_selection(isize::MIN),
                KeyCode::End => app.move_selection(isize::MAX),
                KeyCode::PageDown => app.detail_scroll = app.detail_scroll.saturating_add(5),
                KeyCode::PageUp => app.detail_scroll = app.detail_scroll.saturating_sub(5),
                KeyCode::Char('r') => { requested = true; app.agenda.request(); },
                KeyCode::Char('a') => { app.auto = !app.auto; if app.auto { requested = true; app.agenda.request(); } },
                _ => {},
            }
        }
    }
    Ok(())
}
