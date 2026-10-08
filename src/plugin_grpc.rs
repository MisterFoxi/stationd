//! gRPC transport for the plugin service (plugin_v1.proto).
//!
//! Thin translator over `PluginHandle`, same discipline as the other
//! `*_grpc.rs`: map the proto to the actor and back, no logic here.

use tonic::{Request, Response, Status};

use base64::Engine as _;

use crate::plugin::{Action, DbAdminError, PluginHandle, PluginInfo as CoreInfo};
use crate::plugin_db::DbError;

pub use crate::proto::plugin;

use plugin::plugin_service_server::PluginService;
use plugin::{
    plugin_control_request::Action as ProtoAction, plugin_db_value::Kind, PluginControlRequest,
    PluginControlResponse, PluginDbInfoRequest, PluginDbInfoResponse, PluginDbQueryRequest,
    PluginDbQueryResponse, PluginDbResetRequest, PluginDbResetResponse, PluginDbRow, PluginDbTable,
    PluginDbValue, PluginInfo, PluginListRequest, PluginListResponse,
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
        configurable: i.configurable,
        tabs: i
            .tabs
            .into_iter()
            .map(|t| plugin::PluginTab {
                id: t.id,
                title: t.title,
                description: t.description,
                kind: t.kind,
            })
            .collect(),
        operator_notice: i.operator_notice.map(|notice| match notice {
            crate::plugin::OperatorNotice::AutoSleep { max_connection_age } => {
                plugin::OperatorNotice {
                    code: plugin::operator_notice::Code::AutoSleep as i32,
                    max_connection_age,
                }
            }
        }),
        name: i.name,
        enabled: i.enabled,
        order: i.order,
        state: i.state,
        reason: i.reason,
        failures: i.failures,
        capabilities: i.capabilities,
    }
}

fn config_status(e: String) -> Status {
    if e.starts_with("conflict:") {
        Status::aborted(e)
    } else if e == "unknown plugin" {
        Status::not_found(e)
    } else {
        Status::failed_precondition(e)
    }
}
fn db_status(e: DbAdminError) -> Status {
    match e {
        DbAdminError::UnknownPlugin(_) => Status::not_found(e.to_string()),
        DbAdminError::Precondition(_) => Status::failed_precondition(e.to_string()),
        DbAdminError::Db(DbError::Open { .. }) => Status::failed_precondition(e.to_string()),
        DbAdminError::Db(_) => Status::invalid_argument(e.to_string()),
    }
}

/// A JSON cell of `plugin_db::Rows` back to its SQL kind (a BLOB travels as
/// `{"blob": "<base64>"}` in JSON, as bytes here).
fn map_value(v: serde_json::Value) -> PluginDbValue {
    use serde_json::Value as J;
    let kind = match v {
        J::Null => Kind::Null(true),
        J::Bool(b) => Kind::Integer(i64::from(b)),
        J::Number(n) => match n.as_i64() {
            Some(i) => Kind::Integer(i),
            None => Kind::Real(n.as_f64().unwrap_or(f64::NAN)),
        },
        J::String(s) => Kind::Text(s),
        J::Object(m) => match m.get("blob").and_then(|b| b.as_str()) {
            Some(b) => Kind::Blob(
                base64::engine::general_purpose::STANDARD
                    .decode(b)
                    .unwrap_or_default(),
            ),
            None => Kind::Text(serde_json::Value::Object(m).to_string()),
        },
        other => Kind::Text(other.to_string()),
    };
    PluginDbValue { kind: Some(kind) }
}

#[tonic::async_trait]
impl PluginService for PluginGrpc {
    async fn admin(&self, request: Request<plugin::PluginAdminRequest>) -> Result<Response<plugin::PluginAdminResponse>, Status> {
        let req = request.into_inner();
        let payload = self.handle.admin(&req.name, req.payload).await.map_err(Status::failed_precondition)?;
        Ok(Response::new(plugin::PluginAdminResponse { payload }))
    }
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
                return Err(Status::invalid_argument(
                    "action must be start/stop/restart/reload",
                ))
            }
        };
        let info = self.handle.control(&req.name, action).await.map_err(|e| {
            if e.starts_with("unknown plugin") {
                Status::not_found(e)
            } else {
                Status::failed_precondition(e)
            }
        })?;
        Ok(Response::new(PluginControlResponse {
            plugin: Some(map_info(info)),
        }))
    }

    async fn get_config(
        &self,
        request: Request<plugin::PluginConfigRequest>,
    ) -> Result<Response<plugin::PluginConfigResponse>, Status> {
        let response = self
            .handle
            .config(
                plugin::PluginConfigUpdateRequest {
                    name: request.into_inner().name,
                    ..Default::default()
                },
                true,
            )
            .await
            .map_err(config_status)?;
        Ok(Response::new(response))
    }
    async fn update_config(
        &self,
        request: Request<plugin::PluginConfigUpdateRequest>,
    ) -> Result<Response<plugin::PluginConfigResponse>, Status> {
        let response = self
            .handle
            .config(request.into_inner(), false)
            .await
            .map_err(config_status)?;
        Ok(Response::new(response))
    }

    async fn read_tab(
        &self,
        request: Request<plugin::PluginReadTabRequest>,
    ) -> Result<Response<PluginDbQueryResponse>, Status> {
        let req = request.into_inner();
        let rows = self
            .handle
            .read_tab(&req.name, &req.tab_id)
            .await
            .map_err(db_status)?;
        Ok(Response::new(PluginDbQueryResponse {
            columns: rows.columns,
            rows: rows
                .rows
                .into_iter()
                .map(|r| PluginDbRow {
                    values: r.into_iter().map(map_value).collect(),
                })
                .collect(),
        }))
    }

    async fn db_info(
        &self,
        request: Request<PluginDbInfoRequest>,
    ) -> Result<Response<PluginDbInfoResponse>, Status> {
        let info = self
            .handle
            .db_info(&request.into_inner().name)
            .await
            .map_err(db_status)?;
        let mut resp = PluginDbInfoResponse {
            path: info.path.display().to_string(),
            max_size_mb: info.limits.max_size_mb,
            query_timeout_ms: info.limits.query_timeout_ms,
            max_rows: info.limits.max_rows,
            ..Default::default()
        };
        if let Some(i) = info.inspect {
            resp.exists = true;
            resp.size_bytes = i.size_bytes;
            resp.schema_version = i.schema_version;
            resp.tables = i
                .tables
                .into_iter()
                .map(|(name, rows)| PluginDbTable { name, rows })
                .collect();
        }
        Ok(Response::new(resp))
    }

    async fn db_query(
        &self,
        request: Request<PluginDbQueryRequest>,
    ) -> Result<Response<PluginDbQueryResponse>, Status> {
        let req = request.into_inner();
        let rows = self
            .handle
            .db_query(&req.name, &req.sql)
            .await
            .map_err(db_status)?;
        Ok(Response::new(PluginDbQueryResponse {
            columns: rows.columns,
            rows: rows
                .rows
                .into_iter()
                .map(|r| PluginDbRow {
                    values: r.into_iter().map(map_value).collect(),
                })
                .collect(),
        }))
    }

    async fn db_reset(
        &self,
        request: Request<PluginDbResetRequest>,
    ) -> Result<Response<PluginDbResetResponse>, Status> {
        let removed = self
            .handle
            .db_reset(&request.into_inner().name)
            .await
            .map_err(db_status)?;
        Ok(Response::new(PluginDbResetResponse { removed }))
    }
}
