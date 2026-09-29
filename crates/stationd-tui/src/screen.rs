//! Contrat d'un écran (dossier §4.1, D5) : chaque écran est un module
//! indépendant inscrit dans le registre de `app`. Couche mince au-dessus du
//! modèle rat-salsa (rendu, événements, focus) qui ajoute ce que rat-salsa ne
//! connaît pas : touche d'accès, disponibilité, aide.

use anyhow::Error;
use rat_focus::FocusBuilder;
use rat_salsa::Control;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;

use crate::app::{AppEvent, Global};
use crate::store::Store;

/// Un écran peut exister sans être utilisable (ex. Tags sans plugin `tags`).
/// Il reste listé dans les onglets, grisé, avec la raison : rien ne disparaît
/// en silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable(String),
}

/// Une ligne d'aide : clés de traduction de la touche et de son effet.
pub type KeyHelp = (&'static str, &'static str);

pub trait Screen {
    /// Libellé de l'onglet (traduit).
    fn title(&self) -> String;

    fn availability(&self, _store: &Store) -> Availability {
        Availability::Available
    }

    /// L'écran devient actif : il lance ses lectures.
    fn enter(&mut self, _ctx: &mut Global) -> Result<(), Error> {
        Ok(())
    }

    /// La liaison avec stationd revient (après une perte, ou au premier
    /// contact) : l'écran actif relit ce qui avait échoué.
    fn reconnected(&mut self, _ctx: &mut Global) -> Result<(), Error> {
        Ok(())
    }

    /// Vrai quand un champ de saisie a le focus : les raccourcis globaux
    /// (1..8, q, ?) sont alors du texte et ne sont pas interceptés.
    fn captures_text(&self) -> bool {
        false
    }

    /// Déclare les widgets focusables de l'écran (rat-focus).
    fn build_focus(&self, _builder: &mut FocusBuilder) {}

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error>;

    fn event(&mut self, _event: &AppEvent, _ctx: &mut Global) -> Result<Control<AppEvent>, Error> {
        Ok(Control::Continue)
    }

    /// Raccourcis propres à l'écran (aide `?` et ligne de raccourcis).
    fn help(&self) -> &'static [KeyHelp] {
        &[]
    }
}
