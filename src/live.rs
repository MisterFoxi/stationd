//! Live DJs: who may connect (the DJ file), when (the grid's `live` rules),
//! and the live session itself.
//!
//! Liquidsoap's harbor asks stationd about everything through the loopback
//! bridge (`ls_bridge`, routes `/ls/v1/live/*`); Liquidsoap decides nothing:
//!
//! - **auth** — `user` / `password` / client address. Allowed only if the DJ
//!   is in the DJ file (enabled, argon2 hash matches), no live is already on
//!   air, the DJ's window is open in the grid (`resolver::live_window`: from
//!   the rule's `start` until the next slot starts), and the DJ was not cut
//!   for silence (or kicked) earlier in this same window. A DJ software that
//!   cannot set a user name connects as `source` with `dj,password` as the
//!   password (not `dj:password`: Liquidsoap's harbor cuts a password at its
//!   first `:` — seen on 2.2.4 — so a password never contains `:` nor `,`).
//! - **connected** — the harbor took the stream: the live starts
//!   (`LiveStarted`, `StationControl::set_live` — no hard cut while a DJ holds
//!   the air).
//! - **silence** — `[live] silence_timeout` seconds of silence: the live ends
//!   (the DJ is disconnected through the control socket, `AirEvent::LiveKick`)
//!   and this DJ is refused until the end of its window. `stationctl live
//!   kick` does the same.
//! - **disconnected** — whatever the cause: the live is over (`LiveEnded`);
//!   the programme resumes with a track chosen at that instant.
//!
//! The DJ file (`[live] djs_path`) is re-read at every connection attempt: a
//! DJ added or a password changed applies without a restart. It holds only
//! argon2 hashes (PHC strings, `stationctl dj hash`), never a password.
//!
//! Session state is in memory: after a stationd restart a DJ still connected
//! stays on air (Liquidsoap keeps it) but is unknown here until it leaves.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::Deserialize;

use crate::grid_engine::GridEngine;
use crate::plugin::PluginEvent;
use crate::resolver::Epoch;
use crate::station_control::AirEvent;

/// How long an accepted login waits for the harbor's connection hook.
const PENDING_TTL_S: i64 = 30;
/// Bans (DJ + window occurrence) remembered, oldest dropped first.
const BAN_CAP: usize = 64;

// ---------------------------------------------------------------------------
// The DJ file
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DjsDoc {
    schema_version: u32,
    #[serde(default, rename = "dj")]
    djs: Vec<DjDoc>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DjDoc {
    id: String,
    #[serde(default)]
    name: Option<String>,
    password_hash: String,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

/// One DJ of the DJ file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dj {
    /// What the grid's `live` rules and the harbor login name.
    pub id: String,
    /// Display name (optional).
    pub name: Option<String>,
    /// Argon2 PHC string.
    pub password_hash: String,
    pub enabled: bool,
}

/// Parse and check a DJ file. Strict, like every TOML of stationd: unknown
/// field, wrong `schema_version`, a duplicate or malformed id, or a hash that
/// is not an argon2 PHC string are loud errors.
pub fn parse_djs(text: &str) -> Result<Vec<Dj>, String> {
    let doc: DjsDoc = toml::from_str(text).map_err(|e| e.to_string())?;
    if doc.schema_version != 1 {
        return Err(format!("unsupported schema_version {} (expected 1)", doc.schema_version));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(doc.djs.len());
    for d in doc.djs {
        let id = d.id.trim().to_string();
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
            return Err(format!("dj {:?}: id must be letters, digits, '-', '_', '.'", d.id));
        }
        if !seen.insert(id.clone()) {
            return Err(format!("duplicate dj id {id:?}"));
        }
        let ok = PasswordHash::new(&d.password_hash).is_ok_and(|h| h.algorithm.as_str().starts_with("argon2"));
        if !ok {
            return Err(format!(
                "dj {id:?}: password_hash is not an argon2 hash (generate one with `stationctl dj hash`)"
            ));
        }
        out.push(Dj { id, name: d.name, password_hash: d.password_hash, enabled: d.enabled });
    }
    Ok(out)
}

/// Read and parse the DJ file.
pub fn load_djs(path: &Path) -> Result<Vec<Dj>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("DJ file {}: {e}", path.display()))?;
    parse_djs(&text).map_err(|e| format!("DJ file {}: {e}", path.display()))
}

/// Hash a password for the DJ file (argon2id, random salt, PHC string).
/// CPU-heavy (tens of ms): call it off the async runtime.
pub fn hash_password(password: &str) -> Result<String, String> {
    if password.is_empty() {
        return Err("empty password".into());
    }
    if password.contains([':', ',']) {
        // Liquidsoap's harbor cuts a password at its first ':' (Basic auth
        // split); the `source` + "dj,password" login form splits on ','.
        return Err("a DJ password must not contain ':' or ','".into());
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// The DJ id and password of a harbor login. A software that cannot set a
/// user name sends the default `source`: its password then reads
/// `dj,password`.
pub fn split_login(user: &str, password: &str) -> (String, String) {
    if user.is_empty() || user == "source" {
        if let Some((dj, pw)) = password.split_once(',') {
            return (dj.to_string(), pw.to_string());
        }
    }
    (user.to_string(), password.to_string())
}

// ---------------------------------------------------------------------------
// The live session
// ---------------------------------------------------------------------------

/// A DJ on air.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    pub dj: String,
    /// The `live` rule whose window let the DJ in (empty if unknown).
    pub rule_id: String,
    /// That window's occurrence (`resolver::LiveWindow::occurrence`).
    pub occurrence: String,
    /// Client address, as the harbor saw it.
    pub address: String,
    pub since: Epoch,
}

/// Why a live ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Disconnected,
    Silence,
    Kicked,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::Disconnected => "disconnected",
            EndReason::Silence => "silence",
            EndReason::Kicked => "kicked",
        }
    }
}

/// The last live that ended (for `stationctl live status`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedLive {
    pub session: LiveSession,
    pub reason: EndReason,
    pub at: Epoch,
}

/// The answer to a harbor login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    Allowed { dj: String, rule_id: String },
    Refused(String),
}

impl AuthOutcome {
    pub fn allowed(&self) -> bool {
        matches!(self, AuthOutcome::Allowed { .. })
    }
}

/// Snapshot for `stationctl live status`.
#[derive(Debug, Clone, Default)]
pub struct LiveStatus {
    pub on_air: Option<LiveSession>,
    pub last: Option<EndedLive>,
    /// (dj, window occurrence) refused for the rest of that window.
    pub refused: Vec<(String, String)>,
    /// The last refused login (dj as sent, reason, when).
    pub last_refusal: Option<(String, String, Epoch)>,
}

#[derive(Debug, Default)]
struct State {
    /// Accepted by `auth`, waiting for the harbor's connection hook.
    pending: Option<LiveSession>,
    session: Option<LiveSession>,
    /// Set by `silence` / `kick`: the reason the coming disconnection gets.
    ending: Option<EndReason>,
    banned: VecDeque<(String, String)>,
    last: Option<EndedLive>,
    last_refusal: Option<(String, String, Epoch)>,
}

#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    #[error("no DJ on air")]
    NoLive,
}

/// The live runtime: one per station, shared by the bridge (harbor hooks)
/// and the gRPC service. Cheap to clone.
#[derive(Clone)]
pub struct LiveHub {
    djs_path: PathBuf,
    engine: GridEngine,
    inner: Arc<Mutex<State>>,
}

impl LiveHub {
    pub fn new(djs_path: impl Into<PathBuf>, engine: GridEngine) -> Self {
        Self { djs_path: djs_path.into(), engine, inner: Arc::new(Mutex::new(State::default())) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn now(&self) -> Epoch {
        self.engine.effective_now(None)
    }

    pub fn djs_path(&self) -> &Path {
        &self.djs_path
    }

    /// A harbor login: allowed or refused (with the reason, logged — never
    /// the password).
    pub async fn auth(&self, user: &str, password: &str, address: &str) -> AuthOutcome {
        let (dj, password) = split_login(user, password);
        let outcome = self.decide(&dj, password, address).await;
        match &outcome {
            AuthOutcome::Allowed { rule_id, .. } => {
                tracing::info!(%dj, %address, rule = %rule_id, "live: DJ login accepted")
            }
            AuthOutcome::Refused(why) => {
                tracing::warn!(%dj, %address, reason = %why, "live: DJ login refused");
                self.lock().last_refusal = Some((dj.clone(), why.clone(), self.now()));
            }
        }
        outcome
    }

    async fn decide(&self, dj: &str, password: String, address: &str) -> AuthOutcome {
        let refused = |s: &str| AuthOutcome::Refused(s.to_string());
        let path = self.djs_path.clone();
        let djs = match tokio::task::spawn_blocking(move || load_djs(&path)).await {
            Ok(Ok(djs)) => djs,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "live: DJ file unreadable, every login refused");
                return refused("DJ file unreadable");
            }
            Err(e) => return AuthOutcome::Refused(format!("internal: {e}")),
        };
        let Some(entry) = djs.into_iter().find(|d| d.id == dj) else {
            return refused("bad credentials");
        };
        let hash = entry.password_hash.clone();
        let good = tokio::task::spawn_blocking(move || verify_password(&hash, &password))
            .await
            .unwrap_or(false);
        if !good {
            return refused("bad credentials");
        }
        if !entry.enabled {
            return refused("DJ disabled in the DJ file");
        }
        if let Some(on_air) = &self.lock().session {
            return AuthOutcome::Refused(format!("a live is already on air ({})", on_air.dj));
        }
        let now = self.now();
        let window = match self.engine.live_window(dj, now).await {
            Ok(Some(w)) => w,
            Ok(None) => return refused("outside the DJ's slot"),
            Err(e) => {
                tracing::error!(error = %e, "live: grid unreadable, login refused");
                return refused("grid unreadable");
            }
        };
        let key = (dj.to_string(), window.occurrence.clone());
        let mut st = self.lock();
        if st.banned.contains(&key) {
            return refused("cut earlier in this slot (silence or kick): refused until the next slot");
        }
        st.pending = Some(LiveSession {
            dj: dj.to_string(),
            rule_id: window.rule_id.clone(),
            occurrence: window.occurrence,
            address: address.to_string(),
            since: now,
        });
        AuthOutcome::Allowed { dj: dj.to_string(), rule_id: window.rule_id }
    }

    /// The harbor took the DJ's stream: the live starts.
    pub fn connected(&self) {
        let now = self.now();
        let session = {
            let mut st = self.lock();
            let pending = st.pending.take().filter(|p| now.0 - p.since.0 <= PENDING_TTL_S);
            let session = match pending {
                Some(p) => LiveSession { since: now, ..p },
                None => {
                    tracing::warn!("live: harbor connection without a matching login");
                    LiveSession {
                        dj: "?".into(),
                        rule_id: String::new(),
                        occurrence: String::new(),
                        address: String::new(),
                        since: now,
                    }
                }
            };
            st.session = Some(session.clone());
            st.ending = None;
            session
        };
        tracing::info!(dj = %session.dj, rule = %session.rule_id, address = %session.address, "live: DJ on air");
        let control = self.engine.control();
        control.set_live(Some(session.dj.clone()));
        control.emit_event(PluginEvent::LiveStarted {
            dj: session.dj,
            rule_id: session.rule_id,
            at: now.0,
        });
    }

    /// The DJ left the harbor (whatever the cause): the live is over.
    /// Returns the session that ended, if one was known.
    pub fn disconnected(&self) -> Option<EndedLive> {
        let now = self.now();
        let ended = {
            let mut st = self.lock();
            let reason = st.ending.take().unwrap_or(EndReason::Disconnected);
            let ended = st.session.take().map(|session| EndedLive { session, reason, at: now });
            if let Some(e) = &ended {
                st.last = Some(e.clone());
            }
            ended
        };
        let control = self.engine.control();
        control.set_live(None);
        match &ended {
            Some(e) => {
                tracing::info!(dj = %e.session.dj, reason = e.reason.as_str(), "live: over, back to the programme");
                control.emit_event(PluginEvent::LiveEnded {
                    dj: e.session.dj.clone(),
                    reason: e.reason.as_str().to_string(),
                    at: now.0,
                });
            }
            None => tracing::info!("live: harbor disconnection (no live known: stationd restarted meanwhile?)"),
        }
        ended
    }

    /// `[live] silence_timeout` of silence on the live input: end it.
    pub fn silence(&self) {
        match self.end(EndReason::Silence) {
            Ok(dj) => tracing::warn!(%dj, "live: silence, DJ disconnected and refused until the next slot"),
            Err(_) => tracing::info!("live: silence reported with no live on air, ignored"),
        }
    }

    /// `stationctl live kick`: end the live now. Returns the DJ.
    pub fn kick(&self) -> Result<String, LiveError> {
        let dj = self.end(EndReason::Kicked)?;
        tracing::warn!(%dj, "live: DJ kicked, refused until the next slot");
        Ok(dj)
    }

    /// Ban the DJ for the rest of its window and ask Liquidsoap to disconnect
    /// it; the end itself is recorded by the disconnection hook.
    fn end(&self, reason: EndReason) -> Result<String, LiveError> {
        let dj = {
            let mut st = self.lock();
            let session = st.session.clone().ok_or(LiveError::NoLive)?;
            st.ending = Some(reason);
            if !session.occurrence.is_empty() {
                let key = (session.dj.clone(), session.occurrence);
                if !st.banned.contains(&key) {
                    st.banned.push_back(key);
                    while st.banned.len() > BAN_CAP {
                        st.banned.pop_front();
                    }
                }
            }
            session.dj
        };
        self.engine.control().send_air(AirEvent::LiveKick);
        Ok(dj)
    }

    pub fn status(&self) -> LiveStatus {
        let st = self.lock();
        LiveStatus {
            on_air: st.session.clone(),
            last: st.last.clone(),
            refused: st.banned.iter().cloned().collect(),
            last_refusal: st.last_refusal.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid_index::insert_rule;
    use crate::resolver::{Rule, RuleKind, Validity, WallClock};
    use tokio::sync::mpsc;

    fn djs_file(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("djs.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    fn marc_file(dir: &Path, enabled: bool) -> PathBuf {
        let hash = hash_password("s3cret").unwrap();
        djs_file(
            dir,
            &format!(
                "schema_version = 1\n[[dj]]\nid = \"marc\"\nname = \"Marc\"\npassword_hash = \"{hash}\"\nenabled = {enabled}\n"
            ),
        )
    }

    /// A hub over a grid with marc's slot at 20:00 and a day part at 23:00
    /// (UTC station); returns the air receiver to see the kicks.
    async fn hub(enabled: bool) -> (tempfile::TempDir, LiveHub, mpsc::UnboundedReceiver<AirEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        let rule = |id: &str, kind| Rule { id: id.into(), enabled: true, validity: Validity::default(), kind };
        insert_rule(&pool, &rule("marc-live", RuleKind::Live { dj: "marc".into(), start: WallClock { hour: 20, minute: 0 } }))
            .await
            .unwrap();
        insert_rule(
            &pool,
            &rule(
                "night",
                RuleKind::DayPart { playlist_ref: "nuit".into(), start: WallClock { hour: 23, minute: 0 }, end: None },
            ),
        )
        .await
        .unwrap();
        let path = marc_file(dir.path(), enabled);
        let engine = GridEngine::new(pool, "UTC");
        let (tx, rx) = mpsc::unbounded_channel();
        engine.control().attach_air(tx);
        let hub = LiveHub::new(path, engine);
        (dir, hub, rx)
    }

    impl LiveHub {
        fn at(&self, h: i64, m: i64) {
            // 2026-09-25 (a Friday), UTC
            self.engine.set_clock(Some(Epoch(1_790_294_400 + h * 3600 + m * 60)));
        }
    }

    #[test]
    fn the_dj_file_is_strict() {
        let hash = hash_password("pw").unwrap();
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        let ok = format!("schema_version = 1\n[[dj]]\nid = \"marc\"\npassword_hash = \"{hash}\"\n");
        let djs = parse_djs(&ok).unwrap();
        assert_eq!(djs[0].id, "marc");
        assert!(djs[0].enabled, "enabled by default");
        assert!(verify_password(&djs[0].password_hash, "pw"));
        assert!(!verify_password(&djs[0].password_hash, "PW"));
        for (bad, want) in [
            (ok.replace("schema_version = 1", "schema_version = 2"), "schema_version"),
            (format!("{ok}color = 1\n"), "unknown field"),
            (ok.replace(&hash, "plain-text"), "not an argon2 hash"),
            (ok.replace("\"marc\"", "\"marc dj\""), "id must be"),
            (format!("{ok}[[dj]]\nid = \"marc\"\npassword_hash = \"{hash}\"\n"), "duplicate"),
        ] {
            let err = parse_djs(&bad).unwrap_err();
            assert!(err.contains(want), "{want}: {err}");
        }
        assert!(parse_djs("schema_version = 1\n").unwrap().is_empty());
        assert!(hash_password("a:b").is_err());
        assert!(hash_password("a,b").is_err());
        assert!(hash_password("").is_err());
    }

    #[test]
    fn a_login_without_user_carries_it_in_the_password() {
        assert_eq!(split_login("marc", "pw"), ("marc".into(), "pw".into()));
        assert_eq!(split_login("source", "marc,pw"), ("marc".into(), "pw".into()));
        assert_eq!(split_login("", "marc,pw"), ("marc".into(), "pw".into()));
        assert_eq!(split_login("source", "pw"), ("source".into(), "pw".into()));
    }

    #[tokio::test]
    async fn a_dj_gets_in_only_with_the_right_password_inside_its_slot() {
        let (_d, hub, _rx) = hub(true).await;
        hub.at(19, 59);
        assert_eq!(hub.auth("marc", "s3cret", "1.2.3.4").await, AuthOutcome::Refused("outside the DJ's slot".into()));
        hub.at(20, 30);
        assert_eq!(hub.auth("marc", "nope", "1.2.3.4").await, AuthOutcome::Refused("bad credentials".into()));
        assert_eq!(hub.auth("julie", "s3cret", "1.2.3.4").await, AuthOutcome::Refused("bad credentials".into()));
        assert!(hub.status().last_refusal.is_some());
        let ok = hub.auth("source", "marc,s3cret", "1.2.3.4").await;
        assert_eq!(ok, AuthOutcome::Allowed { dj: "marc".into(), rule_id: "marc-live".into() });
        // the slot lasts until the next one: still open at 22:59, closed at 23:00
        hub.at(22, 59);
        assert!(hub.auth("marc", "s3cret", "x").await.allowed());
        hub.at(23, 0);
        assert!(!hub.auth("marc", "s3cret", "x").await.allowed());
    }

    #[tokio::test]
    async fn a_disabled_dj_is_refused() {
        let (_d, hub, _rx) = hub(false).await;
        hub.at(20, 30);
        assert_eq!(
            hub.auth("marc", "s3cret", "x").await,
            AuthOutcome::Refused("DJ disabled in the DJ file".into())
        );
    }

    #[tokio::test]
    async fn the_live_holds_the_air_until_the_dj_leaves_then_may_come_back() {
        let (_d, hub, _rx) = hub(true).await;
        hub.at(20, 30);
        assert!(hub.auth("marc", "s3cret", "1.2.3.4").await.allowed());
        hub.connected();
        let on = hub.status().on_air.unwrap();
        assert_eq!((on.dj.as_str(), on.rule_id.as_str(), on.address.as_str()), ("marc", "marc-live", "1.2.3.4"));
        assert_eq!(hub.engine.control().live_dj().as_deref(), Some("marc"));
        // a second login while on air
        let busy = hub.auth("marc", "s3cret", "5.6.7.8").await;
        assert!(matches!(&busy, AuthOutcome::Refused(r) if r.contains("already on air")), "{busy:?}");
        // the DJ stays on air past the end of its window
        hub.at(23, 30);
        assert!(hub.status().on_air.is_some());
        let ended = hub.disconnected().unwrap();
        assert_eq!(ended.reason, EndReason::Disconnected);
        assert!(hub.engine.control().live_dj().is_none());
        assert_eq!(hub.status().last.unwrap().session.dj, "marc");
        // a plain disconnection does not ban: back in within the window
        hub.at(21, 0);
        assert!(hub.auth("marc", "s3cret", "1.2.3.4").await.allowed());
    }

    #[tokio::test]
    async fn silence_ends_the_live_and_refuses_the_dj_until_the_next_slot() {
        let (_d, hub, mut rx) = hub(true).await;
        hub.at(20, 30);
        assert!(hub.auth("marc", "s3cret", "x").await.allowed());
        hub.connected();
        hub.silence();
        assert_eq!(rx.try_recv().unwrap(), AirEvent::LiveKick);
        let ended = hub.disconnected().unwrap();
        assert_eq!(ended.reason, EndReason::Silence);
        assert_eq!(hub.status().refused, vec![("marc".to_string(), "marc-live@2026-09-25T20:00".to_string())]);
        hub.at(21, 0);
        let again = hub.auth("marc", "s3cret", "x").await;
        assert!(matches!(&again, AuthOutcome::Refused(r) if r.contains("cut earlier in this slot")), "{again:?}");
        // the next day's slot is a new occurrence
        hub.engine.set_clock(Some(Epoch(1_790_294_400 + 86_400 + 20 * 3600 + 60)));
        assert!(hub.auth("marc", "s3cret", "x").await.allowed());
        // silence with nothing on air: nothing sent
        hub.silence();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_kick_ends_the_live_like_silence() {
        let (_d, hub, mut rx) = hub(true).await;
        assert!(matches!(hub.kick(), Err(LiveError::NoLive)));
        hub.at(20, 30);
        assert!(hub.auth("marc", "s3cret", "x").await.allowed());
        hub.connected();
        assert_eq!(hub.kick().unwrap(), "marc");
        assert_eq!(rx.try_recv().unwrap(), AirEvent::LiveKick);
        assert_eq!(hub.disconnected().unwrap().reason, EndReason::Kicked);
        assert!(!hub.auth("marc", "s3cret", "x").await.allowed());
    }

    #[tokio::test]
    async fn an_unreadable_dj_file_refuses_everyone() {
        let (d, hub, _rx) = hub(true).await;
        std::fs::write(d.path().join("djs.toml"), "schema_version = 1\n[[dj]]\nid = \"marc\"\n").unwrap();
        hub.at(20, 30);
        assert_eq!(hub.auth("marc", "s3cret", "x").await, AuthOutcome::Refused("DJ file unreadable".into()));
    }

    #[tokio::test]
    async fn a_live_on_air_degrades_hard_overrides_to_soft() {
        use crate::station_control::{OverrideContent, OverrideMode, OverrideRequest};
        let (_d, hub, _rx) = hub(true).await;
        hub.at(20, 30);
        assert!(hub.auth("marc", "s3cret", "x").await.allowed());
        hub.connected();
        let out = hub
            .engine
            .control()
            .push_override(
                OverrideRequest {
                    content: OverrideContent::Media("news/flash.mp3".into()),
                    mode: OverrideMode::Hard,
                    expiry: None,
                    tracks: None,
                },
                "cli",
            )
            .unwrap();
        assert!(out.degraded);
    }
}
