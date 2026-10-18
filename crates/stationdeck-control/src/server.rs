use crate::{
    store::{Credentials, Error, Store},
    wire::{self, configuration_server::Configuration},
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
    Json, Router,
};
use stationdeck_services::config::*;
use tonic::{Request, Response, Status};
fn error(e: Error) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::from_u16(e.0).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(serde_json::json!({"message":e.1})),
    )
}
fn operator(
    store: &Store,
    headers: &HeaderMap,
    permission: &str,
    target: Option<&str>,
) -> Result<Credentials, Error> {
    let token = headers
        .get("authorization")
        .and_then(|s| s.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(Error(401, "Author credentials are required."))?;
    let c = store
        .authenticate(token)
        .ok_or(Error(401, "Invalid author credentials."))?;
    if c.target.is_some()
        || !c.permissions.iter().any(|p| p == permission)
        || target.is_some_and(|t| !c.targets.iter().any(|v| v == t))
    {
        return Err(Error(
            403,
            "This principal cannot perform that configuration operation.",
        ));
    }
    Ok(c)
}
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/config/catalog", get(catalog))
        .route("/api/config/targets/:id", get(target))
        .route("/api/config/targets/:id/draft", put(draft))
        .route("/api/config/targets/:id/publish", post(publish))
        .layer(DefaultBodyLimit::max(MAX_BYTES + 8192))
        .with_state(store)
}
async fn catalog(
    State(s): State<Store>,
    h: HeaderMap,
) -> Result<Json<Catalog>, (StatusCode, Json<serde_json::Value>)> {
    let c = operator(&s, &h, "read", None).map_err(error)?;
    let targets = c
        .targets
        .iter()
        .map(|id| s.view(id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let allowed: Vec<_> = s
        .bootstrap
        .targets
        .iter()
        .filter(|t| c.targets.contains(&t.id))
        .flat_map(|t| t.plugins.iter())
        .collect();
    let schemas = s
        .bootstrap
        .schemas
        .iter()
        .filter(|schema| allowed.contains(&&schema.plugin))
        .cloned()
        .collect();
    Ok(Json(Catalog {
        authority: s.bootstrap.authority.clone(),
        schemas,
        targets,
        permissions: c.permissions,
    }))
}
async fn target(
    State(s): State<Store>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Json<TargetView>, (StatusCode, Json<serde_json::Value>)> {
    operator(&s, &h, "read", Some(&id)).map_err(error)?;
    s.view(&id).map(Json).map_err(error)
}
async fn draft(
    State(s): State<Store>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(input): Json<DraftRequest>,
) -> Result<Json<TargetView>, (StatusCode, Json<serde_json::Value>)> {
    operator(&s, &h, "edit", Some(&id)).map_err(error)?;
    s.save(&id, input).map(Json).map_err(error)
}
async fn publish(
    State(s): State<Store>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(input): Json<PublishRequest>,
) -> Result<Json<TargetView>, (StatusCode, Json<serde_json::Value>)> {
    operator(&s, &h, "publish", Some(&id)).map_err(error)?;
    s.publish(&id, input).map(Json).map_err(error)
}
#[derive(Clone)]
pub struct Grpc(pub Store);
impl Grpc {
    fn agent<T>(&self, request: &Request<T>) -> Result<String, &'static str> {
        let token = request
            .metadata()
            .get("authorization")
            .and_then(|s| s.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or("Agent credentials required.")?;
        self.0
            .authenticate(token)
            .and_then(|c| c.target)
            .ok_or("Agent identity is not authorized.")
    }
}
#[tonic::async_trait]
impl Configuration for Grpc {
    async fn pull(
        &self,
        request: Request<wire::PullRequest>,
    ) -> Result<Response<wire::PullReply>, Status> {
        let id = self.agent(&request).map_err(Status::unauthenticated)?;
        let view = self
            .0
            .view(&id)
            .map_err(|_| Status::unavailable("Configuration store is unavailable."))?;
        let bundle_json = match view.published {
            Some(b) => serde_json::to_string(&b)
                .map_err(|_| Status::internal("Cannot encode configuration."))?,
            None => String::new(),
        };
        Ok(Response::new(wire::PullReply { bundle_json }))
    }
    async fn report(
        &self,
        request: Request<wire::ReportRequest>,
    ) -> Result<Response<wire::ReportReply>, Status> {
        let id = self.agent(&request).map_err(Status::unauthenticated)?;
        let r = request.into_inner();
        self.0
            .report(&id, r.revision, r.applied, &r.code)
            .map_err(|e| {
                if e.0 == 409 {
                    Status::failed_precondition(e.1)
                } else {
                    Status::invalid_argument(e.1)
                }
            })?;
        Ok(Response::new(wire::ReportReply {}))
    }
}
