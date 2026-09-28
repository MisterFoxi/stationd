//! Dialogues modaux : confirmation et formulaire.
//!
//! Une modale capture toutes les entrées (dossier §6) : rien ne traverse vers
//! l'écran dessous. Toute action qui touche l'antenne passe par une
//! confirmation qui nomme l'objet exact, « Annuler » sélectionné par défaut.

use rat_salsa::SalsaContext;
use rat_theme4::WidgetStyle;
use rat_widget::event::{HandleEvent, Regular};
use rat_widget::text::HasScreenCursor;
use rat_widget::text_input::{TextInput, TextInputState};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::{StatefulWidget, Widget};
use ratatui_crossterm::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::clear::Clear;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::action::Action;
use crate::app::Global;
use crate::style::Styles;
use crate::{fit, tr};

/// Ce qu'une entrée a fait de la modale.
pub enum Outcome {
    /// Rien d'utile.
    Unchanged,
    /// À redessiner.
    Changed,
    /// Fermée sans rien faire.
    Cancel,
    /// Fermée : exécuter l'action.
    Submit(Action),
    /// Remplacée par une autre modale (confirmation en deux temps).
    Replace(Box<Modal>),
}

pub enum Modal {
    Confirm(Confirm),
    Form(Form),
}

impl Modal {
    pub fn handle(&mut self, event: &Event) -> Outcome {
        match self {
            Modal::Confirm(c) => c.handle(event),
            Modal::Form(f) => f.handle(event),
        }
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) {
        match self {
            Modal::Confirm(c) => c.render(area, buf, ctx),
            Modal::Form(f) => f.render(area, buf, ctx),
        }
    }
}

fn press(e: &Event) -> Option<&KeyEvent> {
    match e {
        Event::Key(k) if k.kind == KeyEventKind::Press => Some(k),
        _ => None,
    }
}

/// Zone centrée de `w` × `h` dans `area` (bornée par elle).
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn frame<'a>(title: &str, s: &Styles, danger: bool) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if danger { s.error() } else { s.accent() })
        .title(Span::styled(format!(" {title} "), s.title()))
}

/// Lignes occupées par `text` coupé aux mots sur `width` colonnes (comme
/// `Paragraph` avec `Wrap`) : la boîte a juste la hauteur de son texte.
fn wrapped_lines(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut lines = 1;
    let mut col = 0;
    for word in text.split_whitespace() {
        let w = Span::raw(word).width();
        let need = if col == 0 { w } else { col + 1 + w };
        if need <= width {
            col = need;
        } else if w <= width {
            lines += 1;
            col = w;
        } else {
            // Mot plus long que la ligne : coupé en morceaux.
            let rest = if col == 0 { w } else { lines += 1; w };
            lines += (rest - 1) / width;
            col = (rest - 1) % width + 1;
        }
    }
    lines
}

// --- confirmation ----------------------------------------------------------------

pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    /// Libellé du bouton qui exécute (« Passer au suivant »…).
    pub yes: String,
    pub action: Action,
    /// Action dangereuse : cadre rouge.
    pub danger: bool,
    /// Seconde confirmation, demandée après le premier « oui ».
    pub then: Option<Box<Confirm>>,
    yes_selected: bool,
}

impl Confirm {
    pub fn new(title: String, lines: Vec<String>, yes: String, action: Action) -> Self {
        Self { title, lines, yes, action, danger: false, then: None, yes_selected: false }
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn then(mut self, second: Confirm) -> Self {
        self.then = Some(Box::new(second));
        self
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        let Some(k) = press(event) else { return Outcome::Unchanged };
        match k.code {
            KeyCode::Esc => Outcome::Cancel,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                self.yes_selected = !self.yes_selected;
                Outcome::Changed
            }
            KeyCode::Enter if self.yes_selected => match self.then.take() {
                Some(second) => Outcome::Replace(Box::new(Modal::Confirm(*second))),
                None => Outcome::Submit(self.action.clone()),
            },
            KeyCode::Enter => Outcome::Cancel,
            _ => Outcome::Unchanged,
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) {
        let s = Styles(&ctx.theme);
        let w = 64.min(area.width.saturating_sub(4)).max(30);
        let inner_w = w.saturating_sub(2) as usize;
        let text_h: u16 = self
            .lines
            .iter()
            .map(|l| wrapped_lines(l, inner_w) as u16)
            .sum();
        // Bordures + texte + ligne vide + boutons.
        let box_a = centered(area, w, text_h + 4);
        Clear.render(box_a, buf);
        let block = frame(&self.title, &s, self.danger)
            .title_bottom(Span::styled(format!(" {} ", tr!("dialog-confirm-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);
        let [text_a, _, buttons_a] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Length(1)]).areas(inner);
        Paragraph::new(self.lines.iter().map(|l| Line::raw(l.clone())).collect::<Vec<_>>())
            .wrap(Wrap { trim: true })
            .render(text_a, buf);
        let cancel = tr!("dialog-cancel");
        let (yes_style, no_style) = if self.yes_selected {
            (if self.danger { s.error() } else { s.tab_active() }, s.muted())
        } else {
            (s.muted(), s.tab_active())
        };
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {cancel} "), no_style),
            Span::raw("   "),
            Span::styled(format!(" {} ", self.yes), yes_style),
        ]))
        .render(buttons_a, buf);
    }
}

// --- formulaire ------------------------------------------------------------------

pub enum Input {
    Text(Box<TextInputState>),
    /// Choix fermé : (libellé affiché, valeur).
    Choice { options: Vec<(String, String)>, index: usize },
}

pub struct Field {
    pub label: String,
    pub input: Input,
}

impl Field {
    pub fn text(label: String, initial: &str) -> Self {
        let mut st = TextInputState::new();
        st.set_text(initial);
        Self { label, input: Input::Text(Box::new(st)) }
    }

    pub fn choice(label: String, options: Vec<(String, String)>) -> Self {
        Self { label, input: Input::Choice { options, index: 0 } }
    }

    /// Texte saisi (sans les blancs autour) ou valeur choisie.
    pub fn value(&self) -> String {
        match &self.input {
            Input::Text(st) => st.text().trim().to_string(),
            Input::Choice { options, index } => options.get(*index).map(|o| o.1.clone()).unwrap_or_default(),
        }
    }
}

type Build = Box<dyn Fn(&[Field]) -> Result<Action, String>>;
type ConfirmWith = Box<dyn Fn(&Action) -> Option<Confirm>>;

pub struct Form {
    pub title: String,
    pub fields: Vec<Field>,
    focus: usize,
    error: Option<String>,
    build: Build,
    /// Confirmation demandée après validation (l'action touche l'antenne).
    confirm: Option<ConfirmWith>,
}

impl Form {
    /// `build` transforme les champs en action, ou dit ce qui manque (le
    /// formulaire reste ouvert, la saisie conservée).
    pub fn new(title: String, fields: Vec<Field>, build: impl Fn(&[Field]) -> Result<Action, String> + 'static) -> Self {
        let mut f = Self { title, fields, focus: 0, error: None, build: Box::new(build), confirm: None };
        f.sync_focus();
        f
    }

    /// Une fois le formulaire valide, `f` peut exiger une confirmation qui
    /// nomme ce qui va être fait.
    pub fn confirm_with(mut self, f: impl Fn(&Action) -> Option<Confirm> + 'static) -> Self {
        self.confirm = Some(Box::new(f));
        self
    }

    fn sync_focus(&mut self) {
        for (i, f) in self.fields.iter().enumerate() {
            if let Input::Text(st) = &f.input {
                st.focus.set(i == self.focus);
            }
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let n = self.fields.len();
        if n == 0 {
            return;
        }
        self.focus = if forward { (self.focus + 1) % n } else { (self.focus + n - 1) % n };
        self.sync_focus();
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        if let Some(k) = press(event) {
            match k.code {
                KeyCode::Esc => return Outcome::Cancel,
                KeyCode::Enter => {
                    return match (self.build)(&self.fields) {
                        Ok(a) => match self.confirm.as_ref().and_then(|f| f(&a)) {
                            Some(c) => Outcome::Replace(Box::new(Modal::Confirm(c))),
                            None => Outcome::Submit(a),
                        },
                        Err(e) => {
                            self.error = Some(e);
                            Outcome::Changed
                        }
                    };
                }
                KeyCode::Tab | KeyCode::Down => {
                    self.move_focus(true);
                    return Outcome::Changed;
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.move_focus(false);
                    return Outcome::Changed;
                }
                _ => {}
            }
            if let Some(Field { input: Input::Choice { options, index }, .. }) = self.fields.get_mut(self.focus) {
                let n = options.len().max(1);
                match k.code {
                    KeyCode::Left => *index = (*index + n - 1) % n,
                    KeyCode::Right | KeyCode::Char(' ') => *index = (*index + 1) % n,
                    _ => return Outcome::Unchanged,
                }
                return Outcome::Changed;
            }
            // Ctrl+… n'est pas du texte.
            if k.modifiers.contains(KeyModifiers::CONTROL) && !matches!(k.code, KeyCode::Char('a' | 'e' | 'u' | 'k' | 'w')) {
                return Outcome::Unchanged;
            }
        }
        if let Some(Field { input: Input::Text(st), .. }) = self.fields.get_mut(self.focus) {
            let r = st.handle(event, Regular);
            if r != rat_widget::event::TextOutcome::Continue {
                self.error = None;
                return Outcome::Changed;
            }
        }
        Outcome::Unchanged
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) {
        let s = Styles(&ctx.theme);
        let w = 64.min(area.width.saturating_sub(4)).max(34);
        // Bordures + (libellé, saisie) par champ + ligne d'erreur.
        let h = self.fields.len() as u16 * 2 + 3;
        let box_a = centered(area, w, h);
        Clear.render(box_a, buf);
        let block = frame(&self.title, &s, false)
            .title_bottom(Span::styled(format!(" {} ", tr!("dialog-form-keys")), s.muted()));
        let inner = block.inner(box_a);
        block.style(s.base()).render(box_a, buf);

        let mut rows: Vec<Constraint> = Vec::new();
        for _ in &self.fields {
            rows.push(Constraint::Length(1));
            rows.push(Constraint::Length(1));
        }
        rows.push(Constraint::Length(1)); // erreur
        let areas = Layout::vertical(rows).split(inner);
        let text_style: rat_widget::text::TextStyle = ctx.theme.style(WidgetStyle::TEXT);
        let mut cursor = None;
        for (i, f) in self.fields.iter_mut().enumerate() {
            let focused = i == self.focus;
            let label_a = areas[i * 2];
            let input_a = areas[i * 2 + 1];
            Paragraph::new(Span::styled(f.label.clone(), if focused { s.accent() } else { s.label() }))
                .render(label_a, buf);
            match &mut f.input {
                Input::Text(st) => {
                    // Repère de saisie : un champ vide reste visible.
                    let [mark_a, field_a] =
                        Layout::horizontal([Constraint::Length(2), Constraint::Fill(1)]).areas(input_a);
                    Span::styled("› ", if focused { s.accent() } else { s.muted() }).render(mark_a, buf);
                    TextInput::new().styles(text_style.clone()).render(field_a, buf, st);
                    if focused {
                        cursor = st.screen_cursor();
                    }
                }
                Input::Choice { options, index } => {
                    let label = options.get(*index).map(|o| o.0.clone()).unwrap_or_default();
                    let st = if focused { s.tab_active() } else { s.label() };
                    Paragraph::new(Line::from(vec![
                        Span::styled("◀ ", if focused { s.accent() } else { s.muted() }),
                        Span::styled(label, st),
                        Span::styled(" ▶", s.muted()),
                    ]))
                    .render(input_a, buf);
                }
            }
        }
        let n = self.fields.len();
        if let Some(e) = &self.error {
            Paragraph::new(Span::styled(fit::ellipsize(e, inner.width as usize), s.error())).render(areas[n * 2], buf);
        }
        ctx.set_screen_cursor(cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::wrapped_lines;

    #[test]
    fn wrapped_lines_counts_like_a_word_wrap() {
        assert_eq!(wrapped_lines("", 10), 1);
        assert_eq!(wrapped_lines("abc def", 7), 1);
        assert_eq!(wrapped_lines("abc defg", 7), 2);
        assert_eq!(wrapped_lines("abcdefghijkl", 5), 3);
        assert_eq!(wrapped_lines("« été » à l'antenne", 9), 2);
    }
}
