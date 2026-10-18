//! Read-only adapter to the existing Icecast administration service.
use stationdeck_services::icecast::{IcecastStatus, StreamStatus};
use tonic::transport::{ClientTlsConfig, Endpoint};
use std::time::Duration;
#[allow(clippy::result_large_err)]
mod rpc { tonic::include_proto!("stationd.admin.icecast.v1"); }
#[derive(Clone)]
pub(crate) struct IcecastTarget { endpoint: Endpoint, token: String }
impl IcecastTarget {
    pub(crate) fn new(address: &str, token: &str) -> Result<Self, &'static str> {
        if token.is_empty() || token.len() > 512 || token.chars().any(char::is_control) {
            return Err("Invalid Icecast administration credential.");
        }
        let mut endpoint = Endpoint::from_shared(address.to_owned()).map_err(|_| "Invalid Icecast administration endpoint.")?;
        let uri = endpoint.uri();
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.authority().is_none_or(|a| a.as_str().contains('@'))
            || uri.path() != "/" || uri.query().is_some() {
            return Err("Invalid Icecast administration endpoint.");
        }
        if uri.scheme_str() == Some("https") {
            endpoint = endpoint.tls_config(ClientTlsConfig::new().with_native_roots()).map_err(|_| "Invalid Icecast TLS configuration.")?;
        }
        Ok(Self { endpoint: endpoint.connect_timeout(Duration::from_secs(2)).timeout(Duration::from_secs(6)), token: token.into() })
    }
    pub(crate) async fn read(&self) -> Result<IcecastStatus, (&'static str, &'static str)> {
        let channel = self.endpoint.connect().await.map_err(|_| ("unavailable", "Icecast administration could not be reached."))?;
        let mut request = tonic::Request::new(rpc::Empty {});
        request.metadata_mut().insert("authorization", format!("Bearer {}", self.token).parse()
            .map_err(|_| ("configuration", "Icecast administration credentials are invalid."))?);
        request.set_timeout(Duration::from_secs(6));
        let reply = rpc::icecast_admin_client::IcecastAdminClient::new(channel)
            .max_decoding_message_size(256 * 1024).get_status(request).await.map_err(|error| match error.code() {
                tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => ("permission_denied", "Icecast administration rejected the server-side credentials."),
                tonic::Code::DeadlineExceeded => ("timeout", "Icecast stream status timed out."),
                _ => ("unavailable", "Icecast stream status is unavailable."),
            })?.into_inner();
        let clean = |s: String, max| s.chars().filter(|c| !c.is_control()).take(max).collect::<String>();
        if reply.mounts.len() > 1024 { return Err(("invalid_response", "Icecast returned too many streams.")); }
        let mut streams: Vec<_> = reply.mounts.into_iter().map(|m| StreamStatus {
            mount: clean(m.mount, 256), connected: m.connected, listeners: m.listeners,
            title: clean(m.title, 1024), content_type: clean(m.content_type, 128),
        }).collect();
        streams.sort_by(|a, b| a.mount.cmp(&b.mount));
        Ok(IcecastStatus { server_id: clean(reply.server_id, 256), streams })
    }
}
