//! Explicit commands and ephemeral presentation state for the broadcast desk.
use super::*;
use stationdeck_services::icecast::{IcecastCommand as Command, IcecastResult as Reply};
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Action {
    Route,
    Switch,
    Listeners,
    Metadata,
    Disconnect,
}
impl IcecastPlugin {
    pub(super) fn control_state(&self) -> Result<RequestState<Reply>, String> {
        self.control
            .as_ref()
            .ok_or_else(|| "Management is not permitted by the local module policy.".to_owned())?
            .with(|s| s.snapshot())
    }
    pub(super) fn can_control(&self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|s| s.with(|s| s.available()).unwrap_or(false))
    }
    pub(super) fn submit(&mut self, command: Command) {
        if !self.can_control() {
            self.message =
                "Set the Icecast control token in Tokens, or check module permissions.".into();
            return;
        }
        let action = match &command {
            Command::Route => Action::Route,
            Command::Switch { .. } => Action::Switch,
            Command::Listeners { .. } => Action::Listeners,
            Command::Metadata { .. } => Action::Metadata,
            _ => Action::Disconnect,
        };
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
                self.feedback.clear();
                self.last_action = Some(action);
                self.handled_result = None;
                self.refresh_after_command = changes;
                if action != Action::Route {
                    self.listeners = None;
                }
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
                if self.handled_result.as_ref() == Some(&result) {
                    return;
                }
                match &result {
                    Reply::Route { route } => {
                        self.feedback = if self.last_action == Some(Action::Switch) {
                            format!("Input {} selected", route.input.to_uppercase())
                        } else {
                            "Route refreshed".into()
                        };
                        self.route = Some(route.clone());
                    }
                    Reply::Listeners { mount, listeners } => {
                        self.listeners = Some((mount.clone(), listeners.clone()));
                        self.show_listeners = true;
                        self.feedback = "Listener snapshot refreshed".into();
                    }
                    Reply::Done { message } => {
                        self.feedback = message.clone();
                        if self.last_action == Some(Action::Metadata) {
                            self.edit_metadata = false;
                        }
                    }
                }
                self.handled_result = Some(result);
                if self.refresh_after_command {
                    self.refresh_after_command = false;
                    let _ = self.refresh();
                }
            }
            Ok(RequestState::Pending) => {}
            Ok(RequestState::Failed(_)) => {
                // Route failures invalidate the diagram; unrelated listener or
                // metadata errors retain the last clearly labelled route snapshot.
                if matches!(self.last_action, Some(Action::Route | Action::Switch)) {
                    self.route = None;
                }
                self.feedback.clear();
                self.refresh_after_command = false;
            }
            _ => {
                self.route = None;
                self.listeners = None;
                self.confirmation = None;
                self.handled_result = None;
                self.feedback.clear();
                self.refresh_after_command = false;
            }
        }
    }
    pub(super) fn management_ui(&mut self, ui: &mut egui::Ui, status: &IcecastStatus) {
        let pending = matches!(self.control_state(), Ok(RequestState::Pending));
        let enabled = self.can_control() && !pending;
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.heading("Stream routing");
            ui.weak("Snapshot");
            if stationdeck_ui::test_ui::response(
                "icecast-get-route",
                ui.add_enabled(
                    enabled,
                    egui::Button::new(if self.route.is_some() {
                        "Refresh route"
                    } else {
                        "Load route"
                    }),
                ),
            )
            .clicked()
            {
                self.submit(Command::Route);
            }
        });
        if !self.can_control() {
            ui.weak("Enable commands with the Icecast control token in Tokens.");
        }
        ui.add_space(8.0);
        if let Some(route) = self.route.clone() {
            self.routing_desk(ui, status, &route, enabled);
        } else {
            egui::Frame::group(ui.style())
                .inner_margin(18.0)
                .show(ui, |ui| {
                    ui.set_width((ui.available_width() - 36.0).max(120.0));
                    ui.strong("Sources  >  Program output");
                    ui.weak(
                        "Load the route to see configured inputs and select the source on air.",
                    );
                });
        }
        ui.add_space(14.0);
        self.stream_details(ui, status, enabled);
        self.disconnect_confirmation(ui, enabled);
        ui.add_space(12.0);
        match self.control_state() {
            Ok(RequestState::Pending) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Operation in progress");
                });
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            Ok(RequestState::Failed(error)) => {
                show_state(ui, ViewState::Error(&error));
            }
            Err(error) => {
                ui.weak(error);
            }
            _ if !self.feedback.is_empty() => {
                ui.horizontal_wrapped(|ui| {
                    stationdeck_ui::status_dot(ui, stationdeck_ui::StatusTone::Good);
                    ui.label(&self.feedback);
                });
            }
            _ => {}
        }
    }
}
