//! UI contributions backed by the transport-free module registry.
use egui::{Color32, RichText, Ui};

/// A stable, module-qualified id, never derived from a translated title.
pub struct ViewDescriptor {
    pub id: &'static str,
    pub title: &'static str,
    pub order: i32,
}

/// Called only on the UI thread. Network/long work must run elsewhere.
/// The host controls navigation, layout, and whether a view may be opened.
pub trait ViewContribution {
    fn descriptor(&self) -> ViewDescriptor;
    fn ui(&mut self, ui: &mut Ui);
}

/// Shared presentation states. Messages must already be safe for the user.
pub enum ViewState<'a> {
    Empty(&'a str),
    Loading(&'a str),
    Error(&'a str),
    Unavailable(&'a str),
    Unauthenticated,
    Forbidden,
}

pub fn show_state(ui: &mut Ui, state: ViewState<'_>) {
    let (title, detail, color, loading) = match state {
        ViewState::Empty(detail) => (
            "No content",
            detail,
            Color32::from_rgb(100, 160, 170),
            false,
        ),
        ViewState::Loading(detail) => ("Loading", detail, Color32::from_rgb(100, 160, 170), true),
        ViewState::Error(detail) => (
            "An error occurred",
            detail,
            Color32::from_rgb(220, 100, 95),
            false,
        ),
        ViewState::Unavailable(detail) => (
            "Unavailable",
            detail,
            Color32::from_rgb(195, 150, 75),
            false,
        ),
        ViewState::Unauthenticated => (
            "Not signed in",
            "A verified session is required to access remote features.",
            Color32::from_rgb(195, 150, 75),
            false,
        ),
        ViewState::Forbidden => (
            "Access denied",
            "You do not have permission to use this action.",
            Color32::from_rgb(220, 100, 95),
            false,
        ),
    };
    egui::Frame::group(ui.style())
        .inner_margin(16.0)
        .show(ui, |ui| {
            ui.set_min_width((ui.available_width() - 4.0).max(0.0));
            ui.horizontal_wrapped(|ui| {
                if loading {
                    ui.spinner();
                }
                ui.label(RichText::new(title).strong().color(color));
            });
            ui.add_space(6.0);
            ui.label(detail);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_states_render_at_narrow_width_without_services() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(360.0, 900.0),
            )),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for state in [
                    ViewState::Empty("No module."),
                    ViewState::Loading("Waiting."),
                    ViewState::Error("Diagnostic."),
                    ViewState::Unavailable("Service unavailable."),
                    ViewState::Unauthenticated,
                    ViewState::Forbidden,
                ] {
                    show_state(ui, state);
                }
            });
        });
        assert!(!output.shapes.is_empty());
    }
}

pub struct ModuleDescriptor {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

/// Compiled, trusted module. The host owns enable/disable and view selection.
/// stop must cancel outstanding work and revoke any in-memory credentials.
pub trait DeckModule {
    fn manifest(&self) -> &str;
    fn initialize(
        &mut self,
        _context: &stationdeck_services::registry::Context,
    ) -> Result<(), String> {
        Ok(())
    }
    fn start(&mut self, _context: &stationdeck_services::registry::Context) -> Result<(), String> {
        Ok(())
    }
    fn dependencies_changed(&mut self, _context: &stationdeck_services::registry::Context) {}
    fn descriptor(&self) -> ModuleDescriptor;
    fn views(&self) -> Vec<ViewDescriptor>;
    fn ui(&mut self, id: &str, ui: &mut Ui);
    /// Compact, read-only status, rendered even when the plugin window is closed.
    fn summary(&self, _ui: &mut Ui) {}
    fn stop(&mut self);
}

pub fn show_station_status(ui: &mut Ui, status: &stationdeck_services::StationStatus) {
    egui::Grid::new("station.status")
        .num_columns(2)
        .spacing([24.0, 12.0])
        .show(ui, |ui| {
            for (key, value) in [
                ("Station", status.station_name.clone()),
                ("Uptime", format!("{} seconds", status.uptime_seconds)),
                ("Process ID", status.pid.to_string()),
                ("Time zone", status.timezone.clone()),
            ] {
                ui.strong(key);
                ui.label(value);
                ui.end_row();
            }
        });
}
/// The UI adapter delegates lifecycle and dependency resolution to the core registry.
pub struct UiModule(Box<dyn DeckModule>);
impl stationdeck_services::registry::Module for UiModule {
    fn id(&self) -> &str {
        self.0.descriptor().id
    }
    fn manifest(&self) -> &str {
        self.0.manifest()
    }
    fn initialize(
        &mut self,
        context: &stationdeck_services::registry::Context,
    ) -> Result<(), String> {
        let manifest = stationdeck_services::manifest::Manifest::parse(self.0.manifest())?;
        let mut seen = std::collections::BTreeSet::new();
        for view in self.0.views() {
            if !seen.insert(view.id)
                || !manifest
                    .contributions
                    .iter()
                    .any(|c| c.id == view.id && c.kind == "view")
            {
                return Err("Undeclared or duplicate UI view.".into());
            }
        }
        self.0.initialize(context)
    }
    fn start(&mut self, context: &stationdeck_services::registry::Context) -> Result<(), String> {
        self.0.start(context)
    }
    fn stop(&mut self) {
        self.0.stop();
    }
    fn dependencies_changed(&mut self, context: &stationdeck_services::registry::Context) {
        self.0.dependencies_changed(context);
    }
}
pub struct ModuleCatalog {
    registry: stationdeck_services::registry::Registry<UiModule>,
}
impl ModuleCatalog {
    pub fn summaries(&self, ui: &mut Ui) {
        for entry in self.registry.entries() {
            if entry.state == stationdeck_services::registry::State::Running {
                ui.push_id(entry.module.0.descriptor().id, |ui| {
                    entry.module.0.summary(ui)
                });
            }
        }
    }
    pub fn new(
        modules: Vec<Box<dyn DeckModule>>,
        enabled: &[String],
        policy: stationdeck_services::registry::Policy,
    ) -> Self {
        Self {
            registry: stationdeck_services::registry::Registry::new(
                modules.into_iter().map(UiModule).collect(),
                enabled,
                policy,
            ),
        }
    }
    pub fn descriptors(&self) -> Vec<ModuleDescriptor> {
        self.registry
            .entries()
            .iter()
            .map(|e| e.module.0.descriptor())
            .collect()
    }
    pub fn is_enabled(&self, id: &str) -> bool {
        self.registry
            .entries()
            .iter()
            .any(|e| e.module.0.descriptor().id == id && e.requested)
    }
    pub fn enabled_ids(&self) -> Vec<String> {
        self.registry
            .entries()
            .iter()
            .filter(|e| e.requested)
            .map(|e| e.module.0.descriptor().id.into())
            .collect()
    }
    pub fn status(
        &self,
        id: &str,
    ) -> Option<(
        stationdeck_services::registry::State,
        Option<&stationdeck_services::registry::Diagnostic>,
    )> {
        self.registry
            .entries()
            .iter()
            .find(|e| e.module.0.descriptor().id == id)
            .map(|e| (e.state, e.diagnostic.as_ref()))
    }
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        self.registry.set_enabled(id, enabled)
    }
    pub fn set_binding(
        &mut self,
        consumer: &str,
        service: &str,
        provider: &str,
    ) -> Result<(), String> {
        self.registry.set_binding(consumer, service, provider)
    }
    pub fn views(&self) -> Vec<ViewDescriptor> {
        let mut views = vec![];
        for entry in self.registry.entries() {
            let Some(manifest) = &entry.manifest else {
                continue;
            };
            for mut view in entry.module.0.views() {
                if self
                    .registry
                    .contribution_allowed(entry.module.0.descriptor().id, view.id)
                {
                    if let Some(c) = manifest.contributions.iter().find(|c| c.id == view.id) {
                        view.order = c.order;
                    }
                    views.push(view);
                }
            }
        }
        views.sort_by_key(|v| (v.order, v.id.split('.').next().unwrap_or_default(), v.id));
        views
    }
    pub fn render(&mut self, id: &str, ui: &mut Ui) -> bool {
        if !self.views().iter().any(|v| v.id == id) {
            return false;
        }
        let module = id.split('.').next().unwrap_or_default();
        if let Some(module) = self.registry.module_mut(module) {
            ui.push_id(id, |ui| module.0.ui(id, ui));
            return true;
        }
        false
    }
}
#[cfg(test)]
mod catalog_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Sample {
        id: &'static str,
        view: &'static str,
        stopped: Rc<Cell<usize>>,
    }
    impl DeckModule for Sample {
        fn manifest(&self) -> &str {
            r#"manifest_version = 1
 dependencies = []
 provides = []
 requires = []
 capabilities = []
 permissions = []
 roles = []
 [module]
 id = "sample"
 version = "0.1.0"
 contract = "^1.0.0"
 entrypoint = "sample"
 [[contributions]]
 id = "sample.view"
 kind = "view"
 title = "Sample"
 slot = "navigation"
 order = 10
 services = []
 permissions = []
"#
        }
        fn descriptor(&self) -> ModuleDescriptor {
            ModuleDescriptor {
                id: self.id,
                title: "Sample",
                description: "Test module",
            }
        }
        fn views(&self) -> Vec<ViewDescriptor> {
            vec![ViewDescriptor {
                id: self.view,
                title: "Sample",
                order: 10,
            }]
        }
        fn ui(&mut self, _: &str, _: &mut Ui) {}
        fn stop(&mut self) {
            self.stopped.set(self.stopped.get() + 1);
        }
    }
    fn sample(
        id: &'static str,
        view: &'static str,
        stopped: Rc<Cell<usize>>,
    ) -> Box<dyn DeckModule> {
        Box::new(Sample { id, view, stopped })
    }
    #[test]
    fn disable_removes_views_and_stops_resources() {
        let stopped = Rc::new(Cell::new(0));
        let mut catalog = ModuleCatalog::new(
            vec![sample("sample", "sample.view", stopped.clone())],
            &["sample".into()],
            stationdeck_services::registry::Policy::default(),
        );
        assert_eq!(catalog.views().len(), 1);
        catalog.set_enabled("sample", false).unwrap();
        assert!(catalog.views().is_empty());
        assert_eq!(stopped.get(), 1);
        catalog.set_enabled("sample", false).unwrap();
        assert_eq!(stopped.get(), 1);
        assert!(catalog.set_enabled("unknown", true).is_err());
    }
    #[test]
    fn duplicate_modules_and_foreign_view_ids_are_rejected() {
        let stopped = Rc::new(Cell::new(0));
        let duplicate = ModuleCatalog::new(
            vec![
                sample("sample", "sample.view", stopped.clone()),
                sample("sample", "sample.other", stopped.clone()),
            ],
            &["sample".into()],
            stationdeck_services::registry::Policy::default(),
        );
        assert!(duplicate.views().is_empty());
        assert!(matches!(
            duplicate.status("sample"),
            Some((stationdeck_services::registry::State::Failed, Some(_)))
        ));
        let foreign = ModuleCatalog::new(
            vec![sample("sample", "foreign.view", stopped)],
            &["sample".into()],
            stationdeck_services::registry::Policy::default(),
        );
        assert!(foreign.views().is_empty());
        assert!(matches!(
            foreign.status("sample"),
            Some((stationdeck_services::registry::State::Failed, Some(_)))
        ));
    }
}

pub mod test_ui;
