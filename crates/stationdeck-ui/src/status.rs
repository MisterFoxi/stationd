//! Shared operational states and compact instruments for the plugin workspace.
use egui::{Color32, RichText, Ui};
use stationdeck_services::{registry::State, RequestState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusTone {
    Good,
    Warning,
    Error,
    Inactive,
}
impl StatusTone {
    pub fn name(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Inactive => "inactive",
        }
    }
    pub fn color(self, ui: &Ui) -> Color32 {
        match (self, ui.visuals().dark_mode) {
            (Self::Good, true) => Color32::from_rgb(82, 214, 118),
            (Self::Good, false) => Color32::from_rgb(24, 119, 55),
            (Self::Warning, true) => Color32::from_rgb(239, 180, 75),
            (Self::Warning, false) => Color32::from_rgb(145, 88, 0),
            (Self::Error, true) => Color32::from_rgb(239, 100, 100),
            (Self::Error, false) => Color32::from_rgb(180, 35, 30),
            (Self::Inactive, _) => Color32::from_rgb(12, 15, 19),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginStatus {
    pub tone: StatusTone,
    pub label: &'static str,
}
impl Default for PluginStatus {
    fn default() -> Self {
        Self::new(StatusTone::Inactive, "Not connected")
    }
}
impl PluginStatus {
    pub const fn new(tone: StatusTone, label: &'static str) -> Self {
        Self { tone, label }
    }
    pub fn lifecycle(state: State) -> Self {
        match state {
            State::Running => Self::new(StatusTone::Good, "Running"),
            State::Failed => Self::new(StatusTone::Error, "Failed"),
            State::Blocked => Self::new(StatusTone::Warning, "Blocked"),
            State::Disabled => Self::new(StatusTone::Inactive, "Disabled"),
            State::Stopped => Self::new(StatusTone::Inactive, "Stopped"),
            State::Stopping => Self::new(StatusTone::Warning, "Stopping"),
            State::Validated => Self::new(StatusTone::Warning, "Waiting to start"),
            State::Initializing | State::Starting => Self::new(StatusTone::Warning, "Starting"),
        }
    }
    pub fn request<T>(state: &RequestState<T>) -> Self {
        match state {
            RequestState::Idle => Self::default(),
            RequestState::Pending => Self::new(StatusTone::Warning, "Reading"),
            RequestState::Ready(_) => Self::new(StatusTone::Good, "Snapshot available"),
            RequestState::Failed(_) => Self::new(StatusTone::Error, "Read failed"),
        }
    }
}
/// Painted geometry avoids missing Unicode status glyphs in bundled fonts.
pub fn status_dot(ui: &mut Ui, tone: StatusTone) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 4.5, tone.color(ui));
    ui.painter().circle_stroke(
        rect.center(),
        4.5,
        egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
    );
    response
}
pub fn status_badge(ui: &mut Ui, status: PluginStatus) {
    ui.horizontal(|ui| {
        status_dot(ui, status.tone);
        ui.label(RichText::new(status.label).small());
    });
}
/// A small, wrapping instrument: label, measured value and explicit state color.
pub fn vital(ui: &mut Ui, label: &str, value: &str, tone: StatusTone) {
    let border = if tone == StatusTone::Inactive {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    } else {
        tone.color(ui).gamma_multiply(0.45)
    };
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0_f32, border))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    status_dot(ui, tone);
                    ui.label(RichText::new(value).strong().size(17.0));
                });
                ui.label(RichText::new(label).small().weak());
            });
        });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operational_states_never_report_unread_or_failed_data_as_healthy() {
        assert_eq!(
            PluginStatus::request::<()>(&RequestState::Idle).tone,
            StatusTone::Inactive
        );
        assert_eq!(
            PluginStatus::request::<()>(&RequestState::Pending).tone,
            StatusTone::Warning
        );
        assert_eq!(
            PluginStatus::request(&RequestState::Ready(())).tone,
            StatusTone::Good
        );
        assert_eq!(
            PluginStatus::request::<()>(&RequestState::Failed("Unavailable".into())).tone,
            StatusTone::Error
        );
        assert_eq!(
            PluginStatus::lifecycle(State::Blocked).tone,
            StatusTone::Warning
        );
        assert_eq!(
            PluginStatus::lifecycle(State::Failed).tone,
            StatusTone::Error
        );
        assert_eq!(
            PluginStatus::lifecycle(State::Disabled).tone,
            StatusTone::Inactive
        );
    }
}
