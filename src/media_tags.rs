//! Editing the STANDARD tags of a media file (title, artist, album, year,
//! genre) — stationd writes, like everything that touches the station's
//! files; a client (stationctl, the TUI) only sends the new values.
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

/// The standard fields, as the scanner reads them (primary tag).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StandardTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    /// The file's genre tag (one string). Genres added by plugins at scan
    /// time are not in the file and not edited here.
    pub genre: Option<String>,
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
    pub genre: FieldEdit<String>,
}

impl TagEdit {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.artist.is_none() && self.album.is_none() && self.year.is_none() && self.genre.is_none()
    }

    /// `tags` with this edit applied (what the file must read back as).
    pub fn apply_to(&self, tags: &StandardTags) -> StandardTags {
        let pick = |e: &FieldEdit<String>, old: &Option<String>| match e {
            None => old.clone(),
            Some(v) => v.clone(),
        };
        StandardTags {
            title: pick(&self.title, &tags.title),
            artist: pick(&self.artist, &tags.artist),
            album: pick(&self.album, &tags.album),
            year: match self.year {
                None => tags.year,
                Some(v) => v,
            },
            genre: pick(&self.genre, &tags.genre),
        }
    }
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

/// Read the standard fields exactly as the scanner does, plus the size.
pub fn read(full: &Path) -> Result<(StandardTags, u64), TagError> {
    let io = |e: &dyn std::fmt::Display| TagError::Io(format!("cannot read {}: {e}", full.display()));
    let size = std::fs::metadata(full).map_err(|e| io(&e))?.len();
    let tagged = lofty::read_from_path(full).map_err(|e| io(&e))?;
    let tags = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(tag) => StandardTags {
            title: tag.title().map(|s| s.to_string()),
            artist: tag.artist().map(|s| s.to_string()),
            album: tag.album().map(|s| s.to_string()),
            year: tag
                .get_string(ItemKey::Year)
                .or_else(|| tag.get_string(ItemKey::RecordingDate))
                .and_then(crate::media::parse_year),
            genre: tag.genre().map(|s| s.to_string()),
        },
        None => StandardTags::default(),
    };
    Ok((tags, size))
}

/// `tags:<hex>` fingerprint of the standard fields and the size.
pub fn revision(tags: &StandardTags, size: u64) -> String {
    let mut h = Sha256::new();
    for f in [&tags.title, &tags.artist, &tags.album, &tags.genre] {
        match f {
            Some(v) => {
                h.update([1]);
                h.update(v.as_bytes());
            }
            None => h.update([0]),
        }
        h.update([0xff]);
    }
    h.update(tags.year.unwrap_or(0).to_le_bytes());
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
    for (name, v) in [("title", &edit.title), ("artist", &edit.artist), ("album", &edit.album), ("genre", &edit.genre)] {
        if let Some(Some(s)) = v {
            if s.trim().is_empty() {
                return Err(TagError::BadValue(format!("{name}: an empty text removes the field, send it as absent")));
            }
        }
    }
    Ok(())
}

/// The file's current ID3v2 tag and its version, or a new 2.3 seeded from
/// ID3v1.
fn id3v2_of(full: &Path, ftype: FileType) -> Result<(Id3v2Tag, Id3v2Version), TagError> {
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
    let ftype = Probe::open(full)
        .map_err(|e| TagError::Io(format!("cannot read {}: {e}", full.display())))?
        .guess_file_type()
        .map_err(|e| TagError::Io(format!("cannot read {}: {e}", full.display())))?
        .file_type();
    if !matches!(ftype, Some(FileType::Mpeg | FileType::Wav | FileType::Aiff)) {
        return Err(TagError::Unsupported(rel_path.to_string()));
    }
    let ftype = ftype.expect("checked above");

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
    text(&mut tag, &edit.genre, |t, v| t.set_genre(v), |t| t.remove_genre());
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

    let wanted = edit.apply_to(&before);
    let (after, size) = read(full)?;
    if after != wanted {
        return Err(TagError::NotKept(format!(
            "the tags of {rel_path} read back differ from what was written (read {after:?}, wanted {wanted:?})"
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
        let edit = TagEdit { title: set("New"), artist: set("Air"), year: Some(Some(1998)), genre: set("ambient"), ..Default::default() };
        let (after, rev2) = write(&p, "a.wav", &rev, &edit).unwrap();
        assert_eq!(after.title.as_deref(), Some("New"));
        assert_eq!(after.artist.as_deref(), Some("Air"));
        assert_eq!(after.year, Some(1998));
        assert_eq!(after.genre.as_deref(), Some("ambient"));
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
