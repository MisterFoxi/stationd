use egui::{Color32, Key, Modifiers, RichText};
use serde::{Deserialize, Serialize};
use stationdeck_ui::{show_state, ModuleCatalog, ViewState};

#[cfg(target_arch = "wasm32")]
const SETTINGS_KEY: &str = "stationdeck.settings.v1";
const ACCENT: Color32 = Color32::from_rgb(44, 169, 160);

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize)]
enum Theme {
    #[default]
    Dark,
    Light,
}

fn default_modules() -> Vec<String> {
    vec!["demo".into(), "overview".into()]
}

fn default_provider() -> String { "demo".into() }

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
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            theme: Theme::Dark,
            zoom: 1.0,
            enabled_modules: default_modules(),
            overview_provider: default_provider(),
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
    }
}

#[derive(Default, PartialEq)]
enum Page {
    #[default]
    Home,
    Modules,
    Settings,
    Contribution(String),
}

pub struct StationDeck {
    settings: Settings,
    page: Page,
    settings_error: bool,
    preferences_dirty: bool,
    persistence_error: bool,
    module_error: Option<String>,
    modules: ModuleCatalog,
}

impl StationDeck {
    fn with_settings(settings: Settings, settings_error: bool) -> Self {
        let modules = ModuleCatalog::new(crate::assembly::modules(), &settings.enabled_modules, crate::assembly::policy_with_provider(&settings.overview_provider));
        Self {
            settings,
            page: Page::Home,
            settings_error,
            preferences_dirty: false,
            persistence_error: false,
            module_error: None,
            modules,
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let storage = web_sys::window()
            .and_then(|window| window.local_storage().ok())
            .flatten();
        let stored = storage
            .as_ref()
            .and_then(|storage| storage.get_item(SETTINGS_KEY).ok())
            .flatten();
        let parsed = stored
            .as_deref()
            .and_then(|value| serde_json::from_str::<Settings>(value).ok());
        let invalid = stored.is_some() && !parsed.as_ref().is_some_and(Settings::valid);
        let mut app =
            Self::with_settings(parsed.filter(Settings::valid).unwrap_or_default(), invalid);
        app.persistence_error = storage.is_none();
        app.apply_style(&cc.egui_ctx);
        app
    }

    #[cfg(target_arch = "wasm32")]
    fn persist_preferences(&mut self) {
        let saved = (|| {
            let storage = web_sys::window()?.local_storage().ok()??;
            let text = serde_json::to_string(&self.settings).ok()?;
            storage.set_item(SETTINGS_KEY, &text).ok()
        })()
        .is_some();
        self.persistence_error = !saved;
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
        visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        if self.settings.theme == Theme::Dark {
            visuals.panel_fill = Color32::from_rgb(20, 28, 39);
            visuals.window_fill = Color32::from_rgb(25, 35, 47);
        }
        ctx.set_visuals(visuals);
        ctx.set_zoom_factor(self.settings.zoom);
        ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(10.0, 10.0);
            style.spacing.button_padding = egui::vec2(12.0, 8.0);
        });
    }

    fn render(&mut self, ctx: &egui::Context) {
        stationdeck_ui::test_ui::begin_frame();
        for descriptor in self.modules.descriptors() {
            if let Some((state, _)) = self.modules.status(descriptor.id) {
                stationdeck_ui::test_ui::value(&format!("state-{}", descriptor.id), &format!("{state:?}"));
            }
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::Comma)) {
            self.page = Page::Settings;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::Home)) {
            self.page = Page::Home;
        }
        egui::TopBottomPanel::top("stationdeck.header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("SD").strong().size(20.0).color(ACCENT));
                ui.heading("StationDeck");
                ui.separator();
                ui.label("Station workspace");
                ui.weak("WebUI");
            });
            ui.add_space(6.0);
        });
        egui::TopBottomPanel::bottom("stationdeck.status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("●").color(ACCENT));
                ui.label("Interface ready");
                ui.separator();
                ui.label(format!(
                    "Modules enabled: {}",
                    self.modules.enabled_ids().len()
                ));
                ui.separator();
                ui.label("Remote status requires gateway access");
                ui.separator();
                ui.weak(format!("v{}", env!("CARGO_PKG_VERSION")));
            });
        });
        egui::SidePanel::left("stationdeck.navigation")
            .default_width(178.0)
            .min_width(145.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.add_space(14.0);
                ui.weak("WORKSPACE");
                ui.add_space(8.0);
                for (label, page) in [
                    ("Home", Page::Home),
                    ("Modules", Page::Modules),
                    ("Settings", Page::Settings),
                ] {
                    let response = ui.selectable_label(self.page == page, label);
                    stationdeck_ui::test_ui::control(&format!("nav-{}", label.to_ascii_lowercase()), &response);
                    if response.clicked() {
                        self.page = page;
                    }
                }
                let views = self.modules.views();
                if !views.is_empty() {
                    ui.separator();
                    ui.weak("MODULE VIEWS");
                    for view in views {
                        ui.push_id(view.id, |ui| {
                            let response = ui.selectable_label(self.page == Page::Contribution(view.id.into()), view.title);
                            stationdeck_ui::test_ui::control(&format!("nav-{}", view.id), &response);
                            if response.clicked() {
                                self.page = Page::Contribution(view.id.into());
                            }
                        });
                    }
                }
            });
        if let Page::Contribution(id) = &self.page {
            if !self.modules.views().iter().any(|view| view.id == id) {
                self.page = Page::Home;
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("stationdeck.content").show(ui, |ui| {
                ui.add_space(18.0);
                if self.settings_error {
                    show_state(ui, ViewState::Error("Saved preferences are invalid or from a newer version. Temporary defaults are being used."));
                    if ui.button("Replace saved preferences").clicked() { self.settings_error = false; self.preferences_dirty = true; }
                }
                if self.persistence_error {
                    show_state(ui, ViewState::Error("Preferences cannot be saved in this browser. Changes are temporary."));
                    if ui.button("Retry saving").clicked() { self.preferences_dirty = true; }
                }
                if let Some(error) = &self.module_error { show_state(ui, ViewState::Error(error)); }
                match &self.page {
                    Page::Home => self.home(ui),
                    Page::Modules => self.module_settings(ui),
                    Page::Settings => self.preferences(ui, ctx),
                    Page::Contribution(id) => { self.modules.render(id, ui); }
                }
            });
        });
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        ui.weak("YOUR STATION WORKSPACE");
        ui.add_space(8.0);
        ui.label(RichText::new("Welcome to StationDeck").size(28.0).strong());
        ui.label("Choose a module from the sidebar. The test module works without a server.");
        ui.add_space(22.0);
        if self.modules.enabled_ids().is_empty() {
            show_state(
                ui,
                ViewState::Empty(
                    "No module is enabled. The workspace remains available without a server.",
                ),
            );
        } else {
            ui.label("Modules are enabled locally. This does not establish a remote connection or grant station permissions.");
        }
        ui.add_space(12.0);
        show_state(ui, ViewState::Unavailable("Remote station data is available only after a successful connection in the gRPC module."));
        ui.add_space(18.0);
        if ui.button("Manage modules").clicked() {
            self.page = Page::Modules;
        }
        if ui.button("Customize interface").clicked() {
            self.page = Page::Settings;
        }
        ui.add_space(22.0);
        ui.weak("Ctrl+Home  Home    ·    Ctrl+,  Settings");
    }

    fn module_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Modules");
        ui.label("Enable trusted modules compiled into this application.");
        let previous = self.settings.overview_provider.clone();
        let provider = egui::ComboBox::from_id_salt("overview.provider")
            .selected_text(if previous == "grpc" { "gRPC" } else { "Demo" })
            .show_ui(ui, |ui| {
                let _ = stationdeck_ui::test_ui::response("provider-demo", ui.selectable_value(&mut self.settings.overview_provider, "demo".into(), "Demo"));
                let _ = stationdeck_ui::test_ui::response("provider-grpc", ui.selectable_value(&mut self.settings.overview_provider, "grpc".into(), "gRPC"));
            });
        stationdeck_ui::test_ui::control("provider", &provider.response);
        ui.weak("Station overview provider. Enable the selected provider and the Station overview module.");
        if previous != self.settings.overview_provider {
            if let Err(error) = self.modules.set_binding("overview", "stationdeck.admin.overview", &self.settings.overview_provider) {
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
                if let Some((state, diagnostic)) = self.modules.status(descriptor.id).map(|(state, diagnostic)| (state, diagnostic.cloned())) {
                    ui.label(format!("{state:?}"));
                    if enabled && matches!(state, stationdeck_services::registry::State::Blocked | stationdeck_services::registry::State::Failed) && ui.button("Retry startup").clicked() {
                        let _ = self.modules.set_enabled(descriptor.id, true);
                    }
                    if let Some(diagnostic) = diagnostic {
                        show_state(ui, ViewState::Error(&format!("{} ({})", diagnostic.message, diagnostic.chain.join(" → "))));
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
                changed |= stationdeck_ui::test_ui::response("theme-dark", ui.selectable_value(&mut self.settings.theme, Theme::Dark, "Dark")).changed();
                changed |= stationdeck_ui::test_ui::response("theme-light", ui.selectable_value(&mut self.settings.theme, Theme::Light, "Light")).changed();
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
    #[test]
    fn future_or_invalid_preferences_are_rejected() {
        assert!(Settings::default().valid());
        for (version, zoom) in [(2, 1.0), (1, f32::NAN), (1, 10.0)] {
            assert!(!Settings {
                version,
                zoom,
                ..Settings::default()
            }
            .valid());
        }
    }
    #[test]
    fn missing_view_returns_home_without_changing_preferences() {
        let ctx = egui::Context::default();
        let mut app = StationDeck::with_settings(Settings::default(), false);
        app.page = Page::Contribution("gone.view".into());
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.render(ctx));
        assert!(app.page == Page::Home);
        assert!(!app.preferences_dirty);
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
