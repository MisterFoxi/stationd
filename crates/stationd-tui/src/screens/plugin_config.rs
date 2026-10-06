//! Generic schema-driven configuration screen.
use super::next_owner;
use crate::{
    app::{AppEvent, Global},
    rpc,
    screen::{Availability, Screen},
    store::Store,
    style::Styles,
    tr,
};
use anyhow::Error;
use rat_salsa::{Control, SalsaContext};
use ratatui_core::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::Widget,
};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui_widgets::{
    block::Block,
    paragraph::{Paragraph, Wrap},
};
use stationd_proto::plugin::{
    PluginConfigResponse, PluginConfigUpdateRequest, PluginConfigValue, PluginTab,
};

pub struct ConfigEditor {
    name: String,
    tab: PluginTab,
    owner: u64,
    request: u64,
    target: String,
    selected: usize,
    data: Option<PluginConfigResponse>,
    edits: Vec<PluginConfigValue>,
    input: Option<String>,
    metadata_editor: Option<super::metadata_rules::Editor>,
    preview: Option<Vec<String>>,
    choice: usize,
    preview_scroll: u16,
    busy: bool,
    message: String,
}
impl ConfigEditor {
    pub fn new(name: String, tab: PluginTab) -> Self {
        Self {
            name,
            tab,
            owner: next_owner(),
            request: 0,
            target: String::new(),
            selected: 0,
            data: None,
            edits: vec![],
            input: None,
            metadata_editor: None,
            preview: None,
            choice: 0,
            preview_scroll: 0,
            busy: false,
            message: String::new(),
        }
    }
    fn request(&mut self, ctx: &mut Global, operation: u8) {
        if self.busy || self.target.is_empty() {
            return;
        }
        if operation == 0 {
            return self.read(ctx);
        }
        let Some(data) = self.data.as_ref() else {
            return;
        };
        let req = PluginConfigUpdateRequest {
            name: self.target.clone(),
            revision: data.revision.clone(),
            schema_revision: data.schema_revision.clone(),
            edits: self.edits.clone(),
            mode: (operation - 1) as i32,
        };
        self.busy = true;
        self.request += 1;
        let (owner, request, channel) = (self.owner, self.request, ctx.channel.clone());
        ctx.spawn_async(async move {
            let result = rpc::update_plugin_config(channel, req).await;
            Ok(Control::Event(AppEvent::PluginConfig(
                owner, request, operation, result,
            )))
        });
    }
    fn read(&mut self, ctx: &mut Global) {
        if let Availability::Unavailable(message) = self.availability(&ctx.store) {
            self.message = message;
            return;
        }
        if self.busy || self.target.is_empty() {
            return;
        }
        self.busy = true;
        self.request += 1;
        self.message.clear();
        let (owner, request, channel, target) = (
            self.owner,
            self.request,
            ctx.channel.clone(),
            self.target.clone(),
        );
        ctx.spawn_async(async move {
            let result = rpc::read_plugin_config(channel, target).await;
            Ok(Control::Event(AppEvent::PluginConfig(
                owner, request, 0, result,
            )))
        });
    }
    fn field(&self) -> Option<&stationd_proto::plugin::PluginConfigField> {
        self.data.as_ref()?.fields.get(self.selected)
    }
    fn value(&self, key: &str) -> Option<&PluginConfigValue> {
        self.edits
            .iter()
            .find(|v| v.key == key)
            .or_else(|| self.data.as_ref()?.values.iter().find(|v| v.key == key))
    }
    fn edit(&mut self, value: Option<String>) {
        let Some(field) = self.field() else {
            return;
        };
        let key = field.key.clone();
        self.edits.retain(|e| e.key != key);
        self.edits.push(PluginConfigValue {
            key,
            present: value.is_some(),
            value: value.unwrap_or_default(),
            redacted: false,
        });
        self.message = tr!("config-draft");
    }
    fn select_target(&mut self, ctx: &mut Global, forward: bool) {
        if !self.edits.is_empty() {
            self.message = tr!("config-draft-kept");
            return;
        }
        let mut names: Vec<_> = ctx
            .store
            .plugins
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        if self.name == "plugin-config" {
            names.push(super::metadata_rules::TARGET.into());
        }
        if names.is_empty() {
            return;
        }
        let index = names.iter().position(|n| n == &self.target).unwrap_or(0);
        self.target = names[if forward {
            (index + 1) % names.len()
        } else {
            (index + names.len() - 1) % names.len()
        }]
        .clone();
        self.data = None;
        self.selected = 0;
        self.read(ctx);
    }
    fn target_label(&self) -> String {
        if self.target == super::metadata_rules::TARGET { tr!("metadata-target") }
        else { clean(&self.target) }
    }
    fn max_choice(&self) -> usize {
        if self.target == super::metadata_rules::TARGET { 1 } else { 2 }
    }
    fn shown(&self, field: &stationd_proto::plugin::PluginConfigField) -> String {
        if field.kind == "metadata_rules" {
            let count = self.value(&field.key).filter(|v| v.present)
                .and_then(|v| serde_json::from_str::<Vec<serde_json::Value>>(&v.value).ok())
                .map_or(0, |v| v.len());
            return tr!("metadata-count", count = count);
        }
        match self.value(&field.key) {
            Some(v) if v.present && field.secret => "••••".into(),
            Some(v) if v.present => clean(&v.value),
            _ => tr!(
                "config-default",
                value = field
                    .default_value
                    .clone()
                    .unwrap_or_else(|| tr!("config-absent"))
            ),
        }
    }
}
fn tail(text: &str, max: usize) -> String {
    let mut width = 0;
    let mut chars = Vec::new();
    for c in text.chars().rev() {
        let next = Span::raw(c.to_string()).width();
        if width + next > max {
            break;
        }
        width += next;
        chars.push(c);
    }
    chars.into_iter().rev().collect()
}
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
impl Screen for ConfigEditor {
    fn title(&self) -> String {
        self.tab.title.clone()
    }
    fn plugin_tab_key(&self) -> Option<(String, String)> {
        Some((self.name.clone(), self.tab.id.clone()))
    }
    fn captures_text(&self) -> bool {
        self.input.is_some() || self.metadata_editor.is_some() || self.preview.is_some() || self.busy
    }
    fn availability(&self, store: &Store) -> Availability {
        match store
            .plugins
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|p| p.name == self.name)
        {
            Some(p) if p.state == "loaded" => Availability::Available,
            _ => Availability::Unavailable(tr!("config-manager-stopped")),
        }
    }
    fn enter(&mut self, ctx: &mut Global) -> Result<(), Error> {
        if self.target.is_empty() {
            self.target = ctx
                .store
                .plugins
                .value
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|p| p.configurable)
                .or_else(|| {
                    ctx.store
                        .plugins
                        .value
                        .as_deref()
                        .unwrap_or_default()
                        .first()
                })
                .map(|p| p.name.clone())
                .unwrap_or_default();
        }
        if self.data.is_none() && self.edits.is_empty() {
            self.read(ctx);
        }
        Ok(())
    }
    fn reconnected(&mut self, ctx: &mut Global) -> Result<(), Error> {
        if self.edits.is_empty() && self.input.is_none() && self.metadata_editor.is_none() && self.preview.is_none() {
            self.read(ctx);
        }
        Ok(())
    }
    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        match event {
            AppEvent::PluginConfig(owner, request, operation, result)
                if *owner == self.owner && *request == self.request =>
            {
                self.busy = false;
                match result {
                    Err(e) => {
                        self.message = clean(e);
                        self.preview = None;
                    }
                    Ok(data) if *operation == 1 => {
                        self.preview = Some(data.changes.clone());
                        self.choice = 0;
                        self.preview_scroll = 0;
                        self.message = tr!("config-preview");
                    }
                    Ok(data) => {
                        self.message = if data.saved && data.applied {
                            tr!("config-applied")
                        } else if data.saved
                            && data.message == "Configuration enregistrée ; rechargement nécessaire"
                        {
                            tr!("config-saved")
                        } else {
                            data.message.clone()
                        };
                        self.selected = self.selected.min(data.fields.len().saturating_sub(1));
                        self.data = Some(data.clone());
                        self.edits.clear();
                        self.preview = None;
                        self.input = None;
                        self.metadata_editor = None;
                    }
                }
            }
            AppEvent::Event(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if let Availability::Unavailable(message) = self.availability(&ctx.store) {
                    self.message = message;
                    return Ok(Control::Changed);
                }
                if self.busy {
                    return Ok(Control::Changed);
                }
                if let Some(editor) = self.metadata_editor.as_mut() {
                    match editor.handle(key) {
                        super::metadata_rules::Outcome::Pending => {}
                        super::metadata_rules::Outcome::Cancel => self.metadata_editor = None,
                        super::metadata_rules::Outcome::Submit(value) => {
                            self.metadata_editor = None;
                            self.edit(Some(value));
                            self.request(ctx, 1);
                        }
                    }
                    return Ok(Control::Changed);
                }
                if let Some(input) = self.input.as_mut() {
                    match key.code {
                        KeyCode::Esc => {
                            self.input = None;
                        }
                        KeyCode::Enter => {
                            let value = self.input.take().unwrap();
                            self.edit(Some(value));
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        KeyCode::Char(c)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                                && !c.is_control() =>
                        {
                            if input.len() + c.len_utf8() <= 4096 {
                                input.push(c);
                            }
                        }
                        _ => {}
                    }
                } else if self.preview.is_some() {
                    match key.code {
                        KeyCode::Esc => self.preview = None,
                        KeyCode::Up => self.preview_scroll = self.preview_scroll.saturating_sub(1),
                        KeyCode::Down => {
                            self.preview_scroll = self.preview_scroll.saturating_add(1)
                        }
                        KeyCode::Left => self.choice = self.choice.saturating_sub(1),
                        KeyCode::Right => self.choice = (self.choice + 1).min(self.max_choice()),
                        KeyCode::Enter if self.choice == 0 => self.preview = None,
                        KeyCode::Enter => self.request(ctx, if self.choice == 1 { 2 } else { 3 }),
                        _ => {}
                    }
                } else {
                    let count = self.data.as_ref().map_or(0, |d| d.fields.len());
                    match key.code {
                        KeyCode::Left => self.select_target(ctx, false),
                        KeyCode::Right => self.select_target(ctx, true),
                        KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                        KeyCode::Down => {
                            self.selected = (self.selected + 1).min(count.saturating_sub(1))
                        }
                        KeyCode::Enter => {
                            if let Some(f) = self.field() {
                                if f.kind == "metadata_rules" {
                                    let value = self.value(&f.key).filter(|v| v.present)
                                        .map(|v| v.value.clone()).unwrap_or_else(|| "[]".into());
                                    match super::metadata_rules::Editor::new(&value) {
                                        Ok(editor) => self.metadata_editor = Some(editor),
                                        Err(e) => self.message = e,
                                    }
                                } else if f.kind == "boolean" {
                                    let value = self
                                        .value(&f.key)
                                        .filter(|v| v.present)
                                        .map(|v| v.value.as_str())
                                        .or(f.default_value.as_deref())
                                        .unwrap_or("false")
                                        != "true";
                                    self.edit(Some(value.to_string()));
                                } else {
                                    self.input = Some(if f.secret {
                                        String::new()
                                    } else {
                                        self.value(&f.key)
                                            .filter(|v| v.present)
                                            .map(|v| v.value.clone())
                                            .unwrap_or_default()
                                    });
                                }
                            }
                        }
                        KeyCode::Delete => {
                            if self
                                .field()
                                .is_some_and(|f| f.optional || f.default_value.is_some())
                            {
                                self.edit(None);
                            }
                        }
                        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.request(ctx, 1)
                        }
                        KeyCode::Char('r') if self.edits.is_empty() => self.read(ctx),
                        KeyCode::Esc => {
                            self.edits.clear();
                            self.message = tr!("config-discarded");
                        }
                        _ => return Ok(Control::Continue),
                    }
                }
            }
            _ => return Ok(Control::Continue),
        }
        Ok(Control::Changed)
    }
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        if let Some(editor) = &self.metadata_editor {
            editor.render(area, buf, &s);
            return Ok(());
        }
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(4),
        ])
        .areas(area);
        let pending = self.data.as_ref().is_some_and(|d| d.pending);
        Paragraph::new(format!(
            "{}\n{}{}",
            tr!("config-target", plugin = self.target_label()),
            if pending {
                tr!("config-pending")
            } else {
                String::new()
            },
            if self.busy {
                tr!("config-busy")
            } else {
                self.message.clone()
            }
        ))
        .style(s.label())
        .wrap(Wrap { trim: false })
        .render(header, buf);
        let [directory, body] =
            Layout::horizontal([Constraint::Length(26), Constraint::Fill(1)]).areas(body);
        let directory_block = Block::bordered()
            .title(Line::from(tr!("screen-plugins")).style(s.title()))
            .border_style(s.border());
        let config_block = Block::bordered()
            .title(
                Line::from(tr!("config-panel-title", plugin = self.target_label()))
                    .style(s.title()),
            )
            .border_style(s.accent());
        let directory_inner = directory_block.inner(directory);
        let config_inner = config_block.inner(body);
        directory_block.render(directory, buf);
        config_block.render(body, buf);
        let directory = directory_inner;
        let body = config_inner;
        let plugins = ctx.store.plugins.value.as_deref().unwrap_or_default();
        let selected = plugins
            .iter()
            .position(|p| p.name == self.target)
            .unwrap_or(0);
        let start = selected.saturating_sub(directory.height.saturating_sub(2) as usize);
        let list: Vec<Line> = plugins
            .iter()
            .enumerate()
            .skip(start)
            .map(|(i, p)| {
                let text = format!(
                    "{} {}{} · {}",
                    if i == selected { "›" } else { " " },
                    clean(&p.name),
                    if p.configurable { " *" } else { "" },
                    clean(&p.state)
                );
                let text = crate::fit::ellipsize(&text, directory.width as usize);
                let padding = (directory.width as usize).saturating_sub(Span::raw(&text).width());
                Line::from(format!("{text}{}", " ".repeat(padding))).style(if i == selected {
                    s.tab_active()
                } else {
                    s.muted()
                })
            })
            .collect();
        Paragraph::new(list).render(directory, buf);
        if let Some(changes) = &self.preview {
            let [changes_area, actions_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(4)]).areas(body);
            let mut lines: Vec<Line> = changes.iter().map(|s| Line::from(clean(s))).collect();
            if lines.is_empty() {
                lines.push(Line::from(tr!("config-no-changes")));
            }
            let approximate_height: usize = changes
                .iter()
                .map(|line| {
                    line.chars()
                        .count()
                        .div_ceil(changes_area.width.max(1) as usize)
                        .max(1)
                })
                .sum();
            self.preview_scroll = self
                .preview_scroll
                .min(approximate_height.saturating_sub(1).min(u16::MAX as usize) as u16);
            Paragraph::new(lines)
                .style(s.label())
                .wrap(Wrap { trim: false })
                .scroll((self.preview_scroll, 0))
                .render(changes_area, buf);
            let mut lines = Vec::new();
            let mut actions = vec![
                tr!("config-cancel"),
                tr!("config-save"),
                tr!("config-save-reload"),
            ];
            actions.truncate(self.max_choice() + 1);
            for (i, a) in actions.iter().enumerate() {
                lines.push(Line::from(if i == self.choice {
                    format!("[{a}]")
                } else {
                    format!(" {a} ")
                }));
            }
            lines.push(Line::from(tr!("config-confirm-keys")));
            Paragraph::new(lines)
                .style(s.label())
                .wrap(Wrap { trim: false })
                .render(actions_area, buf);
        } else if let Some(data) = &self.data {
            let start = self
                .selected
                .saturating_sub(body.height.saturating_sub(2) as usize);
            let lines: Vec<Line> = data
                .fields
                .iter()
                .enumerate()
                .skip(start)
                .map(|(i, f)| {
                    let prefix = format!(
                        "{} {} : ",
                        if i == self.selected { "›" } else { " " },
                        crate::fit::ellipsize(&f.label, body.width as usize / 2)
                    );
                    let available =
                        (body.width as usize).saturating_sub(Span::raw(&prefix).width());
                    let value = if i == self.selected {
                        self.input.as_ref().map(|v| {
                            let text = if f.secret {
                                "•".repeat(v.chars().count())
                            } else {
                                v.clone()
                            };
                            format!("{}▏", tail(&text, available.saturating_sub(1)))
                        })
                    } else {
                        None
                    };
                    Line::from(format!(
                        "{prefix}{}",
                        value.unwrap_or_else(|| self.shown(f))
                    ))
                })
                .collect();
            Paragraph::new(lines).style(s.label()).render(body, buf);
        } else {
            Paragraph::new(tr!("config-no-schema"))
                .style(s.muted())
                .wrap(Wrap { trim: false })
                .render(body, buf);
        }
        let detail = self
            .field()
            .map(|f| {
                if f.kind == "metadata_rules" { return tr!("metadata-restart"); }
                format!(
                    "{} · type {} · min {:?} · max {:?}",
                    f.key, f.kind, f.minimum, f.maximum
                )
            })
            .unwrap_or_default();
        Paragraph::new(format!("{detail}\n{}", tr!("config-edit-keys")))
            .style(s.muted())
            .wrap(Wrap { trim: false })
            .render(footer, buf);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_are_masked_in_saved_values_and_drafts() {
        let mut e = ConfigEditor::new("manager".into(), PluginTab::default());
        let f = stationd_proto::plugin::PluginConfigField {
            key: "token".into(),
            secret: true,
            ..Default::default()
        };
        e.data = Some(PluginConfigResponse {
            fields: vec![f.clone()],
            values: vec![PluginConfigValue {
                key: "token".into(),
                present: true,
                redacted: true,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(e.shown(&f), "••••");
        e.edit(Some("new-secret".into()));
        assert_eq!(e.shown(&f), "••••");
        e.input = Some("new-secret".into());
        assert!(e.captures_text());
        e.input = None;
        e.preview = Some(vec![]);
        assert!(e.captures_text());
        assert_eq!(e.choice, 0);
    }

    #[tokio::test]
    async fn metadata_target_opens_a_form_and_offers_save_without_plugin_reload() {
        let args = crate::Args {
            addr: "http://127.0.0.1:50051".into(), lang: None,
            theme: "Imperial".into(), list_themes: false,
        };
        let channel = rpc::lazy_channel(&args.addr).unwrap();
        let mut ctx = Global::new(&args, channel.clone(), channel);
        ctx.store.plugins.value = Some(vec![stationd_proto::plugin::PluginInfo {
            name: "plugin-config".into(), state: "loaded".into(), ..Default::default()
        }]);
        let mut editor = ConfigEditor::new("plugin-config".into(), PluginTab::default());
        editor.target = super::super::metadata_rules::TARGET.into();
        editor.data = Some(PluginConfigResponse {
            fields: vec![stationd_proto::plugin::PluginConfigField {
                key: "rules".into(), label: "Rules".into(), kind: "metadata_rules".into(),
                ..Default::default()
            }],
            values: vec![PluginConfigValue {
                key: "rules".into(), present: true,
                value: r#"[{"tag":"jingle","text":"Ma Radio"}]"#.into(), ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(editor.max_choice(), 1);
        let enter = AppEvent::Event(Event::Key(
            ratatui_crossterm::crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        editor.event(&enter, &mut ctx).unwrap();
        assert!(editor.metadata_editor.is_some() && editor.captures_text());
        let area = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(area);
        editor.render(area, &mut buf, &mut ctx).unwrap();
        let text: String = (0..20).flat_map(|y| (0..100).map(move |x| (x, y)))
            .map(|p| buf[p].symbol()).collect();
        assert!(text.contains("jingle") && text.contains("Ma Radio"));
        let cancel = AppEvent::Event(Event::Key(
            ratatui_crossterm::crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        editor.event(&cancel, &mut ctx).unwrap();
        assert!(editor.metadata_editor.is_none());
        editor.preview = Some(vec!["Changed rules".into()]);
        let right = AppEvent::Event(Event::Key(
            ratatui_crossterm::crossterm::event::KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)));
        editor.event(&right, &mut ctx).unwrap();
        editor.event(&right, &mut ctx).unwrap();
        assert_eq!(editor.choice, 1);
    }
    #[tokio::test]
    async fn conflict_keeps_draft_and_late_responses_are_ignored_and_secrets_do_not_render() {
        let args = crate::Args {
            addr: "http://127.0.0.1:50051".into(),
            lang: None,
            theme: "Imperial".into(),
            list_themes: false,
        };
        let channel = rpc::lazy_channel(&args.addr).unwrap();
        let mut ctx = Global::new(&args, channel.clone(), channel);
        ctx.store.plugins.value = Some(vec![stationd_proto::plugin::PluginInfo {
            name: "manager".into(),
            state: "loaded".into(),
            ..Default::default()
        }]);
        let mut editor = ConfigEditor::new("manager".into(), PluginTab::default());
        editor.target = "target".into();
        let field = stationd_proto::plugin::PluginConfigField {
            key: "token".into(),
            label: "Token".into(),
            kind: "text".into(),
            secret: true,
            ..Default::default()
        };
        editor.data = Some(PluginConfigResponse {
            fields: vec![field],
            values: vec![PluginConfigValue {
                key: "token".into(),
                present: true,
                redacted: true,
                ..Default::default()
            }],
            ..Default::default()
        });
        editor.edit(Some("new-secret".into()));
        editor.busy = true;
        editor.request = 2;
        let _ = editor
            .event(
                &AppEvent::PluginConfig(editor.owner + 1, 2, 2, Err("other".into())),
                &mut ctx,
            )
            .unwrap();
        assert!(editor.busy);
        let _ = editor
            .event(
                &AppEvent::PluginConfig(editor.owner, 1, 2, Ok(PluginConfigResponse::default())),
                &mut ctx,
            )
            .unwrap();
        assert!(editor.busy && !editor.edits.is_empty());
        let _ = editor
            .event(
                &AppEvent::PluginConfig(
                    editor.owner,
                    2,
                    2,
                    Err("conflict: refresh required".into()),
                ),
                &mut ctx,
            )
            .unwrap();
        assert!(!editor.busy && !editor.edits.is_empty() && editor.message.contains("conflict:"));
        editor.input = Some("new-secret".into());
        let area = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(area);
        editor.render(area, &mut buf, &mut ctx).unwrap();
        let text: String = (0..20)
            .flat_map(|y| (0..100).map(move |x| (x, y)))
            .map(|p| buf[p].symbol())
            .collect();
        assert!(!text.contains("new-secret") && text.contains("••••"));
        // Ordinary navigation keys are input while editing.
        let event = AppEvent::Event(Event::Key(
            ratatui_crossterm::crossterm::event::KeyEvent::new(
                KeyCode::Char('n'),
                KeyModifiers::NONE,
            ),
        ));
        let _ = editor.event(&event, &mut ctx).unwrap();
        assert_eq!(editor.input.as_deref(), Some("new-secretn"));
        assert!(editor.captures_text());
    }
}
