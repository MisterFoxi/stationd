//! Shared, secret-free Icecast configuration schema for V1.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    net::IpAddr,
    path::{Path, PathBuf},
};

#[rustfmt::skip]
#[path = "../../../src/geoip.rs"]
pub mod geoip;
pub const GEOIP_DIR: &str = "/data/geoip";
pub const MAX_CONFIG_BYTES: usize = 256 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub location: String,
    pub hostname: String,
    pub admin_email: String,
    pub port: u16,
    pub max_clients: u32,
    pub admin_user: String,
    pub admin_password_file: String,
    #[serde(default)]
    pub trusted_proxies: Vec<IpAddr>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub logging: Logging,
    #[serde(default)]
    pub geoip: GeoipSettings,
    pub mounts: Vec<Mount>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub name: String,
    pub password_file: String,
    #[serde(default = "yes")]
    pub audience: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub max_listeners: Option<u32>,
    #[serde(default)]
    pub fallback_mount: Option<String>,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    pub queue_size: u32,
    pub burst_size: u32,
    pub client_timeout: u16,
    pub header_timeout: u16,
    pub source_timeout: u16,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            queue_size: 524288,
            burst_size: 8192,
            client_timeout: 30,
            header_timeout: 15,
            source_timeout: 10,
        }
    }
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    #[default]
    Information,
    Warning,
    Error,
}
impl LogLevel {
    fn text(&self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Information => "information",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Logging {
    pub level: LogLevel,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct GeoipSettings {
    pub enabled: bool,
    pub database_file: String,
}
impl Default for GeoipSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            database_file: "GeoLite2-City.mmdb".into(),
        }
    }
}
pub fn revision(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_CONFIG_BYTES {
            bail!("Icecast TOML exceeds 256 KiB");
        }
        let config: Self = toml::from_str(text).map_err(|_| {
            anyhow::anyhow!(
                "invalid Icecast TOML (schema or syntax; inline secrets are not accepted)"
            )
        })?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        // This Docker deployment has one fixed internal port. Host bind/port are separate.
        if self.port != 8000 {
            bail!("internal Icecast port must be 8000; use ICECAST_PORT for the published port");
        }
        if self.max_clients == 0
            || self.max_clients > 32768
            || self.mounts.is_empty()
            || self.mounts.len() > 64
        {
            bail!("max_clients must be 1..32768 and mounts 1..64");
        }
        for value in [
            &self.location,
            &self.hostname,
            &self.admin_email,
            &self.admin_user,
        ] {
            if value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control)
            {
                bail!("invalid server identity field");
            }
        }
        validate_secret_name(&self.admin_password_file)?;
        let mut names = HashSet::new();
        for m in &self.mounts {
            if !valid_mount(&m.name) || !names.insert(m.name.as_str()) {
                bail!("invalid or duplicate mount name");
            }
            validate_secret_name(&m.password_file)?;
            if m.max_listeners
                .is_some_and(|n| n == 0 || n > self.max_clients)
            {
                bail!("mount max_listeners must be 1..max_clients");
            }
        }
        for m in &self.mounts {
            let mut seen = HashSet::new();
            let mut current = Some(m);
            while let Some(node) = current {
                if !seen.insert(&node.name) {
                    bail!("fallback cycle");
                }
                current = match &node.fallback_mount {
                    Some(name) => Some(
                        self.mounts
                            .iter()
                            .find(|m| m.name == *name)
                            .context("fallback mount not declared")?,
                    ),
                    None => None,
                };
            }
        }
        let limits = &self.limits;
        if !(8192..=16 * 1024 * 1024).contains(&limits.queue_size)
            || limits.burst_size > limits.queue_size
            || [
                limits.client_timeout,
                limits.header_timeout,
                limits.source_timeout,
            ]
            .iter()
            .any(|t| !(2..=600).contains(t))
        {
            bail!("invalid buffer or timeout limits");
        }
        let mut proxies = HashSet::new();
        if self.trusted_proxies.len() > 16 {
            bail!("at most 16 trusted proxy addresses");
        }
        for ip in &self.trusted_proxies {
            if ip.is_unspecified() || ip.is_multicast() || !proxies.insert(ip) {
                bail!("trusted proxies must be distinct explicit IP addresses");
            }
        }
        let file = &self.geoip.database_file;
        if file.starts_with('.')
            || !file.ends_with(".mmdb")
            || file.len() > 128
            || !file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            bail!("GeoIP database_file must be a plain MMDB filename");
        }
        Ok(())
    }
    pub fn geoip_path(&self) -> PathBuf {
        Path::new(GEOIP_DIR).join(&self.geoip.database_file)
    }
    pub fn open_geoip(&self) -> Result<Option<geoip::Geoip>> {
        if !self.geoip.enabled {
            return Ok(None);
        }
        geoip::Geoip::open(&self.geoip_path())
            .map(Some)
            .map_err(anyhow::Error::msg)
    }
    pub fn check_assets(&self, secrets: &Path) -> Result<()> {
        secret(secrets, &self.admin_password_file)?;
        for m in &self.mounts {
            secret(secrets, &m.password_file)?;
        }
        self.open_geoip()?;
        Ok(())
    }
}
fn valid_mount(s: &str) -> bool {
    s.starts_with('/')
        && s.len() > 1
        && s.len() <= 256
        && !s.starts_with("//")
        && !s.starts_with("/admin")
        && !s.contains("..")
        && !s.contains('%')
        && !s
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "?#\\".contains(c))
}
fn validate_secret_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        bail!("invalid secret file name");
    }
    Ok(())
}
pub fn secret(dir: &Path, name: &str) -> Result<String> {
    validate_secret_name(name)?;
    let value = std::fs::read_to_string(dir.join(name))
        .with_context(|| format!("cannot read secret file {name}"))?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.chars().any(char::is_control) {
        bail!("empty or invalid secret file {name}");
    }
    Ok(value.to_owned())
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn render(text: &str, secrets: &Path) -> Result<String> {
    let c = Config::parse(text)?;
    c.check_assets(secrets)?;
    let mut out = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- Generated by station-icecast-config. Contains secrets: runtime only. -->
<icecast>
  <location>{}</location><admin>{}</admin><hostname>{}</hostname>
  <limits>
    <clients>{}</clients><sources>{}</sources>
    <queue-size>{}</queue-size><burst-size>{}</burst-size>
    <client-timeout>{}</client-timeout><header-timeout>{}</header-timeout><source-timeout>{}</source-timeout>
  </limits>
  <authentication><admin-user>{}</admin-user><admin-password>{}</admin-password></authentication>
  <listen-socket id="public"><port>8000</port><bind-address>0.0.0.0</bind-address>
"#,
        xml(&c.location),
        xml(&c.admin_email),
        xml(&c.hostname),
        c.max_clients,
        c.mounts.len(),
        c.limits.queue_size,
        c.limits.burst_size,
        c.limits.client_timeout,
        c.limits.header_timeout,
        c.limits.source_timeout,
        xml(&c.admin_user),
        xml(&secret(secrets, &c.admin_password_file)?)
    );
    for i in 0..c.trusted_proxies.len() {
        out.push_str(&format!("    <trusted-proxy>#proxy-{i}</trusted-proxy>\n"));
    }
    out.push_str("  </listen-socket>\n");
    for (i, ip) in c.trusted_proxies.iter().enumerate() {
        out.push_str(&format!("  <listen-socket id=\"proxy-{i}\" type=\"virtual\"><client-address>{ip}</client-address></listen-socket>\n"));
    }
    out.push_str("  <http-headers><header type=\"cors\" name=\"Access-Control-Allow-Origin\"/><header type=\"cors\" name=\"Access-Control-Allow-Headers\"/><header type=\"cors\" name=\"Access-Control-Expose-Headers\"/></http-headers>\n");
    for m in &c.mounts {
        out.push_str(&format!("  <mount type=\"normal\"><mount-name>{}</mount-name><password>{}</password><charset>UTF-8</charset><hidden>{}</hidden>",
            xml(&m.name),xml(&secret(secrets,&m.password_file)?),u8::from(m.hidden)));
        if let Some(n) = m.max_listeners {
            out.push_str(&format!("<max-listeners>{n}</max-listeners>"));
        }
        if let Some(name) = &m.fallback_mount {
            out.push_str(&format!("<fallback-mount>{}</fallback-mount>", xml(name)));
        }
        out.push_str("</mount>\n");
    }
    out.push_str(
        r#"  <paths>
    <basedir>/usr/share/icecast2</basedir><logdir>/tmp</logdir>
    <webroot>/usr/share/icecast2/web</webroot><adminroot>/usr/share/icecast2/admin</adminroot>
    <reportxmldb>/usr/share/icecast2/report-db.xml</reportxmldb>
    <alias source="/" destination="/status.xsl"/>
"#,
    );
    if c.geoip.enabled {
        out.push_str(&format!(
            "    <geoipdb>{}</geoipdb>\n",
            xml(&c.geoip_path().to_string_lossy())
        ));
    }
    out.push_str(&format!("  </paths>\n  <logging><accesslog>-</accesslog><errorlog>-</errorlog><loglevel>{}</loglevel></logging>\n  <security><chroot>false</chroot></security>\n</icecast>\n",c.logging.level.text()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "admin-password",
            "source-a-password",
            "source-b-password",
            "source-radio-password",
        ] {
            std::fs::write(dir.path().join(name), "test<&'\"\n").unwrap();
        }
        (
            include_str!("../../../config/icecast/icecast.toml")
                .replace("enabled = true", "enabled = false"),
            dir,
        )
    }

    #[test]
    fn escapes_secrets_and_declares_only_central_mounts() {
        let (config, dir) = fixture();
        let rendered = render(&config, dir.path()).unwrap();
        let doc = roxmltree::Document::parse(&rendered).unwrap();
        assert!(!doc.descendants().any(|n| n.has_tag_name("source-password")));
        let mounts: Vec<_> = doc
            .descendants()
            .filter(|n| n.has_tag_name("mount-name"))
            .map(|n| n.text().unwrap())
            .collect();
        assert_eq!(mounts, ["/test-a.mp3", "/test-b.mp3", "/radio.mp3"]);
        let password = doc
            .descendants()
            .find(|n| n.has_tag_name("admin-password"))
            .unwrap();
        assert_eq!(password.text(), Some("test<&'\""));
    }

    #[test]
    fn rejects_unknown_fields_duplicate_mounts_paths_and_missing_secrets() {
        let (config, dir) = fixture();
        for invalid in [
            config.replace("port = 8000", "port = 0"),
            config.replace("port = 8000", "port = 8000\npassword = \"inline-secret\""),
            config.replace("/test-b.mp3", "/test-a.mp3"),
            config.replace("source-a-password", "../outside"),
            config.replace("source-a-password", "missing"),
            config.replace("/test-a.mp3", "/admin/stats"),
            config.replace("/test-a.mp3", "/bad mount"),
        ] {
            assert!(render(&invalid, dir.path()).is_err());
        }
    }

    #[test]
    fn refuses_empty_secret_without_disclosing_values() {
        let (config, dir) = fixture();
        std::fs::write(dir.path().join("admin-password"), "\n").unwrap();
        assert!(render(&config, dir.path()).is_err());
        let error = render(
            &config.replace("port = 8000", "port = \"sensitive-value\""),
            dir.path(),
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains("sensitive-value"));
    }
    #[test]
    fn refuses_fallback_cycles_unsafe_proxies_and_buffer_limits() {
        let (text, _) = fixture();
        for invalid in [
            text.replace("192.168.1.94", "0.0.0.0"),
            text.replace("burst_size = 8192", "burst_size = 999999999"),
            text.replace("queue_size = 524288", "queue_size = 0"),
            text.replace(
                "database_file = \"GeoLite2-City.mmdb\"",
                "database_file = \"../outside.mmdb\"",
            ),
            text.replace(
                "name = \"/test-a.mp3\"",
                "name = \"/test-a.mp3\"\nfallback_mount = \"/test-b.mp3\"",
            )
            .replace(
                "name = \"/test-b.mp3\"",
                "name = \"/test-b.mp3\"\nfallback_mount = \"/test-a.mp3\"",
            ),
        ] {
            assert!(Config::parse(&invalid).is_err());
        }
    }

    #[test]
    fn proxy_trust_is_scoped_to_explicit_virtual_socket() {
        let (text, dir) = fixture();
        let rendered = render(&text, dir.path()).unwrap();
        let doc = roxmltree::Document::parse(&rendered).unwrap();
        let proxy = doc
            .descendants()
            .find(|n| n.attribute("id") == Some("proxy-0"))
            .unwrap();
        assert_eq!(proxy.attribute("type"), Some("virtual"));
        assert_eq!(
            proxy
                .children()
                .find(|n| n.has_tag_name("client-address"))
                .unwrap()
                .text(),
            Some("192.168.1.94")
        );
        let public = doc
            .descendants()
            .find(|n| n.attribute("id") == Some("public"))
            .unwrap();
        assert_eq!(
            public
                .children()
                .find(|n| n.has_tag_name("trusted-proxy"))
                .unwrap()
                .text(),
            Some("#proxy-0")
        );
    }
}
