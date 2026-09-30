//! Tags (`6`) — les valeurs de la bibliothèque, par origine (dossier §5.6,
//! voie native : l'écriture des tags est celle de stationd, sans plugin).
//!
//! Une origine par onglet (`Tab`) : le genre du fichier (`TCON`), puis chaque
//! TXXX source du plugin `custom-tags` (ex. `Type`). Pour chaque valeur :
//! effectif, graphies (plus d'une = incohérence), et les médias sans valeur.
//! `e` renomme une valeur sur toute la bibliothèque — ou la fusionne dans une
//! valeur existante — après un aperçu (fichiers, playlists dont un filtre la
//! nomme) ; l'écriture se suit fichier par fichier, les échecs sont listés.
//! Affecter une valeur à des médias se fait depuis Médias (`t` pour `Type`,
//! `e` pour le reste).

use anyhow::Error;
use rat_salsa::{Control, SalsaContext};
use rat_theme4::WidgetStyle;
use rat_widget::event::{HandleEvent, Regular};
use rat_widget::text::HasScreenCursor;
use rat_widget::text_input::{TextInput, TextInputState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui_widgets::block::Block;
use ratatui_widgets::clear::Clear;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use ratatui_widgets::table::{Cell, Row, Table};
use stationd_proto::library::rename_tag_value_event::Event as Step;
use stationd_proto::library::{ListTagValuesResponse, RenamePreview, RenameTagValueRequest, TagOrigin, TagValueCount};

use super::systeme::{gauge, origin_label};
use crate::app::{AppEvent, Global};
use crate::dialog::{centered, frame};
use crate::rpc::{self, Read};
use crate::screen::{KeyHelp, Screen};
use crate::style::Styles;
use crate::{fit, k, tr};

/// Réponses propres à l'écran (n° de requête en tête).
#[derive(Debug)]
pub enum TagsEvent {
    Values(u64, Read<ListTagValuesResponse>),
    Preview(u64, Read<RenamePreview>),
    /// Une étape du renommage en cours.
    Step(u64, Step),
    /// Le flux du renommage s'est interrompu.
    Broken(u64, String),
    /// Médias (`t`) : les origines et leurs valeurs (propriétaire, n°).
    TypeValues(u64, u64, Read<Vec<TagOrigin>>),
}

/// Le renommage d'une valeur, étape par étape.
enum Stage {
    /// Saisie de la nouvelle valeur.
    Input { from: String, input: Box<TextInputState> },
    /// Aperçu demandé / reçu.
    Preview { from: String, to: String, preview: Option<Read<RenamePreview>> },
    /// En cours : fichiers faits / à faire, échecs.
    Running { from: String, to: String, total: u64, done: u64, failed: Vec<(String, String)>, broken: Option<String> },
    /// Terminé.
    Report { from: String, to: String, changed: u64, unchanged: u64, failed: Vec<(String, String)>, broken: Option<String> },
}

const KEYS: &[KeyHelp] = &[
    (k!("key-tab"), k!("help-tags-origin")),
    (k!("key-up-down"), k!("help-select")),
    (k!("key-e"), k!("help-tags-rename")),
    (k!("key-s"), k!("help-tags-sort")),
    (k!("key-r"), k!("help-reload")),
];
const INPUT_KEYS: &[KeyHelp] = &[(k!("key-enter"), k!("help-tags-preview")), (k!("key-esc"), k!("help-cancel"))];
const PREVIEW_KEYS: &[KeyHelp] = &[(k!("key-enter"), k!("help-tags-apply")), (k!("key-esc"), k!("help-cancel"))];
const DONE_KEYS: &[KeyHelp] = &[(k!("key-esc"), k!("help-close"))];

#[derive(Default)]
pub struct Tags {
    data: Option<Read<ListTagValuesResponse>>,
    req: u64,
    origin: usize,
    selected: usize,
    by_count: bool,
    stage: Option<Stage>,
}

impl Tags {
    fn load(&mut self, ctx: &mut Global) {
        self.req += 1;
        let (id, channel) = (self.req, ctx.channel.clone());
        ctx.spawn_async(async move {
            let r = rpc::tag_values(channel).await;
            Ok(Control::Event(AppEvent::Tags(Box::new(TagsEvent::Values(id, r)))))
        });
    }

    fn origins(&self) -> &[TagOrigin] {
        match &self.data {
            Some(Ok(r)) => &r.origins,
            _ => &[],
        }
    }

    fn current(&self) -> Option<&TagOrigin> {
        self.origins().get(self.origin)
    }

    /// Les valeurs de l'origine choisie, dans l'ordre affiché.
    fn values(&self) -> Vec<&TagValueCount> {
        let mut v: Vec<&TagValueCount> = self.current().map(|o| o.values.iter().collect()).unwrap_or_default();
        if self.by_count {
            v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.to_lowercase().cmp(&b.value.to_lowercase())));
        }
        v
    }

    fn origin_name(&self) -> String {
        self.current().map(|o| o.origin.clone()).unwrap_or_default()
    }

    fn preview(&mut self, from: String, to: String, ctx: &mut Global) {
        self.req += 1;
        let (id, channel) = (self.req, ctx.channel.clone());
        let origin = self.origin_name();
        let req = RenameTagValueRequest { origin, from: from.clone(), to: to.clone(), dry_run: true };
        self.stage = Some(Stage::Preview { from, to, preview: None });
        ctx.spawn_async(async move {
            let r = match rpc::rename_value(channel, req).await {
                Ok(mut st) => match st.message().await {
                    Ok(Some(ev)) => match ev.event {
                        Some(Step::Preview(p)) => Ok(p),
                        _ => Err(tr!("tags-rename-unexpected")),
                    },
                    Ok(None) => Err(tr!("stream-closed")),
                    Err(e) => Err(rpc::status_text(&e)),
                },
                Err(e) => Err(e),
            };
            Ok(Control::Event(AppEvent::Tags(Box::new(TagsEvent::Preview(id, r)))))
        });
    }

    fn run(&mut self, from: String, to: String, ctx: &mut Global) {
        self.req += 1;
        let id = self.req;
        // Un renommage peut durer (un fichier après l'autre, sur NFS).
        let channel = ctx.long_channel.clone();
        let req = RenameTagValueRequest { origin: self.origin_name(), from: from.clone(), to: to.clone(), dry_run: false };
        self.stage = Some(Stage::Running { from, to, total: 0, done: 0, failed: Vec::new(), broken: None });
        ctx.spawn_async_ext(move |chan| async move {
            let send = |ev: TagsEvent| Ok(Control::Event(AppEvent::Tags(Box::new(ev))));
            match rpc::rename_value(channel, req).await {
                Ok(mut st) => loop {
                    match st.message().await {
                        Ok(Some(ev)) => {
                            if let Some(step) = ev.event
                                && chan.send(send(TagsEvent::Step(id, step))).await.is_err()
                            {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            let _ = chan.send(send(TagsEvent::Broken(id, rpc::status_text(&e)))).await;
                            break;
                        }
                    }
                },
                Err(e) => {
                    let _ = chan.send(send(TagsEvent::Broken(id, e))).await;
                }
            }
            Ok(Control::Continue)
        });
    }

    fn on_event(&mut self, ev: &TagsEvent, ctx: &mut Global) -> Control<AppEvent> {
        match ev {
            TagsEvent::Values(id, r) if *id == self.req => {
                self.data = Some(r.clone());
                self.origin = self.origin.min(self.origins().len().saturating_sub(1));
            }
            TagsEvent::Preview(id, r) if *id == self.req => {
                if let Some(Stage::Preview { preview, .. }) = &mut self.stage {
                    *preview = Some(r.clone());
                }
            }
            TagsEvent::Step(id, step) if *id == self.req => {
                let Some(Stage::Running { from, to, total, done, failed, broken }) = &mut self.stage else {
                    return Control::Continue;
                };
                match step {
                    Step::Started(n) => *total = *n,
                    Step::File(f) => {
                        *done += 1;
                        if !f.error.is_empty() {
                            failed.push((f.rel_path.clone(), f.error.clone()));
                        }
                    }
                    Step::Done(d) => {
                        let stage = Stage::Report {
                            from: std::mem::take(from),
                            to: std::mem::take(to),
                            changed: d.changed,
                            unchanged: d.unchanged,
                            failed: std::mem::take(failed),
                            broken: broken.take(),
                        };
                        self.stage = Some(stage);
                        // Les effectifs ont changé.
                        self.load(ctx);
                    }
                    Step::Preview(_) => {}
                }
            }
            TagsEvent::Broken(id, why) if *id == self.req => {
                if let Some(Stage::Running { from, to, done, failed, .. }) = &mut self.stage {
                    let stage = Stage::Report {
                        from: std::mem::take(from),
                        to: std::mem::take(to),
                        changed: *done - failed.len() as u64,
                        unchanged: 0,
                        failed: std::mem::take(failed),
                        broken: Some(why.clone()),
                    };
                    self.stage = Some(stage);
                    self.load(ctx);
                }
            }
            _ => return Control::Continue,
        }
        Control::Changed
    }

    fn stage_key(&mut self, e: &Event, ctx: &mut Global) -> Control<AppEvent> {
        let Some(stage) = self.stage.as_mut() else { return Control::Continue };
        let key = match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => Some(k.code),
            _ => None,
        };
        match stage {
            Stage::Input { from, input } => match key {
                Some(KeyCode::Esc) => self.stage = None,
                Some(KeyCode::Enter) => {
                    let to = input.text().trim().to_string();
                    let from = from.clone();
                    if !to.is_empty() && to != from {
                        self.preview(from, to, ctx);
                    }
                }
                _ => {
                    input.handle(e, Regular);
                }
            },
            Stage::Preview { from, to, preview } => match key {
                Some(KeyCode::Esc) => self.stage = None,
                Some(KeyCode::Enter) if matches!(preview, Some(Ok(p)) if !p.files.is_empty()) => {
                    let (from, to) = (from.clone(), to.clone());
                    self.run(from, to, ctx);
                }
                _ => return Control::Unchanged,
            },
            // Rien n'arrête un renommage lancé (il laisserait la bibliothèque
            // à moitié renommée) : on attend son bilan.
            Stage::Running { .. } => return Control::Unchanged,
            Stage::Report { .. } => match key {
                Some(KeyCode::Esc | KeyCode::Enter) => self.stage = None,
                _ => return Control::Unchanged,
            },
        }
        Control::Changed
    }

    /// Rend l'étape du renommage ; la position du curseur en saisie.
    fn render_stage(&mut self, area: Rect, buf: &mut Buffer, ctx: &Global, s: &Styles) -> Option<(u16, u16)> {
        let origin = origin_label(&self.origin_name());
        let stage = self.stage.as_mut()?;
        let w = area.width.saturating_sub(8).min(90);
        match stage {
            Stage::Input { from, input } => {
                let box_a = centered(area, w, 7);
                Clear.render(box_a, buf);
                let block = frame(&tr!("tags-rename-title", origin = origin, from = from.clone()), s, false);
                let inner = block.inner(box_a);
                block.style(s.base()).render(box_a, buf);
                let [hint_a, _, input_a, _] = Layout::vertical([
                    Constraint::Length(2),
                    Constraint::Length(0),
                    Constraint::Length(1),
                    Constraint::Fill(1),
                ])
                .areas(inner);
                Paragraph::new(Line::styled(tr!("tags-rename-hint"), s.muted())).wrap(Wrap { trim: false }).render(hint_a, buf);
                let style: rat_widget::text::TextStyle = ctx.theme.style(WidgetStyle::TEXT);
                TextInput::new().styles(style).render(input_a, buf, input);
                return input.screen_cursor();
            }
            Stage::Preview { from, to, preview } => {
                let mut lines = vec![Line::from(vec![
                    Span::styled(from.clone(), s.title()),
                    Span::raw("  →  "),
                    Span::styled(to.clone(), s.title()),
                ])];
                match preview {
                    None => lines.push(Line::styled(tr!("media-loading"), s.muted())),
                    Some(Err(e)) => lines.push(Line::styled(e.clone(), s.error())),
                    Some(Ok(p)) if p.files.is_empty() => lines.push(Line::styled(tr!("tags-rename-nothing"), s.warn())),
                    Some(Ok(p)) => {
                        lines.push(Line::raw(tr!("tags-rename-files", n = p.files.len())));
                        if p.merges {
                            lines.push(Line::styled(tr!("tags-rename-merges", to = to.clone()), s.warn()));
                        }
                        if p.playlists.is_empty() {
                            lines.push(Line::styled(tr!("tags-rename-no-playlist"), s.muted()));
                        } else {
                            lines.push(Line::styled(tr!("tags-rename-playlists", n = p.playlists.len()), s.warn()));
                            for n in p.playlists.iter().take(6) {
                                lines.push(Line::raw(format!("  · {n}")));
                            }
                            if p.playlists.len() > 6 {
                                lines.push(Line::styled(format!("  {}", tr!("control-more", n = p.playlists.len() - 6)), s.muted()));
                            }
                        }
                        lines.push(Line::default());
                        lines.push(Line::styled(tr!("tags-rename-confirm"), s.accent()));
                    }
                }
                let box_a = centered(area, w, lines.len() as u16 + 2);
                Clear.render(box_a, buf);
                let block = frame(&tr!("tags-rename-preview-title", origin = origin), s, false);
                let inner = block.inner(box_a);
                block.style(s.base()).render(box_a, buf);
                Paragraph::new(lines).wrap(Wrap { trim: false }).render(inner, buf);
            }
            Stage::Running { from, to, total, done, failed, .. } => {
                let mut lines = vec![
                    Line::raw(format!("{from}  →  {to}")),
                    Line::raw(tr!("tags-rename-progress", done = *done, total = *total)),
                    Line::raw(gauge(*done, *total, (w as usize).saturating_sub(12).min(40))),
                ];
                if !failed.is_empty() {
                    lines.push(Line::styled(tr!("tags-rename-failed-n", n = failed.len()), s.error()));
                }
                let box_a = centered(area, w, lines.len() as u16 + 2);
                Clear.render(box_a, buf);
                let block = frame(&tr!("tags-rename-running-title", origin = origin), s, false);
                let inner = block.inner(box_a);
                block.style(s.base()).render(box_a, buf);
                Paragraph::new(lines).render(inner, buf);
            }
            Stage::Report { from, to, changed, unchanged, failed, broken } => {
                let mut lines = vec![
                    Line::raw(format!("{from}  →  {to}")),
                    Line::styled(
                        tr!("tags-rename-report", changed = *changed, unchanged = *unchanged, failed = failed.len()),
                        if failed.is_empty() && broken.is_none() { s.ok() } else { s.warn() },
                    ),
                ];
                if let Some(b) = broken {
                    lines.push(Line::styled(tr!("tags-rename-broken", reason = b.clone()), s.error()));
                }
                let room = area.height.saturating_sub(10) as usize;
                for (path, why) in failed.iter().take(room) {
                    lines.push(Line::from(vec![
                        Span::styled("  ✕ ", s.error()),
                        Span::raw(fit::ellipsize(&format!("{path} : {why}"), (w as usize).saturating_sub(6))),
                    ]));
                }
                if failed.len() > room {
                    lines.push(Line::styled(format!("  {}", tr!("control-more", n = failed.len() - room)), s.muted()));
                }
                let box_a = centered(area, w, lines.len() as u16 + 2);
                Clear.render(box_a, buf);
                let block = frame(&tr!("tags-rename-done-title", origin = origin), s, !failed.is_empty());
                let inner = block.inner(box_a);
                block.style(s.base()).render(box_a, buf);
                Paragraph::new(lines).render(inner, buf);
            }
        }
        None
    }
}

impl Screen for Tags {
    fn title(&self) -> String {
        tr!("screen-tags")
    }

    fn enter(&mut self, ctx: &mut Global) -> Result<(), Error> {
        if self.stage.is_none() {
            self.load(ctx);
        }
        Ok(())
    }

    fn reconnected(&mut self, ctx: &mut Global) -> Result<(), Error> {
        self.enter(ctx)
    }

    fn captures_text(&self) -> bool {
        // Une étape du renommage garde la main (Esc la ferme).
        self.stage.is_some()
    }

    fn help(&self) -> &'static [KeyHelp] {
        match &self.stage {
            None => KEYS,
            Some(Stage::Input { .. }) => INPUT_KEYS,
            Some(Stage::Preview { .. }) => PREVIEW_KEYS,
            Some(Stage::Running { .. }) => &[],
            Some(Stage::Report { .. }) => DONE_KEYS,
        }
    }

    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        if let AppEvent::Tags(ev) = event {
            return Ok(self.on_event(ev, ctx));
        }
        let AppEvent::Event(e) = event else { return Ok(Control::Continue) };
        if self.stage.is_some() {
            return Ok(self.stage_key(e, ctx));
        }
        let Event::Key(k) = e else { return Ok(Control::Continue) };
        if k.kind != KeyEventKind::Press {
            return Ok(Control::Continue);
        }
        let n = self.values().len();
        let origins = self.origins().len().max(1);
        match k.code {
            KeyCode::Tab => {
                self.origin = (self.origin + 1) % origins;
                self.selected = 0;
            }
            KeyCode::BackTab => {
                self.origin = (self.origin + origins - 1) % origins;
                self.selected = 0;
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(n.saturating_sub(1)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = n.saturating_sub(1),
            KeyCode::Char('s') => {
                self.by_count = !self.by_count;
                self.selected = 0;
            }
            KeyCode::Char('r') => self.load(ctx),
            KeyCode::Char('e') => {
                let Some(v) = self.values().get(self.selected).map(|v| v.value.clone()) else {
                    return Ok(Control::Unchanged);
                };
                let mut input = TextInputState::new();
                input.set_text(v.clone());
                // Saisir remplace ; les flèches gardent pour corriger.
                input.select_all();
                input.focus.set(true);
                self.stage = Some(Stage::Input { from: v, input: Box::new(input) });
            }
            _ => return Ok(Control::Continue),
        }
        Ok(Control::Changed)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        Block::new().style(s.base()).render(area, buf);
        let [tabs_a, table_a, foot_a] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(2)]).areas(area);
        match &self.data {
            None => {
                Paragraph::new(Line::styled(format!(" {}", tr!("media-loading")), s.muted())).render(table_a, buf);
                return Ok(());
            }
            Some(Err(e)) => {
                Paragraph::new(Line::styled(format!(" {e}"), s.error())).wrap(Wrap { trim: false }).render(table_a, buf);
                return Ok(());
            }
            Some(Ok(_)) => {}
        }

        // Onglets des origines.
        let mut spans = vec![Span::raw(" ")];
        for (i, o) in self.origins().iter().enumerate() {
            let st = if i == self.origin { s.tab_active() } else { s.tab() };
            spans.push(Span::styled(format!(" {} ", origin_label(&o.origin)), st));
            spans.push(Span::raw(" "));
        }
        if self.origins().len() < 2 {
            spans.push(Span::styled(tr!("tags-no-source"), s.muted()));
        }
        Paragraph::new(Line::from(spans)).render(tabs_a, buf);

        let values = self.values();
        let without = self.current().map(|o| o.without).unwrap_or(0);
        if values.is_empty() {
            Paragraph::new(vec![Line::default(), Line::styled(format!(" {}", tr!("tags-empty")), s.muted())])
                .wrap(Wrap { trim: false })
                .render(table_a, buf);
        } else {
            let sel = self.selected.min(values.len() - 1);
            let max = values.iter().map(|v| v.count).max().unwrap_or(1).max(1);
            let bar_w: u32 = 20;
            let h = table_a.height.saturating_sub(1) as usize;
            let start = sel.saturating_sub(h.saturating_sub(1));
            let rows: Vec<Row> = values
                .iter()
                .enumerate()
                .skip(start)
                .take(h)
                .map(|(i, v)| {
                    let st = if i == sel { s.tab_active() } else { s.base() };
                    let spell = if v.spellings.len() > 1 {
                        Span::styled(tr!("tags-spellings", list = v.spellings.join(" | ")), s.warn())
                    } else {
                        Span::raw("")
                    };
                    let n = ((v.count * bar_w) / max).max(1) as usize;
                    Row::new(vec![
                        Cell::from(Span::styled(format!(" {}", v.value), st)),
                        Cell::from(Span::raw(v.count.to_string())),
                        Cell::from(Span::styled("█".repeat(n), s.accent())),
                        Cell::from(spell),
                    ])
                })
                .collect();
            let header = Row::new(vec![
                Cell::from(Span::styled(format!(" {}", tr!("tags-value")), s.label())),
                Cell::from(Span::styled(tr!("tags-count"), s.label())),
                Cell::from(""),
                Cell::from(""),
            ]);
            let value_w = values.iter().map(|v| Span::raw(v.value.as_str()).width()).max().unwrap_or(10).clamp(10, 40) as u16 + 2;
            let table = Table::new(
                rows,
                [Constraint::Length(value_w), Constraint::Length(6), Constraint::Length(bar_w as u16), Constraint::Fill(1)],
            )
            .header(header)
            .column_spacing(1);
            Widget::render(table, table_a, buf);
        }
        let origin = origin_label(&self.origin_name());
        let foot = vec![
            Line::styled(tr!("tags-without", origin = origin, n = without), if without > 0 { s.warn() } else { s.muted() }),
            Line::styled(
                tr!("tags-summary", n = values.len(), sort = if self.by_count { tr!("tags-sort-count") } else { tr!("tags-sort-name") }),
                s.muted(),
            ),
        ];
        Paragraph::new(foot).render(foot_a, buf);
        let cursor = self.render_stage(area, buf, ctx, &s);
        if cursor.is_some() {
            ctx.set_screen_cursor(cursor);
        }
        Ok(())
    }
}
