//! Local DB-IP City Lite reader. No HTTP and no persistence of queried IPs.
use std::collections::BTreeMap;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use maxminddb::{MaxMindDBError, Reader};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeoipConfig {
    /// Operator-controlled file; relative to stationd's working directory.
    pub database: PathBuf,
}

/// Shared immutable reader. Reload by restarting stationd after file replacement.
pub struct Geoip {
    reader: Reader<Vec<u8>>,
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
    // DB-IP orders subdivisions from broadest to most specific. Index zero
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
        if reader.metadata.database_type != "DBIP-City-Lite" {
            return Err("expected a DBIP-City-Lite MMDB database".into());
        }
        Ok(Self { reader })
    }

    pub fn lookup(&self, ip: IpAddr) -> Result<Location, String> {
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
