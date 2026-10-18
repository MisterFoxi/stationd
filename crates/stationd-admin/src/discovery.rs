//! Resolve outbound interfaces using the shared, signed discovery protocol.
use anyhow::{bail, Context, Result};
use std::path::Path;

pub fn http_origin(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("invalid service origin")?;
    // These existing clients implement plaintext HTTP only. Never downgrade TLS.
    if url.scheme() != "http"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("service requires an HTTP origin supported by this client");
    }
    Ok(url)
}

pub async fn resolve(explicit: &str, interface: &str, transport: &str) -> Result<reqwest::Url> {
    // Explicit origins remain usable without provisioning local discovery.
    if !explicit.trim().is_empty() {
        return http_origin(explicit.trim());
    }
    let path = std::env::var("STATIONDECK_SERVICE_CATALOG")
        .context("service discovery policy is not configured")?;
    let policy = stationdeck_discovery::Policy::read(Path::new(&path))?;
    resolve_policy(explicit, &policy, interface, transport).await
}

async fn resolve_policy(
    explicit: &str,
    policy: &stationdeck_discovery::Policy,
    interface: &str,
    transport: &str,
) -> Result<reqwest::Url> {
    if !explicit.trim().is_empty() {
        return http_origin(explicit.trim());
    }
    if policy.binding(interface)?.transport != transport {
        bail!("incompatible service transport binding");
    }
    http_origin(&policy.resolve(interface).await?.endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn explicit_origin_precedes_discovery_and_invalid_origins_do_not_fall_back() {
        let policy = stationdeck_discovery::Policy {
            authority: "test".into(),
            discovery_address: "invalid".into(),
            trust: vec![],
            bindings: vec![],
        };
        assert_eq!(
            resolve_policy(
                "http://explicit.example:1234",
                &policy,
                "future.service.api",
                "grpc"
            )
            .await
            .unwrap()
            .host_str(),
            Some("explicit.example")
        );
        for origin in [
            "http://user:password@host:1234",
            "https://host",
            "http://host/path",
            "http://host/?token=hidden",
        ] {
            assert!(
                resolve_policy(origin, &policy, "future.service.api", "grpc")
                    .await
                    .is_err()
            );
        }
        assert!(resolve_policy("", &policy, "future.service.api", "grpc")
            .await
            .is_err());
    }
    #[tokio::test]
    async fn shared_resolver_refreshes_a_signed_origin_after_restart() {
        use stationdeck_discovery::{Binding, Descriptor, Interface, Key, Policy, TrustedInstance};
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("discovery-key");
        std::fs::write(&key_file, "a-distinct-test-discovery-key-0123456789").unwrap();
        let key = Key::read(&key_file).unwrap();
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap().to_string();
        drop(reservation);
        let mut descriptor = Descriptor {
            authority: "test".into(),
            instance: "upstream-1".into(),
            lease_seconds: 5,
            interfaces: vec![Interface {
                id: "future.service.api".into(),
                version: "1.0.0".into(),
                transport: "http".into(),
                scheme: "http".into(),
                port: 8000,
                tls: false,
            }],
        };
        let mut policy = Policy {
            authority: "test".into(),
            discovery_address: address.clone(),
            trust: vec![TrustedInstance {
                instance: "upstream-1".into(),
                key_file,
            }],
            bindings: vec![Binding {
                interface: "future.service.api".into(),
                version: "^1.0.0".into(),
                transport: "http".into(),
                instance: Some("upstream-1".into()),
                endpoint: None,
                credential_file: None,
                allow_plaintext: true,
                tls_server_name: None,
            }],
        };
        policy.validate().unwrap();
        let task = stationdeck_discovery::start(&address, descriptor.clone(), key.clone())
            .await
            .unwrap();
        assert_eq!(
            resolve_policy("", &policy, "future.service.api", "http")
                .await
                .unwrap()
                .port(),
            Some(8000)
        );
        task.abort();
        let _ = task.await;
        descriptor.interfaces[0].port = 9000;
        let task = stationdeck_discovery::start(&address, descriptor, key)
            .await
            .unwrap();
        assert_eq!(
            resolve_policy("", &policy, "future.service.api", "http")
                .await
                .unwrap()
                .port(),
            Some(9000)
        );
        assert!(resolve_policy("", &policy, "future.service.api", "grpc")
            .await
            .is_err());
        policy.bindings[0].allow_plaintext = false;
        assert!(resolve_policy("", &policy, "future.service.api", "http")
            .await
            .is_err());
        task.abort();
        let _ = task.await;
        policy.bindings[0].allow_plaintext = true;
        assert!(resolve_policy("", &policy, "future.service.api", "http")
            .await
            .is_err());
    }
}
