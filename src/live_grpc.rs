//! gRPC transport for the live DJ service (live_v1.proto).
//!
//! Thin translator, same discipline as the other `*_grpc.rs`: every decision
//! lives in `live::LiveHub`.

use tonic::{Request, Response, Status};

use crate::config::LiveConfig;
use crate::live::{LiveError, LiveHub, LiveSession};

pub use crate::proto::live;

use live::live_service_server::LiveService;
use live::{
    GetStatusRequest, HashPasswordRequest, HashPasswordResponse, KickRequest, KickResponse, LiveStatus,
    Refused,
};

pub struct LiveGrpc {
    /// `None` = no `[live]` section.
    wired: Option<(LiveHub, LiveConfig)>,
    /// Liquidsoap is wired (a kick can reach the harbor).
    air: bool,
}

impl LiveGrpc {
    pub fn disabled() -> Self {
        Self { wired: None, air: false }
    }

    pub fn new(hub: LiveHub, cfg: LiveConfig, air: bool) -> Self {
        Self { wired: Some((hub, cfg)), air }
    }
}

fn session(s: &LiveSession) -> live::LiveSession {
    live::LiveSession {
        dj: s.dj.clone(),
        rule_id: s.rule_id.clone(),
        occurrence: s.occurrence.clone(),
        address: s.address.clone(),
        since: s.since.0,
    }
}

#[tonic::async_trait]
impl LiveService for LiveGrpc {
    async fn get_status(&self, _req: Request<GetStatusRequest>) -> Result<Response<LiveStatus>, Status> {
        let Some((hub, cfg)) = &self.wired else {
            return Ok(Response::new(LiveStatus { enabled: false, ..Default::default() }));
        };
        let st = hub.status();
        let path = hub.djs_path().to_path_buf();
        let djs = tokio::task::spawn_blocking(move || crate::live::load_djs(&path))
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let (djs_count, djs_error) = match djs {
            Ok(d) => (d.len() as u32, String::new()),
            Err(e) => (0, e),
        };
        let (refusal_dj, refusal_reason, refusal_at) = st
            .last_refusal
            .map(|(dj, why, at)| (dj, why, at.0))
            .unwrap_or_default();
        Ok(Response::new(LiveStatus {
            enabled: true,
            djs_path: cfg.djs_path.to_string_lossy().into_owned(),
            harbor_port: cfg.harbor_port as u32,
            mount: cfg.mount.clone(),
            silence_timeout_s: cfg.silence_timeout,
            on_air: st.on_air.as_ref().map(session),
            last: st.last.as_ref().map(|l| session(&l.session)),
            last_reason: st.last.as_ref().map(|l| l.reason.as_str().to_string()).unwrap_or_default(),
            last_ended_at: st.last.as_ref().map(|l| l.at.0).unwrap_or(0),
            refused: st.refused.into_iter().map(|(dj, occurrence)| Refused { dj, occurrence }).collect(),
            last_refusal_dj: refusal_dj,
            last_refusal_reason: refusal_reason,
            last_refusal_at: refusal_at,
            djs_error,
            djs_count,
        }))
    }

    async fn kick(&self, _req: Request<KickRequest>) -> Result<Response<KickResponse>, Status> {
        let Some((hub, _)) = &self.wired else {
            return Err(Status::failed_precondition("no [live] section: no harbor"));
        };
        if !self.air {
            return Err(Status::unavailable("Liquidsoap is not wired: nothing to disconnect"));
        }
        match hub.kick() {
            Ok(dj) => Ok(Response::new(KickResponse { dj })),
            Err(LiveError::NoLive) => Err(Status::failed_precondition("no DJ on air")),
        }
    }

    async fn hash_password(
        &self,
        req: Request<HashPasswordRequest>,
    ) -> Result<Response<HashPasswordResponse>, Status> {
        let password = req.into_inner().password;
        let hash = tokio::task::spawn_blocking(move || crate::live::hash_password(&password))
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .map_err(Status::invalid_argument)?;
        Ok(Response::new(HashPasswordResponse { hash }))
    }
}
