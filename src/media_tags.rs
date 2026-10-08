//! Editing the tags of a media file — title, artist, album, year, genres
//! (`TCON`, several values), BPM (`TBPM`) and user frames (`TXXX`: the ones
//! a plugin turns into genres, `tempo_manual`, `creation_manual`) — stationd
//! writes, like everything that touches the station's files; a client
//! (stationctl, the TUI) only sends the new values.
//!
//! Pure and blocking (lofty + std): the library actor runs it off the async
//! runtime and serialises it with the scans.
//!
//! Rules:
//! - Only ID3v2 containers are edited (mp3, and wav / aiff which carry ID3v2),
//!   through the CONCRETE `Id3v2Tag`: the generic lofty `Tag` drops the
//!   user-defined frames (`TXXX:Type`, `TXXX:Tags`…) and writing it back would
//!   erase them. Other formats are refused (`Unsupported`), never half-done.
//! - The existing ID3v2 version (2.3 / 2.4) is kept; a file without ID3v2
//!   gets a 2.3 tag seeded from its ID3v1 fields (same policy as
//!   `id3v1-vers-id3v2.py`), the ID3v1 tag itself is left as is.
//! - Revision = fingerprint of what the scanner reads (standard fields + file
//!   size): an edit states the revision it started from; if the file's tags
//!   changed meanwhile, nothing is written (conflict).
//! - The file's modification time is put back after the write (an edit of
//!   tags is not a new episode: `order_by = "mtime"` and the `unplayed_only`
//!   guard must not move), then the tags are read back and compared — a
//!   network filesystem may accept a write it did not keep.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::id3::v2::{Frame, FrameId, Id3v2Tag, Id3v2Version, TextInformationFrame};
use lofty::TextEncoding;
use lofty::probe::Probe;
use lofty::tag::items::Timestamp;
use lofty::tag::{Accessor, ItemKey, TagExt};
use sha2::{Digest, Sha256};

/// Manual tempo label: wins over the one derived from the BPM (the host puts
/// it in `media_meta.tempo` after the plugins, and the scan writes it back
/// into `TXXX:tempo`).
pub const TEMPO_MANUAL: &str = "tempo_manual";
/// Manual creation date (RFC 3339): wins over the one derived from the
/// comment, same path as [`TEMPO_MANUAL`].
pub const CREATION_MANUAL: &str = "creation_manual";

/// The tags of a file, as the scanner reads them (primary tag for the
/// standard fields; the ID3v2 user frames for `user`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StandardTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    /// Every value of the genre tag (`TCON`), in file order.
    pub genres: Vec<String>,
    /// `TBPM` (a decimal BPM is rounded).
    pub bpm: Option<u32>,
    /// ID3v2 user text frames (`TXXX`), by description as written; values
    /// split on NUL (ID3v2 multi-values).
    pub user: std::collections::BTreeMap<String, Vec<String>>,
}

impl StandardTags {
    /// Values of the user frame `name`, case-insensitively (as the scanner
    /// and the plugins match names).
    pub fn user_values(&self, name: &str) -> Vec<String> {
        self.user
            .iter()
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case(name.trim()))
            .flat_map(|(_, v)| v.iter().cloned())
            .collect()
    }
}

/// One field of an edit: `None` = unchanged, `Some(None)` = removed,
/// `Some(Some(v))` = set.
pub type FieldEdit<T> = Option<Option<T>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagEdit {
    pub title: FieldEdit<String>,
    pub artist: FieldEdit<String>,
    pub album: FieldEdit<String>,
    pub year: FieldEdit<u32>,
    /// `Some(list)` replaces every genre value; an empty list removes `TCON`.
    pub genres: Option<Vec<String>>,
    pub bpm: FieldEdit<u32>,
    /// User frames to replace (by name, case-insensitively); an empty list
    /// removes the frame.
    pub user: std::collections::BTreeMap<String, Vec<String>>,
}

impl TagEdit {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.year.is_none()
            && self.genres.is_none()
            && self.bpm.is_none()
            && self.user.is_empty()
    }

    /// Does `after` read back as this edit asked (edited fields only)?
    fn kept_in(&self, after: &StandardTags) -> bool {
        let same = |e: &FieldEdit<String>, v: &Option<String>| match e {
            None => true,
            Some(x) => x.as_deref().map(str::trim) == v.as_deref(),
        };
        same(&self.title, &after.title)
            && same(&self.artist, &after.artist)
            && same(&self.album, &after.album)
            && self.year.is_none_or(|y| y == after.year)
            && self.bpm.is_none_or(|b| b == after.bpm)
            && self.genres.as_ref().is_none_or(|g| clean(g) == after.genres)
            && self.user.iter().all(|(k, v)| clean(v) == after.user_values(k))
    }
}

/// Trimmed, non-empty, first spelling kept (case-insensitive duplicates
/// dropped).
fn clean(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values.iter().map(|v| v.trim()).filter(|v| !v.is_empty()) {
        if !out.iter().any(|o| o.to_lowercase() == v.to_lowercase()) {
            out.push(v.to_string());
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TagError {
    /// Not a path under the media root, or no such file.
    #[error("media `{0}` not found under the media root")]
    NotFound(String),
    /// A format this editor does not write (yet).
    #[error("editing tags of `{0}` is not supported: only ID3v2 files (mp3, wav, aiff) for now")]
    Unsupported(String),
    /// Invalid value (a year of 0 or beyond 9999, an empty text…).
    #[error("{0}")]
    BadValue(String),
    /// The file's tags changed since the given revision: nothing written.
    #[error("the tags of `{path}` changed since they were read (now at {current}): nothing written")]
    Conflict { path: String, current: String },
    /// Reading or writing failed.
    #[error("{0}")]
    Io(String),
    /// Written, but the file does not read back as intended (the file may
    /// have been partly changed: the index must be refreshed from it).
    #[error("{0}")]
    NotKept(String),
}

impl TagError {
    /// The file may have changed: its index row must follow it.
    pub fn file_touched(&self) -> bool {
        matches!(self, TagError::NotKept(_))
    }
}

/// The file of `rel_path` under `root`: a relative path without `..`, that
/// exists as a file. Anything else is `NotFound` (never a write outside the
/// media root).
pub fn resolve(root: &Path, rel_path: &str) -> Result<PathBuf, TagError> {
    let rel = Path::new(rel_path);
    let clean = !rel_path.trim().is_empty()
        && rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    let full = root.join(rel);
    if !clean || !full.is_file() {
        return Err(TagError::NotFound(rel_path.to_string()));
    }
    Ok(full)
}

/// Read the tags exactly as the scanner does, plus the size.
pub fn read(full: &Path) -> Result<(StandardTags, u64), TagError> {
    let io = |e: &dyn std::fmt::Display| TagError::Io(format!("cannot read {}: {e}", full.display()));
    let size = std::fs::metadata(full).map_err(|e| io(&e))?.len();
    let tagged = lofty::read_from_path(full).map_err(|e| io(&e))?;
    let mut tags = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(tag) => StandardTags {
            title: tag.title().map(|s| s.to_string()),
            artist: tag.artist().map(|s| s.to_string()),
            album: tag.album().map(|s| s.to_string()),
            year: tag
                .get_string(ItemKey::Year)
                .or_else(|| tag.get_string(ItemKey::RecordingDate))
                .and_then(crate::media::parse_year),
            genres: crate::media::genre_values(tag),
            bpm: tag
                .get_string(ItemKey::IntegerBpm)
                .or_else(|| tag.get_string(ItemKey::Bpm))
                .and_then(|b| b.trim().parse::<f64>().ok())
                .filter(|b| b.is_finite() && *b > 0.0)
                .map(|b| b.round() as u32),
            user: Default::default(),
        },
        None => StandardTags::default(),
    };
    if let Some(ftype) = id3v2_type(full) {
        if let Ok((t, _)) = id3v2_of(full, ftype) {
            for frame in &t {
                if let Frame::UserText(u) = frame {
                    let values: Vec<String> =
                        u.content.split('\0').map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect();
                    tags.user.entry(u.description.to_string()).or_default().extend(values);
                }
            }
        }
    }
    Ok((tags, size))
}

/// The ID3v2 container type of `full`, if it is one this editor writes.
fn id3v2_type(full: &Path) -> Option<FileType> {
    let t = Probe::open(full).ok()?.guess_file_type().ok()?.file_type()?;
    matches!(t, FileType::Mpeg | FileType::Wav | FileType::Aiff).then_some(t)
}

/// `tags:<hex>` fingerprint of every field read and the size.
pub fn revision(tags: &StandardTags, size: u64) -> String {
    let mut h = Sha256::new();
    let mut put = |v: Option<&str>| {
        match v {
            Some(v) => {
                h.update([1]);
                h.update(v.as_bytes());
            }
            None => h.update([0]),
        }
        h.update([0xff]);
    };
    for f in [&tags.title, &tags.artist, &tags.album] {
        put(f.as_deref());
    }
    for g in &tags.genres {
        put(Some(g));
    }
    put(Some("|"));
    for (k, vs) in &tags.user {
        put(Some(k));
        for v in vs {
            put(Some(v));
        }
    }
    h.update(tags.year.unwrap_or(0).to_le_bytes());
    h.update(tags.bpm.unwrap_or(0).to_le_bytes());
    h.update(size.to_le_bytes());
    let digest = h.finalize();
    let mut out = String::from("tags:");
    for b in &digest[..12] {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn check(edit: &TagEdit) -> Result<(), TagError> {
    if edit.is_empty() {
        return Err(TagError::BadValue("nothing to change".into()));
    }
    if let Some(Some(y)) = edit.year {
        if !(1..=9999).contains(&y) {
            return Err(TagError::BadValue(format!("year {y} is out of range (1-9999)")));
        }
    }
    if let Some(Some(b)) = edit.bpm {
        if !(1..=999).contains(&b) {
            return Err(TagError::BadValue(format!("BPM {b} is out of range (1-999)")));
        }
    }
    for (name, v) in [("title", &edit.title), ("artist", &edit.artist), ("album", &edit.album)] {
        if let Some(Some(s)) = v {
            if s.trim().is_empty() {
                return Err(TagError::BadValue(format!("{name}: an empty text removes the field, send it as absent")));
            }
        }
    }
    for name in edit.user.keys() {
        if name.trim().eq_ignore_ascii_case(crate::media_identity::UUID_TAG) {
            return Err(TagError::BadValue("STATIOND_UUID is a reserved, immutable media identity".into()));
        }
        if name.trim().is_empty() {
            return Err(TagError::BadValue("a user frame needs a name".into()));
        }
    }
    if let Some(v) = edit.user.iter().find(|(k, _)| k.eq_ignore_ascii_case(CREATION_MANUAL)).and_then(|(_, v)| clean(v).into_iter().next()) {
        if v.parse::<jiff::Timestamp>().is_err() {
            return Err(TagError::BadValue(format!("{CREATION_MANUAL}: `{v}` is not an RFC 3339 date (2026-06-14T06:36:48Z)")));
        }
    }
    Ok(())
}

/// The file's current ID3v2 tag and its version, or a new 2.3 seeded from
/// ID3v1.
pub(crate) fn id3v2_of(full: &Path, ftype: FileType) -> Result<(Id3v2Tag, Id3v2Version), TagError> {
    let io = |e: &dyn std::fmt::Display| TagError::Io(format!("cannot read {}: {e}", full.display()));
    let mut f = std::fs::File::open(full).map_err(|e| io(&e))?;
    let opts = ParseOptions::new();
    let (v2, v1) = match ftype {
        FileType::Mpeg => {
            let x = lofty::mpeg::MpegFile::read_from(&mut f, opts).map_err(|e| io(&e))?;
            (x.id3v2().cloned(), x.id3v1().cloned())
        }
        FileType::Wav => {
            let x = lofty::iff::wav::WavFile::read_from(&mut f, opts).map_err(|e| io(&e))?;
            (x.id3v2().cloned(), None)
        }
        FileType::Aiff => {
            let x = lofty::iff::aiff::AiffFile::read_from(&mut f, opts).map_err(|e| io(&e))?;
            (x.id3v2().cloned(), None)
        }
        _ => unreachable!("checked by the caller"),
    };
    Ok(match v2 {
        Some(t) => {
            let v = t.original_version();
            (t, v)
        }
        None => {
            let mut t = Id3v2Tag::default();
            if let Some(v1) = v1 {
                if let Some(x) = v1.title() {
                    t.set_title(x.to_string());
                }
                if let Some(x) = v1.artist() {
                    t.set_artist(x.to_string());
                }
                if let Some(x) = v1.album() {
                    t.set_album(x.to_string());
                }
                if let Some(x) = v1.genre() {
                    t.set_genre(x.to_string());
                }
                if let Some(y) = v1.year {
                    t.insert(Frame::Text(TextInformationFrame::new(
                        FrameId::Valid(Cow::Borrowed("TYER")),
                        TextEncoding::UTF16,
                        y.to_string(),
                    )));
                }
            }
            (t, Id3v2Version::V3)
        }
    })
}

/// Write the edit into `full` if its tags are still at `expected` (empty =
/// no check). Returns the tags read back and the new revision.
pub fn write(full: &Path, rel_path: &str, expected: &str, edit: &TagEdit) -> Result<(StandardTags, String), TagError> {
    check(edit)?;
    let Some(ftype) = id3v2_type(full) else {
        return Err(TagError::Unsupported(rel_path.to_string()));
    };

    let (before, size) = read(full)?;
    let current = revision(&before, size);
    if !expected.trim().is_empty() && expected.trim() != current {
        return Err(TagError::Conflict { path: rel_path.to_string(), current });
    }
    let mtime = std::fs::metadata(full).and_then(|m| m.modified()).ok();

    let (mut tag, version) = id3v2_of(full, ftype)?;
    let text = |tag: &mut Id3v2Tag, e: &FieldEdit<String>, set: fn(&mut Id3v2Tag, String), remove: fn(&mut Id3v2Tag)| {
        match e {
            None => {}
            Some(None) => remove(tag),
            Some(Some(v)) => set(tag, v.trim().to_string()),
        }
    };
    text(&mut tag, &edit.title, |t, v| t.set_title(v), |t| t.remove_title());
    text(&mut tag, &edit.artist, |t, v| t.set_artist(v), |t| t.remove_artist());
    text(&mut tag, &edit.album, |t, v| t.set_album(v), |t| t.remove_album());
    // Genres: one TCON frame, values NUL-separated (ID3v2.4 multi-values;
    // lofty writes the 2.3 form and reads both back as separate genres).
    if let Some(genres) = &edit.genres {
        tag.remove_genre();
        let values = clean(genres);
        if !values.is_empty() {
            tag.insert(Frame::Text(TextInformationFrame::new(
                FrameId::Valid(Cow::Borrowed("TCON")),
                TextEncoding::UTF16,
                values.join("\0"),
            )));
        }
    }
    let tbpm = FrameId::Valid(Cow::Borrowed("TBPM"));
    match edit.bpm {
        None => {}
        Some(None) => {
            let _ = tag.remove(&tbpm).count();
        }
        Some(Some(b)) => {
            let _ = tag.remove(&tbpm).count();
            tag.insert(Frame::Text(TextInformationFrame::new(tbpm.clone(), TextEncoding::UTF16, b.to_string())));
        }
    }
    for (name, values) in &edit.user {
        // Every case variant of the name goes: one frame remains.
        let variants: Vec<String> = (&tag)
            .into_iter()
            .filter_map(|f| match f {
                Frame::UserText(u) if u.description.trim().eq_ignore_ascii_case(name.trim()) => Some(u.description.to_string()),
                _ => None,
            })
            .collect();
        for v in variants {
            tag.remove_user_text(&v);
        }
        let values = clean(values);
        if !values.is_empty() {
            tag.insert_user_text(name.trim().to_string(), values.join("\0"));
        }
    }
    // The year, in the frame of the tag's version: TYER for 2.3, TDRC for
    // 2.4 (lofty drops a TDRC on a 2.3 write). Both are cleared first so a
    // single year remains.
    let tyer = FrameId::Valid(Cow::Borrowed("TYER"));
    if edit.year.is_some() {
        let _ = tag.remove(&tyer).count();
        tag.remove_date();
    }
    if let Some(Some(y)) = edit.year {
        if version == Id3v2Version::V3 {
            tag.insert(Frame::Text(TextInformationFrame::new(tyer.clone(), TextEncoding::UTF16, y.to_string())));
        } else {
            tag.set_date(Timestamp { year: y as u16, ..Timestamp::default() });
        }
    }

    let opts = WriteOptions::default().use_id3v23(version == Id3v2Version::V3);
    tag.save_to_path(full, opts)
        .map_err(|e| TagError::Io(format!("cannot write the tags of {rel_path}: {e}")))?;
    if let Some(t) = mtime {
        if let Ok(f) = std::fs::File::options().write(true).open(full) {
            // Best effort: a filesystem refusing it only makes the file look new.
            let _ = f.set_modified(t);
        }
    }

    let (after, size) = read(full)?;
    if !edit.kept_in(&after) {
        return Err(TagError::NotKept(format!(
            "the tags of {rel_path} read back differ from what was written (read {after:?}, wanted {edit:?})"
        )));
    }
    Ok((after.clone(), revision(&after, size)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A minimal PCM WAV (see media.rs): lofty parses it for real, and WAV
    /// carries ID3v2 like mp3.
    fn wav(path: &Path) {
        let n: u32 = 8000;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + n).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&8000u32.to_le_bytes()).unwrap();
        f.write_all(&8000u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&8u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&n.to_le_bytes()).unwrap();
        f.write_all(&vec![128u8; n as usize]).unwrap();
    }

    fn set(v: &str) -> FieldEdit<String> {
        Some(Some(v.to_string()))
    }

    #[test]
    fn writes_reads_back_and_keeps_user_frames_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        wav(&p);
        // A user frame (TXXX:Type) that the generic tag would drop.
        {
            let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
            let mut w = lofty::iff::wav::WavFile::read_from(&mut file, ParseOptions::new()).unwrap();
            let mut t = Id3v2Tag::default();
            t.insert_user_text("Type".into(), "talks".into());
            t.set_title("Old".into());
            w.set_id3v2(t);
            drop(file);
            w.save_to_path(&p, WriteOptions::default()).unwrap();
        }
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();

        let (tags, size) = read(&p).unwrap();
        assert_eq!(tags.title.as_deref(), Some("Old"));
        let rev = revision(&tags, size);
        let edit = TagEdit { title: set("New"), artist: set("Air"), year: Some(Some(1998)), genres: Some(vec!["ambient".into()]), ..Default::default() };
        let (after, rev2) = write(&p, "a.wav", &rev, &edit).unwrap();
        assert_eq!(after.title.as_deref(), Some("New"));
        assert_eq!(after.artist.as_deref(), Some("Air"));
        assert_eq!(after.year, Some(1998));
        assert_eq!(after.genres, ["ambient"]);
        assert_ne!(rev, rev2);
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), old, "mtime put back");

        // The user frame survived.
        let report = crate::media::scan_library(dir.path()).unwrap();
        let custom = &report.custom_tags["a.wav"];
        assert!(custom.iter().any(|t| t.name == "Type" && t.value == "talks"), "{custom:?}");
        assert_eq!(report.media[0].title.as_deref(), Some("New"), "the scanner reads the edit");

        // Remove a field; an old revision is a conflict and writes nothing.
        let (after, _) = write(&p, "a.wav", &rev2, &TagEdit { artist: Some(None), ..Default::default() }).unwrap();
        assert_eq!(after.artist, None);
        let r = write(&p, "a.wav", &rev2, &TagEdit { title: set("X"), ..Default::default() });
        assert!(matches!(r, Err(TagError::Conflict { .. })), "{r:?}");
        assert_eq!(read(&p).unwrap().0.title.as_deref(), Some("New"));
    }

    #[test]
    fn several_genres_user_frames_bpm_and_manual_values_round_trip() {
        for v23 in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("a.wav");
            wav(&p);
            {
                let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&p).unwrap();
                let mut w = lofty::iff::wav::WavFile::read_from(&mut file, ParseOptions::new()).unwrap();
                let mut t = Id3v2Tag::default();
                t.insert_user_text("type".into(), "talks".into());
                t.insert_user_text("Comment".into(), "keep me".into());
                w.set_id3v2(t);
                drop(file);
                w.save_to_path(&p, WriteOptions::default().use_id3v23(v23)).unwrap();
            }
            let (before, size) = read(&p).unwrap();
            assert_eq!(before.user_values("Type"), ["talks"], "noms sans casse");
            let mut user = std::collections::BTreeMap::new();
            user.insert("Type".to_string(), vec!["song".to_string(), "news".to_string(), "SONG".to_string()]);
            user.insert(TEMPO_MANUAL.to_string(), vec!["lent".to_string()]);
            user.insert(CREATION_MANUAL.to_string(), vec!["2026-06-14T06:36:48Z".to_string()]);
            let edit = TagEdit {
                genres: Some(vec!["électro".into(), " house ".into(), "Électro".into()]),
                bpm: Some(Some(124)),
                user,
                ..Default::default()
            };
            let (after, _) = write(&p, "a.wav", &revision(&before, size), &edit).unwrap();
            assert_eq!(after.genres, ["électro", "house"], "v2.3 = {v23}");
            assert_eq!(after.bpm, Some(124));
            assert_eq!(after.user_values("Type"), ["song", "news"]);
            assert_eq!(after.user.keys().filter(|k| k.eq_ignore_ascii_case("type")).count(), 1, "une seule trame");
            assert_eq!(after.user_values(TEMPO_MANUAL), ["lent"]);
            assert_eq!(after.user_values("Comment"), ["keep me"], "trame non touchée gardée");
            // The scanner sees every genre.
            let report = crate::media::scan_library(dir.path()).unwrap();
            assert_eq!(report.media[0].genres, ["électro", "house"]);
            // Removal: empty lists.
            let mut user = std::collections::BTreeMap::new();
            user.insert("type".to_string(), vec![]);
            user.insert(TEMPO_MANUAL.to_string(), vec![String::new()]);
            let (after, _) = write(&p, "a.wav", "", &TagEdit { genres: Some(vec![]), bpm: Some(None), user, ..Default::default() }).unwrap();
            assert!(after.genres.is_empty() && after.bpm.is_none());
            assert!(after.user_values("Type").is_empty() && after.user_values(TEMPO_MANUAL).is_empty());
        }
        // A creation that is not RFC 3339 is refused before anything is written.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.wav");
        wav(&p);
        let mut user = std::collections::BTreeMap::new();
        user.insert(CREATION_MANUAL.to_string(), vec!["14/06/2026".to_string()]);
        assert!(matches!(write(&p, "b.wav", "", &TagEdit { user, ..Default::default() }), Err(TagError::BadValue(_))));
        assert!(matches!(write(&p, "b.wav", "", &TagEdit { bpm: Some(Some(0)), ..Default::default() }), Err(TagError::BadValue(_))));
    }

    #[test]
    fn refuses_what_it_cannot_do_safely() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        wav(&p);
        assert!(matches!(resolve(dir.path(), "../a.wav"), Err(TagError::NotFound(_))));
        assert!(matches!(resolve(dir.path(), "nope.wav"), Err(TagError::NotFound(_))));
        assert_eq!(resolve(dir.path(), "a.wav").unwrap(), p);
        let bad_year = TagEdit { year: Some(Some(0)), ..Default::default() };
        assert!(matches!(write(&p, "a.wav", "", &bad_year), Err(TagError::BadValue(_))));
        assert!(matches!(write(&p, "a.wav", "", &TagEdit::default()), Err(TagError::BadValue(_))));
        let blank = TagEdit { title: set("  "), ..Default::default() };
        assert!(matches!(write(&p, "a.wav", "", &blank), Err(TagError::BadValue(_))));
        // Another format: refused, file untouched.
        let flac = dir.path().join("b.flac");
        std::fs::write(&flac, b"fLaC\0\0\0\0").unwrap();
        let r = write(&flac, "b.flac", "", &TagEdit { title: set("x"), ..Default::default() });
        assert!(matches!(r, Err(TagError::Unsupported(_)) | Err(TagError::Io(_))), "{r:?}");
    }
}
