use super::*;
use axum::body::Body;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
async fn app() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
    let target = UploadTarget::new(dir.path().into(), false);
    target.attach_library(crate::library_actor::spawn(pool, dir.path().into()));
    let web = Web {
        config: Config {
            max_file_mb: 1,
            ..Default::default()
        },
        target,
        token_hash: Sha256::digest(TOKEN.as_bytes()).into(),
        slots: Arc::new(Semaphore::new(1)),
    };
    (dir, routes(web))
}
fn request(path: &str, body: impl Into<Body>) -> axum::http::Request<Body> {
    axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(body.into())
        .unwrap()
}
fn mp3() -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..10 {
        bytes.extend([0xff, 0xfb, 0x90, 0x00]);
        bytes.extend(vec![0; 413]);
    }
    bytes
}
#[tokio::test]
async fn uploader_http_receives_tags_and_indexes() {
    let (dir, app) = app().await;
    let response = app
        .oneshot(request("/api/upload/%C3%A9t%C3%A9.mp3", mp3()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["indexed"], true);
    let path = result["path"].as_str().unwrap();
    let tags = crate::media_tags::read(&dir.path().join(path)).unwrap().0;
    assert_eq!(tags.user_values("Type"), ["uploaded"]);
    assert!(
        std::fs::read_dir(dir.path().join(crate::media_upload::STAGING_DIR))
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn uploader_auth_size_and_validation_fail_without_publishing() {
    let (dir, app) = app().await;
    let mut no_auth = request("/api/upload/a.mp3", mp3());
    no_auth.headers_mut().remove(header::AUTHORIZATION);
    assert_eq!(
        app.clone().oneshot(no_auth).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut wrong_auth = request("/api/upload/a.mp3", mp3());
    wrong_auth
        .headers_mut()
        .insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(wrong_auth).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(request("/api/upload/a.txt", "data"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.clone()
            .oneshot(request("/api/upload/a%2Fb.mp3", mp3()))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.clone()
            .oneshot(request("/api/upload/a.mp3", ""))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.clone()
            .oneshot(request("/api/upload/a.mp3", "not audio"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let large = vec![0u8; 1024 * 1024 + 1];
    assert_eq!(
        app.clone()
            .oneshot(request("/api/upload/a.mp3", large))
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let mut length = request("/api/upload/a.mp3", "");
    length
        .headers_mut()
        .insert(header::CONTENT_LENGTH, "99999999".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(length).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert!(!dir.path().join("Uploaded").exists());
    assert!(
        std::fs::read_dir(dir.path().join(crate::media_upload::STAGING_DIR))
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn uploader_broken_stream_is_cleaned_up() {
    let (dir, app) = app().await;
    let stream = tokio_stream::iter(vec![
        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial")),
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "interrupted",
        )),
    ]);
    let response = app
        .oneshot(request("/api/upload/a.mp3", Body::from_stream(stream)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!dir.path().join("Uploaded").exists());
    assert!(
        std::fs::read_dir(dir.path().join(crate::media_upload::STAGING_DIR))
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn uploader_page_assets_and_limits() {
    let (_dir, app) = app().await;
    for path in ["/", "/upload.js", "/upload.css"] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY));
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    let mut form = request("/api/upload/a.mp3", mp3());
    form.headers_mut()
        .insert(header::CONTENT_TYPE, "multipart/form-data".parse().unwrap());
    assert_eq!(
        app.oneshot(form).await.unwrap().status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
}

#[test]
fn uploader_configuration_and_readonly_startup() {
    for text in [
        "inbox = '../outside'",
        "inbox = '.stationd-upload'",
        "max_file_mb = 0",
        "max_uploads = 0",
        "tag_field = 'STATIOND_UUID'",
        "token_env = 'LEGACY'",
        "unknown = true",
        "[conversion]\noutput_format = 'wav'",
        "[conversion]\nmp3_bitrate_kbps = 999",
        "[conversion]\ntimeout_seconds = 0",
    ] {
        assert!(
            validate_config(
                &format!("token = '{TOKEN}'\n{text}")
                    .parse::<toml::Table>()
                    .unwrap()
            )
            .is_err(),
            "{text}"
        );
    }
    assert!(validate_config(&toml::Table::new()).is_err());
    let config = format!("token = '{TOKEN}'").parse::<toml::Table>().unwrap();
    assert!(validate_config(&config).is_ok());
    let invalid = "token = 'private-short-token'"
        .parse::<toml::Table>()
        .unwrap();
    let error = validate_config(&invalid).unwrap_err();
    assert!(!error.contains("private-short-token"));
    let dir = tempfile::tempdir().unwrap();
    let mut plugin = MediaUploader::from_config(&config).unwrap();
    let mut host = Host::new(NAME, &[], None);
    host.media_upload = Some(UploadTarget::new(dir.path().into(), true));
    assert!(plugin.on_load(host).unwrap_err().contains("read_only"));
}

#[test]
fn uploader_server_can_stop_and_restart_without_leaking_its_port() {
    let dir = tempfile::tempdir().unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let table = format!("bind = '{address}'\ntoken = '{TOKEN}'")
        .parse::<toml::Table>()
        .unwrap();
    let mut plugin = MediaUploader::from_config(&table).unwrap();
    let mut host = Host::new(NAME, &[], None);
    host.media_upload = Some(UploadTarget::new(dir.path().into(), false));
    for _ in 0..2 {
        plugin.on_load(host.clone()).unwrap();
        assert!(std::net::TcpStream::connect(address).is_ok());
        assert!(plugin.on_load(host.clone()).is_err());
        plugin.on_unload();
        let listener = std::net::TcpListener::bind(address).unwrap();
        drop(listener);
    }
}

#[tokio::test]
#[ignore = "requires FFmpeg with libmp3lame"]
async fn uploader_http_converts_wav_and_reports_retained_conversion_errors() {
    let (dir, app) = app().await;
    let failed = app
        .clone()
        .oneshot(request("/api/upload/broken.flac", "not audio"))
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let failed: serde_json::Value =
        serde_json::from_slice(&failed.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let id = failed["staging_id"].as_str().unwrap();
    let source = dir
        .path()
        .join(crate::media_upload::STAGING_DIR)
        .join(id)
        .join("broken.flac.part");
    assert_eq!(std::fs::read(&source).unwrap(), b"not audio");
    assert!(!dir.path().join("Uploaded").exists());
    let n = 8000u32;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + n).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(8000u32.to_le_bytes());
    bytes.extend(8000u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(8u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(n.to_le_bytes());
    bytes.extend(vec![128; n as usize]);
    let response = app
        .oneshot(request("/api/upload/%C3%A9t%C3%A9.wav", bytes))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let result: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(result["indexed"], true);
    let path = result["path"].as_str().unwrap();
    assert!(path.ends_with("/été.mp3"));
    assert_eq!(
        crate::media_tags::read(&dir.path().join(path))
            .unwrap()
            .0
            .user_values("Type"),
        ["uploaded"]
    );
    assert!(
        source.exists(),
        "failure source is not purged by the next successful import"
    );
}
