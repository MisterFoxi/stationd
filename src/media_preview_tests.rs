use super::*;
use crate::proto::library::library_service_server::LibraryService;
use tokio_stream::StreamExt;

async fn fixture() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    crate::library_grpc::LibraryGrpc,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("selected.mp3"), b"0123456789").unwrap();
    let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
    crate::media_index::replace_library(
        &pool,
        &[crate::media::ScannedMedia {
            rel_path: "selected.mp3".into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 1000,
            size_bytes: 10,
            mtime_ns: 0,
        }],
        0,
    )
    .await
    .unwrap();
    let handle = crate::library_actor::spawn_with_policy(
        pool.clone(),
        dir.path().into(),
        None,
        Default::default(),
        true,
    );
    (dir, pool, crate::library_grpc::LibraryGrpc::new(handle))
}

#[tokio::test]
async fn media_preview_streams_full_ranges_and_suffix_on_readonly_station() {
    let (dir, pool, rpc) = fixture().await;
    let before = std::fs::metadata(dir.path().join("selected.mp3"))
        .unwrap()
        .modified()
        .unwrap();
    for (offset, length, suffix, expected) in [
        (0, 0, 0, "0123456789"),
        (2, 4, 0, "2345"),
        (8, 99, 0, "89"),
        (0, 0, 3, "789"),
        (0, 0, 99, "0123456789"),
    ] {
        let response = rpc
            .preview_audio(tonic::Request::new(library::MediaPreviewRequest {
                rel_path: "selected.mp3".into(),
                offset,
                length,
                suffix_length: suffix,
            }))
            .await
            .unwrap();
        assert_eq!(response.metadata().get("media-size").unwrap(), "10");
        let mut stream = response.into_inner();
        let mut bytes = vec![];
        while let Some(chunk) = stream.next().await {
            bytes.extend(chunk.unwrap().data);
        }
        assert_eq!(bytes, expected.as_bytes());
    }
    assert_eq!(
        std::fs::read(dir.path().join("selected.mp3")).unwrap(),
        b"0123456789"
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("selected.mp3"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    assert!(
        crate::media_index::row(&pool, "selected.mp3")
            .await
            .unwrap()
            .unwrap()
            .available
    );
    for table in ["broadcast_log", "queue_entry", "episode_play"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "preview wrote {table}");
    }
    let error = rpc
        .preview_audio(tonic::Request::new(library::MediaPreviewRequest {
            rel_path: "selected.mp3".into(),
            offset: 10,
            ..Default::default()
        }))
        .await
        .err()
        .unwrap();
    assert_eq!(error.code(), tonic::Code::OutOfRange);
    assert_eq!(error.metadata().get("media-size").unwrap(), "10");
}

#[tokio::test]
async fn media_preview_rejects_unindexed_unavailable_and_escaping_files() {
    let (dir, pool, _rpc) = fixture().await;
    std::fs::write(dir.path().join("unindexed.mp3"), b"hidden").unwrap();
    for path in [
        "../selected.mp3",
        "/selected.mp3",
        "folder/../../selected.mp3",
        "unindexed.mp3",
        "selected.wav",
        ".stationd-upload/source.part",
        "selected.mp3/../selected.mp3",
    ] {
        assert!(
            open(
                &pool,
                dir.path(),
                library::MediaPreviewRequest {
                    rel_path: path.into(),
                    ..Default::default()
                }
            )
            .await
            .is_err(),
            "{path}"
        );
    }
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("outside.mp3"), b"outside").unwrap();
        std::fs::remove_file(dir.path().join("selected.mp3")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("outside.mp3"),
            dir.path().join("selected.mp3"),
        )
        .unwrap();
        assert!(open(
            &pool,
            dir.path(),
            library::MediaPreviewRequest {
                rel_path: "selected.mp3".into(),
                ..Default::default()
            }
        )
        .await
        .is_err());
    }
    crate::media_index::replace_library(&pool, &[], 1)
        .await
        .unwrap();
    assert!(open(
        &pool,
        dir.path(),
        library::MediaPreviewRequest {
            rel_path: "selected.mp3".into(),
            ..Default::default()
        }
    )
    .await
    .is_err());
}

#[tokio::test]
async fn media_preview_shutdown_releases_a_backpressured_stream() {
    let (dir, pool, _rpc) = fixture().await;
    std::fs::write(dir.path().join("selected.mp3"), vec![0u8; 1024 * 1024]).unwrap();
    let (stop, rx) = watch::channel(false);
    let slots = Arc::new(Semaphore::new(1));
    let response = stream(
        &crate::library_actor::spawn_with_policy(
            pool.clone(),
            dir.path().into(),
            None,
            Default::default(),
            true,
        ),
        library::MediaPreviewRequest {
            rel_path: "selected.mp3".into(),
            ..Default::default()
        },
        Some(rx),
        slots.clone(),
    )
    .await
    .unwrap();
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while slots.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(response);
}
