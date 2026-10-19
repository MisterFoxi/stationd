//! Operator controls supplied by a separately authorized typed service.
use super::*;
use stationdeck_services::icecast::{IcecastCommand as Command, IcecastResult as Reply};
impl IcecastPlugin {
    fn control_state(&self) -> Result<RequestState<Reply>, String> {
        self.control
            .as_ref()
            .ok_or_else(|| "Management is not permitted by the local module policy.".to_owned())?
            .with(|s| s.snapshot())
    }
    fn can_control(&self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|s| s.with(|s| s.available()).unwrap_or(false))
    }
    fn submit(&mut self, command: Command) {
        if !self.can_control() {
            self.message =
                "Set the Icecast control token in Tokens, or check module permissions.".into();
            return;
        }
        let changes = command.changes_stream();
        let result = self
            .control
            .as_ref()
            .unwrap()
            .with(|s| s.submit(command))
            .and_then(|r| r);
        match result {
            Ok(()) => {
                self.message.clear();
                self.refresh_after_command = changes;
                self.listeners = None;
            }
            Err(e) => {
                self.message = e;
                self.refresh_after_command = false;
            }
        }
    }
    pub(super) fn control_tick(&mut self) {
        if !self.can_control() {
            self.confirmation = None;
        }
        match self.control_state() {
            Ok(RequestState::Ready(result)) => {
                match result {
                    Reply::Route { route } => {
                        if self.route.as_ref() != Some(&route) {
                            self.selected_input = route.input.clone();
                            self.route = Some(route);
                        }
                    }
                    Reply::Listeners { mount, listeners } => {
                        self.listeners = Some((mount, listeners))
                    }
                    Reply::Done { .. } => {}
                }
                if self.refresh_after_command {
                    self.refresh_after_command = false;
                    let _ = self.refresh();
                }
            }
            Ok(RequestState::Pending) => {}
            _ => {
                self.route = None;
                self.listeners = None;
                self.refresh_after_command = false;
            }
        }
    }
    pub(super) fn management_ui(&mut self, ui: &mut egui::Ui, status: &IcecastStatus) {
        let state = self.control_state();
        let pending = matches!(state, Ok(RequestState::Pending));
        if !self.can_control() {
            ui.weak("Management requires the separate Icecast control token in Tokens and the module command permission.");
        }
        match &state {
            Ok(RequestState::Pending) => {
                ui.spinner();
                ui.label("Operation in progress…");
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            Ok(RequestState::Failed(error)) => show_state(ui, ViewState::Error(error)),
            Ok(RequestState::Ready(Reply::Done { message })) => {
                ui.label(message);
            }
            Err(error) => {
                ui.weak(error);
            }
            _ => {}
        }
        if !status
            .streams
            .iter()
            .any(|s| s.mount == self.selected_mount)
        {
            self.selected_mount = status
                .streams
                .first()
                .map(|s| s.mount.clone())
                .unwrap_or_default();
            self.title = status
                .streams
                .first()
                .map(|s| s.title.clone())
                .unwrap_or_default();
        }
        let previous = self.selected_mount.clone();
        ui.add_enabled_ui(!pending, |ui| {
            egui::ComboBox::from_id_salt("management.mount")
                .selected_text(&self.selected_mount)
                .show_ui(ui, |ui| {
                    for stream in &status.streams {
                        ui.selectable_value(
                            &mut self.selected_mount,
                            stream.mount.clone(),
                            &stream.mount,
                        );
                    }
                });
        });
        if previous != self.selected_mount {
            self.listeners = None;
            self.confirmation = None;
            self.title = status
                .streams
                .iter()
                .find(|s| s.mount == self.selected_mount)
                .map(|s| s.title.clone())
                .unwrap_or_default();
        }
        let connected = status
            .streams
            .iter()
            .any(|s| s.mount == self.selected_mount && s.connected);
        let enabled = self.can_control() && !pending;
        ui.add_enabled_ui(enabled && connected, |ui| {
            ui.label("Current title / metadata");
            stationdeck_ui::test_ui::response(
                "icecast-title",
                ui.add(
                    egui::TextEdit::singleline(&mut self.title)
                        .desired_width(350.0)
                        .char_limit(1024),
                ),
            );
            ui.horizontal_wrapped(|ui| {
                if stationdeck_ui::test_ui::response("icecast-set-title", ui.button("Update title"))
                    .clicked()
                {
                    self.submit(Command::Metadata {
                        mount: self.selected_mount.clone(),
                        title: self.title.clone(),
                    });
                }
                if stationdeck_ui::test_ui::response(
                    "icecast-listeners",
                    ui.button("List listeners"),
                )
                .clicked()
                {
                    self.submit(Command::Listeners {
                        mount: self.selected_mount.clone(),
                    });
                }
                if stationdeck_ui::test_ui::response(
                    "icecast-kick-source",
                    ui.button("Disconnect source…"),
                )
                .clicked()
                {
                    self.confirmation = Some(Command::KickSource {
                        mount: self.selected_mount.clone(),
                    });
                }
            });
        });
        if let Some((mount, listeners)) = self.listeners.clone() {
            ui.strong(format!("Listeners on {mount} · snapshot"));
            if listeners.is_empty() {
                ui.weak("No listeners in this snapshot.");
            }
            egui::ScrollArea::vertical()
                .id_salt("management.listeners")
                .max_height(190.0)
                .show(ui, |ui| {
                    egui::Grid::new("management.listener.grid")
                        .striped(true)
                        .show(ui, |ui| {
                            for heading in ["ID", "Address", "Connected", "Client", "Action"] {
                                ui.strong(heading);
                            }
                            ui.end_row();
                            for listener in listeners {
                                ui.label(listener.id.to_string());
                                ui.label(listener.ip);
                                ui.label(format!("{} s", listener.connected_seconds));
                                ui.label(listener.user_agent);
                                if stationdeck_ui::test_ui::response(
                                    &format!("icecast-kick-listener-{}", listener.id),
                                    ui.add_enabled(enabled, egui::Button::new("Disconnect…")),
                                )
                                .clicked()
                                {
                                    self.confirmation = Some(Command::KickListener {
                                        mount: mount.clone(),
                                        id: listener.id,
                                    });
                                }
                                ui.end_row();
                            }
                        });
                });
        }
        if let Some(command) = self.confirmation.clone() {
            let target = match &command {
                Command::KickSource { mount } => {
                    format!("Disconnect source {mount}? Its listeners may be interrupted.")
                }
                Command::KickListener { mount, id } => {
                    format!("Disconnect listener {id} from {mount}?")
                }
                _ => String::new(),
            };
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(target);
                ui.horizontal(|ui| {
                    if stationdeck_ui::test_ui::response(
                        "icecast-confirm",
                        ui.add_enabled(enabled, egui::Button::new("Confirm disconnect")),
                    )
                    .clicked()
                    {
                        self.confirmation = None;
                        self.submit(command);
                    }
                    if stationdeck_ui::test_ui::response("icecast-cancel", ui.button("Cancel"))
                        .clicked()
                    {
                        self.confirmation = None;
                    }
                });
            });
        }
        ui.separator();
        ui.strong("Stream router");
        if stationdeck_ui::test_ui::response(
            "icecast-get-route",
            ui.add_enabled(enabled, egui::Button::new("Load route")),
        )
        .clicked()
        {
            self.submit(Command::Route);
        }
        if let Some(route) = self.route.clone() {
            ui.label(format!(
                "{} ← {} · {}",
                route.output_mount,
                route.input,
                if route.connected {
                    "Connected"
                } else {
                    "Disconnected"
                }
            ));
            if !route.problem.is_empty() {
                ui.label(&route.problem);
            }
            ui.add_enabled_ui(enabled, |ui| {
                let input_combo = egui::ComboBox::from_id_salt("management.route.input")
                    .selected_text(&self.selected_input)
                    .show_ui(ui, |ui| {
                        for input in &route.inputs {
                            stationdeck_ui::test_ui::response(
                                &format!("icecast-input-{input}"),
                                ui.selectable_value(&mut self.selected_input, input.clone(), input),
                            );
                        }
                    });
                stationdeck_ui::test_ui::control("icecast-route-input", &input_combo.response);
                if stationdeck_ui::test_ui::response(
                    "icecast-switch",
                    ui.add_enabled(
                        route.inputs.contains(&self.selected_input)
                            && self.selected_input != route.input,
                        egui::Button::new("Switch input"),
                    ),
                )
                .clicked()
                {
                    self.submit(Command::Switch {
                        input: self.selected_input.clone(),
                    });
                }
            });
        } else {
            ui.weak(
                "Available only when the administration service has a stream router configured.",
            );
        }
        ui.weak("Operations are sent once. A timeout may leave the outcome unknown: refresh before retrying.");
    }
}
