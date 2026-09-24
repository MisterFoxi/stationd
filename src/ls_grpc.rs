//! gRPC transport for the Liquidsoap service (liquidsoap_v1.proto).
//!
//! Thin translator, same discipline as the other `*_grpc.rs`: the script is
//! rendered by `ls_script`, the status comes from `ls_bridge`.

use tonic::{Request, Response, Status};

use crate::config::LiquidsoapConfig;
use crate::ls_bridge::LsBridge;

pub use crate::proto::liquidsoap;

use liquidsoap::liquidsoap_service_server::LiquidsoapService;
use liquidsoap::{
    GetStatusRequest, LiquidsoapStatus, RenderScriptRequest, RenderScriptResponse,
};

pub struct LsGrpc {
    /// `None` = no `[liquidsoap]` section: nothing airs.
    wired: Option<Wired>,
}

struct Wired {
    config: LiquidsoapConfig,
    script: String,
    bridge: LsBridge,
}

impl LsGrpc {
    pub fn disabled() -> Self {
        Self { wired: None }
    }

    pub fn new(config: LiquidsoapConfig, script: String, bridge: LsBridge) -> Self {
        Self { wired: Some(Wired { config, script, bridge }) }
    }
}

#[tonic::async_trait]
impl LiquidsoapService for LsGrpc {
    async fn render_script(
        &self,
        _req: Request<RenderScriptRequest>,
    ) -> Result<Response<RenderScriptResponse>, Status> {
        let w = self
            .wired
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("no [liquidsoap] section in stationd.toml"))?;
        Ok(Response::new(RenderScriptResponse {
            script: w.script.clone(),
            path: w.config.script_path.to_string_lossy().into_owned(),
        }))
    }

    async fn get_status(
        &self,
        _req: Request<GetStatusRequest>,
    ) -> Result<Response<LiquidsoapStatus>, Status> {
        let Some(w) = &self.wired else {
            return Ok(Response::new(LiquidsoapStatus { enabled: false, ..Default::default() }));
        };
        let s = w.bridge.status();
        let (last_reply, last_detail) = match &s.last_reply {
            None => (String::new(), String::new()),
            Some(r) => {
                let detail = match r.kind.as_str() {
                    "file" => r.uri.clone(),
                    "halted" => r.state.clone(),
                    _ => r.reason.clone(),
                };
                (r.kind.clone(), detail)
            }
        };
        let (on_air_kind, on_air_media, on_air_playlist, on_air_since) = match &s.on_air {
            None => (String::new(), String::new(), String::new(), 0),
            Some(a) => (
                a.kind.as_str().to_string(),
                a.media_path.clone().unwrap_or_default(),
                a.playlist_ref.clone().unwrap_or_default(),
                a.since,
            ),
        };
        Ok(Response::new(LiquidsoapStatus {
            enabled: true,
            http_bind: w.config.http_bind.clone(),
            script_path: w.config.script_path.to_string_lossy().into_owned(),
            pulls: s.pulls,
            last_pull_at: s.last_pull_at.unwrap_or(0),
            last_reply,
            last_detail,
            on_air_kind,
            on_air_media,
            on_air_playlist,
            on_air_since,
            tracks_started: s.tracks_started,
            next_media: s.next.as_ref().map(|n| n.media_path.clone()).unwrap_or_default(),
            next_playlist: s
                .next
                .as_ref()
                .and_then(|n| n.playlist_ref.clone())
                .unwrap_or_default(),
        }))
    }
}
