//! Native Webmin supervision gateway: internal probe and private identities.
//! A dedicated runtime lets the synchronous plugin unload join the server even
//! when the plugin actor itself runs on a single-thread Tokio runtime.
mod auth;
mod console;
mod i18n;
mod live;
mod web;
use super::{Host, Plugin};
use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use tokio::sync::{oneshot, Semaphore};

const NAME: &str = "remote-supervision";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Station {
    id: String,
    label: String,
    grpc_endpoint: String,
    #[serde(default)]
    allow_plaintext_grpc: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default = "default_bind")]
    bind: SocketAddr,
    public_url: String,
    stations: Vec<Station>,
    #[serde(default = "default_max_requests")]
    max_requests: usize,
    #[serde(default = "default_request_timeout")]
    request_timeout_seconds: u64,
    #[serde(default = "default_shutdown_timeout")]
    shutdown_timeout_seconds: u64,
    #[serde(default = "default_session_ttl")]
    session_ttl_seconds: u64,
    #[serde(default = "default_enrollment_ttl")]
    enrollment_ttl_seconds: u64,
    #[serde(default = "default_auth_rate")]
    auth_requests_per_minute: u32,
    #[serde(default = "default_password_min_length")]
    password_min_length: usize,
    #[serde(default = "default_password_max_length")]
    password_max_length: usize,
    #[serde(default = "default_max_event_streams")]
    max_event_streams: usize,
    #[serde(default)]
    console: console::Settings,
}

fn default_max_event_streams() -> usize {
    64
}

fn default_password_min_length() -> usize {
    12
}
fn default_password_max_length() -> usize {
    256
}

fn default_session_ttl() -> u64 {
    43200
}
fn default_enrollment_ttl() -> u64 {
    900
}
fn default_auth_rate() -> u32 {
    120
}
fn default_bind() -> SocketAddr {
    "127.0.0.1:8090".parse().unwrap()
}
fn default_max_requests() -> usize {
    128
}
fn default_request_timeout() -> u64 {
    5
}
fn default_shutdown_timeout() -> u64 {
    2
}

/// Validate an origin, not an arbitrary URL. No credentials, query or path.
fn origin(value: &str, schemes: &[&str]) -> Result<(), String> {
    if value.len() > 2048 {
        return Err("origin exceeds 2048 bytes".into());
    }
    let uri: Uri = value.parse().map_err(|_| "invalid origin".to_string())?;
    let authority = uri.authority().ok_or("origin requires a host")?;
    if !uri.scheme_str().is_some_and(|s| schemes.contains(&s))
        || uri.host().is_none_or(str::is_empty)
        || authority.as_str().contains('@')
        || value.contains(['#', '?', '\\'])
        || !matches!(uri.path(), "" | "/")
        || uri.port().is_some_and(|p| p.as_u16() == 0)
        || !uri
            .host()
            .unwrap_or_default()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    {
        return Err(
            "expected an origin with an allowed scheme and no credentials, path, query or fragment"
                .into(),
        );
    }
    // Reject malformed/non-numeric ports, including a trailing colon.
    let host = uri.host().unwrap_or_default();
    if authority.as_str() != host && uri.port_u16().is_none() {
        return Err("invalid origin port".into());
    }
    Ok(())
}

impl Config {
    fn parse(table: &toml::Table) -> Result<Self, String> {
        let config: Self = toml::Value::Table(table.clone())
            .try_into()
            .map_err(|e| format!("{NAME}: invalid configuration: {e}"))?;
        config.validate().map_err(|e| format!("{NAME}: {e}"))?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if !self.bind.ip().is_loopback() || self.bind.port() == 0 {
            return Err("bind must be a loopback IP with a nonzero port".into());
        }
        origin(&self.public_url, &["https"]).map_err(|e| format!("public_url: {e}"))?;
        if !(1..=64).contains(&self.stations.len()) {
            return Err("stations must contain between 1 and 64 entries".into());
        }
        let mut ids = HashSet::new();
        for station in &self.stations {
            if station.id.is_empty()
                || station.id.len() > 64
                || !station
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                || !ids.insert(station.id.as_str())
            {
                return Err(
                    "station IDs must be unique, 1..64 ASCII letters, digits, '-' or '_'".into(),
                );
            }
            if station.label.trim().is_empty()
                || station.label.chars().count() > 128
                || station.label.chars().any(char::is_control)
            {
                return Err(format!("station {}: invalid label", station.id));
            }
            origin(&station.grpc_endpoint, &["http", "https"])
                .map_err(|e| format!("station {}: grpc_endpoint: {e}", station.id))?;
            let endpoint: Uri = station
                .grpc_endpoint
                .parse()
                .map_err(|_| "invalid station endpoint")?;
            if endpoint.scheme_str() == Some("http") && !station.allow_plaintext_grpc {
                let host = endpoint.host().unwrap_or_default().trim_matches(['[', ']']);
                if host != "localhost"
                    && !host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
                {
                    return Err(format!(
                        "station {}: remote gRPC requires HTTPS, a loopback tunnel, or allow_plaintext_grpc = true for a trusted private network",
                        station.id
                    ));
                }
            }
        }
        if !(1..=1024).contains(&self.max_requests)
            || !(1..=60).contains(&self.request_timeout_seconds)
            || !(1..=10).contains(&self.shutdown_timeout_seconds)
        {
            return Err("max_requests must be 1..1024, request_timeout_seconds 1..60, shutdown_timeout_seconds 1..10".into());
        }
        if !(60..=86400).contains(&self.session_ttl_seconds)
            || !(60..=86400).contains(&self.enrollment_ttl_seconds)
            || !(10..=10000).contains(&self.auth_requests_per_minute)
        {
            return Err("invalid authentication limits".into());
        }
        if !(1..=256).contains(&self.max_event_streams) {
            return Err("max_event_streams must be 1..256".into());
        }
        if self.password_min_length == 0
            || self.password_min_length > self.password_max_length
            || self.password_max_length > 256
        {
            return Err("password lengths must satisfy 1 <= password_min_length <= password_max_length <= 256".into());
        }
        self.console.validate()?;
        Ok(())
    }
}

pub(super) fn validate_config(table: &toml::Table) -> Result<(), String> {
    Config::parse(table).map(|_| ())
}

#[derive(Clone)]
struct Limits {
    requests: Arc<Semaphore>,
    timeout: Duration,
}

async fn limit_request(State(limits): State<Limits>, request: Request, next: Next) -> Response {
    let Ok(_permit) = limits.requests.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match tokio::time::timeout(limits.timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}

fn probe_router() -> Router {
    Router::new()
        // No version, station identities, endpoints or business data.
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        // Fail closed until WebAuthn and session authorization are implemented.
        .fallback(|| async { StatusCode::UNAUTHORIZED })
}

fn bounded_router(config: &Config, app: Router) -> Router {
    app.layer(DefaultBodyLimit::max(65536))
        .layer(middleware::from_fn_with_state(
            Limits {
                requests: Arc::new(Semaphore::new(config.max_requests)),
                timeout: Duration::from_secs(config.request_timeout_seconds),
            },
            limit_request,
        ))
}

#[cfg(test)]
fn router(config: &Config) -> Router {
    bounded_router(config, probe_router())
}

struct Server {
    shutdown: oneshot::Sender<()>,
    thread: thread::JoinHandle<()>,
}

pub(super) struct RemoteSupervision {
    config: Config,
    server: Option<Server>,
    auth: Option<Arc<Mutex<auth::Auth>>>,
}

impl RemoteSupervision {
    pub(super) fn from_config(table: &toml::Table) -> Result<Self, String> {
        Ok(Self {
            config: Config::parse(table)?,
            server: None,
            auth: None,
        })
    }

    fn start(&mut self) -> Result<(), String> {
        if self.server.is_some() {
            return Err("remote-supervision is already running".into());
        }
        // Bind synchronously: a busy port must fail on_load, not a detached task.
        let listener = std::net::TcpListener::bind(self.config.bind)
            .map_err(|e| format!("{NAME}: cannot bind {}: {e}", self.config.bind))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("{NAME}: {e}"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("{NAME}: cannot create HTTP runtime: {e}"))?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(|e| format!("{NAME}: {e}"))?
        };
        let (app, services) = match &self.auth {
            Some(auth) => {
                let web = web::Web::new(auth.clone(), &self.config);
                let services = (web.live.clone(), web.console.clone());
                (probe_router().merge(web::routes(web)), Some(services))
            }
            None => (probe_router(), None),
        };
        let app = bounded_router(&self.config, app);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let (stopping, stopped) = oneshot::channel();
        let grace = Duration::from_secs(self.config.shutdown_timeout_seconds);
        let server_thread = thread::Builder::new().name(NAME.into()).spawn(move || {
            runtime.block_on(async move {
                if let Some((live, console)) = &services { live.start(); console.start(); }
                let serving = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                    if let Some((_, console)) = &services { console.shutdown(); }
                    let _ = stopping.send(());
                });
                tokio::pin!(stopped);
                // Grace starts at shutdown, rather than at server startup.
                let deadline = async move {
                    let _ = stopped.await;
                    tokio::time::sleep(grace).await;
                };
                tokio::select! {
                    result = serving => {
                        if let Err(error) = result {
                            tracing::error!(plugin = NAME, %error, "remote supervision HTTP server failed");
                        }
                    }
                    _ = deadline => {
                        tracing::warn!(plugin = NAME, "remote supervision shutdown grace exceeded; closing connections");
                    }
                }
            });
            // Dropping this dedicated runtime cancels any remaining HTTP tasks.
        }).map_err(|e| format!("{NAME}: cannot start HTTP thread: {e}"))?;
        self.server = Some(Server {
            shutdown,
            thread: server_thread,
        });
        tracing::info!(plugin = NAME, bind = %self.config.bind,
            stations = self.config.stations.len(), "remote supervision internal probe listening");
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            let _ = server.shutdown.send(());
            if server.thread.join().is_err() {
                tracing::error!(plugin = NAME, "remote supervision HTTP thread panicked");
            }
        }
    }
}

impl Plugin for RemoteSupervision {
    fn filters_pool(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        NAME
    }
    fn validate_config(&mut self, config: &toml::Table, _host: &Host) -> Result<(), String> {
        validate_config(config)
    }
    fn db_migrations(&mut self) -> Result<Vec<String>, String> {
        Ok(vec![auth::MIGRATION.into()])
    }
    fn admin_request(&mut self, payload: &str) -> Result<String, String> {
        let request =
            serde_json::from_str(payload).map_err(|_| "invalid remote administration request")?;
        let auth = self
            .auth
            .as_ref()
            .ok_or("declare capability db and load the plugin to manage remote identities")?;
        let result = auth
            .try_lock()
            .map_err(|_| "remote identity service busy")?
            .admin(request)?;
        serde_json::to_string(&result)
            .map_err(|_| "cannot encode remote administration response".into())
    }
    fn on_load(&mut self, host: Host) -> Result<(), String> {
        if host.has(super::Capability::Db) {
            self.auth = Some(Arc::new(Mutex::new(auth::Auth::open(
                &self.config,
                host.db.clone().ok_or("plugin database unavailable")?,
            )?)));
        }
        self.start()
    }
    fn on_unload(&mut self) {
        self.stop();
    }
}

impl Drop for RemoteSupervision {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn table() -> toml::Table {
        toml::from_str(
            r#"
            public_url = "https://remote.example.test"
            [[stations]]
            id = "home-stone"
            label = "Home Stone"
            grpc_endpoint = "http://127.0.0.1:50051"
            [[stations]]
            id = "second"
            label = "Second station"
            grpc_endpoint = "https://second.example.test:50051"
        "#,
        )
        .unwrap()
    }

    #[test]
    fn webmin_config_validates_configurable_password_lengths() {
        let mut value = table();
        let defaults = Config::parse(&value).unwrap();
        assert_eq!(
            (defaults.password_min_length, defaults.password_max_length),
            (12, 256)
        );
        for (min, max, valid) in [
            (6, 8, true),
            (1, 1, true),
            (256, 256, true),
            (0, 8, false),
            (9, 8, false),
            (1, 257, false),
        ] {
            value.insert("password_min_length".into(), toml::Value::Integer(min));
            value.insert("password_max_length".into(), toml::Value::Integer(max));
            assert_eq!(Config::parse(&value).is_ok(), valid, "{min}..{max}");
        }
    }

    #[test]
    fn webmin_live_config_requires_secure_remote_transport_and_bounds_stream_quota() {
        let mut config = Config::parse(&table()).unwrap();
        assert!(!config.stations[0].allow_plaintext_grpc);
        for endpoint in ["http://192.168.1.20:50051", "http://EU-HomeStone.lan:50051"] {
            config.stations[0].grpc_endpoint = endpoint.into();
            assert!(config.validate().is_err());
            config.stations[0].allow_plaintext_grpc = true;
            assert!(config.validate().is_ok());
            config.stations[0].allow_plaintext_grpc = false;
        }
        let mut value = table();
        value.get_mut("stations").unwrap().as_array_mut().unwrap()[0]
            .as_table_mut()
            .unwrap()
            .insert("allow_plaintext_grpc".into(), toml::Value::Boolean(true));
        let opted_in = Config::parse(&value).unwrap();
        assert!(opted_in.stations[0].allow_plaintext_grpc);
        assert!(!opted_in.stations[1].allow_plaintext_grpc);
        config.stations[0].grpc_endpoint = "https://station.internal:50051".into();
        assert!(config.validate().is_ok());
        config.max_event_streams = 0;
        assert!(config.validate().is_err());
        config.max_event_streams = 257;
        assert!(config.validate().is_err());
    }

    #[test]
    fn webmin_config_accepts_multiple_stations_and_ipv6() {
        let mut config = Config::parse(&table()).unwrap();
        assert_eq!(config.stations.len(), 2);
        config.bind = "[::1]:8090".parse().unwrap();
        config.public_url = "https://[::1]:443".into();
        config.validate().unwrap();
    }

    #[test]
    fn webmin_config_rejects_invalid_origins() {
        for value in [
            "http://remote.test",
            "https://user:secret@remote.test",
            "https://remote.test/path",
            "https://remote.test?x=1",
            "https://remote.test#fragment",
            "https://remote.test:0",
            "https://remote.test:",
            "https://remote.test:abc",
            "https://remote.test:99999",
        ] {
            let mut config = Config::parse(&table()).unwrap();
            config.public_url = value.into();
            assert!(config.validate().is_err(), "accepted {value}");
        }
    }

    #[test]
    fn webmin_config_rejects_bad_station_catalogues_and_limits() {
        let valid = Config::parse(&table()).unwrap();
        for mutate in [
            |c: &mut Config| c.stations.clear(),
            |c: &mut Config| c.stations[1].id = c.stations[0].id.clone(),
            |c: &mut Config| c.stations[0].id = "../station".into(),
            |c: &mut Config| c.stations[0].label = "\n".into(),
            |c: &mut Config| c.stations[0].grpc_endpoint = "http://user:secret@host".into(),
            |c: &mut Config| c.bind = "0.0.0.0:8090".parse().unwrap(),
            |c: &mut Config| c.bind = "127.0.0.1:0".parse().unwrap(),
            |c: &mut Config| c.max_requests = 0,
            |c: &mut Config| c.request_timeout_seconds = 61,
            |c: &mut Config| c.shutdown_timeout_seconds = 11,
        ] {
            let mut config = valid.clone();
            mutate(&mut config);
            assert!(config.validate().is_err());
        }
        let mut value = table();
        value.insert("typo".into(), true.into());
        assert!(Config::parse(&value).is_err());
    }

    #[tokio::test]
    async fn webmin_router_exposes_only_probe_and_closes_business_routes() {
        use tower::ServiceExt;
        let app = router(&Config::parse(&table()).unwrap());
        for (method, path, expected) in [
            ("GET", "/healthz", StatusCode::NO_CONTENT),
            ("POST", "/healthz", StatusCode::METHOD_NOT_ALLOWED),
            ("GET", "/", StatusCode::UNAUTHORIZED),
            ("GET", "/api/stations", StatusCode::UNAUTHORIZED),
            ("GET", "/api/status", StatusCode::UNAUTHORIZED),
            ("POST", "/api/control/skip", StatusCode::UNAUTHORIZED),
            ("GET", "/console/session", StatusCode::UNAUTHORIZED),
        ] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{method} {path}");
            assert!(axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap()
                .is_empty());
        }
    }

    #[tokio::test]
    async fn webmin_request_limits_refuse_saturation_and_timeout_slow_handlers() {
        use tower::ServiceExt;
        let limits = Limits {
            requests: Arc::new(Semaphore::new(1)),
            timeout: Duration::from_millis(20),
        };
        let held = limits.requests.clone().acquire_owned().await.unwrap();
        let app = Router::new()
            .route(
                "/",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    StatusCode::NO_CONTENT
                }),
            )
            .layer(middleware::from_fn_with_state(
                limits.clone(),
                limit_request,
            ));
        let request = || {
            axum::http::Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(held);
        assert_eq!(
            app.oneshot(request()).await.unwrap().status(),
            StatusCode::REQUEST_TIMEOUT
        );
        assert_eq!(limits.requests.available_permits(), 1);
    }

    #[test]
    fn webmin_shutdown_bounds_idle_connections() {
        let available = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = available.local_addr().unwrap();
        drop(available);
        let mut plugin = RemoteSupervision::from_config(&table()).unwrap();
        plugin.config.bind = addr;
        plugin.config.shutdown_timeout_seconds = 1;
        plugin.start().unwrap();
        probe(addr);
        let mut idle = std::net::TcpStream::connect(addr).unwrap();
        idle.write_all(b"GET /healthz HTTP/1.1\r\nHost:").unwrap();
        thread::sleep(Duration::from_millis(30));
        let started = std::time::Instant::now();
        plugin.stop();
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(std::net::TcpListener::bind(addr).unwrap());
        idle.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut byte = [0];
        match idle.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("idle connection left open: {result:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn webmin_registry_reports_bind_failure_and_defaults_to_disabled() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = table();
        config.insert(
            "bind".into(),
            listener.local_addr().unwrap().to_string().into(),
        );
        let mut decl: super::super::PluginDecl =
            toml::from_str("name = 'remote-supervision'").unwrap();
        decl.config = config;
        let disabled = super::super::spawn(vec![decl.clone()]);
        assert_eq!(disabled.list().await[0].state, "disabled");
        decl.enabled = true;
        let occupied = super::super::spawn(vec![decl.clone()]);
        let info = occupied.list().await;
        assert_eq!(info[0].state, "failed");
        assert!(info[0].reason.contains("cannot bind"));
        decl.enabled = false;
        decl.config.insert("bind".into(), "0.0.0.0:8090".into());
        assert!(super::super::validate_decls(&[decl]).is_err());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn webmin_identity_admin_and_reload_share_the_private_store() {
        let dir = tempfile::tempdir().unwrap();
        let available = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = available.local_addr().unwrap();
        drop(available);
        let mut decl: super::super::PluginDecl =
            toml::from_str("name = 'remote-supervision'\nenabled = true\ncapabilities = ['db']")
                .unwrap();
        decl.config = table();
        decl.config.insert("bind".into(), addr.to_string().into());
        let env = super::super::PluginEnv {
            db_dir: Some(dir.path().into()),
            ..Default::default()
        };
        let handle = super::super::spawn_env(vec![decl], env);
        assert_eq!(handle.list().await[0].state, "loaded");
        probe(addr);
        let enrollment = handle.admin(NAME, serde_json::json!({"action":"add","name":"Alice","role":"helper","stations":["home-stone"]}).to_string()).await.unwrap();
        assert!(enrollment.contains("enrollment_url"));
        assert!(handle.simulation().admin(NAME, "{}".into()).await.is_err());
        assert!(handle.simulation().filter_pool(Vec::new()).await.is_empty());
        assert_eq!(
            handle
                .control(NAME, super::super::Action::Reload)
                .await
                .unwrap()
                .state,
            "loaded"
        );
        let users: serde_json::Value = serde_json::from_str(
            &handle
                .admin(NAME, "{\"action\":\"list\"}".into())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(users[0]["name"], "Alice");
        assert_eq!(users[0]["grants"]["home-stone"], "helper");
        assert_eq!(
            handle
                .control(NAME, super::super::Action::Stop)
                .await
                .unwrap()
                .state,
            "disabled"
        );
        assert!(handle.admin(NAME, "{}".into()).await.is_err());
        drop(std::net::TcpListener::bind(addr).unwrap());
    }
    fn probe(addr: SocketAddr) {
        let mut socket = std::net::TcpStream::connect(addr).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 204"), "{response}");
    }

    #[test]
    fn webmin_lifecycle_fails_on_occupied_port_and_releases_on_stop_and_drop() {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = occupied.local_addr().unwrap();
        let mut plugin = RemoteSupervision::from_config(&table()).unwrap();
        plugin.config.bind = addr;
        assert!(plugin.start().unwrap_err().contains("cannot bind"));
        drop(occupied);
        for _ in 0..3 {
            plugin.start().unwrap();
            probe(addr);
            plugin.stop();
            drop(std::net::TcpListener::bind(addr).unwrap());
        }
        plugin.start().unwrap();
        drop(plugin);
        drop(std::net::TcpListener::bind(addr).unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn webmin_registry_reload_on_single_thread_runtime() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut config = table();
        config.insert("bind".into(), addr.to_string().into());
        let mut decl: super::super::PluginDecl =
            toml::from_str("name = 'remote-supervision'").unwrap();
        decl.config = config;
        decl.enabled = true;
        super::super::validate_decls(std::slice::from_ref(&decl)).unwrap();
        let handle = super::super::spawn(vec![decl]);
        assert_eq!(handle.list().await[0].state, "loaded");
        probe(addr);
        for _ in 0..3 {
            assert_eq!(
                handle
                    .control(NAME, super::super::Action::Reload)
                    .await
                    .unwrap()
                    .state,
                "loaded"
            );
            probe(addr);
        }
        assert_eq!(
            handle
                .control(NAME, super::super::Action::Stop)
                .await
                .unwrap()
                .state,
            "disabled"
        );
        drop(std::net::TcpListener::bind(addr).unwrap());
    }
}
