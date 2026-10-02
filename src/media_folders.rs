//! Directory inventory for the indexed media library.
use sqlx::SqlitePool;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: String,
    pub count: u64,
}
#[derive(Debug, Clone)]
pub struct FolderPage {
    pub folders: Vec<Folder>,
    pub next: Option<String>,
}
/// Directory inventory from indexed paths. No filesystem access.
pub async fn folders(
    pool: &SqlitePool,
    include_unavailable: bool,
    after: &str,
    limit: usize,
) -> Result<FolderPage, sqlx::Error> {
    let paths: Vec<(String,)> =
        sqlx::query_as("SELECT rel_path FROM media WHERE available = 1 OR ?1")
            .bind(include_unavailable)
            .fetch_all(pool)
            .await?;
    let mut counts = std::collections::BTreeMap::<String, u64>::new();
    counts.insert(String::new(), paths.len() as u64);
    for (path,) in paths {
        let mut parent = path.rsplit_once('/').map(|(p, _)| p);
        while let Some(p) = parent {
            if !p.is_empty() {
                *counts.entry(p.into()).or_default() += 1;
            }
            parent = p.rsplit_once('/').map(|(p, _)| p);
        }
    }
    let limit = limit.clamp(1, 1000);
    let mut folders: Vec<_> = counts
        .into_iter()
        .filter(|(p, _)| after.is_empty() || p.as_str() > after.strip_prefix("p:").unwrap_or(after))
        .take(limit + 1)
        .map(|(path, count)| Folder { path, count })
        .collect();
    let next = if folders.len() > limit {
        folders.truncate(limit);
        folders.last().map(|f| format!("p:{}", f.path))
    } else {
        None
    };
    Ok(FolderPage { folders, next })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_index::{self, SearchQuery};
    #[tokio::test]
    async fn inventory_pages_ancestors_and_direct_search_preserves_paths_and_genres() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("test.db")).await.unwrap();
        let media: Vec<_> = [
            "root.mp3",
            "Rock/a.mp3",
            "Rock/Sous dossier/b.mp3",
            "Rockabilly/c.mp3",
            "rock/d.mp3",
            "Été avec espace/sub/e.mp3",
            "Disparu/x.mp3",
        ]
        .into_iter()
        .map(|p| crate::media::ScannedMedia {
            rel_path: p.into(),
            title: Some("Title".into()),
            artist: None,
            album: None,
            year: None,
            genres: vec!["Rock".into(), "Test".into()],
            duration_ms: 60000,
            size_bytes: 1,
            mtime_ns: 0,
        })
        .collect();
        media_index::replace_library(&pool, &media, 1)
            .await
            .unwrap();
        sqlx::query("UPDATE media SET available=0 WHERE rel_path='Disparu/x.mp3'")
            .execute(&pool)
            .await
            .unwrap();
        let all = folders(&pool, false, "", 1000).await.unwrap().folders;
        assert_eq!(
            all[0],
            Folder {
                path: "".into(),
                count: 6
            }
        );
        assert_eq!(all.iter().find(|f| f.path == "Rock").unwrap().count, 2);
        assert!(all.iter().any(|f| f.path == "Été avec espace"));
        assert!(!all.iter().any(|f| f.path == "Disparu"));
        assert!(folders(&pool, true, "", 1000)
            .await
            .unwrap()
            .folders
            .iter()
            .any(|f| f.path == "Disparu"));
        let mut cursor = String::new();
        let mut pages = Vec::new();
        loop {
            let page = folders(&pool, false, &cursor, 1).await.unwrap();
            pages.extend(page.folders);
            match page.next {
                None => break,
                Some(next) => {
                    assert_ne!(next, cursor);
                    cursor = next;
                }
            }
        }
        assert_eq!(pages, all);
        let direct = |path: &str| SearchQuery {
            directory: Some(path.into()),
            ..Default::default()
        };
        let root = media_index::search(&pool, &direct("")).await.unwrap();
        assert_eq!(root.media[0].rel_path, "root.mp3");
        assert_eq!(root.total, 1);
        let rock = media_index::search(&pool, &direct("Rock")).await.unwrap();
        assert_eq!(rock.total, 1);
        assert_eq!(rock.media[0].rel_path, "Rock/a.mp3");
        assert_eq!(rock.media[0].genres, ["Rock", "Test"]);
        let nested = media_index::search(&pool, &direct("Été avec espace/sub"))
            .await
            .unwrap();
        assert_eq!(nested.media[0].rel_path, "Été avec espace/sub/e.mp3");
        assert_eq!(
            media_index::search(&pool, &direct("rock"))
                .await
                .unwrap()
                .media[0]
                .rel_path,
            "rock/d.mp3"
        );
        let recursive = media_index::search(
            &pool,
            &SearchQuery {
                folder: "Rock".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(recursive.total, 3);
        let filtered = media_index::search(
            &pool,
            &SearchQuery {
                directory: Some("Rock".into()),
                genres: vec!["Other".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(filtered.total, 0);
    }
}
