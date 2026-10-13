//! DJ credentials remain in stationd; the gateway retains only opaque tickets,
//! bound to a dedicated DJ browser session and station. No Webmin account is required.
use super::{
    auth::{now, token as random_token, secret_eq},
    live::transport,
    web::{answer, cookie, set_cookie, Web},
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

const DJ_SESSION: &str = "__Host-stationd-dj";
#[derive(Clone)]
struct BrowserSession { csrf: String, expires: i64 }
#[derive(Clone)]
struct Ticket {
    dj: String,
    token: String,
    expires: i64,
}
#[derive(Clone)]
pub(super) struct Djs {
    stations: Arc<HashMap<String, Station>>,
    tickets: Arc<Mutex<HashMap<(String, String), Ticket>>>,
    previews: Arc<Semaphore>,
    sessions: Arc<Mutex<HashMap<String, BrowserSession>>>,
}
impl Djs {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            stations: Arc::new(config.stations.iter().map(|s| (s.id.clone(), s.clone())).collect()),
            tickets: Default::default(),
            sessions: Default::default(),
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
        let station = self.stations.get(id);
        let source_url = station.map(|s| s.dj_source_url.as_str()).unwrap_or_default();
        let external = webauthn_rs::prelude::Url::parse(source_url).ok();
        let source_host = external.as_ref().and_then(|url| url.host_str())
            .unwrap_or_else(|| station.map(|s| s.dj_source_host.as_str()).unwrap_or_default());
        let port = external.as_ref().and_then(|url| url.port_or_known_default()).map(u32::from).unwrap_or(state.port);
        let mount = external.as_ref().map(|url| url.path()).unwrap_or(&state.mount);
        json!({"dj":state.dj,"enabled":state.enabled,"source_url":source_url,"host":source_host,"port":port,
            "mount":mount,"state":state.state,"can_take":state.can_take,
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
        .route("/api/stations/:id/dj/context", get(context))
        .route("/api/stations/:id/dj/logout", post(logout))
        .route("/api/stations/:id/dj/login", post(login))
        .route("/api/stations/:id/dj", get(status))
        .route("/api/stations/:id/dj/take", post(take))
        .route("/api/stations/:id/dj/stop", post(stop))
        .route("/api/stations/:id/dj/preview", get(preview))
}
async fn authorize(web: &Web, id: &str, headers: &HeaderMap, mutation: bool) -> Result<String, Response> {
    if !web.dj.stations.contains_key(id) { return Err(StatusCode::NOT_FOUND.into_response()); }
    let raw = cookie(headers, DJ_SESSION).unwrap_or_default();
    let mut sessions = web.dj.sessions.lock().map_err(|_| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
    sessions.retain(|_, s| s.expires > now());
    let session = sessions.get(&raw).ok_or_else(|| StatusCode::UNAUTHORIZED.into_response())?;
    if mutation && !headers.get("x-csrf-token").and_then(|h| h.to_str().ok())
        .is_some_and(|c| secret_eq(c, &session.csrf)) {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    Ok(raw)
}
async fn page(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if !web.dj.stations.contains_key(&id) { return StatusCode::NOT_FOUND.into_response(); }
    Html(super::i18n::page(include_str!("dj.html"), &headers).replace("{{STATION_ID}}", &id)).into_response()
}
async fn context(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let station = match web.dj.stations.get(&id) { Some(s) => s, None => return StatusCode::NOT_FOUND.into_response() };
    let mut sessions = match web.dj.sessions.lock() { Ok(s) => s, Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response() };
    sessions.retain(|_, s| s.expires > now());
    let previous = cookie(&headers, DJ_SESSION).filter(|raw| sessions.contains_key(raw));
    let raw = if let Some(raw) = previous { raw } else {
        if sessions.len() >= 512 { return StatusCode::TOO_MANY_REQUESTS.into_response(); }
        let raw = random_token();
        sessions.insert(raw.clone(), BrowserSession { csrf: random_token(), expires: now() + 43200 });
        raw
    };
    let mut response = Json(json!({"csrf_token":sessions[&raw].csrf,"label":station.label})).into_response();
    set_cookie(&mut response, DJ_SESSION, &raw, (sessions[&raw].expires - now()).max(0) as u64);
    response
}
async fn logout(State(web): State<Web>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let raw = match authorize(&web, &id, &headers, true).await { Ok(r) => r, Err(e) => return e };
    if let Ok(mut sessions) = web.dj.sessions.lock() { sessions.remove(&raw); }
    if let Ok(mut tickets) = web.dj.tickets.lock() { tickets.retain(|(s, _), _| s != &raw); }
    let mut response = Json(json!({"authenticated":false})).into_response();
    set_cookie(&mut response, DJ_SESSION, "", 0);
    response
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
    let dj = body.dj.clone();
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
    let rotated = random_token();
    {
        let mut sessions = match web.dj.sessions.lock() { Ok(s) => s, Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response() };
        let previous = match sessions.remove(&raw) { Some(s) => s, None => return StatusCode::UNAUTHORIZED.into_response() };
        sessions.insert(rotated.clone(), BrowserSession { csrf: previous.csrf, expires: now() + 43200 });
    }
    {
        let mut tickets = match web.dj.tickets.lock() {
            Ok(t) => t,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        tickets.retain(|(s, _), t| t.expires > now() && s != &raw);
        if tickets.len() >= 256 && !tickets.contains_key(&(rotated.clone(), id.clone())) {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        tickets.insert(
            (rotated.clone(), id.clone()),
            Ticket {
                dj,
                token,
                expires: now() + 43200,
            },
        );
    }
    let mut response = read_state(web, id, rotated.clone(), "status").await;
    set_cookie(&mut response, DJ_SESSION, &rotated, 43200);
    response
}
async fn read_state(web: Web, id: String, raw: String, action: &str) -> Response {
    let token = match web.dj.ticket(&raw, &id) {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    if action != "status" {
        let actor = web.dj.tickets.lock().ok().and_then(|t| t.get(&(raw.clone(), id.clone())).map(|t| format!("dj:{}", t.dj)));
        let Some(actor) = actor else { return StatusCode::UNAUTHORIZED.into_response(); };
        let station = id.clone();
        let action = format!("dj.{action}");
        if let Err(e) = web.work(move |a| a.console_audit(&actor, &action, "requested", &station)).await {
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
                    if web.dj.ticket(&raw, &id).is_err() { break; }
                    if !web.dj.sessions.lock().ok().is_some_and(|s| s.get(&raw).is_some_and(|s| s.expires > now())) { break; }
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
