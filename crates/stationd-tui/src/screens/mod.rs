//! Registre des écrans. L'ordre donne les touches d'accès `1`..`8`.

mod agenda;
mod antenne;
mod controle;
mod editor;
mod medias;
mod ops;
mod picker;
mod planned;
mod playlists;
mod ruleform;
mod systeme;
mod tagform;
mod tags;
mod typepick;

use std::sync::atomic::{AtomicU64, Ordering};

use crate::k;
use crate::screen::Screen;
use planned::Planned;
pub use agenda::AgEvent;
pub use ruleform::diag_text as grid_diag_text;
pub use playlists::PlEvent;
pub use systeme::SysEvent;
pub use tags::TagsEvent;

/// Rang de l'écran Playlists dans le registre (ouvert depuis Médias).
pub const PLAYLISTS: usize = 2;

/// Identifiant unique d'un demandeur (écran, sélecteur, éditeur) : ses
/// réponses ne sont prises par personne d'autre.
pub fn next_owner() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub fn registry() -> Vec<Box<dyn Screen>> {
    vec![
        Box::new(antenne::Antenne::default()),
        Box::new(controle::Controle::default()),
        Box::new(playlists::Playlists::default()),
        Box::new(agenda::Agenda::default()),
        Box::new(medias::Medias::default()),
        Box::new(tags::Tags::default()),
        Box::new(systeme::Systeme::default()),
        Box::new(Planned {
            title: k!("screen-plugins"),
            lot: 8,
            summary: &[k!("planned-plugins-1"), k!("planned-plugins-2")],
            plugin: None,
        }),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_playlists_screen_is_where_medias_sends_its_selection() {
        let r = super::registry();
        assert_eq!(r[super::PLAYLISTS].title(), crate::tr!("screen-playlists"));
    }
}
