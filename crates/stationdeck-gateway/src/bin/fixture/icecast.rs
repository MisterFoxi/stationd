//! IcecastAdmin fixture: the real wire contract, no production credentials or mutations.
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use tonic::{Request, Response, Status};
#[allow(clippy::result_large_err)]
pub mod rpc { tonic::include_proto!("stationd.admin.icecast.v1"); }
use rpc::*;
#[derive(Clone)]
pub struct IcecastFixture {
    pub mode: Arc<AtomicUsize>,
    pub requests: Arc<AtomicUsize>,
    pub token: Arc<String>,
}
#[tonic::async_trait]
impl icecast_admin_server::IcecastAdmin for IcecastFixture {
    async fn get_status(&self, request: Request<Empty>) -> Result<Response<ServerStatus>, Status> {
        let expected = format!("Bearer {}", self.token);
        if request.metadata().get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
            return Err(Status::unauthenticated("fixture credential required"));
        }
        self.requests.fetch_add(1, Ordering::SeqCst);
        match self.mode.load(Ordering::SeqCst) {
            1 => return Err(Status::unavailable("fixture-only Icecast failure")),
            2 => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            _ => {},
        }
        Ok(Response::new(ServerStatus { server_id: "Fixture Icecast".into(), mounts: vec![
            MountStatus { mount: "/live".into(), connected: true, listeners: 7, title: "Fixture programme".into(), content_type: "audio/mpeg".into() },
            MountStatus { mount: "/offline".into(), ..Default::default() },
        ] }))
    }
    async fn get_monitoring_status(&self, _: Request<Empty>) -> Result<Response<JsonReply>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn get_history_status(&self, _: Request<Empty>) -> Result<Response<JsonReply>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn query_history(&self, _: Request<HistoryRequest>) -> Result<Response<JsonReply>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn get_config(&self, _: Request<Empty>) -> Result<Response<ConfigDocument>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn check_config(&self, _: Request<ConfigText>) -> Result<Response<ConfigCheck>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn get_geography(&self, _: Request<Empty>) -> Result<Response<GeographySnapshot>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn get_route(&self, _: Request<Empty>) -> Result<Response<RouteStatus>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn switch_route(&self, _: Request<SwitchRequest>) -> Result<Response<RouteStatus>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn list_listeners(&self, _: Request<MountRequest>) -> Result<Response<ListenerList>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn set_metadata(&self, _: Request<MetadataRequest>) -> Result<Response<ActionReply>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn kick_listener(&self, _: Request<KickListenerRequest>) -> Result<Response<ActionReply>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }

    async fn kick_source(&self, _: Request<MountRequest>) -> Result<Response<ActionReply>, Status> {
        Err(Status::unimplemented("Only stream status is served by this fixture."))
    }
}
