//! Admin consoles: one-use reservations, fixed station target, bounded PTY/WS.
mod pty;
use super::{
    auth::Auth,
    web::{answer, cookie, Web, SESSION},
    Config,
};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    path::Path as FsPath,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Settings {
    pub enabled: bool,
    pub command: String,
    pub max_sessions: usize,
    pub max_sessions_per_user: usize,
    pub idle_timeout_seconds: u64,
    pub max_duration_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            command: "/usr/local/bin/stationd-tui".into(),
            max_sessions: 5,
            max_sessions_per_user: 1,
            idle_timeout_seconds: 900,
            max_duration_seconds: 14400,
        }
    }
}
impl Settings {
    pub(super) fn validate(&self) -> Result<(), String> {
        let path = FsPath::new(&self.command);
        if !path.is_absolute()
            || path.file_name().is_none_or(|name| name != "stationd-tui")
            || self.command.chars().any(char::is_control)
        {
            return Err("console.command must be an absolute path to stationd-tui".into());
        }
        if !(1..=64).contains(&self.max_sessions)
            || !(1..=self.max_sessions).contains(&self.max_sessions_per_user)
            || !(1..=86400).contains(&self.idle_timeout_seconds)
            || !(1..=86400).contains(&self.max_duration_seconds)
        {
            return Err("invalid console quotas or timeouts".into());
        }
        if self.enabled && !cfg!(target_os = "linux") {
            return Err("Webmin console requires Linux".into());
        }
        Ok(())
    }
}
#[derive(Clone)]
struct Reservation {
    owner: String,
    session: String,
    csrf: String,
    station: String,
    cols: u16,
    rows: u16,
    expires: Instant,
    active: bool,
    cancel: watch::Sender<bool>,
}
#[derive(Default)]
struct Registry {
    stopping: bool,
    entries: HashMap<String, Reservation>,
}
#[derive(Clone)]
pub(super) struct Console {
    settings: Settings,
    endpoints: Arc<HashMap<String, String>>,
    auth: Arc<Mutex<Auth>>,
    registry: Arc<Mutex<Registry>>,
}
struct Lease {
    console: Console,
    id: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.console.registry.lock() {
            registry.entries.remove(&self.id);
        }
    }
}
impl Console {
    pub(super) fn new(auth: Arc<Mutex<Auth>>, config: &Config) -> Self {
        Self {
            settings: config.console.clone(),
            endpoints: Arc::new(
                config
                    .stations
                    .iter()
                    .map(|s| (s.id.clone(), s.grpc_endpoint.clone()))
                    .collect(),
            ),
            auth,
            registry: Default::default(),
        }
    }
    pub(super) fn start(&self) {
        let console = self.clone();
        tokio::spawn(async move {
            let mut changes = console.auth.lock().expect("auth mutex").changes();
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! { _ = tick.tick() => {}, result = changes.changed() => { if result.is_err() { break; } } }
                let Ok(auth) = console.auth.try_lock() else {
                    continue;
                };
                let Ok(mut registry) = console.registry.lock() else {
                    break;
                };
                if registry.stopping {
                    break;
                }
                registry.entries.retain(|_, entry| {
                    let valid = auth
                        .console_owner(&entry.session, &entry.station, &entry.csrf)
                        .is_ok();
                    if !valid {
                        entry.cancel.send_replace(true);
                    }
                    entry.active || (valid && entry.expires > Instant::now())
                });
            }
        });
    }
    pub(super) fn shutdown(&self) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.stopping = true;
            for entry in registry.entries.values() {
                entry.cancel.send_replace(true);
            }
            registry.entries.retain(|_, entry| entry.active);
        }
    }
    fn reserve(
        &self,
        owner: String,
        session: String,
        csrf: String,
        station: String,
        size: Size,
    ) -> Result<String, String> {
        if !self.settings.enabled {
            return Err("permission denied".into());
        }
        let mut registry = self.registry.lock().map_err(|_| "busy")?;
        registry
            .entries
            .retain(|_, entry| entry.active || entry.expires > Instant::now());
        if registry.stopping
            || registry.entries.len() >= self.settings.max_sessions
            || registry
                .entries
                .values()
                .filter(|entry| entry.owner == owner)
                .count()
                >= self.settings.max_sessions_per_user
        {
            return Err("busy".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        registry.entries.insert(
            id.clone(),
            Reservation {
                owner,
                session,
                csrf,
                station,
                cols: size.cols,
                rows: size.rows,
                expires: Instant::now() + Duration::from_secs(15),
                active: false,
                cancel: watch::channel(false).0,
            },
        );
        Ok(id)
    }
    fn claim(
        &self,
        id: &str,
        session: &str,
        station: &str,
    ) -> Result<(Reservation, Lease), String> {
        let mut registry = self.registry.lock().map_err(|_| "busy")?;
        if registry.stopping {
            return Err("busy".into());
        }
        let entry = registry.entries.get_mut(id).ok_or("permission denied")?;
        if entry.session != session
            || entry.station != station
            || entry.active
            || entry.expires <= Instant::now()
            || *entry.cancel.borrow()
        {
            return Err("permission denied".into());
        }
        entry.active = true;
        Ok((
            entry.clone(),
            Lease {
                console: self.clone(),
                id: id.into(),
            },
        ))
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct Size {
    cols: u16,
    rows: u16,
}
impl Size {
    fn valid(self) -> bool {
        (20..=300).contains(&self.cols) && (5..=120).contains(&self.rows)
    }
}
pub(super) fn routes() -> Router<Web> {
    Router::new()
        .route("/station/:id/console", get(page))
        .route("/api/stations/:id/console", post(create))
        .route("/api/stations/:id/console/:console/ws", get(upgrade))
        .route(
            "/console.js",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    include_str!("console.js"),
                )
            }),
        )
        .route(
            "/terminal/xterm.js",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    include_str!("vendor/xterm.js"),
                )
            }),
        )
        .route(
            "/terminal/xterm.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("vendor/xterm.css"),
                )
            }),
        )
        .route(
            "/terminal/fit.js",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    include_str!("vendor/fit.js"),
                )
            }),
        )
}
async fn page(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let raw = cookie(&headers, SESSION).unwrap_or_default();
    let target = id.clone();
    if !web.console.settings.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    match web
        .work(move |a| {
            let context = a.context(&raw)?;
            a.console_owner(
                &raw,
                &target,
                context["csrf_token"].as_str().unwrap_or_default(),
            )?;
            Ok(())
        })
        .await
    {
        Ok(()) => Html(include_str!("console.html").replace("{{STATION_ID}}", &id)).into_response(),
        Err(error) => answer(Err(error)),
    }
}
async fn create(
    State(web): State<Web>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(size): Json<Size>,
) -> Response {
    if !size.valid() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let raw = cookie(&headers, SESSION).unwrap_or_default();
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let (token, proof, target) = (raw.clone(), csrf.clone(), id.clone());
    let owner = match web
        .work(move |a| a.console_attempt(&token, &target, &proof))
        .await
    {
        Ok(owner) => owner,
        Err(error) => return answer(Err(error)),
    };
    answer(
        web.console
            .reserve(owner, raw, csrf, id, size)
            .map(|id| json!({"id":id})),
    )
}
async fn upgrade(
    State(web): State<Web>,
    Path((station, id)): Path<(String, String)>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Mandatory Origin on upgrades, including GET. Reservations are not bearer credentials.
    if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(web.origin.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let raw = cookie(&headers, SESSION).unwrap_or_default();
    let (entry, lease) = match web.console.claim(&id, &raw, &station) {
        Ok(value) => value,
        Err(error) => return answer(Err(error)),
    };
    let check = entry.clone();
    if let Err(error) = web
        .work(move |a| a.console_owner(&check.session, &check.station, &check.csrf))
        .await
    {
        return answer(Err(error));
    }
    ws.max_message_size(8192)
        .max_frame_size(8192)
        .write_buffer_size(0)
        .max_write_buffer_size(32768)
        .on_upgrade(move |socket| run(socket, web, entry, lease))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum Input {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Ack,
}
async fn send(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(1), socket.send(message)).await,
        Ok(Ok(()))
    )
}
async fn run(mut socket: WebSocket, web: Web, entry: Reservation, _lease: Lease) {
    let mut cancel = entry.cancel.subscribe();
    let mut changes = match web.auth.lock() {
        Ok(auth) => auth.changes(),
        Err(_) => return,
    };
    let check = entry.clone();
    if web
        .work(move |a| {
            a.console_owner(&check.session, &check.station, &check.csrf)?;
            a.console_audit(&check.owner, "console.open", "success", &check.station)
        })
        .await
        .is_err()
        || *cancel.borrow()
    {
        return;
    }
    let endpoint = match web.console.endpoints.get(&entry.station) {
        Some(endpoint) => endpoint,
        None => return,
    };
    let mut terminal = match pty::Pty::spawn(
        &web.console.settings.command,
        endpoint,
        entry.cols,
        entry.rows,
    ) {
        Ok(terminal) => terminal,
        Err(_) => {
            if let Ok(auth) = web.auth.lock() {
                let _ = auth.console_audit(
                    &entry.owner,
                    "console.close",
                    "launch_failed",
                    &entry.station,
                );
            }
            let _ = send(
                &mut socket,
                Message::Text(json!({"type":"ended","reason":"launch_failed"}).to_string()),
            )
            .await;
            return;
        }
    };
    let opened = Instant::now();
    let mut activity = opened;
    let idle = Duration::from_secs(web.console.settings.idle_timeout_seconds);
    let maximum = opened + Duration::from_secs(web.console.settings.max_duration_seconds);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut buffer = [0; 4096];
    let mut bytes_this_second = 0usize;
    let mut rate_at = opened;
    let mut awaiting_output = false;
    let mut acknowledged_at = opened;
    let reason = loop {
        if *cancel.borrow() {
            break "revoked_or_stopped";
        }
        let check = entry.clone();
        // Revalidate before every input/output and after identity changes.
        if web
            .work(move |a| a.console_owner(&check.session, &check.station, &check.csrf))
            .await
            .is_err()
        {
            break "revoked";
        }
        if Instant::now() >= maximum {
            break "duration";
        }
        if Instant::now() >= activity + idle {
            break "idle";
        }
        tokio::select! {
            biased;
            _ = cancel.changed() => { break "revoked_or_stopped"; }
            _ = changes.changed() => { continue; }
            _ = tokio::time::sleep_until(maximum) => { break "duration"; }
            _ = tokio::time::sleep_until(activity + idle) => { break "idle"; }
            _ = tokio::time::sleep_until(acknowledged_at + Duration::from_secs(5)), if awaiting_output => { break "slow_client"; }
            _ = tick.tick() => { if terminal.exited() { break "exited"; } }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if rate_at.elapsed() >= Duration::from_secs(1) { rate_at = Instant::now(); bytes_this_second = 0; }
                        bytes_this_second += text.len();
                        if bytes_this_second > 65536 { break "rate_limited"; }
                        // A revocation can commit while recv was pending.
                        let check = entry.clone();
                        if web.work(move |a| a.console_owner(&check.session, &check.station, &check.csrf)).await.is_err() || *cancel.borrow() { break "revoked"; }
                        match serde_json::from_str::<Input>(&text) {
                            Ok(Input::Ack) if awaiting_output => { awaiting_output = false; }
                            Ok(Input::Input { data }) if !data.is_empty() && data.len() <= 4096 => {
                                let written = tokio::select! {
                                    result = tokio::time::timeout(Duration::from_secs(1), terminal.write(data.as_bytes())) => matches!(result, Ok(Ok(()))),
                                    _ = cancel.changed() => false,
                                    _ = tokio::time::sleep_until(maximum) => false,
                                };
                                if !written { break "io_error"; }
                                activity = Instant::now();
                            }
                            Ok(Input::Resize { cols, rows }) if (Size { cols, rows }).valid() => {
                                if terminal.resize(cols, rows).is_err() { break "io_error"; }
                            }
                            _ => { break "invalid_input"; }
                        }
                    }
                    Some(Ok(Message::Ping(value))) => { if !send(&mut socket, Message::Pong(value)).await { break "disconnected"; } }
                    Some(Ok(Message::Pong(_))) => {},
                    _ => { break "disconnected"; }
                }
            }
            output = terminal.read(&mut buffer), if !awaiting_output => {
                match output {
                    Ok(count) if count > 0 => {
                        let check = entry.clone();
                        if web.work(move |a| a.console_owner(&check.session, &check.station, &check.csrf)).await.is_err() || *cancel.borrow() { break "revoked"; }
                        if !send(&mut socket, Message::Binary(buffer[..count].to_vec())).await { break "slow_client"; }
                        awaiting_output = true;
                        acknowledged_at = Instant::now();
                    }
                    _ => { break "exited"; }
                }
            }
        }
    };
    // Kill and reap before any potentially blocked network close. Drop also handles task cancellation.
    drop(terminal);
    if let Ok(auth) = web.auth.lock() {
        let _ = auth.console_audit(&entry.owner, "console.close", reason, &entry.station);
    }
    let _ = send(
        &mut socket,
        Message::Text(json!({"type":"ended","reason":reason}).to_string()),
    )
    .await;
    let _ = send(&mut socket, Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webmin_console_config_and_protocol_are_bounded() {
        let mut settings = Settings::default();
        settings.validate().unwrap();
        for path in ["/bin/bash", "stationd-tui", "/usr/bin/stationd-tui\n"] {
            settings.command = path.into();
            assert!(settings.validate().is_err());
        }
        settings = Settings::default();
        settings.max_sessions_per_user = 6;
        assert!(settings.validate().is_err());
        settings = Settings::default();
        settings.idle_timeout_seconds = 0;
        assert!(settings.validate().is_err());
        assert!(!Size {
            cols: 301,
            rows: 24
        }
        .valid());
        assert!(!Size { cols: 80, rows: 0 }.valid());
        assert!(
            serde_json::from_str::<Input>(r#"{"type":"input","data":"x","command":"bash"}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<Input>(r#"{"type":"resize","cols":80,"rows":24}"#).is_ok());
    }
    #[test]
    fn webmin_console_reservations_bound_global_user_quota_and_are_one_use() {
        let dir = tempfile::tempdir().unwrap();
        let table = toml::from_str(
            r#"
public_url="https://remote.example.test"
[console]
enabled=true
max_sessions=2
max_sessions_per_user=1
[[stations]]
id="one"
label="One"
grpc_endpoint="http://127.0.0.1:50051"
"#,
        )
        .unwrap();
        let config = Config::parse(&table).unwrap();
        let db = Arc::new(
            crate::plugin_db::PluginDb::open(dir.path(), "remote-supervision", Default::default())
                .unwrap(),
        );
        db.migrate(&[super::super::auth::MIGRATION.into()]).unwrap();
        let console = Console::new(
            Arc::new(Mutex::new(Auth::open(&config, db).unwrap())),
            &config,
        );
        let reserve = |owner: &str, token: &str| {
            console.reserve(
                owner.into(),
                token.into(),
                "csrf".into(),
                "one".into(),
                Size { cols: 80, rows: 24 },
            )
        };
        let alice = reserve("alice", "session-a").unwrap();
        assert!(reserve("alice", "different-session").is_err());
        let bob = reserve("bob", "session-b").unwrap();
        assert!(reserve("charlie", "session-c").is_err());
        assert!(console.claim(&alice, "session-b", "one").is_err());
        assert!(console.claim(&alice, "session-a", "two").is_err());
        let (_, lease) = console.claim(&alice, "session-a", "one").unwrap();
        assert!(console.claim(&alice, "session-a", "one").is_err());
        drop(lease);
        assert!(reserve("alice", "session-a").is_ok());
        // Expired pending reservations cannot attach and release quota on the next request.
        console
            .registry
            .lock()
            .unwrap()
            .entries
            .get_mut(&bob)
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);
        assert!(console.claim(&bob, "session-b", "one").is_err());
        assert!(reserve("bob", "session-b").is_ok());
        console.shutdown();
        assert!(reserve("charlie", "session-c").is_err());
    }
}
