use egui::{Color32, Key, Modifiers, RichText};
use serde::{Deserialize, Serialize};
use stationdeck_ui::{
    show_state, status_badge, status_dot, vital, ModuleCatalog, StatusTone, ViewState,
};
use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
const SETTINGS_KEY: &str = "stationdeck.settings.v1";
const ACCENT: Color32 = Color32::from_rgb(57, 189, 176);
const WARNING: Color32 = Color32::from_rgb(235, 180, 75);

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize)]
enum Theme {
    #[default]
    Dark,
    Light,
}
fn default_modules() -> Vec<String> {
    vec!["demo".into(), "overview".into()]
}
fn default_provider() -> String {
    "demo".into()
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginWindow {
    open: bool,
    pinned: bool,
    view: String,
    position: Option<[f32; 2]>,
    size: Option<[f32; 2]>,
}
impl PluginWindow {
    fn valid(&self) -> bool {
        self.view.len() <= 128
            && self
                .position
                .is_none_or(|p| p.iter().all(|n| n.is_finite() && n.abs() <= 100_000.0))
            && self.size.is_none_or(|s| {
                s.iter()
                    .all(|n| n.is_finite() && (1.0..=100_000.0).contains(n))
            })
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    version: u32,
    theme: Theme,
    zoom: f32,
    #[serde(default = "default_modules")]
    enabled_modules: Vec<String>,
    #[serde(default = "default_provider")]
    overview_provider: String,
    #[serde(default)]
    windows: BTreeMap<String, PluginWindow>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            theme: Theme::Dark,
            zoom: 1.0,
            enabled_modules: default_modules(),
            overview_provider: default_provider(),
            windows: BTreeMap::new(),
        }
    }
}
impl Settings {
    fn valid(&self) -> bool {
        self.version == 1
            && self.zoom.is_finite()
            && (0.9..=1.4).contains(&self.zoom)
            && self.enabled_modules.len() <= 64
            && matches!(self.overview_provider.as_str(), "demo" | "grpc")
            && self.windows.len() <= 128
            && self
                .windows
                .iter()
                .all(|(id, w)| id.len() <= 128 && w.valid())
    }
}
#[derive(Default, PartialEq)]
enum Page {
    #[default]
    Home,
    Modules,
    Settings,
}

pub struct StationDeck {
    settings: Settings,
    page: Page,
    settings_error: bool,
    preferences_dirty: bool,
    persistence_error: bool,
    module_error: Option<String>,
    modules: ModuleCatalog,
    layout_generation: u64,
    focus: Option<String>,
}
impl StationDeck {
    fn with_settings(settings: Settings, settings_error: bool) -> Self {
        let modules = ModuleCatalog::new(
            crate::assembly::modules(),
            &settings.enabled_modules,
            crate::assembly::policy_with_provider(&settings.overview_provider),
        );
        Self {
            settings,
            page: Page::Home,
            settings_error,
            preferences_dirty: false,
            persistence_error: false,
            module_error: None,
            modules,
            layout_generation: 0,
            focus: None,
        }
    }
    #[cfg(target_arch = "wasm32")]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let storage = web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten();
        let stored = storage
            .as_ref()
            .and_then(|s| s.get_item(SETTINGS_KEY).ok())
            .flatten();
        let parsed = stored
            .as_deref()
            .and_then(|v| serde_json::from_str::<Settings>(v).ok());
        let invalid = stored.is_some() && !parsed.as_ref().is_some_and(Settings::valid);
        let mut app =
            Self::with_settings(parsed.filter(Settings::valid).unwrap_or_default(), invalid);
        app.persistence_error = storage.is_none();
        app.apply_style(&cc.egui_ctx);
        app
    }
    #[cfg(target_arch = "wasm32")]
    fn persist_preferences(&mut self) {
        self.persistence_error = !(|| {
            let storage = web_sys::window()?.local_storage().ok()??;
            storage
                .set_item(SETTINGS_KEY, &serde_json::to_string(&self.settings).ok()?)
                .ok()
        })()
        .is_some();
        self.preferences_dirty = false;
    }
    fn apply_style(&self, ctx: &egui::Context) {
        ctx.set_theme(if self.settings.theme == Theme::Dark {
            egui::ThemePreference::Dark
        } else {
            egui::ThemePreference::Light
        });
        let mut visuals = if self.settings.theme == Theme::Dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.selection.bg_fill = ACCENT.gamma_multiply(0.3);
        visuals.selection.stroke.color = ACCENT;
        visuals.window_corner_radius = egui::CornerRadius::same(5);
        if self.settings.theme == Theme::Dark {
            visuals.override_text_color = Some(Color32::from_rgb(220, 226, 234));
            visuals.panel_fill = Color32::from_rgb(21, 25, 31);
            visuals.window_fill = Color32::from_rgb(32, 38, 46);
            visuals.window_stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(65, 74, 86));
            visuals.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(55, 64, 76);
        }
        ctx.set_visuals(visuals);
        ctx.set_zoom_factor(self.settings.zoom);
        ctx.style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(10.0, 8.0);
            s.spacing.button_padding = egui::vec2(10.0, 6.0);
            s.spacing.window_margin = egui::Margin::same(12);
        });
    }
    fn open_view(&mut self, id: &str) {
        self.page = Page::Home;
        let module = id.split('.').next().unwrap_or_default();
        let window = self.settings.windows.entry(module.into()).or_default();
        window.open = true;
        window.view = id.into();
        self.focus = Some(module.into());
        self.preferences_dirty = true;
    }
    fn arrange(&mut self) {
        for window in self.settings.windows.values_mut() {
            window.position = None;
            window.size = None;
        }
        self.layout_generation += 1;
        self.preferences_dirty = true;
    }
    fn render(&mut self, ctx: &egui::Context) {
        stationdeck_ui::test_ui::begin_frame();
        for descriptor in self.modules.descriptors() {
            if let Some((state, _)) = self.modules.status(descriptor.id) {
                stationdeck_ui::test_ui::value(
                    &format!("state-{}", descriptor.id),
                    &format!("{state:?}"),
                );
            }
        }
        for descriptor in self.modules.descriptors() {
            if self.modules.health(descriptor.id).label == "Reading" {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            stationdeck_ui::test_ui::value(
                &format!("health-{}", descriptor.id),
                self.modules.health(descriptor.id).tone.name(),
            );
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Comma)) {
            self.page = Page::Settings;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Home)) {
            self.page = Page::Home;
        }
        egui::TopBottomPanel::top("stationdeck.header").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.set_min_height(40.0);
                ui.label(RichText::new("SD").strong().size(22.0).color(ACCENT));
                ui.heading("StationDeck");
                ui.separator();
                for (label, page) in [
                    ("Workspace", Page::Home),
                    ("Modules", Page::Modules),
                    ("Settings", Page::Settings),
                ] {
                    if ui.selectable_label(self.page == page, label).clicked() {
                        self.page = page;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("V1 · Station workspace");
                });
            });
        });
        egui::TopBottomPanel::top("stationdeck.telemetry").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                self.modules.summaries(ui);
                for descriptor in self.modules.descriptors() {
                    if self.modules.is_enabled(descriptor.id)
                        && self.modules.status(descriptor.id).is_some_and(|(s, _)| {
                            matches!(
                                s,
                                stationdeck_services::registry::State::Blocked
                                    | stationdeck_services::registry::State::Failed
                            )
                        })
                    {
                        let health = self.modules.health(descriptor.id);
                        vital(ui, descriptor.title, health.label, health.tone);
                    }
                }
                if self.modules.enabled_ids().is_empty() {
                    vital(ui, "Workspace", "No plugins enabled", StatusTone::Inactive);
                }
            });
        });
        let views = self.modules.views();
        // First appearance opens each plugin; closing a window never stops its module.
        for view in &views {
            let module = view.id.split('.').next().unwrap_or_default();
            self.settings
                .windows
                .entry(module.into())
                .or_insert_with(|| PluginWindow {
                    open: true,
                    view: view.id.into(),
                    ..Default::default()
                });
        }
        egui::TopBottomPanel::bottom("stationdeck.status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let count = self
                    .settings
                    .windows
                    .iter()
                    .filter(|(id, w)| {
                        w.open
                            && views
                                .iter()
                                .any(|v| v.id.split('.').next() == Some(id.as_str()))
                    })
                    .count();
                ui.label(format!("{count} plugin windows open"));
                ui.separator();
                if self.settings_error || self.persistence_error {
                    ui.colored_label(WARNING, "Temporary preferences");
                } else {
                    ui.weak("Layout saved locally");
                }
                ui.separator();
                ui.weak(format!("StationDeck · v{}", env!("CARGO_PKG_VERSION")));
            });
        });
        egui::SidePanel::left("stationdeck.navigation").default_width(190.0).width_range(150.0..=280.0).show(ctx, |ui| {
            ui.add_space(8.0);
            ui.weak("WORKSPACE");
            for (label, page) in [("Home", Page::Home), ("Modules", Page::Modules), ("Settings", Page::Settings)] {
                let response = ui.selectable_label(self.page == page, label);
                stationdeck_ui::test_ui::control(&format!("nav-{}", label.to_ascii_lowercase()), &response);
                if response.clicked() { self.page = page; }
            }
            ui.separator();
            ui.weak("PLUGINS");
            egui::ScrollArea::vertical().id_salt("plugin.launcher").max_height((ui.available_height() - 100.0).max(100.0)).show(ui, |ui| {
                for descriptor in self.modules.descriptors() {
                    let module_views: Vec<_> = views.iter().filter(|v| v.id.split('.').next() == Some(descriptor.id)).collect();
                    ui.push_id(descriptor.id, |ui| {
                        let health = self.modules.health(descriptor.id);
                        let open = !module_views.is_empty() && self.settings.windows.get(descriptor.id).is_some_and(|w| w.open);
                        let response = ui.horizontal(|ui| {
                            let led = status_dot(ui, health.tone);
                            stationdeck_ui::test_ui::control(&format!("plugin-led-{}", descriptor.id), &led);
                            ui.selectable_label(open, descriptor.title)
                        }).inner.on_hover_text(format!("{} — {}\n{}", descriptor.title, health.label, descriptor.description));
                        stationdeck_ui::test_ui::control(&format!("plugin-row-{}", descriptor.id), &response);
                        if response.clicked() {
                            if let Some(first) = module_views.first() {
                                let selected = self.settings.windows.get(descriptor.id)
                                    .map(|w| w.view.as_str()).filter(|id| module_views.iter().any(|v| v.id == *id))
                                    .unwrap_or(first.id).to_owned();
                                self.open_view(&selected);
                            }
                            else { self.page = Page::Modules; }
                        }
                        if module_views.len() == 1 { stationdeck_ui::test_ui::control(&format!("nav-{}", module_views[0].id), &response); }
                        ui.add_space(3.0);
                    });
                }
            });
            ui.separator();            if ui.button("Arrange windows").clicked() { self.arrange(); }
            ui.weak("Drag title bars to move.\nResize from window edges.\nPin a window to lock its position.");
        });
        let workspace = egui::CentralPanel::default().show(ctx, |ui| {
            if self.settings_error {
                show_state(ui, ViewState::Error("Saved preferences are invalid or from a newer version. Temporary defaults are being used."));
                if ui.button("Replace saved preferences").clicked() { self.settings_error = false; self.preferences_dirty = true; }
            }
            if self.persistence_error {
                show_state(ui, ViewState::Error("Preferences cannot be saved in this browser. Changes are temporary."));
                if ui.button("Retry saving").clicked() { self.preferences_dirty = true; }
            }
            if let Some(error) = &self.module_error { show_state(ui, ViewState::Error(error)); }
            if views.is_empty() {
                ui.add_space(24.0);
                ui.heading("Your station workspace");
                ui.label("Enable a plugin in Modules to open its window.");
                if ui.button("Manage modules").clicked() { self.page = Page::Modules; }
            }
            ui.available_rect_before_wrap()
        }).inner.shrink(8.0);
        self.plugin_windows(ctx, workspace);
        // Host settings are separate windows; plugins stay visible underneath.
        if self.page != Page::Home {
            let mut open = true;
            let title = if self.page == Page::Modules {
                "Modules"
            } else {
                "Settings"
            };
            egui::Window::new(title)
                .id(egui::Id::new(("host", title)))
                .order(egui::Order::Foreground)
                .open(&mut open)
                .default_pos(workspace.min + egui::vec2(12.0, 8.0))
                .default_width(600.0)
                .default_height((workspace.height() - 100.0).max(180.0))
                .max_size((workspace.size() - egui::vec2(26.0, 72.0)).max(egui::vec2(160.0, 90.0)))
                .hscroll(true)
                .constrain_to(workspace)
                .vscroll(true)
                .show(ctx, |ui| {
                    if self.page == Page::Modules {
                        self.module_settings(ui);
                    } else {
                        self.preferences(ui, ctx);
                    }
                });
            if !open {
                self.page = Page::Home;
            }
        }
    }
    fn plugin_windows(&mut self, ctx: &egui::Context, workspace: egui::Rect) {
        let views = self.modules.views();
        let groups: Vec<_> = self
            .modules
            .descriptors()
            .into_iter()
            .filter(|d| views.iter().any(|v| v.id.split('.').next() == Some(d.id)))
            .collect();
        let columns = if workspace.width() >= 1000.0 { 2 } else { 1 };
        let width = ((workspace.width() - 16.0) / columns as f32 - 28.0).max(180.0);
        let rows = groups.len().div_ceil(columns).max(1);
        let height = (workspace.height() / rows as f32 - 72.0).clamp(90.0, 340.0);
        for (index, descriptor) in groups.iter().enumerate() {
            let Some(mut state) = self.settings.windows.get(descriptor.id).cloned() else {
                continue;
            };
            if !state.open {
                continue;
            }
            let module_views: Vec<_> = views
                .iter()
                .filter(|v| v.id.split('.').next() == Some(descriptor.id))
                .collect();
            if !module_views.iter().any(|v| v.id == state.view) {
                state.view = module_views[0].id.into();
            }
            let id = egui::Id::new(("plugin.window", descriptor.id, self.layout_generation));
            let default_pos = workspace.min
                + egui::vec2(
                    (index % columns) as f32 * (width + 28.0),
                    (index / columns) as f32 * (height + 54.0),
                );
            let position = state.position.map(egui::Pos2::from).unwrap_or(default_pos);
            let size = state
                .size
                .map(egui::Vec2::from)
                .unwrap_or(egui::vec2(width, height));
            let mut open = state.open;
            let response = egui::Window::new(descriptor.title)
                .id(id)
                .open(&mut open)
                .default_pos(position)
                .default_size(size)
                .min_size(egui::vec2(160.0, 90.0))
                .max_size((workspace.size() - egui::vec2(26.0, 72.0)).max(egui::vec2(160.0, 90.0)))
                .constrain_to(workspace)
                .movable(!state.pinned)
                .resizable(!state.pinned)
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if stationdeck_ui::test_ui::response(
                            &format!("pin-{}", descriptor.id),
                            ui.selectable_label(
                                state.pinned,
                                if state.pinned { "Unpin" } else { "Pin" },
                            ),
                        )
                        .clicked()
                        {
                            state.pinned = !state.pinned;
                            self.preferences_dirty = true;
                        }
                        status_badge(ui, self.modules.health(descriptor.id));
                        for view in &module_views {
                            if module_views.len() > 1
                                && stationdeck_ui::test_ui::response(
                                    &format!("tab-{}", view.id),
                                    ui.selectable_label(state.view == view.id, view.title),
                                )
                                .clicked()
                            {
                                state.view = view.id.into();
                                self.preferences_dirty = true;
                            }
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::both()
                        .id_salt((descriptor.id, "content"))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            self.modules.render(&state.view, ui);
                        });
                    ui.min_rect().size()
                });
            if let Some(response) = response {
                stationdeck_ui::test_ui::control(
                    &format!("window-{}", descriptor.id),
                    &response.response,
                );
                if self.focus.as_deref() == Some(descriptor.id) {
                    ctx.move_to_top(response.response.layer_id);
                    self.focus = None;
                }
                if !ctx.input(|i| i.pointer.any_down()) {
                    let pos = [response.response.rect.min.x, response.response.rect.min.y];
                    if state.position != Some(pos) {
                        state.position = Some(pos);
                        self.preferences_dirty = true;
                    }
                    if let Some(size) = response.inner {
                        let size = [size.x.max(1.0), size.y.max(1.0)];
                        if state.size != Some(size) {
                            state.size = Some(size);
                            self.preferences_dirty = true;
                        }
                    }
                }
            }
            if state.open != open {
                state.open = open;
                self.preferences_dirty = true;
            }
            self.settings.windows.insert(descriptor.id.into(), state);
        }
    }
    fn module_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Modules");
        ui.label("Enable trusted modules compiled into this application.");
        let previous = self.settings.overview_provider.clone();
        let provider = egui::ComboBox::from_id_salt("overview.provider")
            .selected_text(if previous == "grpc" { "gRPC" } else { "Demo" })
            .show_ui(ui, |ui| {
                let _ = stationdeck_ui::test_ui::response(
                    "provider-demo",
                    ui.selectable_value(
                        &mut self.settings.overview_provider,
                        "demo".into(),
                        "Demo",
                    ),
                );
                let _ = stationdeck_ui::test_ui::response(
                    "provider-grpc",
                    ui.selectable_value(
                        &mut self.settings.overview_provider,
                        "grpc".into(),
                        "gRPC",
                    ),
                );
            });
        stationdeck_ui::test_ui::control("provider", &provider.response);
        ui.weak("Station overview provider. Enable the selected provider and the Station overview module.");
        if previous != self.settings.overview_provider {
            if let Err(error) = self.modules.set_binding(
                "overview",
                "stationdeck.admin.overview",
                &self.settings.overview_provider,
            ) {
                self.module_error = Some(error);
            }
            self.preferences_dirty = true;
        }
        ui.add_space(16.0);
        for descriptor in self.modules.descriptors() {
            ui.push_id(descriptor.id, |ui| {
                let mut enabled = self.modules.is_enabled(descriptor.id);
                let checkbox = ui.checkbox(&mut enabled, descriptor.title);
                stationdeck_ui::test_ui::control(&format!("module-{}", descriptor.id), &checkbox);
                if checkbox.changed() {
                    if let Err(error) = self.modules.set_enabled(descriptor.id, enabled) {
                        self.module_error = Some(error);
                    }
                    self.settings.enabled_modules = self.modules.enabled_ids();
                    self.preferences_dirty = true;
                }
                ui.weak(descriptor.description);
                if let Some((state, diagnostic)) = self
                    .modules
                    .status(descriptor.id)
                    .map(|(state, diagnostic)| (state, diagnostic.cloned()))
                {
                    status_badge(ui, self.modules.health(descriptor.id));
                    if enabled
                        && matches!(
                            state,
                            stationdeck_services::registry::State::Blocked
                                | stationdeck_services::registry::State::Failed
                        )
                        && ui.button("Retry startup").clicked()
                    {
                        let _ = self.modules.set_enabled(descriptor.id, true);
                    }
                    if let Some(diagnostic) = diagnostic {
                        show_state(
                            ui,
                            ViewState::Error(&format!(
                                "{} ({})",
                                diagnostic.message,
                                diagnostic.chain.join(" → ")
                            )),
                        );
                    }
                }
                ui.add_space(12.0);
            });
        }
        ui.weak("Disabling a module cancels its requests, removes its views and clears its in-memory credentials.");
    }

    fn preferences(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Local settings");
        ui.label("These preferences apply to this browser.");
        ui.add_space(18.0);
        let mut changed = false;
        ui.push_id("stationdeck.settings", |ui| {
            ui.strong("Appearance");
            ui.horizontal_wrapped(|ui| {
                changed |= stationdeck_ui::test_ui::response(
                    "theme-dark",
                    ui.selectable_value(&mut self.settings.theme, Theme::Dark, "Dark"),
                )
                .changed();
                changed |= stationdeck_ui::test_ui::response(
                    "theme-light",
                    ui.selectable_value(&mut self.settings.theme, Theme::Light, "Light"),
                )
                .changed();
            });
            ui.add_space(12.0);
            ui.label("Interface size");
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.settings.zoom, 0.9..=1.4)
                        .step_by(0.05)
                        .suffix(" ×"),
                )
                .changed();
            ui.add_space(16.0);
            if ui.button("Reset appearance").clicked() {
                self.settings.theme = Theme::Dark;
                self.settings.zoom = 1.0;
                changed = true;
            }
            ui.add_space(16.0);
            ui.weak(
                "Preferences are saved locally in this browser. Access tokens are never saved.",
            );
        });
        if changed {
            self.preferences_dirty = true;
            self.apply_style(ctx);
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl eframe::App for StationDeck {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.render(ctx);
        if self.preferences_dirty && !self.settings_error {
            self.persist_preferences();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(app: &mut StationDeck, ctx: &egui::Context, size: egui::Vec2) {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            },
            |ctx| app.render(ctx),
        );
    }
    #[test]
    fn preferences_reject_invalid_window_geometry() {
        let mut settings = Settings::default();
        assert!(settings.valid());
        settings.windows.insert(
            "demo".into(),
            PluginWindow {
                position: Some([f32::NAN, 0.0]),
                ..Default::default()
            },
        );
        assert!(!settings.valid());
        settings.windows.clear();
        settings.version = 2;
        assert!(!settings.valid());
    }
    #[test]
    fn plugins_coexist_and_closing_a_window_does_not_stop_its_module() {
        let ctx = egui::Context::default();
        let mut app = StationDeck::with_settings(Settings::default(), false);
        frame(&mut app, &ctx, egui::vec2(1440.0, 1000.0));
        assert!(app.settings.windows["demo"].open);
        assert!(app.settings.windows["overview"].open);
        app.settings.windows.get_mut("demo").unwrap().open = false;
        frame(&mut app, &ctx, egui::vec2(1440.0, 1000.0));
        assert!(!app.settings.windows["demo"].open);
        assert!(app.modules.is_enabled("demo"));
        app.open_view("demo.tasks");
        assert!(app.settings.windows["demo"].open);
        assert_eq!(app.settings.windows["demo"].view, "demo.tasks");
    }
    #[test]
    fn disabled_plugins_are_not_rendered_and_geometry_remains_finite() {
        let ctx = egui::Context::default();
        let mut app = StationDeck::with_settings(Settings::default(), false);
        for size in [egui::vec2(1440.0, 1000.0), egui::vec2(600.0, 800.0)] {
            for _ in 0..4 {
                frame(&mut app, &ctx, size);
            }
            assert!(app.settings.valid());
        }
        app.modules.set_enabled("demo", false).unwrap();
        frame(&mut app, &ctx, egui::vec2(1440.0, 1000.0));
        assert!(app.modules.views().is_empty());
        app.arrange();
        assert!(app
            .settings
            .windows
            .values()
            .all(|w| w.position.is_none() && w.size.is_none()));
    }
    #[test]
    fn saved_theme_overrides_the_browser_theme() {
        let ctx = egui::Context::default();
        ctx.set_theme(egui::ThemePreference::Light);
        let app = StationDeck::with_settings(Settings::default(), false);
        app.apply_style(&ctx);
        assert!(ctx.style().visuals.dark_mode);
    }
}
