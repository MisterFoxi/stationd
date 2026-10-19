//! Browser-side status provider. Protobuf and tonic remain in the gateway.
use stationdeck_services::{RequestSlot, RequestState, StationStatus};
use stationdeck_ui::{
    show_state, show_station_status, DeckModule, ModuleDescriptor, ViewDescriptor, ViewState,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
pub struct GrpcModule {
    context: Option<stationdeck_services::registry::Context>,
    token: stationdeck_ui::TokenSlot,
    token_revision: u64,
    request: Rc<RefCell<RequestSlot<StationStatus>>>,
    #[cfg(target_arch = "wasm32")]
    abort: Option<web_sys::AbortController>,
}

impl GrpcModule {
    pub fn with_token(token: stationdeck_ui::TokenSlot) -> Self {
        Self {
            token,
            ..Default::default()
        }
    }
    fn can_connect(&self) -> bool {
        self.context.as_ref().is_some_and(|c| {
            c.authorize("grpc.connection", "grpc.connecter", "connect")
                .is_ok()
        })
    }
    #[cfg(target_arch = "wasm32")]
    fn connect(&mut self, ctx: egui::Context) {
        if !self.can_connect() {
            return;
        }
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
        let token = self.token.get();
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
        if !self.can_connect() {
            return;
        }
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
    fn manifest(&self) -> &str {
        include_str!("../../../Doc/stationdeck/grpc.module.toml")
    }
    fn start(&mut self, context: &stationdeck_services::registry::Context) -> Result<(), String> {
        self.context = Some(context.clone());
        context.publish(
            &stationdeck_services::overview::OVERVIEW,
            Rc::new(stationdeck_services::overview::SharedOverview(
                self.request.clone(),
            )) as Rc<dyn stationdeck_services::overview::OverviewService>,
        )
    }
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
    fn health(&self) -> stationdeck_ui::PluginStatus {
        stationdeck_ui::PluginStatus::request(&self.request.borrow().state)
    }
    fn tick(&mut self) {
        let revision = self.token.revision();
        if revision != self.token_revision {
            self.stop();
            self.token_revision = revision;
        }
    }
    fn stop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(controller) = self.abort.take() {
            controller.abort();
        }
        self.request.borrow_mut().cancel();
    }
    fn ui(&mut self, _id: &str, ui: &mut egui::Ui) {
        ui.heading("Station connection");
        ui.label("Read the status of the station configured by your administrator.");
        ui.add_space(12.0);
        ui.weak("Manage the saved Deck gateway token in Tokens.");
        let pending = matches!(self.request.borrow().state, RequestState::Pending);
        ui.horizontal(|ui| {
            if stationdeck_ui::test_ui::response(
                "grpc-refresh",
                ui.add_enabled(
                    !pending && !self.token.is_empty(),
                    egui::Button::new("Connect / refresh"),
                ),
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
                    "Not connected. Set the Deck gateway token in Tokens, then connect.",
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
        ui.weak(
            "Disconnect stops requests and clears station data. The saved token remains in Tokens.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_or_forgetting_a_saved_token_invalidates_late_replies() {
        let token = stationdeck_ui::TokenSlot::default();
        token.set("first-fixture");
        let mut module = GrpcModule::with_token(token.clone());
        module.tick();
        let generation = module.request.borrow_mut().begin().unwrap();
        token.set("replacement-fixture");
        module.tick();
        assert!(matches!(module.request.borrow().state, RequestState::Idle));
        assert!(!module
            .request
            .borrow_mut()
            .finish(generation, Err("late denial".into())));
        let generation = module.request.borrow_mut().begin().unwrap();
        token.set("");
        module.tick();
        assert!(!module
            .request
            .borrow_mut()
            .finish(generation, Err("late reply".into())));
        assert!(module.token.is_empty());
    }
    #[test]
    fn stop_cancels_work_and_preserves_the_saved_token() {
        let token = stationdeck_ui::TokenSlot::default();
        token.set("saved-fixture");
        let mut module = GrpcModule::with_token(token.clone());
        module.tick();
        let generation = module.request.borrow_mut().begin().unwrap();
        module.stop();
        assert_eq!(token.get(), "saved-fixture");
        assert!(matches!(module.request.borrow().state, RequestState::Idle));
        assert!(!module
            .request
            .borrow_mut()
            .finish(generation, Err("late reply".into())));
    }
}
