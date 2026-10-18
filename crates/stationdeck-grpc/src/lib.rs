//! Browser-side status provider. Protobuf and tonic remain in the gateway.
use stationdeck_services::{RequestSlot, RequestState, StationStatus};
use stationdeck_ui::{
    show_state, show_station_status, DeckModule, ModuleDescriptor, ViewDescriptor, ViewState,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
pub struct GrpcModule {
    token: String,
    request: Rc<RefCell<RequestSlot<StationStatus>>>,
    #[cfg(target_arch = "wasm32")]
    abort: Option<web_sys::AbortController>,
}

impl GrpcModule {
    #[cfg(target_arch = "wasm32")]
    fn connect(&mut self, ctx: egui::Context) {
        use gloo_net::http::Request;
        use stationdeck_services::StatusResponse;
        let Some(generation) = self.request.borrow_mut().begin() else {
            return;
        };
        let controller = match web_sys::AbortController::new() {
            Ok(value) => value,
            Err(_) => {
                self.request.borrow_mut().finish(
                    generation,
                    Err("This browser cannot start the request.".into()),
                );
                return;
            }
        };
        let signal = controller.signal();
        let timeout_controller = controller.clone();
        self.abort = Some(controller);
        let token = self.token.clone();
        let slot = self.request.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _timeout =
                gloo_timers::callback::Timeout::new(6000, move || timeout_controller.abort());
            let result = async {
                let response = Request::get("/api/station/status")
                    .header("Authorization", &format!("Bearer {token}"))
                    .abort_signal(Some(&signal))
                    .send()
                    .await
                    .map_err(|_| {
                        "The gateway request was cancelled, timed out, or could not be reached."
                            .to_string()
                    })?;
                match response
                    .json::<StatusResponse>()
                    .await
                    .map_err(|_| "The gateway returned an invalid response.".to_string())?
                {
                    StatusResponse::Connected { status } => Ok(status),
                    StatusResponse::Unavailable { message, .. } => Err(message),
                }
            }
            .await;
            if slot.borrow_mut().finish(generation, result) {
                ctx.request_repaint();
            }
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn connect(&mut self, _ctx: egui::Context) {
        let generation = self.request.borrow_mut().begin();
        if let Some(generation) = generation {
            self.request.borrow_mut().finish(
                generation,
                Err("The web transport is available in the browser build.".into()),
            );
        }
    }
}

impl DeckModule for GrpcModule {
    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: "grpc",
            title: "gRPC connection",
            description: "Read StationD status through the authenticated web gateway.",
        }
    }
    fn views(&self) -> Vec<ViewDescriptor> {
        vec![ViewDescriptor {
            id: "grpc.connection",
            title: "Station connection",
            order: 20,
        }]
    }
    fn stop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(controller) = self.abort.take() {
            controller.abort();
        }
        self.request.borrow_mut().cancel();
        self.token.clear();
    }
    fn ui(&mut self, _id: &str, ui: &mut egui::Ui) {
        ui.heading("Station connection");
        ui.label("Read the status of the station configured by your administrator.");
        ui.add_space(12.0);
        ui.label("Gateway access token");
        let pending = matches!(self.request.borrow().state, RequestState::Pending);
        ui.add_enabled(
            !pending,
            egui::TextEdit::singleline(&mut self.token)
                .password(true)
                .hint_text("Access token")
                .desired_width(320.0),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !pending && !self.token.is_empty(),
                    egui::Button::new("Connect / refresh"),
                )
                .clicked()
            {
                self.connect(ui.ctx().clone());
            }
            if ui.button("Disconnect").clicked() {
                self.stop();
            }
        });
        match &self.request.borrow().state {
            RequestState::Idle => show_state(
                ui,
                ViewState::Unavailable(
                    "Not connected. Enter the access token provided by your administrator.",
                ),
            ),
            RequestState::Pending => show_state(
                ui,
                ViewState::Loading("Checking the station. Navigation remains available."),
            ),
            RequestState::Ready(status) => {
                ui.strong("Connected through the gateway");
                show_station_status(ui, status);
                ui.weak("Status snapshot. Refresh to obtain current information.");
            }
            RequestState::Failed(error) => show_state(ui, ViewState::Error(error)),
        }
        ui.add_space(16.0);
        ui.weak("The token stays in memory for this enabled module and is cleared on disconnect or disable.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_clears_credentials_and_invalidates_the_pending_response() {
        let mut module = GrpcModule {
            token: "fixture-only-credential".into(),
            ..Default::default()
        };
        let generation = module.request.borrow_mut().begin().unwrap();
        module.stop();
        assert!(module.token.is_empty());
        assert!(matches!(module.request.borrow().state, RequestState::Idle));
        assert!(!module
            .request
            .borrow_mut()
            .finish(generation, Err("late response".into())));
    }
}
