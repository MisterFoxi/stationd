//! Closed V1 manifest schema and validation, independent of the UI.
use semver::{Version, VersionReq};
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u32,
    pub module: ModuleInfo,
    pub dependencies: Vec<Requirement>,
    pub provides: Vec<Provided>,
    pub requires: Vec<Requirement>,
    pub capabilities: Vec<Capability>,
    pub permissions: Vec<Permission>,
    pub roles: Vec<Role>,
    pub contributions: Vec<Contribution>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleInfo {
    pub id: String,
    pub version: String,
    pub contract: String,
    pub entrypoint: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub version: String,
    pub optional: bool,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provided { pub id: String, pub version: String }
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub id: String,
    pub kind: String,
    pub resource: String,
    pub operations: Vec<String>,
    pub max_rows: Option<u32>,
    pub timeout_ms: Option<u32>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permission {
    pub id: String,
    pub description: String,
    pub resource: String,
    pub actions: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role { pub id: String, pub description: String, pub permissions: Vec<String> }
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub slot: String,
    pub order: i32,
    pub services: Vec<String>,
    pub permissions: Vec<String>,
    pub action: Option<String>,
}
pub fn local_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.as_bytes()[0].is_ascii_lowercase()
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
pub fn service_id(id: &str) -> bool {
    id.contains('.') && id.split('.').all(local_id)
}
fn unique<'a>(ids: impl Iterator<Item = &'a str>) -> bool {
    let mut seen = BTreeSet::new();
    ids.into_iter().all(|id| seen.insert(id))
}
impl Manifest {
    pub fn parse(source: &str) -> Result<Self, String> {
        // Never expose TOML source (which may contain deployment data) in diagnostics.
        let manifest: Self = toml::from_str(source).map_err(|_| "Invalid V1 manifest schema.".to_owned())?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<(), String> {
        let reject = |message: &str| Err(message.to_owned());
        let m = &self.module;
        if self.manifest_version != 1 || !local_id(&m.id) || !local_id(&m.entrypoint)
            || Version::parse(&m.version).is_err()
            || !VersionReq::parse(&m.contract).is_ok_and(|r| r.matches(&Version::new(1, 0, 0))) {
            return reject("Invalid module identity, version or host contract.");
        }
        let prefix = format!("{}.", m.id);
        let qualified = |id: &str| id.strip_prefix(&prefix).is_some_and(local_id);
        for ids in [
            self.capabilities.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            self.permissions.iter().map(|x| x.id.as_str()).collect(),
            self.roles.iter().map(|x| x.id.as_str()).collect(),
            self.contributions.iter().map(|x| x.id.as_str()).collect(),
        ] {
            if !ids.iter().all(|id| qualified(id)) || !unique(ids.into_iter()) {
                return reject("Invalid or duplicate qualified identifier.");
            }
        }
        for (requirements, module) in [(&self.dependencies, true), (&self.requires, false)] {
            if !unique(requirements.iter().map(|r| r.id.as_str())) || requirements.iter().any(|r|
                !(if module { local_id(&r.id) } else { service_id(&r.id) }) || VersionReq::parse(&r.version).is_err()) {
                return reject("Invalid dependency or service requirement.");
            }
        }
        if !unique(self.provides.iter().map(|p| p.id.as_str())) || self.provides.iter().any(|p|
            !service_id(&p.id) || Version::parse(&p.version).is_err()) {
            return reject("Invalid provided service.");
        }
        let known_service = |id: &str| self.provides.iter().any(|s| s.id == id) || self.requires.iter().any(|s| s.id == id);
        for cap in &self.capabilities {
            let allowed: &[&str] = match cap.kind.as_str() {
                "service" if known_service(&cap.resource) => &["read", "watch", "command"],
                "network" if cap.resource.starts_with("connection:") && local_id(&cap.resource[11..]) => &["connect"],
                "filesystem" if cap.resource.starts_with("root:") && local_id(&cap.resource[5..]) => &["read", "write"],
                "db" if cap.resource.starts_with("plugin:") && local_id(&cap.resource[7..])
                    && cap.max_rows.is_some_and(|v| v > 0) && cap.timeout_ms.is_some_and(|v| v > 0) => &["read", "write"],
                _ => return reject("Unknown capability type, resource or missing limits."),
            };
            if cap.operations.is_empty() || !unique(cap.operations.iter().map(String::as_str))
                || cap.operations.iter().any(|op| !allowed.contains(&op.as_str()))
                || (cap.kind != "db" && (cap.max_rows.is_some() || cap.timeout_ms.is_some())) {
                return reject("Invalid capability operations or limits.");
            }
        }
        for permission in &self.permissions {
            if permission.description.trim().is_empty() || permission.actions.is_empty()
                || !unique(permission.actions.iter().map(String::as_str))
                || !self.capabilities.iter().any(|c| c.id == permission.resource) {
                return reject("Invalid permission resource or actions.");
            }
            let cap = self.capabilities.iter().find(|c| c.id == permission.resource).unwrap();
            if permission.actions.iter().any(|a| !cap.operations.contains(a)
                && !(cap.kind == "network" && matches!(a.as_str(), "read" | "disconnect"))) {
                return reject("Permission action outside capability scope.");
            }
        }
        let known_permission = |id: &str| self.permissions.iter().any(|p| p.id == id);
        if self.roles.iter().any(|r| r.description.trim().is_empty()
            || !unique(r.permissions.iter().map(String::as_str)) || r.permissions.iter().any(|p| !known_permission(p))) {
            return reject("Invalid role permission reference.");
        }
        for c in &self.contributions {
            if c.title.trim().is_empty() || !matches!((c.kind.as_str(), c.slot.as_str()),
                ("view", "navigation") | ("action", "toolbar") | ("menu", "main-menu") | ("indicator", "status"))
                || c.services.iter().any(|s| !known_service(s)) || c.permissions.iter().any(|p| !known_permission(p))
                || !unique(c.services.iter().map(String::as_str)) || !unique(c.permissions.iter().map(String::as_str)) {
                return reject("Invalid contribution slot or reference.");
            }
            if c.kind == "menu" {
                if !c.action.as_ref().is_some_and(|a| self.contributions.iter().any(|x| x.id == *a && x.kind == "action")) {
                    return reject("Menu must reference a local action.");
                }
            } else if c.action.is_some() { return reject("Only menus reference actions."); }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_manifests_obey_the_same_closed_schema() {
        for source in [
            include_str!("../../../Doc/stationdeck/demo.module.toml"),
            include_str!("../../../Doc/stationdeck/grpc.module.toml"),
            include_str!("../../../Doc/stationdeck/configuration.module.toml"),
            include_str!("../../../Doc/stationdeck/overview.module.toml"),
            include_str!("../../../crates/stationdeck-icecast/module.toml"),
            include_str!("../../../plugins/icecastd-deck/module.toml"),
        ] { Manifest::parse(source).unwrap(); }
    }
    #[test]
    fn foreign_roles_duplicate_permissions_and_bad_menu_targets_are_rejected() {
        let source = include_str!("../../../Doc/stationdeck/demo.module.toml");
        for invalid in [source.replace("permissions = [\"demo.consulter\"]", "permissions = [\"grpc.consulter\"]"),
            source.replace("id = \"demo.simuler\"", "id = \"demo.consulter\""),
            source.replace("action = \"demo.simulate\"", "action = \"demo.overview\"") ] {
            assert!(Manifest::parse(&invalid).is_err());
        }
    }
}
