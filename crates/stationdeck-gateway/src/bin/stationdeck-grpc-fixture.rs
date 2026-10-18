//! Test fixture only. It is excluded from the runtime gateway build.
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use stationd_proto::station::{
    station_server::{Station, StationServer},
    QuitReply, QuitRequest, ShutdownReply, ShutdownRequest, StatusReply, StatusRequest,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tonic::{Request, Response, Status};

#[derive(Clone, Default)]
struct Fixture {
    mode: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
}
#[tonic::async_trait]
impl Station for Fixture {
    async fn status(&self, _: Request<StatusRequest>) -> Result<Response<StatusReply>, Status> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        match self.mode.load(Ordering::SeqCst) {
            1 => return Err(Status::unavailable("fixture-only upstream failure")),
            2 => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
            _ => {}
        }
        Ok(Response::new(StatusReply {
            station_name: "Fixture station".into(),
            uptime_seconds: 321,
            pid: 42,
            timezone: "UTC".into(),
        }))
    }
    async fn quit(&self, _: Request<QuitRequest>) -> Result<Response<QuitReply>, Status> {
        Err(Status::permission_denied(
            "Mutations are not part of this fixture.",
        ))
    }
    async fn shutdown(
        &self,
        _: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownReply>, Status> {
        Err(Status::permission_denied(
            "Mutations are not part of this fixture.",
        ))
    }
}

#[tokio::main]
async fn main() {
    let fixture = Fixture::default();
    let grpc = fixture.clone();
    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(StationServer::new(grpc))
            .serve("0.0.0.0:50051".parse().unwrap())
            .await
            .unwrap();
    });
    let router = Router::new()
        .route(
            "/metrics",
            get(|State(state): State<Fixture>| async move {
                Json(serde_json::json!({"requests": state.requests.load(Ordering::SeqCst)}))
            }),
        )
        .route(
            "/ready",
            post(|State(state): State<Fixture>| async move {
                state.mode.store(0, Ordering::SeqCst);
                "ok"
            }),
        )
        .route(
            "/fail",
            post(|State(state): State<Fixture>| async move {
                state.mode.store(1, Ordering::SeqCst);
                "ok"
            }),
        )
        .route(
            "/delay",
            post(|State(state): State<Fixture>| async move {
                state.mode.store(2, Ordering::SeqCst);
                "ok"
            }),
        )
        .with_state(fixture);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8081").await.unwrap();
    axum::serve(listener, router).await.unwrap();
    task.abort();
}
