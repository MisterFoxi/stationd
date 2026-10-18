//! Small UDP broadcast discovery protocol, separate from authenticated gRPC.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Packet {
    protocol: String,
    authority: String,
    nonce: String,
    port: u16,
}
const PROTOCOL: &str = "stationdeck-config-v1";
pub async fn serve(bind: &str, authority: String, port: u16) -> Result<(), &'static str> {
    let socket = UdpSocket::bind(bind)
        .await
        .map_err(|_| "Cannot bind discovery socket.")?;
    let mut buf = [0u8; 1024];
    loop {
        let (len, peer) = socket
            .recv_from(&mut buf)
            .await
            .map_err(|_| "Discovery listener failed.")?;
        if let Ok(p) = serde_json::from_slice::<Packet>(&buf[..len]) {
            if p.protocol == PROTOCOL
                && p.authority == authority
                && p.nonce.len() == 36
                && p.port == 0
            {
                let reply = serde_json::to_vec(&Packet { port, ..p })
                    .map_err(|_| "Cannot encode discovery reply.")?;
                let _ = socket.send_to(&reply, peer).await;
                // Bound unsolicited local discovery work and amplification.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}
pub async fn discover(
    destination: &str,
    authority: &str,
    timeout: Duration,
) -> Result<String, &'static str> {
    let destination: SocketAddr = destination
        .parse()
        .map_err(|_| "Discovery destination must be an IPv4 address and port.")?;
    if !destination.is_ipv4() {
        return Err("Broadcast discovery currently requires IPv4.");
    }
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|_| "Cannot bind discovery client.")?;
    socket
        .set_broadcast(true)
        .map_err(|_| "Cannot enable broadcast discovery.")?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let data = serde_json::to_vec(&Packet {
        protocol: PROTOCOL.into(),
        authority: authority.into(),
        nonce: nonce.clone(),
        port: 0,
    })
    .map_err(|_| "Cannot encode discovery request.")?;
    let destinations = if destination.ip() == std::net::Ipv4Addr::BROADCAST {
        let routes = std::fs::read_to_string("/proc/net/route")
            .map_err(|_| "Cannot inspect connected networks; configure an explicit endpoint.")?;
        broadcasts(&routes, destination.port())
    } else {
        vec![destination]
    };
    let mut sent = false;
    for address in destinations {
        sent |= socket.send_to(&data, address).await.is_ok();
    }
    if !sent {
        return Err("Cannot send discovery request on a connected network.");
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut found = BTreeSet::new();
    let mut buf = [0u8; 1024];
    while let Ok(Ok((len, peer))) =
        tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await
    {
        if let Ok(p) = serde_json::from_slice::<Packet>(&buf[..len]) {
            if p.protocol == PROTOCOL && p.authority == authority && p.nonce == nonce && p.port != 0
            {
                found.insert(format!("{}:{}", peer.ip(), p.port));
            }
        }
    }
    select(found)
}
// Linux connected routes encode IPv4 addresses in little-endian hex. Using
// subnet broadcasts also works on Docker internal networks without a default route.
fn broadcasts(routes: &str, port: u16) -> Vec<SocketAddr> {
    let mut found = BTreeSet::new();
    for line in routes.lines().skip(1) {
        let columns: Vec<_> = line.split_whitespace().collect();
        if columns.len() < 8 || columns[0] == "lo" || columns[2] != "00000000" {
            continue;
        }
        let (Ok(network), Ok(mask), Ok(flags)) = (
            u32::from_str_radix(columns[1], 16),
            u32::from_str_radix(columns[7], 16),
            u32::from_str_radix(columns[3], 16),
        ) else {
            continue;
        };
        if flags & 1 == 0 || mask == 0 || mask == u32::MAX {
            continue;
        }
        let ip = std::net::Ipv4Addr::from((network | !mask).to_le_bytes());
        found.insert(SocketAddr::new(ip.into(), port));
    }
    found.into_iter().take(16).collect()
}
fn select(found: BTreeSet<String>) -> Result<String, &'static str> {
    if found.len() != 1 {
        return Err(if found.is_empty() {
            "No configuration authority discovered."
        } else {
            "Multiple configuration authorities discovered; configure an explicit endpoint."
        });
    }
    Ok(found.into_iter().next().unwrap())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connected_subnet_broadcast_needs_no_default_route() {
        let routes="Iface Destination Gateway Flags RefCnt Use Metric Mask\neth0 000014AC 00000000 0001 0 0 0 0000FFFF\neth0 00000000 010014AC 0003 0 0 0 00000000\n";
        assert_eq!(
            broadcasts(routes, 50071),
            vec!["172.20.255.255:50071".parse::<SocketAddr>().unwrap()]
        );
    }
    #[test]
    fn ambiguous_and_missing_authorities_fail_closed() {
        assert!(select(BTreeSet::new()).is_err());
        assert!(select(["a:1".into(), "b:1".into()].into()).is_err());
        assert_eq!(select(["a:1".into()].into()).unwrap(), "a:1");
    }
    #[tokio::test]
    async fn real_udp_discovery_filters_authority_and_echoes_nonce() {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let bind = probe.local_addr().unwrap().to_string();
        drop(probe);
        let server_bind = bind.clone();
        let server =
            tokio::spawn(async move { serve(&server_bind, "test-authority".into(), 50070).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            discover(&bind, "test-authority", Duration::from_millis(80))
                .await
                .unwrap(),
            "127.0.0.1:50070"
        );
        assert!(discover(&bind, "other", Duration::from_millis(80))
            .await
            .is_err());
        server.abort();
    }
}
