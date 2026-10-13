//! Public, read-only player. One bounded Watch RPC per station, never per visitor.
//! Explicit DTOs prevent internal media paths and scheduler diagnostics leaking.

use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Redirect, Response,
    },
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use stationd_proto::onair;
use std::{
    collections::{BTreeMap, HashSet},
    convert::Infallible,
    net::{SocketAddr, ToSocketAddrs},
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

const NAME: &str = "station-player";
const STALE_SECONDS: i64 = 60;
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Theme {
    accent: String,
    background: String,
    foreground: String,
}
impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: "#a3e635".into(),
            background: "#111827".into(),
            foreground: "#f9fafb".into(),
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Station {
    slug: String,
    label: String,
    #[serde(default)]
    description: String,
    stream_url: String,
    grpc_endpoint: String,
    #[serde(default)]
    allow_plaintext_grpc: bool,
    #[serde(default)]
    theme: Theme,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_bind")]
    bind: String,
    stations: Vec<Station>,
    #[serde(default = "default_streams")]
    max_event_streams: usize,
}
fn default_bind() -> String {
    "127.0.0.1:8091".into()
}
fn default_streams() -> usize {
    128
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn text(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect()
}

enum BindTarget {
    Address(SocketAddr),
    Hostname(String, u16),
}

/// Validate syntax without DNS or sockets during configuration inspection.
fn parse_bind(value: &str) -> Result<BindTarget, String> {
    if let Ok(address) = value.parse::<SocketAddr>() {
        return if address.port() == 0 {
            Err("bind port must be between 1 and 65535".into())
        } else {
            Ok(BindTarget::Address(address))
        };
    }
    let (host, port_text) = value
        .rsplit_once(':')
        .ok_or("bind requires IP:port or hostname:port; bracket IPv6 addresses")?;
    let port: u16 = port_text
        .parse()
        .ok()
        .filter(|port| *port > 0)
        .filter(|_| port_text.bytes().all(|b| b.is_ascii_digit()))
        .ok_or("bind port must be between 1 and 65535")?;
    let name = host.strip_suffix('.').unwrap_or(host);
    if name.is_empty()
        || name.len() > 253
        || !name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err("bind host must be an IP or ASCII DNS hostname (IDNs use punycode)".into());
    }
    Ok(BindTarget::Hostname(host.into(), port))
}

fn bind_listener(value: &str) -> Result<std::net::TcpListener, String> {
    let addresses = match parse_bind(value)? {
        BindTarget::Address(address) => vec![address],
        BindTarget::Hostname(host, port) => {
            // DNS must not hold the synchronous native on_load indefinitely.
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            thread::Builder::new()
                .name("station-player-dns".into())
                .spawn(move || {
                    let result = (host.as_str(), port)
                        .to_socket_addrs()
                        .map(|addresses| addresses.take(16).collect::<Vec<_>>());
                    let _ = tx.send(result);
                })
                .map_err(|e| format!("{NAME}: cannot start DNS lookup: {e}"))?;
            rx.recv_timeout(Duration::from_secs(3))
                .map_err(|e| format!("{NAME}: DNS lookup for bind {value}: {e}"))?
                .map_err(|e| format!("{NAME}: cannot resolve bind {value}: {e}"))?
        }
    };
    if addresses.is_empty() {
        return Err(format!("{NAME}: bind {value} resolved to no addresses"));
    }
    // Try all returned addresses, including IPv4/IPv6 alternatives. The one
    // successful listener must belong to an interface on this machine.
    std::net::TcpListener::bind(addresses.as_slice())
        .map_err(|e| format!("{NAME}: cannot bind {value}: {e}"))
}

fn url(value: &str) -> Result<axum::http::Uri, String> {
    let uri: axum::http::Uri = value.parse().map_err(|_| "invalid URL")?;
    if value.len() > 2048
        || value.contains(['\\', '#'])
        || !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.host().is_none_or(str::is_empty)
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.port().is_some_and(|p| p.as_u16() == 0)
    {
        return Err("expected an HTTP(S) URL without credentials or fragment".into());
    }
    Ok(uri)
}
impl Config {
    fn parse(table: &toml::Table) -> Result<Self, String> {
        let config: Self = toml::Value::Table(table.clone())
            .try_into()
            .map_err(|e| format!("{NAME}: {e}"))?;
        config.validate().map_err(|e| format!("{NAME}: {e}"))?;
        Ok(config)
    }
    fn validate(&self) -> Result<(), String> {
        parse_bind(&self.bind)?;
        if !(1..=64).contains(&self.stations.len()) || !(1..=4096).contains(&self.max_event_streams)
        {
            return Err(
                "stations must contain 1..64 entries and max_event_streams must be 1..4096".into(),
            );
        }
        let mut slugs = HashSet::new();
        for station in &self.stations {
            if station.slug.is_empty()
                || station.slug.len() > 64
                || !station
                    .slug
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || !slugs.insert(&station.slug)
            {
                return Err("station slugs must be unique, 1..64 lowercase ASCII letters, digits or hyphens".into());
            }
            if station.label.trim().is_empty()
                || station.label.chars().count() > 128
                || station.description.chars().count() > 512
                || station
                    .label
                    .chars()
                    .chain(station.description.chars())
                    .any(char::is_control)
            {
                return Err("invalid station label or description".into());
            }
            url(&station.stream_url)?;
            let endpoint = url(&station.grpc_endpoint)?;
            if !matches!(endpoint.path(), "" | "/") || endpoint.query().is_some() {
                return Err("grpc_endpoint must be an origin".into());
            }
            let host = endpoint.host().unwrap_or_default().trim_matches(['[', ']']);
            if endpoint.scheme_str() == Some("http")
                && !station.allow_plaintext_grpc
                && host != "localhost"
                && !host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            {
                return Err("remote gRPC requires HTTPS or explicit allow_plaintext_grpc on a trusted network".into());
            }
            for color in [
                &station.theme.accent,
                &station.theme.background,
                &station.theme.foreground,
            ] {
                if color.len() != 7
                    || !color.starts_with('#')
                    || !color[1..].bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("theme colors must use #RRGGBB".into());
                }
            }
        }
        Ok(())
    }
}
pub fn validate_config(table: &toml::Table) -> Result<(), String> {
    Config::parse(table).map(|_| ())
}

#[derive(Clone, Debug, Default, Serialize)]
struct Track {
    title: String,
    artist: String,
    album: String,
    duration_ms: Option<u64>,
    started_at: Option<i64>,
    estimated_at: Option<i64>,
    stream: bool,
}
impl From<&onair::Track> for Track {
    fn from(track: &onair::Track) -> Self {
        Self {
            title: text(&track.title),
            artist: text(&track.artist),
            album: text(&track.album),
            duration_ms: track.duration_ms.filter(|n| *n > 0),
            started_at: track.started_at,
            estimated_at: track.estimated_at,
            stream: track.stream,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
struct Agenda {
    label: String,
    at: Option<i64>,
    at_local: String,
}
impl From<&onair::PlaylistSlot> for Agenda {
    fn from(slot: &onair::PlaylistSlot) -> Self {
        Self {
            label: text(&slot.playlist_ref),
            at: slot.from,
            at_local: text(&slot.at_local),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize)]
struct Air {
    revision: u64,
    observed_at: Option<i64>,
    state: String,
    kind: String,
    timezone: String,
    now_playing: Option<Track>,
    coming_next: Vec<Track>,
    indicative: Vec<Track>,
    agenda: Vec<Agenda>,
    indicative_agenda: Vec<Agenda>,
    history: Vec<Track>,
}
impl From<&onair::OnAirSnapshot> for Air {
    fn from(snapshot: &onair::OnAirSnapshot) -> Self {
        Self {
            revision: snapshot.revision,
            observed_at: Some(snapshot.observed_at),
            state: text(&snapshot.state),
            kind: text(&snapshot.on_air_kind),
            timezone: text(&snapshot.timezone),
            // Without Liquidsoap the core's on_air may be illustrative, not an actual air start.
            now_playing: snapshot
                .on_air
                .as_ref()
                .filter(|_| snapshot.liquidsoap)
                .map(Track::from),
            // Only the prefetched item is committed to the real scheduler/LS queue.
            coming_next: snapshot
                .prefetched
                .iter()
                .filter(|_| snapshot.liquidsoap)
                .map(Track::from)
                .collect(),
            indicative: snapshot.upcoming.iter().take(29).map(Track::from).collect(),
            agenda: snapshot
                .next_playlists
                .iter()
                .take(20)
                .map(Agenda::from)
                .collect(),
            indicative_agenda: snapshot
                .indicative
                .iter()
                .take(20)
                .map(Agenda::from)
                .collect(),
            history: snapshot.history.iter().take(50).map(Track::from).collect(),
        }
    }
}
#[derive(Clone, Default)]
struct Cache {
    air: Air,
    connected: bool,
    received_at: Option<i64>,
}
#[derive(Clone)]
struct Entry {
    station: Station,
    cache: watch::Sender<Cache>,
}
#[derive(Clone)]
struct Web {
    entries: Arc<BTreeMap<String, Entry>>,
    streams: Arc<Semaphore>,
}
impl Web {
    fn new(config: &Config) -> Self {
        Self {
            entries: Arc::new(
                config
                    .stations
                    .iter()
                    .map(|station| {
                        let (cache, _) = watch::channel(Cache::default());
                        (
                            station.slug.clone(),
                            Entry {
                                station: station.clone(),
                                cache,
                            },
                        )
                    })
                    .collect(),
            ),
            streams: Arc::new(Semaphore::new(config.max_event_streams)),
        }
    }
    fn start(&self) {
        for entry in self.entries.values() {
            let mut endpoint = Endpoint::from_shared(entry.station.grpc_endpoint.clone())
                .expect("validated endpoint")
                .connect_timeout(RPC_TIMEOUT);
            if entry.station.grpc_endpoint.starts_with("https://") {
                endpoint = endpoint
                    .tls_config(ClientTlsConfig::new().with_native_roots())
                    .expect("TLS endpoint");
            }
            tokio::spawn(watch_station(entry.clone(), endpoint.connect_lazy()));
        }
    }
}
fn view(entry: &Entry) -> serde_json::Value {
    let cache = entry.cache.borrow();
    let fresh = cache.connected
        && cache
            .received_at
            .is_some_and(|at| now().saturating_sub(at) <= STALE_SECONDS)
        && cache
            .air
            .observed_at
            .is_some_and(|at| at > 0 && now().saturating_sub(at) <= STALE_SECONDS);
    serde_json::json!({"station":{"slug":entry.station.slug,"label":entry.station.label,
        "description":entry.station.description,"stream_url":entry.station.stream_url,"theme":entry.station.theme},
        "status": if fresh {"online"} else if cache.received_at.is_none() {"connecting"} else {"offline"},
        "stale":!fresh,"received_at":cache.received_at,"air":cache.air})
}
async fn watch_station(entry: Entry, channel: Channel) {
    let mut delay = 1;
    loop {
        let mut client = onair::on_air_service_client::OnAirServiceClient::new(channel.clone())
            .max_decoding_message_size(262144);
        let request = onair::WatchRequest {
            upcoming: 30,
            history: 50,
            playlists_ahead: 20,
        };
        if let Ok(Ok(response)) = tokio::time::timeout(RPC_TIMEOUT, client.watch(request)).await {
            let mut stream = response.into_inner();
            loop {
                match tokio::time::timeout(
                    Duration::from_secs(STALE_SECONDS as u64),
                    stream.message(),
                )
                .await
                {
                    Ok(Ok(Some(snapshot))) => {
                        // A reconnect may reset revision after a daemon restart; accept its first snapshot.
                        entry.cache.send_replace(Cache {
                            air: Air::from(&snapshot),
                            connected: true,
                            received_at: Some(now()),
                        });
                        delay = 1;
                    }
                    _ => break,
                }
            }
        }
        entry.cache.send_modify(|cache| cache.connected = false);
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(15);
    }
}
fn routes(web: Web) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/player/:slug", get(page))
        .route(
            "/player/assets/player.js",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    include_str!("../web/player.js"),
                )
            }),
        )
        .route(
            "/player/assets/player.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/player.css"),
                )
            }),
        )
        .route("/api/public/player/:slug", get(snapshot))
        .route("/api/public/player/:slug/events", get(events))
        .layer(middleware::from_fn(security))
        .with_state(web)
}
async fn security(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    // frame-ancestors deliberately allows the requested public iframe embedding.
    headers.insert(header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; media-src https: http:; base-uri 'none'; form-action 'none'; frame-ancestors *".parse().unwrap());
    response
}
fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

async fn home(State(web): State<Web>) -> Response {
    if web.entries.len() == 1 {
        if let Some(slug) = web.entries.keys().next() {
            return Redirect::temporary(&format!("/player/{slug}")).into_response();
        }
    }
    let stations = web
        .entries
        .values()
        .map(|entry| {
            format!(
                "<li><a href=\"/player/{}\">{}</a><p>{}</p></li>",
                entry.station.slug,
                escape_html(&entry.station.label),
                escape_html(&entry.station.description)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Html(include_str!("../web/home.html").replace("{{STATIONS}}", &stations)).into_response()
}

async fn page(State(web): State<Web>, Path(slug): Path<String>) -> Response {
    if !web.entries.contains_key(&slug) {
        return StatusCode::NOT_FOUND.into_response();
    }
    Html(include_str!("../web/player.html")).into_response()
}
async fn snapshot(State(web): State<Web>, Path(slug): Path<String>) -> Response {
    match web.entries.get(&slug) {
        Some(entry) => Json(view(entry)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn events(State(web): State<Web>, Path(slug): Path<String>) -> Response {
    let Some(entry) = web.entries.get(&slug).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(permit) = web.streams.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut rx = entry.cache.subscribe();
    let (tx, out) = mpsc::channel::<Result<Event, Infallible>>(1);
    tokio::spawn(async move {
        let _permit = permit;
        // The first full snapshot also makes reconnects independent of Last-Event-ID.
        loop {
            rx.borrow_and_update();
            let event = Event::default()
                .event("snapshot")
                .data(view(&entry).to_string());
            tokio::select! {
                result = tx.send(Ok(event)) => { if result.is_err() { break; } },
                _ = tx.closed() => break,
            }
            tokio::select! {
                _ = tx.closed() => break,
                result = rx.changed() => { if result.is_err() { break; } },
                // Refresh stale status even when the upstream is silent.
                _ = tokio::time::sleep(Duration::from_secs(15)) => {},
            }
        }
    });
    Sse::new(ReceiverStream::new(out))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

struct Server {
    shutdown: oneshot::Sender<()>,
    thread: thread::JoinHandle<()>,
}
pub struct StationPlayer {
    config: Config,
    server: Option<Server>,
}
impl StationPlayer {
    pub fn from_config(table: &toml::Table) -> Result<Self, String> {
        Ok(Self {
            config: Config::parse(table)?,
            server: None,
        })
    }
    pub fn start(&mut self) -> Result<(), String> {
        if self.server.is_some() {
            return Err("station-player is already running".into());
        }
        let listener = bind_listener(&self.config.bind)?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?
        };
        let web = Web::new(&self.config);
        let app = routes(web.clone());
        let (shutdown, shutdown_rx) = oneshot::channel();
        let (stopping, stopped) = oneshot::channel();
        let thread = thread::Builder::new().name(NAME.into()).spawn(move || {
            runtime.block_on(async move {
                web.start();
                let serving = axum::serve(listener, app).with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                    let _ = stopping.send(());
                });
                // SSE connections cannot extend unload forever. Runtime drop cancels workers.
                tokio::select! {
                    result = serving => { if let Err(error) = result { tracing::error!(plugin = NAME, %error, "player server failed"); } },
                    _ = async { let _ = stopped.await; tokio::time::sleep(Duration::from_secs(1)).await; } => {},
                }
            });
        }).map_err(|e| e.to_string())?;
        self.server = Some(Server { shutdown, thread });
        Ok(())
    }
    pub fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            let _ = server.shutdown.send(());
            let _ = server.thread.join();
        }
    }
}
impl Drop for StationPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
