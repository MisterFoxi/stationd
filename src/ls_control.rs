//! Liquidsoap control socket ("C" of the A+C decision): stationd → Liquidsoap.
//!
//! The generated script registers, on Liquidsoap's server socket, the
//! commands `stationd.pause` / `stationd.resume` / `stationd.skip` /
//! `stationd.state`. Protocol (Liquidsoap server): one command line, the
//! reply lines, then a line `END`.
//!
//! Two users:
//! - the **air sync** task follows the broadcast state machine: a transition
//!   to `paused` pauses Liquidsoap NOW (current track frozen, halted noise on
//!   air), a transition to `running` resumes it. Whoever made the transition
//!   (CLI, plugin) — the hook is in `StationControl`. A failed push is warned,
//!   recorded for `stationctl ls status`, and retried until it lands or is
//!   superseded: the station's desired state is never silently lost;
//! - the same task follows the override queue: a soft push sends
//!   `stationd.flush` (it airs at the next boundary), a hard push is resolved
//!   now and cut in with `stationd.interrupt <uri>`;
//! - `BroadcastService.Skip` (`stationctl station next`) sends `stationd.skip`
//!   and reports the outcome to the caller.
//!
//! A `stop` stays graceful — the current track plays to its end — but the
//! track Liquidsoap already prepared is dropped (`stationd.flush`): the next
//! pull then gets `halted`, so the stop lands at the end of the CURRENT track,
//! not one track later.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::grid_engine::GridEngine;
use crate::ls_bridge::LsBridge;
use crate::resolver::Epoch;
use crate::station_control::{AirEvent, BroadcastState, OverrideMode, Transition};

/// The AtClock ticker re-plans at least this often.
const TICK_MAX: Duration = Duration::from_secs(60);

/// Whole round-trip bound (connect + command + reply).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
/// Retry period of a push that failed (Liquidsoap down, socket rights…).
const RETRY_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(5)
};

#[derive(Debug, thiserror::Error)]
pub enum LsControlError {
    #[error("Liquidsoap control socket {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Liquidsoap control socket {0}: no reply within 3s")]
    Timeout(String),
    #[error("Liquidsoap refused `{cmd}`: {reply}")]
    Refused { cmd: String, reply: String },
}

/// Outcome of the last push, for `stationctl ls status`.
#[derive(Debug, Clone, Default)]
pub struct ControlHealth {
    /// Last command that went through, and when (epoch s).
    pub last_ok: Option<(String, i64)>,
    /// Last failure (message, epoch s) — cleared by the next success.
    pub last_error: Option<(String, i64)>,
}

#[derive(Clone)]
pub struct LsControl {
    path: PathBuf,
    health: Arc<Mutex<ControlHealth>>,
}

fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl LsControl {
    pub fn new(path: PathBuf) -> Self {
        Self { path, health: Arc::new(Mutex::new(ControlHealth::default())) }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Forget the last failure (a start-up probe that found no Liquidsoap is
    /// not an error worth showing in `ls status`).
    fn clear_error(&self) {
        self.health.lock().unwrap_or_else(|p| p.into_inner()).last_error = None;
    }

    pub fn health(&self) -> ControlHealth {
        self.health.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Send one command, return its reply (lines before `END`, joined).
    /// Every outcome is recorded in the health snapshot.
    pub async fn command(&self, cmd: &str) -> Result<String, LsControlError> {
        let res = match tokio::time::timeout(COMMAND_TIMEOUT, self.round_trip(cmd)).await {
            Ok(r) => r,
            Err(_) => Err(LsControlError::Timeout(self.path.display().to_string())),
        };
        let mut h = self.health.lock().unwrap_or_else(|p| p.into_inner());
        match &res {
            Ok(_) => {
                // The verb only: an interrupt carries a whole uri.
                let verb = cmd.split(' ').next().unwrap_or(cmd);
                h.last_ok = Some((verb.to_string(), epoch_now()));
                h.last_error = None;
            }
            Err(e) => h.last_error = Some((e.to_string(), epoch_now())),
        }
        res
    }

    async fn round_trip(&self, cmd: &str) -> Result<String, LsControlError> {
        let io = |source| LsControlError::Io { path: self.path.display().to_string(), source };
        let stream = UnixStream::connect(&self.path).await.map_err(io)?;
        let (rd, mut wr) = stream.into_split();
        wr.write_all(format!("{cmd}\n").as_bytes()).await.map_err(io)?;
        let mut lines = BufReader::new(rd).lines();
        let mut reply = Vec::new();
        loop {
            match lines.next_line().await.map_err(io)? {
                None => break, // closed before END: take what we got
                Some(l) if l.trim_end() == "END" => break,
                Some(l) => reply.push(l.trim_end().to_string()),
            }
        }
        // Best effort: tell the server we are done.
        let _ = wr.write_all(b"quit\n").await;
        let reply = reply.join("\n");
        // Liquidsoap answers an unknown command with an error text, not a
        // transport failure: our commands all answer "OK" (or the state).
        let cmd = cmd.split(' ').next().unwrap_or(cmd);
        if cmd != "stationd.state" && reply != "OK" {
            return Err(LsControlError::Refused { cmd: cmd.to_string(), reply });
        }
        Ok(reply)
    }
}

/// The Liquidsoap command a broadcast transition calls for, if any.
pub fn command_for(t: &Transition) -> Option<&'static str> {
    match t.to {
        BroadcastState::Paused => Some("stationd.pause"),
        BroadcastState::Running => Some("stationd.resume"),
        // stop: graceful (current track to its end) but the prepared track is
        // dropped, so the pull asks again and gets `halted` right away.
        BroadcastState::Stopped => Some("stationd.flush"),
        // draining: nothing yet — it becomes `stopped` at a boundary.
        BroadcastState::Draining => None,
    }
}

/// A command to deliver, and why (the retry policy depends on it).
#[derive(Debug, Clone)]
enum Push {
    /// Desired air state (pause / resume / flush on stop): retried until it
    /// lands or a newer state supersedes it.
    State { cmd: &'static str, t: Option<Transition> },
    /// Drop the prepared track so a soft override airs at the next boundary.
    Flush,
}

/// Follow the broadcast state and the override queue, act on the air.
/// `initial` = the state restored at start-up: re-asserted once (a Liquidsoap
/// that stayed up while stationd restarted converges).
///
/// - transitions: `paused` ⇒ `pause`, `running` ⇒ `resume`, `stopped` ⇒
///   `flush` (the stop lands at the end of the CURRENT track);
/// - soft override ⇒ `flush` (it airs at the next boundary, not one later);
/// - hard override ⇒ resolved now and cut in with `interrupt <uri>` (the
///   current track is dropped; the pull re-asks, so a multi-track playlist
///   override carries on after the cut). Not retried: a cut that can't
///   happen now is meaningless later — loudly reported instead.
pub fn spawn_air_sync(
    mut rx: mpsc::UnboundedReceiver<AirEvent>,
    ls: LsControl,
    bridge: LsBridge,
    initial: BroadcastState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut pending: Option<Push> = match initial {
            BroadcastState::Paused => Some(Push::State { cmd: "stationd.pause", t: None }),
            BroadcastState::Running => Some(Push::State { cmd: "stationd.resume", t: None }),
            _ => None,
        };
        let mut first = true;
        loop {
            // Never flush a prepared override: it was consumed from the queue
            // when handed out, dropping it would lose it. It simply airs
            // first (before a stop, before a newer soft override).
            let is_flush = match &pending {
                Some(Push::Flush) => true,
                Some(Push::State { cmd, .. }) => *cmd == "stationd.flush",
                None => false,
            };
            if is_flush && bridge.prepared_is_override() {
                tracing::info!("prepared track is an override: not flushed, it airs first");
                pending = None;
            }
            if let Some(push) = pending.clone() {
                let cmd = match &push {
                    Push::State { cmd, .. } => *cmd,
                    Push::Flush => "stationd.flush",
                };
                match ls.command(cmd).await {
                    Ok(_) => {
                        tracing::info!(cmd, "Liquidsoap control: applied");
                        if let Push::State { t: Some(t), .. } = push {
                            // From a pause — or from a stop pushed during
                            // a pause, which leaves the track frozen: the
                            // frozen track plays on. (No frozen track → no-op.)
                            if matches!(t.from, BroadcastState::Paused | BroadcastState::Stopped)
                                && t.to == BroadcastState::Running
                            {
                                bridge.resumed_from_pause();
                            }
                        }
                        pending = None;
                    }
                    Err(e) if first && matches!(push, Push::State { t: None, .. }) => {
                        // Start-up re-assert: Liquidsoap may simply not be up
                        // yet — it starts consistent anyway (first pull).
                        tracing::info!(error = %e, "Liquidsoap control socket not reachable at start-up");
                        ls.clear_error();
                        pending = None;
                    }
                    Err(e) => match push {
                        Push::State { .. } => {
                            tracing::warn!(cmd, error = %e, "Liquidsoap control: push failed, retrying")
                        }
                        Push::Flush => {
                            // Not worth retrying: the override still airs,
                            // one track later.
                            tracing::warn!(error = %e, "Liquidsoap flush failed: the override airs one track later");
                            pending = None;
                        }
                    },
                }
            }
            first = false;
            let next = if matches!(pending, Some(Push::State { .. })) {
                match tokio::time::timeout(RETRY_EVERY, rx.recv()).await {
                    Ok(msg) => msg.map(Some),
                    Err(_) => Some(None), // retry tick
                }
            } else {
                rx.recv().await.map(Some)
            };
            match next {
                None => break, // control dropped: daemon shutting down
                Some(None) => {} // retry the pending push
                Some(Some(AirEvent::Transition(t))) => {
                    // The latest state supersedes any undelivered one.
                    pending = command_for(&t).map(|cmd| Push::State { cmd, t: Some(t) });
                }
                Some(Some(AirEvent::Override { mode: OverrideMode::Soft, .. })) => {
                    // A pending state push wins (it is retried anyway; a
                    // resume/stop already re-asks the pull).
                    if pending.is_none() {
                        pending = Some(Push::Flush);
                    }
                }
                Some(Some(AirEvent::Override { id, mode: OverrideMode::Hard })) => {
                    cut_in(&ls, &bridge, id).await;
                }
                Some(Some(AirEvent::HardMark { at })) => {
                    cut_in_at_clock(&ls, &bridge, at).await;
                }
            }
        }
    })
}

/// Hard override `id`: resolve it now, cut it in (see [`cut`]).
async fn cut_in(ls: &LsControl, bridge: &LsBridge, id: u64) {
    let prepared_override = bridge.prepared_is_override();
    let Some(uri) = bridge.interrupt_uri(id).await else {
        return; // nothing to cut in with (logged by the bridge)
    };
    match cut(ls, &uri, prepared_override).await {
        Ok(()) => tracing::info!(id, "hard override cut in"),
        Err(e) => tracing::error!(
            id,
            error = %e,
            "hard override could NOT be cut in (Liquidsoap unreachable): consumed, not aired"
        ),
    }
}

/// `AtClock` hard rendez-vous `at`: resolve it now — the engine decides
/// whether it must still cut — and cut it in (see [`cut`]).
async fn cut_in_at_clock(ls: &LsControl, bridge: &LsBridge, at: Epoch) {
    let prepared_override = bridge.prepared_is_override();
    let Some(uri) = bridge.at_clock_uri(at).await else {
        return; // no cut: stays soft-eligible (logged by the engine)
    };
    match cut(ls, &uri, prepared_override).await {
        Ok(()) => tracing::info!(mark = at.0, "AtClock hard cut in"),
        Err(e) => tracing::error!(
            mark = at.0,
            error = %e,
            "AtClock hard could NOT be cut in (Liquidsoap unreachable): occurrence consumed, not aired"
        ),
    }
}

/// Cut `uri` in NOW. The prepared track is flushed first (so the pull
/// re-asks after the insert: rest of a multi-track override, or the grid) —
/// unless it is itself an override, which then plays after the insert.
async fn cut(ls: &LsControl, uri: &str, prepared_override: bool) -> Result<(), LsControlError> {
    if !prepared_override {
        if let Err(e) = ls.command("stationd.flush").await {
            tracing::warn!(error = %e, "flush before the cut failed");
        }
    }
    ls.command(&format!("stationd.interrupt {uri}")).await.map(|_| ())
}

/// The `AtClock` **hard** timer: sleeps until the next hard rendez-vous
/// (`GridEngine::next_hard_mark`, re-planned at least every
/// [`TICK_MAX`]: a new grid or a `clock set` is seen within a minute) and
/// sends [`AirEvent::HardMark`] to the air sync task, which cuts it in if it
/// still must. Each mark is sent once. Woken on the system clock (to the
/// second, sub-second corrected); with a frozen station clock (`clock set`)
/// a mark equal to the frozen instant fires within a minute.
pub fn spawn_at_clock_ticker(
    engine: GridEngine,
    tx: mpsc::UnboundedSender<AirEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_sent: Option<Epoch> = None;
        loop {
            let now = engine.effective_now(None);
            let next = match engine.next_hard_mark(now).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(error = %e, "AtClock hard: could not plan the next mark");
                    None
                }
            };
            let wait = match next {
                Some(mark) if mark.0 <= now.0 => {
                    if last_sent != Some(mark) {
                        last_sent = Some(mark);
                        if tx.send(AirEvent::HardMark { at: mark }).is_err() {
                            break; // air sync gone: daemon shutting down
                        }
                    }
                    Duration::from_secs(1)
                }
                Some(mark) => {
                    let secs = Duration::from_secs((mark.0 - now.0) as u64);
                    // The station clock is whole seconds: remove the part of
                    // the current second already elapsed (real clock only).
                    let elapsed = if engine.clock_override().is_none() {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| Duration::from_nanos(d.subsec_nanos() as u64))
                            .unwrap_or_default()
                    } else {
                        Duration::ZERO
                    };
                    secs.saturating_sub(elapsed).min(TICK_MAX)
                }
                None => TICK_MAX,
            };
            tokio::time::sleep(wait).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    /// A fake Liquidsoap server: answers each command with `reply` + END and
    /// records what it received.
    fn fake_ls(dir: &std::path::Path, reply: &'static str) -> (PathBuf, Arc<Mutex<Vec<String>>>) {
        let path = dir.join("ls.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let seen = seen2.clone();
                tokio::spawn(async move {
                    let (rd, mut wr) = stream.into_split();
                    let mut lines = BufReader::new(rd).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        if l == "quit" {
                            break;
                        }
                        seen.lock().unwrap().push(l);
                        let _ = wr.write_all(format!("{reply}\r\nEND\r\n").as_bytes()).await;
                    }
                });
            }
        });
        (path, seen)
    }

    async fn bridge() -> (tempfile::TempDir, LsBridge) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        let eng = crate::grid_engine::GridEngine::new(pool, "UTC");
        (dir, LsBridge::new(eng, std::path::Path::new("/m")).unwrap())
    }

    async fn wait_for(seen: &Arc<Mutex<Vec<String>>>, n: usize) -> Vec<String> {
        for _ in 0..100 {
            if seen.lock().unwrap().len() >= n {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        seen.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn air_sync_follows_the_state_machine() {
        use crate::station_control::{ControlAction, StationControl};
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_ls(dir.path(), "OK");
        let (_d, bridge) = bridge().await;
        let control = StationControl::new_in_memory();
        let (tx, rx) = mpsc::unbounded_channel();
        control.attach_air(tx);
        spawn_air_sync(rx, LsControl::new(path), bridge, control.state());
        // start-up re-assert of `running`
        assert_eq!(wait_for(&seen, 1).await, ["stationd.resume"]);
        control.apply(ControlAction::Pause, "cli").unwrap();
        control.apply(ControlAction::Resume, "cli").unwrap();
        control.apply(ControlAction::Stop, "cli").unwrap(); // graceful, prepared track dropped
        control.apply(ControlAction::Resume, "cli").unwrap();
        assert_eq!(
            wait_for(&seen, 5).await,
            ["stationd.resume", "stationd.pause", "stationd.resume", "stationd.flush", "stationd.resume"]
        );
    }

    #[tokio::test]
    async fn overrides_flush_or_cut_in() {
        use crate::station_control::{OverrideContent, OverrideRequest};
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_ls(dir.path(), "OK");
        let (_d, bridge) = bridge().await;
        let control = bridge_control(&bridge);
        let (tx, rx) = mpsc::unbounded_channel();
        control.attach_air(tx);
        spawn_air_sync(rx, LsControl::new(path), bridge, control.state());
        assert_eq!(wait_for(&seen, 1).await, ["stationd.resume"]);
        let req = |p: &str, mode| OverrideRequest {
            content: OverrideContent::Media(p.into()),
            mode,
            expiry: None,
            tracks: None,
        };
        control.push_override(req("jingles/a.mp3", OverrideMode::Soft), "cli").unwrap();
        assert_eq!(wait_for(&seen, 2).await[1], "stationd.flush");
        control.push_override(req("news/flash.mp3", OverrideMode::Hard), "cli").unwrap();
        let got = wait_for(&seen, 4).await;
        assert_eq!(got[2], "stationd.flush", "prepared grid track re-asked");
        assert!(
            got[3].starts_with("stationd.interrupt annotate:stationd_rid=")
                && got[3].ends_with(":/m/news/flash.mp3"),
            "{got:?}"
        );
        // the hard one was consumed, the soft one waits for the pull
        assert_eq!(control.list_overrides().len(), 1);
    }

    #[tokio::test]
    async fn a_prepared_override_is_never_flushed() {
        use crate::station_control::{ControlAction, OverrideContent, OverrideRequest};
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_ls(dir.path(), "OK");
        let (_d, bridge) = bridge().await;
        let control = bridge_control(&bridge);
        let (tx, rx) = mpsc::unbounded_channel();
        control.attach_air(tx);
        spawn_air_sync(rx, LsControl::new(path), bridge.clone(), control.state());
        assert_eq!(wait_for(&seen, 1).await, ["stationd.resume"]);
        let req = |p: &str, mode| OverrideRequest {
            content: OverrideContent::Media(p.into()),
            mode,
            expiry: None,
            tracks: None,
        };
        // An override is handed out by a pull: it is the prepared track.
        control.push_override(req("jingles/a.mp3", OverrideMode::Soft), "cli").unwrap();
        assert_eq!(wait_for(&seen, 2).await[1], "stationd.flush");
        assert_eq!(bridge.next().await.kind, "file");
        assert!(bridge.prepared_is_override());
        // A newer soft, a hard and a stop: none of them flushes it.
        control.push_override(req("jingles/b.mp3", OverrideMode::Soft), "cli").unwrap();
        control.push_override(req("news/flash.mp3", OverrideMode::Hard), "cli").unwrap();
        let got = wait_for(&seen, 3).await;
        assert!(got[2].starts_with("stationd.interrupt "), "{got:?}");
        control.apply(ControlAction::Stop, "cli").unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.iter().filter(|c| *c == "stationd.flush").count(), 1, "{got:?}");
    }

    /// Engine with a `music` floor and a `news` AtClock HARD every 15 min.
    async fn hard_engine() -> (tempfile::TempDir, GridEngine) {
        use crate::resolver::{ClockAnchor, Mode, Rule, RuleKind, Validity};
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        let m = |p: &str| crate::media::ScannedMedia {
            rel_path: p.into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 60_000,
            size_bytes: 1,
            mtime_ns: 0,
        };
        crate::media_index::replace_library(&pool, &[m("music/a.mp3"), m("news/n.mp3")], 1000).await.unwrap();
        for r in ["music", "news"] {
            let toml = format!(
                "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                 [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{r}/\"\n"
            );
            let pl = crate::playlist::Playlist::parse(&toml).unwrap();
            crate::store::upsert(&pool, r, &pl, &toml, Some(r)).await.unwrap();
        }
        let rule = |id: &str, kind| Rule { id: id.into(), enabled: true, validity: Validity::default(), kind };
        crate::grid_index::insert_rule(&pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "music".into() }))
            .await
            .unwrap();
        crate::grid_index::insert_rule(
            &pool,
            &rule(
                "news",
                RuleKind::AtClock {
                    playlist_ref: "news".into(),
                    anchor: ClockAnchor::EveryMinutes(15),
                    mode: Mode::Hard,
                    expiry_secs: Some(600),
                },
            ),
        )
        .await
        .unwrap();
        (dir, GridEngine::new(pool, "UTC"))
    }

    #[tokio::test]
    async fn the_ticker_cuts_a_hard_mark_in_once() {
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_ls(dir.path(), "OK");
        let (_d, eng) = hard_engine().await;
        let bridge = LsBridge::new(eng.clone(), std::path::Path::new("/m")).unwrap();
        let control = eng.control().clone();
        let (tx, rx) = mpsc::unbounded_channel();
        control.attach_air(tx.clone());
        spawn_air_sync(rx, LsControl::new(path), bridge, control.state());
        assert_eq!(wait_for(&seen, 1).await, ["stationd.resume"]);
        // The station clock stands on a hard mark (09:15): cut in now.
        eng.set_clock(Some(Epoch(9 * 3600 + 15 * 60)));
        let ticker = spawn_at_clock_ticker(eng.clone(), tx);
        let got = wait_for(&seen, 3).await;
        assert_eq!(got[1], "stationd.flush", "prepared grid track re-asked after the insert");
        assert!(
            got[2].starts_with("stationd.interrupt annotate:stationd_rid=")
                && got[2].ends_with(":/m/news/n.mp3"),
            "{got:?}"
        );
        // Sent once: the ticker keeps running, nothing more is cut.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(seen.lock().unwrap().len(), 3, "{:?}", seen.lock().unwrap());
        ticker.abort();
    }

    #[tokio::test]
    async fn a_hard_mark_on_a_paused_station_is_not_cut() {
        use crate::station_control::ControlAction;
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_ls(dir.path(), "OK");
        let (_d, eng) = hard_engine().await;
        let bridge = LsBridge::new(eng.clone(), std::path::Path::new("/m")).unwrap();
        let control = eng.control().clone();
        let (tx, rx) = mpsc::unbounded_channel();
        control.attach_air(tx.clone());
        spawn_air_sync(rx, LsControl::new(path), bridge, control.state());
        assert_eq!(wait_for(&seen, 1).await, ["stationd.resume"]);
        control.apply(ControlAction::Pause, "cli").unwrap();
        assert_eq!(wait_for(&seen, 2).await[1], "stationd.pause");
        eng.set_clock(Some(Epoch(9 * 3600 + 15 * 60)));
        tx.send(AirEvent::HardMark { at: Epoch(9 * 3600 + 15 * 60) }).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(seen.lock().unwrap().len(), 2, "no flush, no interrupt");
    }

    fn bridge_control(b: &LsBridge) -> crate::station_control::StationControl {
        b.engine_control()
    }

    #[tokio::test]
    async fn a_failed_push_is_retried_until_liquidsoap_answers() {
        use crate::station_control::{ControlAction, StationControl};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ls.sock");
        let (_d, bridge) = bridge().await;
        let control = StationControl::new_in_memory();
        let (tx, rx) = mpsc::unbounded_channel();
        control.attach_air(tx);
        let ls = LsControl::new(path.clone());
        spawn_air_sync(rx, ls.clone(), bridge, control.state());
        control.apply(ControlAction::Pause, "cli").unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(ls.health().last_error.is_some(), "no socket yet: failure recorded");
        // Liquidsoap comes up: the pending pause lands on the next retry.
        let listener_dir = dir.path().to_path_buf();
        let (_p, seen) = fake_ls(&listener_dir, "OK");
        tokio::time::sleep(RETRY_EVERY + Duration::from_millis(100)).await;
        assert_eq!(wait_for(&seen, 1).await, ["stationd.pause"]);
        assert!(ls.health().last_error.is_none());
    }

    #[test]
    fn pause_resume_and_stop_flush_are_pushed() {
        use BroadcastState::*;
        let t = |from, to| Transition { from, to };
        assert_eq!(command_for(&t(Running, Paused)), Some("stationd.pause"));
        assert_eq!(command_for(&t(Paused, Running)), Some("stationd.resume"));
        assert_eq!(command_for(&t(Stopped, Running)), Some("stationd.resume"));
        assert_eq!(command_for(&t(Running, Stopped)), Some("stationd.flush"), "stop drops the prepared track");
        assert_eq!(command_for(&t(Draining, Stopped)), Some("stationd.flush"));
        assert_eq!(command_for(&t(Running, Draining)), None);
    }

    #[tokio::test]
    async fn command_round_trip_and_health() {
        let dir = tempfile::tempdir().unwrap();
        let (path, seen) = fake_ls(dir.path(), "OK");
        let ls = LsControl::new(path);
        assert_eq!(ls.command("stationd.skip").await.unwrap(), "OK");
        assert_eq!(seen.lock().unwrap().as_slice(), ["stationd.skip"]);
        let h = ls.health();
        assert_eq!(h.last_ok.unwrap().0, "stationd.skip");
        assert!(h.last_error.is_none());
    }

    #[tokio::test]
    async fn unreachable_or_refused_is_an_error_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let ls = LsControl::new(dir.path().join("absent.sock"));
        assert!(matches!(ls.command("stationd.pause").await, Err(LsControlError::Io { .. })));
        assert!(ls.health().last_error.is_some());

        let dir2 = tempfile::tempdir().unwrap();
        let (path, _) = fake_ls(dir2.path(), "ERROR: unknown command");
        let ls = LsControl::new(path);
        assert!(matches!(ls.command("stationd.pause").await, Err(LsControlError::Refused { .. })));
    }
}
