//! Test data plane: independent MP3 frames, stable Icecast source, gRPC control.
use crate::{
    read_secret,
    rpc::{stream_router_server::StreamRouter, *},
};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot, Mutex, RwLock},
};
use tonic::{Request, Response, Status};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouterConfig {
    pub icecast_url: String,
    pub output_mount: String,
    pub output_password_file: String,
    pub default_input: String,
    pub inputs: Vec<InputConfig>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    pub id: String,
    pub mount: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    input: String,
}

impl RouterConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Self =
            toml::from_str(&text).map_err(|_| anyhow::anyhow!("invalid router TOML"))?;
        let url = reqwest::Url::parse(&cfg.icecast_url)?;
        if url.scheme() != "http"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("router Icecast URL must be an HTTP origin");
        }
        let valid_mount = |mount: &str| {
            mount.starts_with('/')
                && mount.len() > 1
                && !mount.starts_with("/admin")
                && !mount.contains("..")
                && !mount
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace() || "?#\\".contains(c))
        };
        let mut ids = std::collections::HashSet::new();
        let mut mounts = std::collections::HashSet::new();
        if !valid_mount(&cfg.output_mount) || cfg.inputs.is_empty() {
            bail!("invalid router mounts");
        }
        for input in &cfg.inputs {
            if input.id.is_empty()
                || !input
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                || !ids.insert(&input.id)
                || !valid_mount(&input.mount)
                || input.mount == cfg.output_mount
                || !mounts.insert(&input.mount)
            {
                bail!("invalid, duplicate or recursive router input");
            }
        }
        if !ids.contains(&cfg.default_input) {
            bail!("default input not declared");
        }
        if cfg.output_password_file.is_empty()
            || !cfg
                .output_password_file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            bail!("invalid output secret reference");
        }
        Ok(cfg)
    }
}

#[derive(Default)]
struct FrameBuffer {
    bytes: Vec<u8>,
    locked: bool,
}
impl FrameBuffer {
    fn take(&mut self) -> Result<Option<Vec<u8>>> {
        while self.bytes.len() >= 4 {
            let b = &self.bytes;
            // Bench contract: MPEG-1 Layer III, 64 kbit/s, 44.1 kHz, mono.
            let sync = b[0] == 0xff && b[1] & 0xe0 == 0xe0;
            if !sync {
                if self.locked {
                    bail!("MP3 frame boundary lost");
                }
                self.bytes.remove(0);
                continue;
            }
            if b[1] & 0x1e != 0x1a || b[2] >> 4 != 5 || (b[2] >> 2) & 3 != 0 || b[3] >> 6 != 3 {
                if self.locked {
                    bail!("unsupported MP3 format");
                }
                self.bytes.remove(0);
                continue;
            }
            let length = 144000 * 64 / 44100 + usize::from((b[2] >> 1) & 1);
            if b.len() < length {
                return Ok(None);
            }
            let side = if b[1] & 1 == 0 { 6 } else { 4 };
            if b[side] != 0 || b[side + 1] & 0x80 != 0 {
                bail!("MP3 bit reservoir must be disabled for this switching bench");
            }
            self.locked = true;
            return Ok(Some(self.bytes.drain(..length).collect()));
        }
        Ok(None)
    }
}
struct Input {
    response: reqwest::Response,
    frames: FrameBuffer,
}
impl Input {
    async fn frame(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(frame) = self.frames.take()? {
                return Ok(frame);
            }
            let chunk = self.response.chunk().await?.context("source ended")?;
            if self.frames.bytes.len() + chunk.len() > 128 * 1024 {
                bail!("source buffer exceeded");
            }
            self.frames.bytes.extend_from_slice(&chunk);
        }
    }
}
pub struct Prepared {
    id: String,
    input: Input,
    first: Vec<u8>,
    reply: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct Router {
    config: Arc<RouterConfig>,
    client: reqwest::Client,
    state: Arc<RwLock<RouteStatus>>,
    changes: mpsc::Sender<Prepared>,
    serial: Arc<Mutex<()>>,
}
impl Router {
    pub fn new(
        config: RouterConfig,
        state_file: &Path,
    ) -> Result<(Self, mpsc::Receiver<Prepared>)> {
        let selected = match std::fs::read_to_string(state_file) {
            Ok(text) => {
                serde_json::from_str::<Selection>(&text)
                    .context("invalid saved router selection")?
                    .input
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => config.default_input.clone(),
            Err(e) => return Err(e.into()),
        };
        if !config.inputs.iter().any(|i| i.id == selected) {
            bail!("saved input no longer declared");
        }
        let (changes, receiver) = mpsc::channel(4);
        let status = RouteStatus {
            output_mount: config.output_mount.clone(),
            input: selected,
            connected: false,
            inputs: config.inputs.iter().map(|i| i.id.clone()).collect(),
            problem: "starting".into(),
        };
        Ok((
            Self {
                config: Arc::new(config),
                client: reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(3))
                    .read_timeout(Duration::from_secs(5))
                    .redirect(reqwest::redirect::Policy::none())
                    .no_proxy()
                    .build()?,
                state: Arc::new(RwLock::new(status)),
                changes,
                serial: Arc::new(Mutex::new(())),
            },
            receiver,
        ))
    }
    async fn prepare(&self, id: &str) -> Result<(Input, Vec<u8>)> {
        let cfg = self
            .config
            .inputs
            .iter()
            .find(|i| i.id == id)
            .context("unknown input")?;
        let url = reqwest::Url::parse(&self.config.icecast_url)?.join(&cfg.mount)?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("input unreachable"))?;
        if !response.status().is_success() {
            bail!("input not streaming");
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("audio/mpeg"))
        {
            bail!("input is not MP3");
        }
        let mut input = Input {
            response,
            frames: FrameBuffer::default(),
        };
        let mut first = Vec::new();
        for _ in 0..3 {
            first.extend(input.frame().await?);
        }
        Ok((input, first))
    }
    async fn output(&self, password: &str, first: &[u8]) -> Result<TcpStream> {
        let url = reqwest::Url::parse(&self.config.icecast_url)?;
        let host = url.host_str().context("missing host")?;
        let port = url.port_or_known_default().context("missing port")?;
        let mut socket = TcpStream::connect((host, port)).await?;
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("source:{password}"));
        let header = format!("PUT {} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: Basic {auth}\r\nContent-Type: audio/mpeg\r\nIce-Name: StationD stable switch test\r\nIce-Public: 0\r\nExpect: 100-continue\r\n\r\n", self.config.output_mount);
        socket.write_all(header.as_bytes()).await?;

        let mut reply = Vec::new();
        loop {
            let byte = socket.read_u8().await?;
            reply.push(byte);
            if reply.ends_with(b"\r\n\r\n") {
                break;
            }
            if reply.len() > 16384 {
                bail!("invalid source response");
            }
        }
        if !reply.starts_with(b"HTTP/1.1 100")
            && !reply.starts_with(b"HTTP/1.0 200")
            && !reply.starts_with(b"HTTP/1.1 200")
        {
            bail!("Icecast refused router source");
        }
        socket.write_all(first).await?;
        Ok(socket)
    }
    pub async fn run(
        self,
        receiver: &mut mpsc::Receiver<Prepared>,
        secrets: &Path,
        state_file: PathBuf,
    ) -> Result<()> {
        let password = read_secret(&secrets.join(&self.config.output_password_file))?;
        loop {
            let pending = receiver
                .try_recv()
                .ok()
                .filter(|change| !change.reply.is_closed());
            let result = self
                .forward(receiver, &password, &state_file, pending)
                .await;
            let mut state = self.state.write().await;
            state.connected = false;
            state.problem = match result {
                Ok(()) => "reconnecting".into(),
                Err(error) => format!("stream interrupted: {error}"),
            };
            // Source URLs cannot contain credentials; these errors never contain output headers.
            eprintln!("{}", state.problem);
            drop(state);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    async fn forward(
        &self,
        receiver: &mut mpsc::Receiver<Prepared>,
        password: &str,
        state_file: &Path,
        pending: Option<Prepared>,
    ) -> Result<()> {
        let (mut current, first, acknowledgement) = if let Some(change) = pending {
            if let Err(error) = save_selection(state_file, &change.id) {
                let _ = change
                    .reply
                    .send(Err("cannot persist selection; switch refused".into()));
                return Err(error);
            }
            self.state.write().await.input = change.id;
            (change.input, change.first, Some(change.reply))
        } else {
            let id = self.state.read().await.input.clone();
            let (input, first) =
                tokio::time::timeout(Duration::from_secs(5), self.prepare(&id)).await??;
            (input, first, None)
        };

        let mut output =
            tokio::time::timeout(Duration::from_secs(5), self.output(password, &first)).await??;

        {
            let mut state = self.state.write().await;
            state.connected = true;
            state.problem.clear();
        }
        if let Some(reply) = acknowledgement {
            let _ = reply.send(Ok(()));
        }
        loop {
            tokio::select! {
                change = receiver.recv() => {
                    let Some(change) = change else { bail!("control channel closed"); };
                    if change.reply.is_closed() { continue; }
                    if save_selection(state_file, &change.id).is_err() {
                        let _ = change.reply.send(Err("cannot persist selection; switch refused".into()));
                        continue;
                    }
                    self.state.write().await.input = change.id.clone();
                    // Emit a complete independent MP3 frame before acknowledging.
                    tokio::time::timeout(Duration::from_secs(3), output.write_all(&change.first)).await??;
                    current = change.input;

                    let _ = change.reply.send(Ok(()));
                }
                frame = current.frame() => {
                    tokio::time::timeout(Duration::from_secs(3), output.write_all(&frame?)).await??;
                }
            }
        }
    }
}
fn save_selection(path: &Path, id: &str) -> Result<()> {
    use std::io::Write;
    let temporary = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&temporary)?;
    file.write_all(serde_json::to_string(&Selection { input: id.into() })?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}
#[tonic::async_trait]
impl StreamRouter for Router {
    async fn get_route(&self, _: Request<Empty>) -> Result<Response<RouteStatus>, Status> {
        Ok(Response::new(self.state.read().await.clone()))
    }
    async fn switch_route(
        &self,
        req: Request<SwitchRequest>,
    ) -> Result<Response<RouteStatus>, Status> {
        let _guard = self.serial.lock().await;
        let id = req.into_inner().input;
        if !self.config.inputs.iter().any(|i| i.id == id) {
            return Err(Status::invalid_argument("unknown input"));
        }
        let state = self.state.read().await.clone();
        if state.connected && state.input == id {
            return Ok(Response::new(state));
        }
        let (input, first) = tokio::time::timeout(Duration::from_secs(5), self.prepare(&id))
            .await
            .map_err(|_| Status::unavailable("target input timeout"))?
            .map_err(|_| {
                Status::failed_precondition(
                    "target unavailable or incompatible; current input retained",
                )
            })?;
        let (reply, result) = oneshot::channel();
        self.changes
            .send(Prepared {
                id,
                input,
                first,
                reply,
            })
            .await
            .map_err(|_| Status::unavailable("router worker stopped"))?;
        tokio::time::timeout(Duration::from_secs(8), result)
            .await
            .map_err(|_| Status::deadline_exceeded("switch result unknown; inspect route"))?
            .map_err(|_| Status::unavailable("router worker interrupted; inspect route"))?
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(self.state.read().await.clone()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restores_runtime_selection_and_rejects_invalid_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.json");
        let cfg: RouterConfig =
            toml::from_str(include_str!("../../../config/icecast/router.toml")).unwrap();
        save_selection(&path, "b").unwrap();
        let (router, _) = Router::new(cfg.clone(), &path).unwrap();
        assert_eq!(router.state.read().await.input, "b");
        std::fs::write(&path, r#"{"input":"removed-input"}"#).unwrap();
        assert!(Router::new(cfg.clone(), &path).is_err());
        std::fs::write(&path, "{}").unwrap();
        assert!(Router::new(cfg, &path).is_err());
    }

    #[test]
    fn splits_complete_frames_and_keeps_partial_frame() {
        let mut frame = vec![0; 208];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x50, 0xc0]);
        let mut buffer = FrameBuffer::default();
        buffer.bytes.extend(b"ID3 tag");
        buffer.bytes.extend(&frame[..100]);
        assert!(buffer.take().unwrap().is_none());
        buffer.bytes.extend(&frame[100..]);
        assert_eq!(buffer.take().unwrap().unwrap(), frame);
        assert!(buffer.take().unwrap().is_none());
    }
    #[test]
    fn refuses_reservoir_and_format_changes_after_lock() {
        let mut frame = vec![0; 208];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x50, 0xc0]);
        let mut buffer = FrameBuffer {
            bytes: frame.clone(),
            locked: true,
        };
        buffer.bytes[4] = 1;
        assert!(buffer.take().is_err());
        frame[2] = 0x90;
        buffer.bytes = frame;
        assert!(buffer.take().is_err());
    }
}
