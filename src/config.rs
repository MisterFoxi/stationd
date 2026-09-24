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
        if self.api_token.trim().is_empty() {
            return Err("api_token is empty".into());
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
        // deny_unknown_fields: a typo is a parse error, never ignored.
        let typo = LS.replace("halted_path", "haltd_path");
        assert!(matches!(load_str(&typo), Err(ConfigError::Parse { .. })));
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
}
