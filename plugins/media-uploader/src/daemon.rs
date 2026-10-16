//! Native media upload UI. All paths and write policy come from the daemon;
//! HTTP clients supply only a filename and a streamed body.
use super::{Host, Plugin};
use crate::media_upload::UploadTarget;
use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use http_body_util::BodyExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::Arc, thread, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{
    io::AsyncWriteExt,
    sync::{oneshot, Semaphore},
};

const NAME: &str = "media-uploader";
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    bind: SocketAddr,
    token: String,
    inbox: String,
    tag_field: String,
    max_file_mb: usize,
    max_uploads: usize,
    conversion: crate::media_upload::ConversionConfig,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8092".parse().unwrap(),
            token: String::new(),
            inbox: "Uploaded".into(),
            tag_field: "Type".into(),
            max_file_mb: 256,
            max_uploads: 2,
            conversion: Default::default(),
        }
    }
}
impl Config {
    fn parse(table: &toml::Table) -> Result<Self, String> {
        let config: Self = toml::Value::Table(table.clone()).try_into().map_err(|_| {
            "media-uploader: invalid configuration; check field names and value types".to_string()
        })?;
        if config.bind.port() == 0
            || !(1..=2048).contains(&config.max_file_mb)
            || !(1..=16).contains(&config.max_uploads)
        {
            return Err(
                "media-uploader: invalid port, max_file_mb (1..2048) or max_uploads (1..16)".into(),
            );
        }
        if config.inbox.is_empty()
            || config.inbox.len() > 64
            || config.inbox.starts_with('.')
            || !config
                .inbox
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err("media-uploader: inbox must be one directory name (letters, digits, hyphen, underscore)".into());
        }
        if config.tag_field.is_empty()
            || config.tag_field.len() > 64
            || !config
                .tag_field
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || config
                .tag_field
                .eq_ignore_ascii_case(crate::media_identity::UUID_TAG)
        {
            return Err("media-uploader: invalid tag_field".into());
        }
        if !(32..=512).contains(&config.token.len())
            || !config.token.bytes().all(|c| c.is_ascii_graphic())
        {
            return Err("media-uploader: token must contain 32..512 printable ASCII characters without spaces".into());
        }
        config.conversion.validate()?;
        Ok(config)
    }
}
pub(super) fn validate_config(table: &toml::Table) -> Result<(), String> {
    Config::parse(table).map(|_| ())
}

#[derive(Clone)]
struct Web {
    config: Config,
    target: Arc<UploadTarget>,
    token_hash: [u8; 32],
    slots: Arc<Semaphore>,
}
fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}
fn routes(web: Web) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/upload.html")) }),
        )
        .route(
            "/upload.js",
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        "application/javascript; charset=utf-8",
                    )],
                    include_str!("../web/upload.js"),
                )
            }),
        )
        .route(
            "/upload.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/upload.css"),
                )
            }),
        )
        .route("/api/upload/:filename", post(upload))
        .layer(middleware::from_fn_with_state(web.clone(), security))
        .with_state(web)
}
async fn security(State(web): State<Web>, request: Request, next: Next) -> Response {
    let mut response = if request.uri().path().starts_with("/api/") && !authorized(&web, &request) {
        error(
            StatusCode::UNAUTHORIZED,
            "Clé d’upload absente ou incorrecte.",
        )
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'".parse().unwrap());
    response
}
fn authorized(web: &Web, request: &Request) -> bool {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    token.is_some_and(|value| {
        let digest: [u8; 32] = Sha256::digest(value.as_bytes()).into();
        bool::from(digest.ct_eq(&web.token_hash))
    })
}
async fn upload(
    State(web): State<Web>,
    Path(filename): Path<String>,
    request: Request,
) -> Response {
    if let Err(message) = crate::media_upload::validate_filename(&filename) {
        return error(StatusCode::BAD_REQUEST, &message);
    }
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("application/octet-stream")
    {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Le corps doit être un fichier audio brut.",
        );
    }
    let max_bytes = web.config.max_file_mb as u64 * 1024 * 1024;
    if request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > max_bytes)
    {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "Fichier trop volumineux.");
    }
    let Ok(_permit) = web.slots.clone().try_acquire_owned() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "Tous les transferts sont occupés. Réessayez plus tard.",
        );
    };
    let library = match web.target.library() {
        Ok(library) => library,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "La bibliothèque n’est pas encore prête.",
            )
        }
    };
    let target = web.target.clone();
    let mut staged = match tokio::task::spawn_blocking(move || target.stage(&filename)).await {
        Ok(Ok(staged)) => staged,
        failure => {
            let why = match failure {
                Ok(Err(why)) => why,
                Err(why) => why.to_string(),
                Ok(Ok(_)) => unreachable!(),
            };
            tracing::warn!(plugin = NAME, error = %why, "cannot create upload staging file");
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Zone tampon indisponible. Vérifiez les droits du StationD.",
            );
        }
    };
    let received = tokio::time::timeout(Duration::from_secs(900), async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged.file)
            .await
            .map_err(|_| {
                (
                    StatusCode::INSUFFICIENT_STORAGE,
                    "Écriture impossible dans la zone tampon.",
                )
            })?;
        let mut body = request.into_body();
        let mut size = 0u64;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(30), body.frame())
                .await
                .map_err(|_| {
                    (
                        StatusCode::REQUEST_TIMEOUT,
                        "Transfert interrompu : délai dépassé.",
                    )
                })?;
            let Some(frame) = frame else { break };
            let frame = frame.map_err(|_| (StatusCode::BAD_REQUEST, "Transfert interrompu."))?;
            if let Ok(data) = frame.into_data() {
                size += data.len() as u64;
                if size > max_bytes {
                    return Err((StatusCode::PAYLOAD_TOO_LARGE, "Fichier trop volumineux."));
                }
                file.write_all(&data).await.map_err(|_| {
                    (
                        StatusCode::INSUFFICIENT_STORAGE,
                        "Écriture impossible dans la zone tampon.",
                    )
                })?;
            }
        }
        if size == 0 {
            return Err((StatusCode::BAD_REQUEST, "Fichier vide."));
        }
        file.sync_all().await.map_err(|_| {
            (
                StatusCode::INSUFFICIENT_STORAGE,
                "Impossible de terminer l’écriture du fichier.",
            )
        })?;
        Ok(())
    })
    .await;
    match received {
        Ok(Ok(())) => {}
        Ok(Err((status, message))) => return error(status, message),
        Err(_) => {
            return error(
                StatusCode::REQUEST_TIMEOUT,
                "Durée maximale du transfert dépassée.",
            )
        }
    }
    if let Err(why) = staged.prepare(&web.config.conversion).await {
        let staging_id = staged.staging_id().to_string();
        tracing::warn!(plugin = NAME, %staging_id, error = %why, "upload conversion failed; source kept in staging");
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({
            "error": "Conversion impossible. L’original est conservé en zone tampon ; consultez le journal StationD.",
            "staging_id": staging_id,
        }))).into_response();
    }
    match library
        .import_upload(staged, web.config.inbox, web.config.tag_field)
        .await
    {
        Ok(result) => (StatusCode::CREATED, Json(result)).into_response(),
        Err(why) => {
            tracing::warn!(plugin = NAME, error = %why, "upload rejected before publication");
            error(StatusCode::UNPROCESSABLE_ENTITY, "Import refusé : fichier audio invalide, tagging impossible ou droits insuffisants. Consultez le journal StationD.")
        }
    }
}

struct Server {
    shutdown: oneshot::Sender<()>,
    thread: thread::JoinHandle<()>,
}
pub(super) struct MediaUploader {
    config: Config,
    server: Option<Server>,
}
impl MediaUploader {
    pub(super) fn from_config(table: &toml::Table) -> Result<Self, String> {
        Ok(Self {
            config: Config::parse(table)?,
            server: None,
        })
    }
    fn start(&mut self, host: Host) -> Result<(), String> {
        if self.server.is_some() {
            return Err("media-uploader is already running".into());
        }
        let target = host
            .media_upload
            .ok_or("media-uploader: no media writer configured")?;
        target.check_writable()?;
        let web = Web {
            config: self.config.clone(),
            target,
            token_hash: Sha256::digest(self.config.token.as_bytes()).into(),
            slots: Arc::new(Semaphore::new(self.config.max_uploads)),
        };
        let listener = std::net::TcpListener::bind(self.config.bind).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?
        };
        let app = routes(web);
        let (shutdown, shutdown_rx) = oneshot::channel();
        let (stopping, stopped) = oneshot::channel();
        let thread = thread::Builder::new().name(NAME.into()).spawn(move || {
            runtime.block_on(async move {
                let serving = axum::serve(listener, app).with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                    let _ = stopping.send(());
                });
                tokio::select! {
                    result = serving => { if let Err(error) = result { tracing::error!(plugin = NAME, %error, "uploader server failed"); } },
                    _ = async { let _ = stopped.await; tokio::time::sleep(Duration::from_secs(2)).await; } => {},
                }
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        }).map_err(|e| e.to_string())?;
        self.server = Some(Server { shutdown, thread });
        Ok(())
    }
    fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            let _ = server.shutdown.send(());
            let _ = server.thread.join();
        }
    }
}
impl Plugin for MediaUploader {
    fn name(&self) -> &str {
        NAME
    }
    fn filters_pool(&self) -> bool {
        false
    }
    fn validate_config(&mut self, config: &toml::Table, _host: &Host) -> Result<(), String> {
        validate_config(config)
    }
    fn on_load(&mut self, host: Host) -> Result<(), String> {
        self.start(host)
    }
    fn on_unload(&mut self) {
        self.stop();
    }
}
impl Drop for MediaUploader {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
