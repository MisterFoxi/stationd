//! Detailed Icecast snapshots. No persistence or geolocation policy here.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// IDs are scoped to an Icecast process and mount, not stable people IDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Listener {
    pub id: String,
    pub ip: std::net::IpAddr,
    pub connected_seconds: u64,
    pub user_agent: Option<String>,
}

/// Percent-encode one query value, including slash, ampersand and UTF-8.
pub fn mount_query(mount: &str) -> String {
    let mut out = String::new();
    for b in mount.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            write!(&mut out, "%{b:02X}").expect("writing to String");
        }
    }
    out
}

/// Accept legacy listclients XML (including namespaced 2.5 XML).
/// Reject partial/malformed snapshots rather than inventing departures.
/// Errors deliberately exclude XML contents and client IPs.
pub fn parse_clients(xml: &str, mount: &str) -> Result<Vec<Listener>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|_| "listclients: invalid XML")?;
    let mut root = doc.root_element();
    if root.tag_name().name() == "report" {
        if root.descendants().any(|n| n.is_element() && n.tag_name().name() == "incident") {
            return Err("listclients: Icecast incident".into());
        }
        root = root.descendants().find(|n| n.is_element() && n.tag_name().name() == "icestats")
            .ok_or("listclients: missing icestats")?;
    }
    if root.tag_name().name() != "icestats" {
        return Err("listclients: unexpected root".into());
    }
    let mut sources = root.children().filter(|n| n.is_element() && n.tag_name().name() == "source");
    let source = sources.next().ok_or("listclients: missing source")?;
    if sources.next().is_some() || source.attribute("mount") != Some(mount) {
        return Err("listclients: unexpected mount".into());
    }
    let text = |node: roxmltree::Node<'_, '_>, name: &str| -> Option<String> {
        node.children().find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case(name))
            .and_then(|n| n.text()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
    };
    let count: usize = text(source, "Listeners").ok_or("listclients: missing count")?
        .parse().map_err(|_| "listclients: invalid count")?;
    if count > 10_000 {
        return Err("listclients: too many listeners".into());
    }
    let mut ids = HashSet::new();
    let mut listeners = Vec::new();
    for node in source.children().filter(|n| n.is_element() && n.tag_name().name() == "listener") {
        let id: u64 = text(node, "ID").ok_or("listclients: missing ID")?
            .parse().map_err(|_| "listclients: invalid ID")?;
        if !ids.insert(id) || listeners.len() >= 10_000 {
            return Err("listclients: duplicate ID or too many listeners".into());
        }
        listeners.push(Listener {
            id: id.to_string(),
            ip: text(node, "IP").ok_or("listclients: missing IP")?
                .parse().map_err(|_| "listclients: invalid IP")?,
            connected_seconds: text(node, "Connected").ok_or("listclients: missing Connected")?
                .parse().map_err(|_| "listclients: invalid Connected")?,
            user_agent: text(node, "UserAgent"),
        });
    }
    if listeners.len() != count {
        return Err("listclients: inconsistent count".into());
    }
    Ok(listeners)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<icestats><source mount="/radio"><Listeners>1</Listeners>
        <listener><ID>42</ID><IP>2001:db8::1</IP><Connected>12</Connected>
        <UserAgent>A &amp; B</UserAgent></listener></source></icestats>"#;

    #[test]
    fn parses_details_and_namespaces() {
        let listeners = parse_clients(XML, "/radio").unwrap();
        assert_eq!(parse_clients(&XML.replace("Listeners", "listeners").replace("ID", "id").replace("IP", "ip").replace("Connected", "connected").replace("UserAgent", "useragent"), "/radio").unwrap(), listeners);
        assert_eq!(listeners[0].id, "42");
        assert_eq!(listeners[0].connected_seconds, 12);
        assert_eq!(listeners[0].user_agent.as_deref(), Some("A & B"));
        assert_eq!(parse_clients(&XML.replace("<icestats>", "<icestats xmlns=\"urn:icecast\">"), "/radio").unwrap(), listeners);
        assert_eq!(parse_clients(&XML.replace("2001:db8::1", "192.0.2.1"), "/radio").unwrap().len(), 1);
    }

    #[test]
    fn unknown_is_not_empty() {
        for bad in [XML.replace("<Listeners>1", "<Listeners>0"),
                    XML.replace("<ID>42</ID>", ""),
                    XML.replace("2001:db8::1", "not-an-ip"),
                    XML.replace("<Connected>12", "<Connected>-1"),
                    "<icestats/>".into(), "<html/>".into(), "broken".into()] {
            assert!(parse_clients(&bad, "/radio").is_err());
        }
        assert!(parse_clients(XML, "/other").is_err());
        assert!(parse_clients(r#"<icestats><source mount="/radio"><Listeners>0</Listeners></source></icestats>"#, "/radio").unwrap().is_empty());
    }

    #[test]
    fn encodes_query_delimiters() {
        assert_eq!(mount_query("/a b&x=1%?#"), "%2Fa%20b%26x%3D1%25%3F%23");
        assert_eq!(mount_query("/\u{e9}"), "%2F%C3%A9");
    }

    #[test]
    fn rejects_duplicate_ids_and_report_incidents() {
        let start = XML.find("<listener>").unwrap();
        let end = XML.find("</listener>").unwrap() + "</listener>".len();
        let duplicated = XML.replace("</source>", &format!("{}</source>", &XML[start..end]))
            .replace("<Listeners>1", "<Listeners>2");
        assert!(parse_clients(&duplicated, "/radio").is_err());
        let wrapped = format!("<report><extension>{XML}</extension></report>");
        assert_eq!(parse_clients(&wrapped, "/radio").unwrap().len(), 1);
        assert!(parse_clients(&wrapped.replace("</report>", "<incident/></report>"), "/radio").is_err());
    }
}
