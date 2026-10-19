//! IcecastAdmin fixture: the real wire contract, no production credentials or mutations.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tonic::{Request, Response, Status};
#[allow(clippy::result_large_err)]
pub mod rpc {
    tonic::include_proto!("stationd.admin.icecast.v1");
}
use rpc::*;
#[derive(Clone)]
pub struct ControlState {
    pub source_connected: bool,
    pub listener_connected: bool,
    pub title: String,
    pub input: String,
}
impl Default for ControlState {
    fn default() -> Self {
        Self {
            source_connected: true,
            listener_connected: true,
            title: "Fixture programme".into(),
            input: "a".into(),
        }
    }
}
#[derive(Clone)]
pub struct IcecastFixture {
    pub mode: Arc<AtomicUsize>,
    pub requests: Arc<AtomicUsize>,
    pub token: Arc<String>,
    pub commands: Arc<AtomicUsize>,
    pub state: Arc<std::sync::Mutex<ControlState>>,
}
impl IcecastFixture {
    fn authorize<T>(&self, request: &Request<T>) -> Result<(), Status> {
        if request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            != Some(format!("Bearer {}", self.token).as_str())
        {
            return Err(Status::unauthenticated("fixture credential required"));
        }
        self.commands.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn management_ready(&self) -> Result<(), Status> {
        match self.mode.load(Ordering::SeqCst) {
            1 => Err(Status::unavailable("fixture-only management failure")),
            2 => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn mount(mount: &str) -> Result<(), Status> {
        if mount == "/live" {
            Ok(())
        } else {
            Err(Status::invalid_argument("fixture-only mount rejected"))
        }
    }
    fn route(&self) -> RouteStatus {
        RouteStatus {
            output_mount: "/radio.mp3".into(),
            input_streams: vec![
                RouteInput {
                    id: "a".into(),
                    mount: "/live".into(),
                },
                RouteInput {
                    id: "b".into(),
                    mount: "/test-b.mp3".into(),
                },
            ],
            input: self.state.lock().unwrap().input.clone(),
            connected: true,
            inputs: vec!["a".into(), "b".into()],
            problem: String::new(),
        }
    }
}
#[tonic::async_trait]
impl icecast_admin_server::IcecastAdmin for IcecastFixture {
    async fn get_status(&self, request: Request<Empty>) -> Result<Response<ServerStatus>, Status> {
        let expected = format!("Bearer {}", self.token);
        if request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            != Some(expected.as_str())
        {
            return Err(Status::unauthenticated("fixture credential required"));
        }
        self.requests.fetch_add(1, Ordering::SeqCst);
        match self.mode.load(Ordering::SeqCst) {
            1 => return Err(Status::unavailable("fixture-only Icecast failure")),
            2 => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            _ => {}
        }
        let state = self.state.lock().unwrap().clone();
        Ok(Response::new(ServerStatus {
            server_id: "Fixture Icecast".into(),
            mounts: vec![
                MountStatus {
                    mount: "/radio.mp3".into(),
                    connected: true,
                    title: "Fixture output".into(),
                    content_type: "audio/mpeg".into(),
                    ..Default::default()
                },
                MountStatus {
                    mount: "/test-b.mp3".into(),
                    connected: true,
                    title: "Fixture B".into(),
                    content_type: "audio/mpeg".into(),
                    ..Default::default()
                },
                MountStatus {
                    mount: "/live".into(),
                    connected: state.source_connected,
                    listeners: if state.source_connected {
                        if state.listener_connected {
                            7
                        } else {
                            6
                        }
                    } else {
                        0
                    },
                    title: state.title,
                    content_type: "audio/mpeg".into(),
                },
                MountStatus {
                    mount: "/offline".into(),
                    ..Default::default()
                },
            ],
        }))
    }
    async fn get_monitoring_status(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<JsonReply>, Status> {
        Err(Status::unimplemented(
            "Only stream status is served by this fixture.",
        ))
    }

    async fn get_history_status(&self, _: Request<Empty>) -> Result<Response<JsonReply>, Status> {
        Err(Status::unimplemented(
            "Only stream status is served by this fixture.",
        ))
    }

    async fn query_history(
        &self,
        _: Request<HistoryRequest>,
    ) -> Result<Response<JsonReply>, Status> {
        Err(Status::unimplemented(
            "Only stream status is served by this fixture.",
        ))
    }

    async fn get_config(&self, _: Request<Empty>) -> Result<Response<ConfigDocument>, Status> {
        Err(Status::unimplemented(
            "Only stream status is served by this fixture.",
        ))
    }

    async fn check_config(&self, _: Request<ConfigText>) -> Result<Response<ConfigCheck>, Status> {
        Err(Status::unimplemented(
            "Only stream status is served by this fixture.",
        ))
    }

    async fn get_geography(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<GeographySnapshot>, Status> {
        Err(Status::unimplemented(
            "Only stream status is served by this fixture.",
        ))
    }

    async fn get_route(&self, request: Request<Empty>) -> Result<Response<RouteStatus>, Status> {
        self.authorize(&request)?;
        self.management_ready().await?;
        Ok(Response::new(self.route()))
    }
    async fn switch_route(
        &self,
        request: Request<SwitchRequest>,
    ) -> Result<Response<RouteStatus>, Status> {
        self.authorize(&request)?;
        self.management_ready().await?;
        let input = request.into_inner().input;
        if !["a", "b"].contains(&input.as_str()) {
            return Err(Status::invalid_argument("fixture-only input rejected"));
        }
        self.state.lock().unwrap().input = input;
        Ok(Response::new(self.route()))
    }
    async fn list_listeners(
        &self,
        request: Request<MountRequest>,
    ) -> Result<Response<ListenerList>, Status> {
        self.authorize(&request)?;
        self.management_ready().await?;
        Self::mount(&request.into_inner().mount)?;
        let connected = self.state.lock().unwrap().listener_connected;
        Ok(Response::new(ListenerList {
            listeners: if connected {
                vec![Listener {
                    id: 42,
                    ip: "192.0.2.42".into(),
                    user_agent: "Fixture player".into(),
                    connected_seconds: 60,
                    ..Default::default()
                }]
            } else {
                vec![]
            },
        }))
    }
    async fn set_metadata(
        &self,
        request: Request<MetadataRequest>,
    ) -> Result<Response<ActionReply>, Status> {
        self.authorize(&request)?;
        self.management_ready().await?;
        let request = request.into_inner();
        Self::mount(&request.mount)?;
        self.state.lock().unwrap().title = request.title;
        Ok(Response::new(ActionReply {
            message: "ok".into(),
        }))
    }
    async fn kick_listener(
        &self,
        request: Request<KickListenerRequest>,
    ) -> Result<Response<ActionReply>, Status> {
        self.authorize(&request)?;
        self.management_ready().await?;
        let request = request.into_inner();
        Self::mount(&request.mount)?;
        if request.id != 42 {
            return Err(Status::not_found("fixture-only unknown listener"));
        }
        self.state.lock().unwrap().listener_connected = false;
        Ok(Response::new(ActionReply {
            message: "ok".into(),
        }))
    }
    async fn kick_source(
        &self,
        request: Request<MountRequest>,
    ) -> Result<Response<ActionReply>, Status> {
        self.authorize(&request)?;
        self.management_ready().await?;
        Self::mount(&request.into_inner().mount)?;
        self.state.lock().unwrap().source_connected = false;
        Ok(Response::new(ActionReply {
            message: "ok".into(),
        }))
    }
}
