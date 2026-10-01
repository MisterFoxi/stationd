use super::*;
use axum::{http::{HeaderMap, StatusCode, Uri}, routing::get, Router};

fn config(address: std::net::SocketAddr, password: &str) -> IcecastConfig {
    IcecastConfig {
        admin_url: format!("http://{address}"), admin_user: "admin".into(),
        admin_password: password.into(), poll_interval: 15,
        poll_interval_sleeping: 3, listener_snapshots: true, server: None,
    }
}

#[tokio::test]
async fn listclients_authenticates_and_encodes_the_mount() {
    let app = Router::new().route("/admin/listclients", get(|headers: HeaderMap, uri: Uri| async move {
        if headers.get("authorization").unwrap() != "Basic YWRtaW46Z29vZA==" {
            return (StatusCode::UNAUTHORIZED, "");
        }
        assert_eq!(uri.query(), Some("mount=%2Fradio%20%26%3F"));
        (StatusCode::OK, r#"<icestats><source mount="/radio &amp;?"><Listeners>0</Listeners></source></icestats>"#)
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert!(IcecastClient::new(&config(address, "good")).unwrap().list_clients("/radio &?").await.unwrap().is_empty());
    assert!(IcecastClient::new(&config(address, "bad")).unwrap().list_clients("/radio &?").await.is_err());
    task.abort();
}

#[tokio::test]
async fn slow_details_do_not_delay_global_audience() {
    let app = Router::new()
        .route("/admin/listclients", get(|| async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            ""
        }))
        .route("/admin/stats", get(|| async {
            r#"<icestats><source mount="/radio"><listeners>7</listeners><stream_start>now</stream_start></source></icestats>"#
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let control = StationControl::new_in_memory();
    let sampler = spawn_sampler(IcecastClient::new(&config(address, "good")).unwrap(),
        vec!["/radio".into()], Duration::from_secs(15), Duration::from_secs(3),
        control.clone(), IcecastMonitor::default());
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        while control.listeners() != Some(7) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;
    sampler.abort();
    server.abort();
    assert!(result.is_ok(), "global audience waited for listclients");
}

#[tokio::test]
async fn shared_details_cover_all_mounts_and_failure_wakes_age_sleep() {
    use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
    let fail = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_fail = fail.clone();
    let handler_calls = calls.clone();
    let app = Router::new()
        .route("/admin/listclients", get(move |uri: Uri| {
            let fail = handler_fail.clone();
            let calls = handler_calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let mount = match uri.query() {
                    Some("mount=%2Fa") => "/a",
                    Some("mount=%2Fb") => "/b",
                    _ => panic!("unexpected mount"),
                };
                if mount == "/b" && fail.load(Ordering::SeqCst) {
                    return (StatusCode::SERVICE_UNAVAILABLE, String::new());
                }
                (StatusCode::OK, format!(r#"<icestats><source mount="{mount}"><Listeners>1</Listeners>
                    <listener><ID>1</ID><IP>192.0.2.1</IP><Connected>100</Connected>
                    <UserAgent>private</UserAgent></listener></source></icestats>"#))
            }
        }))
        .route("/admin/stats", get(|| async {
            r#"<icestats><source mount="/a"><listeners>1</listeners><stream_start>now</stream_start></source>
                <source mount="/b"><listeners>1</listeners><stream_start>now</stream_start></source></icestats>"#
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let control = StationControl::new_in_memory();
    let sampler = spawn_sampler(IcecastClient::new(&config(address, "good")).unwrap(),
        vec!["/a".into(), "/a".into(), "/b".into()],
        Duration::from_secs(1), Duration::from_millis(100),
        control.clone(), IcecastMonitor::default());
    let ready = tokio::time::timeout(Duration::from_secs(2), async {
        while control.listener_connections().is_none() || control.listeners().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await;
    if ready.is_err() {
        sampler.abort();
        server.abort();
        panic!("connection collection did not finish");
    }
    let clients = control.listener_connections().unwrap();
    assert_eq!(clients.len(), 2);
    assert_ne!(clients[0].mount, clients[1].mount);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one shared call per distinct mount");
    control.stop_when_connections_old(50, "p").unwrap();
    assert_eq!(control.gate(), crate::station_control::Gate::Halt(crate::station_control::BroadcastState::Sleeping));
    fail.store(true, Ordering::SeqCst);
    let recovered = tokio::time::timeout(Duration::from_secs(3), async {
        while control.state() == crate::station_control::BroadcastState::Sleeping {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;
    sampler.abort();
    server.abort();
    assert!(recovered.is_ok(), "a persistently missing mount must wake age-based sleep");
    assert_eq!(control.listener_connections(), None, "partial snapshot is unknown");
}
