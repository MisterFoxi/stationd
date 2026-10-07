//! Resolve the operator client address without loading daemon business settings.
use anyhow::{bail, Context};
use serde::Deserialize;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
};

const DEFAULT_ADDR: &str = "http://127.0.0.1:50051";

#[derive(Deserialize)]
struct LocalConfig {
    server: Option<ServerConfig>,
}

#[derive(Deserialize)]
struct ServerConfig {
    grpc_bind: Option<String>,
}

pub(super) fn resolve(explicit: Option<&str>, root: &Path) -> anyhow::Result<String> {
    if let Some(addr) = explicit {
        return Ok(addr.to_owned());
    }
    let path = root.join("stationd.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_ADDR.to_owned());
        }
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    // Do not include parser diagnostics: the file can contain credentials.
    let config: LocalConfig = toml::from_str(&text).map_err(|_| {
        anyhow::anyhow!(
            "{}: invalid TOML or invalid [server].grpc_bind",
            path.display()
        )
    })?;
    let Some(bind) = config.server.and_then(|server| server.grpc_bind) else {
        return Ok(DEFAULT_ADDR.to_owned());
    };
    let mut addr: SocketAddr = bind.parse().with_context(|| {
        format!(
            "{}: server.grpc_bind must be an IP address with a port",
            path.display()
        )
    })?;
    if addr.port() == 0 {
        bail!(
            "{}: server.grpc_bind port must be nonzero for the client",
            path.display()
        );
    }
    // A wildcard is a listener address, not the client's destination.
    if addr.ip().is_unspecified() {
        addr.set_ip(match addr.ip() {
            IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
            IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
        });
    }
    Ok(format!("http://{addr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stationctl_address_uses_configured_lan_and_ipv6_and_normalizes_wildcards() {
        let root = tempfile::tempdir().unwrap();
        for (bind, expected) in [
            ("192.168.1.135:50051", "http://192.168.1.135:50051"),
            ("127.0.0.1:6000", "http://127.0.0.1:6000"),
            ("[fd00::135]:6000", "http://[fd00::135]:6000"),
            ("0.0.0.0:6000", "http://127.0.0.1:6000"),
            ("[::]:6000", "http://[::1]:6000"),
        ] {
            std::fs::write(root.path().join("stationd.toml"), format!(
                "[server]\ngrpc_bind = '{bind}'\n[station]\nname = 'test'\n[plugin.config]\nother = true\n"
            )).unwrap();
            assert_eq!(resolve(None, root.path()).unwrap(), expected);
        }
    }

    #[test]
    fn stationctl_address_explicit_override_ignores_invalid_local_config() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("stationd.toml"), "not TOML").unwrap();
        assert_eq!(
            resolve(Some("http://other:9000"), root.path()).unwrap(),
            "http://other:9000"
        );
    }

    #[test]
    fn stationctl_address_defaults_when_file_or_key_is_absent_and_reports_invalid_config() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(resolve(None, root.path()).unwrap(), DEFAULT_ADDR);
        for text in ["[station]\nname = 'test'", "[server]\nother = 1"] {
            std::fs::write(root.path().join("stationd.toml"), text).unwrap();
            assert_eq!(resolve(None, root.path()).unwrap(), DEFAULT_ADDR);
        }
        for text in [
            "password = 'secret'\n[",
            "[server]\ngrpc_bind = '192.168.1.135'",
            "[server]\ngrpc_bind = '127.0.0.1:0'",
        ] {
            std::fs::write(root.path().join("stationd.toml"), text).unwrap();
            let error = resolve(None, root.path()).unwrap_err().to_string();
            assert!(error.contains("stationd.toml"));
            assert!(!error.contains("secret"));
        }
        std::fs::remove_file(root.path().join("stationd.toml")).unwrap();
        std::fs::create_dir(root.path().join("stationd.toml")).unwrap();
        assert!(resolve(None, root.path()).is_err());
    }
}
