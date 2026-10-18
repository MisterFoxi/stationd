//! Service-neutral, authenticated local discovery. No application DTOs or service ids.
//! Announcements grant neither RPC permissions nor access to credentials.
use hmac::{Hmac, Mac};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{net::UdpSocket, time::Instant};
const PROTOCOL: &str = "stationdeck-services-v1";
const MAX_PACKET: usize = 4096;
const MAX_INSTANCES: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    pub id: String,
    pub version: String,
    pub transport: String,
    pub scheme: String,
    pub port: u16,
    pub tls: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub authority: String,
    pub instance: String,
    pub lease_seconds: u16,
    pub interfaces: Vec<Interface>,
}
fn local_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn interface_id(id: &str) -> bool {
    id.contains('.') && id.split('.').all(local_id)
}
impl Descriptor {
    pub fn validate(&self) -> Result<(), Error> {
        if !local_id(&self.authority)
            || !local_id(&self.instance)
            || !(1..=60).contains(&self.lease_seconds)
            || self.interfaces.is_empty()
            || self.interfaces.len() > 16
        {
            return Err(Error::InvalidConfiguration);
        }
        let mut ids = BTreeSet::new();
        for interface in &self.interfaces {
            if !interface_id(&interface.id)
                || Version::parse(&interface.version).is_err()
                || !local_id(&interface.transport)
                || !local_id(&interface.scheme)
                || interface.port == 0
                || !ids.insert(&interface.id)
            {
                return Err(Error::InvalidConfiguration);
            }
            validate_origin(
                &format!("{}://127.0.0.1:{}", interface.scheme, interface.port),
                true,
            )?;
            if matches!(interface.scheme.as_str(), "http" | "ws") && interface.tls
                || matches!(interface.scheme.as_str(), "https" | "wss") && !interface.tls
            {
                return Err(Error::InvalidConfiguration);
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidConfiguration,
    MissingTrust,
    Unavailable,
    Ambiguous,
    Incompatible,
    TransportDenied,
    NotBound,
    Io,
}
impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "invalid_configuration",
            Self::MissingTrust => "missing_trust",
            Self::Unavailable => "not_discovered",
            Self::Ambiguous => "ambiguous",
            Self::Incompatible => "incompatible",
            Self::TransportDenied => "transport_denied",
            Self::NotBound => "not_bound",
            Self::Io => "discovery_unavailable",
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "Invalid service discovery or binding configuration.",
            Self::MissingTrust => "Service discovery credentials are not provisioned.",
            Self::Unavailable => "No trusted matching service was discovered.",
            Self::Ambiguous => "Multiple matching service instances were discovered; select an instance or explicit endpoint.",
            Self::Incompatible => "Discovered services do not satisfy the requested interface version or transport.",
            Self::TransportDenied => "Service transport is not allowed by deployment policy.",
            Self::NotBound => "No deployment binding is configured for this service interface.",
            Self::Io => "Local service discovery is unavailable.",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for Error {}

/// Instance-specific signing material; never serializable or included in diagnostics.
#[derive(Clone)]
pub struct Key(Vec<u8>);
impl Key {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if !(32..=512).contains(&bytes.len()) {
            return Err(Error::InvalidConfiguration);
        }
        Ok(Self(bytes.to_vec()))
    }
    pub fn read(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path).map_err(|_| Error::MissingTrust)?;
        Self::from_bytes(text.trim().as_bytes())
    }
}
fn mac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut signer = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts every key length");
    signer.update(data);
    signer.finalize().into_bytes().into()
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Query {
    protocol: String,
    authority: String,
    nonce: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    protocol: String,
    nonce: String,
    boot_id: String,
    address: Ipv4Addr,
    descriptor: Descriptor,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    payload: Payload,
    signature: Vec<u8>,
}
fn encode_offer(payload: Payload, key: &Key) -> Result<Vec<u8>, Error> {
    let bytes = serde_json::to_vec(&payload).map_err(|_| Error::InvalidConfiguration)?;
    let signature = mac(&key.0, &bytes).to_vec();
    let packet = serde_json::to_vec(&Offer { payload, signature })
        .map_err(|_| Error::InvalidConfiguration)?;
    if packet.len() > MAX_PACKET {
        return Err(Error::InvalidConfiguration);
    }
    Ok(packet)
}
/// Bind before spawning so provisioning/bind errors are observable at startup.
pub async fn start(
    bind: &str,
    descriptor: Descriptor,
    key: Key,
) -> Result<tokio::task::JoinHandle<()>, Error> {
    descriptor.validate()?;
    let socket = UdpSocket::bind(bind).await.map_err(|_| Error::Io)?;
    let boot_id = uuid::Uuid::new_v4().to_string();
    // Validate the complete serialized bound before accepting network requests.
    encode_offer(
        Payload {
            protocol: PROTOCOL.into(),
            nonce: boot_id.clone(),
            boot_id: boot_id.clone(),
            address: Ipv4Addr::LOCALHOST,
            descriptor: descriptor.clone(),
        },
        &key,
    )?;
    Ok(tokio::spawn(async move {
        let mut buffer = [0u8; MAX_PACKET + 1];
        while let Ok((len, peer)) = socket.recv_from(&mut buffer).await {
            if len > MAX_PACKET || !peer.is_ipv4() {
                continue;
            }
            let Ok(query) = serde_json::from_slice::<Query>(&buffer[..len]) else {
                continue;
            };
            if query.protocol != PROTOCOL
                || query.authority != descriptor.authority
                || uuid::Uuid::parse_str(&query.nonce).is_err()
            {
                continue;
            }
            // Route a separate socket to the requester to identify the correct local
            // interface on a multihomed container. The signed address prevents relay
            // of a genuine offer from redirecting RPC credentials to another IP.
            if let Ok(reply) = UdpSocket::bind("0.0.0.0:0").await {
                if reply.connect(peer).await.is_ok() {
                    if let Ok(SocketAddr::V4(local)) = reply.local_addr() {
                        let payload = Payload {
                            protocol: PROTOCOL.into(),
                            nonce: query.nonce,
                            boot_id: boot_id.clone(),
                            address: *local.ip(),
                            descriptor: descriptor.clone(),
                        };
                        if let Ok(packet) = encode_offer(payload, &key) {
                            let _ = reply.send(&packet).await;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }))
}
pub async fn start_from_env() -> Result<Option<tokio::task::JoinHandle<()>>, Error> {
    let Ok(path) = std::env::var("STATIONDECK_SERVICE_DESCRIPTOR") else {
        return Ok(None);
    };
    let key = std::env::var("STATIONDECK_DISCOVERY_KEY_FILE").map_err(|_| Error::MissingTrust)?;
    let descriptor: Descriptor =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| Error::InvalidConfiguration)?)
            .map_err(|_| Error::InvalidConfiguration)?;
    let bind = std::env::var("STATIONDECK_SERVICE_DISCOVERY_BIND")
        .unwrap_or_else(|_| "0.0.0.0:50072".into());
    start(&bind, descriptor, Key::read(Path::new(&key))?)
        .await
        .map(Some)
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedInstance {
    pub instance: String,
    pub key_file: PathBuf,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub interface: String,
    pub version: String,
    pub transport: String,
    pub instance: Option<String>,
    pub endpoint: Option<String>,
    pub credential_file: Option<PathBuf>,
    #[serde(default)]
    pub allow_plaintext: bool,
    pub tls_server_name: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub authority: String,
    pub discovery_address: String,
    pub trust: Vec<TrustedInstance>,
    pub bindings: Vec<Binding>,
}
impl Policy {
    pub fn read(path: &Path) -> Result<Self, Error> {
        let policy: Self =
            serde_json::from_slice(&std::fs::read(path).map_err(|_| Error::InvalidConfiguration)?)
                .map_err(|_| Error::InvalidConfiguration)?;
        policy.validate()?;
        Ok(policy)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if !local_id(&self.authority)
            || self.trust.len() > MAX_INSTANCES
            || self.bindings.len() > MAX_INSTANCES
        {
            return Err(Error::InvalidConfiguration);
        }
        let mut ids = BTreeSet::new();
        for item in &self.trust {
            if !local_id(&item.instance)
                || item.key_file.as_os_str().is_empty()
                || !ids.insert(&item.instance)
            {
                return Err(Error::InvalidConfiguration);
            }
        }
        let mut interfaces = BTreeSet::new();
        for b in &self.bindings {
            if !interface_id(&b.interface)
                || VersionReq::parse(&b.version).is_err()
                || !local_id(&b.transport)
                || b.instance.as_ref().is_some_and(|id| !local_id(id))
                || !interfaces.insert(&b.interface)
            {
                return Err(Error::InvalidConfiguration);
            }
            // Validate explicit origins up front; do not silently fall back to UDP.
            if let Some(endpoint) = &b.endpoint {
                validate_origin(endpoint, b.allow_plaintext)?;
            }
        }
        Ok(())
    }
    pub fn binding(&self, interface: &str) -> Result<&Binding, Error> {
        self.bindings
            .iter()
            .find(|b| b.interface == interface)
            .ok_or(Error::NotBound)
    }
    pub async fn resolve(&self, interface: &str) -> Result<Resolved, Error> {
        let binding = self.binding(interface)?;
        if let Some(endpoint) = &binding.endpoint {
            validate_origin(endpoint, binding.allow_plaintext)?;
            return Ok(Resolved {
                endpoint: endpoint.clone(),
                instance: binding.instance.clone(),
                credential_file: binding.credential_file.clone(),
                tls_server_name: binding.tls_server_name.clone(),
            });
        }
        // A fresh, nonce-bound discovery for each operation avoids retained Docker
        // addresses, stale leases and accidental fallback after a failed RPC.
        self.discover(Duration::from_millis(750))
            .await?
            .select(binding)
    }
    pub async fn discover(&self, timeout: Duration) -> Result<Directory, Error> {
        let mut trust = BTreeMap::new();
        for item in &self.trust {
            if let Ok(key) = Key::read(&item.key_file) {
                trust.insert(item.instance.clone(), key);
            }
        }
        if trust.is_empty() {
            return Err(Error::MissingTrust);
        }
        discover(&self.discovery_address, &self.authority, &trust, timeout).await
    }
}
#[derive(Clone)]
pub struct Resolved {
    pub endpoint: String,
    pub instance: Option<String>,
    pub credential_file: Option<PathBuf>,
    pub tls_server_name: Option<String>,
}
#[derive(Clone, Serialize)]
pub struct Discovered {
    pub instance: String,
    pub boot_id: String,
    pub address: Ipv4Addr,
    pub interfaces: Vec<Interface>,
    #[serde(skip)]
    expires: Option<Instant>,
}
#[derive(Default)]
pub struct Directory {
    services: Vec<Discovered>,
}
impl Directory {
    pub fn services(&self) -> Vec<&Discovered> {
        self.services
            .iter()
            .filter(|s| s.expires.is_some_and(|t| t > Instant::now()))
            .collect()
    }
    pub fn select(&self, binding: &Binding) -> Result<Resolved, Error> {
        let requirement =
            VersionReq::parse(&binding.version).map_err(|_| Error::InvalidConfiguration)?;
        let mut present = false;
        let mut transport_denied = false;
        let mut candidates = Vec::new();
        for service in self.services() {
            if binding
                .instance
                .as_ref()
                .is_some_and(|id| *id != service.instance)
            {
                continue;
            }
            for interface in &service.interfaces {
                if interface.id != binding.interface {
                    continue;
                }
                present = true;
                if interface.transport == binding.transport
                    && Version::parse(&interface.version).is_ok_and(|v| requirement.matches(&v))
                {
                    if !interface.tls && !binding.allow_plaintext {
                        transport_denied = true;
                        continue;
                    }
                    candidates.push((service, interface));
                }
            }
        }
        if candidates.len() > 1 {
            return Err(Error::Ambiguous);
        }
        let Some((service, interface)) = candidates.pop() else {
            return Err(if transport_denied {
                Error::TransportDenied
            } else if present {
                Error::Incompatible
            } else {
                Error::Unavailable
            });
        };
        Ok(Resolved {
            endpoint: format!(
                "{}://{}:{}",
                interface.scheme, service.address, interface.port
            ),
            instance: Some(service.instance.clone()),
            credential_file: binding.credential_file.clone(),
            tls_server_name: binding.tls_server_name.clone(),
        })
    }
}
fn validate_origin(origin: &str, allow_plaintext: bool) -> Result<(), Error> {
    let uri = url::Url::parse(origin).map_err(|_| Error::InvalidConfiguration)?;
    if uri.host_str().is_none()
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.query().is_some()
        || uri.fragment().is_some()
        || !matches!(uri.path(), "" | "/")
    {
        return Err(Error::InvalidConfiguration);
    }
    if !matches!(uri.scheme(), "https" | "wss" | "tls") && !allow_plaintext {
        return Err(Error::TransportDenied);
    }
    Ok(())
}
fn accept(
    packet: &[u8],
    peer: SocketAddr,
    authority: &str,
    nonce: &str,
    trust: &BTreeMap<String, Key>,
) -> Option<Discovered> {
    if packet.len() > MAX_PACKET {
        return None;
    }
    let offer: Offer = serde_json::from_slice(packet).ok()?;
    let p = &offer.payload;
    if p.protocol != PROTOCOL
        || p.nonce != nonce
        || p.descriptor.authority != authority
        || peer.ip() != p.address
        || uuid::Uuid::parse_str(&p.boot_id).is_err()
        || p.descriptor.validate().is_err()
    {
        return None;
    }
    let key = trust.get(&p.descriptor.instance)?;
    let mut verifier = Hmac::<Sha256>::new_from_slice(&key.0).ok()?;
    verifier.update(&serde_json::to_vec(p).ok()?);
    verifier.verify_slice(&offer.signature).ok()?;
    Some(Discovered {
        instance: p.descriptor.instance.clone(),
        boot_id: p.boot_id.clone(),
        address: p.address,
        interfaces: p.descriptor.interfaces.clone(),
        expires: Some(Instant::now() + Duration::from_secs(u64::from(p.descriptor.lease_seconds))),
    })
}
pub async fn discover(
    destination: &str,
    authority: &str,
    trust: &BTreeMap<String, Key>,
    timeout: Duration,
) -> Result<Directory, Error> {
    let destination: SocketAddr = destination
        .parse()
        .map_err(|_| Error::InvalidConfiguration)?;
    if !destination.is_ipv4() || !local_id(authority) || timeout > Duration::from_secs(5) {
        return Err(Error::InvalidConfiguration);
    }
    let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|_| Error::Io)?;
    socket.set_broadcast(true).map_err(|_| Error::Io)?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let query = serde_json::to_vec(&Query {
        protocol: PROTOCOL.into(),
        authority: authority.into(),
        nonce: nonce.clone(),
    })
    .map_err(|_| Error::InvalidConfiguration)?;
    let destinations = if destination.ip() == Ipv4Addr::BROADCAST {
        broadcasts(
            &std::fs::read_to_string("/proc/net/route").map_err(|_| Error::Io)?,
            destination.port(),
        )
    } else {
        vec![destination]
    };
    let mut sent = false;
    for target in destinations {
        sent |= socket.send_to(&query, target).await.is_ok();
    }
    if !sent {
        return Err(Error::Io);
    }
    let deadline = Instant::now() + timeout;
    let mut directory = Directory::default();
    let mut seen = BTreeSet::new();
    let mut buffer = [0u8; MAX_PACKET + 1];
    while let Ok(Ok((len, peer))) =
        tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await
    {
        if directory.services.len() >= MAX_INSTANCES {
            return Err(Error::Ambiguous);
        }
        if let Some(service) = accept(&buffer[..len], peer, authority, &nonce, trust) {
            if seen.insert((
                service.instance.clone(),
                service.boot_id.clone(),
                service.address,
            )) {
                directory.services.push(service);
            }
        }
    }
    directory
        .services
        .sort_by(|a, b| (&a.instance, a.address).cmp(&(&b.instance, b.address)));
    Ok(directory)
}
fn broadcasts(routes: &str, port: u16) -> Vec<SocketAddr> {
    let mut found = BTreeSet::new();
    for line in routes.lines().skip(1) {
        let c: Vec<_> = line.split_whitespace().collect();
        if c.len() < 8 || c[0] == "lo" || c[2] != "00000000" {
            continue;
        }
        let (Ok(net), Ok(mask), Ok(flags)) = (
            u32::from_str_radix(c[1], 16),
            u32::from_str_radix(c[7], 16),
            u32::from_str_radix(c[3], 16),
        ) else {
            continue;
        };
        if flags & 1 == 0 || mask == 0 || mask == u32::MAX {
            continue;
        }
        found.insert(SocketAddr::new(
            Ipv4Addr::from((net | !mask).to_le_bytes()).into(),
            port,
        ));
    }
    found.into_iter().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn descriptor(instance: &str, interface: &str) -> Descriptor {
        Descriptor {
            authority: "test-deployment".into(),
            instance: instance.into(),
            lease_seconds: 5,
            interfaces: vec![Interface {
                id: interface.into(),
                version: "1.2.0".into(),
                transport: "grpc".into(),
                scheme: "http".into(),
                port: 50061,
                tls: false,
            }],
        }
    }
    fn binding(interface: &str) -> Binding {
        Binding {
            interface: interface.into(),
            version: "^1.0.0".into(),
            transport: "grpc".into(),
            instance: None,
            endpoint: None,
            credential_file: None,
            allow_plaintext: true,
            tls_server_name: None,
        }
    }
    fn payload(instance: &str, interface: &str, nonce: &str) -> Payload {
        Payload {
            protocol: PROTOCOL.into(),
            nonce: nonce.into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            address: Ipv4Addr::LOCALHOST,
            descriptor: descriptor(instance, interface),
        }
    }
    fn receive(p: Payload, key: &Key) -> Discovered {
        let nonce = p.nonce.clone();
        let trust = BTreeMap::from([(p.descriptor.instance.clone(), key.clone())]);
        accept(
            &encode_offer(p, key).unwrap(),
            "127.0.0.1:50072".parse().unwrap(),
            "test-deployment",
            &nonce,
            &trust,
        )
        .unwrap()
    }
    #[test]
    fn rfc4231_hmac_sha256_vector() {
        let output = mac(&[0x0b; 20], b"Hi There");
        let hex: String = output.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
    #[test]
    fn signatures_nonce_authority_and_sender_address_are_required() {
        let key = Key::from_bytes(&[7; 32]).unwrap();
        let nonce = uuid::Uuid::new_v4().to_string();
        let p = payload("service-one", "future.scheduler.jobs", &nonce);
        let packet = encode_offer(p.clone(), &key).unwrap();
        let trust = BTreeMap::from([("service-one".into(), key.clone())]);
        let peer = "127.0.0.1:50072".parse().unwrap();
        assert!(accept(&packet, peer, "test-deployment", &nonce, &trust).is_some());
        assert!(accept(&packet, peer, "other-authority", &nonce, &trust).is_none());
        assert!(accept(&packet, peer, "test-deployment", "replayed-nonce", &trust).is_none());
        assert!(accept(
            &packet,
            "127.0.0.2:50072".parse().unwrap(),
            "test-deployment",
            &nonce,
            &trust
        )
        .is_none());
        assert!(accept(&packet, peer, "test-deployment", &nonce, &BTreeMap::new()).is_none());
        let wrong_key = Key::from_bytes(&[8; 32]).unwrap();
        assert!(accept(
            &encode_offer(p.clone(), &wrong_key).unwrap(),
            peer,
            "test-deployment",
            &nonce,
            &trust
        )
        .is_none());
        let mut changed = p;
        changed.descriptor.interfaces[0].port = 12345;
        let mut forged: Offer = serde_json::from_slice(&packet).unwrap();
        forged.payload = changed;
        assert!(accept(
            &serde_json::to_vec(&forged).unwrap(),
            peer,
            "test-deployment",
            &nonce,
            &trust
        )
        .is_none());
    }
    #[test]
    fn missing_ambiguous_incompatible_and_expired_services_never_select_arbitrarily() {
        let key = Key::from_bytes(&[7; 32]).unwrap();
        let nonce = uuid::Uuid::new_v4().to_string();
        let request = binding("future.scheduler.jobs");
        let mut directory = Directory::default();
        assert!(matches!(
            directory.select(&request),
            Err(Error::Unavailable)
        ));
        directory
            .services
            .push(receive(payload("one", &request.interface, &nonce), &key));
        assert_eq!(
            directory.select(&request).unwrap().instance.as_deref(),
            Some("one")
        );
        let mut incompatible = request.clone();
        incompatible.version = "^2.0.0".into();
        assert!(matches!(
            directory.select(&incompatible),
            Err(Error::Incompatible)
        ));
        directory
            .services
            .push(receive(payload("two", &request.interface, &nonce), &key));
        assert!(matches!(directory.select(&request), Err(Error::Ambiguous)));
        let mut explicit = request.clone();
        explicit.instance = Some("two".into());
        assert_eq!(
            directory.select(&explicit).unwrap().instance.as_deref(),
            Some("two")
        );
        directory.services[1].expires = Some(Instant::now() - Duration::from_secs(1));
        assert!(matches!(
            directory.select(&explicit),
            Err(Error::Unavailable)
        ));
        assert_eq!(
            directory.select(&request).unwrap().instance.as_deref(),
            Some("one")
        );
        let mut secure = request;
        secure.allow_plaintext = false;
        assert!(matches!(
            directory.select(&secure),
            Err(Error::TransportDenied)
        ));
    }
    #[test]
    fn future_http_service_uses_the_same_protocol_and_selection_without_application_code() {
        let key = Key::from_bytes(&[7; 32]).unwrap();
        let nonce = uuid::Uuid::new_v4().to_string();
        let mut p = payload("new-analytics", "future.analytics.reports", &nonce);
        p.descriptor.interfaces[0] = Interface {
            id: "future.analytics.reports".into(),
            version: "4.1.0".into(),
            transport: "http".into(),
            scheme: "https".into(),
            port: 8443,
            tls: true,
        };
        let directory = Directory {
            services: vec![receive(p, &key)],
        };
        let mut b = binding("future.analytics.reports");
        b.version = "^4.0.0".into();
        b.transport = "http".into();
        b.allow_plaintext = false;
        assert_eq!(
            directory.select(&b).unwrap().endpoint,
            "https://127.0.0.1:8443"
        );
        assert!(!serde_json::to_string(&directory.services())
            .unwrap()
            .contains("credential"));
    }
    #[tokio::test]
    async fn explicit_endpoint_precedes_discovery_even_with_no_trust_and_invalid_udp_address() {
        let mut b = binding("future.scheduler.jobs");
        b.endpoint = Some("http://scheduler.example:50070".into());
        let policy = Policy {
            authority: "test-deployment".into(),
            discovery_address: "not-a-UDP-address".into(),
            trust: vec![],
            bindings: vec![b],
        };
        policy.validate().unwrap();
        assert_eq!(
            policy
                .resolve("future.scheduler.jobs")
                .await
                .unwrap()
                .endpoint,
            "http://scheduler.example:50070"
        );
        let mut invalid = policy.clone();
        invalid.bindings[0].endpoint = Some("http://user:secret@host:1234".into());
        assert!(invalid.validate().is_err());
    }
    #[tokio::test]
    async fn real_udp_announcement_and_fresh_resolution_after_service_restart() {
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap().to_string();
        drop(reservation);
        let key = Key::from_bytes(&[7; 32]).unwrap();
        let trust = BTreeMap::from([("one".into(), key.clone())]);
        let mut d = descriptor("one", "future.scheduler.jobs");
        let task = start(&address, d.clone(), key.clone()).await.unwrap();
        let first = discover(
            &address,
            "test-deployment",
            &trust,
            Duration::from_millis(80),
        )
        .await
        .unwrap();
        let first_boot = first.services()[0].boot_id.clone();
        assert!(first
            .select(&binding("future.scheduler.jobs"))
            .unwrap()
            .endpoint
            .ends_with(":50061"));
        task.abort();
        let _ = task.await;
        d.interfaces[0].port = 50062;
        let task = start(&address, d, key).await.unwrap();
        let second = discover(
            &address,
            "test-deployment",
            &trust,
            Duration::from_millis(80),
        )
        .await
        .unwrap();
        assert_ne!(first_boot, second.services()[0].boot_id);
        assert!(second
            .select(&binding("future.scheduler.jobs"))
            .unwrap()
            .endpoint
            .ends_with(":50062"));
        task.abort();
    }
    #[test]
    fn subnet_broadcasts_do_not_require_a_default_route() {
        let routes = "Iface Destination Gateway Flags RefCnt Use Metric Mask\neth0 000014AC 00000000 0001 0 0 0 0000FFFF\n";
        assert_eq!(
            broadcasts(routes, 50072),
            vec!["172.20.255.255:50072".parse::<SocketAddr>().unwrap()]
        );
    }
}
