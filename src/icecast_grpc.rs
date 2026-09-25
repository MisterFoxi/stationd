//! gRPC transport for the Icecast service (icecast_v1.proto).
//!
//! Thin translator, same discipline as the other `*_grpc.rs`: everything it
//! shows was read by the sampler (`icecast::spawn_sampler`) and kept in the
//! `IcecastMonitor`; this module only projects it on our mounts.

use tonic::{Request, Response, Status};

use crate::icecast::{IcecastHealth, IcecastMonitor};

pub use crate::proto::icecast;

use icecast::icecast_service_server::IcecastService;
use icecast::{GetStatusRequest, IcecastStatus, MountStatus, RenderConfigRequest, RenderConfigResponse};

pub struct IcecastGrpc {
    /// `None` = no `[icecast]` section: the audience is never read.
    wired: Option<Wired>,
    /// `(xml, path)` = the generated `icecast.xml` (`[icecast.server]`).
    config: Option<(String, String)>,
}

struct Wired {
    monitor: IcecastMonitor,
    server: String,
    poll_interval_s: u32,
    mounts: Vec<String>,
}

impl IcecastGrpc {
    pub fn disabled() -> Self {
        Self { wired: None, config: None }
    }

    pub fn new(monitor: IcecastMonitor, server: String, poll_interval_s: u32, mounts: Vec<String>) -> Self {
        Self { wired: Some(Wired { monitor, server, poll_interval_s, mounts }), config: None }
    }

    /// The generated `icecast.xml` and where it is written.
    pub fn with_config(mut self, xml: String, path: String) -> Self {
        self.config = Some((xml, path));
        self
    }
}

/// Project the monitor on our mounts (pure: tested without gRPC).
pub fn status(h: &IcecastHealth, server: &str, poll_interval_s: u32, mounts: &[String]) -> IcecastStatus {
    let stats = h.last_ok.as_ref().map(|(_, s)| s);
    IcecastStatus {
        enabled: true,
        server: server.to_string(),
        poll_interval_s,
        server_id: stats.and_then(|s| s.server_id.clone()).unwrap_or_default(),
        last_ok_at: h.last_ok.as_ref().map(|(at, _)| at.0).unwrap_or(0),
        problem: h.problem.as_ref().map(|(_, p)| p.clone()).unwrap_or_default(),
        problem_at: h.problem.as_ref().map(|(at, _)| at.0).unwrap_or(0),
        audience: h.audience,
        mounts: mounts
            .iter()
            .map(|m| match stats.and_then(|s| s.source(m)) {
                None => MountStatus { mount: m.clone(), ..Default::default() },
                Some(s) => MountStatus {
                    mount: m.clone(),
                    present: true,
                    connected: s.connected(),
                    stream_start: s.stream_start.clone().unwrap_or_default(),
                    source_ip: s.source_ip.clone().unwrap_or_default(),
                    user_agent: s.user_agent.clone().unwrap_or_default(),
                    bitrate: s.bitrate.clone().unwrap_or_default(),
                    read_kbps: h.read_rate.get(m).map(|&(k, _)| k),
                    read_window_s: h.read_rate.get(m).map(|&(_, w)| w).unwrap_or(0),
                    listeners: s.listeners,
                    listener_peak: s.listener_peak,
                    title: s.title.clone().unwrap_or_default(),
                    content_type: s.content_type.clone().unwrap_or_default(),
                },
            })
            .collect(),
    }
}

#[tonic::async_trait]
impl IcecastService for IcecastGrpc {
    async fn get_status(
        &self,
        _req: Request<GetStatusRequest>,
    ) -> Result<Response<IcecastStatus>, Status> {
        let Some(w) = &self.wired else {
            return Ok(Response::new(IcecastStatus { enabled: false, ..Default::default() }));
        };
        Ok(Response::new(status(&w.monitor.snapshot(), &w.server, w.poll_interval_s, &w.mounts)))
    }

    async fn render_config(
        &self,
        _req: Request<RenderConfigRequest>,
    ) -> Result<Response<RenderConfigResponse>, Status> {
        let (xml, path) = self.config.as_ref().ok_or_else(|| {
            Status::failed_precondition("no [icecast.server] section: stationd does not generate icecast.xml")
        })?;
        // Read now: the operator may have fixed the group since start-up.
        let access = crate::icecast_xml::access(std::path::Path::new(path)).unwrap_or_else(|e| e);
        Ok(Response::new(RenderConfigResponse { xml: xml.clone(), path: path.clone(), access }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icecast::{apply_sample, parse_stats};
    use crate::station_control::StationControl;

    const STATS: &str = r#"<icestats><server_id>Icecast 2.5.0</server_id>
        <source mount="/radio.mp3"><audio_info>channels=2;samplerate=44100;bitrate=192</audio_info>
        <listeners>3</listeners><listener_peak>4</listener_peak><source_ip>127.0.0.1</source_ip>
        <stream_start_iso8601>2026-09-24T16:05:09+0000</stream_start_iso8601>
        <title>A - B</title><server_type>audio/mpeg</server_type>
        <user_agent>Liquidsoap/2.4.0</user_agent><total_bytes_read>100</total_bytes_read></source>
        </icestats>"#;

    fn ours() -> Vec<String> {
        vec!["/radio.mp3".into(), "/backup.mp3".into()]
    }

    #[test]
    fn never_read_shows_every_mount_unknown() {
        let s = status(&IcecastHealth::default(), "127.0.0.1:8000", 15, &ours());
        assert!(s.enabled);
        assert_eq!(s.last_ok_at, 0);
        assert_eq!(s.audience, None);
        assert_eq!(s.mounts.len(), 2);
        assert!(!s.mounts[0].present);
    }

    #[test]
    fn projects_our_mounts_and_the_problem() {
        let control = StationControl::new_in_memory();
        let monitor = IcecastMonitor::default();
        // /backup.mp3 absent → audience unknown, the problem says why.
        apply_sample(Ok(parse_stats(STATS).unwrap()), &ours(), &control, &monitor);
        let s = status(&monitor.snapshot(), "127.0.0.1:8000", 15, &ours());
        assert_eq!(s.server_id, "Icecast 2.5.0");
        assert!(s.last_ok_at > 0);
        assert_eq!(s.audience, None);
        assert!(s.problem.contains("/backup.mp3"), "{}", s.problem);
        let r = &s.mounts[0];
        assert!(r.present && r.connected);
        assert_eq!(r.bitrate, "192");
        assert_eq!(r.listeners, Some(3));
        assert_eq!(r.listener_peak, Some(4));
        assert_eq!(r.title, "A - B");
        assert_eq!(r.user_agent, "Liquidsoap/2.4.0");
        assert_eq!(r.read_kbps, None, "one read: no rate yet");
        assert!(!s.mounts[1].present);
    }
}
