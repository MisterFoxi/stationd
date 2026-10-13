use super::*;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn config() -> Config {
    Config::parse(
        &toml::from_str(
            r#"
        [[stations]]
        slug = "demo"
        label = "Demo Radio"
        stream_url = "https://radio.example.test/live.mp3"
        grpc_endpoint = "http://127.0.0.1:50051"
    "#,
        )
        .unwrap(),
    )
    .unwrap()
}
#[test]
fn config_rejects_unsafe_and_ambiguous_settings() {
    let original = config();
    assert!(original.validate().is_ok());
    for bad in ["Demo", "../demo", "", "demo/other"] {
        let mut c = original.clone();
        c.stations[0].slug = bad.into();
        assert!(c.validate().is_err());
    }
    for bad in [
        "javascript:alert(1)",
        "https://user:pass@example.test/live",
        "file:///secret",
        "https://example.test/#fragment",
    ] {
        let mut c = original.clone();
        c.stations[0].stream_url = bad.into();
        assert!(c.validate().is_err());
    }
    let mut c = original.clone();
    c.stations.push(c.stations[0].clone());
    assert!(c.validate().is_err());
    let mut c = original.clone();
    c.stations[0].theme.accent = "red;display:none".into();
    assert!(c.validate().is_err());
    let mut c = original.clone();
    c.stations[0].grpc_endpoint = "http://remote.example.test:50051".into();
    assert!(c.validate().is_err());
    c.stations[0].allow_plaintext_grpc = true;
    assert!(c.validate().is_ok());
    c.max_event_streams = 0;
    assert!(c.validate().is_err());
}
#[test]
fn bind_accepts_network_ips_dns_names_and_bracketed_ipv6_without_resolving() {
    for value in [
        "127.0.0.1:8091",
        "192.168.1.111:8091",
        "0.0.0.0:8091",
        "[::]:8091",
        "[::1]:8091",
        "devstationd:8091",
        "radio.example.test:8091",
        "Radio.Example.Test.:8091",
    ] {
        let mut config = config();
        config.bind = value.into();
        assert!(config.validate().is_ok(), "{value}");
    }
    for value in [
        "",
        "localhost",
        "localhost:0",
        "localhost:65536",
        "localhost:abc",
        "localhost:+8091",
        "http://localhost:8091",
        "localhost:8091/path",
        "host name:8091",
        "-host.test:8091",
        "host-.test:8091",
        "host..test:8091",
        "host.test..:8091",
        "[::1]:0",
        "::1:8091",
    ] {
        let mut config = config();
        config.bind = value.into();
        assert!(config.validate().is_err(), "{value}");
    }
    assert!(parse_bind(&format!("{}.test:8091", "a".repeat(64))).is_err());
}

#[test]
fn dns_hostname_starts_real_http_server_and_shutdown_releases_port() {
    use std::io::{Read, Write};
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let mut config = config();
    config.bind = format!("localhost:{port}");
    let mut player = StationPlayer {
        config,
        server: None,
    };
    player.start().unwrap();
    let mut client = std::net::TcpStream::connect(("localhost", port)).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .write_all(b"GET /player/demo HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("Lecteur radio"));
    player.stop();
    assert!(std::net::TcpListener::bind(("localhost", port)).is_ok());
}

#[test]
fn optional_station_artwork_is_validated_without_requiring_an_image() {
    let mut config = config();
    assert!(config.stations[0].artwork_url.is_none());
    config.stations[0].artwork_url = Some("https://radio.example.test/visual.jpg".into());
    assert!(config.validate().is_ok());
    for invalid in [
        "javascript:alert(1)",
        "file:///secret",
        "https://user:password@example.test/image",
    ] {
        config.stations[0].artwork_url = Some(invalid.into());
        assert!(config.validate().is_err());
    }
}

fn air() -> onair::OnAirSnapshot {
    onair::OnAirSnapshot {
        observed_at: now(),
        liquidsoap: true,
        on_air: Some(onair::Track {
            title: "Now".into(),
            started_at: Some(now()),
            rel_path: "/private/library/now.mp3".into(),
            ..Default::default()
        }),
        prefetched: Some(onair::Track {
            title: "Committed".into(),
            ..Default::default()
        }),
        upcoming: vec![onair::Track {
            title: "Forecast".into(),
            ..Default::default()
        }],
        history: vec![onair::Track {
            title: "Actually played".into(),
            started_at: Some(now() - 180),
            ..Default::default()
        }],
        ..Default::default()
    }
}
#[test]
fn committed_queue_forecast_history_and_private_data_stay_distinct() {
    let snapshot = air();
    let dto = Air::from(&snapshot);
    assert_eq!(dto.coming_next[0].title, "Committed");
    assert_eq!(dto.indicative[0].title, "Forecast");
    assert_eq!(dto.history[0].title, "Actually played");
    assert!(!serde_json::to_string(&dto).unwrap().contains("private"));
    let mut simulated = snapshot;
    simulated.liquidsoap = false;
    let dto = Air::from(&simulated);
    assert!(dto.now_playing.is_none());
    assert!(dto.coming_next.is_empty());
}
#[tokio::test]
async fn public_router_is_read_only_scoped_and_sanitized() {
    let web = Web::new(&config());
    web.entries["demo"].cache.send_replace(Cache {
        air: Air::from(&air()),
        connected: true,
        received_at: Some(now()),
    });
    let app = routes(web);
    for path in [
        "/player/unknown",
        "/api/public/player/unknown",
        "/api/public/player/unknown/events",
    ] {
        assert_eq!(
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap()
                )
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/public/player/demo")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/public/player/demo")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["status"], "online");
    assert_eq!(value["air"]["coming_next"][0]["title"], "Committed");
    assert!(!String::from_utf8_lossy(&bytes).contains("grpc_endpoint"));
    assert!(!String::from_utf8_lossy(&bytes).contains("private/library"));
    assert_eq!(
        app.oneshot(
            Request::builder()
                .uri("/player/demo?compact=1")
                .body(axum::body::Body::empty())
                .unwrap()
        )
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
}
#[tokio::test]
async fn root_opens_the_only_station_and_lists_multiple_stations_safely() {
    let mut config = config();
    let root_request = || {
        Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let response = routes(Web::new(&config))
        .oneshot(root_request())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(response.headers()[header::LOCATION], "/player/demo");

    let mut second = config.stations[0].clone();
    second.slug = "other".into();
    second.label = "<script>alert('x')</script>".into();
    second.description = "<img src=x onerror=alert(1)>".into();
    config.stations.push(second);
    let response = routes(Web::new(&config))
        .oneshot(root_request())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("href=\"/player/demo\""));
    assert!(html.contains("href=\"/player/other\""));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("<img"));
}

#[test]
fn disconnected_or_old_snapshots_are_explicitly_stale() {
    let web = Web::new(&config());
    let entry = &web.entries["demo"];
    assert_eq!(view(entry)["status"], "connecting");
    entry.cache.send_replace(Cache {
        air: Air::from(&air()),
        connected: false,
        received_at: Some(now()),
    });
    assert_eq!(view(entry)["stale"], true);
    entry.cache.send_modify(|cache| {
        cache.connected = true;
        cache.air.observed_at = Some(now() - 61);
    });
    assert_eq!(view(entry)["status"], "offline");
}
#[tokio::test]
async fn sse_sends_initial_snapshot_updates_and_releases_quota() {
    let mut config = config();
    config.max_event_streams = 1;
    let web = Web::new(&config);
    let app = routes(web.clone());
    let request = || {
        Request::builder()
            .uri("/api/public/player/demo/events")
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut body = response.into_body();
    let first = tokio::time::timeout(Duration::from_secs(1), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("event: snapshot"));
    assert_eq!(
        app.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    web.entries["demo"].cache.send_replace(Cache {
        air: Air::from(&air()),
        connected: true,
        received_at: Some(now()),
    });
    let update = tokio::time::timeout(Duration::from_secs(1), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert!(String::from_utf8_lossy(&update).contains("Committed"));
    drop(body);
    tokio::time::timeout(Duration::from_secs(1), async {
        while web.streams.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[test]
fn occupied_port_fails_load_and_shutdown_releases_listener() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut c = config();
    c.bind = addr.to_string();
    let mut player = StationPlayer {
        config: c,
        server: None,
    };
    assert!(player.start().is_err());
    drop(listener);
    player.start().unwrap();
    assert!(std::net::TcpListener::bind(addr).is_err());
    player.stop();
    assert!(std::net::TcpListener::bind(addr).is_ok());
}

#[derive(Clone)]
struct MockAir {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    snapshots: watch::Sender<onair::OnAirSnapshot>,
}
#[tonic::async_trait]
impl onair::on_air_service_server::OnAirService for MockAir {
    type WatchStream = ReceiverStream<Result<onair::OnAirSnapshot, tonic::Status>>;
    async fn watch(
        &self,
        _: tonic::Request<onair::WatchRequest>,
    ) -> Result<tonic::Response<Self::WatchStream>, tonic::Status> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut rx = self.snapshots.subscribe();
        let (tx, out) = mpsc::channel(1);
        tokio::spawn(async move {
            loop {
                let value = rx.borrow_and_update().clone();
                if tx.send(Ok(value)).await.is_err() {
                    break;
                }
                tokio::select! { _ = tx.closed() => break, result = rx.changed() => { if result.is_err() { break; } } }
            }
        });
        Ok(tonic::Response::new(ReceiverStream::new(out)))
    }
    async fn history(
        &self,
        _: tonic::Request<onair::HistoryRequest>,
    ) -> Result<tonic::Response<onair::HistoryResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented("player must only use Watch"))
    }
}
#[tokio::test]
async fn real_grpc_watch_fans_out_and_updates_without_extra_rpc_per_visitor() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, incoming) = mpsc::channel(1);
    let accept = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            if tx.send(Ok::<_, std::io::Error>(socket)).await.is_err() {
                break;
            }
        }
    });
    let (snapshots, _) = watch::channel(air());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let service = MockAir {
        calls: calls.clone(),
        snapshots: snapshots.clone(),
    };
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(onair::on_air_service_server::OnAirServiceServer::new(
                service,
            ))
            .serve_with_incoming(ReceiverStream::new(incoming)),
    );
    let mut c = config();
    c.stations[0].grpc_endpoint = format!("http://{addr}");
    let web = Web::new(&c);
    let entry = web.entries["demo"].clone();
    let channel = Endpoint::from_shared(c.stations[0].grpc_endpoint.clone())
        .unwrap()
        .connect_lazy();
    let watcher = tokio::spawn(watch_station(entry.clone(), channel));
    let mut rx = entry.cache.subscribe();
    tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|c| c.connected))
        .await
        .unwrap()
        .unwrap();
    let app = routes(web);
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/public/player/demo/events")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let mut updated = air();
    updated.on_air.as_mut().unwrap().title = "Updated via real RPC".into();
    snapshots.send_replace(updated);
    tokio::time::timeout(
        Duration::from_secs(3),
        rx.wait_for(|c| {
            c.air
                .now_playing
                .as_ref()
                .is_some_and(|t| t.title == "Updated via real RPC")
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    watcher.abort();
    server.abort();
    accept.abort();
}
