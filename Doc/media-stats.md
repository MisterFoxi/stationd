# Statistiques des médias — play-stats

Accès TUI : **8 · Plugins**, sélectionner `play-stats`, **Entrée**, puis
Tab/Shift+Tab pour les vues. **f** ouvre les filtres ; Entrée sur Début/Fin
ouvre le calendrier, Ctrl+S applique la période.

- **Diffusions médias** : passages réels, dernière diffusion, durée diffusée,
  passages complets, coupés, issue inconnue et fins non observées.
- **Médias / évolution** : mêmes mesures par heure, jour, semaine (lundi),
  mois, année, heure du jour ou jour de semaine ; Total est disponible.
- **Artistes / albums** : regroupement par média, artiste, album ou playlist,
  combinable avec chaque cumul temporel. Les albums sont distingués par artiste.
- **Sélections anciennes** : compteurs historiques antérieurs, conservés pour
  consultation. Ils comptaient des décisions de sélection, pas des passages
  confirmés ; ils ne sont pas mélangés aux nouvelles statistiques datées.

Périodes UTC : aujourd’hui, 24 h, 7/30/90/365 jours, Tout, Personnalisée.
Début inclus, fin exclue. Le champ Média recherche une sous-chaîne dans le
chemin, titre, artiste ou album. La période et cette recherche sont partagées
entre les trois nouvelles vues. Le regroupement, le cumul et le tri restent
propres à la vue. Tri par passages, durée ou audience ; au plus 1000 lignes.

Une diffusion commence à la première confirmation de mise à l’antenne de
Liquidsoap, et non à la sélection d’un candidat. Une fin confirmée fournit la
durée réellement diffusée ; les notifications répétées sont ignorées. L’UUID
identifie le média malgré un renommage ; sans UUID, le chemin sert d’identité.
Deux fichiers portant le même titre restent distincts. Une fin reçue sans
événement de début peut restaurer le passage à partir de ses métadonnées.

Les durées connues sont additionnées intégralement dans la tranche du début
(elles ne sont pas découpées aux limites de période). Sans fin observée,
la durée reste inconnue. La qualification complet/coupé utilise le verdict du
core ; une durée de média inconnue conserve une issue inconnue.

Audience moyenne et pic utilisent les relevés globaux reçus pendant le
passage et la période sélectionnée. Les zéros comptent ; ce sont des moyennes
par relevé, sans pondération par durée. Pause, arrêt, veille ou entrée en live
interrompent l’attribution au média. Après chargement/rechargement du plugin,
l’attribution attend un nouveau début réel. Aucun nombre de personnes uniques,
temps d’écoute ou détail géographique par média n’est déduit de ces relevés.

La collecte est best-effort depuis le chargement de cette version du plugin :
pas de reconstruction automatique du passé à partir du journal du core.
Les compteurs historiques de sélection restent inchangés lors de la migration.
`retention_days` conserve 365 jours par défaut (1 à 365, configuration TUI).
L’élagage intervient lors des événements de début/fin ; Tout ne recrée pas les
passages supprimés. La simulation ne nourrit pas les statistiques.

Validation dans le conteneur de développement :

```sh
cargo test --locked --test media_stats
cargo build --locked --manifest-path plugins/play-stats-wasm/Cargo.toml --release --target wasm32-unknown-unknown
STATIOND_TEST_UI_WASM="$PWD/plugins/play-stats-wasm/target/wasm32-unknown-unknown/release/play_stats_wasm.wasm" cargo test --locked --lib real_wasm_media_stats -- --ignored
```

Les trois nouvelles vues médias disposent d’un tableau de bord natif :
passages, durée, audience moyenne et pic, courbe, classement à barres et carte
de chaleur des heures de diffusion. `v` revient aux lignes détaillées ; `h`
parcourt les panneaux. La période reste commune aux deux présentations.
Le filtre Classement choisit la mesure de la courbe et des barres (Passages,
Durée ou Audience) ; Regrouper par choisit média, artiste, album ou playlist.
La carte de chaleur compte les passages, quels que soient le tri et le cumul.
Les 600 dernières tranches et les 100 premières entrées sont affichées ;
les indicateurs conservent les totaux de toute la période filtrée.

Media statistics filters include shared `include_genres` and `exclude_genres` text fields.
Enter exact genre names separated by commas (semicolons also work), for example
`TOPH, Annonces` in the exclusion field. An empty field imposes no restriction.
Inclusion matches any named genre; exclusion wins even on multi-genre media.
The filters apply before all table and dashboard calculations, including audience.
Genres are captured when playback starts, including catalogue genres contributed
by custom-tags. Existing history has unknown genres and is preserved: it remains
visible without an inclusion filter, but cannot match an included genre.
The legacy selection counters have no genre filter.
