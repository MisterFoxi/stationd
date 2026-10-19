//! Painted routing diagram and ordinary egui controls; all values come from snapshots.
use super::*;
use egui::{Color32, RichText, Stroke};
use stationdeck_services::icecast::{IcecastCommand as Command, RouteStatus};
use stationdeck_ui::{status_dot, StatusTone};
const TEAL: Color32 = Color32::from_rgb(57, 189, 176);
fn panel(ui: &egui::Ui, active: bool) -> egui::Frame {
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .inner_margin(16.0)
        .corner_radius(7.0)
        .fill(if active {
            if dark {
                Color32::from_rgb(25, 52, 43)
            } else {
                Color32::from_rgb(224, 245, 232)
            }
        } else if dark {
            Color32::from_rgb(28, 36, 44)
        } else {
            Color32::from_rgb(244, 247, 250)
        })
        .stroke(Stroke::new(
            if active { 1.5_f32 } else { 1.0_f32 },
            if active {
                StatusTone::Good.color(ui)
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke.color
            },
        ))
}
fn state(ui: &mut egui::Ui, tone: StatusTone, label: &str) {
    ui.horizontal(|ui| {
        status_dot(ui, tone);
        ui.label(RichText::new(label).small().color(tone.color(ui)));
    });
}
impl IcecastPlugin {
    pub(super) fn routing_desk(
        &mut self,
        ui: &mut egui::Ui,
        status: &IcecastStatus,
        route: &RouteStatus,
        enabled: bool,
    ) {
        let width = ui.available_width();
        if width >= 650.0 {
            let left = (width - 64.0) * 0.52;
            let right = (width - 64.0) - left;
            let mut sources = vec![];
            let output = ui
                .horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(left, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(left);
                            for input in &route.inputs {
                                let rect = self.source_card(ui, status, route, input, enabled);
                                sources.push((rect, route.input == *input));
                                ui.add_space(10.0);
                            }
                        },
                    );
                    ui.add_space(64.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(right, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(right);
                            self.output_card(ui, status, route)
                        },
                    )
                    .inner
                })
                .inner;
            let join = egui::pos2(output.left() - 24.0, output.center().y);
            for (source, selected) in sources {
                let color = if selected {
                    if route.connected {
                        StatusTone::Good.color(ui)
                    } else {
                        StatusTone::Warning.color(ui)
                    }
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                };
                let start = source.right_center();
                let elbow = egui::pos2(join.x - 12.0, start.y);
                let points = vec![
                    start,
                    elbow,
                    egui::pos2(elbow.x, join.y),
                    join,
                    output.left_center(),
                ];
                ui.painter().add(egui::Shape::line(
                    points,
                    Stroke::new(if selected { 2.0_f32 } else { 1.0_f32 }, color),
                ));
            }
            ui.painter().circle_filled(join, 5.0, TEAL);
            ui.painter().arrow(
                output.left_center() - egui::vec2(16.0, 0.0),
                egui::vec2(16.0, 0.0),
                Stroke::new(2.0_f32, TEAL),
            );
        } else {
            for input in &route.inputs {
                self.source_card(ui, status, route, input, enabled);
                ui.add_space(8.0);
            }
            ui.weak(format!(
                "Selected input {}  >  Program output",
                route.input.to_uppercase()
            ));
            self.output_card(ui, status, route);
        }
    }
    fn source_card(
        &mut self,
        ui: &mut egui::Ui,
        status: &IcecastStatus,
        route: &RouteStatus,
        input: &str,
        enabled: bool,
    ) -> egui::Rect {
        let selected = route.input == input;

        let mount = route
            .input_streams
            .iter()
            .find(|source| source.id == input)
            .map(|s| s.mount.as_str());
        let stream = mount.and_then(|mount| status.streams.iter().find(|s| s.mount == mount));
        let active = selected && route.connected && stream.is_none_or(|source| source.connected);
        let frame = ui
            .push_id(("source", input), |ui| {
                panel(ui, active).show(ui, |ui| {
                    ui.set_width(ui.available_width().max(100.0));
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);
                    ui.horizontal_wrapped(|ui| {
                        egui::Frame::new()
                            .inner_margin(6.0)
                            .corner_radius(5.0)
                            .stroke(Stroke::new(
                                1.0_f32,
                                if active {
                                    StatusTone::Good.color(ui)
                                } else {
                                    TEAL
                                },
                            ))
                            .show(ui, |ui| {
                                ui.label(RichText::new(input.to_uppercase()).size(20.0).strong());
                            });
                        ui.vertical(|ui| {
                            ui.weak(format!("SOURCE {}", input.to_uppercase()));
                            ui.label(
                                RichText::new(mount.unwrap_or("Mount not provided"))
                                    .strong()
                                    .size(16.0),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    if let Some(stream) = stream {
                        ui.label(if stream.title.is_empty() {
                            "No title provided"
                        } else {
                            &stream.title
                        });
                        if !stream.content_type.is_empty() {
                            ui.weak(&stream.content_type);
                        }
                    }
                    let (tone, label) = if active {
                        (StatusTone::Good, "ON AIR")
                    } else if selected {
                        (StatusTone::Warning, "Selected · disconnected")
                    } else {
                        match stream {
                            Some(s) if s.connected => (StatusTone::Good, "Ready"),
                            Some(_) => (StatusTone::Warning, "Source disconnected"),
                            None => (StatusTone::Inactive, "Availability not reported"),
                        }
                    };

                    stationdeck_ui::test_ui::value(&format!("icecast-source-{input}-state"), label);
                    stationdeck_ui::test_ui::value(
                        &format!("icecast-source-{input}-mount"),
                        mount.unwrap_or(""),
                    );
                    let available = stream.is_none_or(|s| s.connected);
                    ui.horizontal_wrapped(|ui| {
                        state(ui, tone, label);
                        if stationdeck_ui::test_ui::response(
                            &format!("icecast-take-{input}"),
                            ui.add_enabled(
                                enabled && !selected && available,
                                egui::Button::new(if selected {
                                    "Selected"
                                } else {
                                    "Take on air"
                                })
                                .stroke(Stroke::new(1.0_f32, TEAL)),
                            ),
                        )
                        .clicked()
                        {
                            self.submit(Command::Switch {
                                input: input.into(),
                            });
                        }
                    });
                })
            })
            .inner;
        stationdeck_ui::test_ui::control(&format!("icecast-source-{input}"), &frame.response);
        frame.response.rect
    }
    fn output_card(
        &self,
        ui: &mut egui::Ui,
        status: &IcecastStatus,
        route: &RouteStatus,
    ) -> egui::Rect {
        let output = status
            .streams
            .iter()
            .find(|s| s.mount == route.output_mount);
        let connected = route.connected && output.is_none_or(|stream| stream.connected);
        let response = panel(ui, false).show(ui, |ui| {
            ui.set_width(ui.available_width().max(100.0));
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);
            ui.set_min_height(218.0);
            ui.weak("PROGRAM OUTPUT");
            ui.add_space(12.0);
            ui.label(RichText::new(&route.output_mount).size(25.0).strong());
            ui.add_space(6.0);
            state(
                ui,
                if connected {
                    StatusTone::Good
                } else {
                    StatusTone::Warning
                },
                if connected {
                    "Connected"
                } else {
                    "Disconnected"
                },
            );
            if let Some(output) = output {
                ui.label(if output.title.is_empty() {
                    "No title provided"
                } else {
                    &output.title
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(output.listeners.to_string())
                            .size(35.0)
                            .strong(),
                    );
                    ui.weak("listeners · output snapshot");
                });
            } else {
                ui.weak("Output audience not available in this snapshot");
            }
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("Input {} selected", route.input.to_uppercase())).color(TEAL),
            );
            if !route.problem.is_empty() {
                ui.colored_label(StatusTone::Warning.color(ui), &route.problem);
            }
            stationdeck_ui::test_ui::value("icecast-output-input", &route.input);
            stationdeck_ui::test_ui::value(
                "icecast-output-connected",
                if connected { "true" } else { "false" },
            );
        });
        stationdeck_ui::test_ui::control("icecast-output", &response.response);
        response.response.rect
    }
    pub(super) fn stream_details(
        &mut self,
        ui: &mut egui::Ui,
        status: &IcecastStatus,
        enabled: bool,
    ) {
        if !status
            .streams
            .iter()
            .any(|s| s.mount == self.selected_mount)
        {
            self.selected_mount = status
                .streams
                .first()
                .map(|s| s.mount.clone())
                .unwrap_or_default();
            self.title = status
                .streams
                .first()
                .map(|s| s.title.clone())
                .unwrap_or_default();
        }
        if ui.available_width() >= 650.0 {
            let width = (ui.available_width() - 14.0) / 2.0;
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        self.metadata_card(ui, status, enabled);
                    },
                );
                ui.add_space(4.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        self.listener_card(ui, status, enabled);
                    },
                );
            });
        } else {
            self.metadata_card(ui, status, enabled);
            ui.add_space(10.0);
            self.listener_card(ui, status, enabled);
        }
        if self.show_listeners {
            self.listener_rows(ui, enabled);
        }
    }
    fn metadata_card(&mut self, ui: &mut egui::Ui, status: &IcecastStatus, enabled: bool) {
        panel(ui, false).show(ui, |ui| {
            ui.set_width(ui.available_width().max(100.0));
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);
            ui.strong("Stream details");
            ui.add_space(6.0);
            let previous = self.selected_mount.clone();
            ui.add_enabled_ui(enabled, |ui| {
                let combo = egui::ComboBox::from_id_salt("management.mount")
                    .width((ui.available_width() - 10.0).max(90.0))
                    .selected_text(&self.selected_mount)
                    .show_ui(ui, |ui| {
                        for stream in &status.streams {
                            stationdeck_ui::test_ui::response(
                                &format!("icecast-mount-{}", stream.mount),
                                ui.selectable_value(
                                    &mut self.selected_mount,
                                    stream.mount.clone(),
                                    &stream.mount,
                                ),
                            );
                        }
                    });
                stationdeck_ui::test_ui::control("icecast-mount", &combo.response);
            });
            if previous != self.selected_mount {
                self.listeners = None;
                self.confirmation = None;
                self.edit_metadata = false;
                self.title = status
                    .streams
                    .iter()
                    .find(|s| s.mount == self.selected_mount)
                    .map(|s| s.title.clone())
                    .unwrap_or_default();
            }
            let stream = status
                .streams
                .iter()
                .find(|s| s.mount == self.selected_mount);
            let connected = stream.is_some_and(|s| s.connected);
            ui.weak("Current title");
            if self.edit_metadata {
                stationdeck_ui::test_ui::response(
                    "icecast-title",
                    ui.add(
                        egui::TextEdit::singleline(&mut self.title)
                            .desired_width((ui.available_width() - 8.0).max(80.0))
                            .char_limit(1024),
                    ),
                );
                ui.horizontal_wrapped(|ui| {
                    if stationdeck_ui::test_ui::response(
                        "icecast-set-title",
                        ui.add_enabled(enabled && connected, egui::Button::new("Save title")),
                    )
                    .clicked()
                    {
                        self.submit(Command::Metadata {
                            mount: self.selected_mount.clone(),
                            title: self.title.clone(),
                        });
                    }
                    if ui.button("Cancel edit").clicked() {
                        self.edit_metadata = false;
                    }
                });
            } else {
                ui.label(
                    stream
                        .map(|s| s.title.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("No title provided"),
                );
                if stationdeck_ui::test_ui::response(
                    "icecast-edit-title",
                    ui.add_enabled(enabled && connected, egui::Button::new("Edit metadata")),
                )
                .clicked()
                {
                    self.title = stream.map(|s| s.title.clone()).unwrap_or_default();
                    self.edit_metadata = true;
                }
            }
            ui.add_space(6.0);
            let menu = ui.menu_button("Advanced actions", |ui| {
                if stationdeck_ui::test_ui::response(
                    "icecast-kick-source",
                    ui.add_enabled(
                        enabled && connected,
                        egui::Button::new("Disconnect source…"),
                    ),
                )
                .clicked()
                {
                    self.confirmation = Some(Command::KickSource {
                        mount: self.selected_mount.clone(),
                    });
                    ui.close();
                }
            });
            stationdeck_ui::test_ui::control("icecast-advanced", &menu.response);
        });
    }
    fn listener_card(&mut self, ui: &mut egui::Ui, status: &IcecastStatus, enabled: bool) {
        let stream = status
            .streams
            .iter()
            .find(|s| s.mount == self.selected_mount);
        panel(ui, false).show(ui, |ui| {
            ui.set_width(ui.available_width().max(100.0));
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);
            ui.strong("Listeners");
            ui.weak(&self.selected_mount);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(
                        stream
                            .map(|s| s.listeners.to_string())
                            .unwrap_or_else(|| "—".into()),
                    )
                    .size(32.0)
                    .strong(),
                );
                ui.weak("stream snapshot");
            });
            if stream.is_some_and(|s| s.connected && s.listeners == 0) {
                ui.weak("No listeners connected");
            }
            if stationdeck_ui::test_ui::response(
                "icecast-listeners",
                ui.add_enabled(
                    enabled && stream.is_some_and(|s| s.connected),
                    egui::Button::new("Refresh listener list"),
                ),
            )
            .clicked()
            {
                self.submit(Command::Listeners {
                    mount: self.selected_mount.clone(),
                });
            }
            if self.listeners.is_some()
                && ui
                    .selectable_label(self.show_listeners, "Listener details")
                    .clicked()
            {
                self.show_listeners = !self.show_listeners;
            }
        });
    }
    fn listener_rows(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let Some((mount, listeners)) = self.listeners.clone() else {
            return;
        };
        ui.add_space(8.0);
        egui::CollapsingHeader::new(format!("Listener snapshot · {} · {mount}", listeners.len()))
            .default_open(true)
            .show(ui, |ui| {
                if listeners.is_empty() {
                    ui.weak("No listeners in this snapshot.");
                }
                egui::ScrollArea::both()
                    .id_salt("management.listeners")
                    .max_height(190.0)
                    .show(ui, |ui| {
                        egui::Grid::new("management.listener.grid")
                            .striped(true)
                            .show(ui, |ui| {
                                for heading in ["ID", "Address", "Connected", "Client", "Action"] {
                                    ui.strong(heading);
                                }
                                ui.end_row();
                                for listener in listeners {
                                    ui.label(listener.id.to_string());
                                    ui.label(listener.ip);
                                    ui.label(format!("{} s", listener.connected_seconds));
                                    ui.label(listener.user_agent);
                                    if stationdeck_ui::test_ui::response(
                                        &format!("icecast-kick-listener-{}", listener.id),
                                        ui.add_enabled(enabled, egui::Button::new("Disconnect…")),
                                    )
                                    .clicked()
                                    {
                                        self.confirmation = Some(Command::KickListener {
                                            mount: mount.clone(),
                                            id: listener.id,
                                        });
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            });
    }
    pub(super) fn disconnect_confirmation(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let Some(command) = self.confirmation.clone() else {
            return;
        };
        let target = match &command {
            Command::KickSource { mount } => {
                format!("Disconnect source {mount}? Its listeners may be interrupted.")
            }
            Command::KickListener { mount, id } => {
                format!("Disconnect listener {id} from {mount}?")
            }
            _ => return,
        };
        ui.add_space(10.0);
        egui::Frame::group(ui.style())
            .stroke(Stroke::new(1.0_f32, StatusTone::Warning.color(ui)))
            .show(ui, |ui| {
                ui.label(target);
                ui.horizontal_wrapped(|ui| {
                    if stationdeck_ui::test_ui::response(
                        "icecast-confirm",
                        ui.add_enabled(enabled, egui::Button::new("Confirm disconnect")),
                    )
                    .clicked()
                    {
                        self.confirmation = None;
                        self.submit(command);
                    }
                    if stationdeck_ui::test_ui::response("icecast-cancel", ui.button("Cancel"))
                        .clicked()
                    {
                        self.confirmation = None;
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_cards_fit_wide_and_narrow_windows_with_unknown_or_disconnected_sources() {
        let ctx = egui::Context::default();
        for width in [350.0, 940.0] {
            let mut plugin = IcecastPlugin::default();
            let route = RouteStatus {
                output_mount: "/radio.mp3".into(),
                input: "b".into(),
                connected: false,
                inputs: vec!["a".into(), "b".into()],
                problem: "Input unavailable".into(),
                input_streams: vec![stationdeck_services::icecast::RouteInput {
                    id: "a".into(),
                    mount: "/missing.mp3".into(),
                }],
            };
            let status = IcecastStatus {
                server_id: "fixture".into(),
                streams: vec![],
            };
            for _ in 0..3 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 1800.0),
                        )),
                        ..Default::default()
                    },
                    |root| {
                        egui::CentralPanel::default().show_inside(root, |ui| {
                            let right = ui.max_rect().right();
                            plugin.routing_desk(ui, &status, &route, false);
                            plugin.stream_details(ui, &status, false);
                            assert!(
                                ui.min_rect().right() <= right + 1.0,
                                "desk overflow at {width}: {:?}",
                                ui.min_rect()
                            );
                        });
                    },
                );
            }
        }
    }
}
