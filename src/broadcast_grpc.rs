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
    SampleListenersRequest, SkipRequest, SkipResponse, State as ProtoState,
};

/// The emitter recorded for actions coming through this service.
const SOURCE: &str = "cli";

pub struct BroadcastGrpc {
    control: StationControl,
    /// Liquidsoap control socket, when wired (`Skip` needs it).
    ls: Option<crate::ls_control::LsControl>,
    /// Media root: a media override naming a file that is not there is
    /// refused at push (not accepted, then dropped when it would air).
    media_root: Option<std::path::PathBuf>,
}

impl BroadcastGrpc {
    pub fn new(control: StationControl) -> Self {
        Self { control, ls: None, media_root: None }
    }

    pub fn with_media_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.media_root = Some(root.into());
        self
    }

    pub fn with_liquidsoap(mut self, ls: crate::ls_control::LsControl) -> Self {
        self.ls = Some(ls);
        self
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
        BroadcastState::Draining => ProtoState::Draining,
        BroadcastState::Sleeping => ProtoState::Sleeping,
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
            Ok(ProtoAction::Pause) => ControlAction::Pause,
            Ok(ProtoAction::Resume) => ControlAction::Resume,
            Ok(ProtoAction::StopWhenIdle) => ControlAction::StopWhenIdle,
            Ok(ProtoAction::Wake) => ControlAction::Wake,
            Ok(ProtoAction::Unspecified) | Err(_) => {
                return Err(Status::invalid_argument(
                    "action must be pause/resume/stop_when_idle/wake (stopping stationd: Station.Shutdown)",
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

    async fn skip(&self, _request: Request<SkipRequest>) -> Result<Response<SkipResponse>, Status> {
        let ls = self
            .ls
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("Liquidsoap is not wired (no [liquidsoap] section)"))?;
        ls.command("stationd.skip")
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?;
        tracing::info!(by = SOURCE, "skip");
        Ok(Response::new(SkipResponse {}))
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
            Some(ProtoContent::MediaPath(p)) => {
                if let Some(root) = &self.media_root {
                    let rel = crate::station_control::normalize_media_ref(&p).map_err(Status::invalid_argument)?;
                    if !root.join(&rel).is_file() {
                        return Err(Status::not_found(format!(
                            "media `{rel}` not found under the media root (moved or removed? `library scan` updates the index)"
                        )));
                    }
                }
                OverrideContent::Media(p)
            }
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
                    // 0 = until the end of the group's cycle.
                    remaining: e.remaining.unwrap_or(0),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn push(path: &str) -> PushOverrideRequest {
        PushOverrideRequest {
            content: Some(ProtoContent::MediaPath(path.into())),
            mode: ProtoMode::Soft as i32,
            expiry: String::new(),
            tracks: 0,
        }
    }

    #[tokio::test]
    async fn a_media_override_on_a_missing_file_is_refused_at_push() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Rock")).unwrap();
        std::fs::write(dir.path().join("Rock/a.mp3"), b"x").unwrap();
        let svc = BroadcastGrpc::new(StationControl::new_in_memory()).with_media_root(dir.path());
        let e = svc.push_override(Request::new(push("rock/a.mp3"))).await.unwrap_err();
        assert_eq!(e.code(), tonic::Code::NotFound, "case matters on the disk");
        assert!(svc.control.list_overrides().is_empty());
        svc.push_override(Request::new(push("Rock/a.mp3"))).await.unwrap();
        assert_eq!(svc.control.list_overrides().len(), 1);
    }
}
