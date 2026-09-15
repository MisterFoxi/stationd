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
