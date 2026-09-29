# stationd — guide d'administration

Mis à jour le 2026-09-28.

Toutes les fonctions d'administration de stationd passent par `stationctl` (client gRPC du daemon) ; sur la machine de dev, le `Makefile` du dépôt enchaîne les séquences courantes dans le conteneur.

## Conventions

- **Où lancer.** En dev, les commandes `make` se lancent depuis la racine du dépôt sur l'hôte (`/data/dev/stationd`) : elles passent par le conteneur (`docker compose exec`). Dans l'image de dev, `stationctl` n'est pas dans le PATH (binaire `$STATIOND_BIN/stationctl`) : utiliser `make ctl A="…"`. En exploitation, `docker compose exec station stationctl …` (binaire dans `/usr/local/bin`).
- **Adresse.** Option globale `--addr <URL>`, avant la sous-commande ; défaut `http://127.0.0.1:50051` (`[server] grpc_bind`).
- **Aide.** `stationctl <commande> --help` pour toute commande ; `make` seul liste les cibles.
- **Codes de sortie.** 0 = succès ; ≠ 0 = refus ou erreur, avec la raison (transition sans sens, grille rejetée, `schedule check` avec une règle ✗, file pleine…) ; `station state` rend **3** quand stationd est arrêté par l'opérateur.
- **Durées.** Format court : `30s`, `5m`, `2h`, `1d`, `7d`.
- **États de diffusion.** `RUNNING` (antenne), `PAUSED` (piste gelée, bruit de fond), `DRAINING` (veille armée), `SLEEPING` (veille : bruit de fond, rien n'est résolu). L'arrêt opérateur n'est pas un état : c'est stationd lui-même arrêté.

## Makefile (machine de dev)

`make` seul affiche cette liste. Paramètres passés en `VAR=valeur` sur la ligne de commande.

| Cible | Effet | Paramètres |
| --- | --- | --- |
| `up` / `down` | Démarre / arrête le conteneur de dev | — |
| `image` | Reconstruit l'image de dev et recrée le conteneur (après un changement de `docker/` ou `.env`, dont le script s6 de stationd) | — |
| `logs` | Suit les logs du conteneur | — |
| `shell` | Shell `bash` dans le conteneur (utilisateur `dev`) | — |
| `build` | `cargo build` (stationd + stationctl, debug) | — |
| `release` | `cargo build --release --locked` | — |
| `test` | `cargo test --locked` | — |
| `clippy` | `cargo clippy --locked --all-targets` | — |
| `fmt` | `cargo fmt --check` | — |
| `plugins` | Compile les plugins WASM (`wasm32-unknown-unknown`, release) et liste les `.wasm` produits | `P=<dossier>` : un seul plugin (ex. `P=stop-when-idle-wasm`) ; vide = tous |
| `all` | `build` + `plugins` + `test` | `P=` |
| `tui` | Compile et lance la TUI | — |
| `restart` | `build` puis relance de stationd seul, attend qu'il soit prêt (l'antenne n'est pas coupée) | — |
| `restart-ls` | Relance Liquidsoap (après un changement du `.liq`) | — |
| `restart-icecast` | Relance Icecast (après un changement d'`icecast.xml`) | — |
| `restart-air` | `restart` (réécrit `.liq` / `icecast.xml`), puis Icecast et Liquidsoap | — |
| `check-liq` | `liquidsoap --check` du script généré | `LIQ=<chemin>` (défaut `/src/data/station.liq`) |
| `ctl` | N'importe quelle commande `stationctl` | `A="<arguments>"` (défaut `status`), ex. `A="station state"` |
| `state` | `stationctl station state` | — |
| `stop` | Arrêt opérateur (`station stop`) | `FORCE=1` → `--force` |
| `start` | Relance après arrêt opérateur (`station start`) | — |
| `package` | `docker/package.sh` : image d'exploitation + archive `dist/` | `ARGS=--allow-dirty` si l'arbre git n'est pas propre |

Variables générales : `DC` (défaut `docker compose`), `SVC` (service, défaut `station`). Dans `watch`, qui exécute `sh`, utiliser `make -s …` (pas d'écho des commandes).

## État et cycle de vie

| Commande | Effet | Paramètres |
| --- | --- | --- |
| `status` | Nom de la station, fuseau, uptime, pid | — |
| `quit` | stationd sort proprement ; son superviseur (s6) le relance : c'est un **redémarrage** | — |
| `station state` | État de diffusion + dernier échantillon d'auditeurs (`unknown` = jamais lu ou Icecast illisible). stationd injoignable + marqueur présent → `STOPPED by the operator since …`, code **3** | `--root <dir>` : répertoire de travail de stationd (défaut `$STATIOND_ROOT`, sinon le répertoire courant) |
| `station stop` | **Arrêt opérateur** : la piste en cours va au bout, puis bruit de fond ; stationd s'arrête et n'est plus relancé, même après redémarrage du conteneur ou de l'hôte, jusqu'à `station start` (marqueur `data/stationd.stopped`). Liquidsoap et Icecast restent debout. Refusé si un DJ est à l'antenne | `--force` : déconnecte d'abord le DJ |
| `station start` | Relance après `station stop` : retire le marqueur, relance le service s6, attend que stationd réponde, affiche l'état. Seule commande locale (pas gRPC) : **dans le conteneur** ; station déjà en marche = no-op | `--root <dir>` (comme `state`) ; `--timeout <s>` (défaut 30) |
| `station pause` | Pause immédiate : piste gelée, bruit de fond | — |
| `station resume` | Reprise : la piste gelée continue ; depuis la veille, piste du créneau courant ; annule aussi une veille armée | — |
| `station next` (alias `skip`) | Piste suivante maintenant | — |
| `station stop-when-idle` | Arme la veille : `DRAINING`, puis `SLEEPING` au prochain bord de piste si le dernier échantillon vaut 0. Audience inconnue = jamais de veille | — |
| `station wake` | Quitte la veille (piste du créneau courant). Sans effet dans tout autre état : ne dépause jamais, n'annule pas un drain | — |

Réveils automatiques (core) : audience devenue inconnue, DJ qui prend l'antenne. Le plugin `stop-when-idle` ajoute la veille à 0 auditeur et le réveil au retour d'un auditeur (voir Recettes).

## Programmation

| Commande | Effet | Paramètres |
| --- | --- | --- |
| `playlist validate <PATH>` | Vérifie un TOML sans rien appliquer ni écrire : chaque problème avec son champ (`selection.order`, `selection.members[2].ref`…), la valeur rejetée et les valeurs admises ; références de groupe et cycles jugés avec la vue actuelle ; erreur = code ≠ 0 | `<PATH>` : fichier `.toml` local ; `--as <ref>` (où il serait enregistré : refs `./x` d'un groupe, cycles) |
| `playlist preview <PATH>` | Pool du TOML sans l'appliquer : nombre de médias disponibles, durée, artistes, échantillon ; par membre pour un groupe ; pool vide = avertissement | `--as <ref>` ; `--sample <n>` (défaut 20, max 100) |
| `playlist save <REF> <PATH>` | stationd valide, **écrit le fichier** sous `[playlist] path` (écriture atomique, relue) et l'applique. Crée si absent ; remplacer un fichier existant demande sa révision. L'id est conservé (un `id` différent est refusé). Invalide = rien d'écrit, diagnostics, code ≠ 0 | `<REF>` : ref cible (`emission/intro`) ; `--revision <rev>` (de `export --file`) ou `--force` |
| `playlist add <PATH>` | Enregistre dans la vue un fichier TOML qui vit **ailleurs** que sous `[playlist] path` (entrée sans chemin, désignée par son UUID) ; le fichier donné est réécrit sur place avec l'id. Pour la racine des playlists : `save` | `<PATH>` : fichier `.toml` |
| `playlist sync` | Réconcilie tous les `*.toml` sous `[playlist] path` (récursif). Un fichier invalide est signalé, pas bloquant | — |
| `playlist list` | Playlists connues de stationd, avec ce qui les référence (`used by:` règles de grille, groupes) | — |
| `playlist containing <MEDIA>` | Playlists qui peuvent diffuser un média : statiques qui le listent, dynamiques dont les filtres le retiennent (disponible ou non) ; les groupes ne sont pas dépliés (voir `used by`). Média inconnu de l'index = `NotFound` (chemin relatif à la racine média, casse comprise) | `<MEDIA>` : chemin relatif (`Musique/a.mp3`) |
| `playlist reload` | La vue devient exactement le répertoire : `sync` + retrait des playlists dont le fichier a disparu. Une playlist disparue mais encore référencée (règle de grille, groupe qui reste) est gardée et signalée ; erreur = code ≠ 0 | — |
| `playlist export <REF>` | Affiche le TOML que stationd applique pour cette playlist (signale sur stderr si le fichier en diffère) ; `--file` : le fichier lui-même, commentaires compris, et sa révision (stderr) | `<REF>` : ref (`emission/intro`) ou UUID ; `--out <fichier>` (défaut stdout) ; `--file` |
| `playlist remove <REF> --yes` | Supprime la playlist : son fichier sous `[playlist] path` et son entrée. Refusé tant qu'une règle de grille ou un groupe la référence. L'état de lecture (curseur, épisodes joués, file) est gardé | `<REF>` : ref ou UUID ; `--yes` obligatoire ; `--revision <rev>` (refus si le fichier a changé depuis) |
| `schedule validate <PATH>` | Valide une grille sans l'installer ; rejet = code ≠ 0 | `<PATH>` : `grid.toml` |
| `schedule apply <PATH>` | Valide et installe la grille (atomique) ; l'état de lecture est conservé | `<PATH>` |
| `schedule export` | Réécrit la grille courante en TOML | `--rule <id>` (répétable) ; `--out <fichier>` (défaut stdout) |
| `schedule list` | Règles de la grille (lecture seule) | — |
| `schedule next` | Source que la grille jouerait maintenant. **Consomme** comme un vrai bord de piste (repère `at_clock`, remise à zéro `every`) | `--at <epoch s UTC>` |
| `schedule preview` | Projection de la grille sur une fenêtre, en UTC et en heure locale | `--at <epoch>` (défaut maintenant) ; `--window <s>` (défaut 86400) |
| `schedule check` | Assez de médias par règle ? OK / ⚠ juste / ✗ insuffisant ; un ✗ = code ≠ 0 | `--rule <id>` (répétable) |
| `queue push <REF> <MEDIA>` | Ajoute un média au tampon d'une playlist `queue` (demande d'auditeur, injection DJ) ; refusé si `max_len` atteint | `<REF>` : playlist queue ; `<MEDIA>` : chemin relatif |
| `override push` | Contenu poussé devant la grille. `soft` = au prochain bord de piste. Un média absent du disque est refusé tout de suite (`NotFound`) | `--media <chemin>` ou `--playlist <ref>` ; `--hard` (coupe maintenant ; dégradé en soft sans Liquidsoap ou en pause/veille) ; `--expiry 30s\|5m\|2h` (défaut jamais périmé) ; `--tracks <n>` (playlist, défaut 1) |
| `override list` | Overrides en attente, dans l'ordre de passage | — |
| `override clear` | Retire un override, ou tous | `--id <n>` (absent = tous) |
| `clock show` | Horloge effective | — |
| `clock set <WHEN>` | Fige l'horloge (tests) | `HH:MM` (aujourd'hui) ou `"YYYY-MM-DD HH:MM"`, heure locale |
| `clock reset` | Retour au temps réel | — |

## Médiathèque et statistiques

| Commande | Effet | Paramètres |
| --- | --- | --- |
| `library scan` | Scanne `[media] library_path` et réconcilie l'index ; un fichier illisible est signalé, pas bloquant. Un fichier disparu reste dans l'index, marqué indisponible : `vanished` = disparus **à ce scan**, `unavailable` = disparus au total | — |
| `library prune` | Oublie les médias disparus (lignes indisponibles et leurs genres). L'historique de diffusion garde leur chemin et leur artiste (plus leur titre) | `--older-than <durée>` (seulement ceux vus pour la dernière fois il y a plus de, ex. `30d`) |
| `library list` | Index des médias (disponibles seulement par défaut) | `--all` (inclut les fichiers disparus) ; `--genre <g>` (répétable, OU) ; `--by-genre` (groupé par genre) |
| `library genres` | Nombre de médias par genre + sans genre | `--all` |
| `library search [MOTS]` | Recherche par page : chaque mot doit apparaître dans le titre, l'artiste, l'album ou le chemin (casse ignorée, majuscules accentuées comprises) ; tri stable (clé puis chemin) ; affiche le curseur de la page suivante | `--genre <g>` (répétable, OU) ; `--folder <dossier>` ; `--missing title\|artist\|album\|year\|genre` (répétable : manque tout) ; `--age "<10d"` (âge de la date de création : `<`, `<=`, `>`, `>=` + durée `m`/`h`/`d` ; répétable : toutes ; sans date de création = exclu) ; `--sort path\|title\|artist\|album\|year\|duration` ; `--desc` ; `--limit <n>` (défaut 50, max 500) ; `--cursor <c>` ; `--all` |
| `library tags <MEDIA>` | Tags lus **dans le fichier** (titre, artiste, album, année, genres du fichier, sources `custom-tags` comme `Type`, BPM, tempo et date de création manuels) avec le tempo et la date effectifs, les libellés de tempo proposés et la révision | `<MEDIA>` : chemin relatif |
| `library tag <MEDIA>` | Écrit des tags standard **dans le fichier** (ID3v2 : mp3, wav, aiff ; autre format refusé), relit le fichier pour vérifier, met l'index à jour (plugins `on_scan` compris). Seuls les champs donnés changent ; vide = champ retiré. Version ID3v2 du fichier conservée (2.3 créée si absente, à partir de l'ID3v1), trames utilisateur (`TXXX:Type`…) gardées, date de modification du fichier conservée. Tags changés depuis `--revision` = conflit, rien d'écrit, code ≠ 0 | `--title`, `--artist`, `--album`, `--year` (0 = retirée) ; `--genre X` (répétable, remplace tous les genres `TCON`), `--no-genre` ; `--source Type=a,b` (répétable ; `Type=` retire) ; `--bpm N` (`TBPM`, 0 = retiré) ; `--tempo L` (`TXXX:tempo_manual`, l'emporte sur le libellé tiré du BPM ; `""` = retiré) ; `--creation DATE` (RFC 3339, `TXXX:creation_manual`, l'emporte sur la date déduite ; `""` = retirée) ; `--revision <rev>` (de `library tags` ; sans : relue juste avant) |
| `stats` | Diffusions groupées ; `aired` = réellement démarré par Liquidsoap, `picked` = choisi par stationd | `--since <durée>` (défaut 24h) ; `--by playlist\|leaf\|rule\|origin\|media\|artist` (défaut playlist) ; `--limit <n>` (défaut 20, 0 = tout) ; `--key <clé>` (une seule clé du regroupement, ex. un média avec `--by media`) |

Pour `--by` : `playlist` = playlist de la règle ou de l'override (un groupe compte comme le groupe) ; `leaf` = playlist membre qui a fourni le fichier ; `origin` = `AtClockHard`, `Every`, `BaseRotation`, `Override`…

## Plugins

| Commande | Effet | Paramètres |
| --- | --- | --- |
| `plugin list` | Plugins déclarés, leur état (`loaded`, `stopped`, `failed` + raison) et leurs capacités | — |
| `plugin start <NAME>` | Active un plugin arrêté ou en échec | `<NAME>` |
| `plugin stop <NAME>` | Désactive un plugin chargé | `<NAME>` |
| `plugin restart <NAME>` | Arrêt puis démarrage, même binaire | `<NAME>` |
| `plugin reload <NAME>` | Recharge le `.wasm` depuis le disque (= restart pour un plugin natif) | `<NAME>` |
| `plugin db <NAME> info` | Base propre du plugin (capacité `db`) : fichier, taille, version de schéma, tables, limites | `<NAME>` |
| `plugin db <NAME> query "<SQL>"` | Une requête SQL en lecture seule | `<NAME>`, `<SQL>` |
| `plugin db <NAME> reset --yes` | Supprime la base (plugin arrêté ; recréée et migrée au prochain démarrage). Irréversible | `--yes` obligatoire |

Déclaration dans `stationd.toml` (un plugin déclaré n'est actif qu'avec `enabled = true`) :

```toml
[[plugin]]
name         = "stop-when-idle"          # natif, ou nom libre pour un wasm
enabled      = true                      # défaut false
# wasm       = "/usr/lib/stationd/plugins/<crate>.wasm"   # plugin WASM
capabilities = ["control"]               # control | push_override | db

[plugin.config]                          # transmis au plugin
min_zero_samples = 2
```

Capacités : `control` = pause / reprise, armer la veille, réveiller (jamais arrêter stationd) ; `push_override` = contenu devant la grille ; `db` = base SQLite propre (`data/plugins/<name>.db`). Plugins livrés : natifs `logger`, `blacklist`, `stop-when-idle` ; WASM `blacklist-wasm`, `require-title-wasm`, `stop-when-idle-wasm`, `custom-tags-wasm`, `play-stats-wasm`.

## Chaîne de diffusion

| Commande | Effet | Paramètres |
| --- | --- | --- |
| `ls status` | Pont Liquidsoap : dernier pull, dernière réponse, piste à l'antenne et suivante, état de l'antenne, santé du socket de contrôle | — |
| `ls render` | Script Liquidsoap généré (tel qu'écrit au démarrage) | — |
| `icecast status` | Dernière lecture de `/admin/stats` : audience, source connectée, débit annoncé et mesuré, auditeurs, titre, par mount | — |
| `icecast render` | `icecast.xml` généré (`[icecast.server]`) | — |
| `debug listeners <COUNT>` | Injecte un échantillon d'auditeurs (test) ; écrasé par la lecture Icecast suivante | `<COUNT>` |

États de l'antenne affichés par `ls status` (ligne `tracks`) : `playing`, `paused`, `live`, `sleep armed` (veille armée), `falling asleep` (en veille, la piste en cours va au bout), `sleeping` (bruit de fond).

Échantillonnage Icecast : toutes les `[icecast] poll_interval` s (défaut 15), `poll_interval_sleeping` s pendant la veille (défaut 3 : latence de réveil).

## Live DJ

Nécessite la section `[live]` ; les DJ sont déclarés dans le fichier des DJ (`[live] djs_path`).

| Commande | Effet | Paramètres |
| --- | --- | --- |
| `live status` | DJ à l'antenne, dernier live, ouvertures, droits d'urgence, DJ refusés | — |
| `live kick` | Termine le live maintenant ; la voie d'entrée se ferme (créneau / ouverture : jusqu'à sa fin ; droit d'urgence : `[live] urgent_cooldown`) | — |
| `live open <DJ> --for <durée>` | Autorise un DJ à se connecter maintenant, hors grille (persisté) | `<DJ>` : id du fichier des DJ ; `--for` : 1m à 7d |
| `live close <DJ>` | Ferme l'ouverture d'un DJ (un DJ déjà à l'antenne reste : utiliser `kick`) | `<DJ>` |
| `dj hash` | Empreinte d'un mot de passe pour `password_hash` ; mot de passe lu sur l'entrée standard, jamais en argument | — |

Un DJ qui prend l'antenne réveille une station en veille ; `station stop` est refusé pendant un live sans `--force`.

## Recettes

**Veille automatique à 0 auditeur, réveil au retour.** Déclarer le plugin `stop-when-idle` (section Plugins, `enabled = true`, `capabilities = ["control"]`), puis `make restart` et vérifier avec `stationctl plugin list`. Suivi : `watch -n1 'make -s state'`. Un auditeur de test : `curl -s -o /dev/null http://127.0.0.1:8000/<mount> &`, puis `kill %1`.

**Veille ponctuelle, sans plugin.** `stationctl station stop-when-idle` ; réveil manuel par `station wake` ou `station resume`.

**Arrêter la station pour de bon.** `make stop` (ou `station stop`, `--force` pendant un live) ; contrôle : `make state` rend `STOPPED by the operator` (code 3), y compris après `docker compose restart`. Relance : `make start`. Prérequis : image construite avec le script s6 actuel (`make image` après une modification de `docker/`).

**Nouvelle version du code.** `make restart` (build + relance de stationd seul, l'antenne continue). Plugins : `make plugins`, puis `stationctl plugin reload <nom>`.

**Changement de `[liquidsoap]`, `[icecast]` ou des sorties.** `make restart-air` : stationd réécrit le `.liq` et `icecast.xml`, puis Icecast et Liquidsoap sont relancés ; `make check-liq` pour valider le script.

**Nouvelle grille.** `stationctl schedule validate grid.toml`, `schedule check`, `schedule preview`, puis `schedule apply grid.toml`.

**Livraison.** `make package` (`ARGS=--allow-dirty` si besoin) → `dist/stationd-<tag>.tar`, à copier sur le nœud puis `install.sh`.
