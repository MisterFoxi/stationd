//! One consumer for both offline and gateway providers; no concrete provider imports.
use stationdeck_services::{
    overview::{OverviewService, OVERVIEW},
    registry::{Context, ServiceHandle},
    RequestState,
};
use stationdeck_ui::{
    show_state, show_station_status, DeckModule, ModuleDescriptor, ViewDescriptor, ViewState,
};
#[derive(Default)]
pub struct OverviewModule {
    service: Option<ServiceHandle<dyn OverviewService>>,
}
impl DeckModule for OverviewModule {
    fn manifest(&self) -> &str {
        include_str!("../../../Doc/stationdeck/overview.module.toml")
    }
    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: "overview",
            title: "Station overview",
            description: "Station status from the provider selected by deployment policy.",
        }
    }
    fn views(&self) -> Vec<ViewDescriptor> {
        vec![ViewDescriptor {
            id: "overview.station",
            title: "Station overview",
            order: 40,
        }]
    }
    fn start(&mut self, context: &Context) -> Result<(), String> {
        self.service =
            Some(context.service(&OVERVIEW, "overview.status", "overview.consulter", "read")?);
        Ok(())
    }
    fn summary(&self, ui: &mut egui::Ui) {
        match self.service.as_ref().map(|s| s.with(|s| s.snapshot())) {
            Some(Ok(RequestState::Ready(status))) => {
                ui.colored_label(
                    egui::Color32::from_rgb(57, 189, 176),
                    format!("● {}", status.station_name),
                );
                ui.weak("Status snapshot");
            }
            Some(Ok(RequestState::Pending)) => {
                ui.spinner();
                ui.label("Reading station…");
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            _ => {
                ui.weak("Station status: unavailable");
            }
        }
        ui.separator();
    }
    fn stop(&mut self) {
        self.service = None;
    }
    fn ui(&mut self, _: &str, ui: &mut egui::Ui) {
        ui.heading("Station overview");
        let Some(service) = &self.service else {
            return;
        };
        match service.with(|s| s.snapshot()) {
            Ok(RequestState::Ready(status)) => show_station_status(ui, &status),
            Ok(RequestState::Pending) => {
                show_state(ui, ViewState::Loading("Waiting for the selected provider."));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            Ok(RequestState::Failed(error)) => show_state(ui, ViewState::Error(&error)),
            Ok(RequestState::Idle) => show_state(
                ui,
                ViewState::Unavailable(
                    "The provider is running. Establish its connection to obtain station data.",
                ),
            ),
            Err(error) => show_state(ui, ViewState::Unavailable(&error)),
        }
    }
}
