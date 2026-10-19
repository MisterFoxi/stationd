//! Browser-session diagnostics. Never persist events or record credentials/payloads.
use egui_tracing::{tracing::collector::AllowedTargets, EventCollector};
use stationdeck_services::registry::State;
use stationdeck_ui::{ModuleCatalog, StatusTone};
use std::collections::BTreeMap;
use tracing_subscriber::prelude::*;

const MAX_EVENTS: usize = 1000;
fn collector() -> EventCollector {
    EventCollector::default()
        .with_max_level(tracing::Level::TRACE)
        .with_max_events(Some(MAX_EVENTS))
        .allowed_targets(AllowedTargets::Selected(vec!["stationdeck".into()]))
}

pub struct Diagnostics {
    collector: EventCollector,
    installed: bool,
    previous: BTreeMap<&'static str, (State, StatusTone, &'static str)>,
}
impl Diagnostics {
    pub fn new() -> Self {
        #[cfg(target_arch = "wasm32")]
        let (collector, installed) = {
            // WebRunner can be destroyed/recreated in the same WASM instance.
            // A second host must reuse the one global subscriber and its collector.
            static SESSION: std::sync::OnceLock<(EventCollector, bool)> =
                std::sync::OnceLock::new();
            SESSION.get_or_init(|| {
                let collector = collector();
                let installed = tracing_subscriber::registry().with(collector.clone()).try_init().is_ok();
                if installed {
                    tracing::info!(target: "stationdeck::host", version = env!("CARGO_PKG_VERSION"), "StationDeck browser session started");
                }
                (collector, installed)
            }).clone()
        };
        #[cfg(not(target_arch = "wasm32"))]
        let (collector, installed) = (collector(), true);
        Self {
            collector,
            installed,
            previous: BTreeMap::new(),
        }
    }
    pub fn observe(&mut self, modules: &ModuleCatalog) {
        for descriptor in modules.descriptors() {
            let Some((state, _)) = modules.status(descriptor.id) else {
                continue;
            };
            let health = modules.health(descriptor.id);
            let current = (state, health.tone, health.label);
            if self.previous.insert(descriptor.id, current) == Some(current) {
                continue;
            }
            // Only bounded identifiers and static labels: no diagnostic payloads,
            // stream titles, listener addresses, HTTP headers or tokens.
            match (state, health.tone) {
                (State::Failed, _) | (_, StatusTone::Error) => {
                    tracing::error!(target: "stationdeck::modules", module = descriptor.id, ?state, health = health.label, "Plugin state changed")
                }
                (State::Blocked, _) => {
                    tracing::warn!(target: "stationdeck::modules", module = descriptor.id, ?state, health = health.label, "Plugin state changed")
                }
                _ => {
                    tracing::info!(target: "stationdeck::modules", module = descriptor.id, ?state, health = health.label, "Plugin state changed")
                }
            }
        }
        // Drain even with Logs closed: the upstream collector bounds its retained
        // vector on drain, while its pending buffer is otherwise unbounded.
        self.collector.drain_pending();
    }
    pub fn window(&self, ctx: &egui::Context, open: &mut bool, workspace: egui::Rect) {
        if !*open {
            return;
        }
        let response = egui::Window::new("Logs")
            .id(egui::Id::new("stationdeck.logs"))
            .order(egui::Order::Foreground)
            .open(open)
            .default_pos(workspace.min + egui::vec2(20.0, 16.0))
            .default_size(egui::vec2(900.0, 460.0))
            .min_size(egui::vec2(300.0, 180.0))
            .max_size((workspace.size() - egui::vec2(26.0, 72.0)).max(egui::vec2(300.0, 180.0)))
            .constrain_to(workspace)
            .show(ctx, |ui| {
                ui.weak("Browser session · latest 1,000 events · memory only");
                if !self.installed {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "Trace collection unavailable: another subscriber is already installed.",
                    );
                }
                ui.weak("Daemon logs remain in their containers.");
                stationdeck_ui::test_ui::value("logs-count", &self.collector.len().to_string());
                let response = ui.add(egui_tracing::Logs::new(self.collector.clone()));
                stationdeck_ui::test_ui::control("logs-view", &response);
            });
        if let Some(response) = response {
            stationdeck_ui::test_ui::control("window-logs", &response.response);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_is_bounded_filtered_and_clearable() {
        let collector = collector();
        let subscriber = tracing_subscriber::registry().with(collector.clone());
        tracing::subscriber::with_default(subscriber, || {
            for sequence in 0..1200 {
                tracing::info!(target: "stationdeck::test", sequence, "Fixture event");
            }
            tracing::warn!(target: "unrelated::fixture", "Excluded target");
        });
        let events = collector.events();
        assert_eq!(events.len(), MAX_EVENTS);
        assert!(events.iter().all(|e| e.target == "stationdeck::test"));
        assert!(events
            .front()
            .unwrap()
            .fields
            .iter()
            .any(|(key, value)| key == "sequence" && value == "200"));
        collector.clear();
        assert!(collector.events().is_empty());
    }
    #[test]
    fn unchanged_frames_do_not_repeat_plugin_events() {
        let mut diagnostics = Diagnostics::new();
        let subscriber = tracing_subscriber::registry().with(diagnostics.collector.clone());
        tracing::subscriber::with_default(subscriber, || {
            let mut modules = ModuleCatalog::new(
                crate::assembly::modules(),
                &["demo".into()],
                crate::assembly::policy_with_provider("demo"),
            );
            diagnostics.observe(&modules);
            let count = diagnostics.collector.len();
            diagnostics.observe(&modules);
            assert_eq!(diagnostics.collector.len(), count);
            modules.set_enabled("demo", false).unwrap();
            diagnostics.observe(&modules);
            assert!(diagnostics.collector.len() > count);
        });
    }
}
