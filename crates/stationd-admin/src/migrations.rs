//! Durable cohorts of Icecast connection IDs. Never identify people by IP or UA.
use crate::{child, rpc::*, Admin};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tonic::{Request, Status};
const MAX_MEMBERS: usize = 2048;
fn storage(_: impl std::fmt::Display) -> Status {
    Status::internal("Migration journal unavailable")
}
fn new_route_marker() -> Result<String, Status> {
    let mut bytes=[0u8;16];
    let mut random=std::fs::File::open("/dev/urandom").map_err(storage)?;
    std::io::Read::read_exact(&mut random,&mut bytes).map_err(storage)?;
    Ok(bytes.iter().map(|byte|format!("{byte:02x}")).collect())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Serialize, Deserialize)]
struct Member {
    id: u64,
    state: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Operation {
    #[serde(default)]
    continuous: bool,
    #[serde(default)]
    route_marker: String,
    id: u64,
    mount: String,
    destination: String,
    epoch: String,
    state: String,
    created_at: u64,
    note: String,
    members: Vec<Member>,
}
impl Operation {
    fn summary(&self) -> ListenerMigration {
        let count = |state: &str| self.members.iter().filter(|m| m.state == state).count() as u32;
        ListenerMigration {
            id: self.id,
            continuous: self.continuous,
            mount: self.mount.clone(),
            destination: self.destination.clone(),
            state: self.state.clone(),
            total: self.members.len() as u32,
            moved: count("moved"),
            returned: count("returned"),
            missing: count("missing") + count("unmoved"),
            uncertain: count("uncertain") + count("in_flight"),
            created_at: self.created_at,
            note: self.note.clone(),
        }
    }
}
pub struct Journal {
    db: Mutex<Connection>,
    run: Arc<tokio::sync::Mutex<()>>,
}
impl Journal {
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        db.busy_timeout(Duration::from_secs(2))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS migrations(id INTEGER PRIMARY KEY AUTOINCREMENT,state TEXT NOT NULL,payload TEXT NOT NULL);")?;
        let journal = Self {
            db: Mutex::new(db),
            run: Arc::new(tokio::sync::Mutex::new(())),
        };
        for mut op in journal.list()? {
            if matches!(op.state.as_str(), "moving" | "returning") {
                op.state = "interrupted".into();
                op.note =
                    "Administration restarted. Inspect or explicitly resume/return this cohort."
                        .into();
                for member in &mut op.members {
                    if member.state == "in_flight" {
                        member.state = "uncertain".into();
                    }
                }
                journal.save(&op)?;
            }
        }
        Ok(journal)
    }
    fn load(&self, id: u64) -> Result<Operation, Status> {
        let db = self.db.lock().map_err(storage)?;
        let payload: String = db
            .query_row(
                "SELECT payload FROM migrations WHERE id=?",
                [id.to_string()],
                |r| r.get(0),
            )
            .map_err(|_| Status::not_found("Migration not found"))?;
        serde_json::from_str(&payload).map_err(storage)
    }
    fn list(&self) -> Result<Vec<Operation>, Status> {
        let db = self.db.lock().map_err(storage)?;
        let mut stmt=db.prepare("SELECT payload FROM migrations ORDER BY CASE WHEN state IN ('complete','expired') THEN 1 ELSE 0 END,id DESC LIMIT 100").map_err(storage)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage)?;
        rows.map(|r| serde_json::from_str(&r.map_err(storage)?).map_err(storage))
            .collect()
    }
    fn insert(&self, mut op: Operation) -> Result<Operation, Status> {
        let mut db = self.db.lock().map_err(storage)?;
        let tx = db.transaction().map_err(storage)?;
        let count: u32 = tx
            .query_row("SELECT count(*) FROM migrations", [], |r| r.get(0))
            .map_err(storage)?;
        let active: u32 = tx
            .query_row(
                "SELECT count(*) FROM migrations WHERE state NOT IN ('complete','expired')",
                [],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if count >= 1000 || active >= 20 {
            return Err(Status::resource_exhausted(
                "Migration journal limit reached",
            ));
        }
        let mut stmt = tx
            .prepare("SELECT payload FROM migrations WHERE state NOT IN ('complete','expired')")
            .map_err(storage)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(storage)?;
        for row in rows {
            let other: Operation = serde_json::from_str(&row.map_err(storage)?).map_err(storage)?;
            if other.epoch == op.epoch
                && (other.continuous || op.continuous)
                && (other.mount == op.mount
                    || other.mount == op.destination
                    || other.destination == op.mount)
            {
                return Err(Status::failed_precondition(
                    "An active route already uses this origin or would form a route chain",
                ));
            }
            if other.epoch == op.epoch
                && other.members.iter().any(|m| {
                    !matches!(m.state.as_str(), "returned" | "missing" | "unmoved")
                        && op.members.iter().any(|n| n.id == m.id)
                })
            {
                return Err(Status::failed_precondition(
                    "A connection is already tracked by an active migration",
                ));
            }
        }
        drop(stmt);
        tx.execute(
            "INSERT INTO migrations(state,payload) VALUES('planned','{}')",
            [],
        )
        .map_err(storage)?;
        op.id = tx.last_insert_rowid() as u64;
        tx.execute(
            "UPDATE migrations SET payload=? WHERE id=?",
            params![
                serde_json::to_string(&op).map_err(storage)?,
                op.id.to_string()
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(op)
    }
    fn save(&self, op: &Operation) -> Result<(), Status> {
        let db = self.db.lock().map_err(storage)?;
        db.execute(
            "UPDATE migrations SET state=?,payload=? WHERE id=? AND state!='expired'",
            params![
                op.state,
                serde_json::to_string(op).map_err(storage)?,
                op.id.to_string()
            ],
        )
        .map_err(storage)?;
        Ok(())
    }
}
impl Admin {
    pub fn with_migration_journal(mut self, path: PathBuf) -> anyhow::Result<Self> {
        self.migrations = Some(Arc::new(Journal::open(path)?));
        Ok(self)
    }
    fn journal(&self) -> Result<Arc<Journal>, Status> {
        self.migrations
            .clone()
            .ok_or_else(|| Status::failed_precondition("Migration journal not configured"))
    }
    async fn icecast_epoch(&self) -> Result<String, Status> {
        let body = self.get("stats", &[]).await?;
        let doc = roxmltree::Document::parse(&body)
            .map_err(|_| Status::unavailable("Invalid Icecast status"))?;
        let root = doc.root_element();
        let start = child(root, "server_start_iso8601");
        let start = if start.is_empty() {
            child(root, "server_start")
        } else {
            start
        };
        if start.len() < 8 || start.len() > 128 {
            return Err(Status::failed_precondition(
                "Icecast startup identity is unavailable; migration refused",
            ));
        }
        Ok(format!("{}|{}", self.upstream, start))
    }
    async fn connection_ids(&self, mount: &str) -> Result<BTreeSet<u64>, Status> {
        // Read raw IDs without GeoIP/history enrichment. No addresses are persisted.
        let body = self.get("listclients", &[("mount", mount)]).await?;
        let doc = roxmltree::Document::parse(&body)
            .map_err(|_| Status::unavailable("Invalid listener list"))?;
        // listclients can resolve a missing origin through its native fallback.
        // Never mistake destination-native listeners for origin arrivals.
        if doc.descendants().find(|n| n.has_tag_name("source")).and_then(|n| n.attribute("mount")).is_some_and(|actual| actual != mount) {
            return Err(Status::not_found("Origin is served by a fallback"));
        }
        let mut ids = BTreeSet::new();
        for node in doc.descendants().filter(|n| n.has_tag_name("listener")) {
            let id = child(node, "id")
                .parse::<u64>()
                .or_else(|_| child(node, "ID").parse())
                .or_else(|_| node.attribute("id").unwrap_or_default().parse())
                .map_err(|_| Status::unavailable("Invalid listener ID"))?;
            // Icecast 2.5's targeted move parses id with atoi; never truncate IDs.
            if id == 0 || id > i32::MAX as u64 {
                return Err(Status::failed_precondition(
                    "Listener ID outside the supported targeted-move range",
                ));
            }
            ids.insert(id);
            if ids.len() > MAX_MEMBERS {
                return Err(Status::resource_exhausted(
                    "Too many connections for one migration",
                ));
            }
        }
        Ok(ids)
    }
    async fn check_migration_streams(&self, from: &str, to: &str) -> Result<(), Status> {
        self.mount(from)?;
        self.mount(to)?;
        if from == to {
            return Err(Status::invalid_argument(
                "Source and destination must differ",
            ));
        }
        let status =
            <Admin as icecast_admin_server::IcecastAdmin>::get_status(self, Request::new(Empty {}))
                .await?
                .into_inner();
        let source = status
            .mounts
            .iter()
            .find(|s| s.mount == from && s.connected)
            .ok_or_else(|| Status::failed_precondition("Origin stream is not connected"))?;
        let dest = status
            .mounts
            .iter()
            .find(|s| s.mount == to && s.connected)
            .ok_or_else(|| Status::failed_precondition("Destination stream is not connected"))?;
        if source.content_type.is_empty() || source.content_type != dest.content_type {
            return Err(Status::failed_precondition(
                "Stream types differ or are unknown",
            ));
        }
        Ok(())
    }
    pub(crate) async fn plan_migration(
        &self,
        req: MigrationPlanRequest,
    ) -> Result<ListenerMigration, Status> {
        let journal = self.journal()?;
        let _guard = journal
            .run
            .clone()
            .try_lock_owned()
            .map_err(|_| Status::failed_precondition("Another migration command is running"))?;
        if req.continuous && self.routing_socket.is_none() {
            return Err(Status::failed_precondition("Stream entry routing is not configured"));
        }
        self.check_migration_streams(&req.mount, &req.destination)
            .await?;
        let epoch = self.icecast_epoch().await?;
        let ids = self.connection_ids(&req.mount).await?;
        if epoch != self.icecast_epoch().await? {
            return Err(Status::failed_precondition(
                "Icecast restarted during preparation",
            ));
        }
        let op = journal.insert(Operation {
            id: 0,
            mount: req.mount,
            continuous: req.continuous,
            route_marker: if req.continuous { new_route_marker()? } else { String::new() },
            destination: req.destination,
            epoch,
            state: "planned".into(),
            created_at: now(),
            note: if req.continuous { "Stream route staged. Activation also transfers future connections, even when currently empty." } else { "Prepared current connections only. New arrivals are not part of this cohort." }.into(),
            members: ids
                .into_iter()
                .map(|id| Member {
                    id,
                    state: "planned".into(),
                })
                .collect(),
        })?;
        Ok(op.summary())
    }
    pub(crate) async fn migration_list(&self) -> Result<ListenerMigrations, Status> {
        let journal = self.journal()?;
        // Serve persisted progress while a routing command owns Icecast. Polling
        // the stats endpoint during a configuration reload can contend with the
        // upstream configuration lock, and adds no identity information here.
        let Ok(_guard) = journal.run.clone().try_lock_owned() else {
            return Ok(ListenerMigrations {operations: journal.list()?.into_iter().map(|op|op.summary()).collect()});
        };
        // A failed read does not invalidate identities; a confirmed restart does.
        let epoch = self.icecast_epoch().await?;
        let mut operations = vec![];
        for mut op in journal.list()? {
            if !matches!(
                op.state.as_str(),
                "complete" | "expired" | "moving" | "returning"
            ) && op.epoch != epoch
            {
                op.state = "expired".into();
                op.note =
                    "Icecast restarted or its origin changed. Old connection IDs cannot be reused."
                        .into();
                journal.save(&op)?;
            }
            operations.push(op.summary());
        }
        Ok(ListenerMigrations { operations })
    }
    pub(crate) async fn cancel_migration(&self, id: u64) -> Result<ListenerMigration, Status> {
        let journal = self.journal()?;
        let _guard = journal
            .run
            .clone()
            .try_lock_owned()
            .map_err(|_| Status::failed_precondition("Another migration command is running"))?;
        let mut op = journal.load(id)?;
        if op.state != "planned" {
            return Err(Status::failed_precondition(
                "Only an unexecuted plan can be cancelled",
            ));
        }
        op.state = "complete".into();
        op.note = "Plan cancelled; no connections moved.".into();
        for member in &mut op.members {
            member.state = "unmoved".into();
        }
        journal.save(&op)?;
        Ok(op.summary())
    }
    pub(crate) async fn schedule_migration(
        &self,
        id: u64,
        returning: bool,
    ) -> Result<ListenerMigration, Status> {
        let journal = self.journal()?;
        let _guard = journal
            .run
            .clone()
            .try_lock_owned()
            .map_err(|_| Status::failed_precondition("Another migration command is running"))?;
        let mut op = journal.load(id)?;
        if matches!(op.state.as_str(), "expired" | "complete") {
            return Err(Status::failed_precondition(
                "This migration is closed or expired",
            ));
        }
        if returning
            && !op.continuous
            && !op
                .members
                .iter()
                .any(|m| matches!(m.state.as_str(), "moved" | "uncertain" | "in_flight"))
        {
            return Err(Status::failed_precondition(
                "No migrated connections to return",
            ));
        }
        let (from, to) = if returning {
            (&op.destination, &op.mount)
        } else {
            (&op.mount, &op.destination)
        };
        self.check_migration_streams(from, to).await?;
        if self.icecast_epoch().await? != op.epoch {
            op.state = "expired".into();
            op.note = "Icecast restarted. Migration IDs invalidated.".into();
            journal.save(&op)?;
            return Err(Status::failed_precondition(
                "Icecast restarted; old connection IDs invalidated",
            ));
        }
        op.state = if returning { "returning" } else { "moving" }.into();
        op.note = "Background operation started; closing the browser does not stop it.".into();
        journal.save(&op)?;
        let summary = op.summary();
        let admin = self.clone();
        tokio::spawn(async move {
            let _guard = _guard;
            let result = async {
                if op.continuous {
                    admin.apply_stream_route(&op, !returning).await?;
                    if returning {
                        // Let connections already handed to the local entry finish
                        // their bounded upstream connect before capturing the return.
                        tokio::time::sleep(Duration::from_millis(3200)).await;
                        admin.track_routed_arrivals(&journal, &mut op).await?;
                    }
                }
                admin.migrate_members(&journal, &mut op, returning).await
            }.await;
            if result.is_err()
            {
                op.state = "interrupted".into();
                op.note="Operation interrupted. Refresh the journal; do not assume every connection moved.".into();
                for member in &mut op.members {
                    if member.state == "in_flight" {
                        member.state = "uncertain".into();
                    }
                }
                let _ = journal.save(&op);
            }
        });
        Ok(summary)
    }
    async fn apply_stream_route(&self, op: &Operation, enabled: bool) -> Result<(), Status> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let path=self.routing_socket.as_ref().ok_or_else(||Status::failed_precondition("Stream entry routing unavailable"))?;
        let data=serde_json::to_vec(&serde_json::json!({"route":{"mount":op.mount,"destination":op.destination,"enabled":enabled,"marker":op.route_marker}})).map_err(storage)?;
        tokio::time::timeout(Duration::from_secs(5),async {
            let mut socket=tokio::net::UnixStream::connect(path).await.map_err(|_|Status::unavailable("Stream routing control unavailable"))?;
            socket.write_all(&data).await.map_err(|_|Status::unavailable("Stream routing outcome unknown"))?;
            socket.shutdown().await.map_err(|_|Status::unavailable("Stream routing outcome unknown"))?;
            let mut response=[0;16];
            let count=socket.read(&mut response).await.map_err(|_|Status::unavailable("Stream routing outcome unknown"))?;
            if &response[..count]!=b"applied\n" {return Err(Status::unavailable("Stream routing reload not confirmed"));}
            Ok(())
        }).await.map_err(|_|Status::deadline_exceeded("Stream routing outcome unknown"))?
    }
    async fn track_routed_arrivals(&self, journal: &Journal, op: &mut Operation) -> Result<(), Status> {
        if self.icecast_epoch().await? != op.epoch { return Err(Status::failed_precondition("Icecast restarted")); }
        let body=self.get("listclients", &[("mount",&op.destination)]).await?;
        let doc=roxmltree::Document::parse(&body).map_err(|_|Status::unavailable("Invalid destination snapshot"))?;
        for node in doc.descendants().filter(|n|n.has_tag_name("listener")) {
            let from_origin = !op.route_marker.is_empty() && child(node, "useragent") == format!("StationDeckRoute/{}", op.route_marker);
            if !from_origin {continue;}
            let id=child(node,"id").parse::<u64>().or_else(|_|node.attribute("id").unwrap_or_default().parse()).map_err(|_|Status::unavailable("Invalid routed connection ID"))?;
            if id==0||id>i32::MAX as u64 {return Err(Status::failed_precondition("Unsupported routed connection ID"));}
            if op.members.iter().any(|m|m.id==id) {continue;}
            if op.members.len()>=MAX_MEMBERS {return Err(Status::resource_exhausted("Route connection limit reached"));}
            op.members.push(Member{id,state:"moved".into()});
        }
        journal.save(op)
    }
    /// Durable rules, not replayed HTTP actions: discover new IDs, journal them,
    /// then perform one verified move. Interrupted actions require operator input.
    pub async fn run_stream_routes(self) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let _ = self.routing_sweep().await;
        }
    }
    async fn routing_sweep(&self) -> Result<(), Status> {
        let journal = self.journal()?;
        let Ok(_guard) = journal.run.clone().try_lock_owned() else {
            return Ok(());
        };
        for mut op in journal
            .list()?
            .into_iter()
            .filter(|op| op.continuous && op.state == "away")
        {
            let result = self.capture_route_arrivals(&journal, &mut op).await;
            if result.is_err() {
                op.state = "interrupted".into();
                op.note = "Rerouting paused. Inspect streams and explicitly resume or restore the route; uncertain moves are never retried automatically.".into();
                for member in &mut op.members {
                    if member.state == "in_flight" {
                        member.state = "uncertain".into();
                    }
                }
                journal.save(&op)?;
            }
        }
        Ok(())
    }
    async fn capture_route_arrivals(
        &self,
        journal: &Journal,
        op: &mut Operation,
    ) -> Result<(), Status> {
        if self.icecast_epoch().await? != op.epoch {
            op.state = "expired".into();
            op.note =
                "Icecast restarted. Rerouting stopped; old connection IDs invalidated.".into();
            return journal.save(op);
        }
        self.track_routed_arrivals(journal, op).await?;
        let arrivals = match self.connection_ids(&op.mount).await {
            Ok(ids) => ids,
            // During VM maintenance the source can disappear. Keep the rule armed.
            Err(error) if error.code() == tonic::Code::NotFound => BTreeSet::new(),
            Err(error) => return Err(error),
        };
        let unseen: Vec<_> = arrivals
            .into_iter()
            .filter(|id| !op.members.iter().any(|m| m.id == *id))
            .collect();
        if unseen.is_empty() {
            return Ok(());
        }
        if op.members.len() + unseen.len() > MAX_MEMBERS {
            return Err(Status::resource_exhausted(
                "Stream route connection limit reached",
            ));
        }
        for other in journal.list()? {
            if other.id != op.id
                && other.epoch == op.epoch
                && !matches!(other.state.as_str(), "complete" | "expired")
                && other.members.iter().any(|m| {
                    unseen.contains(&m.id)
                        && !matches!(m.state.as_str(), "returned" | "missing" | "unmoved")
                })
            {
                return Err(Status::failed_precondition(
                    "New connection already tracked by another operation",
                ));
            }
        }
        op.members.extend(unseen.into_iter().map(|id| Member {
            id,
            state: "planned".into(),
        }));
        journal.save(op)?;
        self.migrate_members(journal, op, false).await
    }
    async fn migrate_members(
        &self,
        journal: &Journal,
        op: &mut Operation,
        returning: bool,
    ) -> Result<(), Status> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        let (from, to) = if returning {
            (op.destination.clone(), op.mount.clone())
        } else {
            (op.mount.clone(), op.destination.clone())
        };
        for index in 0..op.members.len() {
            if returning && op.members[index].state == "planned" {
                op.members[index].state = "unmoved".into();
                journal.save(op)?;
                continue;
            }
            let state = op.members[index].state.as_str();
            if matches!(state, "returned" | "missing" | "unmoved")
                || (!returning && state == "moved")
                || (returning && state == "planned")
            {
                continue;
            }
            if tokio::time::Instant::now() > deadline {
                return Err(Status::deadline_exceeded("Migration batch deadline"));
            }
            if self.icecast_epoch().await? != op.epoch {
                op.state = "expired".into();
                op.note = "Icecast restarted. Remaining IDs will not be used.".into();
                journal.save(op)?;
                return Ok(());
            }
            let id = op.members[index].id;
            let source = self.connection_ids(&from).await?;
            let dest = self.connection_ids(&to).await?;
            if dest.contains(&id) {
                op.members[index].state = if returning { "returned" } else { "moved" }.into();
                journal.save(op)?;
                continue;
            }
            if !source.contains(&id) {
                op.members[index].state = "missing".into();
                journal.save(op)?;
                continue;
            }
            op.members[index].state = "in_flight".into();
            journal.save(op)?;
            // Once only. HTTP success is not evidence that this ID moved.
            self.action(
                "moveclients",
                &[
                    ("mount", &from),
                    ("destination", &to),
                    ("id", &id.to_string()),
                ],
            )
            .await?;
            let mut verified = false;
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if self.icecast_epoch().await? != op.epoch {
                    op.state = "expired".into();
                    op.note = "Icecast restarted during a move; remaining IDs invalidated.".into();
                    journal.save(op)?;
                    return Ok(());
                }
                if self.connection_ids(&to).await?.contains(&id) {
                    verified = true;
                    break;
                }
            }
            op.members[index].state = if verified {
                if returning {
                    "returned"
                } else {
                    "moved"
                }
            } else {
                "uncertain"
            }
            .into();
            journal.save(op)?;
        }
        op.state = if op
            .members
            .iter()
            .any(|m| matches!(m.state.as_str(), "uncertain" | "in_flight"))
        {
            "interrupted"
        } else if returning || (!op.continuous && !op.members.iter().any(|m| m.state == "moved")) {
            "complete"
        } else {
            "away"
        }
        .into();
        op.note=if returning{"Route restored. Native destination listeners were not selected."}else if op.continuous {"Rerouting active, including at zero listeners. New origin connections are transferred automatically."}else{"Cohort moved. New arrivals are not included; return only targets recorded connection IDs."}.into();
        journal.save(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn operation() -> Operation {
        Operation {
            id: 0,
            mount: "/two".into(),
            destination: "/three".into(),
            epoch: "instance-start-1".into(),
            continuous: false,
            route_marker: String::new(),
            state: "planned".into(),
            created_at: now(),
            note: String::new(),
            members: vec![Member {
                id: 42,
                state: "planned".into(),
            }],
        }
    }
    #[test]
    fn journal_survives_restart_and_preserves_uncertain_inflight_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.sqlite");
        let journal = Journal::open(path.clone()).unwrap();
        let mut op = journal.insert(operation()).unwrap();
        op.state = "moving".into();
        op.members[0].state = "in_flight".into();
        journal.save(&op).unwrap();
        drop(journal);
        let journal = Journal::open(path).unwrap();
        let recovered = journal.load(op.id).unwrap();
        assert_eq!(recovered.state, "interrupted");
        assert_eq!(recovered.members[0].id, 42);
        assert_eq!(recovered.summary().uncertain, 1);
        assert!(journal.insert(operation()).is_err());
    }
    #[test]
    fn expired_ids_cannot_be_resurrected_and_completed_cohorts_release_their_ids() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path().join("journal.sqlite")).unwrap();
        let mut op = journal.insert(operation()).unwrap();
        op.state = "expired".into();
        journal.save(&op).unwrap();
        op.state = "moving".into();
        journal.save(&op).unwrap();
        assert_eq!(journal.load(op.id).unwrap().state, "expired");
        assert!(journal.insert(operation()).is_ok());
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    struct Icecast {
        epoch: String,
        source: BTreeSet<u64>,
        destination: BTreeSet<u64>,
        moves: usize,
        lose_reply: bool,
        route_marker: String,
    }
    async fn fixture(path: PathBuf) -> (Admin, Arc<Mutex<Icecast>>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());        let bridge=tokio::net::UnixListener::bind(path.with_extension("sock")).unwrap();
        tokio::spawn(async move {loop {let (mut socket,_)=bridge.accept().await.unwrap();tokio::spawn(async move {let mut data=vec![];socket.read_to_end(&mut data).await.unwrap();let _:serde_json::Value=serde_json::from_slice(&data).unwrap();socket.write_all(b"applied\n").await.unwrap();});}});
        let state = Arc::new(Mutex::new(Icecast {
            epoch: "2026-10-19T01:00:00+0000".into(),
            source: BTreeSet::from([42]),
            destination: BTreeSet::from([77]),
            moves: 0,
            lose_reply: false,
            route_marker: String::new(),
        }));
        let data = state.clone();

        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let data = data.clone();
                tokio::spawn(async move {
                    let mut request = vec![];
                    loop {
                        let mut bytes = [0; 1024];
                        let n = socket.read(&mut bytes).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        request.extend_from_slice(&bytes[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8(request).unwrap();
                    assert!(request.to_ascii_lowercase().contains("connection: close"));
                    let target = request
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap();
                    let url = reqwest::Url::parse(&format!("http://fixture{target}")).unwrap();
                    let query = url
                        .query_pairs()
                        .into_owned()
                        .collect::<std::collections::BTreeMap<_, _>>();
                    let (body, lose_reply) = {
                        let mut state = data.lock().unwrap();
                        match url.path() {
                        "/admin/stats"=>(format!("<icestats><server_start_iso8601>{}</server_start_iso8601><source mount='/two'><listeners>{}</listeners><server_type>audio/mpeg</server_type></source><source mount='/three'><listeners>{}</listeners><server_type>audio/mpeg</server_type></source></icestats>",state.epoch,state.source.len(),state.destination.len()),false),
                        "/admin/listclients"=>{
                            let ids=if query.get("mount").unwrap()=="/two"{&state.source}else{&state.destination};
                            (format!("<icestats><source>{}</source></icestats>",ids.iter().map(|id|format!("<listener><id>{id}</id>{}</listener>",if *id>=100 {format!("<useragent>StationDeckRoute/{}</useragent>",state.route_marker)} else {String::new()})).collect::<String>()),false)
                        }
                        "/admin/moveclients"=>{
                            let id=query.get("id").unwrap().parse::<u64>().unwrap();state.moves+=1;
                            if query.get("mount").unwrap()=="/two" {if state.source.remove(&id){state.destination.insert(id);}}
                            else if state.destination.remove(&id){state.source.insert(id);}
                            let lose=state.lose_reply;state.lose_reply=false;
                            ("<iceresponse><return>1</return></iceresponse>".into(),lose)
                        }
                        _=>panic!("Unexpected path"),
                    }
                    };
                    if lose_reply {
                        return;
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: Keep-Alive\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        let text = r#"location="Fixture"
hostname="fixture.local"
admin_email="admin@fixture.local"
port=8000
max_clients=32
admin_user="admin"
admin_password_file="admin-password"
[[mounts]]
name="/two"
password_file="source-two"
[[mounts]]
name="/three"
password_file="source-three"
"#;
        let admin = Admin {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .no_proxy()
                .build()
                .unwrap(),
            upstream: reqwest::Url::parse(&origin).unwrap(),
            config: crate::Config::parse(text).unwrap(),
            raw_config: text.into(),
            effective_config: None,
            geoip: None,
            password: "fixture".into(),
            router: None,
            history: None,
            monitor: None,
            migrations: Some(Arc::new(Journal::open(path.clone()).unwrap())),
            routing_socket: Some(path.with_extension("sock")),
        };
        (admin, state, task)
    }
    async fn wait(admin: &Admin, id: u64, state: &str) -> ListenerMigration {
        for _ in 0..240 {
            let op = admin.journal().unwrap().load(id).unwrap();
            if op.state == state {
                return op.summary();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        panic!(
            "Expected {state}, found {}",
            admin.journal().unwrap().load(id).unwrap().state
        );
    }
    #[tokio::test]
    async fn return_preserves_native_and_reconnected_destination_listeners() {
        let dir = tempfile::tempdir().unwrap();
        let (admin, state, task) = fixture(dir.path().join("journal.sqlite")).await;
        let op = admin
            .plan_migration(MigrationPlanRequest {
                continuous: false,
                mount: "/two".into(),
                destination: "/three".into(),
            })
            .await
            .unwrap();
        assert_eq!(state.lock().unwrap().moves, 0);
        admin.schedule_migration(op.id, false).await.unwrap();
        wait(&admin, op.id, "away").await;
        {
            let mut state = state.lock().unwrap();
            state.destination.remove(&42);
            state.destination.insert(99);
        }
        admin.schedule_migration(op.id, true).await.unwrap();
        let op = wait(&admin, op.id, "complete").await;
        assert_eq!(op.missing, 1);
        let state = state.lock().unwrap();
        assert_eq!(state.destination, BTreeSet::from([77, 99]));
        assert!(state.source.is_empty());
        assert_eq!(state.moves, 1);
        task.abort();
    }
    #[tokio::test]
    async fn lost_reply_is_not_retried_and_explicit_return_reconciles_the_id() {
        let dir = tempfile::tempdir().unwrap();
        let (admin, state, task) = fixture(dir.path().join("journal.sqlite")).await;
        let op = admin
            .plan_migration(MigrationPlanRequest {
                continuous: false,
                mount: "/two".into(),
                destination: "/three".into(),
            })
            .await
            .unwrap();
        state.lock().unwrap().lose_reply = true;
        admin.schedule_migration(op.id, false).await.unwrap();
        let failed = wait(&admin, op.id, "interrupted").await;
        assert_eq!(failed.uncertain, 1);
        assert_eq!(state.lock().unwrap().moves, 1);
        admin.schedule_migration(op.id, true).await.unwrap();
        let result = wait(&admin, op.id, "complete").await;
        assert_eq!(result.returned, 1);
        let state = state.lock().unwrap();
        assert_eq!(state.source, BTreeSet::from([42]));
        assert_eq!(state.destination, BTreeSet::from([77]));
        assert_eq!(state.moves, 2);
        task.abort();
    }
    #[tokio::test]
    async fn restart_identity_rejects_reused_ids_without_touching_them() {
        let dir = tempfile::tempdir().unwrap();
        let (admin, state, task) = fixture(dir.path().join("journal.sqlite")).await;
        let op = admin
            .plan_migration(MigrationPlanRequest {
                continuous: false,
                mount: "/two".into(),
                destination: "/three".into(),
            })
            .await
            .unwrap();
        admin.schedule_migration(op.id, false).await.unwrap();
        wait(&admin, op.id, "away").await;
        state.lock().unwrap().epoch = "2026-10-20T01:00:00+0000".into();
        assert!(admin.schedule_migration(op.id, true).await.is_err());
        assert_eq!(
            admin.journal().unwrap().load(op.id).unwrap().state,
            "expired"
        );
        let state = state.lock().unwrap();
        assert_eq!(state.moves, 1);
        assert_eq!(state.destination, BTreeSet::from([42, 77]));
        task.abort();
    }
    #[tokio::test]
    async fn empty_stream_rule_survives_admin_restart_and_tracks_future_arrivals_until_return() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("journal.sqlite");
        let (mut admin,state,task)=fixture(path.clone()).await;
        state.lock().unwrap().source.clear();
        let op=admin.plan_migration(MigrationPlanRequest {mount:"/two".into(),destination:"/three".into(),continuous:true}).await.unwrap();
        assert_eq!(op.total,0);
        admin.schedule_migration(op.id,false).await.unwrap();
        assert!(wait(&admin,op.id,"away").await.continuous);
        // With no IDs, overlapping/cyclic stream rules must still be rejected.
        assert!(admin.plan_migration(MigrationPlanRequest {mount:"/three".into(),destination:"/two".into(),continuous:true}).await.is_err());
        admin.migrations=Some(Arc::new(Journal::open(path).unwrap()));
        state.lock().unwrap().route_marker = admin.journal().unwrap().load(op.id).unwrap().route_marker.clone();
        state.lock().unwrap().destination.insert(101);
        admin.routing_sweep().await.unwrap();
        assert_eq!(admin.journal().unwrap().load(op.id).unwrap().summary().moved,1);
        state.lock().unwrap().source.insert(102);
        admin.routing_sweep().await.unwrap();
        assert_eq!(state.lock().unwrap().destination,BTreeSet::from([77,101,102]));
        admin.schedule_migration(op.id,true).await.unwrap();
        assert_eq!(wait(&admin,op.id,"complete").await.returned,2);
        state.lock().unwrap().source.insert(103);
        admin.routing_sweep().await.unwrap();
        assert_eq!(state.lock().unwrap().source,BTreeSet::from([101,102,103]));
        assert_eq!(state.lock().unwrap().destination,BTreeSet::from([77]));
        task.abort();
    }
    #[tokio::test]
    async fn empty_route_can_be_restored_and_restart_expires_a_rule_without_any_listeners() {
        let dir=tempfile::tempdir().unwrap();let (admin,state,task)=fixture(dir.path().join("journal.sqlite")).await;
        state.lock().unwrap().source.clear();
        let plan=|| MigrationPlanRequest {mount:"/two".into(),destination:"/three".into(),continuous:true};
        let op=admin.plan_migration(plan()).await.unwrap();
        admin.schedule_migration(op.id,false).await.unwrap();wait(&admin,op.id,"away").await;
        admin.schedule_migration(op.id,true).await.unwrap();wait(&admin,op.id,"complete").await;
        let op=admin.plan_migration(plan()).await.unwrap();
        admin.schedule_migration(op.id,false).await.unwrap();wait(&admin,op.id,"away").await;
        state.lock().unwrap().epoch="another-server-start".into();
        admin.routing_sweep().await.unwrap();
        assert_eq!(admin.journal().unwrap().load(op.id).unwrap().state,"expired");
        assert_eq!(state.lock().unwrap().moves,0);task.abort();
    }
    #[tokio::test]
    async fn lost_reply_for_a_new_arrival_pauses_rule_without_automatic_retry() {
        let dir=tempfile::tempdir().unwrap();let (admin,state,task)=fixture(dir.path().join("journal.sqlite")).await;
        state.lock().unwrap().source.clear();
        let op=admin.plan_migration(MigrationPlanRequest {mount:"/two".into(),destination:"/three".into(),continuous:true}).await.unwrap();
        admin.schedule_migration(op.id,false).await.unwrap();wait(&admin,op.id,"away").await;
        {let mut s=state.lock().unwrap();s.source.insert(101);s.lose_reply=true;}
        admin.routing_sweep().await.unwrap();
        assert_eq!(admin.journal().unwrap().load(op.id).unwrap().state,"interrupted");
        admin.routing_sweep().await.unwrap();assert_eq!(state.lock().unwrap().moves,1);
        admin.schedule_migration(op.id,true).await.unwrap();
        assert_eq!(wait(&admin,op.id,"complete").await.returned,1);
        assert_eq!(state.lock().unwrap().destination,BTreeSet::from([77]));task.abort();
    }
}
