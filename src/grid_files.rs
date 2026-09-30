//! Grid files: the grids of the station live as TOML files under `[grid] path`
//! (`grid/` by default); ONE of them is active — applied to the grid engine.
//! The metier behind `ScheduleService.ListGrids / GetGrid / SaveGrid /
//! ActivateGrid / ReloadGrid / ValidateGrid / ApplyGrid`, without any proto
//! type (`schedule_grpc.rs` only translates).
//!
//! - **File-first.** The active file is the source of truth: re-read and
//!   applied at start-up and on `reload`; SQLite (family A) is its rebuildable
//!   view. A missing or invalid active file never empties the air: the last
//!   applied grid stays, the problem is logged and reported by `ListGrids`.
//! - **stationd writes the files** (D10): a client, local or remote, sends
//!   TOML; stationd validates, writes (atomic, read back), and applies when
//!   the file is the active one. Revision = fingerprint of the content, as for
//!   playlists: a save states the revision it started from, a file changed in
//!   between is a conflict — never an overwrite.
//! - **Which grid is active** is kept in the database (`grid_active`), set by
//!   `ActivateGrid`; none = `grid.toml`.
//!
//! Writes (save, activate, reload, apply) are serialised.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqlx::SqlitePool;

use crate::grid_engine::{EngineError, GridEngine};
use crate::grid_toml::{self, GridDiag};
use crate::playlist_edit::{revision, write_atomic};
use crate::resolver::{Grid, Rule};

/// The grid active when none was chosen.
pub const DEFAULT_GRID: &str = "grid.toml";

#[derive(Debug, thiserror::Error)]
pub enum GridFileError {
    /// Not a valid grid name (a path, `..`, empty…).
    #[error("{0}")]
    BadName(String),
    /// No such grid file.
    #[error("no grid `{0}` in the grid directory")]
    NotFound(String),
    /// The grid has problems: nothing applied, nothing activated.
    #[error("grid `{name}` rejected: {}", diags.iter().map(|d| d.message.as_str()).collect::<Vec<_>>().join("; "))]
    Invalid { name: String, diags: Vec<GridDiag> },
    #[error("{0}")]
    Io(String),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// `ete` / `ete.toml` → `ete.toml`. A grid is a file directly in the grid
/// directory: no separator, no `..`, no hidden file.
pub fn normalize_name(name: &str) -> Result<String, GridFileError> {
    let n = name.trim();
    let bad = |why: &str| GridFileError::BadName(format!("invalid grid name {name:?}: {why}"));
    if n.is_empty() {
        return Err(bad("empty"));
    }
    if n.contains('/') || n.contains('\\') || n.contains('\0') {
        return Err(bad("a grid is a file of the grid directory, without a path"));
    }
    if n.starts_with('.') {
        return Err(bad("must not start with a dot"));
    }
    let n = if n.ends_with(".toml") { n.to_string() } else { format!("{n}.toml") };
    if n == ".toml" {
        return Err(bad("empty"));
    }
    Ok(n)
}

/// One grid file, as `ListGrids` shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct GridInfo {
    pub name: String,
    /// Empty = no file (the active grid whose file is missing).
    pub revision: String,
    pub active: bool,
    /// Number of rules; `None` = the file does not parse.
    pub rules: Option<usize>,
    /// Why it cannot be applied as it is (its first problem); for the active
    /// grid, also a failed start-up / reload load.
    pub problem: Option<GridDiag>,
}

/// A grid file read for editing.
#[derive(Debug, Clone, PartialEq)]
pub struct GridText {
    pub name: String,
    /// Empty when there is no file yet.
    pub toml: String,
    pub revision: String,
    pub active: bool,
    pub exists: bool,
    /// Active grid only: the file no longer says what is applied (edited by
    /// hand since, or invalid and so not applied).
    pub differs_from_applied: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved {
    /// Written (and applied when active).
    pub ok: bool,
    pub diagnostics: Vec<GridDiag>,
    /// The expected revision is not the file's: nothing written.
    pub conflict: bool,
    /// After the write; the current one on a conflict or a refusal.
    pub revision: String,
    pub created: bool,
    /// The file is the active grid: it is now on air.
    pub applied: bool,
}

/// What happened to the active grid at start-up / on reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// Read and applied (its number of rules).
    Applied(usize),
    /// No file: the grid last applied stays.
    Missing,
}

#[derive(Clone)]
pub struct GridFiles {
    dir: PathBuf,
    pool: SqlitePool,
    engine: GridEngine,
    lock: Arc<tokio::sync::Mutex<()>>,
    /// Last start-up / reload problem of the active grid (reported by
    /// `ListGrids` until the next successful load).
    problem: Arc<std::sync::Mutex<Option<GridDiag>>>,
}

impl GridFiles {
    pub fn new(dir: impl Into<PathBuf>, pool: SqlitePool, engine: GridEngine) -> Self {
        Self {
            dir: dir.into(),
            pool,
            engine,
            lock: Arc::new(tokio::sync::Mutex::new(())),
            problem: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn engine(&self) -> &GridEngine {
        &self.engine
    }

    /// The name of the active grid (`grid.toml` when none was chosen).
    pub async fn active(&self) -> Result<String, GridFileError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT name FROM grid_active WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0).unwrap_or_else(|| DEFAULT_GRID.to_string()))
    }

    async fn set_active(&self, name: &str) -> Result<(), GridFileError> {
        sqlx::query(
            "INSERT INTO grid_active (id, name) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name",
        )
        .bind(name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn read_file(&self, name: &str) -> Result<Option<String>, GridFileError> {
        match std::fs::read_to_string(self.path(name)) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(GridFileError::Io(format!("cannot read grid {name}: {e}"))),
        }
    }

    /// Every problem of `text` as a grid of this station: TOML and grammar,
    /// then (when it parses) playlist refs and DJs. The rules when none.
    pub async fn diagnose(&self, text: &str) -> Result<(Option<Vec<Rule>>, Vec<GridDiag>), GridFileError> {
        let (items, mut diags) = grid_toml::diagnose_partial(text);
        let refs: Vec<(usize, &Rule)> = items.iter().map(|(n, r)| (*n, r)).collect();
        diags.extend(self.engine.diagnose_rules_at(&refs).await?);
        // Grammar before refs, then file order.
        grid_toml::in_file_order(&mut diags);
        if diags.is_empty() {
            Ok((Some(items.into_iter().map(|(_, r)| r).collect()), diags))
        } else {
            Ok((None, diags))
        }
    }

    /// The rules of a draft, or of grid `name`, for a preview; `Ok(None)` =
    /// neither (the applied grid). A draft or file with problems is refused.
    pub async fn source(&self, name: &str, draft: &str) -> Result<Option<Grid>, GridFileError> {
        let (label, text) = if !draft.is_empty() {
            ("draft".to_string(), draft.to_string())
        } else if !name.trim().is_empty() {
            let name = normalize_name(name)?;
            let text = self.read_file(&name)?.ok_or_else(|| GridFileError::NotFound(name.clone()))?;
            (name, text)
        } else {
            return Ok(None);
        };
        // Grammar only: a projection of a grid whose refs are broken is still
        // useful (the broken rule shows as a fallback / an empty pool).
        let (rules, diags) = grid_toml::diagnose(&text);
        match rules {
            Some(rules) => Ok(Some(Grid { rules })),
            None => Err(GridFileError::Invalid { name: label, diags }),
        }
    }

    /// The grid files, active one included even without a file.
    pub async fn list(&self) -> Result<Vec<GridInfo>, GridFileError> {
        let active = self.active().await?;
        let mut names: Vec<String> = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n.ends_with(".toml") && !n.starts_with('.'))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(GridFileError::Io(format!("cannot list {}: {e}", self.dir.display()))),
        };
        if !names.contains(&active) {
            names.push(active.clone());
        }
        names.sort();
        let startup_problem = self.problem.lock().expect("grid problem lock").clone();
        let mut out = Vec::new();
        for name in names {
            let is_active = name == active;
            let Some(text) = self.read_file(&name)? else {
                out.push(GridInfo { name, revision: String::new(), active: is_active, rules: None, problem: None });
                continue;
            };
            let (rules, diags) = grid_toml::diagnose(&text);
            let count = rules.as_ref().map(Vec::len);
            let mut problem = diags.into_iter().next();
            if let (None, Some(rules)) = (&problem, &rules) {
                problem = self.engine.diagnose_rules(rules).await?.into_iter().next();
            }
            if is_active && problem.is_none() {
                problem = startup_problem.clone();
            }
            out.push(GridInfo {
                revision: revision(text.as_bytes()),
                name,
                active: is_active,
                rules: count,
                problem,
            });
        }
        Ok(out)
    }

    /// Grid `name` (empty = the active one), for editing.
    pub async fn get(&self, name: &str) -> Result<GridText, GridFileError> {
        let active = self.active().await?;
        let name = if name.trim().is_empty() { active.clone() } else { normalize_name(name)? };
        let is_active = name == active;
        let text = self.read_file(&name)?;
        let differs = match (&text, is_active) {
            (Some(t), true) => {
                let applied = self.engine.list_rules().await?;
                match grid_toml::parse_grid(t) {
                    Ok(rules) => grid_toml::to_toml(&rules).ok() != grid_toml::to_toml(&applied).ok(),
                    Err(_) => true,
                }
            }
            _ => false,
        };
        Ok(GridText {
            revision: text.as_deref().map(|t| revision(t.as_bytes())).unwrap_or_default(),
            exists: text.is_some(),
            toml: text.unwrap_or_default(),
            name,
            active: is_active,
            differs_from_applied: differs,
        })
    }

    /// Validate `text`, write it as grid `name` (atomic, read back), apply it
    /// when it is the active grid. `expected_revision` empty = creation
    /// (conflict if the file exists); otherwise it must be the file's.
    /// Invalid → `ok = false` + diagnostics, nothing written.
    pub async fn save(&self, name: &str, text: &str, expected_revision: &str) -> Result<Saved, GridFileError> {
        let name = normalize_name(name)?;
        let _guard = self.lock.lock().await;
        let current = self.read_file(&name)?.map(|t| revision(t.as_bytes())).unwrap_or_default();
        if expected_revision.trim() != current {
            return Ok(Saved { conflict: true, revision: current, ..Saved::default() });
        }
        let (rules, diagnostics) = self.diagnose(text).await?;
        let Some(rules) = rules else {
            return Ok(Saved { diagnostics, revision: current, ..Saved::default() });
        };
        write_atomic(&self.path(&name), text)
            .map_err(|e| GridFileError::Io(format!("could not write grid {name}: {e}")))?;
        let applied = name == self.active().await?;
        if applied {
            self.engine.replace_rules(&rules).await.map_err(|e| {
                GridFileError::Io(format!("{name} written but not applied ({e}): run `schedule reload`"))
            })?;
            self.clear_problem();
        }
        Ok(Saved {
            ok: true,
            diagnostics,
            conflict: false,
            revision: revision(text.as_bytes()),
            created: current.is_empty(),
            applied,
        })
    }

    /// Make grid `name` the active one: read, judged, applied, remembered.
    /// A grid with problems is refused: the active grid does not change.
    pub async fn activate(&self, name: &str) -> Result<usize, GridFileError> {
        let name = normalize_name(name)?;
        let _guard = self.lock.lock().await;
        let n = self.apply_file(&name).await?.ok_or_else(|| GridFileError::NotFound(name.clone()))?;
        self.set_active(&name).await?;
        self.clear_problem();
        Ok(n)
    }

    /// Re-read the active grid and apply it (after a hand edit).
    pub async fn reload(&self) -> Result<Loaded, GridFileError> {
        let _guard = self.lock.lock().await;
        let name = self.active().await?;
        let r = self.apply_file(&name).await;
        self.note(&name, &r);
        match r? {
            Some(n) => Ok(Loaded::Applied(n)),
            None => Ok(Loaded::Missing),
        }
    }

    /// Start-up: the active file is the source of truth. A missing or
    /// invalid file leaves the grid last applied in place (never an empty
    /// air), and the problem is kept for `ListGrids`.
    pub async fn load_at_startup(&self) -> Result<(String, Result<Loaded, GridFileError>), GridFileError> {
        let _guard = self.lock.lock().await;
        let name = self.active().await?;
        let r = self.apply_file(&name).await;
        self.note(&name, &r);
        Ok((name, r.map(|o| o.map(Loaded::Applied).unwrap_or(Loaded::Missing))))
    }

    /// `schedule apply <file>`: the grid sent becomes the ACTIVE grid file
    /// (written by stationd, replacing it — its comments included) and is
    /// applied. Several files are merged into one.
    pub async fn apply_external(&self, files: &[(String, String)]) -> Result<(String, Vec<String>), GridFileError> {
        let _guard = self.lock.lock().await;
        let text = match files {
            [(_, t)] => t.clone(),
            _ => {
                let mut rules = Vec::new();
                for (path, t) in files {
                    let (r, diags) = grid_toml::diagnose(t);
                    match r {
                        Some(mut r) => rules.append(&mut r),
                        None => return Err(GridFileError::Invalid { name: path.clone(), diags }),
                    }
                }
                grid_toml::to_toml(&rules).map_err(GridFileError::Io)?
            }
        };
        let (rules, diags) = self.diagnose(&text).await?;
        let name = self.active().await?;
        let Some(rules) = rules else {
            let label = files.first().map(|f| f.0.clone()).unwrap_or_default();
            return Err(GridFileError::Invalid { name: label, diags });
        };
        write_atomic(&self.path(&name), &text)
            .map_err(|e| GridFileError::Io(format!("could not write grid {name}: {e}")))?;
        self.engine.replace_rules(&rules).await?;
        self.clear_problem();
        Ok((name, rules.iter().map(|r| r.id.clone()).collect()))
    }

    /// Read, judge and apply grid file `name`; `Ok(None)` = no file.
    async fn apply_file(&self, name: &str) -> Result<Option<usize>, GridFileError> {
        let Some(text) = self.read_file(name)? else { return Ok(None) };
        let (rules, diags) = self.diagnose(&text).await?;
        let Some(rules) = rules else {
            return Err(GridFileError::Invalid { name: name.to_string(), diags });
        };
        self.engine.replace_rules(&rules).await?;
        Ok(Some(rules.len()))
    }

    fn note(&self, _name: &str, r: &Result<Option<usize>, GridFileError>) {
        let problem = match r {
            // No file: `ListGrids` shows it without revision.
            Ok(_) => None,
            Err(GridFileError::Invalid { diags, .. }) => diags.first().cloned(),
            Err(e) => Some(grid_toml::file_unreadable(e.to_string())),
        };
        *self.problem.lock().expect("grid problem lock") = problem;
    }

    fn clear_problem(&self) {
        *self.problem.lock().expect("grid problem lock") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: &str = "schema_version = 1\n\n# le plancher\n[[rule]]\nid = \"floor\"\nkind = \"base_rotation\"\nplaylist_ref = \"music\"\n";
    const NIGHT: &str = "schema_version = 1\n[[rule]]\nid = \"floor\"\nkind = \"base_rotation\"\nplaylist_ref = \"night\"\n";

    async fn fixture() -> (tempfile::TempDir, GridFiles) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        let toml = "name = \"x\"\n[selection]\nmode = \"dynamic\"";
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        for r in ["music", "night"] {
            crate::store::upsert(&pool, r, &pl, toml, Some(r)).await.unwrap();
        }
        let engine = GridEngine::new(pool.clone(), "UTC");
        let files = GridFiles::new(dir.path().join("grid"), pool, engine);
        (dir, files)
    }

    async fn applied_refs(f: &GridFiles) -> Vec<String> {
        f.engine
            .list_rules()
            .await
            .unwrap()
            .iter()
            .filter_map(|r| grid_toml::playlist_ref_of(r).map(str::to_string))
            .collect()
    }

    #[test]
    fn names_are_files_of_the_grid_directory() {
        assert_eq!(normalize_name("ete").unwrap(), "ete.toml");
        assert_eq!(normalize_name("grid.toml").unwrap(), "grid.toml");
        for bad in ["", " ", "../x", "a/b", ".hidden", "a\\b"] {
            assert!(normalize_name(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn save_creates_then_needs_the_revision_and_applies_the_active_grid() {
        let (_d, f) = fixture().await;
        assert_eq!(f.active().await.unwrap(), "grid.toml");
        let s = f.save("grid", FLOOR, "").await.unwrap();
        assert!(s.ok && s.created && s.applied, "{s:?}");
        assert_eq!(applied_refs(&f).await, ["music"]);
        assert_eq!(std::fs::read_to_string(f.dir().join("grid.toml")).unwrap(), FLOOR, "comments kept");
        // Stale revision: conflict, nothing written.
        let c = f.save("grid", NIGHT, "").await.unwrap();
        assert!(c.conflict && !c.ok);
        assert_eq!(c.revision, s.revision);
        // Right revision: written and applied.
        let s2 = f.save("grid", NIGHT, &s.revision).await.unwrap();
        assert!(s2.ok && s2.applied);
        assert_eq!(applied_refs(&f).await, ["night"]);
    }

    #[tokio::test]
    async fn an_invalid_grid_is_not_written_and_says_where() {
        let (_d, f) = fixture().await;
        let bad = "schema_version = 1\n[[rule]]\nid = \"a\"\nkind = \"day_part\"\nplaylist_ref = \"ghost\"\nstart = \"25:00\"\n[[rule]]\nid = \"b\"\nkind = \"base_rotation\"\nplaylist_ref = \"ghost\"\n";
        let s = f.save("grid", bad, "").await.unwrap();
        assert!(!s.ok && !s.conflict);
        assert!(!f.dir().join("grid.toml").exists());
        let fields: Vec<&str> = s.diagnostics.iter().map(|d| d.field.as_str()).collect();
        assert_eq!(
            fields,
            ["rule[1].start", "rule[2].playlist_ref"],
            "a rule's grammar; the refs of the rules that parse: {:?}",
            s.diagnostics
        );
        // Grammar fine, refs broken: both rules reported.
        let refs = bad.replace("25:00", "07:00");
        let s = f.save("grid", &refs, "").await.unwrap();
        let fields: Vec<&str> = s.diagnostics.iter().map(|d| d.field.as_str()).collect();
        assert_eq!(fields, ["rule[1].playlist_ref", "rule[2].playlist_ref"]);
        assert!(s.diagnostics.iter().all(|d| d.code == grid_toml::GridCode::UnknownRef));
    }

    #[tokio::test]
    async fn another_grid_is_prepared_without_touching_the_air_then_activated() {
        let (_d, f) = fixture().await;
        f.save("grid", FLOOR, "").await.unwrap();
        let s = f.save("ete", NIGHT, "").await.unwrap();
        assert!(s.ok && !s.applied, "not active: not applied");
        assert_eq!(applied_refs(&f).await, ["music"]);
        let list = f.list().await.unwrap();
        let names: Vec<(&str, bool)> = list.iter().map(|g| (g.name.as_str(), g.active)).collect();
        assert_eq!(names, [("ete.toml", false), ("grid.toml", true)]);
        // A draft of it can be projected without being applied.
        assert!(f.source("ete", "").await.unwrap().is_some());
        assert!(f.source("", "").await.unwrap().is_none());
        f.activate("ete").await.unwrap();
        assert_eq!(f.active().await.unwrap(), "ete.toml");
        assert_eq!(applied_refs(&f).await, ["night"]);
        // Remembered: a start-up reloads it.
        let (name, r) = f.load_at_startup().await.unwrap();
        assert_eq!((name.as_str(), r.unwrap()), ("ete.toml", Loaded::Applied(1)));
    }

    #[tokio::test]
    async fn a_missing_or_broken_active_file_keeps_the_grid_on_air() {
        let (_d, f) = fixture().await;
        f.save("grid", FLOOR, "").await.unwrap();
        // Hand-broken file: start-up refuses it, the applied grid stays.
        std::fs::write(f.dir().join("grid.toml"), "schema_version = 1\n[[rule]]\nid = 3\n").unwrap();
        let (_, r) = f.load_at_startup().await.unwrap();
        assert!(matches!(r, Err(GridFileError::Invalid { .. })), "{r:?}");
        assert_eq!(applied_refs(&f).await, ["music"]);
        let active = f.list().await.unwrap().into_iter().find(|g| g.active).unwrap();
        assert!(active.problem.is_some());
        assert!(f.get("").await.unwrap().differs_from_applied);
        // No file at all: same.
        std::fs::remove_file(f.dir().join("grid.toml")).unwrap();
        assert_eq!(f.reload().await.unwrap(), Loaded::Missing);
        assert_eq!(applied_refs(&f).await, ["music"]);
        // Activating a grid that does not exist changes nothing.
        assert!(matches!(f.activate("nope").await, Err(GridFileError::NotFound(_))));
        assert_eq!(f.active().await.unwrap(), "grid.toml");
    }

    #[tokio::test]
    async fn apply_writes_the_active_file() {
        let (_d, f) = fixture().await;
        let (name, ids) = f.apply_external(&[("mine.toml".into(), NIGHT.into())]).await.unwrap();
        assert_eq!((name.as_str(), ids), ("grid.toml", vec!["floor".to_string()]));
        assert_eq!(std::fs::read_to_string(f.dir().join("grid.toml")).unwrap(), NIGHT);
        assert!(!f.get("").await.unwrap().differs_from_applied);
    }
}
