# Agenda TUI : projection jour / semaine

L'onglet **4 Agenda** affiche la projection `ScheduleService.Preview` du
daemon. Il réutilise le contrat existant, déjà accessible avec
`stationctl schedule preview`. Aucun appel à `ResolveNext`, aucune lecture
SQLite ou TOML côté agenda et aucune modification des règles ou de la lecture.

## Construire et lancer

```sh
cargo build --features tui --bin stationd-tui
./target/debug/stationd-tui
# Pour un daemon distant :
./target/debug/stationd-tui --addr http://serveur:50051
```

Le daemon doit inclure le RPC Preview implémenté sur `dev` au commit
`f6af469`. Une version plus ancienne affiche une erreur explicite ; elle ne
produit pas un agenda vide présenté comme valide. Le fuseau vient de `Status`.

## Navigation

| Touche | Action dans Agenda |
|---|---|
| `4` | Ouvrir Agenda |
| `d` / `w` | Vue jour / semaine (lundi à dimanche) |
| Gauche / droite | Jour précédent / suivant, y compris dans une semaine |
| PgUp / PgDn ou `[` / `]` | Période précédente / suivante (jour ou semaine) |
| Haut / bas ou `k` / `j` | Sélectionner un créneau, avec défilement horaire |
| Home / End | Premier / dernier créneau |
| `+` / `-` | Zoom : créneaux de 15, 30 ou 60 minutes |
| `t` | Aujourd'hui et heure courante, dans le fuseau de la station |
| `g` | Saisir une date `YYYY-MM-DD`, puis Entrée |
| Entrée | Toutes les occurrences du créneau, avec UTC et heure locale |
| `r` | Actualiser, même en mode manuel |
| `a` | Rafraîchissement automatique / manuel |
| Tab / Shift-Tab ou `1`–`4` | Changer d'onglet |

Un écran d'au moins 105 colonnes affiche les sept jours côte à côte.
En dessous, la vue semaine conserve sa période mais présente le jour
sélectionné ; les flèches gauche/droite permettent de les parcourir. Agrandir
le terminal restitue les sept colonnes. La sélection est conservée au resize.

## Lecture des cellules

- `B` : rotation de base ; `D` : plage DayPart ; `F` : fallback.
- `*` : rendez-vous AtClock soft ; `!` : rendez-vous AtClock hard.
- `+N` : d'autres entrées existent dans ce créneau ; Entrée les montre toutes.
- Les noms de playlists sont utilisés quand `PlaylistList` les fournit ;
  sinon leur référence est affichée.

Les rendez-vous sont des **points**, sans durée supposée. Les intervalles
de source proviennent des changements renvoyés par Preview ; ils ne sont
pas des durées de titres, et leur borne n'est pas une instruction de coupe.
La granularité de projection du serveur est d'une minute ; le zoom regroupe
l'affichage sans arrondir les horaires présentés dans les détails.

Les règles **Every** dépendent de la lecture. Leur nombre configuré/activé est
rappelé sous l'agenda, et leurs paramètres restent consultables dans Grid.
Elles n'apparaissent pas à une heure inventée dans le calendrier.

## Fuseau, changements d'heure et erreurs

Les fenêtres sont calculées entre deux minuits **civils locaux**, pas en
ajoutant arbitrairement 86400 secondes. Une journée peut donc faire 23 h ou
25 h, une semaine 167 h ou 169 h. Les cellules conservent des bornes UTC.

L'heure répétée a des lignes `a` / `b` dans l'ordre des instants réels.
Une heure inexistante porte `DST gap`. Les détails montrent le fuseau IANA,
l'offset effectif et l'UTC pour lever l'ambiguïté. L'agenda ne rajoute pas de
rendez-vous absents de Preview : la politique de consommation/rejeu des
AtClock reste celle du moteur existant.

Le chargement est asynchrone et limité à une requête Preview en cours. Une
réponse à une ancienne période est ignorée après navigation. Une erreur de
rafraîchissement conserve les dernières données de **la même période**, avec
le marquage `STALE`. Changer de période efface ces données pour ne pas les
présenter sous de nouvelles dates. Le polling Preview ne tourne que lorsque
l'onglet Agenda est actif.

## Validation

```sh
cargo test --features tui
```

Les tests couvrent les jours de 23/25 h, les bornes de semaine, une transition
de 30 minutes, la navigation et les réponses tardives, les événements ponctuels,
les erreurs RPC et le rendu après redimensionnement. Le test d'intégration
`tests/agenda_preview.rs` appelle le contrat réel et vérifie que les compteurs
Every et les tokens AtClock persistés sont inchangés après plusieurs previews.
