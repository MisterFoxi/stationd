//! Playlists (`3`) — dossier §5.3.
//!
//! Liste (`PlaylistService.List` : mode, activée, règles et groupes qui la
//! référencent), détail de la playlist choisie (`Export` : fichier, révision,
//! écart avec ce qui est appliqué ; `PreviewPool` : son pool aujourd'hui),
//! et l'éditeur (`editor.rs`) pour créer ou modifier. Suppression par
//! `Remove` avec la révision lue, jamais sans confirmation ; refusée d'avance
//! (et expliquée) tant qu'une règle ou un groupe la référence — stationd
//! reste juge : il refuse aussi de son côté.

use std::time::Duration;

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
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use ratatui_widgets::table::{Cell, Row, Table};
use stationd_proto::library::ListGenresResponse;
use stationd_proto::playlist::{ExportResponse, PlaylistSummary, PreviewPoolResponse, SaveResponse};

use super::editor::{self, Editor};
use super::picker::mode_label;
use crate::action::Action;
use crate::app::{AppEvent, Global, Handoff};
use crate::dialog::{Confirm, Info, Modal};
use crate::draft::MODES;
use crate::screen::{KeyHelp, Screen};
use crate::style::Styles;
use crate::{fit, k, tr};

/// Réponses et minuteries de l'écran Playlists et de ses éditeurs (le
/// premier nombre identifie le demandeur).
#[derive(Debug)]
pub enum PlEvent {
    /// La liste (n° de requête).
    Listed(u64, Result<Vec<PlaylistSummary>, String>),
    /// Le détail de la playlist choisie (n° de requête).
    Detail(u64, Result<ExportResponse, String>),
    /// Le pool de la playlist choisie (n° de requête).
    DetailPool(u64, Result<PreviewPoolResponse, String>),
    /// La sélection n'a plus bougé depuis un moment (n° du mouvement).
    Settled(u64),
    /// Export reçu pour ouvrir l'éditeur (n° de requête, médias confiés).
    Opened(u64, Result<ExportResponse, String>, Vec<String>),
    /// Éditeur : fin du délai après une frappe (éditeur, n° de frappe).
    Typed(u64, u64),
    Preview(u64, u64, Result<PreviewPoolResponse, String>),
    Saved(u64, Result<SaveResponse, String>),
    Genres(u64, Result<ListGenresResponse, String>),
    /// Le fichier du nœud, pour comparer (conflit).
    Compare(u64, Result<ExportResponse, String>),
    /// Le fichier du nœud, pour repartir de lui (conflit : recharger).
    Reloaded(u64, Result<ExportResponse, String>),
}

/// Délai de la sélection avant de lire son détail (on ne lit pas chaque
/// ligne survolée en descendant la liste).
const SETTLE: Duration = Duration::from_millis(200);

/// Tris proposés, dans l'ordre de `s`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Ref,
    Name,
    Mode,
}

const SORTS: [Sort; 3] = [Sort::Ref, Sort::Name, Sort::Mode];

pub struct Playlists {
    rows: Vec<PlaylistSummary>,
    loaded: bool,
    loading: bool,
    error: Option<String>,
    request: u64,
    filter: TextInputState,
    filtering: bool,
    sort: usize,
    /// Sélection suivie par identité (le ref, ou l'id d'une entrée sans
    /// fichier), jamais par numéro de ligne.
    selected: Option<String>,
    moves: u64,
    detail: Option<(String, Result<ExportResponse, String>)>,
    pool: Option<(String, Result<PreviewPoolResponse, String>)>,
    detail_req: u64,
    editor: Option<Editor>,
    /// Ouverture en attente de l'`Export` (n° de requête).
    opening: Option<u64>,
    /// Choix du mode d'une nouvelle playlist (`n`).
    new_mode: Option<usize>,
    /// Playlist à montrer dès que la liste est lue (depuis l'agenda).
    reveal: Option<String>,
}

impl Default for Playlists {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            loaded: false,
            loading: false,
            error: None,
            request: 0,
            filter: TextInputState::new(),
            filtering: false,
            sort: 0,
            selected: None,
            moves: 0,
            detail: None,
            pool: None,
            detail_req: 0,
            editor: None,
            opening: None,
            new_mode: None,
            reveal: None,
        }
    }
}

fn key_of(p: &PlaylistSummary) -> String {
    if p.rel_path.is_empty() { p.id.clone() } else { p.rel_path.clone() }
}

fn send(ctx: &mut Global, ev: impl std::future::Future<Output = PlEvent> + Send + 'static) {
    ctx.spawn_async(async move { Ok(Control::Event(AppEvent::Playlists(Box::new(ev.await)))) });
}

impl Playlists {
    fn load(&mut self, ctx: &mut Global) {
        self.request += 1;
        self.loading = true;
        let (id, channel) = (self.request, ctx.channel.clone());
        send(ctx, async move { PlEvent::Listed(id, crate::rpc::list_playlists(channel).await) });
    }

    /// Lignes visibles : filtrées (ref ou nom contenant le texte) et triées.
    fn visible(&self) -> Vec<&PlaylistSummary> {
        let needle = self.filter.text().trim().to_lowercase();
        let mut v: Vec<&PlaylistSummary> = self
            .rows
            .iter()
            .filter(|p| {
                needle.is_empty() || p.rel_path.to_lowercase().contains(&needle) || p.name.to_lowercase().contains(&needle)
            })
            .collect();
        match SORTS[self.sort] {
            Sort::Ref => v.sort_by(|a, b| a.rel_path.cmp(&b.rel_path)),
            Sort::Name => v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.rel_path.cmp(&b.rel_path))),
            Sort::Mode => v.sort_by(|a, b| a.mode.cmp(&b.mode).then(a.rel_path.cmp(&b.rel_path))),
        }
        v
    }

    /// Sélectionne la playlist `reference` (casse ignorée, comme les refs de
    /// la grille) ; le filtre est vidé pour qu'elle soit visible. Rend faux
    /// si la liste ne la contient pas.
    fn reveal(&mut self, reference: &str, ctx: &mut Global) -> bool {
        let Some(p) = self.rows.iter().find(|p| p.rel_path.eq_ignore_ascii_case(reference)) else {
            return false;
        };
        let key = key_of(p);
        self.filter.set_text("");
        self.filtering = false;
        if self.selected.as_deref() != Some(key.as_str()) {
            self.selected = Some(key);
            self.settle(ctx);
        }
        true
    }

    fn index(&self) -> usize {
        let vis = self.visible();
        self.selected.as_ref().and_then(|k| vis.iter().position(|p| key_of(p) == *k)).unwrap_or(0)
    }

    fn current(&self) -> Option<&PlaylistSummary> {
        let vis = self.visible();
        vis.get(self.index()).copied()
    }

    fn select(&mut self, idx: usize, ctx: &mut Global) {
        let vis = self.visible();
        let Some(p) = vis.get(idx.min(vis.len().saturating_sub(1))) else { return };
        let key = key_of(p);
        if self.selected.as_deref() != Some(key.as_str()) {
            self.selected = Some(key);
            self.settle(ctx);
        }
    }

    /// Le détail suit la sélection, `SETTLE` après le dernier mouvement.
    fn settle(&mut self, ctx: &mut Global) {
        self.moves += 1;
        let id = self.moves;
        send(ctx, async move {
            tokio::time::sleep(SETTLE).await;
            PlEvent::Settled(id)
        });
    }

    fn load_detail(&mut self, ctx: &mut Global) {
        let Some(key) = self.selected.clone() else { return };
        if self.detail.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        self.detail_req += 1;
        self.detail = None;
        self.pool = None;
        let (id, channel) = (self.detail_req, ctx.channel.clone());
        send(ctx, async move { PlEvent::Detail(id, crate::rpc::export_playlist(channel, key).await) });
    }

    fn open_editor(&mut self, reference: String, files: Vec<String>, ctx: &mut Global) {
        self.request += 1;
        self.opening = Some(self.request);
        let (id, channel) = (self.request, ctx.channel.clone());
        send(ctx, async move { PlEvent::Opened(id, crate::rpc::export_playlist(channel, reference).await, files) });
    }

    fn delete(&mut self, ctx: &mut Global) {
        let Some(p) = self.current().cloned() else { return };
        let key = key_of(&p);
        if !p.rules.is_empty() || !p.groups.is_empty() {
            let mut lines = vec![tr!("pl-delete-refused", playlist = key.clone())];
            if !p.rules.is_empty() {
                lines.push(tr!("pl-used-rules", list = p.rules.join(", ")));
            }
            if !p.groups.is_empty() {
                lines.push(tr!("pl-used-groups", list = p.groups.join(", ")));
            }
            ctx.open(Modal::Info(Info { title: tr!("pl-delete-title"), lines }));
            return;
        }
        // La révision lue au détail : si le fichier a changé depuis, stationd
        // refuse (rien n'est supprimé sur la foi d'une lecture périmée).
        let (file, revision) = match &self.detail {
            Some((k, Ok(x))) if *k == key => (x.file.clone(), x.revision.clone()),
            _ => (String::new(), String::new()),
        };
        let mut lines = vec![tr!("pl-delete-body", playlist = key.clone(), name = p.name.clone())];
        lines.push(if file.is_empty() { tr!("pl-delete-no-file") } else { tr!("pl-delete-file", file = file) });
        ctx.open(Modal::Confirm(
            Confirm::new(tr!("pl-delete-title"), lines, tr!("pl-delete-yes"), Action::RemovePlaylist { reference: key, revision })
                .danger(),
        ));
    }

    fn render_list(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) {
        let s = Styles(&ctx.theme);
        let [filter_a, table_a] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        if self.filtering || !self.filter.text().is_empty() {
            let [m, f] = Layout::horizontal([Constraint::Length(2), Constraint::Fill(1)]).areas(filter_a);
            Span::styled("/ ", s.accent()).render(m, buf);
            let style: rat_widget::text::TextStyle = ctx.theme.style(WidgetStyle::TEXT);
            TextInput::new().styles(style).render(f, buf, &mut self.filter);
            if self.filtering {
                ctx.set_screen_cursor(self.filter.screen_cursor());
            }
        } else {
            let sort = match SORTS[self.sort] {
                Sort::Ref => tr!("pl-col-ref"),
                Sort::Name => tr!("pl-col-name"),
                Sort::Mode => tr!("pl-col-mode"),
            };
            let info = tr!("pl-summary", n = self.rows.len(), sort = sort);
            Paragraph::new(Span::styled(format!(" {info}"), s.muted())).render(filter_a, buf);
        }

        let vis = self.visible();
        if vis.is_empty() {
            let msg = match (&self.error, self.loading || !self.loaded) {
                (Some(e), _) => Span::styled(e.clone(), s.error()),
                (None, true) => Span::styled(tr!("media-loading"), s.muted()),
                (None, false) => Span::styled(tr!("pl-none"), s.muted()),
            };
            Paragraph::new(Line::from(vec![Span::raw(" "), msg])).render(table_a, buf);
            return;
        }
        let sel = self.index();
        let header = Row::new(vec![
            Cell::from(tr!("pl-col-ref")),
            Cell::from(tr!("pl-col-name")),
            Cell::from(tr!("pl-col-mode")),
            Cell::from(tr!("pl-col-used")),
        ])
        .style(s.label());
        let h = table_a.height.saturating_sub(1) as usize;
        let start = sel.saturating_sub(h.saturating_sub(1)).min(vis.len().saturating_sub(h));
        let rows: Vec<Row> = vis[start..(start + h).min(vis.len())]
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let refname = if p.rel_path.is_empty() {
                    Span::styled(tr!("pl-no-file"), s.warn())
                } else {
                    Span::raw(p.rel_path.clone())
                };
                let used = match used_count(p) {
                    None => Span::styled("—", s.muted()),
                    Some(t) => Span::styled(t, s.label()),
                };
                let mut row = Row::new(vec![
                    Cell::from(refname),
                    Cell::from(p.name.clone()),
                    Cell::from(Span::styled(mode_label(&p.mode), s.accent())),
                    Cell::from(used),
                ]);
                if !p.enabled {
                    row = row.style(s.muted());
                }
                if start + i == sel { row.style(s.tab_active()) } else { row }
            })
            .collect();
        let widths = [Constraint::Fill(3), Constraint::Fill(3), Constraint::Length(10), Constraint::Length(20)];
        Widget::render(Table::new(rows, widths).header(header).column_spacing(1), table_a, buf);
    }

    fn render_detail(&self, area: Rect, buf: &mut Buffer, ctx: &Global) {
        let s = Styles(&ctx.theme);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(s.border())
            .title(Span::styled(format!(" {} ", tr!("pl-detail")), s.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        let Some(p) = self.current() else { return };
        let key = key_of(p);
        let kv = |k: String, v: Span<'static>| Line::from(vec![Span::styled(format!("{:<12} ", fit::ellipsize(&k, 12)), s.label()), v]);
        let mut lines = vec![
            kv(tr!("pl-col-name"), Span::styled(p.name.clone(), s.title())),
            kv(tr!("pl-col-mode"), Span::styled(mode_label(&p.mode), s.accent())),
            kv(
                tr!("pl-f-enabled"),
                if p.enabled { Span::styled(tr!("val-yes"), s.ok()) } else { Span::styled(tr!("val-no"), s.warn()) },
            ),
        ];
        match &self.detail {
            Some((k, Ok(x))) if *k == key => {
                if x.file.is_empty() {
                    lines.push(kv(tr!("pl-file"), Span::styled(tr!("pl-no-file-long"), s.warn())));
                } else {
                    let short: String = x.revision.trim_start_matches("sha256:").chars().take(8).collect();
                    lines.push(kv(tr!("pl-file"), Span::raw(format!("{}  ({})", x.file, tr!("ed-revision", rev = short)))));
                }
                if x.file_differs {
                    lines.push(Line::styled(tr!("pl-file-differs"), s.warn()));
                }
            }
            Some((k, Err(e))) if *k == key => lines.push(Line::styled(e.clone(), s.error())),
            _ => lines.push(kv(tr!("pl-file"), Span::styled(tr!("media-loading"), s.muted()))),
        }
        if p.rules.is_empty() && p.groups.is_empty() {
            lines.push(kv(tr!("pl-col-used"), Span::styled(tr!("pl-used-none"), s.muted())));
        } else {
            if !p.rules.is_empty() {
                lines.push(kv(tr!("pl-col-used"), Span::raw(tr!("pl-used-rules", list = p.rules.join(", ")))));
            }
            if !p.groups.is_empty() {
                let label = if p.rules.is_empty() { tr!("pl-col-used") } else { String::new() };
                lines.push(kv(label, Span::raw(tr!("pl-used-groups", list = p.groups.join(", ")))));
            }
        }
        let pool = match &self.pool {
            Some((k, Ok(r))) if *k == key => match (r.ok, r.count) {
                (false, _) => Span::styled(tr!("ed-pool-invalid"), s.warn()),
                (_, None) => Span::styled(tr!("ed-pool-unmeasured-short"), s.muted()),
                (_, Some(0)) => Span::styled(tr!("ed-pool-empty"), s.error()),
                (_, Some(n)) => {
                    let mut t = tr!("ed-pool-count", n = n);
                    if let Some(ms) = r.duration_ms {
                        t.push_str(&format!(" · {}", crate::store::human_duration(Duration::from_millis(ms))));
                    }
                    Span::styled(t, s.ok())
                }
            },
            Some((k, Err(e))) if *k == key => Span::styled(e.clone(), s.error()),
            _ => Span::styled(tr!("media-loading"), s.muted()),
        };
        lines.push(kv(tr!("pl-pool"), pool));
        lines.push(Line::default());
        if let Some((k, Ok(x))) = &self.detail
            && *k == key
        {
            let text = if x.file_toml.is_empty() { &x.applied_toml } else { &x.file_toml };
            for l in text.lines() {
                let st = if l.trim_start().starts_with('#') { s.muted() } else { s.base() };
                lines.push(Line::styled(l.to_string(), st));
            }
        }
        Paragraph::new(lines).wrap(Wrap { trim: false }).render(inner, buf);
    }

    fn render_new_mode(&self, area: Rect, buf: &mut Buffer, s: &Styles, sel: usize) {
        let box_a = crate::dialog::centered(area, 46, MODES.len() as u16 + 4);
        ratatui_widgets::clear::Clear.render(box_a, buf);
        let block = crate::dialog::frame(&tr!("pl-new-title"), s, false)
            .title_bottom(Span::styled(format!(" {} ", tr!("picker-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let mut lines = vec![Line::styled(tr!("pl-new-mode"), s.label()), Line::default()];
        for (i, m) in MODES.iter().enumerate() {
            let st = if i == sel { s.tab_active() } else { s.base() };
            lines.push(Line::styled(format!(" {} ({m})", mode_label(m)), st));
        }
        Paragraph::new(lines).render(inner, buf);
    }
}

impl Screen for Playlists {
    fn title(&self) -> String {
        tr!("screen-playlists")
    }

    fn captures_text(&self) -> bool {
        self.filtering || self.editor.is_some() || self.new_mode.is_some()
    }

    fn enter(&mut self, ctx: &mut Global) -> Result<(), Error> {
        let handoff = ctx.handoff.take();
        if let Some(Handoff::Select { reference }) = &handoff {
            // Relue pour la trouver à jour ; un brouillon ouvert reste ouvert
            // (la liste la montre à sa fermeture).
            self.reveal = Some(reference.clone());
            self.load(ctx);
            return Ok(());
        }
        if let Some(Handoff::AddFiles { reference, files }) = handoff {
            match (reference, self.editor.as_mut()) {
                // Un brouillon est déjà ouvert sur cette playlist : on y ajoute.
                (Some(r), Some(ed)) if editor_ref_is(ed, &r) => ed.add_files(&files, ctx),
                (_, Some(_)) => {
                    // Un autre brouillon est ouvert : il n'est pas remplacé.
                    ctx.open(Modal::Info(Info { title: tr!("pl-busy-title"), lines: vec![tr!("pl-busy-body")] }));
                }
                (Some(r), None) => self.open_editor(r, files, ctx),
                (None, None) => {
                    let mut ed = Editor::new_playlist("static", self.rows.clone(), ctx);
                    ed.add_files(&files, ctx);
                    self.editor = Some(ed);
                }
            }
        }
        if !self.loaded || self.loading {
            self.load(ctx);
        }
        Ok(())
    }

    fn help(&self) -> &'static [KeyHelp] {
        if let Some(e) = &self.editor {
            return e.keys();
        }
        if self.filtering {
            return &[(k!("key-enter"), k!("help-media-apply")), (k!("key-esc"), k!("help-media-cancel"))];
        }
        &[
            (k!("key-enter"), k!("help-pl-edit")),
            (k!("key-n"), k!("help-pl-new")),
            (k!("key-d"), k!("help-pl-delete")),
            (k!("key-slash"), k!("help-pl-filter")),
            (k!("key-s"), k!("help-media-sort")),
            (k!("key-r"), k!("help-media-reload")),
            (k!("key-shift-r"), k!("help-pl-reload-root")),
        ]
    }

    fn event(&mut self, event: &AppEvent, ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        match event {
            AppEvent::Playlists(ev) => {
                if let Some(ed) = self.editor.as_mut()
                    && let Some(out) = ed.on_event(ev, ctx)
                {
                    match out {
                        editor::Outcome::Stay => {}
                        editor::Outcome::Close => self.editor = None,
                        editor::Outcome::Saved => {
                            self.detail = None;
                            self.load(ctx);
                        }
                    }
                    return Ok(Control::Changed);
                }
                match &**ev {
                    PlEvent::Listed(id, r) if *id == self.request => {
                        self.loading = false;
                        self.loaded = true;
                        match r {
                            Ok(rows) => {
                                self.rows = rows.clone();
                                self.error = None;
                                if let Some(r) = self.reveal.take()
                                    && !self.reveal(&r, ctx)
                                {
                                    ctx.open(Modal::Info(Info {
                                        title: tr!("pl-reveal-missing-title"),
                                        lines: vec![tr!("pl-reveal-missing", playlist = r)],
                                    }));
                                }
                                // La sélection reste sur sa playlist si elle existe encore.
                                let vis = self.visible();
                                let still = self.selected.as_ref().is_some_and(|k| vis.iter().any(|p| key_of(p) == *k));
                                if !still {
                                    self.selected = vis.first().map(|p| key_of(p));
                                }
                                self.detail = None;
                                self.load_detail(ctx);
                            }
                            Err(e) => self.error = Some(e.clone()),
                        }
                    }
                    PlEvent::Settled(id) if *id == self.moves => self.load_detail(ctx),
                    PlEvent::Detail(id, r) if *id == self.detail_req => {
                        let key = self.selected.clone().unwrap_or_default();
                        if let Ok(x) = r {
                            // Le pool de ce qui est sur le disque (ou appliqué).
                            let toml = if x.file_toml.is_empty() { x.applied_toml.clone() } else { x.file_toml.clone() };
                            let (id, channel, reference) = (self.detail_req, ctx.channel.clone(), x.rel_path.clone());
                            send(ctx, async move {
                                PlEvent::DetailPool(id, crate::rpc::preview_pool(channel, toml, reference, 1).await)
                            });
                        }
                        self.detail = Some((key, r.clone()));
                    }
                    PlEvent::DetailPool(id, r) if *id == self.detail_req => {
                        self.pool = Some((self.selected.clone().unwrap_or_default(), r.clone()));
                    }
                    PlEvent::Opened(id, r, files) if Some(*id) == self.opening => {
                        self.opening = None;
                        match r {
                            Ok(x) if x.rel_path.is_empty() => ctx.open(Modal::Info(Info {
                                title: tr!("pl-no-file"),
                                lines: vec![tr!("pl-edit-no-file")],
                            })),
                            Ok(x) => {
                                let mut ed = Editor::open(x, self.rows.clone(), ctx);
                                if !files.is_empty() {
                                    ed.add_files(files, ctx);
                                }
                                self.editor = Some(ed);
                            }
                            Err(e) => ctx.open(Modal::Info(Info { title: tr!("pl-open-failed"), lines: vec![e.clone()] })),
                        }
                    }
                    _ => return Ok(Control::Continue),
                }
                return Ok(Control::Changed);
            }
            // Une action finie (suppression, relecture…) : la liste est relue.
            AppEvent::ActionDone(Ok(_)) => {
                self.load(ctx);
                return Ok(Control::Changed);
            }
            AppEvent::Media(..) | AppEvent::MediaTyped(..) => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.on_media_event(event, ctx);
                    return Ok(Control::Changed);
                }
                return Ok(Control::Continue);
            }
            AppEvent::Event(_) => {}
            _ => return Ok(Control::Continue),
        }
        let AppEvent::Event(e) = event else { return Ok(Control::Continue) };

        if let Some(ed) = self.editor.as_mut() {
            if let editor::Outcome::Close = ed.on_key(e, ctx) {
                self.editor = None;
                self.detail = None;
                self.load(ctx);
            }
            return Ok(Control::Changed);
        }

        let Event::Key(k) = e else { return Ok(Control::Continue) };
        if k.kind != KeyEventKind::Press {
            return Ok(Control::Continue);
        }

        if let Some(sel) = self.new_mode {
            match k.code {
                KeyCode::Esc => self.new_mode = None,
                KeyCode::Up => self.new_mode = Some(sel.saturating_sub(1)),
                KeyCode::Down => self.new_mode = Some((sel + 1).min(MODES.len() - 1)),
                KeyCode::Enter => {
                    self.new_mode = None;
                    self.editor = Some(Editor::new_playlist(MODES[sel], self.rows.clone(), ctx));
                }
                _ => return Ok(Control::Unchanged),
            }
            return Ok(Control::Changed);
        }

        if self.filtering {
            match k.code {
                KeyCode::Enter => {
                    self.filtering = false;
                    self.filter.focus.set(false);
                }
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter.focus.set(false);
                    self.filter.set_text("");
                }
                _ => {
                    self.filter.handle(e, Regular);
                }
            }
            let idx = self.index();
            self.select(idx, ctx);
            return Ok(Control::Changed);
        }

        let idx = self.index();
        match k.code {
            KeyCode::Up => self.select(idx.saturating_sub(1), ctx),
            KeyCode::Down => self.select(idx + 1, ctx),
            KeyCode::PageUp => self.select(idx.saturating_sub(15), ctx),
            KeyCode::PageDown => self.select(idx + 15, ctx),
            KeyCode::Home => self.select(0, ctx),
            KeyCode::End => self.select(usize::MAX, ctx),
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.focus.set(true);
            }
            KeyCode::Char('s') => self.sort = (self.sort + 1) % SORTS.len(),
            KeyCode::Char('r') => self.load(ctx),
            KeyCode::Char('R') => ctx.open(Modal::Confirm(Confirm::new(
                tr!("pl-reload-title"),
                vec![tr!("pl-reload-body")],
                tr!("pl-reload-yes"),
                Action::ReloadPlaylists,
            ))),
            KeyCode::Char('n') => self.new_mode = Some(0),
            KeyCode::Char('d') => self.delete(ctx),
            KeyCode::Enter | KeyCode::Char('e') => {
                if let Some(p) = self.current() {
                    if p.rel_path.is_empty() {
                        ctx.open(Modal::Info(Info { title: tr!("pl-no-file"), lines: vec![tr!("pl-edit-no-file")] }));
                    } else {
                        let r = p.rel_path.clone();
                        self.open_editor(r, Vec::new(), ctx);
                    }
                }
            }
            _ => return Ok(Control::Continue),
        }
        Ok(Control::Changed)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        if let Some(ed) = self.editor.as_mut() {
            ed.render(area, buf, ctx);
            return Ok(());
        }
        let s = Styles(&ctx.theme);
        Block::new().style(s.base()).render(area, buf);
        if area.width >= 110 {
            let [l, r] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
            self.render_list(l, buf, ctx);
            self.render_detail(r, buf, ctx);
        } else {
            let detail_h = (area.height / 2).min(12);
            let [l, r] = Layout::vertical([Constraint::Fill(1), Constraint::Length(detail_h)]).areas(area);
            self.render_list(l, buf, ctx);
            self.render_detail(r, buf, ctx);
        }
        if self.opening.is_some() {
            let s = Styles(&ctx.theme);
            Paragraph::new(Span::styled(format!(" {} ", tr!("pl-opening")), s.warn()))
                .render(Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1), buf);
        }
        if let Some(sel) = self.new_mode {
            let s = Styles(&ctx.theme);
            self.render_new_mode(area, buf, &s, sel);
        }
        Ok(())
    }
}

/// « 2 règles · 1 groupe » (seulement ce qui existe) ; `None` = rien.
fn used_count(p: &PlaylistSummary) -> Option<String> {
    let mut parts = Vec::new();
    if !p.rules.is_empty() {
        parts.push(tr!("pl-used-rules-n", n = p.rules.len()));
    }
    if !p.groups.is_empty() {
        parts.push(tr!("pl-used-groups-n", n = p.groups.len()));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn editor_ref_is(ed: &Editor, reference: &str) -> bool {
    ed.reference_is(reference)
}
