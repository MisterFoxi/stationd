//! Registre des écrans. L'ordre donne les touches d'accès `1`..`8`.

mod antenne;
mod planned;

use crate::screen::Screen;
use planned::Planned;

pub fn registry() -> Vec<Box<dyn Screen>> {
    vec![
        Box::new(antenne::Antenne::default()),
        Box::new(Planned {
            title: "Contrôle",
            lot: "lot 2",
            summary: &[
                "Diffusion : pause, reprise, suivant, veille, réveil",
                "Overrides : pousser, lister, vider",
                "Live : couper le DJ, ouvrir / fermer un créneau",
                "File queue, scan de la bibliothèque, plugins, arrêt opérateur",
            ],
            plugin: None,
        }),
        Box::new(Planned {
            title: "Playlists",
            lot: "lot 4",
            summary: &[
                "Liste : mode, pool, règles et groupes qui la référencent",
                "Formulaire par mode, aperçu du pool en direct",
                "Enregistrement par stationd (révision, conflits)",
            ],
            plugin: None,
        }),
        Box::new(Planned {
            title: "Agenda",
            lot: "lot 6",
            summary: &[
                "Jour : timeline, bases, rendez-vous, every projetés",
                "Semaine : 7 colonnes, pas 15/30/60 min",
                "Couverture de la grille, édition de règle",
            ],
            plugin: None,
        }),
        Box::new(Planned {
            title: "Médias",
            lot: "lot 4",
            summary: &[
                "Recherche, filtres (Type en premier), tri, pagination",
                "Fiche média, statistiques de diffusion",
                "Scan avec avancement, Type en lot (touche t)",
            ],
            plugin: None,
        }),
        Box::new(Planned {
            title: "Tags",
            lot: "lot 8",
            summary: &[
                "Types : valeurs déclarées, effectifs, médias sans Type",
                "Tags libres : créer, renommer, fusionner",
            ],
            plugin: Some("tags"),
        }),
        Box::new(Planned {
            title: "Système",
            lot: "lot 8",
            summary: &[
                "Santé : stationd, Liquidsoap, Icecast (mounts), live",
                "Événements en direct, statistiques de diffusion",
            ],
            plugin: None,
        }),
        Box::new(Planned {
            title: "Plugins",
            lot: "lot 8",
            summary: &[
                "Vues déclarées par les plugins chargés",
                "Base de chaque plugin (info, requête en lecture seule)",
            ],
            plugin: None,
        }),
    ]
}
