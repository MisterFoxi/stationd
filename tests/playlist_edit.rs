//! Validate / preview / save / export of a playlist through stationd,
//! end to end through the library (temp DB + a `tempdir` playlist root, no
//! tonic server).

use std::fs;

use stationd::playlist::DiagCode;
use stationd::playlist_edit::{self, revision};
use stationd::sync::{self, RemoveError};
use stationd::{db, media, media_index, store};

async fn setup() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let pool = db::init(&dir.path().join("test.db")).await.unwrap();
    let lib: Vec<_> = ["music/a.mp3", "music/b.mp3", "jingles/j.mp3"]
        .iter()
        .enumerate()
        .map(|(i, p)| media::ScannedMedia {
            rel_path: p.to_string(),
            title: Some(format!("T{i}")),
            artist: Some(if i == 0 { "X".into() } else { "Y".into() }),
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 60_000,
            size_bytes: 1,
            mtime_ns: 0,
        })
        .collect();
    media_index::replace_library(&pool, &lib, 1000).await.unwrap();
    fs::create_dir_all(dir.path().join("pl")).unwrap();
    (dir, pool)
}

const MUSIC: &str = "# ma playlist\nname = \"Music\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                     [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"music/\"\n";

fn root(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("pl")
}

#[tokio::test]
async fn save_creates_the_file_keeps_comments_and_applies() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    let s = playlist_edit::save(&pool, &root, "Shows/Music", MUSIC, "").await.unwrap();
    assert!(s.ok && s.created && !s.conflict, "{s:?}");
    assert_eq!(s.file, "shows/music.toml");
    let on_disk = fs::read_to_string(root.join("shows/music.toml")).unwrap();
    assert_eq!(on_disk, s.toml);
    assert!(on_disk.starts_with("# ma playlist\n"), "comment kept");
    assert!(on_disk.contains(&format!("id = \"{}\"", s.id)));
    assert_eq!(s.revision, revision(on_disk.as_bytes()));
    // Applied: the view has it under its ref.
    let row = store::find(&pool, "shows/music").await.unwrap().unwrap();
    assert_eq!(row.id, s.id);
    assert_eq!(row.toml, on_disk);
}

#[tokio::test]
async fn creating_over_an_existing_file_is_a_conflict() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    fs::write(root.join("music.toml"), MUSIC).unwrap();
    let s = playlist_edit::save(&pool, &root, "music", MUSIC, "").await.unwrap();
    assert!(s.conflict && !s.ok);
    assert_eq!(s.revision, revision(MUSIC.as_bytes()));
    assert_eq!(fs::read_to_string(root.join("music.toml")).unwrap(), MUSIC, "untouched");
}

#[tokio::test]
async fn a_stale_revision_writes_nothing() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    let first = playlist_edit::save(&pool, &root, "music", MUSIC, "").await.unwrap();
    // Someone edits the file by hand in between.
    let edited = format!("{}\n# retouche\n", first.toml);
    fs::write(root.join("music.toml"), &edited).unwrap();
    let draft = first.toml.replace("Music", "Musique");
    let s = playlist_edit::save(&pool, &root, "music", &draft, &first.revision).await.unwrap();
    assert!(s.conflict && !s.ok);
    assert_eq!(s.revision, revision(edited.as_bytes()));
    assert_eq!(fs::read_to_string(root.join("music.toml")).unwrap(), edited);
    // With the current revision it goes through, and keeps the id.
    let s = playlist_edit::save(&pool, &root, "music", &draft, &revision(edited.as_bytes())).await.unwrap();
    assert!(s.ok, "{s:?}");
    assert_eq!(s.id, first.id);
}

#[tokio::test]
async fn a_draft_without_id_keeps_the_playlist_identity_and_a_changed_id_is_refused() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    let first = playlist_edit::save(&pool, &root, "music", MUSIC, "").await.unwrap();
    let s = playlist_edit::save(&pool, &root, "music", MUSIC, &first.revision).await.unwrap();
    assert!(s.ok);
    assert_eq!(s.id, first.id, "no id in the draft: the file's is kept");
    let other = format!("id = \"11111111-1111-1111-1111-111111111111\"\n{MUSIC}");
    let s2 = playlist_edit::save(&pool, &root, "music", &other, &s.revision).await.unwrap();
    assert!(!s2.ok);
    assert_eq!(s2.diagnostics[0].code, DiagCode::IdChanged);
    assert_eq!(s2.diagnostics[0].field, "id");
}

#[tokio::test]
async fn an_invalid_draft_writes_nothing_and_says_where() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    let bad = "name = \"q\"\n[selection]\nmode = \"queue\"\norder = \"shuffle\"\n";
    let s = playlist_edit::save(&pool, &root, "q", bad, "").await.unwrap();
    assert!(!s.ok && !s.conflict);
    assert_eq!(s.diagnostics[0].field, "selection.order");
    assert!(!root.join("q.toml").exists());
    assert!(store::find(&pool, "q").await.unwrap().is_none());
}

#[tokio::test]
async fn validation_sees_the_set_member_refs_and_cycles() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    playlist_edit::save(&pool, &root, "shows/music", MUSIC, "").await.unwrap();
    let group = |refs: &[&str]| {
        let m: Vec<String> = refs.iter().map(|r| format!("[[selection.members]]\nref = \"{r}\"\n")).collect();
        format!("name = \"g\"\n[selection]\nmode = \"group\"\nstrategy = \"rotate\"\n{}", m.concat())
    };
    // `./music` is relative to the group's own directory.
    let (_, d) = playlist_edit::validate_draft(&pool, &group(&["./music", "nope"]), Some("shows/main")).await.unwrap();
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!((d[0].code, d[0].field.as_str()), (DiagCode::UnknownRef, "selection.members[2].ref"));
    // A group containing itself.
    let (_, d) = playlist_edit::validate_draft(&pool, &group(&["shows/main"]), Some("shows/main")).await.unwrap();
    assert!(d.iter().any(|d| d.code == DiagCode::Cycle), "{d:?}");
}

#[tokio::test]
async fn preview_counts_the_pool_and_warns_when_empty() {
    let (_dir, pool) = setup().await;
    let p = playlist_edit::preview(&pool, MUSIC, None, 0).await.unwrap();
    assert!(p.ok);
    assert_eq!((p.count, p.duration_ms, p.artists), (Some(2), Some(120_000), Some(2)));
    let paths: Vec<_> = p.sample.iter().map(|m| m.rel_path.as_str()).collect();
    assert_eq!(paths, ["music/a.mp3", "music/b.mp3"]);
    assert!(p.diagnostics.is_empty());

    let empty = MUSIC.replace("music/", "rien/");
    let p = playlist_edit::preview(&pool, &empty, None, 0).await.unwrap();
    assert!(p.ok);
    assert_eq!(p.count, Some(0));
    assert_eq!(p.diagnostics[0].code, DiagCode::EmptyPool);
    assert!(!p.diagnostics[0].error, "a warning, not an error");

    // Invalid draft: diagnostics, no pool.
    let p = playlist_edit::preview(&pool, "name = 1", None, 0).await.unwrap();
    assert!(!p.ok && p.count.is_none());
}

#[tokio::test]
async fn preview_of_a_group_counts_each_member() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    playlist_edit::save(&pool, &root, "music", MUSIC, "").await.unwrap();
    let g = "name = \"g\"\n[selection]\nmode = \"group\"\nstrategy = \"rotate\"\n[[selection.members]]\nref = \"music\"\n";
    let p = playlist_edit::preview(&pool, g, Some("g"), 0).await.unwrap();
    assert!(p.ok, "{:?}", p.diagnostics);
    assert_eq!(p.members.len(), 1);
    assert_eq!((p.members[0].resolved.as_deref(), p.members[0].count), (Some("music"), Some(2)));
    assert_eq!(p.count, Some(2));
    assert!(p.diagnostics.is_empty());

    // A member with nothing to air is flagged on its line (warning).
    let empty = MUSIC.replace("music/", "rien/");
    playlist_edit::save(&pool, &root, "rien", &empty, "").await.unwrap();
    let g2 = format!("{g}[[selection.members]]\nref = \"rien\"\n");
    let p = playlist_edit::preview(&pool, &g2, Some("g"), 0).await.unwrap();
    assert!(p.ok);
    assert_eq!(p.diagnostics.len(), 1, "{:?}", p.diagnostics);
    assert_eq!((p.diagnostics[0].code, p.diagnostics[0].field.as_str()), (DiagCode::EmptyPool, "selection.members[2].ref"));
}

#[tokio::test]
async fn export_gives_the_file_with_its_revision() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    let s = playlist_edit::save(&pool, &root, "music", MUSIC, "").await.unwrap();
    let x = playlist_edit::export(&pool, &root, "Music").await.unwrap();
    let f = x.file.as_ref().unwrap();
    assert_eq!((f.rel.as_str(), f.revision.as_str()), ("music.toml", s.revision.as_str()));
    assert!(!x.file_differs());
    fs::write(root.join("music.toml"), format!("{}# à la main\n", s.toml)).unwrap();
    assert!(playlist_edit::export(&pool, &root, "music").await.unwrap().file_differs());
}

#[tokio::test]
async fn remove_with_a_stale_revision_removes_nothing() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    let s = playlist_edit::save(&pool, &root, "music", MUSIC, "").await.unwrap();
    fs::write(root.join("music.toml"), format!("{}# à la main\n", s.toml)).unwrap();
    let r = sync::remove(&pool, &root, "music", Some(&s.revision)).await;
    assert!(matches!(r, Err(RemoveError::Conflict { .. })), "{r:?}");
    assert!(root.join("music.toml").exists());
    let now = revision(fs::read(root.join("music.toml")).unwrap().as_slice());
    sync::remove(&pool, &root, "music", Some(&now)).await.unwrap();
    assert!(!root.join("music.toml").exists());
}

#[tokio::test]
async fn containing_names_static_and_dynamic_holders_and_refuses_an_unknown_media() {
    let (dir, pool) = setup().await;
    let root = root(&dir);
    playlist_edit::save(&pool, &root, "music", MUSIC, "").await.unwrap();
    let jingles = "name = \"J\"\n[selection]\nmode = \"static\"\nfiles = [\"jingles/j.mp3\", \"/music/b.mp3\"]\n";
    playlist_edit::save(&pool, &root, "jingles", jingles, "").await.unwrap();
    let grp = "name = \"G\"\n[selection]\nmode = \"group\"\nstrategy = \"weighted\"\nmembers = [{ ref = \"music\", weight = 1 }]\n";
    playlist_edit::save(&pool, &root, "grp", grp, "").await.unwrap();

    let refs = |h: Vec<playlist_edit::Holder>| h.into_iter().filter_map(|h| h.rel_path).collect::<Vec<_>>();
    // b: filtered by `music` (dynamic), listed by `jingles` (static, leading `/` normalised).
    assert_eq!(refs(playlist_edit::containing(&pool, "music/b.mp3").await.unwrap()), ["jingles", "music"]);
    assert_eq!(refs(playlist_edit::containing(&pool, "jingles/j.mp3").await.unwrap()), ["jingles"]);
    // A vanished media still says where it was.
    media_index::mark_unavailable(&pool, "music/a.mp3").await.unwrap();
    assert_eq!(refs(playlist_edit::containing(&pool, "music/a.mp3").await.unwrap()), ["music"]);
    // Case matters for a media path: unknown = said, not an empty answer.
    assert!(matches!(
        playlist_edit::containing(&pool, "Music/b.mp3").await,
        Err(playlist_edit::EditError::UnknownMedia(_))
    ));
}

#[tokio::test]
async fn the_reference_index_lists_grid_rules_and_groups() {
    use stationd::grid_index::insert_rule;
    use stationd::resolver::{Rule, RuleKind, Validity};
    let (dir, pool) = setup().await;
    let root = root(&dir);
    playlist_edit::save(&pool, &root, "shows/music", MUSIC, "").await.unwrap();
    let grp = "name = \"G\"\n[selection]\nmode = \"group\"\nstrategy = \"sequence\"\n\
               members = [{ ref = \"./music\" }, { ref = \"shows/music\" }]\n";
    playlist_edit::save(&pool, &root, "shows/main", grp, "").await.unwrap();
    let rule = |id: &str, pl: &str| Rule {
        id: id.into(),
        enabled: true,
        validity: Validity::default(),
        kind: RuleKind::BaseRotation { playlist_ref: pl.into() },
    };
    insert_rule(&pool, &rule("floor", "Shows/Music.toml")).await.unwrap();
    insert_rule(&pool, &rule("main", "shows/main")).await.unwrap();

    let idx = sync::reference_index(&pool).await.unwrap();
    let music = &idx["shows/music"];
    assert_eq!(music.rules, ["floor"]);
    assert_eq!(music.groups, ["shows/main"], "one entry per group, however many times it lists it");
    assert_eq!(idx["shows/main"].rules, ["main"]);
    assert!(idx["shows/main"].groups.is_empty());
    // `referrers` (remove) reads the same index.
    let by = sync::referrers(&pool, "shows/music", &Default::default()).await.unwrap();
    assert_eq!(by, ["grid rule `floor`", "group `shows/main`"]);
}
