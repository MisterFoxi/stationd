//! Host-owned receipt and publication of uploads. Only the writable node gets
//! this target; completed imports are serialized by the library actor.
use crate::library_actor::LibraryHandle;
use crate::media_tags::{self, TagEdit, TagError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Receipt directory. Its files carry a .part suffix, ignored by every scanner.
mod conversion;
pub(crate) use conversion::ConversionConfig;

pub const STAGING_DIR: &str = ".stationd-upload";

pub struct UploadTarget {
    root: PathBuf,
    read_only: bool,
    library: OnceLock<LibraryHandle>,
}
impl UploadTarget {
    pub fn new(root: PathBuf, read_only: bool) -> Arc<Self> {
        Arc::new(Self {
            root,
            read_only,
            library: OnceLock::new(),
        })
    }
    pub fn attach_library(&self, library: LibraryHandle) {
        let _ = self.library.set(library);
    }
    pub(crate) fn library(&self) -> Result<LibraryHandle, String> {
        self.library
            .get()
            .cloned()
            .ok_or_else(|| "media library is not ready".into())
    }
    pub(crate) fn check_writable(&self) -> Result<(), String> {
        if self.read_only {
            return Err("media-uploader requires media.read_only = false; enable it only on the media writer".into());
        }
        if !self.root.is_dir() {
            return Err("media library directory is unavailable".into());
        }
        Ok(())
    }
    pub(crate) fn stage(&self, name: &str) -> Result<StagedUpload, String> {
        self.check_writable()?;
        validate_filename(name)?;
        let root = self.root.canonicalize().map_err(|e| e.to_string())?;
        let staging = child_directory(&root, STAGING_DIR)?;
        let id = uuid::Uuid::new_v4().to_string();
        let directory = staging.join(&id);
        std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
        Ok(StagedUpload {
            source: None,
            conversion_output: None,
            retain: false,
            file: directory.join(format!("{name}.part")),
            directory,
            root,
            id,
            name: name.into(),
        })
    }
}

pub(crate) fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 200
        || name.starts_with('.')
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        || name.ends_with(['.', ' '])
    {
        return Err("invalid filename (a filename without directory is required)".into());
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(
        ext.as_str(),
        "mp3" | "wav" | "aif" | "aiff" | "flac" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wma"
    ) {
        return Err(
            "supported input formats: MP3, WAV, AIFF, FLAC, Ogg, Opus, M4A, AAC and WMA".into(),
        );
    }
    Ok(())
}

/// A single directory component, kept beneath the canonical library root.
fn child_directory(root: &Path, name: &str) -> Result<PathBuf, String> {
    let directory = root.join(name);
    match std::fs::create_dir(&directory) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.to_string()),
    }
    if std::fs::symlink_metadata(&directory)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
        || directory.canonicalize().map_err(|e| e.to_string())? != directory
        || !directory.is_dir()
    {
        return Err("upload directory must be a real directory under the media root".into());
    }
    Ok(directory)
}

/// Owned by the HTTP request until passed to the library actor. Cancellation
/// of an incomplete transfer removes its files; conversion failures retain the original.
pub(crate) struct StagedUpload {
    pub(crate) file: PathBuf,
    source: Option<PathBuf>,
    conversion_output: Option<PathBuf>,
    retain: bool,
    directory: PathBuf,
    root: PathBuf,
    id: String,
    name: String,
}
impl Drop for StagedUpload {
    fn drop(&mut self) {
        if let Some(output) = &self.conversion_output {
            let _ = std::fs::remove_file(output);
        }
        if self.retain {
            // A completed source stays available after conversion/tagging failure.
            // Converted output may be discarded; the original is authoritative.
            if self.source.is_some() {
                let _ = std::fs::remove_file(&self.file);
            }
            return;
        }
        let _ = std::fs::remove_file(&self.file);
        if let Some(source) = &self.source {
            let _ = std::fs::remove_file(source);
        }
        let _ = std::fs::remove_file(self.directory.join("conversion.log"));
        let _ = std::fs::remove_file(self.directory.join("error.txt"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}
impl StagedUpload {
    /// Called off-runtime, under the library writer lock, never from HTTP.
    pub(crate) fn publish(
        mut self,
        root: &Path,
        inbox: &str,
        tag_field: &str,
    ) -> Result<String, TagError> {
        let io = |e: String| TagError::Io(e);
        let canonical_root = root.canonicalize().map_err(|e| io(e.to_string()))?;
        if canonical_root != self.root {
            return Err(io("upload belongs to a different media library".into()));
        }
        let actual_type = lofty::probe::Probe::open(&self.file)
            .and_then(|probe| Ok(probe.guess_file_type()?))
            .map_err(|e| TagError::BadValue(format!("invalid audio file: {e}")))?
            .file_type();
        let extension = Path::new(&self.name)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let expected_type = match extension.as_str() {
            "mp3" => lofty::file::FileType::Mpeg,
            _ => return Err(TagError::BadValue("unsupported audio extension".into())),
        };
        if actual_type != Some(expected_type) {
            return Err(TagError::BadValue(
                "audio content does not match the filename extension".into(),
            ));
        }
        // MPEG audio also includes Layer I/II, which are not MP3.
        use lofty::file::AudioFile;
        let mut encoded = std::fs::File::open(&self.file).map_err(|e| io(e.to_string()))?;
        let mpeg =
            lofty::mpeg::MpegFile::read_from(&mut encoded, lofty::config::ParseOptions::new())
                .map_err(|e| TagError::BadValue(format!("invalid MP3: {e}")))?;
        if mpeg.properties().layer() != &lofty::mpeg::Layer::Layer3 {
            return Err(TagError::BadValue(
                "broadcast upload must be MPEG Audio Layer III (MP3)".into(),
            ));
        }
        drop(encoded);
        // Check the actual content, including nonzero duration, before touching tags.
        crate::media::scan_file(&self.directory, &self.file)
            .map_err(|e| TagError::BadValue(format!("invalid audio file: {e:?}")))?;
        crate::media_identity::initialize_upload(&self.file, expected_type)?;
        let mut edit = TagEdit::default();
        // Replace Type: an incoming file may already carry song/news/etc.
        edit.user.insert(tag_field.into(), vec!["uploaded".into()]);
        media_tags::write(&self.file, &self.name, "", &edit)?;
        let destination = child_directory(&canonical_root, inbox).map_err(io)?;
        let final_directory = destination.join(&self.id);
        std::fs::create_dir(&final_directory).map_err(|e| io(e.to_string()))?;
        // Every receipt file has a .part suffix, including during tagging.
        // Existing StationD scanners ignore it too. Only this last rename
        // makes the completed, tagged audio file visible to any station.
        if let Err(error) = std::fs::rename(&self.file, final_directory.join(&self.name)) {
            let _ = std::fs::remove_dir(&final_directory);
            return Err(io(error.to_string()));
        }
        self.retain = false;
        Ok(format!("{inbox}/{}/{}", self.id, self.name))
    }
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct UploadResult {
    pub(crate) path: String,
    pub(crate) indexed: bool,
    pub(crate) scan_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    pub(super) fn mp3(path: &Path) {
        let mut bytes = Vec::new();
        for _ in 0..10 {
            bytes.extend([0xff, 0xfb, 0x90, 0x00]);
            bytes.extend(vec![0; 413]);
        }
        std::fs::write(path, bytes).unwrap();
    }

    pub(super) fn wav(path: &Path) {
        let mut file = std::fs::File::create(path).unwrap();
        let n = 8000u32;
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + n).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&8000u32.to_le_bytes()).unwrap();
        file.write_all(&8000u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&8u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&n.to_le_bytes()).unwrap();
        file.write_all(&vec![128; n as usize]).unwrap();
    }

    #[test]
    fn pending_uploads_are_invisible_and_cancelled_uploads_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let target = UploadTarget::new(dir.path().into(), false);
        let staged = target.stage("été.mp3").unwrap();
        mp3(&staged.file);
        assert!(crate::media::scan_library(dir.path())
            .unwrap()
            .media
            .is_empty());
        let file = staged.file.clone();
        drop(staged);
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn import_tags_then_scans_and_keeps_same_named_files() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
        let target = UploadTarget::new(dir.path().into(), false);
        let library = crate::library_actor::spawn(pool, dir.path().into());
        target.attach_library(library.clone());
        let mut paths: Vec<String> = Vec::new();
        let mut identities = Vec::new();
        for _ in 0..2 {
            let staged = target.stage("été.mp3").unwrap();
            if let Some(previous) = paths.first() {
                std::fs::copy(dir.path().join(previous), &staged.file).unwrap();
            } else {
                mp3(&staged.file);
            }

            let mut edit = TagEdit::default();
            edit.user.insert("Type".into(), vec!["song".into()]);
            media_tags::write(&staged.file, "été.mp3", "", &edit).unwrap();
            let result = library
                .import_upload(staged, "Uploaded".into(), "Type".into())
                .await
                .unwrap();
            assert!(result.indexed, "{:?}", result.scan_error);
            let tags = media_tags::read(&dir.path().join(&result.path)).unwrap().0;
            assert_eq!(tags.user_values("Type"), ["uploaded"]);
            identities.push(tags.user_values(crate::media_identity::UUID_TAG));
            paths.push(result.path);
        }
        assert_ne!(paths[0], paths[1]);
        assert_eq!(identities[0].len(), 1);
        assert_eq!(identities[1].len(), 1);
        assert_ne!(identities[0], identities[1]);
        let replica_pool = crate::db::init(&dir.path().join("replica.db"))
            .await
            .unwrap();
        let replica = crate::library_actor::spawn_with_policy(
            replica_pool,
            dir.path().into(),
            None,
            Default::default(),
            true,
        );
        assert_eq!(replica.scan().await.unwrap().report.media.len(), 2);
        assert_eq!(library.list(true, vec![]).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn readonly_actor_refuses_import_even_with_a_writable_target() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
        let target = UploadTarget::new(dir.path().into(), false);
        let staged = target.stage("a.mp3").unwrap();
        mp3(&staged.file);
        let file = staged.file.clone();
        let library = crate::library_actor::spawn_with_policy(
            pool,
            dir.path().into(),
            None,
            Default::default(),
            true,
        );
        assert!(matches!(
            library
                .import_upload(staged, "Uploaded".into(), "Type".into())
                .await,
            Err(crate::library_actor::LibraryError::ReadOnly)
        ));
        assert!(!file.exists());
        assert!(!dir.path().join("Uploaded").exists());
        assert!(UploadTarget::new(dir.path().into(), true)
            .stage("a.mp3")
            .is_err());
    }

    #[test]
    fn rejects_traversal_unsupported_formats_and_invalid_audio() {
        for name in [
            "../a.mp3",
            "a/b.wav",
            "a\\b.wav",
            "a:foo.mp3",
            ".hidden.wav",
            "a.txt",
            "a.mp3\n",
        ] {
            assert!(validate_filename(name).is_err(), "{name}");
        }
        let dir = tempfile::tempdir().unwrap();
        let target = UploadTarget::new(dir.path().into(), false);
        let staged = target.stage("fake.mp3").unwrap();
        std::fs::write(&staged.file, b"not audio").unwrap();
        assert!(staged.publish(dir.path(), "Uploaded", "Type").is_err());
        assert!(!dir.path().join("Uploaded").exists());
    }

    #[tokio::test]
    async fn published_file_is_kept_when_scan_fails() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
        let target = UploadTarget::new(dir.path().into(), false);
        let staged = target.stage("a.mp3").unwrap();
        mp3(&staged.file);
        let library = crate::library_actor::spawn(pool.clone(), dir.path().into());
        pool.close().await;
        let result = library
            .import_upload(staged, "Uploaded".into(), "Type".into())
            .await
            .unwrap();
        assert!(!result.indexed);
        assert!(result.scan_error.is_some());
        assert!(dir.path().join(result.path).is_file());
    }

    #[test]
    fn filename_must_match_audio_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = UploadTarget::new(dir.path().into(), false);
        let staged = target.stage("wrong.mp3").unwrap();
        wav(&staged.file);
        assert!(staged.publish(dir.path(), "Uploaded", "Type").is_err());
        assert!(!dir.path().join("Uploaded").exists());
    }

    #[test]
    fn upload_rejects_other_mpeg_layers_disguised_as_mp3() {
        let dir = tempfile::tempdir().unwrap();
        let target = UploadTarget::new(dir.path().into(), false);
        let staged = target.stage("wrong.mp3").unwrap();
        let mut audio = Vec::new();
        for _ in 0..10 {
            audio.extend([0xff, 0xfd, 0x80, 0x00]);
            audio.extend(vec![0; 413]);
        }
        std::fs::write(&staged.file, audio).unwrap();
        let error = staged.publish(dir.path(), "Uploaded", "Type").unwrap_err();
        assert!(error.to_string().contains("Layer III"), "{error}");
        assert!(!dir.path().join("Uploaded").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_staging_or_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(STAGING_DIR)).unwrap();
        let target = UploadTarget::new(dir.path().into(), false);
        assert!(target.stage("a.mp3").is_err());
        std::fs::remove_file(dir.path().join(STAGING_DIR)).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("Uploaded")).unwrap();
        let staged = target.stage("a.mp3").unwrap();
        mp3(&staged.file);
        assert!(staged.publish(dir.path(), "Uploaded", "Type").is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }
}
