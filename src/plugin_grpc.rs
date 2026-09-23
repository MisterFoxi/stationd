//! gRPC transport for the plugin service (plugin_v1.proto).
//!
//! Thin translator over `PluginHandle`, same discipline as the other
//! `*_grpc.rs`: map the proto to the actor and back, no logic here.

use tonic::{Request, Response, Status};

use crate::plugin::{Action, PluginHandle, PluginInfo as CoreInfo};

pub use crate::proto::plugin;

use plugin::plugin_service_server::PluginService;
use plugin::{
    plugin_control_request::Action as ProtoAction, PluginControlRequest, PluginControlResponse,
    PluginInfo, PluginListRequest, PluginListResponse,
};

pub struct PluginGrpc {
    handle: PluginHandle,
}

impl PluginGrpc {
    pub fn new(handle: PluginHandle) -> Self {
        Self { handle }
    }
}

fn map_info(i: CoreInfo) -> PluginInfo {
    PluginInfo {
        name: i.name,
        enabled: i.enabled,
        order: i.order,
        state: i.state,
        reason: i.reason,
        failures: i.failures,
        capabilities: i.capabilities,
    }
}

#[tonic::async_trait]
impl PluginService for PluginGrpc {
    async fn list(
        &self,
        _request: Request<PluginListRequest>,
    ) -> Result<Response<PluginListResponse>, Status> {
        let plugins = self.handle.list().await.into_iter().map(map_info).collect();
        Ok(Response::new(PluginListResponse { plugins }))
    }

    async fn control(
        &self,
        request: Request<PluginControlRequest>,
    ) -> Result<Response<PluginControlResponse>, Status> {
        let req = request.into_inner();
        let action = match ProtoAction::try_from(req.action) {
            Ok(ProtoAction::Start) => Action::Start,
            Ok(ProtoAction::Stop) => Action::Stop,
            Ok(ProtoAction::Restart) => Action::Restart,
            Ok(ProtoAction::Reload) => Action::Reload,
            Ok(ProtoAction::Unspecified) | Err(_) => {
                return Err(Status::invalid_argument("action must be start/stop/restart/reload"))
            }
        };
        let info = self
            .handle
            .control(&req.name, action)
            .await
            .map_err(Status::not_found)?;
        Ok(Response::new(PluginControlResponse {
            plugin: Some(map_info(info)),
        }))
    }
}
