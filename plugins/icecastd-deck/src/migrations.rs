//! Unified stream continuity desk. Only backend snapshots drive live indicators.
use super::*;
use egui::{Color32, RichText, Stroke};
use stationdeck_services::icecast::{IcecastCommand as Command, ListenerMigration};
const ACCENT: Color32 = Color32::from_rgb(62, 211, 190);
fn surface(ui: &egui::Ui, active: bool) -> egui::Frame {
    egui::Frame::new()
        .inner_margin(18.0)
        .corner_radius(12.0)
        .fill(if ui.visuals().dark_mode {
            if active {
                Color32::from_rgb(19, 48, 49)
            } else {
                Color32::from_rgb(26, 34, 45)
            }
        } else {
            Color32::from_rgb(239, 247, 248)
        })
        .stroke(Stroke::new(
            1.0_f32,
            if active {
                ACCENT
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke.color
            },
        ))
}
fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.13))
        .corner_radius(5.0)
        .inner_margin(egui::vec2(9.0, 4.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(color));
        });
}
fn endpoint(
    ui: &mut egui::Ui,
    status: &IcecastStatus,
    selected: &mut String,
    origin: bool,
    exclude: &str,
    enabled: bool,
) -> egui::Rect {
    let prefix = if origin {
        "migration-source"
    } else {
        "migration-destination"
    };
    surface(ui, false)
        .show(ui, |ui| {
            ui.set_min_width((ui.available_width() - 4.0).max(100.0));
            ui.set_min_height(172.0);
            ui.label(
                RichText::new(if origin {
                    "01  /  ORIGIN"
                } else {
                    "02  /  TAKEOVER TARGET"
                })
                .small()
                .strong()
                .color(ACCENT),
            );
            ui.add_space(10.0);
            let combo = ui
                .add_enabled_ui(enabled, |ui| {
                    egui::ComboBox::from_id_salt(prefix)
                        .width((ui.available_width() - 12.0).max(80.0))
                        .selected_text(RichText::new(selected.as_str()).size(20.0).strong())
                        .show_ui(ui, |ui| {
                            for stream in &status.streams {
                                if !origin && stream.mount == exclude {
                                    continue;
                                }
                                stationdeck_ui::test_ui::response(
                                    &format!("{prefix}-{}", stream.mount.replace('/', "_")),
                                    ui.selectable_value(
                                        selected,
                                        stream.mount.clone(),
                                        &stream.mount,
                                    ),
                                );
                            }
                        })
                })
                .inner;
            stationdeck_ui::test_ui::control(prefix, &combo.response);
            ui.add_space(10.0);
            if let Some(stream) = status.streams.iter().find(|s| s.mount == *selected) {
                badge(
                    ui,
                    if stream.connected {
                        "SOURCE ONLINE"
                    } else {
                        "SOURCE OFFLINE"
                    },
                    if stream.connected {
                        ACCENT
                    } else {
                        Color32::from_rgb(237, 174, 78)
                    },
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(stream.listeners.to_string())
                            .size(26.0)
                            .strong(),
                    );
                    ui.weak("connections");
                });
                ui.label(if stream.title.is_empty() {
                    "No title supplied"
                } else {
                    &stream.title
                });
            } else {
                ui.weak("Select a configured stream");
            }
        })
        .response
        .rect
}
impl IcecastPlugin {
    pub(super) fn migrations_ui(&mut self, ui: &mut egui::Ui, status: &IcecastStatus) {
        let pending = matches!(self.control_state(), Ok(RequestState::Pending));
        let busy = self
            .migrations
            .iter()
            .any(|op| matches!(op.state.as_str(), "moving" | "returning"));
        let enabled = self.can_control() && !pending && !busy;
        if self.migration_poll <= 0.0 { self.migration_poll = ui.input(|i| i.time) + 5.0; }
        stationdeck_ui::test_ui::value("routing-controls-ready", if !pending {"true"} else {"false"});
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.heading(RichText::new("Stream continuity").size(25.0));
            badge(ui, "ROUTING DESK", ACCENT);
        });
        ui.label("Keep a station listening while its VM is under maintenance.");
        ui.add_space(12.0);
        let active = self
            .migrations
            .iter()
            .filter(|op| op.continuous && op.state == "away")
            .count();
        ui.horizontal_wrapped(|ui| {
            badge(
                ui,
                &format!(
                    "{active} ACTIVE ROUTE{}",
                    if active == 1 { "" } else { "S" }
                ),
                if active > 0 {
                    ACCENT
                } else {
                    ui.visuals().weak_text_color()
                },
            );
            ui.weak("Saved on server  /  Other stations stay on their own route");
        });
        if !status
            .streams
            .iter()
            .any(|s| s.mount == self.migration_source)
        {
            self.migration_source = status
                .streams
                .iter()
                .find(|s| s.mount == self.selected_mount)
                .or_else(|| status.streams.first())
                .map(|s| s.mount.clone())
                .unwrap_or_default();
        }
        if self.migration_destination == self.migration_source
            || !status
                .streams
                .iter()
                .any(|s| s.mount == self.migration_destination)
        {
            self.migration_destination = status
                .streams
                .iter()
                .find(|s| s.mount != self.migration_source && s.connected)
                .map(|s| s.mount.clone())
                .unwrap_or_default();
        }
        ui.add_space(14.0);
        let linked = self.migrations.iter().any(|op| {
            op.continuous
                && op.state == "away"
                && op.mount == self.migration_source
                && op.destination == self.migration_destination
        });
        let width = ui.available_width();
        let origin = self.migration_source.clone();
        if width >= 660.0 {
            let card = (width - 100.0) * 0.5;
            let (from, to) = ui
                .horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let from = ui
                        .allocate_ui_with_layout(
                            egui::vec2(card, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                endpoint(ui, status, &mut self.migration_source, true, "", enabled)
                            },
                        )
                        .inner;
                    ui.add_space(100.0);
                    let to = ui
                        .allocate_ui_with_layout(
                            egui::vec2(card, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                endpoint(
                                    ui,
                                    status,
                                    &mut self.migration_destination,
                                    false,
                                    &origin,
                                    enabled,
                                )
                            },
                        )
                        .inner;
                    (from, to)
                })
                .inner;
            let start = from.right_center();
            let end = to.left_center();
            let color = if linked {
                ACCENT
            } else {
                ui.visuals().weak_text_color()
            };
            ui.painter()
                .line_segment([start, end], Stroke::new(7.0_f32, color.gamma_multiply(0.12)));
            ui.painter()
                .line_segment([start, end], Stroke::new(2.0_f32, color));
            let center = start.lerp(end, 0.5);
            ui.painter()
                .circle_filled(center, 15.0, ui.visuals().panel_fill);
            ui.painter().text(
                center,
                egui::Align2::CENTER_CENTER,
                "→",
                egui::FontId::proportional(25.0),
                color,
            );
            ui.painter().text(
                center + egui::vec2(0.0, 32.0),
                egui::Align2::CENTER_CENTER,
                if linked { "ACTIVE" } else { "PREVIEW" },
                egui::FontId::proportional(10.0),
                color,
            );
        } else {
            endpoint(ui, status, &mut self.migration_source, true, "", enabled);
            ui.label(RichText::new("↓  TAKEOVER").color(ACCENT));
            endpoint(
                ui,
                status,
                &mut self.migration_destination,
                false,
                &origin,
                enabled,
            );
        }
        ui.add_space(12.0);
        let ready = self.migration_source != self.migration_destination
            && [&self.migration_source, &self.migration_destination]
                .iter()
                .all(|mount| {
                    status
                        .streams
                        .iter()
                        .any(|s| s.mount == **mount && s.connected)
                });
        ui.horizontal_wrapped(|ui| {
            if stationdeck_ui::test_ui::response(
                "migration-plan",
                ui.add_enabled(
                    enabled && ready,
                    egui::Button::new(RichText::new("Reroute stream…").strong())
                        .min_size(egui::vec2(175.0, 38.0))
                        .fill(Color32::from_rgb(30, 112, 105)),
                ),
            )
            .clicked()
            {
                self.submit(Command::MigrationPlan {
                    mount: self.migration_source.clone(),
                    destination: self.migration_destination.clone(),
                    continuous: true,
                });
            }
            ui.weak("Works at zero listeners · Includes new connections");
            if stationdeck_ui::test_ui::response(
                "migration-refresh",
                ui.add_enabled(
                    self.can_control() && !pending,
                    egui::Button::new("Refresh routes"),
                ),
            )
            .clicked()
            {
                self.submit(Command::Migrations);
            }
        });
        ui.weak("Same listening URL, even with the origin VM offline. New arrivals go directly to the takeover target.");
        if !self.can_control() {
            ui.weak("Add the Icecast control token in Tokens to operate this desk.");
        }
        ui.add_space(16.0);
        for op in &self.migrations {
            stationdeck_ui::test_ui::value(&format!("migration-{}-state", op.id), &op.state);
            stationdeck_ui::test_ui::value(
                &format!("migration-{}-returned", op.id),
                &op.returned.to_string(),
            );
        }
        let open: Vec<_> = self
            .migrations
            .iter()
            .filter(|op| !matches!(op.state.as_str(), "complete" | "expired"))
            .cloned()
            .collect();
        if open.is_empty() {
            surface(ui,false).show(ui,|ui| {ui.strong("Ready for the next maintenance");ui.weak("Choose an origin and a takeover target above. Existing destination listeners remain in place.");});
        }
        for op in open {
            self.route_operation(ui, &op, enabled);
            ui.add_space(8.0);
        }
        if let Some(command) = self.migration_confirm.clone() {
            let (id, returning) = match command {
                Command::MigrationRun { id } => (id, false),
                Command::MigrationReturn { id } => (id, true),
                _ => return,
            };
            if let Some(op) = self.migrations.iter().find(|op| op.id == id).cloned() {
                surface(ui,true).show(ui,|ui| {
                    ui.heading(if returning {"Restore origin?"} else {"Activate stream rerouting?"});
                    ui.label(format!("{}  →  {}",if returning {&op.destination} else {&op.mount},if returning {&op.mount} else {&op.destination}));
                    ui.label(if returning {"Stops automatic forwarding and returns this route's connected listeners. Native target listeners stay in place."} else {"Arms the route even with no listeners. Present and future origin connections are transferred to the target."});
                    ui.horizontal(|ui| {
                        if stationdeck_ui::test_ui::response("migration-confirm",ui.add_enabled(enabled,egui::Button::new(if returning {"Restore origin"} else {"Activate route"}).min_size(egui::vec2(145.0,34.0)))).clicked() {self.migration_confirm=None;self.submit(command);}
                        if stationdeck_ui::test_ui::response("migration-dismiss",ui.button("Cancel")).clicked() {self.migration_confirm=None;}
                    });
                });
            }
        }
        let history: Vec<_> = self
            .migrations
            .iter()
            .filter(|op| matches!(op.state.as_str(), "complete" | "expired"))
            .cloned()
            .collect();
        if !history.is_empty() {
            egui::CollapsingHeader::new(format!("Route history · {}", history.len()))
                .id_salt("routing-history")
                .show(ui, |ui| {
                    for op in history {
                        self.route_operation(ui, &op, false);
                        ui.add_space(6.0);
                    }
                });
        }
        let time = ui.input(|i| i.time);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
        if (busy || self.migrations.iter().any(|op| op.state == "away"))
            && !pending
            && self.can_control()
            && self.migration_confirm.is_none()
            && time >= self.migration_poll
        {
            self.migration_poll = time + if busy { 1.0 } else { 5.0 };
            self.submit(Command::Migrations);
        }
    }
    fn route_operation(&mut self, ui: &mut egui::Ui, op: &ListenerMigration, enabled: bool) {
        let active = op.continuous && op.state == "away";
        surface(ui, active).show(ui, |ui| {
            let label = match op.state.as_str() {
                "planned" => "READY TO ACTIVATE",
                "away" if op.continuous => "REROUTING",
                "away" => "MOVED · LEGACY GROUP",
                "moving" => "SWITCHING",
                "returning" => "RESTORING",
                "interrupted" => "CHECK ROUTE STATE",
                "complete" => "RESTORED",
                _ => "EXPIRED",
            };
            ui.horizontal_wrapped(|ui| {
                badge(
                    ui,
                    label,
                    if matches!(op.state.as_str(), "interrupted" | "expired") {
                        Color32::from_rgb(237, 174, 78)
                    } else {
                        ACCENT
                    },
                );
                ui.strong(format!("{}  →  {}", op.mount, op.destination));
            });
            ui.add_space(7.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} tracked", op.total));
                ui.separator();
                ui.label(format!("{} transferred", op.moved));
                ui.separator();
                ui.label(format!("{} returned", op.returned));
                if op.missing > 0 {
                    ui.separator();
                    ui.weak(format!("{} disconnected / unmoved", op.missing));
                }
                if op.uncertain > 0 {
                    ui.colored_label(
                        Color32::from_rgb(237, 174, 78),
                        format!("{} uncertain", op.uncertain),
                    );
                }
            });
            ui.weak(&op.note);
            ui.horizontal_wrapped(|ui| {
                if op.state == "planned" {
                    if stationdeck_ui::test_ui::response(
                        &format!("migration-run-{}", op.id),
                        ui.add_enabled(
                            enabled && (op.continuous || op.total > 0),
                            egui::Button::new("Review activation…"),
                        ),
                    )
                    .clicked()
                    {
                        self.migration_confirm = Some(Command::MigrationRun { id: op.id });
                    }
                    if stationdeck_ui::test_ui::response(
                        &format!("migration-cancel-{}", op.id),
                        ui.add_enabled(enabled, egui::Button::new("Discard")),
                    )
                    .clicked()
                    {
                        self.submit(Command::MigrationCancel { id: op.id });
                    }
                }
                if op.state == "interrupted"
                    && op.returned == 0
                    && stationdeck_ui::test_ui::response(
                        &format!("migration-resume-{}", op.id),
                        ui.add_enabled(enabled, egui::Button::new("Inspect & resume…")),
                    )
                    .clicked()
                {
                    self.migration_confirm = Some(Command::MigrationRun { id: op.id });
                }
                if matches!(op.state.as_str(), "away" | "interrupted")
                    && (op.continuous || op.moved + op.uncertain > 0)
                    && stationdeck_ui::test_ui::response(
                        &format!("migration-return-{}", op.id),
                        ui.add_enabled(
                            enabled,
                            egui::Button::new("Restore origin…").min_size(egui::vec2(145.0, 34.0)),
                        ),
                    )
                    .clicked()
                {
                    self.migration_confirm = Some(Command::MigrationReturn { id: op.id });
                }
            });
        });
    }
}
