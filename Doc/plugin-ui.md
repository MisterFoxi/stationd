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

`play-stats` fournit Diffusions ; `listener-stats` fournit quatre vues statistiques :

- **Audience** : synthèse des dernières 24 h par mount, dernier effectif observé,
  moyenne, pic, pourcentage de collectes réussies et date du dernier relevé UTC.
  Un dernier échec affiche NULL ; vérifier la date pour détecter une collecte arrêtée.
- **Audience / heure** : moyennes, pics, minima, relevés valides et échecs par
  heure UTC sur les dernières 24 h (au plus 1000 lignes).
- **Audience / jour** : mêmes indicateurs par jour UTC sur 30 jours, selon
  la rétention disponible (au plus 1000 lignes).
- **Géographie** : classement des lieux sur 24 h, avec moyenne, pic et part
  d'audience par mount ; lieux inconnus conservés (top 200).

Les moyennes sont arithmétiques par relevé réussi : les zéros comptent,
les collectes échouées sont exclues. Pour un lieu, tous les relevés réussis du
mount comptent au dénominateur, même quand ce lieu est absent. La part est
la proportion des observations d'auditeurs du mount sur la période ; ce ne
sont pas des auditeurs uniques ni une durée d'écoute. Les périodes aux bornes
peuvent être partielles ; les périodes sans relevé ne produisent aucune ligne.
Les vues Audience et Géographie sont limitées à 200 lignes.
Reconstruire les guests WASM puis recharger les plugins pour voir les onglets.
Aucun reset de base ni changement des migrations n'est nécessaire.

## Vérification

Depuis le worktree, dans le conteneur de développement :

```sh
cargo test --locked --workspace --lib --bins
cargo build --locked --manifest-path plugins/play-stats-wasm/Cargo.toml --release --target wasm32-unknown-unknown
cargo build --locked --manifest-path plugins/listener-stats-wasm/Cargo.toml --release --target wasm32-unknown-unknown
STATIOND_TEST_UI_WASM="$PWD/plugins/play-stats-wasm/target/wasm32-unknown-unknown/release/play_stats_wasm.wasm" cargo test --locked --lib real_wasm_ui_tabs -- --ignored
```
