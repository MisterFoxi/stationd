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
