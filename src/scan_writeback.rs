//! Write creation/tempo TXXX frames and estimated TBPM after plugin enrichment.
//! The WASM guest never gets filesystem access. The library actor owns writes.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::id3::v2::{Frame, FrameId, Id3v2Tag, Id3v2Version, TextInformationFrame};
use lofty::TextEncoding;
use std::borrow::Cow;
use lofty::tag::TagExt;
use crate::media::ScanReport;
use crate::media_tags::TagError;

pub type Originals = BTreeMap<String, (u64, i64)>;

fn io(error: impl std::fmt::Display) -> TagError { TagError::Io(error.to_string()) }
fn identity(path: &Path) -> Result<(u64, i64), TagError> {
    let m = std::fs::metadata(path).map_err(io)?;
    let ns = m.modified().map_err(io)?.duration_since(std::time::UNIX_EPOCH)
        .map_err(io)?.as_nanos() as i64;
    Ok((m.len(), ns))
}
fn read_tag(path: &Path) -> Result<Id3v2Tag, TagError> {
    let mut f = std::fs::File::open(path).map_err(io)?;
    let mp3 = lofty::mpeg::MpegFile::read_from(&mut f, ParseOptions::new()).map_err(io)?;
    // Readback must find the concrete ID3v2 tag that the staged write produced.
    mp3.id3v2().cloned().ok_or_else(|| io("MP3 has no ID3v2 tag"))
}
struct Staged(PathBuf);
impl Drop for Staged {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
}

/// Returns true only after a verified, atomic replacement. Missing derived
/// values leave existing frames alone; no inference from the file's mtime.
pub fn write(root: &Path, rel: &str, expected: (u64, i64), values: &BTreeMap<String, String>) -> Result<bool, TagError> {
    let wanted: Vec<_> = ["creation", "tempo"].into_iter()
        .filter_map(|k| values.get(k).map(|v| (k, v))).collect();
    let bpm = values.get("bpm");
    if wanted.is_empty() && bpm.is_none() { return Ok(false); }
    let full = crate::media_tags::resolve(root, rel)?;
    let canonical_root = root.canonicalize().map_err(io)?;
    if !full.canonicalize().map_err(io)?.starts_with(&canonical_root)
        || std::fs::symlink_metadata(&full).map_err(io)?.file_type().is_symlink() {
        return Err(io(format!("refusing metadata write outside root or through symlink: {rel}")));
    }
    if identity(&full)? != expected { return Err(io(format!("file changed during scan: {rel}"))); }
    let before = std::fs::metadata(&full).map_err(io)?;
    // Seed missing ID3v2 from ID3v1, using the standard editor's preservation policy.
    let (mut tag, version) = crate::media_tags::id3v2_of(&full, lofty::file::FileType::Mpeg)?;
    if !matches!(version, Id3v2Version::V3 | Id3v2Version::V4) {
        return Err(io(format!("metadata write requires ID3v2.3 or ID3v2.4: {rel}")));
    }
    let mut changed = false;
    let bpm_id = FrameId::Valid(Cow::Borrowed("TBPM"));
    let mut written_bpm = None;
    if let Some(value) = bpm {
        let parsed = value.parse::<u32>().map_err(io)?;
        if !(45..=240).contains(&parsed) { return Err(io("estimated BPM outside 45..240")); }
        // Existing valid TBPM is authoritative, even if an enrichment supplied BPM.
        let valid = tag.get_text(&bpm_id).and_then(|v| v.parse::<f64>().ok())
            .is_some_and(|v| v.is_finite() && v > 0.0);
        if !valid {
            tag.insert(Frame::Text(TextInformationFrame::new(bpm_id.clone(), TextEncoding::UTF16, value.clone())));
            written_bpm = Some(value);
            changed = true;
        }
    }
    for (key, value) in &wanted {
        if value.trim().is_empty() { return Err(io(format!("blank {key} for {rel}"))); }
        let names: Vec<String> = (&tag).into_iter().filter_map(|f| match f {
            Frame::UserText(t) if t.description.eq_ignore_ascii_case(key) => Some(t.description.to_string()),
            _ => None,
        }).collect();
        if names.len() == 1 && names[0] == *key && tag.get_user_text(key) == Some(value.as_str()) { continue; }
        for name in names { tag.remove_user_text(&name); }
        tag.insert_user_text((*key).to_string(), (*value).clone());
        changed = true;
    }
    if !changed { return Ok(false); }
    // Stage beside the original: the final rename stays on the same filesystem.
    // A failed save/readback never truncates the original MP3.
    let stage = Staged(full.with_file_name(format!(".stationd-{}.mp3", uuid::Uuid::new_v4())));
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&stage.0).map_err(io)?;
    let mut source = std::fs::File::open(&full).map_err(io)?;
    std::io::copy(&mut source, &mut output).map_err(io)?;
    drop(source);
    drop(output);
    tag.save_to_path(&stage.0, WriteOptions::default().use_id3v23(version == Id3v2Version::V3)).map_err(io)?;
    let after = read_tag(&stage.0)?;
    if let Some(value) = written_bpm {
        if after.get_text(&bpm_id) != Some(value.as_str()) { return Err(io("BPM readback failed")); }
    }
    for (key, value) in wanted {
        if after.get_user_text(key) != Some(value.as_str()) {
            return Err(io(format!("metadata readback failed for {rel}: {key}")));
        }
    }
    std::fs::set_permissions(&stage.0, before.permissions()).map_err(io)?;
    let staged_file = std::fs::OpenOptions::new().write(true).open(&stage.0).map_err(io)?;
    staged_file.set_modified(before.modified().map_err(io)?).map_err(io)?;
    staged_file.sync_all().map_err(io)?;
    drop(staged_file);
    if identity(&full)? != expected { return Err(io(format!("file changed during metadata write: {rel}"))); }
    std::fs::rename(&stage.0, &full).map_err(io)?;
    Ok(true)
}

/// Only MP3s are written. Keep the fresh size/mtime in the snapshot, so database
/// identity and play-once guards describe the actual file after tag growth.
pub fn apply(root: &Path, report: &mut ScanReport) -> Result<Originals, TagError> {
    let mut originals = Originals::new();
    for m in &mut report.media {
        if !Path::new(&m.rel_path).extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) { continue; }
        let Some(values) = report.metadata.get(&m.rel_path) else { continue };
        let old = (m.size_bytes, m.mtime_ns);
        if write(root, &m.rel_path, old, values).map_err(|e| io(format!("{}: {e}", m.rel_path)))? {
            originals.insert(m.rel_path.clone(), old);
            (m.size_bytes, m.mtime_ns) = identity(&root.join(&m.rel_path))?;
        }
    }
    Ok(originals)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bpm_write_preserves_audio_and_never_overwrites_valid_tbpm() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("test.mp3");
        let audio = fixture(&p, false);
        // Also exercise a completely untagged MPEG stream.
        std::fs::write(&p, &audio).unwrap();
        let values = [("bpm".into(), "140".into()), ("tempo".into(), "fast".into())].into();
        assert!(write(dir.path(), "test.mp3", identity(&p).unwrap(), &values).unwrap());
        let id = FrameId::Valid(Cow::Borrowed("TBPM"));
        assert_eq!(read_tag(&p).unwrap().get_text(&id), Some("140"));
        assert!(std::fs::read(&p).unwrap().ends_with(&audio));
        let before = std::fs::read(&p).unwrap();
        let other = [("bpm".into(), "90".into())].into();
        assert!(!write(dir.path(), "test.mp3", identity(&p).unwrap(), &other).unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), before);
        let invalid = [("bpm".into(), "NaN".into())].into();
        assert!(write(dir.path(), "test.mp3", identity(&p).unwrap(), &invalid).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    fn fixture(path: &Path, v3: bool) -> Vec<u8> {
        // Ten MPEG1 Layer III frames, 128 kbps, 44.1 kHz. No copyrighted audio.
        let mut audio = Vec::new();
        for _ in 0..10 {
            audio.extend([0xff, 0xfb, 0x90, 0x00]);
            audio.extend(vec![0; 413]);
        }
        std::fs::write(path, &audio).unwrap();
        let mut tag = Id3v2Tag::default();
        tag.insert_user_text("comment".into(), "made with suno; created=2026-06-14T06:36:48Z; id=test".into());
        tag.insert_user_text("type".into(), "song".into());
        tag.insert_user_text("BPM".into(), "125".into());
        tag.insert_user_text("CREATION".into(), "old".into());
        tag.save_to_path(path, WriteOptions::default().use_id3v23(v3)).unwrap();
        audio
    }
    #[test]
    fn mp3_roundtrip_preserves_source_audio_version_mtime_and_is_idempotent() {
        for v3 in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("test.mp3");
            let audio = fixture(&p, v3);
            let expected = identity(&p).unwrap();
            let values = [("creation".into(), "2026-06-14T06:36:48Z".into()), ("tempo".into(), "fast".into())].into();
            assert!(write(dir.path(), "test.mp3", expected, &values).unwrap());
            let tag = read_tag(&p).unwrap();
            assert_eq!(tag.get_user_text("creation"), Some("2026-06-14T06:36:48Z"));
            assert_eq!(tag.get_user_text("tempo"), Some("fast"));
            assert_eq!(tag.get_user_text("CREATION"), None);
            assert_eq!(tag.get_user_text("type"), Some("song"));
            assert_eq!(tag.get_user_text("BPM"), Some("125"));
            assert!(tag.get_user_text("comment").unwrap().contains("id=test"));
            assert_eq!(tag.original_version(), if v3 { Id3v2Version::V3 } else { Id3v2Version::V4 });
            assert_eq!(identity(&p).unwrap().1, expected.1);
            let bytes = std::fs::read(&p).unwrap();
            assert!(bytes.ends_with(&audio), "audio frames must be byte-for-byte intact");
            assert!(!write(dir.path(), "test.mp3", identity(&p).unwrap(), &values).unwrap());
            assert_eq!(std::fs::read(&p).unwrap(), bytes);
        }
    }
    #[test]
    fn stale_scan_and_missing_values_do_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("test.mp3");
        fixture(&p, false);
        let before = std::fs::read(&p).unwrap();
        assert!(!write(dir.path(), "test.mp3", (0, 0), &BTreeMap::new()).unwrap());
        let values = [("tempo".into(), "fast".into())].into();
        assert!(write(dir.path(), "test.mp3", (0, 0), &values).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }
    #[test]
    fn scan_report_tracks_updated_size_and_keeps_genres() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("test.mp3");
        fixture(&p, false);
        let mut report = crate::media::scan_library(dir.path()).unwrap();
        report.metadata.insert("test.mp3".into(), [("tempo".into(), "fast".into())].into());
        let old = (report.media[0].size_bytes, report.media[0].mtime_ns);
        let originals = apply(dir.path(), &mut report).unwrap();
        assert_eq!(originals["test.mp3"], old);
        assert_eq!((report.media[0].size_bytes, report.media[0].mtime_ns), identity(&p).unwrap());
        assert!(report.media[0].genres.is_empty());
        assert!(apply(dir.path(), &mut report).unwrap().is_empty());
    }
    #[tokio::test]
    async fn writeback_persists_metadata_and_preserves_played_episode_guard() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("test.mp3");
        fixture(&p, false);
        let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
        let mut report = crate::media::scan_library(dir.path()).unwrap();
        crate::media_index::replace_library(&pool, &report.media, 1).await.unwrap();
        let old = (report.media[0].size_bytes as i64, report.media[0].mtime_ns);
        sqlx::query("INSERT INTO episode_play (playlist_ref, rel_path, size_bytes, mtime_ns, played_at) VALUES ('test', 'test.mp3', ?1, ?2, 1)")
            .bind(old.0).bind(old.1).execute(&pool).await.unwrap();
        report.metadata.insert("test.mp3".into(), [
            ("creation".into(), "2026-06-14T06:36:48Z".into()),
            ("tempo".into(), "fast".into()),
        ].into());
        let originals = apply(dir.path(), &mut report).unwrap();
        crate::media_index::replace_library_with_writeback(&pool, &report.media, &report.metadata, &originals, 2).await.unwrap();
        let guard: (i64, i64) = sqlx::query_as("SELECT size_bytes, mtime_ns FROM episode_play WHERE rel_path = 'test.mp3'").fetch_one(&pool).await.unwrap();
        assert_eq!(guard, (report.media[0].size_bytes as i64, report.media[0].mtime_ns));
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM media_meta ORDER BY key").fetch_all(&pool).await.unwrap();
        assert_eq!(rows, vec![("creation".into(), "2026-06-14T06:36:48.000000000Z".into()), ("tempo".into(), "fast".into())]);
    }

}
