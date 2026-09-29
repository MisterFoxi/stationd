//! Registre des écrans. L'ordre donne les touches d'accès `1`..`8`.

mod antenne;
mod controle;
mod editor;
mod medias;
mod ops;
mod picker;
mod planned;
mod playlists;
mod tagform;

use std::sync::atomic::{AtomicU64, Ordering};

use crate::k;
use crate::screen::Screen;
use planned::Planned;
pub use playlists::PlEvent;

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
        Box::new(Planned {
            title: k!("screen-agenda"),
            lot: 6,
            summary: &[k!("planned-agenda-1"), k!("planned-agenda-2"), k!("planned-agenda-3")],
            plugin: None,
        }),
        Box::new(medias::Medias::default()),
        Box::new(Planned {
            title: k!("screen-tags"),
            lot: 8,
            summary: &[k!("planned-tags-1"), k!("planned-tags-2")],
            plugin: Some("tags"),
        }),
        Box::new(Planned {
            title: k!("screen-system"),
            lot: 8,
            summary: &[k!("planned-system-1"), k!("planned-system-2")],
            plugin: None,
        }),
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
