//! Disk reorganization, serialized by the library actor.
use crate::library_actor::LibraryError;
use sqlx::SqlitePool;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Default)]
pub struct Report {
    pub files: Vec<FileResult>,
}
impl Report {
    pub fn count(&self, status: &str) -> u64 {
        self.files.iter().filter(|f| f.status == status).count() as u64
    }
}
#[derive(Debug)]
pub struct FileResult {
    pub from: String,
    pub to: String,
    pub status: &'static str,
    pub detail: String,
}

// Discogs labels use --- between genre and style. A slash inside a label
// (Funk / Soul, Pop Rock / Indie) is text, not an extra hierarchy level.
fn destination(rel: &str, genre: &str) -> Result<String, String> {
    let path = Path::new(rel);
    if !path.components().all(|c| matches!(c, Component::Normal(_))) || rel.contains('\\') {
        return Err("invalid source path".into());
    }
    let file = path
        .file_name()
        .and_then(|p| p.to_str())
        .ok_or("invalid filename")?;
    let mut folders = Vec::new();
    for label in genre.split("---") {
        let label = label.trim();
        if label.is_empty() || label == "." || label == ".." {
            return Err("empty or invalid AI genre level".into());
        }
        let clean: String = label
            .chars()
            .map(|c| {
                if c.is_control() || "<>:\"/\\|?*".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let clean = clean.trim_end_matches(['.', ' ']);
        if clean.is_empty() {
            return Err("invalid AI genre level".into());
        }
        folders.push(clean.to_string());
    }
    folders.push(file.to_string());
    Ok(folders.join("/"))
}

// Do not follow source or target symlinks, even when they point within root.
fn checked_path(root: &Path, rel: &str, allow_missing: bool) -> Result<PathBuf, String> {
    let mut full = root.to_path_buf();
    let path = Path::new(rel);
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err("invalid relative path".into());
    }
    let components: Vec<_> = path.components().collect();
    for (i, component) in components.iter().enumerate() {
        full.push(component.as_os_str());
        match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.file_type().is_symlink() => return Err("symlink in media path".into()),
            Ok(meta) if i + 1 < components.len() && !meta.is_dir() => {
                return Err("parent is not a directory".into())
            }
            Ok(_) => {}
            Err(e) if allow_missing && e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(full)
}

fn inspect(root: &Path, from: &str, to: &str, size: i64, mtime: i64) -> Result<(), String> {
    let source = checked_path(root, from, false)?;
    let meta = std::fs::metadata(source).map_err(|e| e.to_string())?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    if !meta.is_file() || meta.len() != size as u64 || modified != mtime {
        return Err("media changed since scan; run library scan first".into());
    }
    let target = checked_path(root, to, true)?;
    if from != to && std::fs::symlink_metadata(target).is_ok() {
        return Err("destination already exists; no overwrite".into());
    }
    Ok(())
}

pub(crate) async fn run(
    pool: &SqlitePool,
    root: &Path,
    dry_run: bool,
) -> Result<Report, LibraryError> {
    let root = std::fs::canonicalize(root).map_err(|_| LibraryError::BadRoot(root.into()))?;
    if !root.is_dir() {
        return Err(LibraryError::BadRoot(root));
    }
    let rows: Vec<(String, Option<String>, i64, i64)> = sqlx::query_as(
        "SELECT m.rel_path, a.genre_top, m.size_bytes, m.mtime_ns FROM media m
         LEFT JOIN media_analysis a ON a.rel_path = m.rel_path WHERE m.available = 1 ORDER BY m.rel_path"
    ).fetch_all(pool).await?;
    // Reserve all destinations before moving anything: both conflicting
    // sources are reported, independent of traversal or execution order.
    let targets: Vec<_> = rows
        .iter()
        .map(|(rel, genre, _, _)| {
            genre
                .as_deref()
                .filter(|g| !g.trim().is_empty())
                .map(|g| destination(rel, g))
        })
        .collect();
    let mut counts = BTreeMap::new();
    for to in targets.iter().flatten().filter_map(|r| r.as_ref().ok()) {
        *counts.entry(to.clone()).or_insert(0usize) += 1;
    }
    let mut report = Report::default();
    for ((from, _, size, mtime), target) in rows.into_iter().zip(targets) {
        let mut result = FileResult {
            from,
            to: String::new(),
            status: "skipped",
            detail: String::new(),
        };
        let to = match target {
            None => {
                result.detail = "no AI genre; left in place".into();
                report.files.push(result);
                continue;
            }
            Some(Err(e)) => {
                result.status = "failed";
                result.detail = e;
                report.files.push(result);
                continue;
            }
            Some(Ok(to)) => to,
        };
        result.to = to;
        let root_copy = root.clone();
        let from_copy = result.from.clone();
        let to_copy = result.to.clone();
        let checked = tokio::task::spawn_blocking(move || {
            inspect(&root_copy, &from_copy, &to_copy, size, mtime)
        })
        .await
        .map_err(|e| LibraryError::Join(e.to_string()))?;
        if let Err(e) = checked {
            result.status = "failed";
            result.detail = e;
        } else if result.from == result.to {
            result.status = "unchanged";
        } else if counts[&result.to] > 1 {
            result.status = "failed";
            result.detail = "multiple media have the same destination; no overwrite".into();
        } else {
            let indexed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE rel_path = ?")
                .bind(&result.to)
                .fetch_one(pool)
                .await?;
            if indexed != 0 {
                result.status = "failed";
                result.detail =
                    "destination already indexed; prune unavailable entries first".into();
            } else if dry_run {
                result.status = "planned";
            } else {
                match move_one(pool, &root, &result.from, &result.to, size, mtime).await {
                    Ok(()) => result.status = "moved",
                    Err(e) => {
                        result.status = "failed";
                        result.detail = e;
                    }
                }
            }
        }
        report.files.push(result);
    }
    Ok(report)
}

async fn move_one(
    pool: &SqlitePool,
    root: &Path,
    from: &str,
    to: &str,
    size: i64,
    mtime: i64,
) -> Result<(), String> {
    // Prepare every SQL update before touching the disk. FK validation is
    // deferred until commit because child keys have no ON UPDATE CASCADE.
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
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
            "UPDATE {table} SET rel_path = ? WHERE rel_path = ?"
        ))
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    sqlx::query("UPDATE playlist_cursor SET last_rel_path = ? WHERE last_rel_path = ?")
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    // Historical broadcast paths stay historical.
    let task_root = root.to_path_buf();
    let task_from = from.to_string();
    let task_to = to.to_string();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        inspect(&task_root, &task_from, &task_to, size, mtime)?;
        let source = checked_path(&task_root, &task_from, false)?;
        let target = checked_path(&task_root, &task_to, true)?;
        std::fs::create_dir_all(target.parent().ok_or("missing destination parent")?)
            .map_err(|e| e.to_string())?;
        checked_path(&task_root, &task_to, true)?;
        // hard_link atomically refuses an existing destination and preserves
        // bytes, inode, permissions and mtime. Cross-device moves fail safely.
        std::fs::hard_link(&source, &target)
            .map_err(|e| format!("cannot move without overwrite: {e}"))?;
        if let Err(e) = std::fs::remove_file(&source) {
            let cleanup = std::fs::remove_file(&target);
            return Err(format!(
                "cannot remove source: {e}; destination cleanup: {cleanup:?}"
            ));
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;
    if let Err(e) = tx.commit().await {
        let source = root.join(from);
        let target = root.join(to);
        let recovery = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::hard_link(&target, &source)?;
            std::fs::remove_file(&target)
        })
        .await
        .map_err(|e| e.to_string())?;
        return Err(format!(
            "index commit failed: {e}; disk rollback: {recovery:?}"
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, library_actor, media, media_index};

    #[test]
    fn discogs_hierarchy_preserves_labels_and_filename() {
        assert_eq!(
            destination("old/album/song.mp3", "Electronic---House").unwrap(),
            "Electronic/House/song.mp3"
        );
        assert_eq!(
            destination("song.mp3", "Funk / Soul---Soul").unwrap(),
            "Funk _ Soul/Soul/song.mp3"
        );
        assert_eq!(destination("song.mp3", "Rock").unwrap(), "Rock/song.mp3");
        assert!(destination("../song.mp3", "Rock").is_err());
        assert!(destination("song.mp3", "Electronic---..").is_err());
        assert!(destination("song.mp3", "Electronic---").is_err());
    }

    async fn index_file(pool: &SqlitePool, root: &Path, rel: &str, genre: Option<&str>) {
        let full = root.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, b"media bytes").unwrap();
        let meta = std::fs::metadata(full).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let media = media::ScannedMedia {
            rel_path: rel.into(),
            title: Some("Song".into()),
            artist: None,
            album: None,
            year: None,
            genres: vec!["original genre".into()],
            duration_ms: 1000,
            size_bytes: meta.len(),
            mtime_ns: mtime,
        };
        media_index::refresh_one(pool, &media, 1).await.unwrap();
        if let Some(genre) = genre {
            sqlx::query("INSERT INTO media_analysis (rel_path, genre_top, analyzer_version, analyzed_at) VALUES (?, ?, 'test', 1)")
                .bind(rel).bind(genre).execute(pool).await.unwrap();
        }
    }

    #[tokio::test]
    async fn preview_move_preserves_metadata_and_runtime_references() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let pool = db::init(&dir.path().join("test.db")).await.unwrap();
        index_file(&pool, &root, "old/song.mp3", Some("Electronic---House")).await;
        sqlx::query("INSERT INTO media_meta VALUES ('old/song.mp3', 'tempo', 'fast')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO media_tag VALUES ('old/song.mp3', '', 'original genre', 'original genre')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO queue_entry (playlist_ref, rel_path, enqueued_at) VALUES ('q', 'old/song.mp3', 1)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO playlist_cursor VALUES ('p', 'old/song.mp3', 1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO episode_play VALUES ('p', 'old/song.mp3', 11, 1, 1)")
            .execute(&pool)
            .await
            .unwrap();
        let lib = library_actor::spawn(pool.clone(), root.clone());
        use crate::proto::library::{library_service_server::LibraryService, ReorganizeRequest};
        let service = crate::library_grpc::LibraryGrpc::new(lib.clone());
        let preview = service
            .reorganize(tonic::Request::new(ReorganizeRequest { dry_run: true }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(preview.planned, 1);
        assert_eq!(preview.files[0].from, "old/song.mp3");
        assert_eq!(preview.files[0].to, "Electronic/House/song.mp3");
        assert!(root.join("old/song.mp3").is_file());
        assert!(!root.join("Electronic").exists());
        let report = lib.reorganize(false).await.unwrap();
        assert_eq!(report.count("moved"), 1, "{report:?}");
        assert!(!root.join("old/song.mp3").exists());
        assert_eq!(
            std::fs::read(root.join("Electronic/House/song.mp3")).unwrap(),
            b"media bytes"
        );
        for table in [
            "media",
            "media_genre",
            "media_meta",
            "media_tag",
            "media_analysis",
            "queue_entry",
            "episode_play",
        ] {
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {table} WHERE rel_path = 'Electronic/House/song.mp3'"
            ))
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, 1, "{table}");
        }
        let cursor: String = sqlx::query_scalar("SELECT last_rel_path FROM playlist_cursor")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(cursor, "Electronic/House/song.mp3");
        let fk: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(fk.is_empty());
        assert_eq!(lib.reorganize(false).await.unwrap().count("unchanged"), 1);
    }

    #[tokio::test]
    async fn collisions_missing_genres_and_stale_files_stay_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let pool = db::init(&dir.path().join("test.db")).await.unwrap();
        index_file(&pool, &root, "a/song.mp3", Some("Electronic---House")).await;
        index_file(&pool, &root, "b/song.mp3", Some("Electronic---House")).await;
        index_file(&pool, &root, "unknown.mp3", None).await;
        index_file(&pool, &root, "stale.mp3", Some("Rock---Indie")).await;
        std::fs::write(root.join("stale.mp3"), b"changed media").unwrap();
        index_file(&pool, &root, "exists.mp3", Some("Rock---Indie")).await;
        std::fs::create_dir_all(root.join("Rock/Indie")).unwrap();
        std::fs::write(root.join("Rock/Indie/exists.mp3"), b"keep me").unwrap();
        let preview = run(&pool, &root, true).await.unwrap();
        let report = run(&pool, &root, false).await.unwrap();
        assert_eq!(preview.count("failed"), 4);
        assert_eq!(report.count("failed"), 4);
        assert_eq!(report.count("skipped"), 1);
        assert_eq!(report.count("moved"), 0);
        for source in [
            "a/song.mp3",
            "b/song.mp3",
            "unknown.mp3",
            "stale.mp3",
            "exists.mp3",
        ] {
            assert!(root.join(source).is_file());
        }
        assert_eq!(
            std::fs::read(root.join("Rock/Indie/exists.mp3")).unwrap(),
            b"keep me"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_destination_symlink_outside_library() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let pool = db::init(&dir.path().join("test.db")).await.unwrap();
        index_file(&pool, &root, "song.mp3", Some("Electronic---House")).await;
        std::os::unix::fs::symlink(&outside, root.join("Electronic")).unwrap();
        let report = run(&pool, &root, false).await.unwrap();
        assert_eq!(report.count("failed"), 1);
        assert!(root.join("song.mp3").is_file());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }
}

#[cfg(test)]
mod rollback_tests {
    use super::*;

    #[tokio::test]
    async fn failed_commit_restores_disk_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
        std::fs::write(root.join("song.mp3"), b"media").unwrap();
        let meta = std::fs::metadata(root.join("song.mp3")).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        sqlx::query("INSERT INTO media (rel_path, duration_ms, size_bytes, mtime_ns, available, scanned_at) VALUES ('song.mp3', 1000, 5, ?, 1, 1)")
            .bind(mtime).execute(&pool).await.unwrap();
        // A deferred FK violation fails at commit, after the disk move.
        sqlx::query("CREATE TRIGGER fail_move AFTER UPDATE OF rel_path ON media BEGIN INSERT INTO media_meta VALUES ('missing.mp3', 'test', 'bad'); END")
            .execute(&pool).await.unwrap();
        let error = move_one(
            &pool,
            &root,
            "song.mp3",
            "Electronic/House/song.mp3",
            5,
            mtime,
        )
        .await
        .unwrap_err();
        assert!(error.contains("index commit failed"), "{error}");
        assert_eq!(std::fs::read(root.join("song.mp3")).unwrap(), b"media");
        assert!(!root.join("Electronic/House/song.mp3").exists());
        let rel: String = sqlx::query_scalar("SELECT rel_path FROM media")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rel, "song.mp3");
    }
}
