//! Autonomous Deck plugin: consumes injected stream-status and optional management contracts.
//! No endpoint, network client, protobuf, upstream credential or host dependency.
mod desk;
mod management;
mod migrations;
use stationdeck_services::{
    icecast::{IcecastService, IcecastStatus, ICECAST},
    registry::{Context, ServiceHandle},
    RequestState,
};
use stationdeck_ui::{show_state, DeckModule, ModuleDescriptor, ViewDescriptor, ViewState};
#[derive(Default)]
pub struct IcecastPlugin {
    service: Option<ServiceHandle<dyn IcecastService>>,
    message: String,
    control: Option<ServiceHandle<dyn stationdeck_services::icecast::IcecastControlService>>,
    management: bool,
    migration_source: String,
    migration_destination: String,
    migrations: Vec<stationdeck_services::icecast::ListenerMigration>,
    migration_confirm: Option<stationdeck_services::icecast::IcecastCommand>,
    migration_poll: f64,
    selected_mount: String,
    title: String,
    edit_metadata: bool,
    show_listeners: bool,
    last_action: Option<management::Action>,
    handled_result: Option<stationdeck_services::icecast::IcecastResult>,
    feedback: String,
    route: Option<stationdeck_services::icecast::RouteStatus>,
    listeners: Option<(String, Vec<stationdeck_services::icecast::Listener>)>,
    confirmation: Option<stationdeck_services::icecast::IcecastCommand>,
    refresh_after_command: bool,
}
impl IcecastPlugin {
    fn snapshot(&self) -> Result<RequestState<IcecastStatus>, String> {
        self.service
            .as_ref()
            .ok_or_else(|| "Icecast provider is not available.".to_owned())?
            .with(|s| s.snapshot())
    }
    fn refresh(&self) -> Result<(), String> {
        self.service
            .as_ref()
            .ok_or_else(|| "Icecast provider is not available.".to_owned())?
            .with(|s| s.refresh())?
    }
}
impl DeckModule for IcecastPlugin {
    fn manifest(&self) -> &str {
        include_str!("../module.toml")
    }
    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: "icecastd",
            title: "Icecast",
            description:
                "Read stream presence, audience and metadata through the injected Icecast service.",
        }
    }
    fn views(&self) -> Vec<ViewDescriptor> {
        vec![ViewDescriptor {
            id: "icecastd.streams",
            title: "Icecast streams",
            order: 25,
        }]
    }
    fn start(&mut self, context: &Context) -> Result<(), String> {
        self.service =
            Some(context.service(&ICECAST, "icecastd.streams", "icecastd.consulter", "read")?);
        self.control = context
            .service(
                &stationdeck_services::icecast::ICECAST_CONTROL,
                "icecastd.control",
                "icecastd.commander",
                "command",
            )
            .ok();
        Ok(())
    }
    fn health(&self) -> stationdeck_ui::PluginStatus {
        use stationdeck_ui::{PluginStatus, StatusTone};
        match self.snapshot() {
            Ok(RequestState::Ready(status)) if status.streams.is_empty() => {
                PluginStatus::new(StatusTone::Warning, "No mounts configured")
            }
            Ok(RequestState::Ready(status)) if status.streams.iter().any(|s| !s.connected) => {
                PluginStatus::new(StatusTone::Warning, "Source missing")
            }
            Ok(state) => PluginStatus::request(&state),
            Err(_) => PluginStatus::new(StatusTone::Error, "Service unavailable"),
        }
    }
    fn summary(&self, ui: &mut egui::Ui) {
        use stationdeck_ui::{vital, StatusTone};
        match self.snapshot() {
            Ok(RequestState::Ready(status)) => {
                let online = status.streams.iter().filter(|s| s.connected).count();
                let listeners: u64 = status
                    .streams
                    .iter()
                    .filter(|s| s.connected)
                    .map(|s| s.listeners)
                    .sum();
                let missing = status.streams.len() - online;
                vital(ui, "Icecast · snapshot", "Available", StatusTone::Good);
                vital(ui, "Listeners", &listeners.to_string(), StatusTone::Good);
                vital(
                    ui,
                    "Sources online",
                    &format!("{online} / {}", status.streams.len()),
                    self.health().tone,
                );
                vital(
                    ui,
                    "Mounts without source",
                    &missing.to_string(),
                    if missing == 0 {
                        StatusTone::Good
                    } else {
                        StatusTone::Warning
                    },
                );
                stationdeck_ui::test_ui::value("vital-listeners", &listeners.to_string());
                stationdeck_ui::test_ui::value("vital-missing-sources", &missing.to_string());
            }
            _ => {
                let health = self.health();
                vital(ui, "Icecast", health.label, health.tone);
                if health.label == "Reading" {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
    }
    fn tick(&mut self) {
        self.control_tick();
    }
    fn stop(&mut self) {
        self.control = None;
        self.route = None;
        self.listeners = None;
        self.confirmation = None;
        self.refresh_after_command = false;
        self.title.clear();
        self.feedback.clear();
        self.last_action = None;
        self.handled_result = None;
        self.edit_metadata = false;
        self.show_listeners = false;
        self.migrations.clear();
        self.migration_confirm = None;
        self.service = None;
        self.message.clear();
    }
    fn ui(&mut self, _: &str, ui: &mut egui::Ui) {
        if !self.management {
            ui.heading("Icecast streams");
        }
        ui.horizontal_wrapped(|ui| {
            if stationdeck_ui::test_ui::response(
                "icecast-tab-streams",
                ui.selectable_label(!self.management, "Streams"),
            )
            .clicked()
            {
                self.management = false;
            }
            if stationdeck_ui::test_ui::response(
                "icecast-tab-management",
                ui.selectable_label(self.management, "Routing"),
            )
            .clicked()
            {
                self.management = true;
                self.migration_poll = 0.0;
                if self.can_control() && !matches!(self.control_state(), Ok(RequestState::Pending))
                {
                    self.submit(stationdeck_services::icecast::IcecastCommand::Migrations);
                }
            }
            if stationdeck_ui::test_ui::response(
                "icecast-streams-refresh",
                ui.button("Refresh streams"),
            )
            .clicked()
            {
                self.message = self.refresh().err().unwrap_or_default();
            }
        });
        if !self.message.is_empty() {
            show_state(ui, ViewState::Error(&self.message));
        }
        match self.snapshot() {
            Err(error) => show_state(ui, ViewState::Unavailable(&error)),
            Ok(RequestState::Idle) => show_state(
                ui,
                ViewState::Unavailable(
                    "Connect Icecast provider in Icecast connection, then refresh.",
                ),
            ),
            Ok(RequestState::Pending) => {
                show_state(ui, ViewState::Loading("Reading current stream status."));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            Ok(RequestState::Failed(error)) => show_state(ui, ViewState::Unavailable(&error)),
            Ok(RequestState::Ready(status)) => {
                if self.management {
                    self.management_ui(ui, &status);
                    return;
                }
                ui.label(format!("Server: {}", status.server_id));
                if status.streams.is_empty() {
                    show_state(ui, ViewState::Empty("No stream mounts are configured."));
                }
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    egui::Grid::new("icecast.streams")
                        .striped(true)
                        .show(ui, |ui| {
                            for title in ["Mount", "Source", "Listeners", "Title", "Format"] {
                                ui.strong(title);
                            }
                            ui.end_row();
                            for (index, stream) in status.streams.into_iter().enumerate() {
                                stationdeck_ui::test_ui::value(
                                    &format!("icecast-stream-{index}-mount"),
                                    &stream.mount,
                                );
                                stationdeck_ui::test_ui::value(
                                    &format!("icecast-stream-{index}-connected"),
                                    if stream.connected { "true" } else { "false" },
                                );
                                stationdeck_ui::test_ui::value(
                                    &format!("icecast-stream-{index}-listeners"),
                                    &stream.listeners.to_string(),
                                );
                                ui.label(stream.mount);
                                stationdeck_ui::status_badge(
                                    ui,
                                    stationdeck_ui::PluginStatus::new(
                                        if stream.connected {
                                            stationdeck_ui::StatusTone::Good
                                        } else {
                                            stationdeck_ui::StatusTone::Warning
                                        },
                                        if stream.connected {
                                            "Online"
                                        } else {
                                            "No source"
                                        },
                                    ),
                                );
                                ui.label(if stream.connected {
                                    stream.listeners.to_string()
                                } else {
                                    "—".into()
                                });
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
