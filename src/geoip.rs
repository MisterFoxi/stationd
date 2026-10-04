//! Local GeoLite2 City / DB-IP City Lite reader. IP persistence is opt-in for diagnostics only.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use maxminddb::{MaxMindDBError, Reader};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeoipConfig {
    /// Operator-controlled file; relative to stationd's working directory.
    pub database: PathBuf,
    /// Optional bounded JSONL capture of IPs and lookup results for debugging.
    #[serde(default)]
    pub debug_log: Option<PathBuf>,
}

/// Shared immutable reader. Reload by restarting stationd after file replacement.
pub struct Geoip {
    reader: Reader<Vec<u8>>,
    debug_log: Option<DebugLog>,
}

const DEBUG_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

struct DebugLog {
    // None after the first write failure or when the size limit is reached.
    file: Mutex<Option<std::fs::File>>,
}

impl DebugLog {
    fn open(path: &Path) -> std::io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { file: Mutex::new(Some(file)) })
    }

    fn record(&self, ip: IpAddr, database_type: &str, database_build_epoch: u64, result: &Result<Location, String>) {
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let Some(file) = guard.as_mut() else { return };
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_secs();
        let value = match result {
            Ok(location) => serde_json::json!({
                "at": at, "ip": ip, "database_type": database_type, "database_build_epoch": database_build_epoch,
                "result": location,
            }),
            Err(reason) => serde_json::json!({
                "at": at, "ip": ip, "database_type": database_type, "database_build_epoch": database_build_epoch,
                "error": reason,
            }),
        };
        let mut bytes = value.to_string().into_bytes();
        bytes.push(b'\n');
        let written = (|| -> Result<(), &'static str> {
            let size = file.metadata().map_err(|_| "cannot read capture size")?.len();
            if size.saturating_add(bytes.len() as u64) > DEBUG_LOG_MAX_BYTES {
                return Err("capture reached its 10 MiB limit");
            }
            file.write_all(&bytes).map_err(|_| "cannot write capture")
        })();
        if let Err(reason) = written {
            *guard = None;
            tracing::warn!(reason, "GeoIP debug capture stopped; lookup results are unaffected");
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Location {
    pub status: &'static str,
    pub country: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
}

impl Location {
    pub fn unavailable() -> Self {
        Self { status: "unavailable", country: None, city: None, region: None }
    }

    fn not_found() -> Self {
        Self { status: "not_found", country: None, city: None, region: None }
    }
}

#[derive(Default, Deserialize)]
struct Country {
    iso_code: Option<String>,
}

#[derive(Default, Deserialize)]
struct City {
    #[serde(default)]
    names: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    country: Country,
    #[serde(default)]
    city: City,
    // Both supported databases order subdivisions from broadest to most specific. Index zero
    // is the administrative region/state; do not confuse it with the city.
    #[serde(default)]
    subdivisions: Vec<City>,
}

impl Geoip {
    pub fn open(path: &Path) -> Result<Self, String> {
        // Bound allocations even if the file changes while being read.
        const MAX_BYTES: u64 = 512 * 1024 * 1024;
        let file = std::fs::File::open(path).map_err(|_| "cannot open GeoIP database")?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)
            .map_err(|_| "cannot read GeoIP database")?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("GeoIP database exceeds 512 MiB".into());
        }
        let reader = std::panic::catch_unwind(|| Reader::from_source(bytes))
            .map_err(|_| "invalid GeoIP MMDB database")?
            .map_err(|_| "invalid GeoIP MMDB database")?;
        validate_database_type(&reader.metadata.database_type)?;
        Ok(Self { reader, debug_log: None })
    }

    pub fn database_type(&self) -> &str {
        &self.reader.metadata.database_type
    }

    /// The parent directory must already exist. Failure does not change lookups.
    pub fn enable_debug_log(&mut self, path: &Path) -> std::io::Result<()> {
        self.debug_log = Some(DebugLog::open(path)?);
        Ok(())
    }

    pub fn lookup(&self, ip: IpAddr) -> Result<Location, String> {
        let result = self.lookup_inner(ip);
        if let Some(log) = &self.debug_log {
            log.record(ip, self.database_type(), self.reader.metadata.build_epoch, &result);
        }
        result
    }

    fn lookup_inner(&self, ip: IpAddr) -> Result<Location, String> {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            _ => ip,
        };
        if non_public(ip) {
            return Ok(Location::not_found());
        }
        let result = std::panic::catch_unwind(|| self.reader.lookup::<Record>(ip))
            .map_err(|_| "GeoIP database lookup failed")?;
        match result {
            Ok(record) => Ok(location(record)),
            Err(MaxMindDBError::AddressNotFoundError(_)) => Ok(Location::not_found()),
            // Corruption is a host error, not a successful unknown lookup.
            Err(_) => Err("GeoIP database lookup failed".into()),
        }
    }
}

fn validate_database_type(database_type: &str) -> Result<(), String> {
    match database_type {
        "GeoLite2-City" | "DBIP-City-Lite" => Ok(()),
        _ => Err("expected a GeoLite2-City or DBIP-City-Lite MMDB database".into()),
    }
}

// History can contain observations from both providers; attribution covers both.
pub const DATA_ATTRIBUTION: &str = "This product includes GeoLite2 data created by MaxMind - https://www.maxmind.com\nIP Geolocation by DB-IP - https://db-ip.com\n";

fn location(record: Record) -> Location {
    let Some(country) = record.country.iso_code else { return Location::not_found() };
    let country = country.to_ascii_uppercase();
    if country == "ZZ" || country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Location::not_found();
    }
    let city = record.city.names.get("en").map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.len() <= 256);
    let region = record.subdivisions.first().and_then(|s| {
        s.names.get("fr").or_else(|| s.names.get("en"))
    }).map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s.len() <= 256);
    Location { status: "found", country: Some(country), city, region }
}

/// Exclude local, documentation, benchmarking and multicast addresses even
/// when a database happens to associate them with a location.
fn non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            ip.is_private() || ip.is_loopback() || ip.is_link_local()
                || ip.is_documentation() || a == 0 || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            ip.is_unspecified() || ip.is_loopback() || ip.is_multicast()
                || s[0] & 0xfe00 == 0xfc00 || s[0] & 0xffc0 == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_city_databases_and_rejects_other_editions() {
        for edition in ["GeoLite2-City", "DBIP-City-Lite"] {
            assert!(validate_database_type(edition).is_ok());
        }
        for edition in ["GeoLite2-Country", "GeoLite2-ASN", "GeoIP2-City", "unknown", ""] {
            assert!(validate_database_type(edition).is_err());
        }
    }

    #[test]
    fn debug_capture_is_opt_in_and_records_results_and_errors() {
        let config: GeoipConfig = toml::from_str("database = 'db.mmdb'").unwrap();
        assert!(config.debug_log.is_none());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("debug.jsonl");
        let log = DebugLog::open(&path).unwrap();
        let found = Ok(Location {
            status: "found", country: Some("FR".into()),
            city: Some("Paris".into()), region: Some("Ile-de-France".into()),
        });
        log.record("8.8.8.8".parse().unwrap(), "GeoLite2-City", 1234, &found);
        log.record("192.168.1.254".parse().unwrap(), "GeoLite2-City", 1234, &Ok(Location::not_found()));
        log.record("1.1.1.1".parse().unwrap(), "GeoLite2-City", 1234, &Err("GeoIP database lookup failed".into()));
        let records: Vec<serde_json::Value> = std::fs::read_to_string(&path).unwrap()
            .lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["ip"], "8.8.8.8");
        assert_eq!(records[0]["database_build_epoch"], 1234);
        assert_eq!(records[0]["database_type"], "GeoLite2-City");
        assert!(records[0]["at"].as_u64().unwrap() > 0);
        assert_eq!(records[0]["result"]["country"], "FR");
        assert_eq!(records[1]["result"]["status"], "not_found");
        assert_eq!(records[2]["error"], "GeoIP database lookup failed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn debug_capture_appends_and_stops_at_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("debug.jsonl");
        let result = Ok(Location::not_found());
        let log = DebugLog::open(&path).unwrap();
        log.record("127.0.0.1".parse().unwrap(), "GeoLite2-City", 1234, &result);
        drop(log);
        let log = DebugLog::open(&path).unwrap();
        log.record("::1".parse().unwrap(), "GeoLite2-City", 1234, &result);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        log.file.lock().unwrap().as_ref().unwrap().set_len(DEBUG_LOG_MAX_BYTES).unwrap();
        log.record("::1".parse().unwrap(), "GeoLite2-City", 1234, &result);
        assert!(log.file.lock().unwrap().is_none());
        log.record("::1".parse().unwrap(), "GeoLite2-City", 1234, &result);
        assert_eq!(std::fs::metadata(path).unwrap().len(), DEBUG_LOG_MAX_BYTES);
        assert!(DebugLog::open(&dir.path().join("missing/debug.jsonl")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn debug_capture_write_failure_disables_capture() {
        let log = DebugLog {
            file: Mutex::new(Some(std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap())),
        };
        log.record("8.8.8.8".parse().unwrap(), "GeoLite2-City", 1234, &Ok(Location::not_found()));
        assert!(log.file.lock().unwrap().is_none());
    }

    #[test]
    #[ignore = "requires STATIOND_TEST_GEOLITE_FIXTURE (official GeoLite2-City-Test.mmdb)"]
    fn geolite2_fixture_maps_country_city_and_region() {
        let path = std::env::var_os("STATIOND_TEST_GEOLITE_FIXTURE").expect("STATIOND_TEST_GEOLITE_FIXTURE");
        let reader = Geoip::open(Path::new(&path)).unwrap();
        assert_eq!(reader.database_type(), "GeoLite2-City");
        let result = reader.lookup("81.2.69.160".parse().unwrap()).unwrap();
        assert_eq!(result.status, "found");
        assert_eq!(result.country.as_deref(), Some("GB"));
        assert_eq!(result.city.as_deref(), Some("London"));
        assert_eq!(result.region.as_deref(), Some("Angleterre"));
    }

    #[test]
    #[ignore = "requires STATIOND_TEST_GEOIP (or STATIOND_TEST_DBIP)"]
    fn real_geoip_debug_capture_matches_lookup() {
        let path = std::env::var_os("STATIOND_TEST_GEOIP").or_else(|| std::env::var_os("STATIOND_TEST_DBIP")).expect("STATIOND_TEST_GEOIP or STATIOND_TEST_DBIP");
        let mut reader = Geoip::open(Path::new(&path)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("debug.jsonl");
        reader.enable_debug_log(&capture).unwrap();
        for ip in ["104.28.42.16", "104.28.42.27", "8.8.8.8", "1.1.1.1", "::ffff:192.168.1.254"] {
            let result = reader.lookup(ip.parse().unwrap()).unwrap();
            let last = std::fs::read_to_string(&capture).unwrap();
            let record: serde_json::Value = serde_json::from_str(last.lines().last().unwrap()).unwrap();
            assert_eq!(record["ip"], ip);
            assert_eq!(record["result"], serde_json::to_value(result).unwrap());
            println!("{record}");
        }
    }

    #[test]
    fn maps_city_country_and_missing_values() {
        let record = serde_json::from_str(r#"{"country":{"iso_code":"fr"},"city":{"names":{"en":"Paris"}}}"#).unwrap();
        assert_eq!(location(record), Location {status: "found", country: Some("FR".into()), city: Some("Paris".into()), region: None});
        let record = serde_json::from_str(r#"{"country":{"iso_code":"FR"}}"#).unwrap();
        assert_eq!(location(record).city, None);
        for data in ["{}", r#"{"country":{"iso_code":"ZZ"}}"#, r#"{"country":{"iso_code":"FRA"}}"#] {
            assert_eq!(location(serde_json::from_str(data).unwrap()), Location::not_found());
        }
    }

    #[test]
    fn selects_first_administrative_subdivision_and_prefers_french() {
        let record = serde_json::from_str(r#"{"country":{"iso_code":"FR"},"subdivisions":[{"names":{"en":"Brittany","fr":"Bretagne"}},{"names":{"en":"Finistere"}}]}"#).unwrap();
        assert_eq!(location(record).region.as_deref(), Some("Bretagne"));
        let record = serde_json::from_str(r#"{"country":{"iso_code":"US"},"subdivisions":[{"names":{"en":"California"}}]}"#).unwrap();
        assert_eq!(location(record).region.as_deref(), Some("California"));
    }

    #[test]
    fn excludes_non_public_addresses() {
        for ip in ["127.0.0.1", "10.1.2.3", "192.168.1.1", "100.64.0.1", "169.254.1.1",
            "192.0.2.1", "198.18.0.1", "224.0.0.1", "255.255.255.255", "::", "::1",
            "fc00::1", "fe80::1", "ff02::1", "2001:db8::1"] {
            assert!(non_public(ip.parse().unwrap()), "{ip}");
        }
        assert!(!non_public("8.8.8.8".parse().unwrap()));
        assert!(!non_public("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn rejects_missing_and_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Geoip::open(&dir.path().join("missing.mmdb")).is_err());
        let path = dir.path().join("broken.mmdb");
        std::fs::write(&path, b"not an MMDB").unwrap();
        assert!(Geoip::open(&path).is_err());
    }

    #[test]
    #[ignore = "requires STATIOND_TEST_DBIP pointing to a downloaded DB-IP City Lite MMDB"]
    fn reads_real_dbip_ipv4_ipv6_and_mapped_addresses() {
        let path = std::env::var_os("STATIOND_TEST_DBIP").expect("STATIOND_TEST_DBIP");
        let reader = Geoip::open(Path::new(&path)).unwrap();
        let mut saw_region = false;
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            let result = reader.lookup(ip.parse().unwrap()).unwrap();
            assert_eq!(result.status, "found");
            assert_eq!(result.country.as_ref().unwrap().len(), 2);
            saw_region |= result.region.is_some();
        }
        assert!(saw_region, "expected at least one subdivision in DB-IP City Lite");
        assert_eq!(reader.lookup("::ffff:8.8.8.8".parse().unwrap()).unwrap(),
            reader.lookup("8.8.8.8".parse().unwrap()).unwrap());
        assert_eq!(reader.lookup("::ffff:192.168.1.1".parse().unwrap()).unwrap(), Location::not_found());
        assert_eq!(reader.lookup("2001:db8::1".parse().unwrap()).unwrap(), Location::not_found());
    }
}
