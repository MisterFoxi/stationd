//! Registre des écrans. L'ordre donne les touches d'accès `1`..`8`.

mod antenne;
mod controle;
mod ops;
mod planned;

use crate::k;
use crate::screen::Screen;
use planned::Planned;

pub fn registry() -> Vec<Box<dyn Screen>> {
    vec![
        Box::new(antenne::Antenne::default()),
        Box::new(controle::Controle::default()),
        Box::new(Planned {
            title: k!("screen-playlists"),
            lot: 4,
            summary: &[k!("planned-playlists-1"), k!("planned-playlists-2"), k!("planned-playlists-3")],
            plugin: None,
        }),
        Box::new(Planned {
            title: k!("screen-agenda"),
            lot: 6,
            summary: &[k!("planned-agenda-1"), k!("planned-agenda-2"), k!("planned-agenda-3")],
            plugin: None,
        }),
        Box::new(Planned {
            title: k!("screen-media"),
            lot: 4,
            summary: &[k!("planned-media-1"), k!("planned-media-2"), k!("planned-media-3")],
            plugin: None,
        }),
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
