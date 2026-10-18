//! Offline test module: two views and a cancellable, nonblocking simulated task.
use stationdeck_services::{RequestSlot, RequestState, StationStatus};
use stationdeck_ui::{
    show_state, show_station_status, DeckModule, ModuleDescriptor, ViewDescriptor, ViewState,
};

#[derive(Default)]
pub struct DemoModule {
    context: Option<stationdeck_services::registry::Context>,
    task: RequestSlot<u32>,
    due: Option<(u64, f64)>,
    completed: u32,
    read_only: bool,
}

impl DemoModule {
    fn start_task(&mut self, now: f64) -> bool {
        if self.read_only || !self.context.as_ref().is_some_and(|c| c.authorize("demo.overview", "demo.simuler", "command").is_ok()) {
            return false;
        }
        let Some(generation) = self.task.begin() else {
            return false;
        };
        self.due = Some((generation, now + 0.75));
        true
    }
}
impl DeckModule for DemoModule {
    fn manifest(&self) -> &str { include_str!("../../../Doc/stationdeck/demo.module.toml") }
    fn start(&mut self, context: &stationdeck_services::registry::Context) -> Result<(), String> {
        self.context = Some(context.clone());
        let mut slot = stationdeck_services::RequestSlot::default();
        let generation = slot.begin().unwrap();
        slot.finish(generation, Ok(StationStatus { station_name: "StationDeck Demo".into(), uptime_seconds: 3600, pid: 0, timezone: "Europe/Paris".into() }));
        context.publish(&stationdeck_services::overview::OVERVIEW,
            std::rc::Rc::new(stationdeck_services::overview::SharedOverview(std::rc::Rc::new(std::cell::RefCell::new(slot)))) as std::rc::Rc<dyn stationdeck_services::overview::OverviewService>)
    }
    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: "demo",
            title: "Test module",
            description: "Offline sample data and a cancellable simulated task.",
        }
    }
    fn views(&self) -> Vec<ViewDescriptor> {
        vec![
            ViewDescriptor {
                id: "demo.overview",
                title: "Test station",
                order: 100,
            },
            ViewDescriptor {
                id: "demo.tasks",
                title: "Test tasks",
                order: 110,
            },
        ]
    }
    fn stop(&mut self) {
        self.task.cancel();
        self.due = None;
    }
    fn ui(&mut self, id: &str, ui: &mut egui::Ui) {
        if id == "demo.overview" {
            ui.heading("Test station");
            ui.label("Simulated data. No server is contacted and no real station is changed.");
            ui.add_space(16.0);
            show_station_status(
                ui,
                &StationStatus {
                    station_name: "StationDeck Demo".into(),
                    uptime_seconds: 3600,
                    pid: 0,
                    timezone: "Europe/Paris".into(),
                },
            );
            return;
        }
        ui.heading("Test tasks");
        ui.label("Run a short local task while keeping navigation responsive.");
        if stationdeck_ui::test_ui::response("demo-read-only", ui.checkbox(&mut self.read_only, "Read-only test role"))
            .changed()
            && self.read_only
        {
            self.stop();
        }
        if self.read_only || !self.context.as_ref().is_some_and(|c| c.authorize("demo.overview", "demo.simuler", "command").is_ok()) {
            show_state(ui, ViewState::Forbidden);
        }
        let now = ui.input(|input| input.time);
        if let Some((generation, due)) = self.due {
            if now >= due {
                self.completed += 1;
                self.task.finish(generation, Ok(self.completed));
                self.due = None;
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        let pending = matches!(self.task.state, RequestState::Pending);
        if stationdeck_ui::test_ui::response("demo-run", ui.add_enabled(
            !pending && !self.read_only, egui::Button::new("Run test task"),
        )).clicked()
            && self.start_task(now)
        {
            ui.ctx().request_repaint();
        }
        if pending && ui.button("Cancel task").clicked() {
            self.stop();
        }
        match &self.task.state {
            RequestState::Idle => show_state(ui, ViewState::Empty("No test task has run.")),
            RequestState::Pending => show_state(
                ui,
                ViewState::Loading("Simulated work is in progress. You can continue navigating."),
            ),
            RequestState::Ready(count) => {
                ui.label(format!("Test task completed. Completed tasks: {count}"));
            }
            RequestState::Failed(error) => show_state(ui, ViewState::Error(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    use stationdeck_services::registry::{Context, Grants, Module, Policy, Registry};
    struct Hosted(Rc<RefCell<DemoModule>>);
    impl Module for Hosted {
        fn id(&self) -> &str { "demo" }
        fn manifest(&self) -> &str { include_str!("../../../Doc/stationdeck/demo.module.toml") }
        fn start(&mut self, context: &Context) -> Result<(), String> { self.0.borrow_mut().start(context) }
        fn stop(&mut self) { self.0.borrow_mut().stop(); }
    }
    #[test]
    fn read_only_cancellation_and_host_revocation_are_enforced_below_the_button() {
        let module = Rc::new(RefCell::new(DemoModule::default()));
        assert!(!module.borrow_mut().start_task(0.0));
        let manifest = stationdeck_services::manifest::Manifest::parse(module.borrow().manifest()).unwrap();
        let mut policy = Policy::default();
        // This fixture explicitly grants only the known local demo permissions.
        policy.grants.insert("demo".into(), Grants {
            capabilities: manifest.capabilities,
            permissions: ["demo.consulter".into(), "demo.simuler".into()].into_iter().collect(),
        });
        let mut registry = Registry::new(vec![Hosted(module.clone())], &["demo".into()], policy);
        {
            let mut module = module.borrow_mut();
            module.read_only = true;
            assert!(!module.start_task(0.0));
            module.read_only = false;
            assert!(module.start_task(0.0));
            assert!(!module.start_task(0.1));
            module.stop();
            assert!(module.due.is_none());
            assert!(matches!(module.task.state, RequestState::Idle));
            assert!(module.start_task(1.0));
        }
        registry.set_enabled("demo", false).unwrap();
        assert!(module.borrow().due.is_none());
        assert!(!module.borrow_mut().start_task(2.0));
    }
}
