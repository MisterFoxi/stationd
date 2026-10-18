//! Autonomous Deck plugin: consumes only the injected stream-status contract.
//! No endpoint, network client, protobuf, upstream credential or host dependency.
use stationdeck_services::{icecast::{IcecastService, IcecastStatus, ICECAST}, registry::{Context, ServiceHandle}, RequestState};
use stationdeck_ui::{show_state, DeckModule, ModuleDescriptor, ViewDescriptor, ViewState};
#[derive(Default)]
pub struct IcecastPlugin {
    service: Option<ServiceHandle<dyn IcecastService>>,
    message: String,
}
impl IcecastPlugin {
    fn snapshot(&self) -> Result<RequestState<IcecastStatus>, String> {
        self.service.as_ref().ok_or_else(|| "Icecast provider is not available.".to_owned())?.with(|s| s.snapshot())
    }
    fn refresh(&self) -> Result<(), String> {
        self.service.as_ref().ok_or_else(|| "Icecast provider is not available.".to_owned())?.with(|s| s.refresh())?
    }
}
impl DeckModule for IcecastPlugin {
    fn manifest(&self) -> &str { include_str!("../module.toml") }
    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor { id: "icecastd", title: "Icecast streams plugin", description: "Read stream presence, audience and metadata through the injected Icecast service." }
    }
    fn views(&self) -> Vec<ViewDescriptor> {
        vec![ViewDescriptor { id: "icecastd.streams", title: "Icecast streams", order: 25 }]
    }
    fn start(&mut self, context: &Context) -> Result<(), String> {
        self.service = Some(context.service(&ICECAST, "icecastd.streams", "icecastd.consulter", "read")?);
        Ok(())
    }
    fn stop(&mut self) { self.service = None; self.message.clear(); }
    fn ui(&mut self, _: &str, ui: &mut egui::Ui) {
        ui.heading("Icecast streams");
        if stationdeck_ui::test_ui::response("icecast-streams-refresh", ui.button("Refresh streams")).clicked() {
            self.message = self.refresh().err().unwrap_or_default();
        }
        if !self.message.is_empty() { show_state(ui, ViewState::Error(&self.message)); }
        match self.snapshot() {
            Err(error) => show_state(ui, ViewState::Unavailable(&error)),
            Ok(RequestState::Idle) => show_state(ui, ViewState::Unavailable("Connect Icecast provider in Icecast connection, then refresh.")),
            Ok(RequestState::Pending) => { show_state(ui, ViewState::Loading("Reading current stream status.")); ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)); },
            Ok(RequestState::Failed(error)) => show_state(ui, ViewState::Unavailable(&error)),
            Ok(RequestState::Ready(status)) => {
                ui.label(format!("Server: {}", status.server_id));
                if status.streams.is_empty() { show_state(ui, ViewState::Empty("No stream mounts are configured.")); }
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    egui::Grid::new("icecast.streams").striped(true).show(ui, |ui| {
                        for title in ["Mount", "Source", "Listeners", "Title", "Format"] { ui.strong(title); }
                        ui.end_row();
                        for (index, stream) in status.streams.into_iter().enumerate() {
                            stationdeck_ui::test_ui::value(&format!("icecast-stream-{index}-mount"), &stream.mount);
                            stationdeck_ui::test_ui::value(&format!("icecast-stream-{index}-connected"), if stream.connected { "true" } else { "false" });
                            stationdeck_ui::test_ui::value(&format!("icecast-stream-{index}-listeners"), &stream.listeners.to_string());
                            ui.label(stream.mount);
                            ui.label(if stream.connected { "Connected" } else { "Disconnected" });
                            ui.label(if stream.connected { stream.listeners.to_string() } else { "—".into() });
                            ui.label(stream.title);
                            ui.label(stream.content_type);
                            ui.end_row();
                        }
                    });
                });
                ui.weak("Status snapshot. Refresh for current information. An unavailable service is not a disconnected stream.");
            }
        }
    }
}
