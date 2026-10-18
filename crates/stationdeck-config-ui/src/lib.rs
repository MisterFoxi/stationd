//! Schema-driven authoring. No daemon, protobuf, or native transport in the UI.
use stationdeck_services::{config::*, RequestSlot, RequestState};
use stationdeck_ui::{DeckModule, ModuleDescriptor, ViewDescriptor};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
enum Reply {
    Catalog(Catalog),
    Target(TargetView),
}
fn report_age(observed_at: u64) -> u64 {
    #[cfg(target_arch = "wasm32")]
    let now = (js_sys::Date::now() / 1000.0) as u64;
    #[cfg(not(target_arch = "wasm32"))]
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_sub(observed_at)
}
#[derive(Default)]
pub struct ConfigModule {
    token: String,
    catalog: Option<Rc<Catalog>>,
    selected: String,
    current: Option<Rc<TargetView>>,
    documents: Vec<Document>,
    dirty: bool,
    message: String,
    request: Rc<RefCell<RequestSlot<Reply>>>,
    #[cfg(target_arch = "wasm32")]
    abort: Option<web_sys::AbortController>,
}
impl ConfigModule {
    #[cfg(target_arch = "wasm32")]
    fn send(&mut self, ctx: egui::Context, method: &str, path: String, body: Option<String>) {
        let Some(generation) = self.request.borrow_mut().begin() else {
            return;
        };
        let controller = match web_sys::AbortController::new() {
            Ok(c) => c,
            Err(_) => {
                self.request
                    .borrow_mut()
                    .finish(generation, Err("Cannot start the web request.".into()));
                return;
            }
        };
        let signal = controller.signal();
        let timeout_controller = controller.clone();
        self.abort = Some(controller);
        let token = self.token.clone();
        let method = method.to_owned();
        let slot = self.request.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _timeout =
                gloo_timers::callback::Timeout::new(7000, move || timeout_controller.abort());
            let result = async {
                use gloo_net::http::Request;
                let builder = match method.as_str() {
                    "PUT" => Request::put(&path),
                    "POST" => Request::post(&path),
                    _ => Request::get(&path),
                }
                .header("Authorization", &format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .abort_signal(Some(&signal));
                let response = if let Some(body) = body {
                    builder
                        .body(body)
                        .map_err(|_| "Cannot encode configuration request.")?
                        .send()
                        .await
                } else {
                    builder.send().await
                }
                .map_err(|_| "Request cancelled, timed out, or authority unavailable.")?;
                if matches!(response.status(), 401 | 403) {
                    return Err(
                        "Access denied. Configuration credentials are invalid or insufficient."
                            .to_owned(),
                    );
                }
                if !response.ok() {
                    let value = response
                        .json::<serde_json::Value>()
                        .await
                        .map_err(|_| "Configuration request was refused.")?;
                    return Err(value
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Configuration request was refused.")
                        .to_owned());
                }
                if path.ends_with("/catalog") {
                    response
                        .json::<Catalog>()
                        .await
                        .map(Reply::Catalog)
                        .map_err(|_| "Invalid authority response.".into())
                } else {
                    response
                        .json::<TargetView>()
                        .await
                        .map(Reply::Target)
                        .map_err(|_| "Invalid target response.".into())
                }
            }
            .await;
            if slot.borrow_mut().finish(generation, result) {
                ctx.request_repaint();
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn send(&mut self, _: egui::Context, _: &str, _: String, _: Option<String>) {
        self.message = "Web transport requires the browser build.".into();
    }
    fn select(&mut self, target: TargetView) {
        self.selected = target.id.clone();
        self.documents = target.draft.clone();
        self.current = Some(Rc::new(target));
        self.dirty = false;
    }
    fn receive(&mut self) {
        let response = {
            let mut slot = self.request.borrow_mut();
            if matches!(slot.state, RequestState::Ready(_) | RequestState::Failed(_)) {
                Some(std::mem::take(&mut slot.state))
            } else {
                None
            }
        };
        match response {
            Some(RequestState::Ready(Reply::Catalog(c))) => {
                if !c.targets.iter().any(|t| t.id == self.selected) {
                    self.current = None;
                    self.documents.clear();
                    self.dirty = false;
                    self.selected.clear();
                }
                let selected = c
                    .targets
                    .iter()
                    .find(|t| t.id == self.selected)
                    .or_else(|| c.targets.first())
                    .cloned();
                if !self.dirty {
                    if let Some(t) = selected {
                        self.select(t);
                    }
                }
                self.catalog = Some(Rc::new(c));
                self.message = "Configuration authority connected.".into();
            }
            Some(RequestState::Ready(Reply::Target(t))) => {
                if let Some(c) = &mut self.catalog {
                    if let Some(old) = Rc::make_mut(c)
                        .targets
                        .iter_mut()
                        .find(|old| old.id == t.id)
                    {
                        *old = t.clone();
                    }
                }
                self.select(t);
                self.message =
                    "Configuration state saved. Refresh to check the agent report.".into();
            }
            Some(RequestState::Failed(message)) => {
                if message.starts_with("Access denied.") {
                    self.stop();
                }
                self.message = message;
            }
            _ => {}
        }
    }
}
impl DeckModule for ConfigModule {
    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: "configuration",
            title: "Configuration authoring",
            description:
                "Publish versioned plugin configuration and inspect agent application reports.",
        }
    }
    fn views(&self) -> Vec<ViewDescriptor> {
        vec![ViewDescriptor {
            id: "configuration.authoring",
            title: "Configuration",
            order: 30,
        }]
    }
    fn stop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(c) = self.abort.take() {
            c.abort();
        }
        self.request.borrow_mut().cancel();
        self.token.clear();
        self.catalog = None;
        self.current = None;
        self.documents.clear();
        self.selected.clear();
        self.message.clear();
        self.dirty = false;
    }
    fn ui(&mut self, _: &str, ui: &mut egui::Ui) {
        self.receive();
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
        ui.heading("Configuration authoring");
        ui.label("Edit a draft, save it, then publish it for the selected agent.");
        let pending = matches!(self.request.borrow().state, RequestState::Pending);
        ui.label("Configuration author token");
        if ui
            .add_enabled(
                !pending,
                egui::TextEdit::singleline(&mut self.token)
                    .password(true)
                    .desired_width(320.0),
            )
            .changed()
        {
            let token = std::mem::take(&mut self.token);
            self.stop();
            self.token = token;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !pending && !self.token.is_empty(),
                    egui::Button::new("Connect / refresh configuration"),
                )
                .clicked()
            {
                self.send(ui.ctx().clone(), "GET", "/api/config/catalog".into(), None);
            }
            if ui.button("Disconnect configuration").clicked() {
                self.stop();
            }
            if pending {
                ui.spinner();
            }
        });
        if !self.message.is_empty() {
            ui.label(&self.message);
        }
        let Some(catalog) = self.catalog.clone() else {
            ui.weak("Credentials and drafts stay in module memory only. The backend owns published configuration.");
            return;
        };
        let targets = &catalog.targets;
        let schemas = &catalog.schemas;
        let permissions = &catalog.permissions;
        ui.weak(format!("Authority: {}", catalog.authority));
        let old = self.selected.clone();
        ui.add_enabled_ui(!pending && !self.dirty, |ui| {
            egui::ComboBox::from_id_salt("config.target")
                .selected_text(&self.selected)
                .show_ui(ui, |ui| {
                    for target in targets {
                        ui.selectable_value(&mut self.selected, target.id.clone(), &target.id);
                    }
                });
        });
        if self.selected != old {
            if let Some(t) = targets.iter().find(|t| t.id == self.selected) {
                self.select(t.clone());
            }
        }
        let Some(current) = self.current.clone() else {
            return;
        };
        ui.label(format!(
            "Draft version: {}  |  Published revision: {}  |  Last confirmed revision: {}",
            current.draft_version,
            current.published.as_ref().map_or(0, |b| b.revision),
            current.applied_revision
        ));
        match &current.report {
            Some(r) => {
                let outcome = match r.code.as_str() {
                    "applied" => "Applied",
                    "validation_failed" => "Local validation failed",
                    "apply_failed" => "Application failed",
                    "cache_failed" => "Local configuration could not be persisted",
                    _ => "Unknown application result",
                };
                ui.label(format!(
                    "Last fetched agent report: revision {} — {} ({}s old)",
                    r.revision,
                    outcome,
                    report_age(r.observed_at)
                ));
            }
            None => {
                ui.weak("No agent application report yet.");
            }
        }
        let editable = permissions.iter().any(|p| p == "edit") && !pending;
        if !current.history.is_empty() {
            egui::CollapsingHeader::new("Published history").show(ui, |ui| {
                for bundle in current.history.iter().rev() {
                    if ui
                        .add_enabled(
                            editable,
                            egui::Button::new(format!(
                                "Use revision {} as a new draft",
                                bundle.revision
                            )),
                        )
                        .clicked()
                    {
                        self.documents = bundle.documents.clone();
                        self.dirty = true;
                    }
                }
            });
        }
        ui.add_enabled_ui(editable, |ui| {
            let mut remove = None;
            for (index, doc) in self.documents.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.strong(format!(
                            "{} / {} (schema {})",
                            doc.plugin, doc.instance, doc.version
                        ));
                        ui.horizontal(|ui| {
                            ui.label("Instance");
                            if ui.text_edit_singleline(&mut doc.instance).changed() {
                                self.dirty = true;
                            }
                            if ui.button("Remove from draft").clicked() {
                                remove = Some(index);
                            }
                        });
                        if let Some(schema) = schemas
                            .iter()
                            .find(|s| s.plugin == doc.plugin && s.version == doc.version)
                        {
                            ui.weak(format!("Application: {}", schema.apply_mode));
                            for field in &schema.fields {
                                let Some(value) = doc.values.get_mut(&field.key) else {
                                    continue;
                                };
                                ui.horizontal(|ui| {
                                    ui.label(&field.key);
                                    match field.kind.as_str() {
                                        "string" => {
                                            let mut v =
                                                value.as_str().unwrap_or_default().to_owned();
                                            if ui.text_edit_singleline(&mut v).changed() {
                                                *value = v.into();
                                                self.dirty = true;
                                            }
                                        }
                                        "boolean" => {
                                            let mut v = value.as_bool().unwrap_or(false);
                                            if ui.checkbox(&mut v, "").changed() {
                                                *value = v.into();
                                                self.dirty = true;
                                            }
                                        }
                                        "integer" => {
                                            let mut v = value.as_i64().unwrap_or(0);
                                            if ui
                                                .add(egui::DragValue::new(&mut v).range(
                                                    field.minimum.unwrap_or(i64::MIN)
                                                        ..=field.maximum.unwrap_or(i64::MAX),
                                                ))
                                                .changed()
                                            {
                                                *value = v.into();
                                                self.dirty = true;
                                            }
                                        }
                                        _ => {
                                            ui.label("Unsupported field type.");
                                        }
                                    }
                                });
                            }
                        } else {
                            ui.label("Unsupported plugin schema.");
                        }
                    });
                });
            }
            if let Some(index) = remove {
                self.documents.remove(index);
                self.dirty = true;
            }
            if self.documents.len() < 64 {
                for schema in schemas {
                    if ui
                        .button(format!("Add {} configuration", schema.plugin))
                        .clicked()
                    {
                        let values: BTreeMap<_, _> = schema
                            .fields
                            .iter()
                            .map(|f| {
                                (
                                    f.key.clone(),
                                    match f.kind.as_str() {
                                        "integer" => serde_json::json!(f.minimum.unwrap_or(0)),
                                        "boolean" => serde_json::json!(false),
                                        _ => serde_json::json!(""),
                                    },
                                )
                            })
                            .collect();
                        self.documents.push(Document {
                            plugin: schema.plugin.clone(),
                            instance: if self
                                .documents
                                .iter()
                                .any(|d| d.plugin == schema.plugin && d.instance == "default")
                            {
                                format!("instance-{}", self.documents.len() + 1)
                            } else {
                                "default".into()
                            },
                            version: schema.version,
                            values,
                        });
                        self.dirty = true;
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    editable && self.dirty && !self.documents.is_empty(),
                    egui::Button::new("Save draft"),
                )
                .clicked()
            {
                let body = serde_json::to_string(&DraftRequest {
                    base_draft: current.draft_version,
                    documents: self.documents.clone(),
                })
                .unwrap();
                self.send(
                    ui.ctx().clone(),
                    "PUT",
                    format!("/api/config/targets/{}/draft", self.selected),
                    Some(body),
                );
            }
            if ui
                .add_enabled(
                    !pending
                        && !self.dirty
                        && !current.draft.is_empty()
                        && permissions.iter().any(|p| p == "publish"),
                    egui::Button::new("Publish configuration"),
                )
                .clicked()
            {
                let body = serde_json::to_string(&PublishRequest {
                    draft_version: current.draft_version,
                    base_revision: current.published.as_ref().map_or(0, |b| b.revision),
                })
                .unwrap();
                self.send(
                    ui.ctx().clone(),
                    "POST",
                    format!("/api/config/targets/{}/publish", self.selected),
                    Some(body),
                );
            }
            if ui
                .add_enabled(
                    !pending && self.dirty,
                    egui::Button::new("Discard local edits"),
                )
                .clicked()
            {
                self.documents = current.draft.clone();
                self.dirty = false;
            }
        });
        if self.dirty {
            ui.weak("Unsaved local edits. Save before publishing or changing targets.");
        }
        ui.weak("Publishing changes desired configuration. The agent validates and reports what it actually applied.");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disable_clears_author_credentials_and_private_draft() {
        let mut module = ConfigModule {
            token: "private-token".into(),
            dirty: true,
            ..Default::default()
        };
        let generation = module.request.borrow_mut().begin().unwrap();
        module.stop();
        assert!(module.token.is_empty() && !module.dirty && module.documents.is_empty());
        assert!(!module
            .request
            .borrow_mut()
            .finish(generation, Err("late".into())));
    }
    #[test]
    fn changed_target_scope_purges_private_local_edits() {
        let mut module = ConfigModule {
            selected: "removed".into(),
            dirty: true,
            current: Some(Rc::new(TargetView {
                id: "removed".into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        module.documents.push(Document {
            plugin: "demo.settings".into(),
            instance: "default".into(),
            version: 1,
            values: BTreeMap::new(),
        });
        let generation = module.request.borrow_mut().begin().unwrap();
        module.request.borrow_mut().finish(
            generation,
            Ok(Reply::Catalog(Catalog {
                authority: "test".into(),
                schemas: vec![],
                targets: vec![],
                permissions: vec!["read".into()],
            })),
        );
        module.receive();
        assert!(
            module.current.is_none()
                && module.documents.is_empty()
                && !module.dirty
                && module.selected.is_empty()
        );
    }
}
