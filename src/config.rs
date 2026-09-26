use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub station: StationConfig,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub media: MediaConfig,
    pub playlist: PlaylistConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Plugins declared for this station (order + enable flag + opaque config).
    /// Empty by default. Cf. Doc/plugin-hooks.md. TOML key is `[[plugin]]`
    /// (singular, like `[[rule]]`), the field stays plural in Rust.
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<crate::plugin::PluginDecl>,
    /// Liquidsoap wiring (optional). Absent = nothing airs: the daemon still
    /// schedules and answers the CLI, but no `.liq` is generated and no
    /// bridge listens. Cf. `ls_script` / `ls_bridge`.
    #[serde(default)]
    pub liquidsoap: Option<LiquidsoapConfig>,
    /// Icecast admin access (optional). Absent = the audience is never read
    /// (only `stationctl debug listeners` feeds it). Requires `[liquidsoap]`:
    /// the watched mounts are its `[[liquidsoap.output]]` mounts.
    #[serde(default)]
    pub icecast: Option<IcecastConfig>,
    /// Live DJs (optional): a harbor input in the generated script, DJ
    /// credentials in a separate file, connection windows from the grid's
    /// `live` rules. Absent = no harbor, and a `live` rule is refused at
    /// `schedule apply`. Requires `[liquidsoap]`.
    #[serde(default)]
    pub live: Option<LiveConfig>,
}

/// `[live]` — live DJ input (Liquidsoap harbor). Who may connect lives in
/// `djs_path` (DJ ids + argon2 password hashes, re-read at each connection
/// attempt); when, in the grid (`kind = "live"`). stationd decides every
/// connection (Liquidsoap asks it through the loopback bridge) and ends the
/// live on disconnection or on `silence_timeout` seconds of silence.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveConfig {
    /// The DJ file (`schema_version = 1` + `[[dj]]`). Kept apart from
    /// stationd.toml: it holds credentials (hashed).
    pub djs_path: PathBuf,
    /// Harbor port the DJs' software connects to (Icecast source protocol).
    /// Exposed to the DJs: open it in the firewall / NAT, and nothing else.
    #[serde(default = "default_live_port")]
    pub harbor_port: u16,
    /// Harbor mount point, e.g. "/live".
    #[serde(default = "default_live_mount")]
    pub mount: String,
    /// Short fade, seconds, when the live takes the air and when it gives it
    /// back.
    #[serde(default = "default_live_fade")]
    pub fade: f64,
    /// Seconds of silence after which the live ends (the DJ is disconnected
    /// and refused until the end of the slot).
    #[serde(default = "default_live_silence")]
    pub silence_timeout: u32,
    /// Level (dBFS) under which the live input counts as silent.
    #[serde(default = "default_live_threshold")]
    pub silence_threshold: f64,
    /// Seconds of the DJ stream buffered before it airs (network jitter).
    #[serde(default = "default_live_buffer")]
    pub buffer: f64,
}

fn default_live_port() -> u16 {
    8005
}
fn default_live_mount() -> String {
    "/live".into()
}
fn default_live_fade() -> f64 {
    1.5
}
fn default_live_silence() -> u32 {
    30
}
fn default_live_threshold() -> f64 {
    -40.0
}
fn default_live_buffer() -> f64 {
    5.0
}

impl LiveConfig {
    /// Loud checks at load. `ls` = `[liquidsoap]` (required: the harbor lives
    /// in its script), `icecast_ports` = the ports Icecast listens on (the
    /// harbor must not collide with them).
    pub fn validate(&self, ls: Option<&LiquidsoapConfig>, icecast_ports: &[u16]) -> Result<(), String> {
        if ls.is_none() {
            return Err("[live] needs [liquidsoap]: the harbor is part of the generated script".into());
        }
        if self.harbor_port == 0 {
            return Err("harbor_port must not be 0".into());
        }
        if icecast_ports.contains(&self.harbor_port) {
            return Err(format!("harbor_port {} is Icecast's port", self.harbor_port));
        }
        let m = &self.mount;
        if !m.starts_with('/') || m.len() < 2 || !m[1..].bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
            return Err(format!("mount {m:?}: '/' then letters, digits, '-', '_', '.' only"));
        }
        if !(self.fade.is_finite() && (0.0..=10.0).contains(&self.fade)) {
            return Err(format!("fade {} out of 0..=10 s", self.fade));
        }
        if !(5..=3600).contains(&self.silence_timeout) {
            return Err(format!("silence_timeout {} out of 5..=3600 s", self.silence_timeout));
        }
        if !(self.silence_threshold.is_finite() && (-90.0..=0.0).contains(&self.silence_threshold)) {
            return Err(format!("silence_threshold {} out of -90..=0 dB", self.silence_threshold));
        }
        if !(self.buffer.is_finite() && (0.5..=30.0).contains(&self.buffer)) {
            return Err(format!("buffer {} out of 0.5..=30 s", self.buffer));
        }
        let p = self.djs_path.to_string_lossy();
        if p.trim().is_empty() {
            return Err("djs_path is empty".into());
        }
        Ok(())
    }
}

/// `[icecast]` — read-only access to Icecast's admin API (`/admin/stats`),
/// sampled periodically: audience (`ListenersSampled`) and mount health.
/// Admin credentials, distinct from the source password Liquidsoap uses in
/// `[[liquidsoap.output]]`. stationd talks to Icecast directly, never through
/// the reverse proxy: plain `http://` only.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IcecastConfig {
    /// Base URL of Icecast itself, e.g. "http://127.0.0.1:8000" (no path).
    pub admin_url: String,
    #[serde(default = "default_icecast_admin_user")]
    pub admin_user: String,
    pub admin_password: String,
    /// Seconds between two samples.
    #[serde(default = "default_icecast_poll_interval")]
    pub poll_interval: u64,
    /// `[icecast.server]` present = stationd generates Icecast's own config
    /// (`icecast.xml`). Absent = stationd only reads an Icecast configured
    /// elsewhere.
    #[serde(default)]
    pub server: Option<IcecastServerConfig>,
}

/// `[icecast.server]` — the `icecast.xml` stationd writes at start-up (only
/// when it changed; Icecast runs under its own unit and must be restarted to
/// pick it up). Mounts = `[[liquidsoap.output]]`, each with its own source
/// password; admin credentials = `[icecast]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IcecastServerConfig {
    /// Where the generated `icecast.xml` is written (mode 0640: holds the
    /// passwords; Icecast's user must be in stationd's group).
    #[serde(default = "default_icecast_config_path")]
    pub config_path: PathBuf,
    /// Listening port. `admin_url` and every output must use it.
    #[serde(default = "default_icecast_port")]
    pub port: u16,
    /// Optional bind address (default: all interfaces).
    #[serde(default)]
    pub bind_address: Option<String>,
    /// Public host name (listen URLs, directory listings).
    #[serde(default = "default_icecast_hostname")]
    pub hostname: String,
    /// `<location>` shown by Icecast. Default: the station name.
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default = "default_icecast_admin_email")]
    pub admin_email: String,
    /// Maximum simultaneous clients (listeners + sources + admin).
    #[serde(default = "default_icecast_max_clients")]
    pub max_clients: u32,
    /// Reverse proxies trusted for `X-Forwarded-For` (Icecast 2.5: one
    /// virtual socket per address). Exact IP addresses only.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// Icecast's installed data (`web/`, `admin/`, `report-db.xml`).
    #[serde(default = "default_icecast_share_dir")]
    pub share_dir: PathBuf,
    #[serde(default = "default_icecast_log_dir")]
    pub log_dir: PathBuf,
    /// Group given to the written `icecast.xml` (mode 0640): the group
    /// shared by stationd and Icecast's user, created at install (e.g.
    /// `stationd`). stationd must be a member. Absent = the group of the
    /// stationd process (logged at start-up).
    #[serde(default)]
    pub file_group: Option<String>,
}

fn default_icecast_config_path() -> PathBuf {
    PathBuf::from("./data/icecast.xml")
}
fn default_icecast_port() -> u16 {
    8000
}
fn default_icecast_hostname() -> String {
    "localhost".to_string()
}
fn default_icecast_admin_email() -> String {
    "icemaster@localhost".to_string()
}
fn default_icecast_max_clients() -> u32 {
    100
}
fn default_icecast_share_dir() -> PathBuf {
    PathBuf::from("/usr/share/icecast2")
}
fn default_icecast_log_dir() -> PathBuf {
    PathBuf::from("/var/log/icecast2")
}

fn default_icecast_admin_user() -> String {
    "admin".to_string()
}

fn default_icecast_poll_interval() -> u64 {
    15
}

/// Bounds of `[icecast] poll_interval` (s).
const ICECAST_POLL_RANGE: std::ops::RangeInclusive<u64> = 2..=600;

impl IcecastConfig {
    /// Loud checks at load. `ls` = the `[liquidsoap]` section: its outputs
    /// are the mounts sampled, so `[icecast]` without it is refused.
    pub fn validate(&self, ls: Option<&LiquidsoapConfig>) -> Result<(), String> {
        self.authority()?;
        if self.admin_user.is_empty() || self.admin_user.contains(':') || !printable_ascii(&self.admin_user) {
            return Err(format!(
                "admin_user {:?}: non-empty printable ASCII without ':' required (HTTP Basic auth)",
                self.admin_user
            ));
        }
        if self.admin_password.is_empty() || !printable_ascii(&self.admin_password) {
            return Err("admin_password: non-empty printable ASCII required".into());
        }
        if !ICECAST_POLL_RANGE.contains(&self.poll_interval) {
            return Err(format!(
                "poll_interval {} out of {}..={} s",
                self.poll_interval,
                ICECAST_POLL_RANGE.start(),
                ICECAST_POLL_RANGE.end()
            ));
        }
        let ls = match ls {
            Some(ls) if !ls.outputs.is_empty() => ls,
            _ => return Err("[icecast] needs [liquidsoap] outputs: they are the mounts it watches".into()),
        };
        if let Some(srv) = &self.server {
            self.validate_server(srv, ls)?;
        }
        Ok(())
    }

    /// `[icecast.server]`: stationd owns Icecast's config, so every
    /// contradiction with it is an error (a mount generated for nobody, a
    /// sampler reading another server, admin = source password).
    fn validate_server(&self, srv: &IcecastServerConfig, ls: &LiquidsoapConfig) -> Result<(), String> {
        if srv.port == 0 {
            return Err("server.port must not be 0".into());
        }
        let admin_port = self.authority()?.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok());
        if admin_port != Some(srv.port) {
            return Err(format!(
                "admin_url {:?} does not target server.port {}: stationd would read another Icecast than the one it configures",
                self.admin_url, srv.port
            ));
        }
        for o in &ls.outputs {
            if o.port != srv.port {
                return Err(format!(
                    "output {} targets port {}, not server.port {}: its mount would be generated for nobody",
                    o.mount, o.port, srv.port
                ));
            }
            if o.password.is_empty() || !printable_ascii(&o.password) {
                return Err(format!("output {}: source password must be non-empty printable ASCII", o.mount));
            }
            if o.password == self.admin_password {
                return Err(format!(
                    "output {}: source password equals admin_password — they must differ",
                    o.mount
                ));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for o in &ls.outputs {
            if !seen.insert(o.mount.as_str()) {
                return Err(format!("mount {} declared twice in [[liquidsoap.output]]", o.mount));
            }
        }
        if let Some(b) = &srv.bind_address {
            b.parse::<std::net::IpAddr>()
                .map_err(|_| format!("server.bind_address {b:?} is not an IP address"))?;
        }
        let mut proxies = std::collections::HashSet::new();
        for p in &srv.trusted_proxies {
            let ip: std::net::IpAddr = p.parse().map_err(|_| {
                format!("server.trusted_proxies: {p:?} is not an IP address (one exact address per proxy, no CIDR)")
            })?;
            if !proxies.insert(ip) {
                return Err(format!("server.trusted_proxies: {p} listed twice"));
            }
        }
        for (what, v) in [("hostname", &srv.hostname), ("admin_email", &srv.admin_email)] {
            if v.trim().is_empty() || v.contains(char::is_whitespace) {
                return Err(format!("server.{what} {v:?}: non-empty, no spaces"));
            }
        }
        if srv.max_clients == 0 {
            return Err("server.max_clients must be > 0".into());
        }
        if let Some(g) = &srv.file_group {
            if g.is_empty() || !g.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)) {
                return Err(format!("server.file_group {g:?}: a group name (letters, digits, _ - .)"));
            }
        }
        if srv.config_path == ls.script_path {
            return Err("server.config_path is the Liquidsoap script path".into());
        }
        Ok(())
    }

    /// `host:port` of `admin_url` (port 80 when absent). Refuses anything but
    /// a bare `http://host[:port][/]`.
    pub fn authority(&self) -> Result<String, String> {
        let url = self.admin_url.trim();
        if url.starts_with("https://") {
            return Err(format!(
                "admin_url {url:?}: https not supported — stationd reads Icecast directly, not through the reverse proxy"
            ));
        }
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("admin_url {url:?} must start with http://"))?;
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) {
            return Err(format!("admin_url {url:?}: expected http://host[:port] (no path, no credentials)"));
        }
        let with_port = match rest.rsplit_once(':') {
            // "host:port" (an IPv6 literal is bracketed: "[::1]:8000").
            Some((host, port)) if !host.is_empty() && !port.contains(']') => {
                port.parse::<u16>()
                    .map_err(|_| format!("admin_url {url:?}: bad port {port:?}"))?;
                rest.to_string()
            }
            _ => format!("{rest}:80"),
        };
        Ok(with_port)
    }

    /// Outputs whose `host:port` differs from `admin_url` (textually): likely
    /// a different Icecast than the one sampled. Warned at start-up, not
    /// refused (`localhost` vs `127.0.0.1` is legitimate).
    pub fn foreign_outputs<'a>(&self, ls: &'a LiquidsoapConfig) -> Vec<&'a IcecastOutput> {
        let Ok(admin) = self.authority() else { return Vec::new() };
        ls.outputs
            .iter()
            .filter(|o| format!("{}:{}", o.host, o.port) != admin)
            .collect()
    }
}

fn printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// `[liquidsoap]` — the generated script and the loopback bridge Liquidsoap
/// pulls from. Liquidsoap itself is started by its own unit (systemd/Docker):
/// stationd only writes the script, so stationd can restart without cutting
/// the air.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiquidsoapConfig {
    /// Where the generated `.liq` is written at start-up (rewritten only when
    /// its content changed; Liquidsoap must be restarted to pick it up).
    pub script_path: PathBuf,
    /// Loopback HTTP bridge (Liquidsoap → stationd). Must be a loopback
    /// address: the bridge is never exposed (checked at load).
    #[serde(default = "default_ls_http_bind")]
    pub http_bind: String,
    /// Liquidsoap's control socket (stationd → Liquidsoap: pause, resume,
    /// skip). Created by Liquidsoap (mode 0660): the stationd user must be in
    /// Liquidsoap's group.
    #[serde(default = "default_ls_control_socket")]
    pub control_socket: PathBuf,
    /// Shared secret Liquidsoap sends in the `X-Stationd-Token` header.
    pub api_token: String,
    /// Safety fallback, looped when stationd has nothing to air (no rule
    /// covers now, every pool empty, stationd unreachable).
    pub fallback_path: PathBuf,
    /// Background noise looped while the station is halted (paused/stopped):
    /// keeps the stream occupied. Distinct from the safety fallback.
    pub halted_path: PathBuf,
    #[serde(default)]
    pub crossfade: CrossfadeConfig,
    /// Loudness normalisation + compression on the air chain (same settings
    /// as the AzuraCast station profile). Off by default.
    #[serde(default)]
    pub normalize: bool,
    /// Optional user snippet `%include`d just before the outputs. It sees the
    /// air source as `radio` and may reassign it (metadata.map, …).
    #[serde(default)]
    pub custom_include: Option<PathBuf>,
    /// Liquidsoap log level (1 = critical … 5 = debug).
    #[serde(default = "default_ls_log_level")]
    pub log_level: u8,
    /// Icecast outputs (at least one). TOML key `[[liquidsoap.output]]`.
    #[serde(default, rename = "output")]
    pub outputs: Vec<IcecastOutput>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossfadeConfig {
    #[serde(default)]
    pub mode: CrossfadeMode,
    /// Fade-in / fade-out length, seconds.
    #[serde(default = "default_fade")]
    pub fade: f64,
    /// Overlap between two tracks, seconds.
    #[serde(default = "default_cross")]
    pub duration: f64,
}

impl Default for CrossfadeConfig {
    fn default() -> Self {
        Self { mode: CrossfadeMode::default(), fade: default_fade(), duration: default_cross() }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossfadeMode {
    /// Hard cut between tracks.
    None,
    /// Fade out / fade in over `duration` (AzuraCast "normal").
    #[default]
    Simple,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IcecastOutput {
    pub host: String,
    pub port: u16,
    pub password: String,
    /// Mount point, starting with `/` (e.g. "/radio.mp3").
    pub mount: String,
    #[serde(default)]
    pub format: OutputFormat,
    /// kbit/s.
    #[serde(default = "default_bitrate")]
    pub bitrate: u32,
    /// Stream name shown by Icecast. Default: the station name.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    /// Listed in the public Icecast directory.
    #[serde(default)]
    pub public: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Mp3,
}

fn default_ls_http_bind() -> String {
    "127.0.0.1:8081".to_string()
}
fn default_ls_control_socket() -> PathBuf {
    PathBuf::from("./data/liquidsoap.sock")
}
fn default_ls_log_level() -> u8 {
    3
}
fn default_fade() -> f64 {
    2.0
}
fn default_cross() -> f64 {
    3.0
}
fn default_bitrate() -> u32 {
    192
}

/// MP3 bitrates LAME accepts (kbit/s).
const MP3_BITRATES: &[u32] = &[32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];

impl LiquidsoapConfig {
    /// Loud checks at load: a bad `[liquidsoap]` stops start-up rather than
    /// producing a script Liquidsoap rejects (or, worse, a bridge exposed on
    /// the network).
    pub fn validate(&self) -> Result<(), String> {
        let addr: std::net::SocketAddr = self
            .http_bind
            .parse()
            .map_err(|e| format!("http_bind {:?}: {e}", self.http_bind))?;
        if !addr.ip().is_loopback() {
            return Err(format!(
                "http_bind {:?} is not a loopback address: the Liquidsoap bridge is never exposed",
                self.http_bind
            ));
        }
        // sun_path is 108 bytes (NUL included) on Linux: a longer path makes
        // Liquidsoap fail to create the socket.
        let sock = std::path::absolute(&self.control_socket)
            .unwrap_or_else(|_| self.control_socket.clone());
        let sock_len = sock.as_os_str().len();
        if sock_len >= 108 {
            return Err(format!(
                "control_socket {sock:?} is {sock_len} bytes long: a Unix socket path must be < 108"
            ));
        }
        if self.api_token.trim().is_empty() {
            return Err("api_token is empty".into());
        }
        // Sent by Liquidsoap in an HTTP header: a non-ASCII character (a `…`
        // pasted from an example) would make every pull a silent 401.
        if !printable_ascii(&self.api_token) {
            return Err("api_token: printable ASCII only (it travels in an HTTP header)".into());
        }
        if !(1..=5).contains(&self.log_level) {
            return Err(format!("log_level {} out of 1..=5", self.log_level));
        }
        let cf = &self.crossfade;
        if !(cf.fade.is_finite() && cf.fade >= 0.0 && cf.duration.is_finite() && cf.duration >= 0.0) {
            return Err("crossfade.fade / crossfade.duration must be finite and >= 0".into());
        }
        if let Some(inc) = &self.custom_include {
            let s = inc.to_string_lossy();
            if s.contains('"') || s.contains('\n') {
                return Err(format!("custom_include {s:?}: quotes/newlines not allowed"));
            }
        }
        if self.outputs.is_empty() {
            return Err("no [[liquidsoap.output]]: the script would air nowhere".into());
        }
        for (i, o) in self.outputs.iter().enumerate() {
            let n = i + 1;
            if !o.mount.starts_with('/') {
                return Err(format!("output #{n}: mount {:?} must start with '/'", o.mount));
            }
            if o.host.trim().is_empty() {
                return Err(format!("output #{n}: empty host"));
            }
            match o.format {
                OutputFormat::Mp3 if !MP3_BITRATES.contains(&o.bitrate) => {
                    return Err(format!(
                        "output #{n}: mp3 bitrate {} not in {MP3_BITRATES:?}",
                        o.bitrate
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Start-up check of the files Liquidsoap plays on its own (`fallback_path`,
    /// `halted_path`) — I/O, so kept out of [`Self::validate`]. `Err` (start-up
    /// refused): missing, not a regular file, empty, or unreadable by stationd.
    /// `Ok(warnings)`: a file not readable by "others" — Liquidsoap runs as
    /// another user and stationd cannot check its rights; it then needs read
    /// access through one of its groups (Docker: the media group `MEDIA_GID`,
    /// or `stationd`). Relative paths are resolved against the CWD, like the
    /// script does.
    pub fn check_air_files(&self) -> Result<Vec<String>, String> {
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;
        let mut warnings = Vec::new();
        for (what, path) in [("fallback_path", &self.fallback_path), ("halted_path", &self.halted_path)] {
            let meta = std::fs::metadata(path)
                .map_err(|e| format!("[liquidsoap] {what} {path:?}: {e} (Liquidsoap cannot start without it)"))?;
            if !meta.is_file() {
                return Err(format!("[liquidsoap] {what} {path:?} is not a regular file"));
            }
            if meta.len() == 0 {
                return Err(format!("[liquidsoap] {what} {path:?} is empty"));
            }
            let mut byte = [0u8; 1];
            std::fs::File::open(path)
                .and_then(|mut f| f.read(&mut byte))
                .map_err(|e| format!("[liquidsoap] {what} {path:?} is not readable by stationd: {e}"))?;
            if meta.mode() & 0o004 == 0 {
                warnings.push(format!(
                    "[liquidsoap] {what} {path:?} is not readable by others (mode {:o}, uid {}, gid {}): \
                     Liquidsoap runs as another user and needs read access through one of its groups \
                     (Docker: MEDIA_GID or stationd), or it stops on a failed fallback",
                    meta.mode() & 0o7777,
                    meta.uid(),
                    meta.gid()
                ));
            }
        }
        Ok(warnings)
    }

    /// The bridge address (validated loopback).
    pub fn http_addr(&self) -> std::net::SocketAddr {
        self.http_bind.parse().expect("validated at load")
    }
}

#[derive(Debug, Deserialize)]
pub struct StationConfig {
    pub name: String,
    /// IANA timezone of the station (e.g. "Europe/Paris"). One reference zone
    /// per node: the scheduler resolves every civil time against it. Validated
    /// at load — an unknown name stops start-up rather than mis-scheduling
    /// silently later.
    pub timezone: String,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// Listen address for the future gRPC server (api + CLI will connect here).
    pub grpc_bind: String,
}

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct MediaConfig {
    pub library_path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct PlaylistConfig {
    /// Directory holding the per-playlist TOML files. These files are the
    /// source of truth (file-first): SQLite is only a rebuildable view of
    /// them. Edited both by a human (editor / `git pull`) and by stationd
    /// itself — never by `api`.
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid TOML config in {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unknown IANA station timezone {name:?}")]
    UnknownTimeZone { name: String },
    #[error("invalid [liquidsoap] section: {0}")]
    Liquidsoap(String),
    #[error("invalid [icecast] section: {0}")]
    Icecast(String),
    #[error("invalid [live] section: {0}")]
    Live(String),
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let config: Config = toml::from_str(&contents).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        // Fail fast on an unknown station timezone: the scheduler resolves all
        // civil times against it, so a bogus name must stop start-up, not
        // surface later as silent mis-scheduling.
        jiff::tz::TimeZone::get(&config.station.timezone).map_err(|_| {
            ConfigError::UnknownTimeZone {
                name: config.station.timezone.clone(),
            }
        })?;
        if let Some(ls) = &config.liquidsoap {
            ls.validate().map_err(ConfigError::Liquidsoap)?;
        }
        if let Some(ic) = &config.icecast {
            ic.validate(config.liquidsoap.as_ref()).map_err(ConfigError::Icecast)?;
        }
        if let Some(live) = &config.live {
            let mut ports: Vec<u16> =
                config.liquidsoap.iter().flat_map(|ls| ls.outputs.iter().map(|o| o.port)).collect();
            if let Some(srv) = config.icecast.as_ref().and_then(|i| i.server.as_ref()) {
                ports.push(srv.port);
            }
            live.validate(config.liquidsoap.as_ref(), &ports).map_err(ConfigError::Live)?;
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let toml_str = r#"
            [station]
            name = "Test Radio"
            timezone = "Europe/Paris"

            [server]
            grpc_bind = "127.0.0.1:50051"

            [database]
            path = "./data/stationd.db"

            [media]
            library_path = "./media"

            [playlist]
            path = "./playlist"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.station.name, "Test Radio");
        assert_eq!(config.station.timezone, "Europe/Paris");
        assert_eq!(config.playlist.path, PathBuf::from("./playlist"));
        assert_eq!(config.logging.level, "info"); // default value
    }

    #[test]
    fn rejects_unknown_timezone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(
            &p,
            r#"
            [station]
            name = "R"
            timezone = "Mars/Olympus_Mons"
            [server]
            grpc_bind = "127.0.0.1:50051"
            [database]
            path = "./d.db"
            [media]
            library_path = "./media"
            [playlist]
            path = "./playlist"
        "#,
        )
        .unwrap();
        // A syntactically valid config with a bogus zone must not start up.
        assert!(matches!(
            Config::load(&p),
            Err(ConfigError::UnknownTimeZone { .. })
        ));
    }

    const BASE: &str = r#"
        [station]
        name = "R"
        timezone = "Europe/Paris"
        [server]
        grpc_bind = "127.0.0.1:50051"
        [database]
        path = "./d.db"
        [media]
        library_path = "./media"
        [playlist]
        path = "./playlist"
    "#;

    const LS: &str = r#"
        [liquidsoap]
        script_path = "./data/station.liq"
        api_token = "s3cret"
        fallback_path = "/srv/error.mp3"
        halted_path = "/srv/noise.mp3"
        [[liquidsoap.output]]
        host = "127.0.0.1"
        port = 8000
        password = "hackme"
        mount = "/radio.mp3"
    "#;

    fn load_str(extra: &str) -> Result<Config, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, format!("{BASE}{extra}")).unwrap();
        Config::load(&p)
    }

    #[test]
    fn liquidsoap_section_is_optional() {
        assert!(load_str("").unwrap().liquidsoap.is_none());
    }

    #[test]
    fn liquidsoap_defaults() {
        let c = load_str(LS).unwrap();
        let ls = c.liquidsoap.unwrap();
        assert_eq!(ls.http_bind, "127.0.0.1:8081");
        assert_eq!(ls.control_socket, PathBuf::from("./data/liquidsoap.sock"));
        assert_eq!(ls.crossfade.mode, CrossfadeMode::Simple);
        assert_eq!(ls.crossfade.duration, 3.0);
        assert_eq!(ls.log_level, 3);
        assert!(!ls.normalize);
        assert_eq!(ls.outputs.len(), 1);
        assert_eq!(ls.outputs[0].bitrate, 192);
        assert_eq!(ls.outputs[0].format, OutputFormat::Mp3);
    }

    #[test]
    fn liquidsoap_bridge_must_be_loopback() {
        let extra = LS.replace("api_token", "http_bind = \"0.0.0.0:8081\"\n        api_token");
        assert!(matches!(load_str(&extra), Err(ConfigError::Liquidsoap(m)) if m.contains("loopback")));
    }

    #[test]
    fn liquidsoap_rejects_bad_settings() {
        let no_output = LS.split("[[liquidsoap.output]]").next().unwrap().to_string();
        assert!(matches!(load_str(&no_output), Err(ConfigError::Liquidsoap(m)) if m.contains("output")));
        let bad_mount = LS.replace("\"/radio.mp3\"", "\"radio.mp3\"");
        assert!(matches!(load_str(&bad_mount), Err(ConfigError::Liquidsoap(m)) if m.contains("mount")));
        let bad_rate = format!("{LS}        bitrate = 100\n");
        assert!(matches!(load_str(&bad_rate), Err(ConfigError::Liquidsoap(m)) if m.contains("bitrate")));
        let empty_token = LS.replace("\"s3cret\"", "\"  \"");
        assert!(matches!(load_str(&empty_token), Err(ConfigError::Liquidsoap(m)) if m.contains("api_token")));
        // A non-ASCII (or control) character in the token: refused at load.
        let fancy = LS.replace("\"s3cret\"", "\"s3cret…\"");
        assert!(matches!(load_str(&fancy), Err(ConfigError::Liquidsoap(m)) if m.contains("printable ASCII")));
        let tab = LS.replace("\"s3cret\"", "\"s3\\tcret\"");
        assert!(matches!(load_str(&tab), Err(ConfigError::Liquidsoap(m)) if m.contains("api_token")));
        // deny_unknown_fields: a typo is a parse error, never ignored.
        let typo = LS.replace("halted_path", "haltd_path");
        assert!(matches!(load_str(&typo), Err(ConfigError::Parse { .. })));
    }

    const IC: &str = r#"
        [icecast]
        admin_url = "http://127.0.0.1:8000"
        admin_password = "adm1n"
    "#;

    #[test]
    fn icecast_section_is_optional_and_has_defaults() {
        assert!(load_str(LS).unwrap().icecast.is_none());
        let c = load_str(&format!("{LS}{IC}")).unwrap();
        let ic = c.icecast.unwrap();
        assert_eq!(ic.admin_user, "admin");
        assert_eq!(ic.poll_interval, 15);
        assert_eq!(ic.authority().unwrap(), "127.0.0.1:8000");
        assert!(ic.foreign_outputs(c.liquidsoap.as_ref().unwrap()).is_empty());
    }

    #[test]
    fn icecast_requires_liquidsoap_outputs() {
        assert!(matches!(load_str(IC), Err(ConfigError::Icecast(m)) if m.contains("[liquidsoap]")));
    }

    #[test]
    fn icecast_admin_url_forms() {
        let ic = |url: &str| IcecastConfig {
            admin_url: url.into(),
            admin_user: "admin".into(),
            admin_password: "x".into(),
            poll_interval: 15,
            server: None,
        };
        assert_eq!(ic("http://icecast.lan/").authority().unwrap(), "icecast.lan:80");
        assert_eq!(ic("http://[::1]:8000").authority().unwrap(), "[::1]:8000");
        assert_eq!(ic("http://[::1]").authority().unwrap(), "[::1]:80");
        let https = ic("https://radio.example").authority().unwrap_err();
        assert!(https.contains("reverse proxy"), "{https}");
        for bad in ["127.0.0.1:8000", "http://", "http://h:8000/admin", "http://u:p@h:8000", "http://h:x"] {
            assert!(ic(bad).authority().is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn icecast_rejects_bad_settings() {
        let full = format!("{LS}{IC}");
        let empty_pw = full.replace("\"adm1n\"", "\"\"");
        assert!(matches!(load_str(&empty_pw), Err(ConfigError::Icecast(m)) if m.contains("admin_password")));
        let non_ascii = full.replace("\"adm1n\"", "\"adm1n…\"");
        assert!(matches!(load_str(&non_ascii), Err(ConfigError::Icecast(m)) if m.contains("admin_password")));
        let colon_user = format!("{full}        admin_user = \"a:b\"\n");
        assert!(matches!(load_str(&colon_user), Err(ConfigError::Icecast(m)) if m.contains("admin_user")));
        let fast = format!("{full}        poll_interval = 1\n");
        assert!(matches!(load_str(&fast), Err(ConfigError::Icecast(m)) if m.contains("poll_interval")));
        let typo = full.replace("admin_password", "admin_pasword");
        assert!(matches!(load_str(&typo), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn icecast_flags_outputs_on_another_server() {
        let c = load_str(&format!("{LS}{IC}").replace("\"http://127.0.0.1:8000\"", "\"http://localhost:8000\"")).unwrap();
        let foreign = c.icecast.as_ref().unwrap().foreign_outputs(c.liquidsoap.as_ref().unwrap());
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].mount, "/radio.mp3");
    }

    const SRV: &str = r#"
        [icecast.server]
        trusted_proxies = ["192.168.1.94"]
    "#;

    #[test]
    fn icecast_server_defaults() {
        let c = load_str(&format!("{LS}{IC}{SRV}")).unwrap();
        let srv = c.icecast.unwrap().server.unwrap();
        assert_eq!(srv.port, 8000);
        assert_eq!(srv.config_path, PathBuf::from("./data/icecast.xml"));
        assert_eq!(srv.share_dir, PathBuf::from("/usr/share/icecast2"));
        assert_eq!(srv.trusted_proxies, vec!["192.168.1.94".to_string()]);
    }

    #[test]
    fn icecast_server_rejects_contradictions() {
        let full = format!("{LS}{IC}{SRV}");
        let other_port = full.replace("\"http://127.0.0.1:8000\"", "\"http://127.0.0.1:8001\"");
        assert!(matches!(load_str(&other_port), Err(ConfigError::Icecast(m)) if m.contains("server.port")));
        let out_port = full.replace("port = 8000", "port = 8002");
        assert!(matches!(load_str(&out_port), Err(ConfigError::Icecast(m)) if m.contains("for nobody")));
        let same_pw = full.replace("\"adm1n\"", "\"hackme\"");
        assert!(matches!(load_str(&same_pw), Err(ConfigError::Icecast(m)) if m.contains("must differ")));
        let cidr = full.replace("\"192.168.1.94\"", "\"192.168.1.0/24\"");
        assert!(matches!(load_str(&cidr), Err(ConfigError::Icecast(m)) if m.contains("no CIDR")));
        let twice = full.replace("[\"192.168.1.94\"]", "[\"192.168.1.94\", \"192.168.1.94\"]");
        assert!(matches!(load_str(&twice), Err(ConfigError::Icecast(m)) if m.contains("twice")));
        let typo = format!("{full}        trusted_proxy = []\n");
        assert!(matches!(load_str(&typo), Err(ConfigError::Parse { .. })));
        // Without [icecast.server], the same passwords are not stationd's business.
        let read_only = format!("{LS}{IC}").replace("\"adm1n\"", "\"hackme\"");
        assert!(load_str(&read_only).is_ok());
    }

    const LIVE: &str = r#"
        [live]
        djs_path = "./djs.toml"
    "#;

    #[test]
    fn live_section_defaults() {
        assert!(load_str(LS).unwrap().live.is_none());
        let live = load_str(&format!("{LS}{LIVE}")).unwrap().live.unwrap();
        assert_eq!(live.djs_path, PathBuf::from("./djs.toml"));
        assert_eq!(live.harbor_port, 8005);
        assert_eq!(live.mount, "/live");
        assert_eq!(live.fade, 1.5);
        assert_eq!(live.silence_timeout, 30);
        assert_eq!(live.silence_threshold, -40.0);
        assert_eq!(live.buffer, 5.0);
    }

    #[test]
    fn live_rejects_bad_settings() {
        let full = format!("{LS}{LIVE}");
        let live_err = |extra: &str, want: &str| {
            let r = load_str(&format!("{full}        {extra}\n"));
            assert!(matches!(&r, Err(ConfigError::Live(m)) if m.contains(want)), "{extra}: {r:?}");
        };
        live_err("harbor_port = 8000", "Icecast's port");
        live_err("mount = \"live\"", "mount");
        live_err("mount = \"/li ve\"", "mount");
        live_err("fade = 20.0", "fade");
        live_err("silence_timeout = 2", "silence_timeout");
        live_err("silence_threshold = 3.0", "silence_threshold");
        live_err("buffer = 0.0", "buffer");
        assert!(matches!(load_str(&format!("{full}        harbor = 1\n")), Err(ConfigError::Parse { .. })));
        // the harbor lives in the generated script
        assert!(matches!(load_str(LIVE), Err(ConfigError::Live(m)) if m.contains("[liquidsoap]")));
    }

    #[test]
    fn rejects_missing_required_field() {
        let toml_str = r#"
            [station]
            name = "Test Radio"
        "#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn air_files_are_checked_at_start_up() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, content: &[u8], mode: u32| {
            let p = dir.path().join(name);
            std::fs::write(&p, content).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            p
        };
        let good = file("noise.mp3", b"ID3", 0o644);
        let mut ls = load_str(LS).unwrap().liquidsoap.unwrap();
        ls.fallback_path = good.clone();
        ls.halted_path = good.clone();
        assert_eq!(ls.check_air_files(), Ok(vec![]));

        // Group-only (640 / 660): fine for stationd, a warning for Liquidsoap.
        ls.fallback_path = file("error.mp3", b"ID3", 0o660);
        let w = ls.check_air_files().unwrap();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("fallback_path") && w[0].contains("mode 660"), "{w:?}");

        // Refused: missing, a directory, empty.
        ls.fallback_path = dir.path().join("missing.mp3");
        assert!(ls.check_air_files().unwrap_err().contains("fallback_path"));
        ls.fallback_path = good.clone();
        ls.halted_path = dir.path().to_path_buf();
        assert!(ls.check_air_files().unwrap_err().contains("not a regular file"));
        ls.halted_path = file("empty.mp3", b"", 0o644);
        assert!(ls.check_air_files().unwrap_err().contains("empty"));

        // Unreadable by stationd itself (not testable as root: root reads all).
        if unsafe { libc::geteuid() } != 0 {
            ls.halted_path = file("locked.mp3", b"ID3", 0o000);
            assert!(ls.check_air_files().unwrap_err().contains("not readable by stationd"));
        }
    }
}
