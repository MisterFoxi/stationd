//! Read-only canvas geometry for the isolated browser recipe. No actions or secrets.
#[cfg(all(feature = "ui-test", target_arch = "wasm32"))]
fn canvas() -> Option<web_sys::Element> {
    web_sys::window()?.document()?.get_element_by_id("stationdeck")
}

pub fn begin_frame() {
    #[cfg(all(feature = "ui-test", target_arch = "wasm32"))]
    if let Some(canvas) = canvas() {
        for name in canvas.get_attribute_names().iter().filter_map(|name| name.as_string()) {
            if name.starts_with("data-qa-") {
                let _ = canvas.remove_attribute(&name);
            }
        }
    }
}

pub fn value(id: &str, value: &str) {
    #[cfg(all(feature = "ui-test", target_arch = "wasm32"))]
    if let Some(canvas) = canvas() {
        let _ = canvas.set_attribute(&format!("data-qa-{id}"), value);
    }
    #[cfg(not(all(feature = "ui-test", target_arch = "wasm32")))]
    let _ = (id, value);
}

pub fn control(id: &str, response: &egui::Response) {
    #[cfg(all(feature = "ui-test", target_arch = "wasm32"))]
    {
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio()) as f32;
        let scale = response.ctx.pixels_per_point() / dpr;
        let r = response.rect;
        value(id, &format!("{},{},{},{}", r.left() * scale, r.top() * scale, r.width() * scale, r.height() * scale));
    }
    #[cfg(not(all(feature = "ui-test", target_arch = "wasm32")))]
    let _ = (id, response);
}

pub fn response(id: &str, response: egui::Response) -> egui::Response {
    control(id, &response);
    response
}
