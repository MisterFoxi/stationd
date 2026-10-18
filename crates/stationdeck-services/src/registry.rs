//! Deterministic module lifecycle and revocable typed services. No UI or transport.
use crate::manifest::{Capability, Manifest};
use semver::{Version, VersionReq};
use std::{any::Any, cell::{Cell, RefCell}, collections::{BTreeMap, BTreeSet}, marker::PhantomData, rc::Rc};

type Bindings = BTreeMap<(String, String), String>;
type Services = Rc<RefCell<BTreeMap<(String, String), Published>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State { Disabled, Validated, Blocked, Initializing, Starting, Running, Stopping, Stopped, Failed }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic { pub message: String, pub chain: Vec<String> }
#[derive(Clone, Default)]
pub struct Grants {
    /// Trusted deployment policy, never populated from a module's role definitions.
    pub capabilities: Vec<Capability>,
    pub permissions: BTreeSet<String>,
}
#[derive(Clone, Default)]
pub struct Policy {
    pub grants: BTreeMap<String, Grants>,
    /// (consumer module, service id) -> provider module.
    pub bindings: Bindings,
}

/// An assembly-owned association between a stable service id/version and a Rust interface.
pub struct ServiceKey<T: ?Sized + 'static> {
    pub id: &'static str,
    pub version: &'static str,
    marker: PhantomData<fn(&T)>,
}
impl<T: ?Sized + 'static> ServiceKey<T> {
    pub const fn new(id: &'static str, version: &'static str) -> Self {
        Self { id, version, marker: PhantomData }
    }
}
#[derive(Clone)]
struct Published {
    value: Rc<dyn Any>,
    lease: Rc<Cell<bool>>,
    version: String,
}
/// A retained handle cannot outlive either module's grant. Callers cannot extract the service Rc.
pub struct ServiceHandle<T: ?Sized + 'static> {
    value: Rc<T>,
    provider: Rc<Cell<bool>>,
    consumer: Rc<Cell<bool>>,
}
impl<T: ?Sized + 'static> ServiceHandle<T> {
    pub fn with<R>(&self, call: impl FnOnce(&T) -> R) -> Result<R, String> {
        if !self.provider.get() || !self.consumer.get() {
            return Err("Service access has been revoked.".into());
        }
        Ok(call(self.value.as_ref()))
    }
}
#[derive(Clone)]
pub struct Context {
    manifest: Manifest,
    grants: Grants,
    lease: Rc<Cell<bool>>,
    bindings: BTreeMap<String, String>,
    services: Services,
    staged: Rc<RefCell<BTreeMap<String, Published>>>,
}
impl Context {
    pub fn authorize(&self, capability: &str, permission: &str, operation: &str) -> Result<(), String> {
        let granted = self.lease.get() && self.grants.permissions.contains(permission)
            && self.manifest.permissions.iter().any(|p| p.id == permission && p.resource == capability
                && p.actions.iter().any(|a| a == operation))
            && self.grants.capabilities.iter().any(|c| c.id == capability &&
                (c.operations.iter().any(|op| op == operation)
                || (c.kind == "network" && matches!(operation, "read" | "disconnect"))));
        if granted { Ok(()) } else { Err("Module operation is not authorized by local policy.".into()) }
    }
    pub fn contribution_allowed(&self, id: &str) -> bool {
        self.lease.get() && self.manifest.contributions.iter().any(|c| c.id == id
            && c.permissions.iter().all(|p| self.grants.permissions.contains(p))
            && c.services.iter().all(|s| self.manifest.provides.iter().any(|p| p.id == *s)
                || self.bindings.get(s).is_some_and(|provider|
                    self.services.borrow().contains_key(&(provider.clone(), s.clone())))))
    }
    pub fn publish<T: ?Sized + 'static>(&self, key: &ServiceKey<T>, value: Rc<T>) -> Result<(), String> {
        if !self.lease.get() || !self.manifest.provides.iter().any(|p| p.id == key.id && p.version == key.version) {
            return Err("Undeclared service publication or revoked access.".into());
        }
        let mut staged = self.staged.borrow_mut();
        if staged.contains_key(key.id) { return Err("Duplicate service publication.".into()); }
        staged.insert(key.id.into(), Published {
            value: Rc::new(value), lease: self.lease.clone(), version: key.version.into(),
        });
        Ok(())
    }
    pub fn service<T: ?Sized + 'static>(&self, key: &ServiceKey<T>, capability: &str, permission: &str,
        operation: &str) -> Result<ServiceHandle<T>, String> {
        self.authorize(capability, permission, operation)?;
        if !self.manifest.capabilities.iter().any(|c| c.id == capability && c.kind == "service" && c.resource == key.id) {
            return Err("Service is outside granted scope.".into());
        }
        let provider = self.bindings.get(key.id).ok_or("No service binding.")?;
        let services = self.services.borrow();
        let entry = services.get(&(provider.clone(), key.id.into())).ok_or("Service is unavailable.")?;
        if !VersionReq::parse(&format!("^{}", key.version)).is_ok_and(|r|
            Version::parse(&entry.version).is_ok_and(|v| r.matches(&v))) {
            return Err("Typed service version mismatch.".into());
        }
        let value = entry.value.downcast_ref::<Rc<T>>().ok_or("Typed service interface mismatch.")?;
        Ok(ServiceHandle { value: value.clone(), provider: entry.lease.clone(), consumer: self.lease.clone() })
    }
}

/// Hooks run on the host thread and must not block; stop cancels all private work.
/// Initializing/starting failures call stop even when publication never happened.
pub trait Module {
    fn id(&self) -> &str;
    fn manifest(&self) -> &str;
    fn initialize(&mut self, _context: &Context) -> Result<(), String> { Ok(()) }
    fn start(&mut self, _context: &Context) -> Result<(), String> { Ok(()) }
    fn stop(&mut self);
    fn dependencies_changed(&mut self, _context: &Context) {}
}
pub struct Entry<M> {
    pub module: M,
    pub manifest: Option<Manifest>,
    pub state: State,
    pub diagnostic: Option<Diagnostic>,
    pub requested: bool,
    context: Option<Context>,
    invalid: Option<String>,
    edges: BTreeSet<usize>,
}
pub struct Registry<M: Module> {
    entries: Vec<Entry<M>>,
    policy: Policy,
    services: Services,
    order: Vec<usize>,
}
impl<M: Module> Registry<M> {
    pub fn new(modules: Vec<M>, enabled: &[String], policy: Policy) -> Self {
        let mut entries: Vec<_> = modules.into_iter().map(|module| {
            let parsed = Manifest::parse(module.manifest());
            let invalid = match &parsed {
                Err(e) => Some(e.clone()),
                Ok(m) if m.module.id != module.id() || m.module.entrypoint != module.id() => Some("Manifest does not match compiled constructor.".into()),
                _ => None,
            };
            let requested = enabled.iter().any(|id| id == module.id());
            Entry { module, manifest: parsed.ok(), state: State::Validated, diagnostic: None,
                requested, context: None, invalid, edges: BTreeSet::new() }
        }).collect();
        let mut counts = BTreeMap::new();
        for e in &entries { *counts.entry(e.module.id().to_owned()).or_insert(0) += 1; }
        for e in &mut entries {
            if counts[e.module.id()] > 1 { e.invalid = Some("Duplicate compiled module identifier.".into()); }
        }
        let mut registry = Self { entries, policy, services: Rc::default(), order: vec![] };
        registry.resolve();
        registry
    }
    pub fn entries(&self) -> &[Entry<M>] { &self.entries }
    pub fn module_mut(&mut self, id: &str) -> Option<&mut M> {
        self.entries.iter_mut().find(|e| e.module.id() == id && e.state == State::Running).map(|e| &mut e.module)
    }
    pub fn contribution_allowed(&self, module: &str, contribution: &str) -> bool {
        self.entries.iter().any(|e| e.module.id() == module && e.state == State::Running
            && e.context.as_ref().is_some_and(|c| c.contribution_allowed(contribution)))
    }
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let index = self.entries.iter().position(|e| e.module.id() == id).ok_or("Unknown compiled module.")?;
        if !enabled { self.stop_dependents(index); }
        self.entries[index].requested = enabled;
        // Retry is explicit. Unrelated failed modules stay failed.
        if enabled && self.entries[index].state == State::Failed && self.entries[index].invalid.is_none() {
            self.entries[index].state = State::Validated;
            self.entries[index].diagnostic = None;
        }
        self.resolve();
        Ok(())
    }
    pub fn set_binding(&mut self, consumer: &str, service: &str, provider: &str) -> Result<(), String> {
        if !self.entries.iter().any(|e| e.module.id() == consumer && e.manifest.as_ref().is_some_and(|m| m.requires.iter().any(|r| r.id == service))) {
            return Err("Unknown consumer service requirement.".into());
        }
        self.policy.bindings.insert((consumer.into(), service.into()), provider.into());
        self.resolve();
        Ok(())
    }
    pub fn set_policy(&mut self, policy: Policy) {
        // Revoke every old context before applying changed grants/bindings.
        self.stop_all();
        self.policy = policy;
        self.resolve();
    }
    pub fn fail(&mut self, id: &str) -> Result<(), String> {
        let i = self.entries.iter().position(|e| e.module.id() == id).ok_or("Unknown compiled module.")?;
        self.stop_dependents(i);
        self.entries[i].state = State::Failed;
        self.entries[i].diagnostic = Some(Diagnostic { message: "Module execution failed.".into(), chain: vec![id.into()] });
        self.resolve();
        Ok(())
    }
    fn stop_one(&mut self, i: usize) {
        let entry = &mut self.entries[i];
        if let Some(context) = entry.context.take() {
            entry.state = State::Stopping;
            context.lease.set(false);
            self.services.borrow_mut().retain(|(provider, _), _| provider != entry.module.id());
            entry.module.stop();
            entry.state = State::Stopped;
        }
    }
    fn stop_dependents(&mut self, provider: usize) {
        // Optional edges affect ordering, but only mandatory loss stops consumers.
        let mut affected = BTreeSet::from([provider]);
        loop {
            let before = affected.len();
            for (i, e) in self.entries.iter().enumerate() {
                if e.edges.iter().any(|p| affected.contains(p) && self.mandatory(i, *p)) { affected.insert(i); }
            }
            if before == affected.len() { break; }
        }
        for i in self.order.clone().into_iter().rev() {
            if affected.contains(&i) { self.stop_one(i); }
        }
    }
    fn stop_all(&mut self) {
        for i in self.order.clone().into_iter().rev() { self.stop_one(i); }
    }
    fn mandatory(&self, consumer: usize, provider: usize) -> bool {
        let Some(m) = &self.entries[consumer].manifest else { return false; };
        let p = self.entries[provider].module.id();
        m.dependencies.iter().any(|r| r.id == p && !r.optional)
            || m.requires.iter().any(|r| !r.optional && self.binding(consumer, &r.id).as_deref() == Some(p))
    }
    fn binding(&self, consumer: usize, service: &str) -> Option<String> {
        let id = self.entries[consumer].module.id();
        if let Some(provider) = self.policy.bindings.get(&(id.into(), service.into())) { return Some(provider.clone()); }
        let m = self.entries[consumer].manifest.as_ref()?;
        let req = m.requires.iter().find(|r| r.id == service)?;
        let mut candidates = self.entries.iter().filter(|e| e.requested && e.invalid.is_none())
            .filter(|e| e.manifest.as_ref().is_some_and(|m| m.provides.iter().any(|p|
                p.id == service && compatible(&req.version, &p.version))));
        let one = candidates.next()?;
        if candidates.next().is_some() { None } else { Some(one.module.id().into()) }
    }
    fn block(&mut self, i: usize, message: &str, dependency: Option<usize>) {
        let mut chain = vec![self.entries[i].module.id().into()];
        if let Some(p) = dependency {
            if let Some(d) = &self.entries[p].diagnostic { chain.extend(d.chain.clone()); }
            else { chain.push(self.entries[p].module.id().into()); }
        }
        self.entries[i].state = State::Blocked;
        self.entries[i].diagnostic = Some(Diagnostic { message: message.into(), chain });
    }
    fn resolve(&mut self) {
        let old_bindings: Vec<_> = self.entries.iter().map(|e| e.context.as_ref().map(|c| c.bindings.clone())).collect();
        for e in &mut self.entries {
            e.edges.clear();
            if let Some(error) = &e.invalid {
                e.state = State::Failed;
                e.diagnostic = Some(Diagnostic { message: error.clone(), chain: vec![e.module.id().into()] });
            } else if !e.requested { e.state = State::Disabled; e.diagnostic = None; }
            else if e.state != State::Failed { e.state = State::Validated; e.diagnostic = None; }
        }
        let mut bindings: Vec<BTreeMap<String, String>> = vec![BTreeMap::new(); self.entries.len()];
        for (i, module_bindings) in bindings.iter_mut().enumerate() {
            if self.entries[i].state != State::Validated { continue; }
            let m = self.entries[i].manifest.clone().unwrap();
            let grants = self.policy.grants.get(&m.module.id).cloned().unwrap_or_default();
            if m.capabilities.iter().any(|c| !grants.capabilities.iter().any(|g| grants_capability(g, c))) {
                self.block(i, "Required capability denied by deployment policy.", None);
            }
            if grants.capabilities.iter().any(|g| !m.capabilities.iter().any(|c| c.id == g.id)) {
                self.block(i, "Policy references an undeclared capability.", None);
            }
            if grants.permissions.iter().any(|p| !m.permissions.iter().any(|declared| declared.id == *p)) {
                self.block(i, "Policy references an undeclared permission.", None);
            }
            for r in &m.dependencies {
                match self.entries.iter().position(|e| e.module.id() == r.id) {
                    None if r.optional => {},
                    None => self.block(i, &format!("Missing dependency: {}.", r.id), None),
                    Some(p) => {
                        self.entries[i].edges.insert(p);
                        if !self.entries[p].manifest.as_ref().is_some_and(|m| compatible(&r.version, &m.module.version)) {
                            self.block(i, "Incompatible dependency version.", Some(p));
                        }
                    }
                }
            }
            for r in &m.requires {
                if let Some(provider) = self.binding(i, &r.id) {
                    if let Some(p) = self.entries.iter().position(|e| e.module.id() == provider) {
                        let valid = self.entries[p].manifest.as_ref().is_some_and(|m|
                            m.provides.iter().any(|s| s.id == r.id && compatible(&r.version, &s.version)));
                        if valid {
                            self.entries[i].edges.insert(p);
                            module_bindings.insert(r.id.clone(), provider);
                        } else { self.block(i, "Incompatible service provider binding.", Some(p)); }
                    } else { self.block(i, "Bound service provider is missing.", None); }
                } else {
                    let present = self.entries.iter().any(|e| e.manifest.as_ref().is_some_and(|m| m.provides.iter().any(|s| s.id == r.id)) && e.requested);
                    if !r.optional || present { self.block(i, &format!("Missing, incompatible or ambiguous service binding: {}.", r.id), None); }
                }
            }
        }
        // Reachability marks only actual cycle members, not unrelated modules or their dependents.
        for i in 0..self.entries.len() {
            let mut pending: Vec<_> = self.entries[i].edges.iter().map(|p| (*p, vec![self.entries[i].module.id().to_owned()])).collect();
            let mut seen = BTreeSet::new();
            while let Some((p, mut chain)) = pending.pop() {
                chain.push(self.entries[p].module.id().to_owned());
                if p == i {
                    self.entries[i].state = State::Blocked;
                    self.entries[i].diagnostic = Some(Diagnostic { message: "Dependency cycle detected.".into(), chain });
                    break;
                }
                if seen.insert(p) { pending.extend(self.entries[p].edges.iter().map(|next| (*next, chain.clone()))); }
            }
        }
        // Stable topological order; unresolved cycles are appended for diagnostics/cleanup.
        let mut remaining: BTreeSet<_> = (0..self.entries.len()).collect();
        let mut order = vec![];
        while let Some(i) = remaining.iter().filter(|i| self.entries[**i].edges.iter().all(|p| !remaining.contains(p)))
            .min_by_key(|i| self.entries[**i].module.id()).copied() {
            remaining.remove(&i); order.push(i);
        }
        order.extend(remaining);
        // Propagate unavailable mandatory dependencies before retaining any old running context.
        loop {
            let mut changed = false;
            for &i in &order {
                if self.entries[i].state != State::Validated { continue; }
                let missing = self.entries[i].edges.iter().copied().find(|p| self.mandatory(i, *p)
                    && self.entries[*p].state != State::Validated);
                if let Some(p) = missing { self.block(i, "Required dependency is unavailable.", Some(p)); changed = true; }
            }
            if !changed { break; }
        }
        let mut retiring = BTreeSet::new();
        for (i, old) in old_bindings.iter().enumerate() {
            let changed = old.as_ref().is_some_and(|old| {
                let required = |service: &String| self.entries[i].manifest.as_ref().unwrap().requires.iter().any(|r| r.id == *service && !r.optional);
                old.iter().filter(|(s, _)| required(s)).ne(bindings[i].iter().filter(|(s, _)| required(s)))
            });
            if self.entries[i].context.is_some() && (self.entries[i].state != State::Validated || changed) { retiring.insert(i); }
        }
        loop {
            let before = retiring.len();
            for (i, e) in self.entries.iter().enumerate() {
                if e.edges.iter().any(|p| retiring.contains(p) && self.mandatory(i, *p)) { retiring.insert(i); }
            }
            if before == retiring.len() { break; }
        }
        for i in self.order.clone().into_iter().rev() {
            if retiring.contains(&i) { self.stop_one_preserving_state(i); }
        }
        self.order = order.clone();
        for i in order {
            if self.entries[i].state != State::Validated { continue; }
            let missing = self.entries[i].edges.iter().copied().find(|p| self.mandatory(i, *p) && self.entries[*p].state != State::Running);
            if let Some(p) = missing { self.block(i, "Required dependency failed to start.", Some(p)); self.stop_one_preserving_state(i); continue; }
            // Optional services are injected only while their providers are running.
            bindings[i].retain(|_, provider| self.entries.iter().any(|e| e.module.id() == provider.as_str() && e.state == State::Running));
            if let Some(context) = &mut self.entries[i].context {
                context.bindings = bindings[i].clone();
                let context = context.clone();
                self.entries[i].state = State::Running;
                self.entries[i].module.dependencies_changed(&context);
                continue;
            }
            let manifest = self.entries[i].manifest.clone().unwrap();
            let context = Context { grants: self.policy.grants.get(&manifest.module.id).cloned().unwrap_or_default(),
                manifest, lease: Rc::new(Cell::new(true)), bindings: bindings[i].clone(), services: self.services.clone(), staged: Rc::default() };
            self.entries[i].context = Some(context.clone());
            self.entries[i].state = State::Initializing;
            let result = self.entries[i].module.initialize(&context).and_then(|()| {
                // Initialization cannot publish a service before start succeeds.
                context.staged.borrow_mut().clear();
                self.entries[i].state = State::Starting;
                self.entries[i].module.start(&context)
            }).and_then(|()| {
                if context.manifest.provides.iter().all(|p| context.staged.borrow().contains_key(&p.id)) { Ok(()) }
                else { Err("Declared service was not published.".into()) }
            });
            if result.is_err() {
                self.stop_one(i);
                self.entries[i].state = State::Failed;
                // Hook errors may contain secrets. The host exposes a stable local diagnostic.
                self.entries[i].diagnostic = Some(Diagnostic { message: "Module initialization or startup failed.".into(), chain: vec![self.entries[i].module.id().into()] });
            } else {
                for (id, value) in std::mem::take(&mut *context.staged.borrow_mut()) {
                    self.services.borrow_mut().insert((self.entries[i].module.id().into(), id), value);
                }
                self.entries[i].state = State::Running;
            }
        }
    }
    fn stop_one_preserving_state(&mut self, i: usize) {
        let state = self.entries[i].state; self.stop_one(i); self.entries[i].state = state;
    }
}
impl<M: Module> Drop for Registry<M> { fn drop(&mut self) { self.stop_all(); } }
fn compatible(requirement: &str, version: &str) -> bool {
    VersionReq::parse(requirement).ok().zip(Version::parse(version).ok()).is_some_and(|(r, v)| r.matches(&v))
}
fn grants_capability(grant: &Capability, requested: &Capability) -> bool {
    grant.id == requested.id && grant.kind == requested.kind && grant.resource == requested.resource
        && requested.operations.iter().all(|op| grant.operations.contains(op))
        && requested.max_rows.is_none_or(|n| grant.max_rows.is_some_and(|limit| n <= limit))
        && requested.timeout_ms.is_none_or(|n| grant.timeout_ms.is_some_and(|limit| n <= limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    const VALUE: ServiceKey<Cell<u32>> = ServiceKey::new("test.status.value", "1.0.0");
    struct Sample {
        id: String,
        source: String,
        fail_init: bool,
        fail_start: bool,
        log: Rc<RefCell<Vec<String>>>,
        access: Option<ServiceHandle<Cell<u32>>>,
    }
    impl Module for Sample {
        fn id(&self) -> &str { &self.id }
        fn manifest(&self) -> &str { &self.source }
        fn initialize(&mut self, _: &Context) -> Result<(), String> {
            self.log.borrow_mut().push(format!("init:{}", self.id));
            if self.fail_init { Err("secret must not be displayed".into()) } else { Ok(()) }
        }
        fn start(&mut self, context: &Context) -> Result<(), String> {
            self.log.borrow_mut().push(format!("start:{}", self.id));
            if self.source.contains("[[provides]]") { context.publish(&VALUE, Rc::new(Cell::new(7)))?; }
            if self.source.contains("[[requires]]") {
                self.access = context.service(&VALUE, &format!("{}.status", self.id), &format!("{}.read", self.id), "read").ok();
            }
            if self.fail_start { Err("secret".into()) } else { Ok(()) }
        }
        fn stop(&mut self) { self.log.borrow_mut().push(format!("stop:{}", self.id)); }
        fn dependencies_changed(&mut self, context: &Context) {
            self.access = context.service(&VALUE, &format!("{}.status", self.id), &format!("{}.read", self.id), "read").ok();
        }
    }
    fn sample(id: &str, dependencies: &[(&str, bool)], provider: bool, consumer: Option<bool>, log: &Rc<RefCell<Vec<String>>>) -> Sample {
        let mut source = "manifest_version = 1\nroles = []\ncontributions = []\n".to_owned();
        if dependencies.is_empty() { source.push_str("dependencies = []\n"); }
        if !provider { source.push_str("provides = []\n"); }
        if consumer.is_none() { source.push_str("requires = []\ncapabilities = []\npermissions = []\n"); }
        source.push_str(&format!("[module]\nid = {id:?}\nversion = \"1.0.0\"\ncontract = \"^1.0.0\"\nentrypoint = {id:?}\n"));
        for (dependency, optional) in dependencies {
            source.push_str(&format!("[[dependencies]]\nid = {dependency:?}\nversion = \"^1.0.0\"\noptional = {optional}\n"));
        }
        if provider { source.push_str("[[provides]]\nid = \"test.status.value\"\nversion = \"1.0.0\"\n"); }
        if let Some(optional) = consumer {
            source.push_str(&format!(r#"
[[requires]]
id = "test.status.value"
version = "^1.0.0"
optional = {optional}
[[capabilities]]
id = "{id}.status"
kind = "service"
resource = "test.status.value"
operations = ["read"]
[[permissions]]
id = "{id}.read"
description = "Read local test status"
resource = "{id}.status"
actions = ["read"]
"#));
        }
        Sample { id: id.into(), source, fail_init: false, fail_start: false, log: log.clone(), access: None }
    }
    fn policy() -> Policy {
        let mut policy = Policy::default();
        policy.grants.insert("consumer".into(), Grants {
            capabilities: vec![Capability { id: "consumer.status".into(), kind: "service".into(), resource: VALUE.id.into(), operations: vec!["read".into()], max_rows: None, timeout_ms: None }],
            permissions: BTreeSet::from(["consumer.read".into()]),
        });
        policy
    }
    fn enabled(ids: &[&str]) -> Vec<String> { ids.iter().map(|s| (*s).into()).collect() }
    fn state(registry: &Registry<Sample>, id: &str) -> State {
        registry.entries().iter().find(|e| e.module.id == id).unwrap().state
    }
    #[test]
    fn cycles_missing_dependencies_and_duplicates_are_isolated() {
        let log = Rc::default();
        let registry = Registry::new(vec![
            sample("a", &[("b", false)], false, None, &log),
            sample("b", &[("a", true)], false, None, &log),
            sample("missing", &[("absent", false)], false, None, &log),
            sample("duplicate", &[], false, None, &log),
            sample("duplicate", &[], false, None, &log),
            sample("healthy", &[], false, None, &log),
        ], &enabled(&["a", "b", "missing", "duplicate", "healthy"]), Policy::default());
        assert_eq!(state(&registry, "healthy"), State::Running);
        for id in ["a", "b", "missing"] { assert_eq!(state(&registry, id), State::Blocked); }
        assert_eq!(state(&registry, "duplicate"), State::Failed);
        assert_eq!(&*log.borrow(), &["init:healthy", "start:healthy"]);
    }
    #[test]
    fn startup_failure_cleans_up_and_blocks_transitive_consumers() {
        for fail_init in [true, false] {
            let log = Rc::default();
            let mut provider = sample("provider", &[], true, None, &log);
            provider.fail_init = fail_init;
            provider.fail_start = !fail_init;
            let registry = Registry::new(vec![
                sample("consumer", &[], false, Some(false), &log), provider,
                sample("dependent", &[("consumer", false)], false, None, &log),
                sample("healthy", &[], false, None, &log),
            ], &enabled(&["provider", "consumer", "dependent", "healthy"]), policy());
            assert_eq!(state(&registry, "provider"), State::Failed);
            assert_eq!(state(&registry, "consumer"), State::Blocked);
            assert_eq!(state(&registry, "dependent"), State::Blocked);
            assert_eq!(state(&registry, "healthy"), State::Running);
            assert!(registry.services.borrow().is_empty());
            assert_eq!(log.borrow().iter().filter(|s| *s == "stop:provider").count(), 1);
            assert!(!log.borrow().iter().any(|s| s == "init:consumer" || s == "init:dependent"));
            let d = registry.entries().iter().find(|e| e.module.id == "dependent").unwrap().diagnostic.as_ref().unwrap();
            assert_eq!(d.chain, ["dependent", "consumer", "provider"]);
            assert!(!format!("{:?}", registry.entries()[1].diagnostic).contains("secret"));
        }
    }
    #[test]
    fn providers_are_ordered_and_handles_revoked_before_reverse_shutdown() {
        let log = Rc::default();
        let mut registry = Registry::new(vec![sample("consumer", &[], false, Some(false), &log), sample("provider", &[], true, None, &log)],
            &enabled(&["provider", "consumer"]), policy());
        assert_eq!(&*log.borrow(), &["init:provider", "start:provider", "init:consumer", "start:consumer"]);
        let handle = registry.module_mut("consumer").unwrap().access.take().unwrap();
        assert_eq!(handle.with(Cell::get).unwrap(), 7);
        log.borrow_mut().clear();
        registry.set_enabled("provider", false).unwrap();
        assert_eq!(&*log.borrow(), &["stop:consumer", "stop:provider"]);
        assert!(handle.with(Cell::get).is_err());
        assert_eq!(state(&registry, "consumer"), State::Blocked);
        registry.set_enabled("provider", true).unwrap();
        assert_eq!(state(&registry, "consumer"), State::Running);
        assert!(handle.with(Cell::get).is_err()); // Old grants never revive on restart.
    }
    #[test]
    fn ambiguity_needs_explicit_binding_and_optional_loss_keeps_consumer_running() {
        let log = Rc::default();
        let mut registry = Registry::new(vec![sample("consumer", &[], false, Some(true), &log), sample("one", &[], true, None, &log), sample("two", &[], true, None, &log)],
            &enabled(&["consumer", "one", "two"]), policy());
        assert_eq!(state(&registry, "consumer"), State::Blocked);
        let mut explicit = policy();
        explicit.bindings.insert(("consumer".into(), VALUE.id.into()), "one".into());
        registry.set_policy(explicit);
        assert_eq!(state(&registry, "consumer"), State::Running);
        let handle = registry.module_mut("consumer").unwrap().access.take().unwrap();
        log.borrow_mut().clear();
        registry.set_enabled("one", false).unwrap();
        assert_eq!(state(&registry, "consumer"), State::Running);
        assert!(!log.borrow().iter().any(|s| s == "stop:consumer"));
        assert!(handle.with(Cell::get).is_err());
    }
    #[test]
    fn absent_optional_dependency_is_allowed_but_present_incompatible_is_blocked() {
        let log = Rc::default();
        let mut registry = Registry::new(vec![sample("consumer", &[("provider", true)], false, None, &log)], &enabled(&["consumer"]), Policy::default());
        assert_eq!(state(&registry, "consumer"), State::Running);
        registry.set_enabled("consumer", false).unwrap();
        let mut provider = sample("provider", &[], false, None, &log);
        provider.source = provider.source.replace("version = \"1.0.0\"", "version = \"2.0.0\"");
        let registry = Registry::new(vec![sample("consumer", &[("provider", true)], false, None, &log), provider], &enabled(&["consumer", "provider"]), Policy::default());
        assert_eq!(state(&registry, "consumer"), State::Blocked);
    }
    #[test]
    fn policy_is_explicit_and_revocation_clears_retained_access() {
        let log = Rc::default();
        let mut registry = Registry::new(vec![sample("consumer", &[], false, Some(false), &log), sample("provider", &[], true, None, &log)], &enabled(&["consumer", "provider"]), Policy::default());
        assert_eq!(state(&registry, "consumer"), State::Blocked);
        registry.set_policy(policy());
        let handle = registry.module_mut("consumer").unwrap().access.take().unwrap();
        registry.set_policy(Policy::default());
        assert!(handle.with(Cell::get).is_err());
        assert_eq!(state(&registry, "consumer"), State::Blocked);
    }
    #[test]
    fn runtime_failure_withdraws_services_and_stops_required_consumers() {
        let log = Rc::default();
        let mut registry = Registry::new(vec![sample("consumer", &[], false, Some(false), &log), sample("provider", &[], true, None, &log)], &enabled(&["consumer", "provider"]), policy());
        registry.fail("provider").unwrap();
        assert_eq!(state(&registry, "provider"), State::Failed);
        assert_eq!(state(&registry, "consumer"), State::Blocked);
        assert!(registry.services.borrow().is_empty());
        registry.set_enabled("provider", true).unwrap();
        assert_eq!(state(&registry, "consumer"), State::Running);
    }
    #[test]
    fn schema_namespace_versions_references_and_unknown_keys_are_validated() {
        let log = Rc::default();
        let module = sample("consumer", &[], false, Some(false), &log);
        assert!(Manifest::parse(&module.source).is_ok());
        for source in [
            format!("unknown = true\n{}", module.source),
            module.source.replace("consumer.status", "foreign.status"),
            module.source.replace("^1.0.0", "^2.0.0"),
            module.source.replace("test.status.value", "*"),
            module.source.replace("actions = [\"read\"]", "actions = [\"write\"]"),
            module.source.replace("operations = [\"read\"]", "operations = [\"unknown\"]"),
        ] { assert!(Manifest::parse(&source).is_err(), "{source}"); }
    }
    #[test]
    fn wrong_rust_interface_is_rejected_and_rebinding_preserves_unrelated_modules() {
        let log = Rc::default();
        let mut explicit = policy();
        explicit.bindings.insert(("consumer".into(), VALUE.id.into()), "one".into());
        let mut registry = Registry::new(vec![sample("consumer", &[], false, Some(false), &log), sample("one", &[], true, None, &log), sample("two", &[], true, None, &log)],
            &enabled(&["consumer", "one", "two"]), explicit);
        let context = registry.entries[0].context.clone().unwrap();
        let wrong: ServiceKey<String> = ServiceKey::new(VALUE.id, VALUE.version);
        assert!(context.service(&wrong, "consumer.status", "consumer.read", "read").is_err());
        let old = registry.module_mut("consumer").unwrap().access.take().unwrap();
        log.borrow_mut().clear();
        registry.set_binding("consumer", VALUE.id, "two").unwrap();
        assert_eq!(&*log.borrow(), &["stop:consumer", "init:consumer", "start:consumer"]);
        assert!(old.with(Cell::get).is_err());
        assert_eq!(registry.module_mut("consumer").unwrap().access.as_ref().unwrap().with(Cell::get).unwrap(), 7);
    }
}
