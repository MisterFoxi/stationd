//! HTTP boundary: secure cookies, same-origin mutations, bounded crypto jobs.
use super::{auth::Auth, Config};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};
const SESSION: &str = "__Host-stationd-session";
const CEREMONY: &str = "__Host-stationd-ceremony";

#[derive(Clone)]
pub(super) struct Web {
    auth: Arc<Mutex<Auth>>,
    jobs: Arc<Semaphore>,
    origin: String,
    session_ttl: u64,
    rate: u32,
    password_min_length: usize,
    password_max_length: usize,
}
impl Web {
    pub(super) fn new(auth: Arc<Mutex<Auth>>, config: &Config) -> Self {
        Self {
            auth,
            jobs: Arc::new(Semaphore::new(4)),
            origin: webauthn_rs::prelude::Url::parse(&config.public_url)
                .expect("validated origin")
                .origin()
                .ascii_serialization(),
            session_ttl: config.session_ttl_seconds,
            rate: config.auth_requests_per_minute,
            password_min_length: config.password_min_length,
            password_max_length: config.password_max_length,
        }
    }
    async fn work<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Auth) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let permit = self
            .jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| "busy".to_string())?;
        let auth = self.auth.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut auth = auth.lock().map_err(|_| "identity storage unavailable")?;
            f(&mut auth)
        })
        .await
        .map_err(|_| "identity storage unavailable".to_string())?
    }
}

pub(super) fn routes(web: Web) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/login", get(|| async { Html(include_str!("login.html")) }))
        .route("/enroll", get(enroll_page))
        .route(
            "/remote.js",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    include_str!("remote.js"),
                )
            }),
        )
        .route(
            "/remote.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("remote.css"),
                )
            }),
        )
        .route("/auth/enroll/start", post(enroll_start))
        .route("/auth/enroll/finish", post(enroll_finish))
        .route("/auth/login/start", post(login_start))
        .route("/auth/login/finish", post(login_finish))
        .route("/auth/password/enroll", post(password_enroll))
        .route("/auth/password/login", post(password_login))
        .route("/auth/logout", post(logout))
        .route("/api/session", get(context))
        .route("/api/stations", get(stations))
        .layer(middleware::from_fn_with_state(web.clone(), security))
        .with_state(web)
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        for item in value.to_str().ok()?.split(';') {
            if let Some((key, value)) = item.trim().split_once('=') {
                if key == name {
                    if found.is_some()
                        || value.len() != 43
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    {
                        return None;
                    }
                    found = Some(value.to_string());
                }
            }
        }
    }
    found
}
fn set_cookie(response: &mut Response, name: &str, value: &str, ttl: u64) {
    let cookie =
        format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={ttl}");
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("server-generated cookie"),
    );
}
fn answer(result: Result<Value, String>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let status = match error.as_str() {
                "busy" => StatusCode::SERVICE_UNAVAILABLE,
                "rate limited" | "challenge limit reached" => StatusCode::TOO_MANY_REQUESTS,
                "identity storage unavailable" => StatusCode::SERVICE_UNAVAILABLE,
                "permission denied" => StatusCode::FORBIDDEN,
                _ => StatusCode::UNAUTHORIZED,
            };
            (status, Json(json!({"error":"Request refused"}))).into_response()
        }
    }
}
async fn security(State(web): State<Web>, request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok());
    if (!matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && origin != Some(web.origin.as_str()))
        || origin.is_some_and(|o| o != web.origin)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.uri().path().starts_with("/auth/") {
        // Deliberately ignore forwarded IP headers: an untrusted client cannot
        // bypass the limit. A reverse proxy shares its peer-IP quota.
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|p| p.0.ip().to_string())
            .unwrap_or_else(|| "local".into());
        let max = web.rate;
        if !matches!(
            web.work(move |a| a.peer_limit(&format!("peer:{peer}"), max))
                .await,
            Ok(true)
        ) {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
    }
    let mut response = next.run(request).await;
    for (key,value) in [
        ("cache-control","no-store"),("referrer-policy","no-referrer"),("x-content-type-options","nosniff"),
        ("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"),
    ]{response.headers_mut().insert(axum::http::HeaderName::from_static(key),HeaderValue::from_static(value));}
    response
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enroll {
    token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordEnrollment {
    token: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordLogin {
    name: String,
    password: String,
}
async fn enroll_page(State(web): State<Web>) -> Html<String> {
    Html(
        include_str!("enroll.html")
            .replace(
                "{{PASSWORD_MIN_LENGTH}}",
                &web.password_min_length.to_string(),
            )
            .replace(
                "{{PASSWORD_MAX_LENGTH}}",
                &web.password_max_length.to_string(),
            ),
    )
}
async fn password_enroll(State(web): State<Web>, Json(body): Json<PasswordEnrollment>) -> Response {
    let length = body.password.chars().count();
    if body.token.len() != 43
        || length < web.password_min_length
        || length > web.password_max_length
        || body.password.len() > 1024
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("Mot de passe : entre {} et {} caractères.", web.password_min_length, web.password_max_length)})),
        )
            .into_response();
    }
    answer(
        web.work(move |a| a.password_enroll(&body.token, &body.password))
            .await,
    )
}
async fn password_login(
    State(web): State<Web>,
    headers: HeaderMap,
    Json(body): Json<PasswordLogin>,
) -> Response {
    let old = cookie(&headers, SESSION);
    match web
        .work(move |a| a.password_login(&body.name, &body.password, old.as_deref()))
        .await
    {
        Ok((value, raw)) => {
            let mut response = answer(Ok(value));
            set_cookie(&mut response, SESSION, &raw, web.session_ttl);
            response
        }
        Err(e) => answer(Err(e)),
    }
}
async fn enroll_start(State(web): State<Web>, Json(body): Json<Enroll>) -> Response {
    let result = web.work(move |a| a.registration_start(&body.token)).await;
    match result {
        Ok((value, raw)) => {
            let mut response = answer(Ok(value));
            set_cookie(&mut response, CEREMONY, &raw, 120);
            response
        }
        Err(e) => answer(Err(e)),
    }
}
async fn enroll_finish(
    State(web): State<Web>,
    headers: HeaderMap,
    Json(body): Json<RegisterPublicKeyCredential>,
) -> Response {
    let raw = cookie(&headers, CEREMONY).unwrap_or_default();
    let mut response = answer(web.work(move |a| a.registration_finish(&raw, body)).await);
    set_cookie(&mut response, CEREMONY, "", 0);
    response
}
async fn login_start(State(web): State<Web>, Json(body): Json<Login>) -> Response {
    if body.name.len() > 128 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match web.work(move |a| a.login_start(&body.name)).await {
        Ok((value, raw)) => {
            let mut response = answer(Ok(value));
            set_cookie(&mut response, CEREMONY, &raw, 120);
            response
        }
        Err(e) => answer(Err(e)),
    }
}
async fn login_finish(
    State(web): State<Web>,
    headers: HeaderMap,
    Json(body): Json<PublicKeyCredential>,
) -> Response {
    let ceremony = cookie(&headers, CEREMONY).unwrap_or_default();
    let old = cookie(&headers, SESSION);
    let mut response = match web
        .work(move |a| a.login_finish(&ceremony, body, old.as_deref()))
        .await
    {
        Ok((value, raw)) => {
            let mut response = answer(Ok(value));
            set_cookie(&mut response, SESSION, &raw, web.session_ttl);
            response
        }
        Err(e) => answer(Err(e)),
    };
    set_cookie(&mut response, CEREMONY, "", 0);
    response
}
async fn context(State(web): State<Web>, headers: HeaderMap) -> Response {
    let raw = cookie(&headers, SESSION).unwrap_or_default();
    answer(web.work(move |a| a.context(&raw)).await)
}
async fn stations(State(web): State<Web>, headers: HeaderMap) -> Response {
    let raw = cookie(&headers, SESSION).unwrap_or_default();
    answer(
        web.work(move |a| Ok(json!({"stations":a.context(&raw)?["stations"]})))
            .await,
    )
}
async fn logout(State(web): State<Web>, headers: HeaderMap) -> Response {
    let raw = cookie(&headers, SESSION).unwrap_or_default();
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let result = web.work(move |a| a.logout(&raw, &csrf)).await;
    let success = result.is_ok();
    let mut response = answer(result);
    if success {
        set_cookie(&mut response, SESSION, "", 0);
    }
    response
}
async fn home(State(web): State<Web>, headers: HeaderMap) -> Response {
    let raw = cookie(&headers, SESSION).unwrap_or_default();
    if web.work(move |a| a.context(&raw)).await.is_ok() {
        Html(include_str!("network.html")).into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}
