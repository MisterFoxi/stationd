use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use stationdeck_services::config::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPolicy {
    pub id: String,
    pub token_file: String,
    pub plugins: Vec<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorPolicy {
    pub token_file: String,
    pub targets: Vec<String>,
    pub permissions: Vec<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    pub authority: String,
    pub schemas: Vec<Schema>,
    pub targets: Vec<TargetPolicy>,
    pub operators: Vec<OperatorPolicy>,
}
#[derive(Clone)]
pub struct Credentials {
    pub digest: [u8; 32],
    pub target: Option<String>,
    pub targets: Vec<String>,
    pub permissions: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Database {
    format: u32,
    authority: String,
    targets: BTreeMap<String, TargetView>,
}
#[derive(Clone)]
pub struct Store {
    pub bootstrap: Arc<Bootstrap>,
    credentials: Arc<Vec<Credentials>>,
    data: Arc<Mutex<Database>>,
    path: PathBuf,
    _lock: Arc<fs::File>,
}
#[derive(Debug)]
pub struct Error(pub u16, pub &'static str);
impl Bootstrap {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_id(&self.authority)
            || self.targets.is_empty()
            || self.targets.len() > 128
            || self.schemas.len() > 128
            || self.operators.len() > 64
        {
            return Err("Invalid authority catalog.");
        }
        let mut ids = BTreeSet::new();
        for s in &self.schemas {
            if !valid_id(&s.plugin)
                || s.version == 0
                || !ids.insert((&s.plugin, s.version))
                || !matches!(s.apply_mode.as_str(), "hot" | "reload" | "restart")
                || s.fields.len() > 64
            {
                return Err("Invalid plugin schema catalog.");
            }
            let mut keys = BTreeSet::new();
            for f in &s.fields {
                if !valid_id(&f.key)
                    || !keys.insert(&f.key)
                    || !matches!(f.kind.as_str(), "integer" | "boolean" | "string")
                    || matches!((f.minimum,f.maximum),(Some(a),Some(b)) if a>b)
                {
                    return Err("Invalid plugin field schema.");
                }
            }
            if s.requires
                .iter()
                .any(|id| !self.schemas.iter().any(|other| &other.plugin == id))
            {
                return Err("Unknown plugin dependency.");
            }
        }
        let mut targets = BTreeSet::new();
        for t in &self.targets {
            if !valid_id(&t.id)
                || !valid_id(&t.token_file)
                || !targets.insert(&t.id)
                || t.plugins.is_empty()
                || t.plugins
                    .iter()
                    .any(|p| !self.schemas.iter().any(|s| &s.plugin == p))
            {
                return Err("Invalid target policy.");
            }
        }
        for op in &self.operators {
            if !valid_id(&op.token_file)
                || op.targets.is_empty()
                || op.targets.iter().any(|id| !targets.contains(id))
                || op.permissions.is_empty()
                || op
                    .permissions
                    .iter()
                    .any(|p| !matches!(p.as_str(), "read" | "edit" | "publish"))
            {
                return Err("Invalid operator policy.");
            }
        }
        Ok(())
    }
}
impl Store {
    pub fn open(bootstrap: Bootstrap, path: &Path, secrets: &Path) -> Result<Self, &'static str> {
        bootstrap.validate()?;
        fs::create_dir_all(path.parent().ok_or("Invalid database path.")?)
            .map_err(|_| "Cannot create database directory.")?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .map_err(|_| "Cannot open database lock.")?;
        lock.try_lock()
            .map_err(|_| "Configuration database is already in use.")?;
        let mut credentials = Vec::new();
        for t in &bootstrap.targets {
            credentials.push(Credentials {
                digest: Sha256::digest(
                    crate::read_token(&secrets.join("agents").join(&t.token_file).join("token"))?
                        .as_bytes(),
                )
                .into(),
                target: Some(t.id.clone()),
                targets: vec![],
                permissions: vec![],
            });
        }
        for op in &bootstrap.operators {
            credentials.push(Credentials {
                digest: Sha256::digest(
                    crate::read_token(&secrets.join("authors").join(&op.token_file))?.as_bytes(),
                )
                .into(),
                target: None,
                targets: op.targets.clone(),
                permissions: op.permissions.clone(),
            });
        }
        let mut unique = BTreeSet::new();
        if credentials.iter().any(|c| !unique.insert(c.digest)) {
            return Err("Credentials must be unique for each principal.");
        }
        let mut db = if path.exists() {
            let bytes = fs::read(path).map_err(|_| "Cannot read configuration database.")?;
            if bytes.len() > 32 * 1024 * 1024 {
                return Err("Configuration database exceeds the size limit.");
            }
            let db: Database = serde_json::from_slice(&bytes)
                .map_err(|_| "Configuration database is corrupt; refusing to reset it.")?;
            if db.format != 1 || db.authority != bootstrap.authority {
                return Err("Configuration database authority or format mismatch.");
            }
            db
        } else {
            Database {
                format: 1,
                authority: bootstrap.authority.clone(),
                targets: BTreeMap::new(),
            }
        };
        for t in &bootstrap.targets {
            let view = db
                .targets
                .entry(t.id.clone())
                .or_insert_with(|| TargetView {
                    id: t.id.clone(),
                    ..Default::default()
                });
            if view.id != t.id {
                return Err("Invalid persisted target identity.");
            }
            if !view.draft.is_empty() {
                validate(&bootstrap.schemas, &t.plugins, &view.draft)?;
            }
            if let Some(b) = &view.published {
                if b.authority != bootstrap.authority || b.target != t.id || b.revision == 0 {
                    return Err("Invalid persisted publication.");
                }
                validate(&bootstrap.schemas, &t.plugins, &b.documents)?;
            }
        }
        Ok(Self {
            bootstrap: Arc::new(bootstrap),
            credentials: Arc::new(credentials),
            data: Arc::new(Mutex::new(db)),
            path: path.into(),
            _lock: Arc::new(lock),
        })
    }
    pub fn authenticate(&self, token: &str) -> Option<Credentials> {
        if token.len() > 512 {
            return None;
        }
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        self.credentials
            .iter()
            .find(|c| bool::from(c.digest.ct_eq(&digest)))
            .cloned()
    }
    pub fn view(&self, id: &str) -> Result<TargetView, Error> {
        if !self.bootstrap.targets.iter().any(|t| t.id == id) {
            return Err(Error(404, "Unknown target."));
        }
        self.data
            .lock()
            .map_err(|_| Error(500, "Configuration store is unavailable."))?
            .targets
            .get(id)
            .cloned()
            .ok_or(Error(404, "Unknown target."))
    }
    fn change(
        &self,
        id: &str,
        operation: impl FnOnce(&mut TargetView) -> Result<(), Error>,
    ) -> Result<TargetView, Error> {
        let mut db = self
            .data
            .lock()
            .map_err(|_| Error(500, "Configuration store is unavailable."))?;
        let mut next = db.clone();
        let view = next
            .targets
            .get_mut(id)
            .ok_or(Error(404, "Unknown target."))?;
        operation(view)?;
        let result = view.clone();
        let bytes = serde_json::to_vec(&next)
            .map_err(|_| Error(500, "Cannot encode configuration state."))?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(Error(507, "Configuration history storage limit reached."));
        }
        crate::atomic_write(&self.path, &bytes).map_err(|_| {
            Error(
                500,
                "Cannot persist configuration state; refresh before retrying.",
            )
        })?;
        *db = next;
        Ok(result)
    }
    pub fn save(&self, id: &str, input: DraftRequest) -> Result<TargetView, Error> {
        let target = self
            .bootstrap
            .targets
            .iter()
            .find(|t| t.id == id)
            .ok_or(Error(404, "Unknown target."))?;
        validate(&self.bootstrap.schemas, &target.plugins, &input.documents)
            .map_err(|e| Error(422, e))?;
        self.change(id, |v| {
            if input.base_draft != v.draft_version {
                return Err(Error(409, "The draft changed. Reload before editing."));
            }
            v.draft_version = v
                .draft_version
                .checked_add(1)
                .ok_or(Error(409, "Draft version limit reached."))?;
            v.draft = input.documents;
            Ok(())
        })
    }
    pub fn publish(&self, id: &str, input: PublishRequest) -> Result<TargetView, Error> {
        let target = self
            .bootstrap
            .targets
            .iter()
            .find(|t| t.id == id)
            .ok_or(Error(404, "Unknown target."))?;
        self.change(id, |v| {
            if input.draft_version != v.draft_version
                || input.base_revision != v.published.as_ref().map_or(0, |b| b.revision)
            {
                return Err(Error(
                    409,
                    "The configuration changed. Reload before publishing.",
                ));
            }
            validate(&self.bootstrap.schemas, &target.plugins, &v.draft)
                .map_err(|e| Error(422, e))?;
            let revision = input
                .base_revision
                .checked_add(1)
                .ok_or(Error(409, "Revision limit reached."))?;
            if v.history.len() >= 128 {
                return Err(Error(507, "Publication history limit reached."));
            }
            let bundle = Bundle {
                authority: self.bootstrap.authority.clone(),
                target: id.into(),
                revision,
                documents: v.draft.clone(),
            };
            v.history.push(bundle.clone());
            v.published = Some(bundle);
            Ok(())
        })
    }
    pub fn report(&self, id: &str, revision: u64, applied: bool, code: &str) -> Result<(), Error> {
        if !matches!(
            code,
            "applied" | "validation_failed" | "apply_failed" | "cache_failed"
        ) || applied != (code == "applied")
        {
            return Err(Error(422, "Invalid application report."));
        }
        self.change(id, |v| {
            if revision == 0
                || revision > v.published.as_ref().map_or(0, |b| b.revision)
                || v.report.as_ref().is_some_and(|r| r.revision > revision)
            {
                return Err(Error(409, "Unknown or obsolete applied revision."));
            }
            if applied {
                v.applied_revision = revision;
            }
            v.report = Some(AgentReport {
                revision,
                applied,
                code: code.into(),
                observed_at: crate::now(),
            });
            Ok(())
        })
        .map(|_| ())
    }
}
