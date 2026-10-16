use super::*;
use crate::media_upload::{
    tests::{mp3, wav},
    UploadTarget, STAGING_DIR,
};

#[tokio::test]
async fn upload_mp3_is_not_reencoded_and_does_not_need_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let target = UploadTarget::new(dir.path().into(), false);
    let mut staged = target.stage("original.MP3").unwrap();
    mp3(&staged.file);
    let original = std::fs::read(&staged.file).unwrap();
    let config = ConversionConfig {
        ffmpeg: "no-such-ffmpeg-executable".into(),
        ..Default::default()
    };
    staged.prepare(&config).await.unwrap();
    assert_eq!(std::fs::read(&staged.file).unwrap(), original);
    assert_eq!(staged.name, "original.MP3");
    assert!(staged.source.is_none());
}

#[tokio::test]
async fn upload_conversion_failure_keeps_original_hidden_from_scan() {
    let dir = tempfile::tempdir().unwrap();
    let target = UploadTarget::new(dir.path().into(), false);
    let mut staged = target.stage("original.flac").unwrap();
    std::fs::write(&staged.file, b"received source").unwrap();
    let original = staged.file.clone();
    let job = staged.directory.clone();
    let config = ConversionConfig {
        ffmpeg: "no-such-ffmpeg-executable".into(),
        ..Default::default()
    };
    assert!(staged.prepare(&config).await.is_err());
    drop(staged);
    assert_eq!(std::fs::read(original).unwrap(), b"received source");
    assert!(job.join("error.txt").is_file());
    assert!(crate::media::scan_library(dir.path())
        .unwrap()
        .media
        .is_empty());
    assert!(!dir.path().join("Uploaded").exists());
}

#[tokio::test]
#[ignore = "requires FFmpeg with libmp3lame (included in StationD Docker images)"]
async fn upload_ffmpeg_converts_wav_then_tags_and_indexes_mp3() {
    let dir = tempfile::tempdir().unwrap();
    let target = UploadTarget::new(dir.path().into(), false);
    let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
    let library = crate::library_actor::spawn(pool, dir.path().into());
    let mut staged = target.stage("été.wav").unwrap();
    wav(&staged.file);
    let mut edit = crate::media_tags::TagEdit::default();
    edit.artist = Some(Some("Artiste test".into()));
    edit.title = Some(Some("Titre test".into()));
    crate::media_tags::write(&staged.file, "été.wav", "", &edit).unwrap();
    let source = staged.file.clone();
    staged.prepare(&ConversionConfig::default()).await.unwrap();
    assert_eq!(staged.name, "été.mp3");
    assert!(source.is_file());
    assert!(crate::media::scan_library(dir.path())
        .unwrap()
        .media
        .is_empty());
    let result = library
        .import_upload(staged, "Uploaded".into(), "Type".into())
        .await
        .unwrap();
    assert!(result.indexed, "{:?}", result.scan_error);
    assert!(result.path.ends_with("/été.mp3"));
    let tags = crate::media_tags::read(&dir.path().join(&result.path))
        .unwrap()
        .0;
    assert_eq!(tags.user_values("Type"), ["uploaded"]);
    assert_eq!(tags.title.as_deref(), Some("Titre test"));
    assert_eq!(tags.artist.as_deref(), Some("Artiste test"));
    assert_eq!(tags.user_values(crate::media_identity::UUID_TAG).len(), 1);
    assert!(!source.exists());
    assert!(std::fs::read_dir(dir.path().join(STAGING_DIR))
        .unwrap()
        .next()
        .is_none());
}

#[tokio::test]
#[ignore = "requires FFmpeg (included in StationD Docker images)"]
async fn upload_ffmpeg_invalid_source_is_retained_without_publication() {
    let dir = tempfile::tempdir().unwrap();
    let target = UploadTarget::new(dir.path().into(), false);
    let mut staged = target.stage("broken.wav").unwrap();
    std::fs::write(&staged.file, b"not WAV audio").unwrap();
    let source = staged.file.clone();
    let directory = staged.directory.clone();
    assert!(staged.prepare(&ConversionConfig::default()).await.is_err());
    drop(staged);
    assert!(source.exists());
    assert!(directory.join("error.txt").exists());
    assert!(directory.join("conversion.log").exists());
    assert!(!directory.join(".converted.mp3.part").exists());
    assert!(!dir.path().join("Uploaded").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn upload_conversion_timeout_kills_child_and_keeps_original() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("converter");
    std::fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = UploadTarget::new(dir.path().into(), false);
    let mut staged = target.stage("a.wav").unwrap();
    wav(&staged.file);
    let source = staged.file.clone();
    let config = ConversionConfig {
        ffmpeg: executable.to_string_lossy().into_owned(),
        timeout_seconds: 1,
        ..Default::default()
    };
    let started = std::time::Instant::now();
    assert!(staged
        .prepare(&config)
        .await
        .unwrap_err()
        .contains("timeout"));
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(staged);
    assert!(source.exists());
}

#[tokio::test]
#[ignore = "requires FFmpeg with libmp3lame"]
async fn upload_conversion_output_limit_rejects_truncated_result() {
    let dir = tempfile::tempdir().unwrap();
    let target = UploadTarget::new(dir.path().into(), false);
    let mut staged = target.stage("long.wav").unwrap();
    wav(&staged.file);
    let mut bytes = std::fs::read(&staged.file).unwrap();
    let samples = 8000u32 * 35;
    bytes[4..8].copy_from_slice(&(36 + samples).to_le_bytes());
    bytes[40..44].copy_from_slice(&samples.to_le_bytes());
    bytes.resize(44 + samples as usize, 128);
    std::fs::write(&staged.file, bytes).unwrap();
    let source = staged.file.clone();
    let config = ConversionConfig {
        max_output_mb: 1,
        ..Default::default()
    };
    assert!(staged
        .prepare(&config)
        .await
        .unwrap_err()
        .contains("output size limit"));
    drop(staged);
    assert!(source.exists());
    assert!(crate::media::scan_library(dir.path())
        .unwrap()
        .media
        .is_empty());
}
