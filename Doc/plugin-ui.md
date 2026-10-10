# Onglets TUI déclarés par les plugins — V1

Le plugin fournit ses vues ; la TUI reste un client gRPC générique et ne connaît
ni le nom du plugin ni son schéma SQL. Les tableaux sont en lecture seule.
Le type plugin_config ouvre un éditeur générique de configuration :
voir [Configuration des plugins](plugin-config.md).

## Déclaration

Natif : implémenter `Plugin::ui_tabs() -> Result<Vec<UiTab>, String>`.
WASM : export facultatif `ui_tabs`, entrée vide, sortie JSON :

```json
[
  {
    "id": "plays",
    "title": "Diffusions",
    "description": "Derniers passages",
    "sql": "SELECT media, plays FROM play_count ORDER BY plays DESC LIMIT 200"
  }
]
```

Sans export : aucun onglet ; les anciens plugins fonctionnent toujours.
La découverte se fait après les migrations et `on_load`, une fois par
chargement. Le host refuse les effets de bord pendant cet appel : écriture DB,
contrôle et overrides. Un descripteur invalide provoque un état `failed` avec
la raison. La SQL est vérifiée à la lecture : une erreur s'affiche dans la vue
sans mettre le plugin en quarantaine.

Les tableaux nécessitent la capacité `db` ; un onglet de configuration ne la nécessite pas. Maximum quatre onglets par plugin ; identifiant
ASCII alphanumérique, `-` ou `_`, non vide, unique, limité à 64 octets.
Titre non vide : 64 caractères ; description : 512 caractères. Les caractères
de contrôle sont refusés. SQL non vide : 16 KiB ; sortie WASM : 64 KiB.
Les titres et descriptions viennent du plugin, sans traduction automatique.

## Transport et confinement

`PluginService.List` et `Control` ajoutent `PluginInfo.tabs` : id, titre,
description. La SQL reste côté daemon. `ReadTab(name, tab_id)` retourne les
colonnes et cellules typées de `PluginDbQueryResponse`.

L'identité est `(nom déclaré du plugin, id de l'onglet)`. Le plugin doit être
chargé et l'onglet déclaré. Une connexion séparée en lecture seule consulte
sa base hors de la boucle de l'acteur, sous les protections de `DbQuery` :
pas de mutation ou d'ATTACH. Les limites configurées sont plafonnées à 1000
lignes et 1000 ms pour les vues. Dépasser le quota est une erreur explicite ;
le plugin borne ses vues avec `LIMIT`.

Les descripteurs restent en mémoire après arrêt/échec/quarantaine : les vues
restent visibles et indisponibles. Un rechargement réussi remplace la liste.
Ils ne sont pas persistés : après redémarrage du daemon, seuls les plugins
ayant déclaré leurs vues lors d'un chargement réussi ont des onglets.

## Utilisation dans la TUI

Les huit écrans intégrés gardent leur position ; les vues de plugins suivent.
`8 Plugins` liste tous les plugins, leurs capacités, états, erreurs et onglets.
Entrée ouvre leur première vue ; `s/x/r/l` contrôlent leur cycle de vie.

- `1`–`9` : accès direct aux neuf premiers écrans.
- `F6` / `Maj+F6`, ou `Ctrl+PgDown` / `Ctrl+PgUp` : suivant / précédent,
  avec retour au début/à la fin. Les modales et champs de saisie gardent la main.
- Barre d'onglets : fenêtre autour de l'écran actif avec flèches de débordement.
- Dans les vues de plugins, Tab / n passe à la vue suivante, Maj+Tab / p revient
  à la précédente ; la navigation boucle entre les vues des plugins. Ces touches
  servent aussi dans les terminaux qui interceptent F6 (notamment VS Code).
- Vue : haut/bas, Home/End pour les lignes ; gauche/droite pour les colonnes ;
  `r` pour actualiser.
- Actualisation toutes les cinq secondes pour la vue active, une requête
  simultanée par vue. Les réponses arrivent aussi aux vues devenues inactives.
- Erreur : dernières données conservées et marquées anciennes ; sans données,
  l'erreur seule s'affiche. Une vue retirée ramène au catalogue Plugins.

`play-stats` fournit les vues [Diffusions médias, évolution et artistes/albums](media-stats.md),
ainsi que les anciennes sélections ; `listener-stats` fournit quatre vues :

- **Audience** : synthèse par flux, moyenne, pic, minimum, cumul des
  observations, relevés valides, échecs, pourcentage de collecte et premier/dernier relevé.
- **Évolution** : regroupement chronologique par heure, jour, semaine (lundi),
  mois ou année ; mêmes indicateurs de collecte.
- **Habitudes d’écoute** : regroupement par heure du jour ou jour de semaine
  (1=lundi, 7=dimanche), avec les mêmes statistiques sur la période.
- **Géographie** : pays, région ou ville, avec moyenne, pic, minimum,
  observations cumulées et part d’audience. Chaque regroupement temporel
  peut être croisé avec la géographie, par exemple pays et jour.

Dans une vue, **f** ouvre le formulaire de filtres : champs et choix côte à côte,
avec un aperçu des bornes UTC. ↑/↓ sélectionne un champ, ←/→ ou 1–9 change
un choix, Entrée ouvre le calendrier pour une date ou modifie un texte,
Suppr restaure le défaut. **Ctrl+S** applique ; Esc annule.

Le calendrier natif `rat-widget` affiche le mois dans la langue de la TUI.
Les flèches parcourent les jours ; PgUp/PgDown change de mois, Ctrl+PgUp/PgDown
change d’année. Home/End sélectionne le premier/dernier jour du mois.
Tab passe aux heures, minutes et secondes : ↑/↓ ajuste ou saisir deux chiffres.
`t` choisit aujourd’hui, `n` l’instant courant, `0` minuit. Pour la borne de fin,
`e` inclut la journée sélectionnée en choisissant le lendemain à minuit.
Entrée confirme la date et revient au formulaire ; Esc ferme sans modifier.
Choisir une borne passe automatiquement en période personnalisée ; l’autre
borne est initialisée depuis la période présélectionnée. Les heures invalides
et une fin antérieure ou égale au début empêchent la validation.

Périodes : aujourd’hui (UTC), dernières 24 h, 7/30/90/365 jours, tout
l’historique conservé ou période personnalisée. Pour une période personnalisée,
choisir les deux dates dans le calendrier (contrat **YYYY-MM-DD HH:MM:SS UTC**) : début inclus,
fin exclue. La période, les dates et le flux sont partagés entre les vues du
même plugin. Le type de cumul et le niveau géographique restent propres à
chaque vue. Un flux vide sélectionne tous les flux, affichés séparément.

Toutes les dates et tranches horaires sont en UTC. Les périodes aux bornes
peuvent être partielles ; les périodes sans relevé ne produisent aucune ligne.
La rétention reste de 30 jours par défaut (`retention_days`, de 1 à 365).
Le champ est éditable dans la vue Configuration des plugins.
Sélectionner une année ne recrée pas l’historique supprimé. Premier_UTC et
Dernier_UTC indiquent les bornes des données réellement présentes dans la synthèse.
Dernier_effectif est le dernier relevé de la période choisie, qui peut être ancien.
Les tableaux sont bornés à 1000 lignes ; affiner la période ou le flux si la
limite est atteinte. La géographie classe les lignes par période puis moyenne.

Les moyennes sont arithmétiques par relevé réussi : les zéros comptent,
les collectes échouées sont exclues. Pour un lieu, tous les relevés réussis du
flux et du groupe temporel comptent au dénominateur, même quand ce lieu est absent.
Les villes sont d’abord additionnées à chaque relevé pour obtenir un pays ou
une région ; le pic porte sur ce total. La part est la proportion des observations
d’auditeurs du flux dans le groupe temporel. **Observations_auditeurs** est la
somme des effectifs des relevés : elle dépend de la fréquence de collecte et
ne représente ni des personnes uniques, ni des sessions, ni un temps d’écoute.
Les inconnus sont conservés avec leur statut GeoIP. Les attributions MaxMind
et DB-IP restent dans la description de la vue Géographie.

Reconstruire le daemon, la TUI et le guest WASM, puis recharger le plugin pour
bénéficier des filtres. Aucun reset de base. `play-stats` ajoute sa migration
des diffusions réelles tout en conservant les anciens compteurs de sélection.

### Filtres déclaratifs

Un onglet peut fournir `filters` (12 champs maximum), une liste de
`{key, label, kind, default_value, options, shared}`. Types : `choice`
(32 choix maximum), `text` et `datetime` (date UTC exacte ou chaîne vide).
Clés ASCII alphanumériques/underscore, uniques, de 64 octets maximum ; valeurs
sans caractères de contrôle, de 256 octets maximum. `shared: true` partage
le choix entre les vues du même plugin, pendant la session TUI.

Les valeurs sont envoyées dans `PluginReadTabRequest.filters`, validées
contre les descripteurs et liées comme paramètres SQL nommés (`:key`).
La SQL reste côté serveur ; clés inconnues, choix invalides et dates invalides
sont refusés. Les filtres absents prennent leur valeur par défaut. Les bornes
standards `from`/`to` doivent être ordonnées lorsqu’elles sont actives (période
personnalisée ou absence de filtre `period`) ;
`period=Personnalisée` exige les deux. Les clients antérieurs continuent de
lire les vues avec leurs valeurs par défaut. Les onglets sans filtres gardent
leur comportement existant.

## Vérification

Depuis le worktree, dans le conteneur de développement :

```sh
cargo test --locked --workspace --lib --bins
cargo build --locked --manifest-path plugins/play-stats-wasm/Cargo.toml --release --target wasm32-unknown-unknown
cargo build --locked --manifest-path plugins/listener-stats-wasm/Cargo.toml --release --target wasm32-unknown-unknown
STATIOND_TEST_UI_WASM="$PWD/plugins/play-stats-wasm/target/wasm32-unknown-unknown/release/play_stats_wasm.wasm" cargo test --locked --lib real_wasm_ui_tabs -- --ignored
```

## Tableaux de bord natifs

Audience et les trois vues médias ouvrent un tableau de bord par défaut :
quatre indicateurs, une courbe sélectionnable et des classements à barres.
Sur un terminal large, courbe et classement sont côte à côte ; ils s’empilent
si la hauteur le permet. Sur un petit terminal, les panneaux restent
accessibles séparément.

- `v` : graphiques / tableau détaillé, avec les mêmes filtres.
- `h` : aperçu / carte de chaleur / classement.
- ←/→ : point de courbe ; ↑/↓ : entrée du classement.
- Dans la carte de chaleur : flèches pour choisir jour et heure et lire la
  valeur exacte. `·` indique une donnée inconnue ; `_` un zéro mesuré.
- `f` : période et regroupements ; `r` : actualiser.

La synthèse d’audience est calculée pour un seul flux, nommé dans le bandeau.
Sans filtre de flux, le premier flux par ordre alphabétique est affiché ;
choisir un autre flux avec `f`. Le tableau détaillé conserve tous les flux.
Les pays inconnus gardent leur statut de géolocalisation. Les attributions
MaxMind/DB-IP restent dans la vue Géographie.

Les indicateurs utilisent toute la période sélectionnée. La courbe affiche
au plus les 600 dernières tranches observées, le classement les 100 premières
entrées. Avec Cumul=Total, la courbe choisit automatiquement heure (jusqu’à
2 jours), jour (jusqu’à 90 jours) ou mois. Les bornes réelles de la requête
sont affichées en UTC. Les tranches sans collecte ne deviennent pas des zéros
et les trous interrompent les segments de la courbe. Les valeurs inconnues
s’affichent avec `—`. Les durées du tableau sont affichées en h/min/s.

### Contrat du jeu de données visuel

Un onglet table peut ajouter `dashboard_sql` (SQL en lecture seule, 16 Kio
maximum, gardée sur le serveur). Le descripteur gRPC annonce seulement
`has_dashboard`. `ReadTab.dashboard=true` choisit cette requête déclarée ;
false conserve le tableau original et la compatibilité des anciens clients.
Aucune SQL n’est fournie par la TUI. Validation des filtres, plafonds de
1000 lignes / 1000 ms et restrictions SQLite restent identiques.

Colonnes obligatoires : `Section`, `Scope`, `Label`, `Bucket`, `Value`, `Unit`,
`Samples`. Sections : `summary` (quatre indicateurs), `series` (tranches UTC),
`ranking` (ordre défini par le plugin), `heatmap` (Label=jour 1–7,
Bucket=heure 00–23), `period` (Label=début inclus, Bucket=fin exclue), `note`.
`Value=NULL` signifie inconnue ; `0` reste un zéro. `Unit=s` est affichée en
h/min/s, `%` en pourcentage. L’interface ne calcule pas de moyennes à partir
d’agrégats et ne déduit pas la présence d’un tableau de bord du nom du plugin.

Les quatre vues du plugin d’écoute déclarent désormais une présentation
visuelle : Audience, Évolution, Habitudes d’écoute et Géographie. Les barres
géographiques suivent le choix Pays/Région/Ville. La bascule `v` indique
explicitement « Tableau de bord » ou « Tableau détaillé », même sans données.
L’aide ne propose la bascule que si le daemon annonce un jeu de données
visuel pour la vue. Les anciennes sélections médias restent un tableau.
