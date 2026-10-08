//! One shared on-air stream and bounded health polling per configured station.
//! Only explicit presentation DTOs cross the HTTP boundary; never raw RPCs.
use super::{auth::now, Config, Station};
use crate::proto::{icecast, liquidsoap, onair, plugin, station};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::watch;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

const READ_TIMEOUT: Duration = Duration::from_secs(4);
const STALE_AFTER: i64 = 45;

#[derive(Clone, Default)]
struct Health {
    at: Option<i64>,
    online: bool,
    uptime: Option<u64>,
    liquidsoap: String,
    icecast: String,
    plugins: String,
    audience: Option<u32>,
}
#[derive(Clone, Default)]
struct Cached {
    snapshot: Option<onair::OnAirSnapshot>,
    received_at: Option<i64>,
    stream_online: bool,
    health: Health,
}
#[derive(Clone)]
struct Entry {
    station: Station,
    cache: watch::Sender<Cached>,
}
#[derive(Clone)]
pub(super) struct Live {
    entries: Arc<BTreeMap<String, Entry>>,
    started: Arc<AtomicBool>,
}
#[derive(Serialize)]
struct Media {
    title: String,
    artist: String,
    album: String,
    duration_ms: Option<u64>,
    started_at: Option<i64>,
    estimated_at: Option<i64>,
    stream: bool,
}
fn text(value: &str) -> String {
    value.chars().take(256).collect()
}
fn media(track: &onair::Track) -> Media {
    Media {
        title: text(&track.title),
        artist: text(&track.artist),
        album: text(&track.album),
        duration_ms: track.duration_ms,
        started_at: track.started_at,
        estimated_at: track.estimated_at,
        stream: track.stream,
    }
}
fn playlist(slot: &onair::PlaylistSlot) -> Value {
    json!({"name":text(&slot.playlist_ref),"from":slot.from,"at_local":text(&slot.at_local),"issue":slot.issue})
}
fn transport(station: &Station) -> Result<Channel, String> {
    let mut endpoint = Endpoint::from_shared(station.grpc_endpoint.clone())
        .map_err(|_| "invalid endpoint")?
        .connect_timeout(READ_TIMEOUT)
        .http2_keep_alive_interval(Duration::from_secs(15))
        .keep_alive_timeout(Duration::from_secs(5))
        .keep_alive_while_idle(true);
    if station.grpc_endpoint.starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(|_| "TLS configuration unavailable")?;
    }
    Ok(endpoint.connect_lazy())
}
impl Live {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            entries: Arc::new(
                config
                    .stations
                    .iter()
                    .map(|station| {
                        let (cache, _) = watch::channel(Cached::default());
                        (
                            station.id.clone(),
                            Entry {
                                station: station.clone(),
                                cache,
                            },
                        )
                    })
                    .collect(),
            ),
            started: Arc::new(AtomicBool::new(false)),
        }
    }
    // Must run inside the dedicated HTTP runtime. Its shutdown drops all tasks.
    pub(super) fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        for entry in self.entries.values() {
            match transport(&entry.station) {
                Ok(channel) => {
                    tokio::spawn(watch_station(entry.clone(), channel.clone()));
                    tokio::spawn(poll_health(entry.clone(), channel));
                }
                Err(_) => entry.cache.send_modify(|cache| {
                    cache.health.at = Some(now());
                }),
            }
        }
    }
    pub(super) fn network(&self, permitted: &Value) -> Value {
        json!({"stations":permitted.as_array().into_iter().flatten().filter_map(|station| {
            let id=station["id"].as_str()?;
            let mut view=self.view(id,false)?;
            view["role"]=station["role"].clone();
            Some(view)
        }).collect::<Vec<_>>()})
    }
    pub(super) fn view(&self, id: &str, detail: bool) -> Option<Value> {
        let entry = self.entries.get(id)?;
        let cache = entry.cache.borrow();
        let current = cache.snapshot.as_ref();
        let stream_fresh = cache.stream_online
            && cache
                .received_at
                .is_some_and(|at| now() - at <= STALE_AFTER)
            && current.is_some_and(|snapshot| {
                snapshot.observed_at > 0
                    && now().saturating_sub(snapshot.observed_at) <= STALE_AFTER
            });
        let health_fresh = cache.health.at.is_some_and(|at| now() - at <= 15);
        let connection = if cache.health.at.is_none() && current.is_none() {
            "connecting"
        } else if cache.health.online && health_fresh {
            "online"
        } else {
            "offline"
        };
        let stale = current.is_some() && (!stream_fresh || connection != "online");
        let mut alerts = Vec::new();
        if connection == "offline" {
            alerts.push("station_unreachable");
        }
        if !stream_fresh && connection == "online" {
            alerts.push("onair_unavailable");
        }
        if stale {
            alerts.push("stale_data");
        }
        if cache.health.liquidsoap == "error" {
            alerts.push("liquidsoap_error");
        }
        if cache.health.icecast == "error" {
            alerts.push("icecast_error");
        }
        if cache.health.plugins == "error" {
            alerts.push("plugin_error");
        }
        if current.is_some_and(|s| !s.liquidsoap) {
            alerts.push("no_liquidsoap");
        }
        let notes = current
            .map(|s| s.notes.iter().take(32).map(|n| n.code).collect::<Vec<_>>())
            .unwrap_or_default();
        if notes.contains(&8) {
            alerts.push("pool_empty");
        }
        if notes.iter().any(|n| matches!(n, 12 | 13 | 14 | 15)) {
            alerts.push("program_error");
        }
        let audience = if connection == "online" && health_fresh && cache.health.icecast == "ok" {
            cache.health.audience
        } else {
            None
        };
        let mut view = json!({"id":entry.station.id,"label":entry.station.label,"connection":connection,
            "stale":stale,"observed_at":current.map(|s|s.observed_at),"checked_at":cache.health.at,
            "state":current.map(|s|text(&s.state)),"audience":audience,
            "on_air_kind":current.map(|s|text(&s.on_air_kind)),"media":current.and_then(|s|s.on_air.as_ref()).map(media),"alerts":alerts});
        if detail {
            view["timezone"] = json!(current.map(|s| text(&s.timezone)));
            view["prefetched"] = json!(current.and_then(|s| s.prefetched.as_ref()).map(media));
            view["upcoming"] = json!(current
                .map(|s| s.upcoming.iter().take(10).map(media).collect::<Vec<_>>())
                .unwrap_or_default());
            view["upcoming_simulated"] = json!(true);
            view["current_playlist"] = json!(current
                .and_then(|s| s.current_playlist.as_ref())
                .map(playlist));
            view["next_playlists"] = json!(current
                .map(|s| s
                    .next_playlists
                    .iter()
                    .take(5)
                    .map(playlist)
                    .collect::<Vec<_>>())
                .unwrap_or_default());
            view["notes"] = json!(notes);
            view["live_dj"] = json!(current.map(|s| text(&s.live_dj)));
            view["services"] = json!({"stationd":connection,"uptime_seconds":cache.health.uptime,
                "liquidsoap":if health_fresh {cache.health.liquidsoap.as_str()} else {"unknown"},
                "icecast":if health_fresh {cache.health.icecast.as_str()} else {"unknown"},
                "plugins":if health_fresh {cache.health.plugins.as_str()} else {"unknown"}});
        }
        Some(view)
    }
}
async fn watch_station(entry: Entry, channel: Channel) {
    let mut delay = 1;
    loop {
        let mut client = onair::on_air_service_client::OnAirServiceClient::new(channel.clone())
            .max_decoding_message_size(262144);
        let request = onair::WatchRequest {
            upcoming: 10,
            history: 1,
            playlists_ahead: 5,
        };
        if let Ok(Ok(response)) = tokio::time::timeout(READ_TIMEOUT, client.watch(request)).await {
            let mut stream = response.into_inner();
            let mut first = true;
            loop {
                let timeout = if first {
                    Duration::from_secs(20)
                } else {
                    Duration::from_secs(STALE_AFTER as u64)
                };
                match tokio::time::timeout(timeout, stream.message()).await {
                    Ok(Ok(Some(snapshot))) => {
                        first = false;
                        delay = 1;
                        entry.cache.send_modify(|cache| {
                            cache.snapshot = Some(snapshot);
                            cache.received_at = Some(now());
                            cache.stream_online = true;
                        });
                    }
                    _ => break,
                }
            }
        }
        entry.cache.send_modify(|cache| cache.stream_online = false);
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(15);
    }
}
async fn poll_health(entry: Entry, channel: Channel) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let mut station = station::station_client::StationClient::new(channel.clone())
            .max_decoding_message_size(262144);
        let mut ls =
            liquidsoap::liquidsoap_service_client::LiquidsoapServiceClient::new(channel.clone())
                .max_decoding_message_size(262144);
        let mut ice = icecast::icecast_service_client::IcecastServiceClient::new(channel.clone())
            .max_decoding_message_size(262144);
        let mut plugins = plugin::plugin_service_client::PluginServiceClient::new(channel.clone())
            .max_decoding_message_size(262144);
        let (station, ls, ice, plugins) = tokio::join!(
            tokio::time::timeout(READ_TIMEOUT, station.status(station::StatusRequest {})),
            tokio::time::timeout(READ_TIMEOUT, ls.get_status(liquidsoap::GetStatusRequest {})),
            tokio::time::timeout(READ_TIMEOUT, ice.get_status(icecast::GetStatusRequest {})),
            tokio::time::timeout(READ_TIMEOUT, plugins.list(plugin::PluginListRequest {})),
        );
        let station = station.ok().and_then(Result::ok).map(|r| r.into_inner());
        let ls = ls.ok().and_then(Result::ok).map(|r| r.into_inner());
        let ice = ice.ok().and_then(Result::ok).map(|r| r.into_inner());
        let plugins = plugins.ok().and_then(Result::ok).map(|r| r.into_inner());
        let health = Health {
            at: Some(now()),
            online: station.is_some(),
            uptime: station.map(|s| s.uptime_seconds),
            liquidsoap: match ls {
                None => "unknown",
                Some(s) if !s.enabled => "disabled",
                Some(s) if !s.control_error.is_empty() => "error",
                Some(_) => "ok",
            }
            .into(),
            icecast: match &ice {
                None => "unknown",
                Some(s) if !s.enabled => "disabled",
                Some(s)
                    if !s.problem.is_empty()
                        || s.last_ok_at == 0
                        || now().saturating_sub(s.last_ok_at)
                            > i64::from(s.poll_interval_s) * 2 + 5 =>
                {
                    "error"
                }
                Some(_) => "ok",
            }
            .into(),
            plugins: match plugins {
                None => "unknown",
                Some(s)
                    if s.plugins
                        .iter()
                        .any(|p| matches!(p.state.as_str(), "failed" | "quarantined")) =>
                {
                    "error"
                }
                Some(_) => "ok",
            }
            .into(),
            audience: ice.and_then(|s| s.audience),
        };
        entry.cache.send_modify(|cache| cache.health = health);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};

    fn config(endpoint: &str) -> Config {
        Config::parse(
            &toml::from_str(&format!(
                r#"
            public_url = "https://remote.example.test"
            [[stations]]
            id = "one"
            label = "Station One"
            grpc_endpoint = "{endpoint}"
            [[stations]]
            id = "two"
            label = "Private Station Two"
            grpc_endpoint = "http://127.0.0.1:1"
        "#
            ))
            .unwrap(),
        )
        .unwrap()
    }
    fn snapshot() -> onair::OnAirSnapshot {
        onair::OnAirSnapshot {
            revision: 1,
            observed_at: now(),
            state: "running".into(),
            listeners: Some(0),
            on_air_kind: "track".into(),
            liquidsoap: true,
            timezone: "Europe/Paris".into(),
            on_air: Some(onair::Track {
                title: "Current title".into(),
                artist: "Artist".into(),
                rel_path: "secret-media-path".into(),
                duration_ms: Some(180000),
                started_at: Some(now() - 20),
                ..Default::default()
            }),
            upcoming: vec![onair::Track {
                title: "Next title".into(),
                rel_path: "secret-media-path".into(),
                ..Default::default()
            }],
            notes: vec![onair::Note {
                code: 13,
                reason: "secret-raw-error".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    #[test]
    fn webmin_live_projection_hides_secrets_and_preserves_unknown_and_stale_data() {
        let live = Live::new(&config("http://127.0.0.1:50051"));
        let entry = &live.entries["one"];
        entry.cache.send_modify(|cache| {
            cache.snapshot = Some(snapshot());
            cache.stream_online = true;
            cache.received_at = Some(now());
            cache.health = Health {
                at: Some(now()),
                online: true,
                icecast: "ok".into(),
                audience: Some(0),
                ..Default::default()
            };
        });
        let view = live.view("one", true).unwrap();
        assert_eq!(view["audience"], 0);
        assert_eq!(view["connection"], "online");
        assert_eq!(view["upcoming_simulated"], true);
        assert!(!view.to_string().contains("secret-"));
        let network = live.network(&json!([{"id":"one","role":"viewer"}]));
        assert_eq!(network["stations"].as_array().unwrap().len(), 1);
        assert!(!network.to_string().contains("Private Station Two"));
        assert!(!network.to_string().contains("grpc_endpoint"));
        entry
            .cache
            .send_modify(|cache| cache.snapshot.as_mut().unwrap().observed_at = now() - 90);
        assert_eq!(live.view("one", false).unwrap()["stale"], true);
        entry
            .cache
            .send_modify(|cache| cache.snapshot.as_mut().unwrap().observed_at = now());
        entry
            .cache
            .send_modify(|cache| cache.health.icecast = "error".into());
        assert!(live.view("one", false).unwrap()["audience"].is_null());
        entry.cache.send_modify(|cache| {
            cache.stream_online = false;
            cache.health.online = false;
        });
        let stale = live.view("one", false).unwrap();
        assert_eq!(stale["stale"], true);
        assert_eq!(stale["connection"], "offline");
        assert_eq!(stale["media"]["title"], "Current title");
        assert!(stale["audience"].is_null());
        assert!(live.view("missing", false).is_none());
    }
    #[derive(Clone)]
    struct Fake {
        source: watch::Sender<onair::OnAirSnapshot>,
        watches: Arc<AtomicUsize>,
        disconnect: watch::Sender<u64>,
    }
    #[tonic::async_trait]
    impl onair::on_air_service_server::OnAirService for Fake {
        type WatchStream = ReceiverStream<Result<onair::OnAirSnapshot, tonic::Status>>;
        async fn watch(
            &self,
            _: tonic::Request<onair::WatchRequest>,
        ) -> Result<tonic::Response<Self::WatchStream>, tonic::Status> {
            self.watches.fetch_add(1, Ordering::SeqCst);
            let mut source = self.source.subscribe();
            let mut disconnect = self.disconnect.subscribe();
            let (send, receive) = tokio::sync::mpsc::channel(1);
            tokio::spawn(async move {
                loop {
                    let snapshot = source.borrow_and_update().clone();
                    if send.send(Ok(snapshot)).await.is_err() {
                        break;
                    }
                    tokio::select! {
                        result = source.changed() => if result.is_err() {break;},
                        _ = disconnect.changed() => {let _=send.send(Err(tonic::Status::unavailable("test connection lost"))).await;break;},
                    }
                }
            });
            Ok(tonic::Response::new(ReceiverStream::new(receive)))
        }
        async fn history(
            &self,
            _: tonic::Request<onair::HistoryRequest>,
        ) -> Result<tonic::Response<onair::HistoryResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented("not used"))
        }
    }
    #[tonic::async_trait]
    impl station::station_server::Station for Fake {
        async fn status(
            &self,
            _: tonic::Request<station::StatusRequest>,
        ) -> Result<tonic::Response<station::StatusReply>, tonic::Status> {
            Ok(tonic::Response::new(station::StatusReply {
                station_name: "Actual station".into(),
                uptime_seconds: 20,
                ..Default::default()
            }))
        }
        async fn quit(
            &self,
            _: tonic::Request<station::QuitRequest>,
        ) -> Result<tonic::Response<station::QuitReply>, tonic::Status> {
            Err(tonic::Status::permission_denied("read only"))
        }
        async fn shutdown(
            &self,
            _: tonic::Request<station::ShutdownRequest>,
        ) -> Result<tonic::Response<station::ShutdownReply>, tonic::Status> {
            Err(tonic::Status::permission_denied("read only"))
        }
    }
    #[tonic::async_trait]
    impl icecast::icecast_service_server::IcecastService for Fake {
        async fn get_status(
            &self,
            _: tonic::Request<icecast::GetStatusRequest>,
        ) -> Result<tonic::Response<icecast::IcecastStatus>, tonic::Status> {
            Ok(tonic::Response::new(icecast::IcecastStatus {
                enabled: true,
                poll_interval_s: 15,
                last_ok_at: now(),
                audience: Some(0),
                server: "secret-server-endpoint".into(),
                mounts: vec![icecast::MountStatus {
                    source_ip: "secret-source-ip".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }))
        }
        async fn render_config(
            &self,
            _: tonic::Request<icecast::RenderConfigRequest>,
        ) -> Result<tonic::Response<icecast::RenderConfigResponse>, tonic::Status> {
            Err(tonic::Status::permission_denied("read only"))
        }
    }
    async fn await_view(live: &Live, title: &str) {
        tokio::time::timeout(Duration::from_secs(7), async {
            loop {
                let view = live.view("one", true).unwrap();
                if view["connection"] == "online" && view["media"]["title"] == title {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn webmin_live_shares_real_grpc_stream_and_reconnects_after_server_restart() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (source, _) = watch::channel(snapshot());
        let watches = Arc::new(AtomicUsize::new(0));
        let (disconnect, _) = watch::channel(0);
        let fake = Fake {
            source: source.clone(),
            watches: watches.clone(),
            disconnect: disconnect.clone(),
        };
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(onair::on_air_service_server::OnAirServiceServer::new(
                    fake.clone(),
                ))
                .add_service(station::station_server::StationServer::new(fake.clone()))
                .add_service(icecast::icecast_service_server::IcecastServiceServer::new(
                    fake.clone(),
                ))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _ = stopped.await;
                }),
        );
        let live = Live::new(&config(&format!("http://{address}")));
        live.start();
        live.start();
        await_view(&live, "Current title").await;
        for _ in 0..20 {
            let _ = live.network(&json!([{"id":"one","role":"viewer"}]));
        }
        assert_eq!(watches.load(Ordering::SeqCst), 1);
        assert_eq!(live.view("one", true).unwrap()["audience"], 0);
        assert!(!live
            .view("one", true)
            .unwrap()
            .to_string()
            .contains("secret-"));
        source.send_modify(|snapshot| {
            snapshot.revision += 1;
            snapshot.on_air.as_mut().unwrap().title = "Updated title".into();
        });
        await_view(&live, "Updated title").await;
        assert_eq!(watches.load(Ordering::SeqCst), 1);
        assert_ne!(live.view("two", false).unwrap()["connection"], "online");
        disconnect.send_modify(|generation| *generation += 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while live.entries["one"].cache.borrow().stream_online {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(live.view("one", false).unwrap()["stale"], true);
        let _ = stop.send(());
        // Abort after signalling shutdown: a graceful gRPC server otherwise
        // waits for the deliberately long-lived watch to finish.
        serving.abort();
        let _ = serving.await;
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let restarted = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(onair::on_air_service_server::OnAirServiceServer::new(
                    fake.clone(),
                ))
                .add_service(station::station_server::StationServer::new(fake.clone()))
                .add_service(icecast::icecast_service_server::IcecastServiceServer::new(
                    fake,
                ))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        // A new unary health read re-establishes the shared channel even while
        // the old stream is being closed by the transport.
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if watches.load(Ordering::SeqCst) > 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        await_view(&live, "Updated title").await;
        restarted.abort();
    }
    #[tokio::test]
    #[ignore = "requires a running station; set STATIOND_WEBMIN_TEST_GRPC"]
    async fn webmin_live_running_station_smoke() {
        let endpoint =
            std::env::var("STATIOND_WEBMIN_TEST_GRPC").expect("set STATIOND_WEBMIN_TEST_GRPC");
        let live = Live::new(&config(&endpoint));
        live.start();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let view = live.view("one", true).unwrap();
                if view["connection"] == "online" && !view["observed_at"].is_null() {
                    println!("WEBMIN_SNAPSHOT={}", view);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn webmin_live_rejects_untrusted_tls_certificates() {
        use openssl::{
            asn1::Asn1Time,
            hash::MessageDigest,
            pkey::PKey,
            rsa::Rsa,
            x509::{X509NameBuilder, X509},
        };
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "localhost").unwrap();
        let name = name.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        let serial = openssl::bn::BigNum::from_u32(1)
            .unwrap()
            .to_asn1_integer()
            .unwrap();
        cert.set_serial_number(&serial).unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        let identity = tonic::transport::Identity::from_pem(
            cert.build().to_pem().unwrap(),
            key.private_key_to_pem_pkcs8().unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (source, _) = watch::channel(snapshot());
        let (disconnect, _) = watch::channel(0);
        let watches = Arc::new(AtomicUsize::new(0));
        let fake = Fake {
            source,
            disconnect,
            watches: watches.clone(),
        };
        let serving = tokio::spawn(
            tonic::transport::Server::builder()
                .tls_config(tonic::transport::ServerTlsConfig::new().identity(identity))
                .unwrap()
                .add_service(onair::on_air_service_server::OnAirServiceServer::new(
                    fake.clone(),
                ))
                .add_service(station::station_server::StationServer::new(fake))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        let live = Live::new(&config(&format!("https://localhost:{}", address.port())));
        live.start();
        tokio::time::timeout(Duration::from_secs(7), async {
            loop {
                if live.view("one", false).unwrap()["connection"] == "offline" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(watches.load(Ordering::SeqCst), 0);
        assert!(live.view("one", false).unwrap()["media"].is_null());
        serving.abort();
    }
}
