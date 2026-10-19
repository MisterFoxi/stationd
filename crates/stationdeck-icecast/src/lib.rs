//! Transport provider. The Icecast Deck plugin does not depend on this crate.
use stationdeck_services::{
    icecast::{IcecastService, IcecastStatus, ICECAST},
    registry::Context,
    RequestSlot, RequestState,
};
use stationdeck_ui::{show_state, DeckModule, ModuleDescriptor, ViewDescriptor, ViewState};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Client {
    token: stationdeck_ui::TokenSlot,
    slot: Rc<RefCell<RequestSlot<IcecastStatus>>>,
    #[cfg(target_arch = "wasm32")]
    abort: RefCell<Option<web_sys::AbortController>>,
}
impl Client {
    fn disconnect(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(controller) = self.abort.borrow_mut().take() {
            controller.abort();
        }
        self.slot.borrow_mut().cancel();
    }
}
impl IcecastService for Client {
    fn snapshot(&self) -> RequestState<IcecastStatus> {
        self.slot.borrow().state.clone()
    }
    #[cfg(target_arch = "wasm32")]
    fn refresh(&self) -> Result<(), String> {
        use gloo_net::http::Request;
        use stationdeck_services::icecast::IcecastResponse;
        let token = self.token.get();
        if token.is_empty() {
            return Err("Connect the Icecast provider with your Deck gateway token first.".into());
        }
        // Create the controller before entering Pending: controller failure must not wedge the slot.
        let controller = web_sys::AbortController::new()
            .map_err(|_| "This browser cannot start the request.")?;
        let Some(generation) = self.slot.borrow_mut().begin() else {
            return Err("A refresh is already in progress.".into());
        };
        let signal = controller.signal();
        let deadline = controller.clone();
        *self.abort.borrow_mut() = Some(controller);
        let slot = self.slot.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _timeout = gloo_timers::callback::Timeout::new(10000, move || deadline.abort());
            let result = async {
                let response = Request::get("/api/icecast/streams")
                    .header("Authorization", &format!("Bearer {token}"))
                    .abort_signal(Some(&signal))
                    .send()
                    .await
                    .map_err(|_| {
                        "Icecast status was cancelled, timed out, or could not be reached."
                            .to_owned()
                    })?;
                match response
                    .json::<IcecastResponse>()
                    .await
                    .map_err(|_| "Invalid Icecast gateway response.".to_owned())?
                {
                    IcecastResponse::Connected { status } => Ok(status),
                    IcecastResponse::Unavailable { message, .. } => Err(message),
                }
            }
            .await;
            slot.borrow_mut().finish(generation, result);
        });
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh(&self) -> Result<(), String> {
        Err("Icecast web transport requires the browser build.".into())
    }
}

#[derive(Default)]
pub struct IcecastProvider {
    client: Rc<Client>,
    context: Option<Context>,
    message: String,
    token_revision: u64,
}
impl IcecastProvider {
    pub fn with_token(token: stationdeck_ui::TokenSlot) -> Self {
        Self {
            client: Rc::new(Client {
                token,
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}
impl DeckModule for IcecastProvider {
    fn manifest(&self) -> &str {
        include_str!("../module.toml")
    }
    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor { id: "icecast-gateway", title: "Icecast provider", description: "Connect the stream status service to the Deck gateway. No StationD connection required." }
    }
    fn views(&self) -> Vec<ViewDescriptor> {
        vec![ViewDescriptor {
            id: "icecast-gateway.connection",
            title: "Icecast connection",
            order: 24,
        }]
    }
    fn start(&mut self, context: &Context) -> Result<(), String> {
        self.context = Some(context.clone());
        context.publish(&ICECAST, self.client.clone() as Rc<dyn IcecastService>)
    }
    fn health(&self) -> stationdeck_ui::PluginStatus {
        stationdeck_ui::PluginStatus::request(&self.client.slot.borrow().state)
    }
    fn tick(&mut self) {
        let revision = self.client.token.revision();
        if revision != self.token_revision {
            self.stop();
            self.token_revision = revision;
        }
    }
    fn stop(&mut self) {
        self.client.disconnect();
        self.message.clear();
    }
    fn ui(&mut self, _: &str, ui: &mut egui::Ui) {
        ui.heading("Icecast connection");
        ui.label("The Deck gateway connects to the Icecast administration service. No StationD endpoint is used.");
        ui.weak("Manage the shared Deck gateway token in Tokens.");
        let pending = matches!(self.client.slot.borrow().state, RequestState::Pending);
        let authorized = self.context.as_ref().is_some_and(|c| {
            c.authorize(
                "icecast-gateway.connection",
                "icecast-gateway.connecter",
                "connect",
            )
            .is_ok()
        });
        if stationdeck_ui::test_ui::response(
            "icecast-connect",
            ui.add_enabled(
                authorized && !pending && !self.client.token.is_empty(),
                egui::Button::new("Connect / refresh"),
            ),
        )
        .clicked()
        {
            if let Err(error) = self.client.refresh() {
                self.message = error;
            }
        }
        if ui.button("Disconnect Icecast").clicked() {
            self.client.disconnect();
            self.message.clear();
        }
        if !self.message.is_empty() {
            show_state(ui, ViewState::Error(&self.message));
        }
        match self.client.snapshot() {
            RequestState::Idle => show_state(
                ui,
                ViewState::Unavailable("Connect this provider, then open Icecast streams."),
            ),
            RequestState::Pending => {
                show_state(ui, ViewState::Loading("Reading Icecast stream status."));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            RequestState::Ready(_) => {
                ui.label("Connected. Open Icecast streams to inspect the snapshot.");
            }
            RequestState::Failed(error) => show_state(ui, ViewState::Error(&error)),
        }
        ui.weak("The saved Deck token is managed in Tokens. Icecast administration credentials stay on the server.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_cancels_late_stream_snapshots_and_keeps_saved_credentials() {
        let mut provider = IcecastProvider::default();
        provider.client.token.set("fixture-only-token");
        let old = provider.client.slot.borrow_mut().begin().unwrap();
        provider.stop();
        assert_eq!(provider.client.token.get(), "fixture-only-token");
        assert!(matches!(provider.client.snapshot(), RequestState::Idle));
        assert!(!provider.client.slot.borrow_mut().finish(
            old,
            Ok(IcecastStatus {
                server_id: "late".into(),
                streams: vec![]
            })
        ));
        stationdeck_services::manifest::Manifest::parse(provider.manifest()).unwrap();
    }
}
