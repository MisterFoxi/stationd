//! gRPC transport for the live DJ service (live_v1.proto).
//!
//! Thin translator, same discipline as the other `*_grpc.rs`: every decision
//! lives in `live::LiveHub`.

use tonic::{Request, Response, Status};

use crate::config::LiveConfig;
use crate::live::{LiveError, LiveHub, LiveSession};
use crate::live_opening::Opening;

pub use crate::proto::live;

use live::live_service_server::LiveService;
use live::{
    CloseRequest, CloseResponse, Cooldown, GetStatusRequest, HashPasswordRequest, HashPasswordResponse, KickRequest,
    KickResponse, LiveStatus, OpenRequest, OpenResponse, Refused,
};

pub struct LiveGrpc {
    /// `None` = no `[live]` section.
    wired: Option<(LiveHub, LiveConfig)>,
    /// Liquidsoap is wired (a kick can reach the harbor).
    air: bool,
    preview_secret: String,
    previews: std::sync::Arc<tokio::sync::Semaphore>,
    logins: std::sync::Arc<tokio::sync::Semaphore>,
}

impl LiveGrpc {
    pub fn disabled() -> Self {
        Self {
            wired: None,
            air: false,
            preview_secret: String::new(),
            previews: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            logins: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }

    pub fn new(hub: LiveHub, cfg: LiveConfig, air: bool) -> Self {
        Self {
            wired: Some((hub, cfg)),
            air,
            ..Self::disabled()
        }
    }
    pub fn with_preview_secret(mut self, secret: String) -> Self {
        self.preview_secret = secret;
        self
    }
    fn wired(&self) -> Result<&(LiveHub, LiveConfig), Status> {
        self.wired
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("Live DJ is not configured"))
    }
    async fn identity(&self, token: &str) -> Result<String, Status> {
        self.wired()?.0.dj_identity(token).await.map_err(map_err)
    }
    async fn dj_view(&self, dj: &str) -> Result<live::DjState, Status> {
        let (hub, cfg) = self.wired()?;
        let mut view = hub.dj_state(dj).await;
        view.enabled = true;
        view.port = cfg.harbor_port as u32;
        view.mount = cfg.mount.clone();
        view.buffer_seconds = cfg.buffer;
        view.manual_takeover = cfg.manual_takeover;
        Ok(view)
    }
}

fn session(s: &LiveSession) -> live::LiveSession {
    live::LiveSession {
        dj: s.dj.clone(),
        rule_id: s.rule_id.clone(),
        occurrence: s.occurrence.clone(),
        address: s.address.clone(),
        since: s.since.0,
        access: s.access.as_str().to_string(),
    }
}

fn opening(o: &Opening) -> live::Opening {
    live::Opening {
        dj: o.dj.clone(),
        opened_at: o.opened_at.0,
        until: o.until.0,
        cut: o.cut,
    }
}

fn map_err(e: LiveError) -> Status {
    match e {
        LiveError::Authentication => Status::unauthenticated("DJ authentication required"),
        LiveError::NotReady => Status::failed_precondition(e.to_string()),
        LiveError::BadDuration(_) | LiveError::UnknownDj(_) => Status::invalid_argument(e.to_string()),
        LiveError::NoLive | LiveError::DjDisabled(_) | LiveError::NoOpening(_) | LiveError::DjFile(_) => {
            Status::failed_precondition(e.to_string())
        }
        LiveError::Db(_) => Status::internal(e.to_string()),
    }
}

#[tonic::async_trait]
impl LiveService for LiveGrpc {
    type DjPreviewStream = tokio_stream::wrappers::ReceiverStream<Result<live::PreviewChunk, Status>>;

    async fn dj_login(&self, req: Request<live::DjLoginRequest>) -> Result<Response<live::DjLoginResponse>, Status> {
        let _permit = self
            .logins
            .try_acquire()
            .map_err(|_| Status::resource_exhausted("DJ login busy"))?;
        let req = req.into_inner();
        let token = self
            .wired()?
            .0
            .dj_login(req.dj.trim(), req.password)
            .await
            .map_err(map_err)?;
        Ok(Response::new(live::DjLoginResponse { token }))
    }
    async fn dj_status(&self, req: Request<live::DjRequest>) -> Result<Response<live::DjState>, Status> {
        let dj = self.identity(&req.into_inner().token).await?;
        Ok(Response::new(self.dj_view(&dj).await?))
    }
    async fn dj_take(&self, req: Request<live::DjRequest>) -> Result<Response<live::DjState>, Status> {
        let dj = self.identity(&req.into_inner().token).await?;
        self.wired()?.0.dj_take(&dj).await.map_err(map_err)?;
        Ok(Response::new(self.dj_view(&dj).await?))
    }
    async fn dj_stop(&self, req: Request<live::DjRequest>) -> Result<Response<live::DjState>, Status> {
        let dj = self.identity(&req.into_inner().token).await?;
        self.wired()?.0.dj_stop(&dj).map_err(map_err)?;
        Ok(Response::new(self.dj_view(&dj).await?))
    }
    async fn dj_preview(&self, req: Request<live::DjRequest>) -> Result<Response<Self::DjPreviewStream>, Status> {
        use base64::Engine;
        use http_body_util::{BodyExt, Empty};
        use hyper_util::rt::TokioIo;
        use std::time::Duration;
        let token = req.into_inner().token;
        let dj = self.identity(&token).await?;
        let (hub, cfg) = self.wired()?;
        if self.preview_secret.is_empty() || !hub.dj_state(&dj).await.can_preview {
            return Err(Status::failed_precondition("DJ audio is not available"));
        }
        let permit = self
            .previews
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("Preview capacity reached"))?;
        let port = cfg.harbor_port;
        let secret = self.preview_secret.clone();
        let mut body = tokio::time::timeout(Duration::from_secs(4), async move {
            let socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
            let (mut client, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket)).await?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            let auth = base64::engine::general_purpose::STANDARD.encode(format!("stationd:{secret}"));
            let request = hyper::Request::builder()
                .uri("/__stationd_preview")
                .header("Host", format!("127.0.0.1:{port}"))
                .header("Authorization", format!("Basic {auth}"))
                .body(Empty::<bytes::Bytes>::new())?;
            let response = client.send_request(request).await?;
            if !response.status().is_success() {
                anyhow::bail!("preview refused");
            }
            Ok::<_, anyhow::Error>(response.into_body())
        })
        .await
        .map_err(|_| Status::unavailable("Preview timed out"))?
        .map_err(|_| Status::unavailable("Preview unavailable"))?;
        let hub = hub.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _permit = permit;
            let mut check = tokio::time::interval(Duration::from_secs(2));
            let lifetime = tokio::time::sleep(Duration::from_secs(1800));
            tokio::pin!(lifetime);
            loop {
                tokio::select! {
                    _ = &mut lifetime => break,
                    _ = tx.closed() => break,
                    _ = check.tick() => {
                        if !hub.preview_allowed(&dj) || hub.dj_identity(&token).await.is_err() { break; }
                    }
                    frame = body.frame() => {
                        if !hub.preview_allowed(&dj) { break; }
                        match frame {
                            Some(Ok(frame)) => if let Ok(data) = frame.into_data() {
                                if !matches!(tokio::time::timeout(Duration::from_secs(2), tx.send(Ok(live::PreviewChunk { audio: data.to_vec() }))).await, Ok(Ok(()))) { break; }
                            },
                            _ => break,
                        }
                    }
                }
            }
        });
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn get_status(&self, _req: Request<GetStatusRequest>) -> Result<Response<LiveStatus>, Status> {
        let Some((hub, cfg)) = &self.wired else {
            return Ok(Response::new(LiveStatus {
                enabled: false,
                ..Default::default()
            }));
        };
        let st = hub.status();
        let openings = hub.openings().await.map_err(map_err)?;
        let path = hub.djs_path().to_path_buf();
        let djs = tokio::task::spawn_blocking(move || crate::live::load_djs(&path))
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let (djs_count, djs_error, urgent_djs) = match djs {
            Ok(d) => {
                let urgent: Vec<String> = d.iter().filter(|dj| dj.urgent).map(|dj| dj.id.clone()).collect();
                (d.len() as u32, String::new(), urgent)
            }
            Err(e) => (0, e, Vec::new()),
        };
        // Only the DJs holding the right have a cooldown worth showing.
        let cooldowns = st
            .cooldowns
            .into_iter()
            .filter(|(dj, _)| urgent_djs.contains(dj))
            .map(|(dj, until)| Cooldown { dj, until: until.0 })
            .collect();
        let (refusal_dj, refusal_reason, refusal_at) =
            st.last_refusal.map(|(dj, why, at)| (dj, why, at.0)).unwrap_or_default();
        Ok(Response::new(LiveStatus {
            preparing: st.preparing.as_ref().map(session),
            manual_takeover: cfg.manual_takeover,
            enabled: true,
            djs_path: cfg.djs_path.to_string_lossy().into_owned(),
            harbor_port: cfg.harbor_port as u32,
            mount: cfg.mount.clone(),
            silence_timeout_s: cfg.silence_timeout,
            on_air: st.on_air.as_ref().map(session),
            last: st.last.as_ref().map(|l| session(&l.session)),
            last_reason: st
                .last
                .as_ref()
                .map(|l| l.reason.as_str().to_string())
                .unwrap_or_default(),
            last_ended_at: st.last.as_ref().map(|l| l.at.0).unwrap_or(0),
            refused: st
                .refused
                .into_iter()
                .map(|(dj, occurrence)| Refused { dj, occurrence })
                .collect(),
            last_refusal_dj: refusal_dj,
            last_refusal_reason: refusal_reason,
            last_refusal_at: refusal_at,
            djs_error,
            djs_count,
            openings: openings.iter().map(opening).collect(),
            urgent_djs,
            cooldowns,
            urgent_cooldown_s: cfg.urgent_cooldown,
        }))
    }

    async fn kick(&self, _req: Request<KickRequest>) -> Result<Response<KickResponse>, Status> {
        let Some((hub, _)) = &self.wired else {
            return Err(Status::failed_precondition("no [live] section: no harbor"));
        };
        if !self.air {
            return Err(Status::unavailable("Liquidsoap is not wired: nothing to disconnect"));
        }
        let dj = hub.kick().await.map_err(map_err)?;
        Ok(Response::new(KickResponse { dj }))
    }

    async fn open(&self, req: Request<OpenRequest>) -> Result<Response<OpenResponse>, Status> {
        let Some((hub, _)) = &self.wired else {
            return Err(Status::failed_precondition("no [live] section: no harbor"));
        };
        let req = req.into_inner();
        let o = hub.open(req.dj.trim(), req.duration.trim()).await.map_err(map_err)?;
        Ok(Response::new(OpenResponse {
            opening: Some(opening(&o)),
        }))
    }

    async fn close(&self, req: Request<CloseRequest>) -> Result<Response<CloseResponse>, Status> {
        let Some((hub, _)) = &self.wired else {
            return Err(Status::failed_precondition("no [live] section: no harbor"));
        };
        let o = hub.close(req.into_inner().dj.trim()).await.map_err(map_err)?;
        Ok(Response::new(CloseResponse {
            opening: Some(opening(&o)),
        }))
    }

    async fn hash_password(&self, req: Request<HashPasswordRequest>) -> Result<Response<HashPasswordResponse>, Status> {
        let password = req.into_inner().password;
        let hash = tokio::task::spawn_blocking(move || crate::live::hash_password(&password))
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .map_err(Status::invalid_argument)?;
        Ok(Response::new(HashPasswordResponse { hash }))
    }
}
