//! gRPC transport for the broadcast control service (broadcast_v1.proto).
//!
//! Thin translator over `StationControl`, same discipline as the other
//! `*_grpc.rs`: map the proto to the control and back, no logic here. The CLI
//! acts as the emitter `cli` (a plugin acts through its host surface instead).

use tonic::{Request, Response, Status};

use crate::station_control::{
    BroadcastState, ControlAction, ControlError, OverrideContent, OverrideMode, OverrideRequest,
    StationControl,
};

pub use crate::proto::broadcast;

use broadcast::broadcast_service_server::BroadcastService;
use broadcast::{
    control_request::Action as ProtoAction, push_override_request::Content as ProtoContent,
    push_override_request::Mode as ProtoMode, BroadcastStatus, ClearOverridesRequest,
    ClearOverridesResponse, ControlRequest, ControlResponse, GetStateRequest,
    ListOverridesRequest, ListOverridesResponse, PushOverrideRequest, PushOverrideResponse,
    SampleListenersRequest, State as ProtoState,
};

/// The emitter recorded for actions coming through this service.
const SOURCE: &str = "cli";

pub struct BroadcastGrpc {
    control: StationControl,
}

impl BroadcastGrpc {
    pub fn new(control: StationControl) -> Self {
        Self { control }
    }

    fn status(&self) -> BroadcastStatus {
        BroadcastStatus {
            state: map_state(self.control.state()) as i32,
            listeners: self.control.listeners(),
        }
    }
}

fn map_state(s: BroadcastState) -> ProtoState {
    match s {
        BroadcastState::Running => ProtoState::Running,
        BroadcastState::Paused => ProtoState::Paused,
        BroadcastState::Stopped => ProtoState::Stopped,
        BroadcastState::Draining => ProtoState::Draining,
    }
}

fn map_control_error(e: ControlError) -> Status {
    match e {
        ControlError::Refused(m) => Status::failed_precondition(m),
        ControlError::InvalidOverride(m) => Status::invalid_argument(m),
    }
}

#[tonic::async_trait]
impl BroadcastService for BroadcastGrpc {
    async fn get_state(
        &self,
        _request: Request<GetStateRequest>,
    ) -> Result<Response<BroadcastStatus>, Status> {
        Ok(Response::new(self.status()))
    }

    async fn control(
        &self,
        request: Request<ControlRequest>,
    ) -> Result<Response<ControlResponse>, Status> {
        let action = match ProtoAction::try_from(request.into_inner().action) {
            Ok(ProtoAction::Stop) => ControlAction::Stop,
            Ok(ProtoAction::Pause) => ControlAction::Pause,
            Ok(ProtoAction::Resume) => ControlAction::Resume,
            Ok(ProtoAction::StopWhenIdle) => ControlAction::StopWhenIdle,
            Ok(ProtoAction::Unspecified) | Err(_) => {
                return Err(Status::invalid_argument(
                    "action must be stop/pause/resume/stop_when_idle",
                ))
            }
        };
        let before = self.control.state();
        let t = self.control.apply(action, SOURCE).map_err(map_control_error)?;
        let (from, to, changed) = match t {
            Some(t) => (t.from, t.to, true),
            None => (before, before, false),
        };
        Ok(Response::new(ControlResponse {
            from: map_state(from) as i32,
            to: map_state(to) as i32,
            changed,
        }))
    }

    async fn sample_listeners(
        &self,
        request: Request<SampleListenersRequest>,
    ) -> Result<Response<BroadcastStatus>, Status> {
        self.control.sample_listeners(request.into_inner().count);
        Ok(Response::new(self.status()))
    }

    async fn push_override(
        &self,
        request: Request<PushOverrideRequest>,
    ) -> Result<Response<PushOverrideResponse>, Status> {
        let req = request.into_inner();
        let content = match req.content {
            Some(ProtoContent::MediaPath(p)) => OverrideContent::Media(p),
            Some(ProtoContent::PlaylistRef(r)) => OverrideContent::Playlist(r),
            None => return Err(Status::invalid_argument("media_path or playlist_ref is required")),
        };
        let mode = match ProtoMode::try_from(req.mode) {
            Ok(ProtoMode::Hard) => OverrideMode::Hard,
            Ok(ProtoMode::Soft) | Ok(ProtoMode::Unspecified) => OverrideMode::Soft,
            Err(_) => return Err(Status::invalid_argument("unknown override mode")),
        };
        let out = self
            .control
            .push_override(
                OverrideRequest {
                    content,
                    mode,
                    expiry: (!req.expiry.trim().is_empty()).then(|| req.expiry.trim().to_string()),
                    tracks: (req.tracks > 0).then_some(req.tracks),
                },
                SOURCE,
            )
            .map_err(map_control_error)?;
        Ok(Response::new(PushOverrideResponse {
            id: out.id,
            degraded: out.degraded,
            pending: out.pending as u32,
        }))
    }

    async fn list_overrides(
        &self,
        _request: Request<ListOverridesRequest>,
    ) -> Result<Response<ListOverridesResponse>, Status> {
        let overrides = self
            .control
            .list_overrides()
            .into_iter()
            .map(|e| {
                let (media_path, playlist_ref) = match e.content {
                    OverrideContent::Media(p) => (p, String::new()),
                    OverrideContent::Playlist(r) => (String::new(), r),
                };
                broadcast::Override {
                    id: e.id,
                    media_path,
                    playlist_ref,
                    mode: match e.mode {
                        OverrideMode::Soft => "soft".into(),
                        OverrideMode::Hard => "hard".into(),
                    },
                    source: e.source,
                    pushed_at: e.pushed_at.0,
                    expires_at: e.expires_at.map(|x| x.0).unwrap_or(0),
                    remaining: e.remaining,
                }
            })
            .collect();
        Ok(Response::new(ListOverridesResponse { overrides }))
    }

    async fn clear_overrides(
        &self,
        request: Request<ClearOverridesRequest>,
    ) -> Result<Response<ClearOverridesResponse>, Status> {
        let id = request.into_inner().id;
        let removed = self.control.clear_overrides((id != 0).then_some(id));
        Ok(Response::new(ClearOverridesResponse {
            removed: removed as u32,
        }))
    }
}
