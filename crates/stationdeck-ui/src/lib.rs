//! UI extension seam. Module registration and service injection arrive later.
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
    fn descriptor(&self) -> ModuleDescriptor;
    fn views(&self) -> Vec<ViewDescriptor>;
    fn ui(&mut self, id: &str, ui: &mut Ui);
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
/// Initial compiled-module catalog. Dependency/service resolution is a later layer.
#[derive(Default)]
pub struct ModuleCatalog {
    modules: Vec<(Box<dyn DeckModule>, bool)>,
}

impl ModuleCatalog {
    pub fn new(modules: Vec<Box<dyn DeckModule>>, enabled: &[String]) -> Result<Self, String> {
        let mut module_ids = std::collections::BTreeSet::new();
        let mut view_ids = std::collections::BTreeSet::new();
        for module in &modules {
            let descriptor = module.descriptor();
            let valid = |id: &str| {
                !id.is_empty()
                    && id.len() <= 64
                    && id.as_bytes()[0].is_ascii_lowercase()
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            };
            if !valid(descriptor.id) || !module_ids.insert(descriptor.id) {
                return Err("Invalid or duplicate compiled module identifier.".into());
            }
            let prefix = format!("{}.", descriptor.id);
            for view in module.views() {
                if !view.id.strip_prefix(&prefix).is_some_and(valid) || !view_ids.insert(view.id) {
                    return Err("Invalid or duplicate compiled view identifier.".into());
                }
            }
        }
        Ok(Self {
            modules: modules
                .into_iter()
                .map(|module| {
                    let active = enabled.iter().any(|id| id == module.descriptor().id);
                    (module, active)
                })
                .collect(),
        })
    }
    pub fn descriptors(&self) -> Vec<ModuleDescriptor> {
        self.modules
            .iter()
            .map(|(module, _)| module.descriptor())
            .collect()
    }
    pub fn is_enabled(&self, id: &str) -> bool {
        self.modules
            .iter()
            .any(|(module, enabled)| *enabled && module.descriptor().id == id)
    }
    pub fn enabled_ids(&self) -> Vec<String> {
        self.modules
            .iter()
            .filter(|(_, enabled)| *enabled)
            .map(|(module, _)| module.descriptor().id.to_owned())
            .collect()
    }
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let Some((module, active)) = self
            .modules
            .iter_mut()
            .find(|(module, _)| module.descriptor().id == id)
        else {
            return Err("Unknown compiled module.".into());
        };
        if *active && !enabled {
            module.stop();
        }
        *active = enabled;
        Ok(())
    }
    pub fn views(&self) -> Vec<ViewDescriptor> {
        let mut views: Vec<_> = self
            .modules
            .iter()
            .filter(|(_, enabled)| *enabled)
            .flat_map(|(module, _)| module.views())
            .collect();
        views.sort_by_key(|view| {
            (
                view.order,
                view.id.split('.').next().unwrap_or_default(),
                view.id,
            )
        });
        views
    }
    pub fn render(&mut self, id: &str, ui: &mut Ui) -> bool {
        for (module, enabled) in &mut self.modules {
            if *enabled && module.views().iter().any(|view| view.id == id) {
                ui.push_id(id, |ui| module.ui(id, ui));
                return true;
            }
        }
        false
    }
}

impl Drop for ModuleCatalog {
    fn drop(&mut self) {
        for (module, _) in &mut self.modules {
            module.stop();
        }
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
        )
        .unwrap();
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
        assert!(ModuleCatalog::new(
            vec![
                sample("sample", "sample.view", stopped.clone()),
                sample("sample", "sample.other", stopped.clone())
            ],
            &[]
        )
        .is_err());
        assert!(ModuleCatalog::new(vec![sample("sample", "foreign.view", stopped)], &[]).is_err());
    }
}
