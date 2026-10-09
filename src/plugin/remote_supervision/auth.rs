//! Webmin identity store and safe WebAuthn ceremonies. All mutations are
//! serialized under Auth's mutex and persisted atomically with their audit.
use super::Config;
use crate::plugin_db::{Params, PluginDb, Statement};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use webauthn_rs::prelude::*;

pub(super) const MIGRATION: &str = "CREATE TABLE webmin_identity (id INTEGER PRIMARY KEY CHECK(id = 1), value TEXT NOT NULL); CREATE TABLE webmin_audit (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, actor TEXT NOT NULL, action TEXT NOT NULL, result TEXT NOT NULL, station TEXT NOT NULL); CREATE INDEX webmin_audit_at ON webmin_audit(at);";
const CHALLENGE_TTL: i64 = 120;

pub(super) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn token() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
fn hash(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}
fn secret_eq(a: &str, b: &str) -> bool {
    bool::from(hash(a).as_bytes().ct_eq(hash(b).as_bytes()))
}
fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement {
        sql: sql.into(),
        params: Params::Positional(values),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum Role {
    Viewer,
    Helper,
    Admin,
}
impl Role {
    pub(super) fn permits(self, permission: &str) -> bool {
        match permission {
            "station.read" | "queue.read" | "agenda.read" | "events.read" => true,
            "player.skip" | "schedule.reload" | "alerts.acknowledge" => self != Self::Viewer,
            "console.open" | "engine.restart" | "station.restart" => self == Self::Admin,
            _ => false,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
struct User {
    id: Uuid,
    name: String,
    active: bool,
    grants: BTreeMap<String, Role>,
    passkeys: Vec<Passkey>,
    #[serde(default)]
    password_hash: Option<String>,
    enrollment: Option<Enrollment>,
}
#[derive(Clone, Deserialize, Serialize)]
struct Enrollment {
    hash: String,
    expires: i64,
}
#[derive(Clone, Deserialize, Serialize)]
struct Session {
    id: Uuid,
    user: Uuid,
    credential: String,
    csrf: String,
    expires: i64,
}
#[derive(Clone, Default, Deserialize, Serialize)]
struct Store {
    origin: String,
    users: Vec<User>,
    sessions: BTreeMap<String, Session>,
}

enum Ceremony {
    Register {
        user: Uuid,
        enrollment: String,
        expires: i64,
        state: PasskeyRegistration,
    },
    Login {
        user: Uuid,
        expires: i64,
        state: PasskeyAuthentication,
    },
}
impl Ceremony {
    fn expires(&self) -> i64 {
        match self {
            Self::Register { expires, .. } | Self::Login { expires, .. } => *expires,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum AdminRequest {
    Add {
        name: String,
        role: Role,
        stations: Vec<String>,
    },
    List,
    Show {
        name: String,
    },
    Enroll {
        name: String,
    },
    Role {
        name: String,
        station: String,
        role: Role,
    },
    Revoke {
        name: String,
    },
    Purge {
        name: String,
    },
    RevokeDevice {
        name: String,
        credential: String,
    },
    Sessions,
    RevokeSession {
        id: Uuid,
    },
    RevokeSessions {
        name: String,
    },
    Audit,
}

pub(super) struct Auth {
    config: Config,
    db: Arc<PluginDb>,
    webauthn: Webauthn,
    dummy_password_hash: String,
    store: Store,
    challenges: HashMap<String, Ceremony>,
    attempts: HashMap<String, (i64, u32)>,
    changes: tokio::sync::watch::Sender<u64>,
}

impl Auth {
    pub(super) fn open(config: &Config, db: Arc<PluginDb>) -> Result<Self, String> {
        let url = Url::parse(&config.public_url).map_err(|_| "invalid public_url")?;
        let rp = url
            .domain()
            .ok_or("passkeys require a DNS public_url (not an IP address)")?;
        let webauthn = WebauthnBuilder::new(rp, &url)
            .map_err(|_| "invalid WebAuthn relying party")?
            .rp_name("StationD Remote")
            .build()
            .map_err(|_| "cannot initialise WebAuthn")?;
        let rows = db
            .query(
                "SELECT value FROM webmin_identity WHERE id = 1",
                &Params::default(),
            )
            .map_err(|e| e.to_string())?;
        let store: Store = match rows.rows.first() {
            Some(row) => serde_json::from_value(
                row[0]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .ok_or("invalid identity store")?,
            )
            .map_err(|_| "invalid identity store")?,
            None => Store {
                origin: url.origin().ascii_serialization(),
                ..Default::default()
            },
        };
        if store.origin != url.origin().ascii_serialization() {
            return Err(
                "public_url changed: restore its original origin before using existing identities"
                    .into(),
            );
        }
        Ok(Self {
            config: config.clone(),
            db,
            webauthn,
            dummy_password_hash: Self::hash_password(&token())?,
            store,
            challenges: HashMap::new(),
            attempts: HashMap::new(),
            changes: tokio::sync::watch::channel(0).0,
        })
    }

    fn save(
        &mut self,
        mut store: Store,
        actor: &str,
        action: &str,
        result: &str,
        station: &str,
    ) -> Result<(), String> {
        store.sessions.retain(|_, s| s.expires > now());
        let data = serde_json::to_string(&store).map_err(|_| "cannot serialize identities")?;
        self.db.batch(&[
            stmt("INSERT INTO webmin_identity(id,value) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET value=excluded.value", vec![json!(data)]),
            stmt("INSERT INTO webmin_audit(at,actor,action,result,station) VALUES(?1,?2,?3,?4,?5)", vec![json!(now()),json!(actor),json!(action),json!(result),json!(station)]),
            stmt("DELETE FROM webmin_audit WHERE at < ?1", vec![json!(now() - 90*86400)]),
        ]).map_err(|_| "identity storage unavailable")?;
        self.store = store;
        self.changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }
    fn audit_failure(&self, action: &str) -> Result<(), String> {
        self.db.batch(&[
            stmt("INSERT INTO webmin_audit(at,actor,action,result,station) VALUES(?1,'anonymous',?2,'denied','')",vec![json!(now()),json!(action)]),
            stmt("DELETE FROM webmin_audit WHERE at < ?1",vec![json!(now()-90*86400)]),
        ]).map_err(|_| "identity storage unavailable")?;
        Ok(())
    }
    fn user(&self, name: &str) -> Result<&User, String> {
        self.store
            .users
            .iter()
            .find(|u| u.name == name && u.active)
            .ok_or_else(|| "unknown or revoked user".into())
    }
    fn by_id(&self, id: Uuid) -> Result<&User, String> {
        self.store
            .users
            .iter()
            .find(|u| u.id == id && u.active)
            .ok_or_else(|| "authentication denied".into())
    }
    fn public_user(user: &User) -> Value {
        json!({"id":user.id,"name":user.name,"active":user.active,"grants":user.grants,"devices":user.passkeys.iter().map(|p|URL_SAFE_NO_PAD.encode(p.cred_id().as_ref())).collect::<Vec<_>>()})
    }
    fn known_station(&self, station: &str) -> bool {
        self.config.stations.iter().any(|s| s.id == station)
    }
    fn issue_enrollment(&mut self, name: &str) -> Result<Value, String> {
        let raw = token();
        let expires = now() + self.config.enrollment_ttl_seconds as i64;
        let mut store = self.store.clone();
        let user = store
            .users
            .iter_mut()
            .find(|u| u.name == name && u.active)
            .ok_or("unknown or revoked user")?;
        if user.passkeys.len() >= 8 {
            return Err("maximum of 8 passkeys per user".into());
        }
        user.enrollment = Some(Enrollment {
            hash: hash(&raw),
            expires,
        });
        self.save(store, "local-admin", "user.enroll", "success", "")?;
        Ok(
            json!({"name":name,"expires_at":expires,"enrollment_url":format!("{}/enroll#{}", self.config.public_url.trim_end_matches('/'),raw)}),
        )
    }

    pub(super) fn admin(&mut self, request: AdminRequest) -> Result<Value, String> {
        match request {
            AdminRequest::List => {
                return Ok(json!(self
                    .store
                    .users
                    .iter()
                    .map(Self::public_user)
                    .collect::<Vec<_>>()))
            }
            AdminRequest::Show { name } => return Ok(Self::public_user(self.user(&name)?)),
            AdminRequest::Enroll { name } => return self.issue_enrollment(&name),
            AdminRequest::Sessions => {
                return Ok(json!(self
                    .store
                    .sessions
                    .values()
                    .filter(|s| s.expires > now())
                    .map(|s| json!({"id":s.id,"user":s.user,"expires_at":s.expires}))
                    .collect::<Vec<_>>()))
            }
            AdminRequest::Audit => {
                let rows = self.db.query("SELECT at,actor,action,result,station FROM webmin_audit ORDER BY id DESC LIMIT 200", &Params::default()).map_err(|_| "audit unavailable")?;
                return Ok(json!({"columns":rows.columns,"rows":rows.rows}));
            }
            _ => {}
        }
        let mut store = self.store.clone();
        let (action, target) = match request {
            AdminRequest::Add {
                name,
                role,
                stations,
            } => {
                if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control)
                {
                    return Err("invalid user name".into());
                }
                if store.users.len() >= 1000 || store.users.iter().any(|u| u.name == name) {
                    return Err("user already exists or user limit reached".into());
                }
                if stations.is_empty()
                    || stations.len() > 64
                    || stations.iter().any(|s| !self.known_station(s))
                {
                    return Err("specify existing station IDs".into());
                }
                store.users.push(User {
                    id: Uuid::new_v4(),
                    name: name.clone(),
                    active: true,
                    grants: stations.into_iter().map(|s| (s, role)).collect(),
                    passkeys: vec![],
                    password_hash: None,
                    enrollment: None,
                });
                self.save(store, "local-admin", "user.add", "success", "")?;
                return self.issue_enrollment(&name);
            }
            AdminRequest::Role {
                name,
                station,
                role,
            } => {
                if !self.known_station(&station) {
                    return Err("unknown station".into());
                }
                let user = store
                    .users
                    .iter_mut()
                    .find(|u| u.name == name && u.active)
                    .ok_or("unknown or revoked user")?;
                user.grants.insert(station.clone(), role);
                let id = user.id;
                store.sessions.retain(|_, s| s.user != id);
                self.challenges.retain(|_, c| match c {
                    Ceremony::Login { user, .. } | Ceremony::Register { user, .. } => *user != id,
                });
                ("user.role", station)
            }
            AdminRequest::Revoke { name } => {
                let user = store
                    .users
                    .iter_mut()
                    .find(|u| u.name == name && u.active)
                    .ok_or("unknown or revoked user")?;
                user.active = false;
                user.passkeys.clear();
                user.password_hash = None;
                user.enrollment = None;
                let id = user.id;
                store.sessions.retain(|_, s| s.user != id);
                self.challenges.retain(|_, c| match c {
                    Ceremony::Login { user, .. } | Ceremony::Register { user, .. } => *user != id,
                });
                ("user.revoke", String::new())
            }
            AdminRequest::Purge { name } => {
                let id = store
                    .users
                    .iter()
                    .find(|u| u.name == name)
                    .map(|u| u.id)
                    .ok_or("unknown user")?;
                store.users.retain(|u| u.id != id);
                store.sessions.retain(|_, session| session.user != id);
                // Commit the deletion and its audit together before removing volatile challenges.
                self.save(store, "local-admin", "user.purge", "success", "")?;
                self.challenges.retain(|_, challenge| match challenge {
                    Ceremony::Login { user, .. } | Ceremony::Register { user, .. } => *user != id,
                });
                return Ok(json!({"ok":true,"purged":name}));
            }
            AdminRequest::RevokeDevice { name, credential } => {
                let user = store
                    .users
                    .iter_mut()
                    .find(|u| u.name == name && u.active)
                    .ok_or("unknown or revoked user")?;
                let before = user.passkeys.len();
                user.passkeys
                    .retain(|p| URL_SAFE_NO_PAD.encode(p.cred_id().as_ref()) != credential);
                if user.passkeys.len() == before {
                    return Err("unknown device".into());
                }
                store.sessions.retain(|_, s| s.credential != credential);
                ("device.revoke", String::new())
            }
            AdminRequest::RevokeSession { id } => {
                let before = store.sessions.len();
                store.sessions.retain(|_, s| s.id != id);
                if store.sessions.len() == before {
                    return Err("unknown session".into());
                }
                ("session.revoke", String::new())
            }
            AdminRequest::RevokeSessions { name } => {
                let id = self.user(&name)?.id;
                store.sessions.retain(|_, s| s.user != id);
                ("session.revoke_user", String::new())
            }
            _ => unreachable!(),
        };
        self.save(store, "local-admin", action, "success", &target)?;
        Ok(json!({"ok":true}))
    }

    pub(super) fn peer_limit(&mut self, key: &str, max: u32) -> Result<bool, String> {
        let allowed = self.rate_limit(key, max);
        if !allowed {
            self.audit_failure("auth.rate_limit")?;
        }
        Ok(allowed)
    }
    pub(super) fn rate_limit(&mut self, key: &str, max: u32) -> bool {
        let at = now();
        self.attempts.retain(|_, (start, _)| at - *start < 60);
        let key = hash(key);
        if self.attempts.len() >= 4096 && !self.attempts.contains_key(&key) {
            return false;
        }
        let entry = self.attempts.entry(key).or_insert((at, 0));
        entry.1 = entry.1.saturating_add(1);
        entry.1 <= max
    }
    fn reserve_challenge(&mut self) -> Result<String, String> {
        self.challenges.retain(|_, c| c.expires() > now());
        if self.challenges.len() >= 512 {
            return Err("challenge limit reached".into());
        }
        Ok(token())
    }
    fn hash_password(password: &str) -> Result<String, String> {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| "identity storage unavailable".into())
    }

    pub(super) fn password_enroll(&mut self, raw: &str, password: &str) -> Result<Value, String> {
        let result = (|| {
            let length = password.chars().count();
            if length < self.config.password_min_length
                || length > self.config.password_max_length
                || password.len() > 1024
            {
                return Err("invalid password length".into());
            }
            let key = hash(raw);
            let id = self
                .store
                .users
                .iter()
                .find(|u| {
                    u.active
                        && u.enrollment
                            .as_ref()
                            .is_some_and(|e| e.hash == key && e.expires > now())
                })
                .map(|u| u.id)
                .ok_or("authentication denied")?;
            let password_hash = Self::hash_password(password)?;
            let mut store = self.store.clone();
            let user = store
                .users
                .iter_mut()
                .find(|u| u.id == id)
                .ok_or("authentication denied")?;
            user.password_hash = Some(password_hash);
            user.enrollment = None;
            user.passkeys.clear();
            store.sessions.retain(|_, session| session.user != id);
            self.save(store, &id.to_string(), "password.enroll", "success", "")?;
            self.challenges.retain(|_, c| match c {
                Ceremony::Register { user, .. } | Ceremony::Login { user, .. } => *user != id,
            });
            Ok(json!({"registered":true}))
        })();
        if result.is_err() {
            self.audit_failure("password.enroll")?;
        }
        result
    }

    pub(super) fn password_login(
        &mut self,
        name: &str,
        password: &str,
        old: Option<&str>,
    ) -> Result<(Value, String), String> {
        let result = (|| {
            if name.len() > 128 || password.len() > 1024 {
                return Err("authentication denied".into());
            }
            if !self.rate_limit(&format!("user:{name}"), 10) {
                return Err("rate limited".into());
            }
            let user = self
                .store
                .users
                .iter()
                .find(|u| u.name == name && u.active && u.password_hash.is_some());
            // Unknown and revoked accounts still perform the same expensive verification.
            let encoded = user
                .and_then(|u| u.password_hash.as_deref())
                .unwrap_or(&self.dummy_password_hash);
            let valid = PasswordHash::new(encoded).is_ok_and(|h| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &h)
                    .is_ok()
            });
            let id = user
                .filter(|_| valid)
                .map(|u| u.id)
                .ok_or("authentication denied")?;
            let mut store = self.store.clone();
            store.sessions.retain(|_, session| session.expires > now());
            if let Some(old) = old {
                store.sessions.remove(&hash(old));
            }
            if store.sessions.len() >= 4096
                || store.sessions.values().filter(|s| s.user == id).count() >= 16
            {
                return Err("session limit reached".into());
            }
            let raw = token();
            let csrf = token();
            store.sessions.insert(
                hash(&raw),
                Session {
                    id: Uuid::new_v4(),
                    user: id,
                    credential: "password".into(),
                    csrf: csrf.clone(),
                    expires: now() + self.config.session_ttl_seconds as i64,
                },
            );
            self.save(store, &id.to_string(), "password.login", "success", "")?;
            Ok((json!({"authenticated":true,"csrf_token":csrf}), raw))
        })();
        if result.is_err() {
            self.audit_failure("password.login")?;
        }
        result
    }

    pub(super) fn registration_start(&mut self, raw: &str) -> Result<(Value, String), String> {
        let key = hash(raw);
        let user = self
            .store
            .users
            .iter()
            .find(|u| {
                u.active
                    && u.enrollment
                        .as_ref()
                        .is_some_and(|e| e.hash == key && e.expires > now())
            })
            .cloned();
        let Some(user) = user else {
            self.audit_failure("enroll.start")?;
            return Err("authentication denied".into());
        };
        let cookie = self.reserve_challenge()?;
        let (options, state) = self
            .webauthn
            .start_passkey_registration(
                user.id,
                &user.name,
                &user.name,
                Some(user.passkeys.iter().map(|p| p.cred_id().clone()).collect()),
            )
            .map_err(|_| "authentication denied")?;
        self.challenges.insert(
            hash(&cookie),
            Ceremony::Register {
                user: user.id,
                enrollment: key,
                expires: now() + CHALLENGE_TTL,
                state,
            },
        );
        Ok((json!({"account":user.name,"options":options}), cookie))
    }
    pub(super) fn registration_finish(
        &mut self,
        cookie: &str,
        credential: RegisterPublicKeyCredential,
    ) -> Result<Value, String> {
        let result = (|| {
            let Some(Ceremony::Register {
                user,
                enrollment,
                expires,
                state,
            }) = self.challenges.remove(&hash(cookie))
            else {
                return Err("authentication denied".into());
            };
            if expires <= now()
                || !self
                    .by_id(user)?
                    .enrollment
                    .as_ref()
                    .is_some_and(|e| e.hash == enrollment && e.expires > now())
            {
                return Err("authentication denied".into());
            }
            let passkey = self
                .webauthn
                .finish_passkey_registration(&credential, &state)
                .map_err(|_| "authentication denied")?;
            if self
                .store
                .users
                .iter()
                .any(|u| u.passkeys.iter().any(|p| p.cred_id() == passkey.cred_id()))
            {
                return Err("authentication denied".into());
            }
            let mut store = self.store.clone();
            let account = store
                .users
                .iter_mut()
                .find(|u| u.id == user && u.active)
                .ok_or("authentication denied")?;
            if account.passkeys.len() >= 8 {
                return Err("authentication denied".into());
            }
            account.passkeys.push(passkey);
            account.enrollment = None;
            self.save(store, &user.to_string(), "enroll.finish", "success", "")?;
            Ok(json!({"registered":true}))
        })();
        if result.is_err() {
            self.audit_failure("enroll.finish")?;
        }
        result
    }
    pub(super) fn login_start(&mut self, name: &str) -> Result<(Value, String), String> {
        if !self.rate_limit(&format!("user:{name}"), 10) {
            self.audit_failure("login.rate_limit")?;
            return Err("authentication denied".into());
        }
        let Some(user) = self
            .store
            .users
            .iter()
            .find(|u| u.name == name && u.active && !u.passkeys.is_empty())
            .cloned()
        else {
            self.audit_failure("login.start")?;
            return Err("authentication denied".into());
        };
        let cookie = self.reserve_challenge()?;
        let (options, state) = self
            .webauthn
            .start_passkey_authentication(&user.passkeys)
            .map_err(|_| "authentication denied")?;
        self.challenges.insert(
            hash(&cookie),
            Ceremony::Login {
                user: user.id,
                expires: now() + CHALLENGE_TTL,
                state,
            },
        );
        Ok((json!({"options":options}), cookie))
    }
    pub(super) fn login_finish(
        &mut self,
        cookie: &str,
        credential: PublicKeyCredential,
        old: Option<&str>,
    ) -> Result<(Value, String), String> {
        let result = (|| {
            let Some(Ceremony::Login {
                user,
                expires,
                state,
            }) = self.challenges.remove(&hash(cookie))
            else {
                return Err("authentication denied".into());
            };
            if expires <= now() {
                return Err("authentication denied".into());
            }
            let account = self.by_id(user)?;
            let result = self
                .webauthn
                .finish_passkey_authentication(&credential, &state)
                .map_err(|_| "authentication denied")?;
            // A device revoked while the challenge was in progress cannot log in.
            if !account
                .passkeys
                .iter()
                .any(|p| p.cred_id() == result.cred_id())
            {
                return Err("authentication denied".into());
            }
            let raw = token();
            let csrf = token();
            let credential = URL_SAFE_NO_PAD.encode(result.cred_id().as_ref());
            let mut store = self.store.clone();
            store.sessions.retain(|_, s| s.expires > now());
            if store.sessions.len() >= 4096
                || store.sessions.values().filter(|s| s.user == user).count() >= 16
            {
                return Err("session limit reached".into());
            }
            if let Some(old) = old {
                store.sessions.remove(&hash(old));
            }
            let account = store
                .users
                .iter_mut()
                .find(|u| u.id == user)
                .ok_or("authentication denied")?;
            for passkey in &mut account.passkeys {
                passkey.update_credential(&result);
            }
            store.sessions.insert(
                hash(&raw),
                Session {
                    id: Uuid::new_v4(),
                    user,
                    credential,
                    csrf: csrf.clone(),
                    expires: now() + self.config.session_ttl_seconds as i64,
                },
            );
            self.save(store, &user.to_string(), "login.finish", "success", "")?;
            Ok((json!({"authenticated":true,"csrf_token":csrf}), raw))
        })();
        if result.is_err() {
            self.audit_failure("login.finish")?;
        }
        result
    }
    fn session(&self, raw: &str) -> Result<(&Session, &User), String> {
        let session = self
            .store
            .sessions
            .get(&hash(raw))
            .filter(|s| s.expires > now())
            .ok_or("authentication denied")?;
        Ok((session, self.by_id(session.user)?))
    }
    pub(super) fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
    pub(super) fn console_owner(
        &self,
        raw: &str,
        station: &str,
        csrf: &str,
    ) -> Result<String, String> {
        self.authorize(raw, station, "console.open", Some(csrf))?;
        Ok(self.session(raw)?.1.id.to_string())
    }
    pub(super) fn console_attempt(
        &self,
        raw: &str,
        station: &str,
        csrf: &str,
    ) -> Result<String, String> {
        let result = self.console_owner(raw, station, csrf);
        if result.is_err() {
            let actor = self
                .session(raw)
                .map(|(_, user)| user.id.to_string())
                .unwrap_or_else(|_| "anonymous".into());
            let target = if self.known_station(station) {
                station
            } else {
                ""
            };
            self.console_audit(&actor, "console.open", "denied", target)?;
        }
        result
    }
    pub(super) fn console_audit(
        &self,
        actor: &str,
        action: &str,
        result: &str,
        station: &str,
    ) -> Result<(), String> {
        self.db.batch(&[
            stmt("INSERT INTO webmin_audit(at,actor,action,result,station) VALUES(?1,?2,?3,?4,?5)", vec![json!(now()),json!(actor),json!(action),json!(result),json!(station)]),
            stmt("DELETE FROM webmin_audit WHERE at < ?1", vec![json!(now()-90*86400)]),
        ]).map_err(|_| "identity storage unavailable".to_string())?;
        Ok(())
    }
    pub(super) fn context(&self, raw: &str) -> Result<Value, String> {
        let (session, user) = self.session(raw)?;
        Ok(
            json!({"name":user.name,"csrf_token":session.csrf,"expires_at":session.expires,"console_enabled":self.config.console.enabled,"stations":self.config.stations.iter().filter(|s|self.authorize(raw,&s.id,"station.read",None).is_ok()).filter_map(|s|user.grants.get(&s.id).map(|role|json!({"id":s.id,"label":s.label,"role":role}))).collect::<Vec<_>>()}),
        )
    }
    pub(super) fn authorize(
        &self,
        raw: &str,
        station: &str,
        permission: &str,
        csrf: Option<&str>,
    ) -> Result<(), String> {
        let (session, user) = self.session(raw)?;
        if !self.known_station(station)
            || !user
                .grants
                .get(station)
                .is_some_and(|role| role.permits(permission))
        {
            return Err("permission denied".into());
        }
        let mutation = !matches!(
            permission,
            "station.read" | "queue.read" | "agenda.read" | "events.read"
        );
        if (mutation && !csrf.is_some_and(|c| secret_eq(c, &session.csrf)))
            || csrf.is_some_and(|c| !secret_eq(c, &session.csrf))
        {
            return Err("permission denied".into());
        }
        Ok(())
    }
    pub(super) fn logout(&mut self, raw: &str, csrf: &str) -> Result<Value, String> {
        let (session, _) = self.session(raw)?;
        if !secret_eq(csrf, &session.csrf) {
            return Err("permission denied".into());
        }
        let actor = session.user.to_string();
        let mut store = self.store.clone();
        store.sessions.remove(&hash(raw));
        self.save(store, &actor, "session.logout", "success", "")?;
        Ok(json!({"ok":true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webauthn_authenticator_rs::{softpasskey::SoftPasskey, WebauthnAuthenticator};
    fn fixture() -> (tempfile::TempDir, Auth, WebauthnAuthenticator<SoftPasskey>) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::parse(
            &toml::from_str(
                r#"
            public_url = "https://remote.example.test"
            [[stations]]
            id = "one"
            label = "Station One"
            grpc_endpoint = "http://127.0.0.1:50051"
            [[stations]]
            id = "two"
            label = "Station Two"
            grpc_endpoint = "http://127.0.0.1:50052"
        "#,
            )
            .unwrap(),
        )
        .unwrap();
        let db =
            Arc::new(PluginDb::open(dir.path(), "remote-supervision", Default::default()).unwrap());
        db.migrate(&[MIGRATION.into()]).unwrap();
        (
            dir,
            Auth::open(&config, db).unwrap(),
            WebauthnAuthenticator::new(SoftPasskey::new(true)),
        )
    }
    fn add(auth: &mut Auth, name: &str, role: Role) -> String {
        auth.admin(AdminRequest::Add {
            name: name.into(),
            role,
            stations: vec!["one".into()],
        })
        .unwrap()["enrollment_url"]
            .as_str()
            .unwrap()
            .split_once('#')
            .unwrap()
            .1
            .into()
    }
    fn register(auth: &mut Auth, soft: &mut WebauthnAuthenticator<SoftPasskey>, enroll: &str) {
        let (value, cookie) = auth.registration_start(enroll).unwrap();
        let response = soft
            .do_registration(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(value["options"].clone()).unwrap(),
            )
            .unwrap();
        auth.registration_finish(&cookie, response).unwrap();
    }
    fn login(
        auth: &mut Auth,
        soft: &mut WebauthnAuthenticator<SoftPasskey>,
        name: &str,
    ) -> (Value, String) {
        let (value, cookie) = auth.login_start(name).unwrap();
        let response = soft
            .do_authentication(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(value["options"].clone()).unwrap(),
            )
            .unwrap();
        auth.login_finish(&cookie, response, None).unwrap()
    }
    #[test]
    fn webmin_passkey_end_to_end_persists_public_credentials_and_hashes_session_tokens() {
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Helper);
        register(&mut auth, &mut soft, &enroll);
        let (_, session) = login(&mut auth, &mut soft, "Alice");
        let context = auth.context(&session).unwrap();
        assert_eq!(context["stations"].as_array().unwrap().len(), 1);
        assert_eq!(context["stations"][0]["id"], "one");
        assert!(!context.to_string().contains("grpc_endpoint"));
        let stored = serde_json::to_string(&auth.store).unwrap();
        assert!(!stored.contains(&enroll));
        assert!(!stored.contains(&session));
        let reopened = Auth::open(&auth.config, auth.db.clone()).unwrap();
        assert!(reopened.context(&session).is_ok());
        let mut changed = auth.config.clone();
        changed.public_url = "https://different.example.test".into();
        assert!(Auth::open(&changed, auth.db.clone()).is_err());
    }
    #[test]
    fn webmin_enrollment_and_login_challenges_are_one_use_and_expire() {
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Viewer);
        let (value, cookie) = auth.registration_start(&enroll).unwrap();
        let response = soft
            .do_registration(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(value["options"].clone()).unwrap(),
            )
            .unwrap();
        auth.registration_finish(&cookie, response.clone()).unwrap();
        assert!(auth.registration_finish(&cookie, response).is_err());
        assert!(auth.registration_start(&enroll).is_err());
        let (value, cookie) = auth.login_start("Alice").unwrap();
        let response = soft
            .do_authentication(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(value["options"].clone()).unwrap(),
            )
            .unwrap();
        auth.login_finish(&cookie, response.clone(), None).unwrap();
        assert!(auth.login_finish(&cookie, response, None).is_err());
        let (value, cookie) = auth.login_start("Alice").unwrap();
        let response = soft
            .do_authentication(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(value["options"].clone()).unwrap(),
            )
            .unwrap();
        if let Ceremony::Login { expires, .. } = auth.challenges.get_mut(&hash(&cookie)).unwrap() {
            *expires = 0;
        }
        assert!(auth.login_finish(&cookie, response, None).is_err());
        let enroll = auth.issue_enrollment("Alice").unwrap()["enrollment_url"]
            .as_str()
            .unwrap()
            .split_once('#')
            .unwrap()
            .1
            .to_owned();
        auth.store.users[0].enrollment.as_mut().unwrap().expires = 0;
        assert!(auth.registration_start(&enroll).is_err());
    }
    #[test]
    fn webmin_permissions_are_scoped_and_role_changes_revoke_sessions() {
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Viewer);
        register(&mut auth, &mut soft, &enroll);
        let (value, session) = login(&mut auth, &mut soft, "Alice");
        assert!(auth
            .authorize(&session, "one", "station.read", None)
            .is_ok());
        assert!(auth
            .authorize(&session, "two", "station.read", None)
            .is_err());
        assert!(auth
            .authorize(
                &session,
                "one",
                "player.skip",
                Some(value["csrf_token"].as_str().unwrap())
            )
            .is_err());
        auth.admin(AdminRequest::Role {
            name: "Alice".into(),
            station: "one".into(),
            role: Role::Helper,
        })
        .unwrap();
        assert!(auth.context(&session).is_err());
        let (value, session) = login(&mut auth, &mut soft, "Alice");
        assert!(auth
            .authorize(
                &session,
                "one",
                "player.skip",
                Some(value["csrf_token"].as_str().unwrap())
            )
            .is_ok());
        assert!(auth
            .authorize(&session, "one", "console.open", None)
            .is_err());
        assert!(auth
            .authorize(&session, "one", "player.skip", Some("wrong"))
            .is_err());
        assert!(auth.authorize(&session, "one", "made.up", None).is_err());
        assert!(auth
            .authorize(&session, "one", "player.skip", None)
            .is_err());
    }
    #[test]
    fn webmin_revoking_a_device_blocks_pending_login_and_revokes_its_sessions() {
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Admin);
        register(&mut auth, &mut soft, &enroll);
        let (_, session) = login(&mut auth, &mut soft, "Alice");
        let (value, cookie) = auth.login_start("Alice").unwrap();
        let response = soft
            .do_authentication(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(value["options"].clone()).unwrap(),
            )
            .unwrap();
        let credential = URL_SAFE_NO_PAD.encode(auth.store.users[0].passkeys[0].cred_id().as_ref());
        auth.admin(AdminRequest::RevokeDevice {
            name: "Alice".into(),
            credential,
        })
        .unwrap();
        assert!(auth.context(&session).is_err());
        assert!(auth.login_finish(&cookie, response, None).is_err());
        assert!(auth.login_start("Alice").is_err());
        let enrollment = auth.issue_enrollment("Alice").unwrap()["enrollment_url"]
            .as_str()
            .unwrap()
            .split_once('#')
            .unwrap()
            .1
            .to_owned();
        auth.admin(AdminRequest::Revoke {
            name: "Alice".into(),
        })
        .unwrap();
        assert!(auth.registration_start(&enrollment).is_err());
    }
    #[test]
    fn webmin_logout_checks_csrf_and_sessions_expire_and_rotate() {
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Admin);
        register(&mut auth, &mut soft, &enroll);
        let (_, old) = login(&mut auth, &mut soft, "Alice");
        let (value, cookie) = auth.login_start("Alice").unwrap();
        let response = soft
            .do_authentication(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(value["options"].clone()).unwrap(),
            )
            .unwrap();
        let (value, session) = auth.login_finish(&cookie, response, Some(&old)).unwrap();
        assert!(auth.context(&old).is_err());
        assert!(auth.logout(&session, "wrong").is_err());
        assert!(auth.context(&session).is_ok());
        auth.logout(&session, value["csrf_token"].as_str().unwrap())
            .unwrap();
        assert!(auth.context(&session).is_err());
        let (_, session) = login(&mut auth, &mut soft, "Alice");
        auth.store
            .sessions
            .get_mut(&hash(&session))
            .unwrap()
            .expires = 0;
        assert!(auth.context(&session).is_err());
    }
    #[test]
    fn webmin_rejects_client_origin_tampering_and_consumes_the_challenge() {
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Admin);
        register(&mut auth, &mut soft, &enroll);
        let (value, cookie) = auth.login_start("Alice").unwrap();
        let response = soft
            .do_authentication(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(value["options"].clone()).unwrap(),
            )
            .unwrap();
        let mut credential = serde_json::to_value(&response).unwrap();
        let mut client: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(credential["response"]["clientDataJSON"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        client["origin"] = json!("https://evil.example.test");
        credential["response"]["clientDataJSON"] =
            json!(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&client).unwrap()));
        assert!(auth
            .login_finish(&cookie, serde_json::from_value(credential).unwrap(), None)
            .is_err());
        assert!(auth.login_finish(&cookie, response, None).is_err());
    }
    #[test]
    fn webmin_rate_limits_and_audit_do_not_record_secrets() {
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Admin);
        register(&mut auth, &mut soft, &enroll);
        let (_, session) = login(&mut auth, &mut soft, "Alice");
        for _ in 0..2 {
            assert!(auth.rate_limit("peer:127.0.0.1", 2));
        }
        assert!(!auth.rate_limit("peer:127.0.0.1", 2));
        auth.db.exec(&stmt("INSERT INTO webmin_audit(at,actor,action,result,station) VALUES(0,'anonymous','expired','denied','')",vec![])).unwrap();
        auth.audit_failure("login.test_denied").unwrap();
        let audit = auth.admin(AdminRequest::Audit).unwrap().to_string();
        assert!(!audit.contains("expired"));
        assert!(!audit.contains(&enroll));
        assert!(!audit.contains(&session));
        assert!(!audit.contains("csrf"));
        assert!(audit.contains("login.finish"));
    }
    #[tokio::test]
    async fn webmin_http_passkey_flow_sets_secure_session_and_opens_network_home() {
        use axum::{
            body::Body,
            http::{header, Request, StatusCode},
            Router,
        };
        use tower::ServiceExt;
        async fn post(
            app: &Router,
            path: &str,
            body: Value,
            cookie: &str,
        ) -> axum::response::Response {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header(header::ORIGIN, "https://remote.example.test")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::COOKIE, cookie)
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
        fn response_cookie(response: &axum::response::Response, name: &str) -> String {
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .find(|v| v.starts_with(name))
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .into()
        }
        async fn data(response: axum::response::Response) -> Value {
            assert_eq!(response.status(), StatusCode::OK);
            serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap(),
            )
            .unwrap()
        }
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Helper);
        let config = auth.config.clone();
        let app = super::super::bounded_router(
            &config,
            super::super::probe_router().merge(super::super::web::routes(
                super::super::web::Web::new(Arc::new(std::sync::Mutex::new(auth)), &config),
            )),
        );
        let start = post(&app, "/auth/enroll/start", json!({"token":enroll}), "").await;
        let cookie = response_cookie(&start, "__Host-stationd-ceremony");
        let body = data(start).await;
        let response = soft
            .do_registration(
                Url::parse(&config.public_url).unwrap(),
                serde_json::from_value(body["options"].clone()).unwrap(),
            )
            .unwrap();
        assert_eq!(
            data(
                post(
                    &app,
                    "/auth/enroll/finish",
                    serde_json::to_value(response).unwrap(),
                    &cookie
                )
                .await
            )
            .await["registered"],
            true
        );
        let start = post(&app, "/auth/login/start", json!({"name":"Alice"}), "").await;
        let cookie = response_cookie(&start, "__Host-stationd-ceremony");
        let body = data(start).await;
        let response = soft
            .do_authentication(
                Url::parse(&config.public_url).unwrap(),
                serde_json::from_value(body["options"].clone()).unwrap(),
            )
            .unwrap();
        let finish = post(
            &app,
            "/auth/login/finish",
            serde_json::to_value(response).unwrap(),
            &cookie,
        )
        .await;
        let cookie = response_cookie(&finish, "__Host-stationd-session");
        let session_header = finish
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("__Host-stationd-session"))
            .unwrap();
        for flag in ["Secure", "HttpOnly", "SameSite=Strict", "Path=/"] {
            assert!(session_header.contains(flag));
        }
        assert_eq!(data(finish).await["authenticated"], true);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(String::from_utf8(
            axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap()
                .to_vec()
        )
        .unwrap()
        .contains("Réseau StationD"));
        let oversized = post(
            &app,
            "/auth/enroll/start",
            json!({"token":"x".repeat(70000)}),
            "",
        )
        .await;
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
    #[tokio::test]
    async fn webmin_http_checks_origin_cookie_csrf_and_hides_unauthorized_stations() {
        use axum::{
            body::Body,
            http::{header, Request, StatusCode},
        };
        use tower::ServiceExt;
        let (_dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Helper);
        register(&mut auth, &mut soft, &enroll);
        let (value, session) = login(&mut auth, &mut soft, "Alice");
        let config = auth.config.clone();
        let app = super::super::web::routes(super::super::web::Web::new(
            Arc::new(std::sync::Mutex::new(auth)),
            &config,
        ));
        let cookie = format!("__Host-stationd-session={session}");
        let request = |method: &str, path: &str, origin: &str, csrf: &str, body: &str| {
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::ORIGIN, origin)
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-csrf-token", csrf)
                .body(Body::from(body.to_owned()))
                .unwrap()
        };
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/session",
                "https://remote.example.test",
                "",
                "",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["stations"].as_array().unwrap().len(), 1);
        assert_eq!(
            app.clone()
                .oneshot(request(
                    "POST",
                    "/auth/logout",
                    "https://evil.example.test",
                    "",
                    "{}"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request(
                    "POST",
                    "/auth/logout",
                    "https://remote.example.test",
                    "wrong",
                    "{}"
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/auth/logout",
                "https://remote.example.test",
                value["csrf_token"].as_str().unwrap(),
                "{}",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let set = response.headers()[header::SET_COOKIE].to_str().unwrap();
        for flag in [
            "Secure",
            "HttpOnly",
            "SameSite=Strict",
            "Path=/",
            "Max-Age=0",
        ] {
            assert!(set.contains(flag));
        }
        assert_eq!(
            app.oneshot(request(
                "GET",
                "/api/session",
                "https://remote.example.test",
                "",
                ""
            ))
            .await
            .unwrap()
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[test]
    fn webmin_password_enrollment_login_persistence_reset_and_revocation() {
        let (_dir, mut auth, _) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Helper);
        let password = "my long password : , 123";
        assert!(auth.password_enroll(&enroll, "short").is_err());
        auth.password_enroll(&enroll, password).unwrap();
        assert!(auth.password_enroll(&enroll, password).is_err());
        assert!(auth.password_login("Alice", "wrong", None).is_err());
        assert!(auth.password_login("Unknown", password, None).is_err());
        let (_, session) = auth.password_login("Alice", password, None).unwrap();
        assert_eq!(auth.context(&session).unwrap()["stations"][0]["id"], "one");
        assert!(auth
            .authorize(&session, "two", "station.read", None)
            .is_err());
        let stored = serde_json::to_string(&auth.store).unwrap();
        assert!(!stored.contains(password));
        assert!(!stored.contains(&enroll));
        assert!(!stored.contains(&session));
        assert!(stored.contains("$argon2id$"));
        let mut reopened = Auth::open(&auth.config, auth.db.clone()).unwrap();
        let (_, rotated) = reopened
            .password_login("Alice", password, Some(&session))
            .unwrap();
        assert!(reopened.context(&session).is_err());
        let reset = reopened
            .admin(AdminRequest::Enroll {
                name: "Alice".into(),
            })
            .unwrap();
        let token = reset["enrollment_url"]
            .as_str()
            .unwrap()
            .split_once('#')
            .unwrap()
            .1;
        reopened
            .password_enroll(token, "a completely different password")
            .unwrap();
        assert!(reopened.context(&rotated).is_err());
        assert!(reopened.password_login("Alice", password, None).is_err());
        assert!(reopened
            .password_login("Alice", "a completely different password", None)
            .is_ok());
        reopened
            .admin(AdminRequest::Revoke {
                name: "Alice".into(),
            })
            .unwrap();
        assert!(reopened
            .password_login("Alice", "a completely different password", None)
            .is_err());
        let audit = reopened.admin(AdminRequest::Audit).unwrap().to_string();
        assert!(!audit.contains(password));
        assert!(!audit.contains(token));
    }

    #[test]
    fn webmin_password_rejects_expired_links_and_limits_login_attempts() {
        let (_dir, mut auth, _) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Viewer);
        auth.store.users[0].enrollment.as_mut().unwrap().expires = now() - 1;
        assert!(auth
            .password_enroll(&enroll, "long enough password")
            .is_err());
        let enroll = auth.issue_enrollment("Alice").unwrap();
        let raw = enroll["enrollment_url"]
            .as_str()
            .unwrap()
            .split_once('#')
            .unwrap()
            .1;
        auth.password_enroll(raw, "long enough password").unwrap();
        assert!(auth
            .password_login("Alice", &"x".repeat(1025), None)
            .is_err());
        for _ in 0..10 {
            assert!(auth
                .password_login("Alice", "wrong password", None)
                .is_err());
        }
        assert_eq!(
            auth.password_login("Alice", "long enough password", None)
                .unwrap_err(),
            "rate limited"
        );
    }

    #[tokio::test]
    async fn webmin_password_http_enrollment_login_and_origin_protection() {
        use axum::{
            body::Body,
            http::{header, Request, StatusCode},
        };
        use tower::ServiceExt;
        let (_dir, mut auth, _) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Viewer);
        let config = auth.config.clone();
        let app = super::super::bounded_router(
            &config,
            super::super::probe_router().merge(super::super::web::routes(
                super::super::web::Web::new(Arc::new(std::sync::Mutex::new(auth)), &config),
            )),
        );
        async fn post(
            app: &axum::Router,
            path: &str,
            origin: &str,
            body: Value,
        ) -> axum::response::Response {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header(header::ORIGIN, origin)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
        let password = "a long HTTP test password";
        let body = json!({"token":enroll,"password":password});
        assert_eq!(
            post(
                &app,
                "/auth/password/enroll",
                "https://evil.test",
                body.clone()
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            post(
                &app,
                "/auth/password/enroll",
                "https://remote.example.test",
                body.clone()
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            post(
                &app,
                "/auth/password/enroll",
                "https://remote.example.test",
                body
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(
                &app,
                "/auth/password/login",
                "https://remote.example.test",
                json!({"name":"Alice","password":"wrong"})
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        let response = post(
            &app,
            "/auth/password/login",
            "https://remote.example.test",
            json!({"name":"Alice","password":password}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        for flag in ["Secure", "HttpOnly", "SameSite=Strict", "Path=/"] {
            assert!(cookie.contains(flag));
        }
        let cookie = cookie.split(';').next().unwrap();
        let home = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(home.status(), StatusCode::OK);
    }
    #[tokio::test]
    async fn webmin_password_policy_is_shared_by_form_and_server_and_counts_unicode() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;
        let (_dir, mut auth, _) = fixture();
        auth.config.password_min_length = 6;
        auth.config.password_max_length = 8;
        let enrollment = add(&mut auth, "Alice", Role::Viewer);
        assert!(auth.password_enroll(&enrollment, "12345").is_err());
        assert!(auth.password_enroll(&enrollment, "123456789").is_err());
        let config = auth.config.clone();
        let app = super::super::web::routes(super::super::web::Web::new(
            Arc::new(std::sync::Mutex::new(auth)),
            &config,
        ));
        let page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/enroll")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let html = String::from_utf8(
            axum::body::to_bytes(page.into_body(), 65536)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("data-password-min-length=\"6\""));
        assert!(html.contains("data-password-max-length=\"8\""));
        assert!(html.contains("6 à 8 caractères"));
        assert!(!html.contains("{{PASSWORD_"));
        let enrolled = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/password/enroll")
                    .header("origin", "https://remote.example.test")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"token":enrollment,"password":"😀abcde"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(enrolled.status(), StatusCode::OK);
        let login = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/password/login")
                    .header("origin", "https://remote.example.test")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"name":"Alice","password":"😀abcde"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
    }
    #[test]
    fn webmin_user_purge_removes_active_identity_and_sessions_and_allows_name_reuse() {
        let (_dir, mut auth, _) = fixture();
        let enrollment = add(&mut auth, "Alice", Role::Helper);
        auth.password_enroll(&enrollment, "long enough password")
            .unwrap();
        let (_, session) = auth
            .password_login("Alice", "long enough password", None)
            .unwrap();
        let old_id = auth.user("Alice").unwrap().id;
        let other_enrollment = add(&mut auth, "Bob", Role::Viewer);
        let reset = auth.issue_enrollment("Alice").unwrap();
        let reset = reset["enrollment_url"]
            .as_str()
            .unwrap()
            .split_once('#')
            .unwrap()
            .1;
        let (_, pending_registration) = auth.registration_start(reset).unwrap();
        assert_eq!(
            auth.admin(AdminRequest::Purge {
                name: "Alice".into()
            })
            .unwrap()["purged"],
            "Alice"
        );
        assert!(auth.context(&session).is_err());
        assert!(auth
            .password_login("Alice", "long enough password", None)
            .is_err());
        assert!(auth
            .password_enroll(reset, "another long password")
            .is_err());
        assert!(!auth.challenges.contains_key(&hash(&pending_registration)));
        assert!(auth.store.users.iter().all(|user| user.id != old_id));
        assert!(auth
            .store
            .sessions
            .values()
            .all(|session| session.user != old_id));
        let reopened = Auth::open(&auth.config, auth.db.clone()).unwrap();
        assert!(reopened.user("Alice").is_err());
        assert!(reopened.context(&session).is_err());
        let fresh = add(&mut auth, "Alice", Role::Viewer);
        assert_ne!(auth.user("Alice").unwrap().id, old_id);
        auth.password_enroll(&fresh, "new account password")
            .unwrap();
        assert!(auth.context(&session).is_err());
        assert!(auth
            .password_enroll(reset, "another long password")
            .is_err());
        auth.password_enroll(&other_enrollment, "bob account password")
            .unwrap();
        assert!(auth
            .password_login("Bob", "bob account password", None)
            .is_ok());
        let before = serde_json::to_string(&auth.store).unwrap();
        assert!(auth
            .admin(AdminRequest::Purge {
                name: "Missing".into()
            })
            .is_err());
        assert_eq!(serde_json::to_string(&auth.store).unwrap(), before);
        assert!(auth
            .admin(AdminRequest::Audit)
            .unwrap()
            .to_string()
            .contains("user.purge"));
    }

    #[test]
    fn webmin_user_purge_accepts_revoked_accounts_and_cancels_pending_passkey_login() {
        let (_dir, mut auth, mut soft) = fixture();
        let enrollment = add(&mut auth, "Alice", Role::Helper);
        register(&mut auth, &mut soft, &enrollment);
        let (_, session) = login(&mut auth, &mut soft, "Alice");
        let (options, ceremony) = auth.login_start("Alice").unwrap();
        let assertion = soft
            .do_authentication(
                Url::parse(&auth.config.public_url).unwrap(),
                serde_json::from_value(options["options"].clone()).unwrap(),
            )
            .unwrap();
        auth.admin(AdminRequest::Purge {
            name: "Alice".into(),
        })
        .unwrap();
        assert!(auth.login_finish(&ceremony, assertion, None).is_err());
        assert!(auth.context(&session).is_err());
        add(&mut auth, "Alice", Role::Viewer);
        auth.admin(AdminRequest::Revoke {
            name: "Alice".into(),
        })
        .unwrap();
        auth.admin(AdminRequest::Purge {
            name: "Alice".into(),
        })
        .unwrap();
        assert!(auth
            .admin(AdminRequest::List)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        let mut reopened = Auth::open(&auth.config, auth.db.clone()).unwrap();
        add(&mut reopened, "Alice", Role::Viewer);
    }
    #[tokio::test]
    async fn webmin_live_routes_enforce_station_acl_and_bound_sse_and_close_on_purge() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let (_dir, mut auth, _) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Viewer);
        auth.password_enroll(&enroll, "a long enough password")
            .unwrap();
        let (_, session) = auth
            .password_login("Alice", "a long enough password", None)
            .unwrap();
        let mut config = auth.config.clone();
        config.max_event_streams = 1;
        let auth = Arc::new(std::sync::Mutex::new(auth));
        let web = super::super::web::Web::new(auth.clone(), &config);
        let app = super::super::web::routes(web);
        let request = |path: &str, cookie: &str| {
            Request::builder()
                .uri(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap()
        };
        let cookie = format!("__Host-stationd-session={session}");
        for path in [
            "/api/stations",
            "/api/network/events",
            "/api/stations/one",
            "/api/stations/one/events",
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(request(path, ""))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        for path in [
            "/api/stations/two",
            "/api/stations/two/events",
            "/station/two",
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(request(path, &cookie))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        let response = app
            .clone()
            .oneshot(request("/api/stations", &cookie))
            .await
            .unwrap();
        let data: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(data["stations"].as_array().unwrap().len(), 1);
        assert_eq!(data["stations"][0]["id"], "one");
        assert!(!data.to_string().contains("grpc_endpoint"));
        let response = app
            .clone()
            .oneshot(request("/api/network/events", &cookie))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("text/event-stream"));
        let mut body = response.into_body();
        // Leave this browser unread: its one-frame buffer must not prevent
        // other authenticated reads, and the stream quota remains enforced.
        assert_eq!(
            app.clone()
                .oneshot(request("/api/stations/one/events", &cookie))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            app.clone()
                .oneshot(request("/api/stations/one", &cookie))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let first = String::from_utf8(first.into_data().unwrap().to_vec()).unwrap();
        assert!(first.contains("snapshot"));
        assert!(!first.contains("Station Two"));
        auth.lock()
            .unwrap()
            .admin(AdminRequest::Purge {
                name: "Alice".into(),
            })
            .unwrap();
        let ended = tokio::time::timeout(std::time::Duration::from_secs(3), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8(ended.into_data().unwrap().to_vec())
            .unwrap()
            .contains("session-ended"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            app.oneshot(request("/api/stations/one", &cookie))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn webmin_console_linux_ws_acl_resize_revocation_and_shutdown() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use futures_util::{SinkExt, StreamExt};
        use std::{os::unix::fs::PermissionsExt, time::Duration};
        use tokio_tungstenite::{
            connect_async,
            tungstenite::{client::IntoClientRequest, Message},
        };
        use tower::ServiceExt;
        let (dir, mut auth, mut soft) = fixture();
        let enroll = add(&mut auth, "Alice", Role::Admin);
        register(&mut auth, &mut soft, &enroll);
        let (alice, session) = login(&mut auth, &mut soft, "Alice");
        let enroll = add(&mut auth, "Bob", Role::Admin);
        register(&mut auth, &mut soft, &enroll);
        let (_, bob_session) = login(&mut auth, &mut soft, "Bob");
        let enroll = add(&mut auth, "Helper", Role::Helper);
        register(&mut auth, &mut soft, &enroll);
        let (helper, helper_session) = login(&mut auth, &mut soft, "Helper");
        let program = dir.path().join("stationd-tui");
        let pid_file = dir.path().join("pid");
        let script = format!(
            r#"#!/usr/bin/python3
import os, sys, tty, signal, fcntl, termios, struct, threading, time
open({pid:?}, 'w').write(str(os.getpid()))
assert sys.argv[1:] == ['--addr', 'http://127.0.0.1:50051', '--lang', 'de'], sys.argv
assert 'STATIOND_ROOT' not in os.environ
tty.setraw(0)
def dimensions(*args):
    rows, cols, _, _ = struct.unpack('HHHH', fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
    os.write(1, ('SIZE:%sx%s\n' % (cols, rows)).encode())
signal.signal(signal.SIGWINCH, dimensions)
os.write(1, '\x1b[32mÉté 🎵\x1b[0m\n'.encode())
dimensions()
while True:
    data = os.read(0, 4096)
    os.write(1, b'ECHO:' + data)
    if b'periodic' in data:
        def ticker():
            while True:
                time.sleep(0.2)
                os.write(1, b'TICK\n')
        threading.Thread(target=ticker, daemon=True).start()
"#,
            pid = pid_file.to_str().unwrap()
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = auth.config.clone();
        config.console.enabled = true;
        config.console.command = program.to_str().unwrap().into();
        config.console.max_sessions = 2;
        config.console.idle_timeout_seconds = 3;
        config.console.max_duration_seconds = 5;
        let auth = Arc::new(std::sync::Mutex::new(auth));
        let web = super::super::web::Web::new(auth.clone(), &config);
        let console = web.console.clone();
        console.start();
        let app = super::super::web::routes(web);
        // All pages negotiate the browser language before the console opens.
        for language in ["fr", "en", "de"] {
            for path in ["/login", "/enroll", "/", "/station/one", "/station/one/console"] {
                let response = app.clone().oneshot(
                    Request::builder()
                        .uri(path)
                        .header("cookie", format!("__Host-stationd-session={session}"))
                        .header("accept-language", format!("{language}-ZZ"))
                        .body(Body::empty())
                        .unwrap(),
                ).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK, "{path}");
                assert_eq!(response.headers()["content-language"], language);
                assert_eq!(response.headers()["vary"], "Accept-Language");
                let body = axum::body::to_bytes(response.into_body(), 65536).await.unwrap();
                let html = std::str::from_utf8(&body).unwrap();
                assert!(html.contains(&format!("lang=\"{language}\"")), "{path}");
                assert!(html.contains("src=\"/i18n.js\""), "{path}");
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn({
            let app = app.clone();
            async move {
                axum::serve(listener, app).await.unwrap();
            }
        });
        let post = |session: &str, station: &str, csrf: &str, origin: &str| {
            Request::builder()
                .method("POST")
                .uri(format!("/api/stations/{station}/console"))
                .header("cookie", format!("__Host-stationd-session={session}"))
                .header("origin", origin)
                .header("accept-language", "de-DE,de;q=0.9,en;q=0.5")
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"cols":80,"rows":24}"#))
                .unwrap()
        };
        let csrf = alice["csrf_token"].as_str().unwrap();
        for (token, station, proof, origin) in [
            (&session[..], "one", "wrong", "https://remote.example.test"),
            (&session[..], "one", csrf, "https://evil.test"),
            (&session[..], "two", csrf, "https://remote.example.test"),
            (
                &helper_session[..],
                "one",
                helper["csrf_token"].as_str().unwrap(),
                "https://remote.example.test",
            ),
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(post(token, station, proof, origin))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        let create = || post(&session, "one", csrf, "https://remote.example.test");
        let response = app.clone().oneshot(create()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let data: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        let url = format!(
            "ws://{addr}/api/stations/one/console/{}/ws",
            data["id"].as_str().unwrap()
        );
        assert_eq!(
            app.clone().oneshot(create()).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let upgrade = |token: &str, origin: Option<&str>| {
            let mut request = url.clone().into_client_request().unwrap();
            request.headers_mut().insert(
                "cookie",
                format!("__Host-stationd-session={token}").parse().unwrap(),
            );
            if let Some(origin) = origin {
                request
                    .headers_mut()
                    .insert("origin", origin.parse().unwrap());
            }
            request
        };
        assert!(
            connect_async(upgrade(&bob_session, Some("https://remote.example.test")))
                .await
                .is_err()
        );
        assert!(connect_async(upgrade(&session, None)).await.is_err());
        assert!(connect_async(upgrade(&session, Some("https://evil.test")))
            .await
            .is_err());
        let (mut socket, _) = connect_async(upgrade(&session, Some("https://remote.example.test")))
            .await
            .unwrap();
        assert!(
            connect_async(upgrade(&session, Some("https://remote.example.test")))
                .await
                .is_err()
        );
        async fn read_until(
            socket: &mut tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            expected: &str,
        ) -> String {
            let mut output = Vec::new();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match socket.next().await.unwrap().unwrap() {
                        Message::Binary(bytes) => {
                            output.extend(bytes);
                            socket
                                .send(Message::Text(r#"{"type":"ack"}"#.into()))
                                .await
                                .unwrap();
                        }
                        other => panic!("unexpected {other:?}"),
                    }
                    let text = String::from_utf8_lossy(&output);
                    if text.contains(expected) {
                        return text.into_owned();
                    }
                }
            })
            .await
            .unwrap()
        }
        let initial = read_until(&mut socket, "SIZE:80x24").await;
        assert!(initial.contains("Été 🎵"));
        assert!(initial.contains("\x1b[32m"));
        socket
            .send(Message::Text(
                r#"{"type":"resize","cols":100,"rows":30}"#.into(),
            ))
            .await
            .unwrap();
        read_until(&mut socket, "SIZE:100x30").await;
        socket
            .send(Message::Text(r#"{"type":"probe"}"#.into()))
            .await
            .unwrap();
        let probe = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await.unwrap().unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(probe.to_text().unwrap()).unwrap(),
            json!({"type":"probe_ack"})
        );
        socket
            .send(Message::Text(r#"{"type":"input","data":"diagnostic","seq":7}"#.into()))
            .await
            .unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await.unwrap().unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(ack.to_text().unwrap()).unwrap(),
            json!({"type":"input_ack","seq":7})
        );
        read_until(&mut socket, "diagnostic").await;
        socket
            .send(Message::Text(
                r#"{"type":"input","data":"Bonjour é"}"#.into(),
            ))
            .await
            .unwrap();
        read_until(&mut socket, "Bonjour é").await;
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        auth.lock()
            .unwrap()
            .admin(AdminRequest::RevokeSessions {
                name: "Alice".into(),
            })
            .unwrap();
        let end = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(end.to_text().unwrap().contains("ended"));
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "PTY child still alive after revoke"
        );
        assert!(
            connect_async(upgrade(&session, Some("https://remote.example.test")))
                .await
                .is_err()
        );
        assert!(app
            .clone()
            .oneshot(create())
            .await
            .unwrap()
            .status()
            .is_client_error());
        // A fresh login can open a new console after revocation releases its quota.
        let (value, fresh) = login(&mut auth.lock().unwrap(), &mut soft, "Alice");
        let response = app
            .clone()
            .oneshot(post(
                &fresh,
                "one",
                value["csrf_token"].as_str().unwrap(),
                "https://remote.example.test",
            ))
            .await
            .unwrap();
        let data: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        let mut next = format!(
            "ws://{addr}/api/stations/one/console/{}/ws",
            data["id"].as_str().unwrap()
        )
        .into_client_request()
        .unwrap();
        next.headers_mut().insert(
            "cookie",
            format!("__Host-stationd-session={fresh}").parse().unwrap(),
        );
        next.headers_mut()
            .insert("origin", "https://remote.example.test".parse().unwrap());
        let (mut socket, _) = connect_async(next).await.unwrap();
        read_until(&mut socket, "SIZE:80x24").await;
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        // Periodic TUI output and acknowledgements do not extend idle lifetime.
        socket
            .send(Message::Text(
                r#"{"type":"input","data":"periodic"}"#.into(),
            ))
            .await
            .unwrap();
        read_until(&mut socket, "ECHO:periodic").await;
        let mut ticks = 0;
        let ended = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                match socket.next().await.unwrap().unwrap() {
                    Message::Binary(_) => {
                        ticks += 1;
                        socket
                            .send(Message::Text(r#"{"type":"ack"}"#.into()))
                            .await
                            .unwrap();
                    }
                    Message::Text(text) => break text,
                    other => panic!("unexpected {other:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert!(ticks > 0 && ended.contains("idle"));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        async fn reopen(
            app: &axum::Router,
            addr: std::net::SocketAddr,
            session: &str,
            csrf: &str,
        ) -> tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        > {
            let request = Request::builder()
                .method("POST")
                .uri("/api/stations/one/console")
                .header("cookie", format!("__Host-stationd-session={session}"))
                .header("origin", "https://remote.example.test")
                .header("accept-language", "de-DE")
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"cols":80,"rows":24}"#))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let data: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap(),
            )
            .unwrap();
            let mut request = format!(
                "ws://{addr}/api/stations/one/console/{}/ws",
                data["id"].as_str().unwrap()
            )
            .into_client_request()
            .unwrap();
            request.headers_mut().insert(
                "cookie",
                format!("__Host-stationd-session={session}")
                    .parse()
                    .unwrap(),
            );
            request
                .headers_mut()
                .insert("origin", "https://remote.example.test".parse().unwrap());
            connect_async(request).await.unwrap().0
        }
        // Closing the browser releases the child and quota.
        let proof = value["csrf_token"].as_str().unwrap();
        let mut socket = reopen(&app, addr, &fresh, proof).await;
        read_until(&mut socket, "SIZE:80x24").await;
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        socket.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while unsafe { libc::kill(pid, 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // Input keeps the console active but cannot extend its absolute duration.
        let mut socket = reopen(&app, addr, &fresh, proof).await;
        read_until(&mut socket, "SIZE:80x24").await;
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        let ended=tokio::time::timeout(Duration::from_secs(6),async {
            let mut tick=tokio::time::interval(Duration::from_millis(200));
            loop {
                tokio::select! {
                    _ = tick.tick() => { let _ = socket.send(Message::Text(r#"{"type":"input","data":"x"}"#.into())).await; },
                    frame = socket.next() => match frame.unwrap().unwrap() {
                        Message::Binary(_) => { let _ = socket.send(Message::Text(r#"{"type":"ack"}"#.into())).await; },
                        Message::Text(text) => break text,
                        other => panic!("unexpected {other:?}"),
                    }
                }
            }
        }).await.unwrap();
        assert!(ended.contains("duration"));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        let mut socket = reopen(&app, addr, &fresh, proof).await;
        read_until(&mut socket, "SIZE:80x24").await;
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        console.shutdown();
        tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "PTY child still alive after stop"
        );
        let audit = auth
            .lock()
            .unwrap()
            .admin(AdminRequest::Audit)
            .unwrap()
            .to_string();
        assert!(audit.contains("console.open") && audit.contains("console.close"));
        assert!(!audit.contains("Bonjour") && !audit.contains(&session));
        server.abort();
    }
}
