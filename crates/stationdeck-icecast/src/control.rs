//! Explicit Icecast operations; no automatic retry.
use stationdeck_services::{
    icecast::{IcecastCommand, IcecastControlService, IcecastResult},
    RequestSlot, RequestState,
};
use std::{cell::RefCell, rc::Rc};
#[derive(Default)]
pub(crate) struct ControlClient {
    pub token: stationdeck_ui::TokenSlot,
    slot: Rc<RefCell<RequestSlot<IcecastResult>>>,
    #[cfg(target_arch = "wasm32")]
    abort: RefCell<Option<web_sys::AbortController>>,
}
impl ControlClient {
    pub fn with_token(token: stationdeck_ui::TokenSlot) -> Self {
        Self {
            token,
            ..Default::default()
        }
    }
    pub fn disconnect(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(c) = self.abort.borrow_mut().take() {
            c.abort();
        }
        self.slot.borrow_mut().cancel();
    }
}
impl IcecastControlService for ControlClient {
    fn available(&self) -> bool {
        !self.token.is_empty()
    }
    fn snapshot(&self) -> RequestState<IcecastResult> {
        self.slot.borrow().state.clone()
    }
    #[cfg(target_arch = "wasm32")]
    fn submit(&self, command: IcecastCommand) -> Result<(), String> {
        use gloo_net::http::Request;
        use stationdeck_services::icecast::ControlResponse;
        command.validate()?;
        if self.token.is_empty() {
            return Err("Set the Icecast control token in Tokens.".into());
        }
        let body =
            serde_json::to_string(&command).map_err(|_| "Cannot encode Icecast operation.")?;
        let controller =
            web_sys::AbortController::new().map_err(|_| "Cannot start the operation.")?;
        let Some(generation) = self.slot.borrow_mut().begin() else {
            return Err("An Icecast operation is already pending.".into());
        };
        let action = match &command {
            IcecastCommand::Route => "route",
            IcecastCommand::Listeners { .. } => "listeners",
            IcecastCommand::Metadata { .. } => "metadata",
            IcecastCommand::KickSource { .. } => "kick_source",
            IcecastCommand::KickListener { .. } => "kick_listener",
            IcecastCommand::Switch { .. } => "switch",
        };
        tracing::debug!(target: "stationdeck::icecast", action, "Icecast operation submitted");
        let token = self.token.get();
        let slot = self.slot.clone();
        let signal = controller.signal();
        let deadline = controller.clone();
        *self.abort.borrow_mut() = Some(controller);
        wasm_bindgen_futures::spawn_local(async move {
            let _timeout = gloo_timers::callback::Timeout::new(12000, move || deadline.abort());
            let result = async {
                let response = Request::post("/api/icecast/control").header("Authorization", &format!("Bearer {token}"))
                    .header("Content-Type", "application/json").abort_signal(Some(&signal)).body(body)
                    .map_err(|_| "Cannot encode Icecast operation.")?.send().await
                    .map_err(|_| "Operation cancelled or timed out; its outcome may be unknown. Refresh before retrying.")?;
                match response.json::<ControlResponse>().await.map_err(|_| "Invalid Icecast operation response.")? {
                    ControlResponse::Completed { result } => Ok(result),
                    ControlResponse::Unavailable { message, .. } => Err(message),
                }
            }.await;
            let succeeded = result.is_ok();
            if slot.borrow_mut().finish(generation, result) {
                if succeeded {
                    tracing::info!(target: "stationdeck::icecast", action, "Icecast operation completed");
                } else {
                    tracing::warn!(target: "stationdeck::icecast", action, "Icecast operation failed; verify status before retrying");
                }
            } else {
                tracing::debug!(target: "stationdeck::icecast", action, "Late Icecast operation result ignored");
            }
        });
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn submit(&self, command: IcecastCommand) -> Result<(), String> {
        command.validate()?;
        Err("Icecast operations require the browser build.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disconnect_invalidates_a_late_operation_result() {
        let client = ControlClient::default();
        let generation = client.slot.borrow_mut().begin().unwrap();
        client.disconnect();
        assert!(!client.slot.borrow_mut().finish(
            generation,
            Ok(IcecastResult::Done {
                message: "late".into()
            })
        ));
        assert!(matches!(client.snapshot(), RequestState::Idle));
    }
}
