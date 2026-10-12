//! DJ credentials remain in stationd; the gateway retains only opaque tickets,
//! bound to the existing Webmin session and station. Every route reauthorizes.
use super::{
    auth::now,
    live::transport,
    web::{answer, cookie, Web, SESSION},
    Config, Station,
};
use crate::proto::live::{self, live_service_client::LiveServiceClient};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct Ticket {
    token: String,
    expires: i64,
}
#[derive(Clone)]
pub(super) struct Djs {
    stations: Arc<HashMap<String, Station>>,
    tickets: Arc<Mutex<HashMap<(String, String), Ticket>>>,
    previews: Arc<Semaphore>,
}
impl Djs {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            stations: Arc::new(config.stations.iter().map(|s| (s.id.clone(), s.clone())).collect()),
            tickets: Default::default(),
            previews: Arc::new(Semaphore::new(16)),
        }
    }
    fn client(&self, id: &str) -> Result<LiveServiceClient<tonic::transport::Channel>, StatusCode> {
        let station = self.stations.get(id).ok_or(StatusCode::FORBIDDEN)?;
        Ok(LiveServiceClient::new(
            transport(station).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
        ))
    }
    fn ticket(&self, raw: &str, id: &str) -> Result<String, StatusCode> {
        let mut tickets = self.tickets.lock().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        tickets.retain(|_, ticket| ticket.expires > now());
        tickets
            .get(&(raw.into(), id.into()))
            .map(|t| t.token.clone())
            .ok_or(StatusCode::UNAUTHORIZED)
    }
    fn view(&self, id: &str, state: live::DjState) -> Value {
        let source_host = self
            .stations
            .get(id)
            .map(|s| s.dj_source_host.as_str())
            .unwrap_or_default();
        json!({"dj":state.dj,"enabled":state.enabled,"host":source_host,"port":state.port,
            "mount":state.mount,"state":state.state,"can_take":state.can_take,
            "can_preview":state.can_preview,"level_db":state.level_db,"peak_db":state.peak_db,
            "clipping":state.clipping,"other_dj":state.other_dj,"buffer_seconds":state.buffer_seconds,
            "manual_takeover":state.manual_takeover})
    }
}
pub(super) fn routes() -> Router<Web> {
    Router::new()
        .route(
            "/dj.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
                    include_str!("dj.js"),
                )
            }),
        )
        .route("/station/:id/dj", get(page))
        .route("/api/stations/:id/dj/login", post(login))
        .route("/api/stations/:id/dj", get(status))
        .route("/api/stations/:id/dj/take", post(take))
        .route("/api/stations/:id/dj/stop", post(stop))
        .route("/api/stations/:id/dj/preview", get(preview))
}
async fn authorize(web: &Web, id: &str, headers: &HeaderMap, mutation: bool) -> Result<String, Response> {
    let raw = cookie(headers, SESSION).unwrap_or_default();
    let session = raw.clone();
    let station = id.to_string();
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|h| h.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    web.work(move |a| {
        a.authorize(
            &session,
            &station,
            if mutation { "dj.control" } else { "station.read" },
            csrf.as_deref(),
        )
    })
    .await
    .map_err(|e| answer(Err(e)))?;
    Ok(raw)
}
async fn page(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if let Err(e) = authorize(&web, &id, &headers, false).await {
        return e;
    }
    Html(super::i18n::page(include_str!("dj.html"), &headers).replace("{{STATION_ID}}", &id)).into_response()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    dj: String,
    password: String,
}
fn rpc_error(error: tonic::Status) -> Response {
    let (status, message) = match error.code() {
        tonic::Code::Unauthenticated => (StatusCode::UNAUTHORIZED, "DJ authentication required"),
        tonic::Code::FailedPrecondition => (
            StatusCode::CONFLICT,
            "DJ stream is not ready or the connection window is closed",
        ),
        tonic::Code::ResourceExhausted => (StatusCode::TOO_MANY_REQUESTS, "Please retry later"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "Station unavailable"),
    };
    (status, Json(json!({"error":message}))).into_response()
}
async fn login(
    State(web): State<Web>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Login>,
) -> Response {
    let raw = match authorize(&web, &id, &headers, true).await {
        Ok(raw) => raw,
        Err(e) => return e,
    };
    if body.dj.len() > 128 || body.password.len() > 256 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let key = format!("dj-login:{raw}:{id}");
    if !matches!(web.work(move |a| a.peer_limit(&key, 10)).await, Ok(true)) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let mut client = match web.dj.client(&id) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        client.dj_login(live::DjLoginRequest {
            dj: body.dj,
            password: body.password,
        }),
    )
    .await;
    let token = match result {
        Ok(Ok(r)) => r.into_inner().token,
        Ok(Err(e)) => return rpc_error(e),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    {
        let mut tickets = match web.dj.tickets.lock() {
            Ok(t) => t,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        tickets.retain(|_, t| t.expires > now());
        if tickets.len() >= 256 && !tickets.contains_key(&(raw.clone(), id.clone())) {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        tickets.insert(
            (raw.clone(), id.clone()),
            Ticket {
                token,
                expires: now() + 43200,
            },
        );
    }
    read_state(web, id, raw, "status").await
}
async fn read_state(web: Web, id: String, raw: String, action: &str) -> Response {
    let token = match web.dj.ticket(&raw, &id) {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    if action != "status" {
        let session = raw.clone();
        let station = id.clone();
        let audit_action = format!("dj.{action}");
        if let Err(e) = web
            .work(move |a| a.dj_audit(&session, &station, &audit_action, "requested"))
            .await
        {
            return answer(Err(e));
        }
    }
    let mut client = match web.dj.client(&id) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let result = tokio::time::timeout(Duration::from_secs(4), async {
        let req = live::DjRequest { token };
        match action {
            "take" => client.dj_take(req).await,
            "stop" => client.dj_stop(req).await,
            _ => client.dj_status(req).await,
        }
    })
    .await;
    match result {
        Ok(Ok(r)) => Json(web.dj.view(&id, r.into_inner())).into_response(),
        Ok(Err(e)) => rpc_error(e),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn status(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let raw = match authorize(&web, &id, &headers, false).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    read_state(web, id, raw, "status").await
}
async fn take(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let raw = match authorize(&web, &id, &headers, true).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    read_state(web, id, raw, "take").await
}
async fn stop(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let raw = match authorize(&web, &id, &headers, true).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    read_state(web, id, raw, "stop").await
}
async fn preview(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let raw = match authorize(&web, &id, &headers, false).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    let token = match web.dj.ticket(&raw, &id) {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let permit = match web.dj.previews.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return StatusCode::TOO_MANY_REQUESTS.into_response(),
    };
    let mut client = match web.dj.client(&id) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let mut stream =
        match tokio::time::timeout(Duration::from_secs(4), client.dj_preview(live::DjRequest { token })).await {
            Ok(Ok(r)) => r.into_inner(),
            Ok(Err(e)) => return rpc_error(e),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let _permit = permit;
        let mut check = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = tx.closed() => break,
                _ = check.tick() => {
                    let session=raw.clone(); let station=id.clone();
                    if web.work(move |a| a.authorize(&session, &station, "station.read", None)).await.is_err() { break; }
                }
                chunk = stream.message() => match chunk {
                    Ok(Some(chunk)) => if !matches!(tokio::time::timeout(Duration::from_secs(2), tx.send(Ok(bytes::Bytes::from(chunk.audio)))).await, Ok(Ok(()))) { break; },
                    _ => break,
                }
            }
        }
    });
    (
        [
            (header::CONTENT_TYPE, "audio/mpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
    )
        .into_response()
}
