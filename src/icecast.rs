//! Icecast — read-only access to the admin API (`/admin/stats`).
//!
//! stationd stays the only component that talks to Icecast (never exposed);
//! here it only *reads*. A periodic sampler (`spawn_sampler`) fetches the
//! stats, keeps the mounts stationd feeds (its `[[liquidsoap.output]]`) and:
//!
//! - **audience**: the sum of their listeners → `StationControl::
//!   sample_listeners` → `ListenersSampled` (stop-when-idle becomes real);
//! - **unknown ≠ 0**: Icecast unreachable, bad credentials, unreadable stats,
//!   one of our mounts absent or without a source → the audience is
//!   *unknown* (`clear_listeners`), never a zero. A failure must not stop the
//!   station.
//!
//! The last stats and the last error are kept in an [`IcecastMonitor`]
//! (mount health for `stationctl icecast status`).
//!
//! Split for testability: `parse_stats` / `audience` are pure,
//! `IcecastClient` does the HTTP, `apply_sample` the effects.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;

use crate::config::IcecastConfig;
use crate::resolver::Epoch;
use crate::station_control::StationControl;

/// Whole request (connect + response + body) budget.
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);
/// `/admin/stats` is a few KiB per mount; anything past this is not Icecast.
const MAX_BODY: usize = 2 * 1024 * 1024;
/// Icecast refreshes `total_bytes_read` only every ~5 s (seen on 2.4.4): a
/// rate between two reads is off by up to ±5 s of data. Averaged over a
/// sliding window instead — the error stays under 10 %.
pub const RATE_WINDOW_S: i64 = 60;
/// A rate is shown once the window spans at least this much.
pub const RATE_MIN_SPAN_S: i64 = 30;

// ---------------------------------------------------------------------------
// Stats model + parser (pure)
// ---------------------------------------------------------------------------

/// One `<source mount="…">` of `/admin/stats`. Every field is optional: the
/// parser records what Icecast says and judges nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceStats {
    pub mount: String,
    pub listeners: Option<u32>,
    pub listener_peak: Option<u32>,
    /// `stream_start_iso8601`, else `stream_start` (as sent).
    pub stream_start: Option<String>,
    /// Nominal bitrate as announced by the source (kbit/s): `<bitrate>`
    /// (2.4), else `bitrate=` in `<audio_info>` (2.5 has only the latter).
    pub bitrate: Option<String>,
    pub content_type: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub total_bytes_read: Option<u64>,
    pub total_bytes_sent: Option<u64>,
    pub source_ip: Option<String>,
    /// Source client (`Liquidsoap/2.4.0+dev …`).
    pub user_agent: Option<String>,
}

impl SourceStats {
    /// A source client is feeding this mount. Icecast stamps
    /// `stream_start*` when a source connects; a node without it is a
    /// leftover (configured mount, fallback) with nobody feeding it.
    pub fn connected(&self) -> bool {
        self.stream_start.is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IcecastStats {
    /// `server_id` (e.g. "Icecast 2.5.0").
    pub server_id: Option<String>,
    pub sources: Vec<SourceStats>,
}

impl IcecastStats {
    pub fn source(&self, mount: &str) -> Option<&SourceStats> {
        self.sources.iter().find(|s| s.mount == mount)
    }
}

/// Parse `/admin/stats`. Loud on anything that is not an `<icestats>`
/// document or on a malformed number — never a silent default.
pub fn parse_stats(xml: &str) -> Result<IcecastStats, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("stats are not XML: {e}"))?;
    let root = doc.root_element();
    // Element names are compared by local name: Icecast 2.5 puts
    // `<icestats>` in a namespace (legacystats-0.0.1).
    let root = match local(root) {
        "icestats" => root,
        // Icecast 2.5 « report XML »: an error (incident) or, possibly, the
        // legacy stats wrapped in an extension.
        "report" => {
            if let Some(why) = report_incident(root) {
                return Err(format!("Icecast refused: {why}"));
            }
            root.descendants()
                .find(|n| local(*n) == "icestats")
                .ok_or("Icecast <report> without incident nor <icestats>")?
        }
        other => return Err(format!("unexpected root <{other}> (expected <icestats>)")),
    };
    let mut stats = IcecastStats {
        server_id: child_text(root, "server_id"),
        sources: Vec::new(),
    };
    for node in root.children().filter(|n| local(*n) == "source") {
        let mount = node
            .attribute("mount")
            .ok_or("a <source> has no mount attribute")?
            .to_string();
        let num = |name: &str| -> Result<Option<u64>, String> {
            child_text(node, name)
                .map(|t| t.parse::<u64>().map_err(|_| format!("{mount}: <{name}> {t:?} is not a number")))
                .transpose()
        };
        let small = |name: &str| -> Result<Option<u32>, String> {
            num(name)?
                .map(|v| u32::try_from(v).map_err(|_| format!("{mount}: <{name}> {v} out of range")))
                .transpose()
        };
        stats.sources.push(SourceStats {
            listeners: small("listeners")?,
            listener_peak: small("listener_peak")?,
            stream_start: child_text(node, "stream_start_iso8601").or_else(|| child_text(node, "stream_start")),
            bitrate: child_text(node, "bitrate").or_else(|| {
                child_text(node, "audio_info").and_then(|ai| {
                    ai.split(';')
                        .find_map(|kv| kv.trim().strip_prefix("bitrate="))
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty())
                })
            }),
            content_type: child_text(node, "server_type").or_else(|| child_text(node, "content-type")),
            title: child_text(node, "title"),
            artist: child_text(node, "artist"),
            total_bytes_read: num("total_bytes_read")?,
            total_bytes_sent: num("total_bytes_sent")?,
            source_ip: child_text(node, "source_ip"),
            user_agent: child_text(node, "user_agent"),
            mount,
        });
    }
    Ok(stats)
}

/// Local (namespace-free) element name; "" for a non-element.
fn local<'a>(n: roxmltree::Node<'a, '_>) -> &'a str {
    if n.is_element() { n.tag_name().name() } else { "" }
}

/// Text of a 2.5 report's `<incident>` (`<state><text>…`), if any.
fn report_incident(report: roxmltree::Node) -> Option<String> {
    let incident = report.descendants().find(|n| local(*n) == "incident")?;
    Some(
        incident
            .descendants()
            .find(|n| local(*n) == "text")
            .and_then(|n| n.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "incident without text".to_string()),
    )
}

/// Trimmed text of the first direct child `name`; empty = absent.
fn child_text(node: roxmltree::Node, name: &str) -> Option<String> {
    node.children()
        .find(|n| local(*n) == name)
        .and_then(|n| n.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Audience of *our* mounts: the sum of their listeners. Any mount absent,
/// without a source, or without a listener count makes the whole audience
/// unknown (`Err`, the reason) — a partial sum would under-count.
pub fn audience(stats: &IcecastStats, mounts: &[String]) -> Result<u32, String> {
    let mut total: u32 = 0;
    for m in mounts {
        let s = stats.source(m).ok_or_else(|| format!("mount {m} absent from Icecast"))?;
        if !s.connected() {
            return Err(format!("mount {m}: no source connected"));
        }
        let n = s.listeners.ok_or_else(|| format!("mount {m}: no listener count"))?;
        total = total.saturating_add(n);
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

/// Minimal admin client: `GET /admin/stats` with HTTP Basic auth, plain HTTP
/// (Icecast is reached directly, never through the reverse proxy).
#[derive(Clone)]
pub struct IcecastClient {
    http: Client<HttpConnector, Empty<Bytes>>,
    authority: String,
    auth: String,
}

impl IcecastClient {
    pub fn new(cfg: &IcecastConfig) -> Result<Self, String> {
        let creds = format!("{}:{}", cfg.admin_user, cfg.admin_password);
        Ok(Self {
            http: Client::builder(TokioExecutor::new()).build_http(),
            authority: cfg.authority()?,
            auth: format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(creds)),
        })
    }

    /// `host:port` queried.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Fetch and parse `/admin/stats`. Every failure is an `Err` with a
    /// human reason (unreachable, 401, HTTP status, timeout, parse).
    pub async fn stats(&self) -> Result<IcecastStats, String> {
        let body = tokio::time::timeout(FETCH_TIMEOUT, self.get("/admin/stats"))
            .await
            .map_err(|_| format!("{}: no answer within {FETCH_TIMEOUT:?}", self.authority))??;
        parse_stats(&body)
    }

    async fn get(&self, path: &str) -> Result<String, String> {
        let uri = format!("http://{}{path}", self.authority);
        let req = hyper::Request::get(&uri)
            .header(hyper::header::AUTHORIZATION, &self.auth)
            .header(hyper::header::USER_AGENT, concat!("stationd/", env!("CARGO_PKG_VERSION")))
            .body(Empty::<Bytes>::new())
            .map_err(|e| format!("{uri}: {e}"))?;
        let resp = self
            .http
            .request(req)
            .await
            .map_err(|e| format!("{}: unreachable ({})", self.authority, error_chain(&e)))?;
        let status = resp.status();
        if status == hyper::StatusCode::UNAUTHORIZED {
            return Err(format!("{uri}: 401 — admin_user/admin_password refused"));
        }
        if !status.is_success() {
            return Err(format!("{uri}: HTTP {status}"));
        }
        let body = Limited::new(resp.into_body(), MAX_BODY)
            .collect()
            .await
            .map_err(|e| format!("{uri}: body: {e}"))?
            .to_bytes();
        String::from_utf8(body.to_vec()).map_err(|_| format!("{uri}: body is not UTF-8"))
    }
}

/// `e: source: source…` — hyper's top-level error alone is uninformative.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

// ---------------------------------------------------------------------------
// Monitor (shared health) + sampler
// ---------------------------------------------------------------------------

/// What the sampler last saw. Read by `stationctl icecast status`.
#[derive(Debug, Clone, Default)]
pub struct IcecastHealth {
    /// Last successful read of `/admin/stats`, and what it said.
    pub last_ok: Option<(Epoch, IcecastStats)>,
    /// Last problem (fetch failure, or audience unknown), with its instant.
    /// Cleared by a fully successful sample.
    pub problem: Option<(Epoch, String)>,
    /// Audience of our mounts at the last sample (`None` = unknown).
    pub audience: Option<u32>,
    /// Measured rate received from each source: (kbit/s, window in s) —
    /// growth of `total_bytes_read` over the last [`RATE_WINDOW_S`]. Absent
    /// until the window spans [`RATE_MIN_SPAN_S`] (e.g. right after a
    /// (re)connection). 0 while connected = a stalled source.
    pub read_rate: HashMap<String, (f64, u32)>,
    /// Per mount: stream_start and the (instant, total_bytes_read) samples
    /// of the window, oldest first.
    history: HashMap<String, RateWindow>,
}

/// `stream_start` of the measured source + its (instant, total_bytes_read)
/// samples, oldest first.
type RateWindow = (Option<String>, VecDeque<(Epoch, u64)>);

impl IcecastHealth {
    /// Record a successful read: slide each mount's window, derive the
    /// rates, keep the stats. A counter that went backwards or a new
    /// `stream_start` (source reconnected) restarts the window.
    fn record(&mut self, at: Epoch, stats: IcecastStats) {
        let mut rates = HashMap::new();
        let mut history = HashMap::new();
        for s in &stats.sources {
            let Some(bytes) = s.total_bytes_read else { continue };
            let (start, mut win) = self
                .history
                .remove(&s.mount)
                .unwrap_or_else(|| (s.stream_start.clone(), VecDeque::new()));
            let continues = start == s.stream_start
                && win.back().is_none_or(|&(t, b)| t.0 < at.0 && b <= bytes);
            if !continues {
                win.clear();
            }
            win.push_back((at, bytes));
            // Keep one sample at or beyond the window's start.
            while win.len() > 1 && at.0 - win[1].0 .0 >= RATE_WINDOW_S {
                win.pop_front();
            }
            let (t0, b0) = win[0];
            let span = at.0 - t0.0;
            if span >= RATE_MIN_SPAN_S {
                let kbps = (bytes - b0) as f64 * 8.0 / 1000.0 / span as f64;
                rates.insert(s.mount.clone(), (kbps, span as u32));
            }
            history.insert(s.mount.clone(), (s.stream_start.clone(), win));
        }
        self.read_rate = rates;
        self.history = history;
        self.last_ok = Some((at, stats));
    }
}

#[derive(Clone, Default)]
pub struct IcecastMonitor {
    inner: Arc<Mutex<IcecastHealth>>,
}

impl IcecastMonitor {
    pub fn snapshot(&self) -> IcecastHealth {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, IcecastHealth> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Apply one fetch result: record it, then feed the audience (or forget it).
/// Returns the problem, if any (the caller logs changes only).
pub fn apply_sample(
    fetched: Result<IcecastStats, String>,
    mounts: &[String],
    control: &StationControl,
    monitor: &IcecastMonitor,
) -> Option<String> {
    let at = control.now();
    let result = match fetched {
        Err(e) => Err(e),
        Ok(stats) => {
            let a = audience(&stats, mounts);
            monitor.lock().record(at, stats);
            a
        }
    };
    match result {
        Ok(n) => {
            {
                let mut h = monitor.lock();
                h.audience = Some(n);
                h.problem = None;
            }
            control.sample_listeners(n);
            None
        }
        Err(reason) => {
            {
                let mut h = monitor.lock();
                h.audience = None;
                h.problem = Some((at, reason.clone()));
            }
            control.clear_listeners();
            Some(reason)
        }
    }
}

/// Sample every `every` (first sample right away). Logs a problem when it
/// appears or changes, and the recovery — not every failed poll.
pub fn spawn_sampler(
    client: IcecastClient,
    mounts: Vec<String>,
    every: Duration,
    control: StationControl,
    monitor: IcecastMonitor,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_problem: Option<String> = None;
        loop {
            tick.tick().await;
            let fetched = client.stats().await;
            let problem = apply_sample(fetched, &mounts, &control, &monitor);
            match (&last_problem, &problem) {
                (_, Some(p)) if last_problem.as_ref() != Some(p) => {
                    tracing::warn!(problem = %p, "Icecast: audience unknown (a draining station keeps playing)");
                }
                (Some(_), None) => {
                    tracing::info!(listeners = ?control.listeners(), "Icecast: audience sampled again");
                }
                _ => {}
            }
            last_problem = problem;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station_control::{ControlAction, Gate};

    /// Synthetic 2.4-shaped stats with two mounts (sums, `<bitrate>`,
    /// artist). Real 2.5 captures: `AIRING_2_5`, `EMPTY_2_5`.
    const STATS: &str = r#"<?xml version="1.0"?>
<icestats>
  <admin>icemaster@localhost</admin>
  <client_connections>42</client_connections>
  <host>localhost</host>
  <listeners>3</listeners>
  <server_id>Icecast 2.5.0</server_id>
  <sources>2</sources>
  <source mount="/radio.mp3">
    <audio_info>channels=2;samplerate=44100;bitrate=192</audio_info>
    <bitrate>192</bitrate>
    <genre>various</genre>
    <listener_peak>5</listener_peak>
    <listeners>2</listeners>
    <listenurl>http://localhost:8000/radio.mp3</listenurl>
    <server_name>My Radio</server_name>
    <server_type>audio/mpeg</server_type>
    <source_ip>127.0.0.1</source_ip>
    <stream_start>Thu, 24 Sep 2026 15:02:11 +0200</stream_start>
    <stream_start_iso8601>2026-09-24T15:02:11+0200</stream_start_iso8601>
    <title>Artiste - Titre é</title>
    <total_bytes_read>1234567</total_bytes_read>
    <total_bytes_sent>2345678</total_bytes_sent>
  </source>
  <source mount="/other.ogg">
    <listeners>1</listeners>
    <stream_start_iso8601>2026-09-24T14:00:00+0200</stream_start_iso8601>
  </source>
</icestats>"#;

    fn mounts(m: &[&str]) -> Vec<String> {
        m.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_sources_and_their_fields() {
        let s = parse_stats(STATS).unwrap();
        assert_eq!(s.server_id.as_deref(), Some("Icecast 2.5.0"));
        assert_eq!(s.sources.len(), 2);
        let r = s.source("/radio.mp3").unwrap();
        assert_eq!(r.listeners, Some(2));
        assert_eq!(r.listener_peak, Some(5));
        assert_eq!(r.stream_start.as_deref(), Some("2026-09-24T15:02:11+0200"));
        assert_eq!(r.bitrate.as_deref(), Some("192"));
        assert_eq!(r.content_type.as_deref(), Some("audio/mpeg"));
        assert_eq!(r.title.as_deref(), Some("Artiste - Titre é"));
        assert_eq!(r.total_bytes_read, Some(1_234_567));
        assert!(r.connected());
    }

    /// Verbatim answer of Icecast 2.5 (devstationd) to a refused
    /// `/admin/stats`.
    const REFUSED_2_5: &str = r#"<?xml version="1.0"?>
<report xmlns="http://icecast.org/specs/reportxml-0.0.1" version="0.0.1"><extension application="http://icecast.org/specs/legacy-icestats"><icestats xmlns="http://icecast.org/specs/legacystats-0.0.1"><modules/></icestats></extension><incident><state definition="25387198-0643-4577-9139-7c4f24f59d4a"><text>You need to authenticate</text></state></incident></report>"#;

    /// Verbatim `/admin/stats` of Icecast 2.5.0 (devstationd, 2026-09-24),
    /// Liquidsoap 2.4 airing on /radio.mp3.
    const AIRING_2_5: &str = r#"<?xml version="1.0"?>
<icestats><modules/><authentication><role id="0" type="static" name="legacy-admin" management-url="/admin/manageauth.xsl?id=0" can-adduser="false" can-deleteuser="false" can-listuser="true"/><role id="1" type="static" name="legacy-relay" management-url="/admin/manageauth.xsl?id=1" can-adduser="false" can-deleteuser="false" can-listuser="true"/><role id="2" type="anonymous" name="anonymous" can-adduser="false" can-deleteuser="false" can-listuser="false"/></authentication><admin>icemaster@localhost</admin><client_connections>31</client_connections><clients>1</clients><connections>43</connections><file_connections>0</file_connections><host>localhost</host><instance_uuid>157d238a-1d96-4878-9bc4-5ed087983bd7</instance_uuid><listener_connections>0</listener_connections><listeners>0</listeners><location>Earth</location><server_id>Icecast 2.5.0</server_id><server_start>Thu, 24 Sep 2026 09:49:35 +0000</server_start><server_start_iso8601>2026-09-24T09:49:35+0000</server_start_iso8601><source_client_connections>8</source_client_connections><source_relay_connections>0</source_relay_connections><source_total_connections>8</source_total_connections><sources>1</sources><stats>0</stats><stats_connections>0</stats_connections><source mount="/radio.mp3"><allow-direct-access>true</allow-direct-access><audio_info>channels=2;samplerate=44100;bitrate=192</audio_info><display-title>Tuxis - DiversTuxis - the_iron_trinity</display-title><genre>various</genre><instance_uuid>2ff4099f-a2db-483e-ad98-418f4b9353ae</instance_uuid><listener_peak>0</listener_peak><listeners>0</listeners><listenurl>http://127.0.0.1:8000/radio.mp3</listenurl><max_listeners>unlimited</max_listeners><public>0</public><server_description>Unspecified description</server_description><server_name>Ma Radio</server_name><server_type>audio/mpeg</server_type><slow_listeners>0</slow_listeners><source_ip>127.0.0.1</source_ip><stream_start>Thu, 24 Sep 2026 16:05:09 +0000</stream_start><stream_start_iso8601>2026-09-24T16:05:09+0000</stream_start_iso8601><title>Tuxis - DiversTuxis - the_iron_trinity</title><total_bytes_read>2144800</total_bytes_read><total_bytes_sent>0</total_bytes_sent><user_agent>Liquidsoap/2.4.0+dev (Unix; OCaml 5.4.0)</user_agent><playlist version="1" xmlns="http://xspf.org/ns/0/"><trackList><track/><track><title>Tuxis - DiversTuxis - the_iron_trinity</title></track></trackList></playlist><metadata><x_icy_title>Tuxis - DiversTuxis - the_iron_trinity</x_icy_title></metadata><content-type>audio/mpeg</content-type><authentication><role id="3" type="static" name="legacy-global-source" management-url="/admin/manageauth.xsl?id=3" can-adduser="false" can-deleteuser="false" can-listuser="true"/></authentication></source></icestats>"#;

    #[test]
    fn real_2_5_stats_while_airing() {
        let s = parse_stats(AIRING_2_5).unwrap();
        assert_eq!(s.sources.len(), 1);
        let r = s.source("/radio.mp3").unwrap();
        assert!(r.connected());
        assert_eq!(r.stream_start.as_deref(), Some("2026-09-24T16:05:09+0000"));
        assert_eq!(r.listeners, Some(0));
        assert_eq!(r.listener_peak, Some(0));
        assert_eq!(r.bitrate.as_deref(), Some("192"), "2.5: from audio_info");
        assert_eq!(r.content_type.as_deref(), Some("audio/mpeg"));
        // Direct child only (the XSPF <playlist> nests another <title>).
        assert_eq!(r.title.as_deref(), Some("Tuxis - DiversTuxis - the_iron_trinity"));
        assert_eq!(r.artist, None, "mp3/ICY: one string, no separate artist");
        assert_eq!(r.total_bytes_read, Some(2_144_800));
        assert_eq!(r.source_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(r.user_agent.as_deref(), Some("Liquidsoap/2.4.0+dev (Unix; OCaml 5.4.0)"));
        assert_eq!(audience(&s, &mounts(&["/radio.mp3"])), Ok(0));
    }

    /// Verbatim `/admin/stats` of Icecast 2.5.0 (devstationd, 2026-09-24),
    /// captured while no source was connected (`<sources>0</sources>`).
    const EMPTY_2_5: &str = r#"<?xml version="1.0"?>
<icestats><modules/><authentication><role id="0" type="static" name="legacy-admin" management-url="/admin/manageauth.xsl?id=0" can-adduser="false" can-deleteuser="false" can-listuser="true"/><role id="1" type="static" name="legacy-relay" management-url="/admin/manageauth.xsl?id=1" can-adduser="false" can-deleteuser="false" can-listuser="true"/><role id="2" type="anonymous" name="anonymous" can-adduser="false" can-deleteuser="false" can-listuser="false"/></authentication><admin>icemaster@localhost</admin><client_connections>20</client_connections><clients>0</clients><connections>31</connections><file_connections>0</file_connections><host>localhost</host><instance_uuid>157d238a-1d96-4878-9bc4-5ed087983bd7</instance_uuid><listener_connections>0</listener_connections><listeners>0</listeners><location>Earth</location><server_id>Icecast 2.5.0</server_id><server_start>Thu, 24 Sep 2026 09:49:35 +0000</server_start><server_start_iso8601>2026-09-24T09:49:35+0000</server_start_iso8601><source_client_connections>7</source_client_connections><source_relay_connections>0</source_relay_connections><source_total_connections>7</source_total_connections><sources>0</sources><stats>0</stats><stats_connections>0</stats_connections></icestats>"#;

    #[test]
    fn real_2_5_stats_without_source_make_the_audience_unknown() {
        let s = parse_stats(EMPTY_2_5).unwrap();
        assert_eq!(s.server_id.as_deref(), Some("Icecast 2.5.0"));
        assert!(s.sources.is_empty());
        // Server-wide <listeners>0</listeners> is NOT our audience: our
        // mount has no source → unknown, never 0.
        assert!(audience(&s, &mounts(&["/radio.mp3"])).unwrap_err().contains("absent"));
    }

    #[test]
    fn a_2_5_report_incident_is_an_error_with_its_text() {
        let e = parse_stats(REFUSED_2_5).unwrap_err();
        assert_eq!(e, "Icecast refused: You need to authenticate");
    }

    #[test]
    fn namespaced_or_wrapped_icestats_are_read() {
        // 2.5 namespaces <icestats>; a report may wrap it (no incident).
        let ns = STATS.replace("<icestats>", r#"<icestats xmlns="http://icecast.org/specs/legacystats-0.0.1">"#);
        assert_eq!(parse_stats(&ns).unwrap(), parse_stats(STATS).unwrap());
        let body = ns.trim_start_matches(r#"<?xml version="1.0"?>"#);
        let wrapped = format!(
            r#"<report xmlns="http://icecast.org/specs/reportxml-0.0.1" version="0.0.1"><extension application="http://icecast.org/specs/legacy-icestats">{body}</extension></report>"#
        );
        assert_eq!(parse_stats(&wrapped).unwrap(), parse_stats(STATS).unwrap());
    }

    #[test]
    fn malformed_stats_are_loud() {
        assert!(parse_stats("not xml").is_err());
        assert!(parse_stats("<html><body>Not Found</body></html>").unwrap_err().contains("icestats"));
        let bad = STATS.replace("<listeners>2</listeners>", "<listeners>two</listeners>");
        assert!(parse_stats(&bad).unwrap_err().contains("listeners"));
        assert!(parse_stats("<icestats><source/></icestats>").unwrap_err().contains("mount"));
    }

    #[test]
    fn audience_sums_only_our_mounts() {
        let s = parse_stats(STATS).unwrap();
        assert_eq!(audience(&s, &mounts(&["/radio.mp3"])), Ok(2));
        assert_eq!(audience(&s, &mounts(&["/radio.mp3", "/other.ogg"])), Ok(3));
    }

    #[test]
    fn audience_is_unknown_when_a_mount_is_absent_or_unfed() {
        let s = parse_stats(STATS).unwrap();
        assert!(audience(&s, &mounts(&["/radio.mp3", "/gone.mp3"])).unwrap_err().contains("absent"));
        let unfed = STATS
            .replace("<stream_start>Thu, 24 Sep 2026 15:02:11 +0200</stream_start>", "")
            .replace("<stream_start_iso8601>2026-09-24T15:02:11+0200</stream_start_iso8601>", "");
        let s = parse_stats(&unfed).unwrap();
        assert!(audience(&s, &mounts(&["/radio.mp3"])).unwrap_err().contains("no source"));
    }

    #[test]
    fn a_failure_forgets_the_audience_and_never_stops_a_drain() {
        let control = StationControl::new_in_memory();
        let monitor = IcecastMonitor::default();
        let ours = mounts(&["/radio.mp3"]);
        let zero = parse_stats(&STATS.replace("<listeners>2</listeners>", "<listeners>0</listeners>")).unwrap();

        assert_eq!(apply_sample(Ok(zero.clone()), &ours, &control, &monitor), None);
        assert_eq!(control.listeners(), Some(0));
        assert_eq!(monitor.snapshot().audience, Some(0));

        control.apply(ControlAction::StopWhenIdle, "cli").unwrap();
        let p = apply_sample(Err("127.0.0.1:8000: unreachable".into()), &ours, &control, &monitor);
        assert!(p.unwrap().contains("unreachable"));
        assert_eq!(control.listeners(), None);
        let h = monitor.snapshot();
        assert_eq!(h.audience, None);
        assert!(h.problem.is_some());
        assert!(h.last_ok.is_some(), "the last good stats are kept");
        assert_eq!(control.gate(), Gate::Play, "unknown audience never completes a drain");

        // Mount without a source: stats readable, audience still unknown.
        let unfed = parse_stats("<icestats><source mount=\"/radio.mp3\"><listeners>0</listeners></source></icestats>").unwrap();
        assert!(apply_sample(Ok(unfed), &ours, &control, &monitor).unwrap().contains("no source"));
        assert_eq!(control.gate(), Gate::Play);

        // Recovery.
        assert_eq!(apply_sample(Ok(zero), &ours, &control, &monitor), None);
        assert!(monitor.snapshot().problem.is_none());
        assert_eq!(control.gate(), Gate::Halt(crate::station_control::BroadcastState::Stopped));
    }

    /// Icecast refreshes its counter every ~5 s: two reads 15 s apart may
    /// see 10 or 20 s of data. Over 60 s the step is diluted.
    #[test]
    fn a_counter_step_is_diluted_by_the_window() {
        let control = StationControl::new_in_memory();
        let monitor = IcecastMonitor::default();
        let ours = mounts(&["/radio.mp3"]);
        let mut last = None;
        for (i, t) in (1_000..=1_060).step_by(15).enumerate() {
            // True rate 24 000 B/s, but the counter lags by 0 or 5 s.
            let lag = if i % 2 == 1 { 5 } else { 0 };
            let bytes = 24_000 * (t - 1_000 - lag) as u64;
            control.set_clock(Some(Epoch(t)));
            let xml = AIRING_2_5.replace("<total_bytes_read>2144800<", &format!("<total_bytes_read>{bytes}<"));
            apply_sample(Ok(parse_stats(&xml).unwrap()), &ours, &control, &monitor);
            last = monitor.snapshot().read_rate.get("/radio.mp3").copied();
        }
        let (kbps, _) = last.unwrap();
        assert!((kbps - 192.0).abs() / 192.0 < 0.10, "{kbps} within 10 %");
    }

    #[test]
    fn read_rate_is_averaged_over_a_sliding_window() {
        let control = StationControl::new_in_memory();
        let monitor = IcecastMonitor::default();
        let ours = mounts(&["/radio.mp3"]);
        let at_bytes = |bytes: u64| {
            parse_stats(&AIRING_2_5.replace("<total_bytes_read>2144800<", &format!("<total_bytes_read>{bytes}<"))).unwrap()
        };
        let read = |t: i64, bytes: u64| {
            control.set_clock(Some(Epoch(t)));
            apply_sample(Ok(at_bytes(bytes)), &ours, &control, &monitor);
            monitor.snapshot().read_rate.get("/radio.mp3").copied()
        };
        // 192 kbit/s = 24 000 B/s.
        assert_eq!(read(1_000, 1_000_000), None, "one read: no rate yet");
        assert_eq!(read(1_015, 1_000_000 + 24_000 * 15), None, "15 s: window too short");
        assert_eq!(read(1_030, 1_000_000 + 24_000 * 30), Some((192.0, 30)));
        // The window slides: never more than 60 s.
        read(1_045, 1_000_000 + 24_000 * 45);
        read(1_060, 1_000_000 + 24_000 * 60);
        assert_eq!(read(1_075, 1_000_000 + 24_000 * 75), Some((192.0, 60)));
        // Stalled source: counter frozen for a whole window → 0.
        for t in [1_090, 1_105, 1_120] {
            read(t, 1_000_000 + 24_000 * 75);
        }
        assert_eq!(read(1_135, 1_000_000 + 24_000 * 75), Some((0.0, 60)));
        // Source reconnected (new stream_start, counter reset): measure restarts.
        control.set_clock(Some(Epoch(1_150)));
        let reconnected = parse_stats(
            &AIRING_2_5
                .replace("2026-09-24T16:05:09+0000", "2026-09-24T16:30:00+0000")
                .replace("<total_bytes_read>2144800<", "<total_bytes_read>50000<"),
        )
        .unwrap();
        apply_sample(Ok(reconnected), &ours, &control, &monitor);
        assert!(monitor.snapshot().read_rate.is_empty());
    }

    fn cfg(url: &str, password: &str) -> IcecastConfig {
        IcecastConfig {
            admin_url: url.into(),
            admin_user: "admin".into(),
            admin_password: password.into(),
            poll_interval: 15,
            server: None,
        }
    }

    /// A fake Icecast: `/admin/stats` behind Basic auth admin:good.
    async fn fake_icecast() -> std::net::SocketAddr {
        use axum::{http::HeaderMap, http::StatusCode, routing::get, Router};
        let expected = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("admin:good"));
        let app = Router::new().route(
            "/admin/stats",
            get(move |h: HeaderMap| {
                let ok = h.get("authorization").and_then(|v| v.to_str().ok()) == Some(expected.as_str());
                async move {
                    if ok {
                        (StatusCode::OK, STATS.to_string())
                    } else {
                        (StatusCode::UNAUTHORIZED, String::new())
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    #[tokio::test]
    async fn client_fetches_with_basic_auth() {
        let addr = fake_icecast().await;
        let c = IcecastClient::new(&cfg(&format!("http://{addr}"), "good")).unwrap();
        let s = c.stats().await.unwrap();
        assert_eq!(s.source("/radio.mp3").unwrap().listeners, Some(2));
        let bad = IcecastClient::new(&cfg(&format!("http://{addr}/"), "bad")).unwrap();
        assert!(bad.stats().await.unwrap_err().contains("401"));
    }

    #[tokio::test]
    async fn client_reports_an_unreachable_icecast() {
        // Bind then drop: nothing listens on that port any more.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = IcecastClient::new(&cfg(&format!("http://127.0.0.1:{port}"), "good")).unwrap();
        assert!(c.stats().await.unwrap_err().contains("unreachable"));
    }
}
