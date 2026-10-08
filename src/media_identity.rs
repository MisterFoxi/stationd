//! Durable URI/UUID registry and UUID tags, independent of rebuildable media views.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::TagExt;
use sqlx::{SqliteConnection, SqlitePool};

use crate::media::{CustomTag, ScanReport};
use crate::media_tags::TagError;

pub const UUID_TAG: &str = "STATIOND_UUID";

fn io(e: impl std::fmt::Display) -> TagError {
    TagError::Io(e.to_string())
}

/// Exactly one, non-nil UUID. A copied tag is handled separately by the registry.
pub fn from_tags(tags: &[CustomTag]) -> Result<Option<String>, TagError> {
    let values: Vec<_> = tags
        .iter()
        .filter(|t| t.name.trim().eq_ignore_ascii_case(UUID_TAG))
        .collect();
    if values.is_empty() {
        return Ok(None);
    }
    if values.len() != 1 {
        return Err(io(format!("{UUID_TAG}: multiple UUID values")));
    }
    let id = uuid::Uuid::parse_str(values[0].value.trim())
        .map_err(|_| io(format!("{UUID_TAG}: invalid UUID")))?;
    if id.is_nil() {
        return Err(io(format!("{UUID_TAG}: nil UUID")));
    }
    Ok(Some(id.to_string()))
}

/// Used by legacy path-facing APIs. Registry survives deletion of the media cache.
pub async fn ensure(pool: &SqlitePool, uri: &str) -> Result<String, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    ensure_on(&mut conn, uri).await
}

pub async fn ensure_on(conn: &mut SqliteConnection, uri: &str) -> Result<String, sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO media_identity (uri) VALUES (?1)")
        .bind(uri)
        .execute(&mut *conn)
        .await?;
    sqlx::query_scalar("SELECT uuid FROM media_identity WHERE uri = ?1")
        .bind(uri)
        .fetch_one(&mut *conn)
        .await
}

pub async fn id_for_uri(pool: &SqlitePool, uri: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT uuid FROM media_identity WHERE uri = ?1")
        .bind(uri)
        .fetch_optional(pool)
        .await
}

pub async fn uri_for_id(pool: &SqlitePool, id: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT uri FROM media_identity WHERE uuid = ?1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Update path caches when a tagged file was moved outside StationD. History
/// keeps the URI at selection time, but continues to refer to this UUID.
async fn reconcile(
    pool: &SqlitePool,
    ids: &BTreeMap<String, String>,
    confirmed: &HashSet<String>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *tx)
        .await?;
    for (uri, id) in ids {
        let local: Option<(String, bool)> =
            sqlx::query_as("SELECT uuid, confirmed FROM media_identity WHERE uri = ?1")
                .bind(uri)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((old, was_confirmed)) = local.filter(|(old, _)| old != id) {
            if was_confirmed {
                return Err(sqlx::Error::Protocol(format!(
                    "{uri}: confirmed UUID changed"
                )));
            }
            // Free the locator while keeping the provisional row until references
            // have been rebound (the immutability trigger checks that row).
            sqlx::query("UPDATE media_identity SET uri = ?1 WHERE uuid = ?2")
                .bind(format!(".stationd-provisional/{old}"))
                .bind(&old)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT OR IGNORE INTO media_identity (uuid, uri) VALUES (?1, ?2)")
                .bind(id)
                .bind(uri)
                .execute(&mut *tx)
                .await?;
            for table in [
                "media",
                "media_genre",
                "media_meta",
                "media_tag",
                "media_analysis",
                "broadcast_log",
                "episode_play",
                "queue_entry",
                "playlist_cursor",
                "shuffle_member",
                "shuffle_pick",
            ] {
                sqlx::query(&format!(
                    "UPDATE {table} SET media_uuid = ?1 WHERE media_uuid = ?2"
                ))
                .bind(id)
                .bind(&old)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query("DELETE FROM media_identity WHERE uuid = ?1")
                .bind(&old)
                .execute(&mut *tx)
                .await?;
        }
        let previous: Option<String> =
            sqlx::query_scalar("SELECT uri FROM media_identity WHERE uuid = ?1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(old) = previous.filter(|p| p != uri) {
            for table in [
                "media",
                "media_genre",
                "media_meta",
                "media_tag",
                "media_analysis",
                "episode_play",
                "queue_entry",
            ] {
                sqlx::query(&format!(
                    "UPDATE {table} SET rel_path = ?1 WHERE media_uuid = ?2"
                ))
                .bind(uri)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query("UPDATE playlist_cursor SET last_rel_path = ?1 WHERE media_uuid = ?2")
                .bind(uri)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE media_identity SET uri = ?1 WHERE uuid = ?2")
                .bind(uri)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tracing::info!(media_uuid = %id, from = %old, to = %uri, "media identity relocated");
        } else {
            sqlx::query(
                "INSERT INTO media_identity (uuid, uri) VALUES (?1, ?2)
                        ON CONFLICT(uuid) DO UPDATE SET uri = excluded.uri",
            )
            .bind(id)
            .bind(uri)
            .execute(&mut *tx)
            .await?;
        }
        if confirmed.contains(id) {
            sqlx::query("UPDATE media_identity SET confirmed = 1 WHERE uuid = ?1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Validate the entire scan before writing a tag. A duplicate UUID or a
/// URI/UUID contradiction fails loudly, without guessing which file is original.
pub async fn prepare(
    pool: &SqlitePool,
    root: &Path,
    report: &mut ScanReport,
) -> Result<crate::scan_writeback::Originals, crate::library_actor::LibraryError> {
    let _lock = crate::library_lock::acquire(root).await?;
    // Reports made before waiting for another station are stale. Read again
    // under the shared lock, including its newly installed identity tag.
    let task_root = root.to_path_buf();
    let paths: Vec<_> = report.media.iter().map(|m| m.rel_path.clone()).collect();
    let fresh = tokio::task::spawn_blocking(move || {
        let mut fresh = ScanReport::default();
        for uri in paths {
            let file = crate::media::scan_file(&task_root, &task_root.join(&uri))
                .map_err(|e| io(format!("{uri}: cannot refresh scan: {e:?}")))?;
            fresh.media.extend(file.media);
            fresh.custom_tags.extend(file.custom_tags);
            fresh.metadata.extend(file.metadata);
        }
        Ok::<_, TagError>(fresh)
    })
    .await
    .map_err(|e| crate::library_actor::LibraryError::Join(e.to_string()))??;
    report.media = fresh.media;
    report.custom_tags = fresh.custom_tags;
    report.metadata = fresh.metadata;
    prepare_locked(pool, root, report, &_lock).await
}

/// Caller holds the shared library lock from reading through all file writes.
pub(crate) async fn prepare_locked(
    pool: &SqlitePool,
    root: &Path,
    report: &mut ScanReport,
    lock: &std::sync::Arc<crate::library_lock::LibraryLock>,
) -> Result<crate::scan_writeback::Originals, crate::library_actor::LibraryError> {
    prepare_impl(pool, root, report, Some(lock)).await
}

/// Replica: adopt the master's tags and update only the local SQLite database.
pub(crate) async fn prepare_readonly(
    pool: &SqlitePool,
    root: &Path,
    report: &mut ScanReport,
) -> Result<crate::scan_writeback::Originals, crate::library_actor::LibraryError> {
    prepare_impl(pool, root, report, None).await
}

async fn prepare_impl(
    pool: &SqlitePool,
    root: &Path,
    report: &mut ScanReport,
    lock: Option<&std::sync::Arc<crate::library_lock::LibraryLock>>,
) -> Result<crate::scan_writeback::Originals, crate::library_actor::LibraryError> {
    let write_files = lock.is_some();
    let rows: Vec<(String, String, bool)> =
        sqlx::query_as("SELECT uuid, uri, confirmed FROM media_identity")
            .fetch_all(pool)
            .await?;
    let by_uri: HashMap<_, _> = rows
        .iter()
        .map(|(id, uri, confirmed)| (uri.clone(), (id.clone(), *confirmed)))
        .collect();
    let by_id: HashMap<_, _> = rows.into_iter().map(|(id, uri, _)| (id, uri)).collect();
    let mut seen = HashSet::new();
    let mut ids = BTreeMap::new();
    for m in &report.media {
        let tagged = from_tags(
            report
                .custom_tags
                .get(&m.rel_path)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )
        .map_err(|e| io(format!("{}: {e}", m.rel_path)))?;
        if !write_files && tagged.is_none() {
            return Err(io(format!("{}: missing {UUID_TAG}; scan this media on the master before scanning a read-only replica", m.rel_path)).into());
        }
        let known = by_uri.get(&m.rel_path);
        let id = match (tagged, known) {
            (Some(id), Some((old, true))) if &id != old => {
                return Err(io(format!(
                    "{}: UUID tag {id} conflicts with registered UUID {old}",
                    m.rel_path
                ))
                .into());
            }
            (Some(id), _) => id,
            (None, Some((id, _))) => id.clone(),
            (None, None) => uuid::Uuid::new_v4().to_string(),
        };
        if !seen.insert(id.clone()) {
            return Err(io(format!("duplicate media UUID {id} in scan ({}); copied files require an explicit new identity", m.rel_path)).into());
        }
        if let Some(old) = by_id.get(&id).filter(|old| *old != &m.rel_path) {
            if root.join(old).is_file() {
                return Err(io(format!(
                    "UUID {id} exists at both {old} and {}; refusing to merge files",
                    m.rel_path
                ))
                .into());
            }
        }
        ids.insert(m.rel_path.clone(), id);
    }
    let task_root = root.to_path_buf();
    let mut written = report.clone();
    let plan = ids.clone();
    let task_lock = lock.cloned();
    let (updated, originals, confirmed) = tokio::task::spawn_blocking(move || {
        let _lock = task_lock;
        let mut originals = crate::scan_writeback::Originals::new();
        let mut confirmed = HashSet::new();
        for m in &mut written.media {
            let id = &plan[&m.rel_path];
            let old = (m.size_bytes, m.mtime_ns);
            if write_files && write_tag(&task_root, &m.rel_path, id, old)? {
                originals.insert(m.rel_path.clone(), old);
                let meta = std::fs::metadata(task_root.join(&m.rel_path)).map_err(io)?;
                m.size_bytes = meta.len();
                m.mtime_ns = modified_ns(&meta);
            }
        }
        for m in &mut written.media {
            let (_, tags, meta) = crate::media::read_tagged_snapshot(&task_root.join(&m.rel_path)).map_err(io)?;
            let tagged = from_tags(&tags)?;
            if tagged.as_deref() == Some(plan[&m.rel_path].as_str()) {
                confirmed.insert(plan[&m.rel_path].clone());
            } else if tagged.is_some() || !write_files || originals.contains_key(&m.rel_path) {
                return Err(io(format!("{}: UUID changed during scan", m.rel_path)));
            }
            if originals.contains_key(&m.rel_path) {
                // A path stat immediately after rename can still be cached on
                // NFS. Keep the fingerprint from the verified, reopened file.
                m.size_bytes = meta.len();
                m.mtime_ns = modified_ns(&meta);
            }
        }
        // Identity is core-owned, never editable or derived by a plugin.
        for tags in written.custom_tags.values_mut() {
            tags.retain(|t| !t.name.trim().eq_ignore_ascii_case(UUID_TAG));
        }
        Ok::<_, TagError>((written, originals, confirmed))
    })
    .await
    .map_err(|e| crate::library_actor::LibraryError::Join(e.to_string()))??;
    reconcile(pool, &ids, &confirmed).await?;
    *report = updated;
    Ok(originals)
}

fn modified_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|t| t.as_nanos() as i64)
        .unwrap_or(0)
}

struct Staged(std::path::PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Preserve concrete tag containers (generic tags discard unknown user frames).
fn save_tag(path: &Path, ty: FileType, id: &str) -> Result<(), TagError> {
    let opts = ParseOptions::new();
    let mut file = std::fs::File::open(path).map_err(io)?;
    let mut write_opts = WriteOptions::default();
    match ty {
        FileType::Mpeg | FileType::Wav | FileType::Aiff => {
            let parsed = Probe::open(path)
                .map_err(io)?
                .guess_file_type()
                .map_err(io)?
                .read()
                .map_err(io)?;
            let (mut tag, version) = crate::media_tags::id3v2_of(path, ty)?;
            // Adding ID3 changes the primary tag for WAV/AIFF. Seed its standard
            // fields from INFO/NAME (or ID3v1), so the scanner sees the same tags.
            if parsed.tag(lofty::tag::TagType::Id3v2).is_none() {
                if let Some(primary) = parsed.primary_tag().or_else(|| parsed.first_tag()) {
                    let mut seeded: lofty::id3::v2::Id3v2Tag = primary.clone().into();
                    for frame in &tag {
                        seeded.insert(frame.clone());
                    }
                    tag = seeded;
                }
            }
            tag.insert_user_text(UUID_TAG.into(), id.into());
            tag.save_to_path(
                path,
                write_opts.use_id3v23(version == lofty::id3::v2::Id3v2Version::V3),
            )
            .map_err(io)?;
        }
        FileType::Flac => {
            let parsed = lofty::flac::FlacFile::read_from(&mut file, opts).map_err(io)?;
            let mut tag = parsed.vorbis_comments().cloned().unwrap_or_default();
            tag.insert(UUID_TAG.into(), id.into());
            tag.save_to_path(path, write_opts).map_err(io)?;
        }
        FileType::Vorbis | FileType::Opus | FileType::Speex => {
            let mut tag = match ty {
                FileType::Vorbis => lofty::ogg::VorbisFile::read_from(&mut file, opts)
                    .map_err(io)?
                    .vorbis_comments()
                    .clone(),
                FileType::Opus => lofty::ogg::OpusFile::read_from(&mut file, opts)
                    .map_err(io)?
                    .vorbis_comments()
                    .clone(),
                _ => lofty::ogg::SpeexFile::read_from(&mut file, opts)
                    .map_err(io)?
                    .vorbis_comments()
                    .clone(),
            };
            tag.insert(UUID_TAG.into(), id.into());
            tag.save_to_path(path, write_opts).map_err(io)?;
        }
        FileType::Mp4 => {
            use lofty::mp4::{Atom, AtomData, AtomIdent};
            let parsed = lofty::mp4::Mp4File::read_from(&mut file, opts).map_err(io)?;
            let mut tag = parsed.ilst().cloned().unwrap_or_default();
            tag.insert(Atom::new(
                AtomIdent::Freeform {
                    mean: "com.stationd".into(),
                    name: UUID_TAG.into(),
                },
                AtomData::UTF8(id.into()),
            ));
            tag.save_to_path(path, write_opts).map_err(io)?;
        }
        FileType::Ape | FileType::WavPack | FileType::Mpc => {
            let mut tag = match ty {
                FileType::Ape => lofty::ape::ApeFile::read_from(&mut file, opts)
                    .map_err(io)?
                    .ape()
                    .cloned()
                    .unwrap_or_default(),
                FileType::WavPack => lofty::wavpack::WavPackFile::read_from(&mut file, opts)
                    .map_err(io)?
                    .ape()
                    .cloned()
                    .unwrap_or_default(),
                _ => lofty::musepack::MpcFile::read_from(&mut file, opts)
                    .map_err(io)?
                    .ape()
                    .cloned()
                    .unwrap_or_default(),
            };
            tag.insert(
                lofty::ape::ApeItem::new(UUID_TAG.into(), lofty::tag::ItemValue::Text(id.into()))
                    .map_err(io)?,
            );
            tag.save_to_path(path, write_opts).map_err(io)?;
        }
        _ => return Err(TagError::Unsupported(path.display().to_string())),
    }
    Ok(())
}

/// Verified staged replacement, same directory, same mtime/permissions.
/// Unsupported containers retain their UUID in SQLite and emit a diagnostic.
fn write_tag(root: &Path, uri: &str, id: &str, expected: (u64, i64)) -> Result<bool, TagError> {
    let full = crate::media_tags::resolve(root, uri)?;
    let (_, tags) = crate::media::read_tagged(&full).map_err(io)?;
    if from_tags(&tags)?.as_deref() == Some(id) {
        return Ok(false);
    }
    if from_tags(&tags)?.is_some() {
        return Err(io(format!("{uri}: UUID changed during scan")));
    }
    let ty = Probe::open(&full)
        .map_err(io)?
        .guess_file_type()
        .map_err(io)?
        .file_type();
    let Some(ty) = ty.filter(|ty| {
        matches!(
            ty,
            FileType::Mpeg
                | FileType::Wav
                | FileType::Aiff
                | FileType::Flac
                | FileType::Vorbis
                | FileType::Opus
                | FileType::Speex
                | FileType::Mp4
                | FileType::Ape
                | FileType::WavPack
                | FileType::Mpc
        )
    }) else {
        tracing::warn!(
            uri,
            media_uuid = id,
            "media container cannot store UUID; identity retained in SQLite only"
        );
        return Ok(false);
    };
    if std::fs::symlink_metadata(&full)
        .map_err(io)?
        .file_type()
        .is_symlink()
        || !full
            .canonicalize()
            .map_err(io)?
            .starts_with(root.canonicalize().map_err(io)?)
    {
        return Err(io(format!(
            "{uri}: refusing UUID write outside media root or through symlink"
        )));
    }
    let before = std::fs::metadata(&full).map_err(io)?;
    if (before.len(), modified_ns(&before)) != expected {
        return Err(io(format!("{uri}: file changed during UUID scan")));
    }
    let stage = Staged(full.with_file_name(format!(
        ".stationd-uuid-{}.{}",
        uuid::Uuid::new_v4(),
        full.extension().unwrap_or_default().to_string_lossy()
    )));
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stage.0)
        .map_err(io)?;
    let mut source = std::fs::File::open(&full).map_err(io)?;
    std::io::copy(&mut source, &mut output).map_err(io)?;
    drop(source);
    drop(output);
    save_tag(&stage.0, ty, id)?;
    let (_, after) = crate::media::read_tagged(&stage.0).map_err(io)?;
    if from_tags(&after)?.as_deref() != Some(id) {
        return Err(io(format!("{uri}: UUID write did not survive readback")));
    }
    let out = std::fs::OpenOptions::new()
        .write(true)
        .open(&stage.0)
        .map_err(io)?;
    out.set_modified(before.modified().map_err(io)?)
        .map_err(io)?;
    out.sync_all().map_err(io)?;
    drop(out);
    // Restore access attributes after closing the verified staged writer.
    crate::library_lock::preserve_access(&full, &stage.0)?;
    std::fs::File::open(&stage.0)
        .map_err(io)?
        .sync_all()
        .map_err(io)?;
    let current = std::fs::metadata(&full).map_err(io)?;
    if (current.len(), modified_ns(&current)) != expected {
        return Err(io(format!("{uri}: file changed during UUID write")));
    }
    std::fs::rename(&stage.0, &full).map_err(io)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::Epoch;
    use crate::{
        broadcast_log, db, episode_play, media, media_index, playlist_cursor, queue_state,
    };
    use std::io::Write;

    fn wav(path: &Path) {
        let n = 8000u32;
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
        f.write_all(&vec![128; n as usize]).unwrap();
    }

    async fn sync(pool: &SqlitePool, root: &Path) -> ScanReport {
        let mut report = media::scan_library(root).unwrap();
        let guards = prepare(pool, root, &mut report).await.unwrap();
        media_index::replace_library_with_writeback(
            pool,
            &report.media,
            &report.metadata,
            &guards,
            100,
        )
        .await
        .unwrap();
        report
    }

    async fn shared_stations(root: &Path) {
        let local = tempfile::tempdir().unwrap();
        wav(&root.join("shared.wav"));
        let a = db::init(&local.path().join("a.db")).await.unwrap();
        let b_path = local.path().join("b.db");
        let b = db::init(&b_path).await.unwrap();
        let mut ra = media::scan_library(root).unwrap();
        let mut rb = ra.clone();
        // Two already migrated stations start with different path-generated IDs.
        media_index::replace_library(&a, &ra.media, 1)
            .await
            .unwrap();
        media_index::replace_library(&b, &rb.media, 1)
            .await
            .unwrap();
        let before_a = ensure(&a, "shared.wav").await.unwrap();
        let before_b = ensure(&b, "shared.wav").await.unwrap();
        assert_ne!(before_a, before_b);
        let log = broadcast_log::record(&b, "shared.wav", None, Epoch(50), Default::default())
            .await
            .unwrap();
        episode_play::mark(
            &b,
            "p",
            "shared.wav",
            rb.media[0].size_bytes as i64,
            rb.media[0].mtime_ns,
            Epoch(50),
        )
        .await
        .unwrap();
        queue_state::push(&b, "q", "shared.wav", None, Epoch(50))
            .await
            .unwrap();
        playlist_cursor::set(&b, "p", "shared.wav").await.unwrap();
        // Each report predates the other writer: the lock must protect re-reading
        // as well as assigning and installing the UUID.
        let (ga, gb) = tokio::join!(prepare(&a, root, &mut ra), prepare(&b, root, &mut rb));
        media_index::replace_library_with_writeback(&a, &ra.media, &ra.metadata, &ga.unwrap(), 2)
            .await
            .unwrap();
        media_index::replace_library_with_writeback(&b, &rb.media, &rb.metadata, &gb.unwrap(), 2)
            .await
            .unwrap();
        let id = ensure(&a, "shared.wav").await.unwrap();
        assert_eq!(id, ensure(&b, "shared.wav").await.unwrap());
        let (_, tags) = media::read_tagged(&root.join("shared.wav")).unwrap();
        assert_eq!(from_tags(&tags).unwrap().as_deref(), Some(id.as_str()));
        for table in [
            "media",
            "broadcast_log",
            "episode_play",
            "queue_entry",
            "playlist_cursor",
        ] {
            let ids: Vec<String> = sqlx::query_scalar(&format!("SELECT media_uuid FROM {table}"))
                .fetch_all(&b)
                .await
                .unwrap();
            assert_eq!(ids, vec![id.clone()], "{table}");
        }
        assert_eq!(
            broadcast_log::row(&b, log).await.unwrap().unwrap().rel_path,
            "shared.wav"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT confirmed FROM media_identity WHERE uuid = ?")
                .bind(&id)
                .fetch_one(&b)
                .await
                .unwrap(),
            1
        );
        let copy = db::memory_copy(&b_path).await.unwrap();
        assert_eq!(ensure(&copy, "shared.wav").await.unwrap(), id);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT confirmed FROM media_identity WHERE uuid = ?")
                .bind(&id)
                .fetch_one(&copy)
                .await
                .unwrap(),
            1
        );
        assert!(!root.join(".stationd-library.lock").exists());
    }

    #[tokio::test]
    async fn concurrent_stations_adopt_file_uuid_and_rebind_provisional_history() {
        let root = tempfile::tempdir().unwrap();
        crate::library_lock::tests::verify_process_lock(root.path());
        shared_stations(root.path()).await;
    }

    #[tokio::test]
    #[ignore = "requires writable shared filesystem in STATIOND_TEST_SHARED_ROOT; databases stay local"]
    async fn shared_stations_on_nfs() {
        let shared = std::env::var("STATIOND_TEST_SHARED_ROOT").expect("STATIOND_TEST_SHARED_ROOT");
        let root = tempfile::Builder::new()
            .prefix(".stationd-uuid-test-")
            .tempdir_in(shared)
            .unwrap();
        crate::library_lock::tests::verify_process_lock(root.path());
        shared_stations(root.path()).await;
    }

    #[cfg(target_os = "linux")]
    fn access_snapshot(path: &Path) -> (u32, u32, u32, BTreeMap<Vec<u8>, Vec<u8>>) {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).unwrap();
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let size = unsafe { libc::listxattr(path.as_ptr(), std::ptr::null_mut(), 0) };
        assert!(size >= 0);
        let mut names = vec![0u8; size as usize];
        let size =
            unsafe { libc::listxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
        assert!(size >= 0);
        let mut attrs = BTreeMap::new();
        for name in names[..size as usize]
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
        {
            let key = std::ffi::CString::new(name).unwrap();
            let size =
                unsafe { libc::getxattr(path.as_ptr(), key.as_ptr(), std::ptr::null_mut(), 0) };
            assert!(size >= 0);
            let mut value = vec![0u8; size as usize];
            let size = unsafe {
                libc::getxattr(
                    path.as_ptr(),
                    key.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            };
            assert!(size >= 0);
            value.truncate(size as usize);
            attrs.insert(name.to_vec(), value);
        }
        (meta.uid(), meta.gid(), meta.mode(), attrs)
    }

    async fn master_and_readonly_replica(root: &Path) {
        let local = tempfile::tempdir().unwrap();
        std::fs::write(
            root.join("shared.mp3"),
            include_bytes!("../tests/fixtures/media-identity/sample.mp3"),
        )
        .unwrap();
        let pool = db::init(&local.path().join("replica.db")).await.unwrap();
        let initial = media::scan_library(root).unwrap();
        media_index::replace_library(&pool, &initial.media, 1)
            .await
            .unwrap();
        let old = ensure(&pool, "shared.mp3").await.unwrap();
        let log = broadcast_log::record(&pool, "shared.mp3", None, Epoch(1), Default::default())
            .await
            .unwrap();
        // The master tags the physical file before the replica's first scan.
        let master = db::init(&local.path().join("master.db")).await.unwrap();
        let mut report = initial;
        #[cfg(target_os = "linux")]
        let original_access = access_snapshot(&root.join("shared.mp3"));
        prepare(&master, root, &mut report).await.unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(access_snapshot(&root.join("shared.mp3")), original_access);
        let id = ensure(&master, "shared.mp3").await.unwrap();
        assert_ne!(old, id);
        let saved = std::fs::read(root.join("shared.mp3")).unwrap();
        let analysis = crate::config::AnalysisConfig {
            enabled: true,
            extractor: Some(local.path().join("extractor-must-not-run")),
            ..Default::default()
        };
        // Even an active master lock must not require a replica to write or wait.
        let lock = crate::library_lock::acquire(root).await.unwrap();
        let replica = crate::library_actor::spawn_with_policy(
            pool.clone(),
            root.into(),
            None,
            analysis,
            true,
        );
        let outcome = replica.scan().await.unwrap();
        assert_eq!(outcome.stats.present, 1);
        assert_eq!(ensure(&pool, "shared.mp3").await.unwrap(), id);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT media_uuid FROM broadcast_log WHERE id = ?")
                .bind(log)
                .fetch_one(&pool)
                .await
                .unwrap(),
            id
        );
        assert_eq!(std::fs::read(root.join("shared.mp3")).unwrap(), saved);
        assert!(matches!(
            replica.scan_with(true).await,
            Err(crate::library_actor::LibraryError::ReadOnly)
        ));
        assert!(matches!(
            replica
                .set_tags("shared.mp3".into(), String::new(), Default::default())
                .await,
            Err(crate::library_actor::LibraryError::ReadOnly)
        ));
        assert!(matches!(
            replica.reorganize(false).await,
            Err(crate::library_actor::LibraryError::ReadOnly)
        ));
        // The replica can rescan repeatedly and reconstruct its UUID in a new DB.
        replica.scan().await.unwrap();
        let rebuilt_pool = db::init(&local.path().join("rebuilt.db")).await.unwrap();
        let rebuilt = crate::library_actor::spawn_with_policy(
            rebuilt_pool.clone(),
            root.into(),
            None,
            Default::default(),
            true,
        );
        rebuilt.scan().await.unwrap();
        assert_eq!(ensure(&rebuilt_pool, "shared.mp3").await.unwrap(), id);
        assert_eq!(std::fs::read(root.join("shared.mp3")).unwrap(), saved);
        drop(lock);
        assert!(!root.join(".stationd-library.lock").exists());
    }

    #[tokio::test]
    async fn replicas_only_index_master_tags_without_writing_or_analyzing() {
        let root = tempfile::tempdir().unwrap();
        master_and_readonly_replica(root.path()).await;
    }

    #[tokio::test]
    async fn replica_requires_master_uuid_without_inventing_an_identity() {
        let root = tempfile::tempdir().unwrap();
        wav(&root.path().join("untagged.wav"));
        let local = tempfile::tempdir().unwrap();
        let pool = db::init(&local.path().join("replica.db")).await.unwrap();
        let saved = std::fs::read(root.path().join("untagged.wav")).unwrap();
        let replica = crate::library_actor::spawn_with_policy(
            pool.clone(),
            root.path().into(),
            None,
            Default::default(),
            true,
        );
        assert!(
            replica
                .scan()
                .await
                .unwrap_err()
                .to_string()
                .contains("master")
        );
        assert!(id_for_uri(&pool, "untagged.wav").await.unwrap().is_none());
        assert_eq!(
            std::fs::read(root.path().join("untagged.wav")).unwrap(),
            saved
        );
        assert!(!root.path().join(".stationd-library.lock").exists());
    }

    #[tokio::test]
    #[ignore = "full master UUID and metadata writes on NFS; optional real-file copy fixture"]
    async fn master_full_scan_on_nfs() {
        let shared = std::env::var("STATIOND_TEST_SHARED_ROOT").unwrap();
        let root = tempfile::Builder::new().prefix(".stationd-full-scan-test-").tempdir_in(shared).unwrap();
        let file = root.path().join("sample.mp3");
        if let Ok(source) = std::env::var("STATIOND_TEST_SOURCE_MEDIA") {
            std::fs::copy(source, &file).unwrap();
            let (mut tag, version) = crate::media_tags::id3v2_of(&file, FileType::Mpeg).unwrap();
            tag.remove_user_text(UUID_TAG);
            tag.remove_user_text("creation");
            tag.save_to_path(&file, WriteOptions::default().use_id3v23(version == lofty::id3::v2::Id3v2Version::V3)).unwrap();
        } else {
            std::fs::write(&file, include_bytes!("../tests/fixtures/media-identity/sample.mp3")).unwrap();
        }
        let local = tempfile::tempdir().unwrap();
        let pool = db::init(&local.path().join("master.db")).await.unwrap();
        std::fs::OpenOptions::new().write(true).open(&file).unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234567890)).unwrap();
        let mut report = media::scan_library(root.path()).unwrap();
        prepare(&pool, root.path(), &mut report).await.unwrap();
        // Let NFS attribute caches expire between the two staged writes, as
        // happens while a large library is tagged and plugins process it.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        crate::scan_writeback::apply(root.path(), &mut report).unwrap();
        let master = crate::library_actor::spawn(pool.clone(), root.path().into());
        master.scan().await.unwrap();
        let saved = std::fs::read(&file).unwrap();
        master.scan().await.unwrap();
        assert_eq!(std::fs::read(file).unwrap(), saved);
    }

    #[tokio::test]
    #[ignore = "requires writable shared filesystem in STATIOND_TEST_SHARED_ROOT; only master writes media"]
    async fn master_and_replicas_on_nfs() {
        let shared = std::env::var("STATIOND_TEST_SHARED_ROOT").expect("STATIOND_TEST_SHARED_ROOT");
        let root = tempfile::Builder::new()
            .prefix(".stationd-master-test-")
            .tempdir_in(shared)
            .unwrap();
        master_and_readonly_replica(root.path()).await;
    }

    #[tokio::test]
    async fn uuid_is_tagged_once_survives_rescan_prune_and_database_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("audio");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("a.wav");
        wav(&path);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let pool = db::init(&dir.path().join("one.db")).await.unwrap();
        sync(&pool, &root).await;
        let id = id_for_uri(&pool, "a.wav").await.unwrap().unwrap();
        let (_, tags) = media::read_tagged(&path).unwrap();
        assert_eq!(from_tags(&tags).unwrap().as_deref(), Some(id.as_str()));
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        let mut again = media::scan_library(&root).unwrap();
        assert!(prepare(&pool, &root, &mut again).await.unwrap().is_empty());
        assert!(
            !again
                .custom_tags
                .values()
                .flatten()
                .any(|t| t.name == UUID_TAG)
        );
        media_index::mark_unavailable(&pool, "a.wav").await.unwrap();
        media_index::prune_unavailable(&pool, None).await.unwrap();
        sync(&pool, &root).await;
        assert_eq!(
            id_for_uri(&pool, "a.wav").await.unwrap().as_deref(),
            Some(id.as_str())
        );
        let rebuilt = db::init(&dir.path().join("two.db")).await.unwrap();
        sync(&rebuilt, &root).await;
        assert_eq!(
            id_for_uri(&rebuilt, "a.wav").await.unwrap().as_deref(),
            Some(id.as_str())
        );
    }

    #[tokio::test]
    async fn external_move_preserves_history_constraints_episode_queue_and_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("audio");
        std::fs::create_dir(&root).unwrap();
        wav(&root.join("a.wav"));
        let db_path = dir.path().join("station.db");
        let pool = db::init(&db_path).await.unwrap();
        let report = sync(&pool, &root).await;
        let m = &report.media[0];
        let id = id_for_uri(&pool, "a.wav").await.unwrap().unwrap();
        let log = broadcast_log::record(&pool, "a.wav", None, Epoch(50), Default::default())
            .await
            .unwrap();
        broadcast_log::mark_aired(&pool, log, Epoch(51))
            .await
            .unwrap();
        episode_play::mark(
            &pool,
            "p",
            "a.wav",
            m.size_bytes as i64,
            m.mtime_ns,
            Epoch(52),
        )
        .await
        .unwrap();
        queue_state::push(&pool, "q", "a.wav", None, Epoch(53))
            .await
            .unwrap();
        playlist_cursor::set(&pool, "p", "a.wav").await.unwrap();
        std::fs::rename(root.join("a.wav"), root.join("b.wav")).unwrap();
        sync(&pool, &root).await;
        assert_eq!(
            id_for_uri(&pool, "b.wav").await.unwrap().as_deref(),
            Some(id.as_str())
        );
        assert_eq!(id_for_uri(&pool, "a.wav").await.unwrap(), None);
        assert!(
            broadcast_log::tracks_since(&pool, 0)
                .await
                .unwrap()
                .contains("b.wav")
        );
        assert!(
            episode_play::played_matching(&pool, "p")
                .await
                .unwrap()
                .contains("b.wav")
        );
        assert_eq!(
            playlist_cursor::get(&pool, "p").await.unwrap().as_deref(),
            Some("b.wav")
        );
        assert_eq!(
            queue_state::pop(&pool, "q", false)
                .await
                .unwrap()
                .as_deref(),
            Some("b.wav")
        );
        let historical = broadcast_log::row(&pool, log).await.unwrap().unwrap();
        assert_eq!(historical.rel_path, "a.wav");
        let stored_id: String =
            sqlx::query_scalar("SELECT media_uuid FROM broadcast_log WHERE id = ?")
                .bind(log)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored_id, id);
        let copy = db::memory_copy(&db_path).await.unwrap();
        assert_eq!(
            uri_for_id(&copy, &id).await.unwrap().as_deref(),
            Some("b.wav")
        );
        assert_eq!(
            broadcast_log::tracks_since(&copy, 0).await.unwrap(),
            broadcast_log::tracks_since(&pool, 0).await.unwrap()
        );
    }

    #[tokio::test]
    async fn duplicate_uuid_is_rejected_without_changing_registry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("audio");
        std::fs::create_dir(&root).unwrap();
        wav(&root.join("a.wav"));
        let pool = db::init(&dir.path().join("station.db")).await.unwrap();
        sync(&pool, &root).await;
        let before = crate::onair_sim::dump(&pool).await;
        std::fs::copy(root.join("a.wav"), root.join("copy.wav")).unwrap();
        let mut report = media::scan_library(&root).unwrap();
        assert!(
            prepare(&pool, &root, &mut report)
                .await
                .unwrap_err()
                .to_string()
                .contains("UUID")
        );
        assert_eq!(crate::onair_sim::dump(&pool).await, before);
    }

    #[tokio::test]
    async fn missing_tag_is_restored_and_contradictory_tag_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        wav(&dir.path().join("a.wav"));
        let pool = db::init(&dir.path().join("station.db")).await.unwrap();
        let report = sync(&pool, dir.path()).await;
        let id = id_for_uri(&pool, "a.wav").await.unwrap().unwrap();
        let guard = (report.media[0].size_bytes as i64, report.media[0].mtime_ns);
        episode_play::mark(&pool, "p", "a.wav", guard.0, guard.1, Epoch(1))
            .await
            .unwrap();
        let (mut tag, _) =
            crate::media_tags::id3v2_of(&dir.path().join("a.wav"), FileType::Wav).unwrap();
        tag.remove_user_text(UUID_TAG);
        tag.save_to_path(dir.path().join("a.wav"), WriteOptions::default())
            .unwrap();
        // Removing tags externally is a content revision; identity still survives.
        sync(&pool, dir.path()).await;
        assert_eq!(
            id_for_uri(&pool, "a.wav").await.unwrap().as_deref(),
            Some(id.as_str())
        );
        let other = uuid::Uuid::new_v4().to_string();
        save_tag(&dir.path().join("a.wav"), FileType::Wav, &other).unwrap();
        let mut changed = media::scan_library(dir.path()).unwrap();
        assert!(
            prepare(&pool, dir.path(), &mut changed)
                .await
                .unwrap_err()
                .to_string()
                .contains("conflicts")
        );
        assert_eq!(
            id_for_uri(&pool, "a.wav").await.unwrap().as_deref(),
            Some(id.as_str())
        );
    }

    #[tokio::test]
    async fn migration_backfills_shared_uuid_without_rewriting_historical_uri() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let migrator = sqlx::migrate!("./migrations");
        for migration in migrator.iter().filter(|m| m.version < 33) {
            sqlx::query(&migration.sql).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO media (rel_path, duration_ms, size_bytes, mtime_ns, scanned_at) VALUES ('old.wav', 1000, 10, 1, 1)")
            .execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO broadcast_log (rel_path, played_at, aired_at) VALUES ('old.wav', 1, 2)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(include_str!("../migrations/0033_media_identity.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let (id, history): (String, String) = sqlx::query_as(
            "SELECT m.media_uuid, b.media_uuid FROM media m JOIN broadcast_log b ON b.rel_path = m.rel_path")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(id, history);
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
        assert_eq!(
            uri_for_id(&pool, &id).await.unwrap().as_deref(),
            Some("old.wav")
        );
    }

    #[test]
    fn invalid_nil_and_multiple_uuid_tags_are_not_accepted() {
        let tag = |value: &str| CustomTag {
            name: UUID_TAG.into(),
            value: value.into(),
        };
        assert!(from_tags(&[tag("bad")]).is_err());
        assert!(from_tags(&[tag(&uuid::Uuid::nil().to_string())]).is_err());
        let valid = tag(&uuid::Uuid::new_v4().to_string());
        assert!(from_tags(&[valid.clone(), valid]).is_err());
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn uuid_roundtrips_in_native_containers_without_losing_existing_tags() {
        let fixtures: &[(&str, &[u8])] = &[
            (
                "mp3",
                include_bytes!("../tests/fixtures/media-identity/sample.mp3"),
            ),
            (
                "flac",
                include_bytes!("../tests/fixtures/media-identity/sample.flac"),
            ),
            (
                "ogg",
                include_bytes!("../tests/fixtures/media-identity/sample.ogg"),
            ),
            (
                "opus",
                include_bytes!("../tests/fixtures/media-identity/sample.opus"),
            ),
            (
                "m4a",
                include_bytes!("../tests/fixtures/media-identity/sample.m4a"),
            ),
            (
                "aiff",
                include_bytes!("../tests/fixtures/media-identity/sample.aiff"),
            ),
        ];
        for (ext, bytes) in fixtures {
            let dir = tempfile::tempdir().unwrap();
            let uri = format!("sample.{ext}");
            let path = dir.path().join(&uri);
            std::fs::write(&path, bytes).unwrap();
            let before = std::fs::metadata(&path).unwrap();
            let (_, original_tags) = crate::media::read_tagged(&path).unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            assert!(
                write_tag(dir.path(), &uri, &id, (before.len(), modified_ns(&before))).unwrap(),
                "{ext}"
            );
            let (_, tags) = crate::media::read_tagged(&path).unwrap();
            assert_eq!(
                from_tags(&tags).unwrap().as_deref(),
                Some(id.as_str()),
                "{ext}"
            );
            for old in original_tags {
                assert!(tags.contains(&old), "{ext}: lost original tag {old:?}");
            }
            let report = crate::media::scan_file(dir.path(), &path).unwrap();
            assert_eq!(
                report.media[0].title.as_deref(),
                Some("UUID-fixture"),
                "{ext}"
            );
            assert_eq!(report.media[0].mtime_ns, modified_ns(&before), "{ext}");
            let saved = std::fs::read(&path).unwrap();
            assert!(
                !write_tag(
                    dir.path(),
                    &uri,
                    &id,
                    (report.media[0].size_bytes, report.media[0].mtime_ns)
                )
                .unwrap()
            );
            assert_eq!(std::fs::read(&path).unwrap(), saved, "{ext}: idempotence");
        }
    }

    #[test]
    fn uuid_tag_is_reserved_in_user_edits() {
        let mut edit = crate::media_tags::TagEdit::default();
        edit.user.insert(
            "stationd_uuid".into(),
            vec![uuid::Uuid::new_v4().to_string()],
        );
        // Validation is before even opening the file.
        assert!(matches!(
            crate::media_tags::write(Path::new("missing.wav"), "missing.wav", "", &edit),
            Err(TagError::BadValue(_))
        ));
    }

    #[tokio::test]
    async fn statistics_follow_uuid_across_uri_changes() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("station.db"))
            .await
            .unwrap();
        let id = ensure(&pool, "old.mp3").await.unwrap();
        let first = crate::broadcast_log::record(
            &pool,
            "old.mp3",
            None,
            crate::resolver::Epoch(1),
            Default::default(),
        )
        .await
        .unwrap();
        crate::broadcast_log::mark_aired(&pool, first, crate::resolver::Epoch(2))
            .await
            .unwrap();
        sqlx::query("UPDATE media_identity SET uri = 'new.mp3' WHERE uuid = ?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        crate::broadcast_log::record(
            &pool,
            "new.mp3",
            None,
            crate::resolver::Epoch(3),
            Default::default(),
        )
        .await
        .unwrap();
        let rows = crate::broadcast_log::plays(&pool, 0, crate::broadcast_log::PlaysBy::Media, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key.as_deref(), Some("new.mp3"));
        assert_eq!((rows[0].picked, rows[0].aired), (2, 1));
        for key in ["new.mp3", id.as_str()] {
            let row =
                crate::broadcast_log::plays_of(&pool, 0, crate::broadcast_log::PlaysBy::Media, key)
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!((row.picked, row.aired), (2, 1));
        }
    }
}

#[cfg(test)]
mod malformed_tag_tests {
    use super::*;

    #[tokio::test]
    async fn blank_uuid_tag_fails_scan_without_rewriting_file_or_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.mp3");
        std::fs::write(
            &path,
            include_bytes!("../tests/fixtures/media-identity/sample.mp3"),
        )
        .unwrap();
        save_tag(&path, FileType::Mpeg, " ").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let pool = crate::db::init(&dir.path().join("station.db"))
            .await
            .unwrap();
        let mut report = crate::media::scan_library(dir.path()).unwrap();
        let error = prepare(&pool, dir.path(), &mut report).await.unwrap_err();
        assert!(error.to_string().contains("invalid UUID"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(id_for_uri(&pool, "sample.mp3").await.unwrap(), None);
    }
}
