//! StationDeck web host. Native builds run model/UI tests, not a desktop window.
#[cfg(any(target_arch = "wasm32", test))]
mod app;
#[cfg(any(target_arch = "wasm32", test))]
mod assembly;

#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    pub struct WebHandle {
        runner: eframe::WebRunner,
    }

    #[wasm_bindgen]
    impl WebHandle {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            Self {
                runner: eframe::WebRunner::new(),
            }
        }

        pub async fn start(&self, canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
            self.runner
                .start(
                    canvas,
                    eframe::WebOptions::default(),
                    Box::new(|cc| Ok(Box::new(crate::app::StationDeck::new(cc)))),
                )
                .await
        }

        pub fn destroy(&self) {
            self.runner.destroy();
        }
    }

    impl Default for WebHandle {
        fn default() -> Self {
            Self::new()
        }
    }
}
