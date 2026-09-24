//! Media library scanner — the PURE side (no SQLite, no tokio).
//!
//! Walks the media root and reads audio tags/duration with `lofty`. Produces
//! plain data (`ScannedMedia` + a `ScanReport` of what was skipped and why),
//! which `media_index` then persists into the family-(A) view. Keeping this
//! module free of `sqlx` and `tokio` is what makes it testable without a DB
//! and safe to run off the async runtime: the gRPC handler wraps `scan_library`
//! in `tokio::task::spawn_blocking` (the walk + parse is blocking I/O). Parallel
//! parsing via `rayon` is a later optimisation, not needed for a first pass.
//!
//! No-silent-failure applied to the scan: a per-file failure (unreadable,
//! zero-length) does NOT abort the batch and is NOT swallowed — it lands in
//! `ScanReport.skipped` with a reason. Only a broken root (missing / not a
//! directory) is a hard error, because that is a deployment/config fault, not
//! a data fault.
//!
//! Path identity: `rel_path` is relative to the media root, '/'-separated,
//! with case preserved (grammaire §3.3). This is deliberately different from
//! playlist refs, which are lower-cased — media paths address real files on a
//! possibly case-sensitive filesystem.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// Audio extensions we attempt to read. Anything else in the media tree
/// (cover art, `.txt`, stray files) is simply not a media candidate and is
/// NOT reported as an error — only audio-extension files that then fail to
/// parse are. Compared lower-cased; the stored path keeps its original case.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "m4b", "aac", "wav", "wave",
    "wv", "ape", "aif", "aiff", "mpc", "spx",
];

/// One playable media file, as read from disk. Precise durable duration is in
/// milliseconds; the grammaire's second-granularity `duration` filter divides
/// by 1000 at query time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedMedia {
    /// Relative to the media root, '/'-separated, case preserved.
    pub rel_path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    /// Set of genres (0..n). v1 reads the primary genre string as a single
    /// element; multi-valued genre frames are a later refinement.
    pub genres: Vec<String>,
    /// Strictly positive (a zero-length file is skipped, never stored).
    pub duration_ms: u64,
    pub size_bytes: u64,
    /// Modification time in nanoseconds since the UNIX epoch (guard-rail).
    /// `0` if the platform did not expose it — degraded, not fatal.
    pub mtime_ns: i64,
}

/// Why an audio-extension file was left out of the index. Surfaced, never
/// swallowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `lofty` could not parse the file (corrupt, truncated, wrong content).
    Unreadable(String),
    /// Parsed, but reported no usable duration — the scheduler cannot place a
    /// track it cannot time, so it is not a valid library entry.
    ZeroDuration,
    /// The directory walk itself failed on this entry (permissions, etc.).
    WalkError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSkip {
    /// Best-effort path (relative when known, else the raw path from the walk).
    pub path: String,
    pub reason: SkipReason,
}

/// One user-defined tag as found in the file, before any interpretation:
/// ID3v2 `TXXX` (name = description), Vorbis comment and APE item (name =
/// key), MP4 freeform atom (name = the part after `----:mean:`). A multi-valued
/// tag yields one `CustomTag` per value. Names keep the case of the file.
///
/// Not persisted: the core catalogue stays closed to standard tags. These are
/// scan-time input for the plugins' `on_scan` hook, which decides what (if
/// anything) they become — e.g. `TXXX:Type = talks` → genre `talks`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CustomTag {
    pub name: String,
    pub value: String,
}

/// Outcome of a scan: the playable media plus everything deliberately left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub media: Vec<ScannedMedia>,
    pub skipped: Vec<ScanSkip>,
    /// User-defined tags per `rel_path` (only media that carry at least one).
    /// Kept beside `media` rather than in `ScannedMedia`: transient plugin
    /// input, never a column of the index.
    pub custom_tags: std::collections::BTreeMap<String, Vec<CustomTag>>,
}

impl ScanReport {
    /// Number of playable files found.
    pub fn found(&self) -> usize {
        self.media.len()
    }
    /// Number of audio-extension files left out (with a recorded reason).
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }
}

/// Hard failure of the scan as a whole (as opposed to a per-file skip).
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("media root {0} does not exist or is not a directory")]
    BadRoot(PathBuf),
}

/// Normalise a filesystem path (relative to `root`) into a stored `rel_path`:
/// '/'-separated, case preserved, no leading separator. Returns `None` if the
/// path is not under `root` (should not happen for walk output).
fn to_rel_path(root: &Path, full: &Path) -> Option<String> {
    let rel = full.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    Some(s.trim_start_matches('/').to_string())
}

/// Is this a file we should try to read as audio?
fn is_audio_ext(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let lower = ext.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&lower.as_str())
        }
        None => false,
    }
}

/// Extract a 4-digit year from a tag value that may be a bare year (`2020`)
/// or a full date (`2020-05-01`). Returns `None` if no leading 4-digit run in
/// range 1..=9999 is present (kept in sync with the migration's CHECK).
fn parse_year(s: &str) -> Option<u32> {
    let digits: String = s.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() != 4 {
        return None;
    }
    match digits.parse::<u32>() {
        Ok(y) if (1..=9999).contains(&y) => Some(y),
        _ => None,
    }
}

/// Push `value` under `name`, splitting ID3v2.4 multi-values (NUL-separated)
/// and dropping blank names/values (nothing to say, not an error).
fn push_custom(out: &mut Vec<CustomTag>, name: &str, value: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    for v in value.split('\0') {
        let v = v.trim();
        if !v.is_empty() {
            out.push(CustomTag { name: name.to_string(), value: v.to_string() });
        }
    }
}

fn id3v2_custom(tag: Option<&lofty::id3::v2::Id3v2Tag>, out: &mut Vec<CustomTag>) {
    use lofty::id3::v2::Frame;
    for frame in tag.into_iter().flatten() {
        if let Frame::UserText(f) = frame {
            push_custom(out, &f.description, &f.content);
        }
    }
}

fn vorbis_custom(tag: Option<&lofty::ogg::VorbisComments>, out: &mut Vec<CustomTag>) {
    for (k, v) in tag.into_iter().flat_map(|t| t.items()) {
        push_custom(out, k, v);
    }
}

fn ape_custom(tag: Option<&lofty::ape::ApeTag>, out: &mut Vec<CustomTag>) {
    for item in tag.into_iter().flatten() {
        if let lofty::tag::ItemValue::Text(v) = item.value() {
            push_custom(out, item.key(), v);
        }
    }
}

fn mp4_custom(tag: Option<&lofty::mp4::Ilst>, out: &mut Vec<CustomTag>) {
    use lofty::mp4::{AtomData, AtomIdent};
    for atom in tag.into_iter().flatten() {
        if let AtomIdent::Freeform { name, .. } = atom.ident() {
            for data in atom.data() {
                if let AtomData::UTF8(v) = data {
                    push_custom(out, name, v);
                }
            }
        }
    }
}

/// Parse the file ONCE with its concrete lofty type, harvest the user-defined
/// tags from the raw tag (the generic `Tag` drops unknown `TXXX` & co.), then
/// convert to the generic `TaggedFile` for the standard fields. Same parse
/// options and extension-based type detection as `lofty::read_from_path`.
fn read_tagged(
    full: &Path,
) -> Result<(lofty::file::TaggedFile, Vec<CustomTag>), lofty::error::LoftyError> {
    use lofty::config::ParseOptions;
    use lofty::file::{AudioFile, FileType, TaggedFile};
    use lofty::probe::Probe;

    let probe = Probe::open(full)?;
    let Some(file_type) = probe.file_type() else {
        // Unknown to lofty by extension: let the generic path report it.
        return Ok((probe.read()?, Vec::new()));
    };
    let mut f = std::fs::File::open(full)?;
    let opts = ParseOptions::new();
    let mut custom = Vec::new();
    let tagged: TaggedFile = match file_type {
        FileType::Mpeg => {
            let x = lofty::mpeg::MpegFile::read_from(&mut f, opts)?;
            id3v2_custom(x.id3v2(), &mut custom);
            ape_custom(x.ape(), &mut custom);
            x.into()
        }
        FileType::Wav => {
            let x = lofty::iff::wav::WavFile::read_from(&mut f, opts)?;
            id3v2_custom(x.id3v2(), &mut custom);
            x.into()
        }
        FileType::Aiff => {
            let x = lofty::iff::aiff::AiffFile::read_from(&mut f, opts)?;
            id3v2_custom(x.id3v2(), &mut custom);
            x.into()
        }
        FileType::Flac => {
            let x = lofty::flac::FlacFile::read_from(&mut f, opts)?;
            vorbis_custom(x.vorbis_comments(), &mut custom);
            x.into()
        }
        FileType::Vorbis => {
            let x = lofty::ogg::VorbisFile::read_from(&mut f, opts)?;
            vorbis_custom(Some(x.vorbis_comments()), &mut custom);
            x.into()
        }
        FileType::Opus => {
            let x = lofty::ogg::OpusFile::read_from(&mut f, opts)?;
            vorbis_custom(Some(x.vorbis_comments()), &mut custom);
            x.into()
        }
        FileType::Speex => {
            let x = lofty::ogg::SpeexFile::read_from(&mut f, opts)?;
            vorbis_custom(Some(x.vorbis_comments()), &mut custom);
            x.into()
        }
        FileType::Mp4 => {
            let x = lofty::mp4::Mp4File::read_from(&mut f, opts)?;
            mp4_custom(x.ilst(), &mut custom);
            x.into()
        }
        FileType::Ape => {
            let x = lofty::ape::ApeFile::read_from(&mut f, opts)?;
            id3v2_custom(x.id3v2(), &mut custom);
            ape_custom(x.ape(), &mut custom);
            x.into()
        }
        FileType::WavPack => {
            let x = lofty::wavpack::WavPackFile::read_from(&mut f, opts)?;
            ape_custom(x.ape(), &mut custom);
            x.into()
        }
        FileType::Mpc => {
            let x = lofty::musepack::MpcFile::read_from(&mut f, opts)?;
            id3v2_custom(x.id3v2(), &mut custom);
            ape_custom(x.ape(), &mut custom);
            x.into()
        }
        // AAC (ADTS) and custom resolvers: no user-defined tag harvesting.
        _ => probe.read()?,
    };
    Ok((tagged, custom))
}

/// Read one audio file's tags + duration. Returns the media and its
/// user-defined tags on success, `Err(SkipReason)` for a surfaced per-file
/// failure.
fn read_one(root: &Path, full: &Path) -> Result<(ScannedMedia, Vec<CustomTag>), SkipReason> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{Accessor, ItemKey};

    let rel_path = to_rel_path(root, full)
        .unwrap_or_else(|| full.to_string_lossy().replace('\\', "/"));

    let meta = std::fs::metadata(full).map_err(|e| SkipReason::Unreadable(e.to_string()))?;
    let size_bytes = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    let (tagged, custom_tags) =
        read_tagged(full).map_err(|e| SkipReason::Unreadable(e.to_string()))?;

    let duration_ms = tagged.properties().duration().as_millis() as u64;
    if duration_ms == 0 {
        return Err(SkipReason::ZeroDuration);
    }

    let (title, artist, album, year, genres) =
        match tagged.primary_tag().or_else(|| tagged.first_tag()) {
            Some(tag) => (
                tag.title().map(|s| s.to_string()),
                tag.artist().map(|s| s.to_string()),
                tag.album().map(|s| s.to_string()),
                tag.get_string(ItemKey::Year)
                    .or_else(|| tag.get_string(ItemKey::RecordingDate))
                    .and_then(parse_year),
                tag.genre().map(|s| s.to_string()).into_iter().collect(),
            ),
            None => (None, None, None, None, Vec::new()),
        };

    Ok((
        ScannedMedia {
            rel_path,
            title,
            artist,
            album,
            year,
            genres,
            duration_ms,
            size_bytes,
            mtime_ns,
        },
        custom_tags,
    ))
}

/// Walk `root` and read every audio-extension file. Per-file failures are
/// collected into the report rather than aborting the scan. A missing / non-
/// directory root is the one hard error.
pub fn scan_library(root: &Path) -> Result<ScanReport, ScanError> {
    if !root.is_dir() {
        return Err(ScanError::BadRoot(root.to_path_buf()));
    }

    let mut report = ScanReport::default();

    for entry in WalkDir::new(root).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                let path = err
                    .path()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|| "<unknown>".to_string());
                report.skipped.push(ScanSkip {
                    path,
                    reason: SkipReason::WalkError(err.to_string()),
                });
                continue;
            }
        };

        if !entry.file_type().is_file() || !is_audio_ext(entry.path()) {
            continue;
        }

        match read_one(root, entry.path()) {
            Ok((media, custom)) => {
                if !custom.is_empty() {
                    report.custom_tags.insert(media.rel_path.clone(), custom);
                }
                report.media.push(media);
            }
            Err(reason) => {
                let path = to_rel_path(root, entry.path())
                    .unwrap_or_else(|| entry.path().to_string_lossy().replace('\\', "/"));
                report.skipped.push(ScanSkip { path, reason });
            }
        }
    }

    // Stable output regardless of directory-walk order (nicer diffs, tests).
    report.media.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    report.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    /// Write a minimal, valid PCM WAV of `seconds` seconds (mono, 8-bit,
    /// 8000 Hz). Handcrafted so the scanner test exercises real `lofty`
    /// parsing without shipping a binary fixture.
    fn write_wav(path: &Path, seconds: u32) {
        let sample_rate: u32 = 8000;
        let channels: u16 = 1;
        let bits: u16 = 8;
        let n_samples = sample_rate * seconds;
        let data_len = n_samples; // 1 byte per sample (8-bit mono)
        let byte_rate = sample_rate * channels as u32 * (bits as u32 / 8);
        let block_align = channels * (bits / 8);

        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        f.write_all(&vec![128u8; data_len as usize]).unwrap(); // silence
    }

    #[test]
    fn bad_root_is_a_hard_error() {
        let missing = Path::new("X:/definitely/not/here-stationd-test");
        assert!(matches!(scan_library(missing), Err(ScanError::BadRoot(_))));
    }

    #[test]
    fn reads_a_real_wav_with_positive_duration() {
        let dir = tempfile::tempdir().unwrap();
        write_wav(&dir.path().join("jingle.wav"), 1);

        let report = scan_library(dir.path()).unwrap();
        assert_eq!(report.found(), 1);
        let m = &report.media[0];
        assert_eq!(m.rel_path, "jingle.wav");
        assert!(m.duration_ms > 0, "duration must be read and positive");
        // A bare WAV carries no tags: degraded but stored, not skipped.
        assert!(m.title.is_none() && m.artist.is_none());
    }

    #[test]
    fn rel_path_uses_forward_slashes_and_keeps_case() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("Jingles");
        std::fs::create_dir_all(&sub).unwrap();
        write_wav(&sub.join("ID-01.wav"), 1);

        let report = scan_library(dir.path()).unwrap();
        assert_eq!(report.found(), 1);
        assert_eq!(
            report.media[0].rel_path, "Jingles/ID-01.wav",
            "separator normalised to '/', case preserved"
        );
    }

    #[test]
    fn non_audio_files_are_ignored_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cover.jpg"), b"not audio").unwrap();
        std::fs::write(dir.path().join("README.txt"), b"hello").unwrap();
        write_wav(&dir.path().join("track.wav"), 1);

        let report = scan_library(dir.path()).unwrap();
        assert_eq!(report.found(), 1, "only the audio file counts");
        assert_eq!(
            report.skipped_count(),
            0,
            "non-audio extensions are not scan failures"
        );
    }

    #[test]
    fn audio_extension_but_garbage_is_surfaced_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        // Right extension, wrong content: this IS a surfaced per-file failure.
        std::fs::write(dir.path().join("broken.mp3"), b"definitely not an mp3").unwrap();

        let report = scan_library(dir.path()).unwrap();
        assert_eq!(report.found(), 0);
        assert_eq!(report.skipped_count(), 1);
        assert!(matches!(
            report.skipped[0].reason,
            SkipReason::Unreadable(_)
        ));
    }

    #[test]
    fn user_defined_id3v2_tags_are_harvested_with_multi_values() {
        use lofty::config::WriteOptions;
        use lofty::file::AudioFile;
        use lofty::id3::v2::Id3v2Tag;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talk.wav");
        write_wav(&path, 1);
        // WAV carries ID3v2 like mp3 (same TXXX frames), without shipping a
        // binary mp3 fixture.
        let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let mut wav =
            lofty::iff::wav::WavFile::read_from(&mut file, lofty::config::ParseOptions::new())
                .unwrap();
        let mut tag = Id3v2Tag::default();
        tag.insert_user_text("Type".into(), "talks\0news".into());
        tag.insert_user_text("Mood".into(), "calm".into());
        wav.set_id3v2(tag);
        drop(file);
        wav.save_to_path(&path, WriteOptions::default()).unwrap();

        let report = scan_library(dir.path()).unwrap();
        assert_eq!(report.found(), 1);
        let mut tags = report.custom_tags["talk.wav"].clone();
        tags.sort_by(|a, b| (&a.name, &a.value).cmp(&(&b.name, &b.value)));
        let flat: Vec<(&str, &str)> =
            tags.iter().map(|t| (t.name.as_str(), t.value.as_str())).collect();
        assert_eq!(flat, vec![("Mood", "calm"), ("Type", "news"), ("Type", "talks")]);
        assert!(report.media[0].genres.is_empty(), "custom tags are not genres by themselves");
    }

    #[test]
    fn media_without_user_defined_tags_have_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_wav(&dir.path().join("plain.wav"), 1);
        assert!(scan_library(dir.path()).unwrap().custom_tags.is_empty());
    }
}
