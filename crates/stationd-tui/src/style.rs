//! Styles sémantiques de la TUI, dérivés de la palette rat-theme4.
//! Une couleur n'est jamais seule porteuse de sens : le texte dit toujours
//! l'état (RUNNING, « inconnu », « ancienne »…).

use rat_theme4::StyleName;
use rat_theme4::palette::Colors;
use rat_theme4::theme::SalsaTheme;
use ratatui_core::style::{Modifier, Style};

pub struct Styles<'a>(pub &'a SalsaTheme);

impl Styles<'_> {
    /// Fond et texte de base des conteneurs.
    pub fn base(&self) -> Style {
        self.0.style_style(Style::CONTAINER_BASE)
    }
    pub fn title(&self) -> Style {
        self.0.style_style(Style::TITLE)
    }
    pub fn label(&self) -> Style {
        self.0.p.fg_style(Colors::Gray, 2)
    }
    /// Donnée secondaire ou inconnue.
    pub fn muted(&self) -> Style {
        self.0.p.fg_style(Colors::Gray, 3)
    }
    pub fn ok(&self) -> Style {
        self.0.p.fg_style(Colors::Green, 2).add_modifier(Modifier::BOLD)
    }
    /// Alerte, projection, valeur ancienne.
    pub fn warn(&self) -> Style {
        self.0.p.fg_style(Colors::Orange, 2).add_modifier(Modifier::BOLD)
    }
    pub fn error(&self) -> Style {
        self.0.p.fg_style(Colors::Red, 2).add_modifier(Modifier::BOLD)
    }
    /// Veille, état calme.
    pub fn calm(&self) -> Style {
        self.0.p.fg_style(Colors::Blue, 2).add_modifier(Modifier::BOLD)
    }
    pub fn accent(&self) -> Style {
        self.0.p.fg_style(Colors::Cyan, 2).add_modifier(Modifier::BOLD)
    }
    /// Bandeau : fond distinct de la zone de travail.
    pub fn banner(&self) -> Style {
        self.0.p.bg_style(Colors::Gray, 7).patch(self.0.p.fg_style(Colors::TextLight, 0))
    }
    pub fn tab(&self) -> Style {
        self.muted()
    }
    pub fn tab_active(&self) -> Style {
        self.0.style_style(Style::SELECT).add_modifier(Modifier::BOLD)
    }
    pub fn tab_key(&self) -> Style {
        self.0.style_style(Style::KEY_BINDING)
    }
    pub fn tab_unavailable(&self) -> Style {
        self.0.style_style(Style::DISABLED)
    }
    pub fn border(&self) -> Style {
        self.0.p.fg_style(Colors::Gray, 3)
    }
}
