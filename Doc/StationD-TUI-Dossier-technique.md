# StationD — Dossier technique de la TUI

Version 3.0 — 30 septembre 2026
Statut : refonte complète (remplace la v1.0 du 25/09). Document vivant : il suit les besoins, pas l'inverse.
Socle : Rust + Ratatui + rat-salsa / rat-widget, client gRPC pur de `stationd`.

## 0. Décisions actées (28/09)

| # | Décision |
|---|---|
| D1 | La TUI repart de zéro. L'actuelle (`src/bin/stationd-tui`) est abandonnée, rien n'en est repris. |
| D2 | La TUI est **indépendante** : un client gRPC de `stationd` exactement au même titre que `stationctl`. Aucune logique métier, aucun accès direct à la SQLite de stationd. |
| D3 | Tout ce que fait la TUI passe par un RPC. Un besoin de la TUI qui n'a pas de RPC se traite **d'abord dans stationd** (le CLI en profite aussi), jamais par un contournement côté TUI. |
| D4 | Les **tags restent un plugin** (`tags`). stationd n'a aucune notion de tag ; il fournit seulement des mécanismes génériques (§3.8). Les tags sont stockés **dans le fichier** (ID3 `TXXX`). On peut créer des tags librement, mais **Type est le plus utilisé** : le plugin `tags` gère les deux familles et reprend le rôle de l'actuel `TXXX:Type` / `on_scan`. |
| D5 | Modularité par écran : chaque écran est un module indépendant (trait `Screen`) inscrit dans un registre. Un plugin stationd peut apporter un écran via un contrat générique (§4.3). Pas de plugins binaires chargés par la TUI. |
| D6 | Vue principale : morceau à l'antenne, **10 morceaux théoriques à suivre**, **20 derniers joués**, **playlist en cours et playlists à suivre**. X et Y réglables dans la TUI. |
| D7 | La simulation des morceaux à suivre applique les `filter_pool` des plugins (appel en mode simulation, §3.2). |
| D8 | La TUI peut tourner **sur une autre machine du réseau local** : elle se connecte en gRPC direct à stationd, après **login**. Pas d'accès depuis l'extérieur par la TUI. |
| D9 | Le login est implémenté **une seule fois, dans stationd** (`AuthService`, §3.9). La TUI l'appelle directement ; le futur webadmin passe par `api`, qui appelle ce même service et transmet le jeton. `api` n'a pas de base d'utilisateurs. |
| D10 | Les TOML (playlists, grille) sont **enregistrés par stationd**, avec contrôle de révision : un client distant ne peut pas écrire dans les dossiers du nœud. |
| D11 | Socle d'interface : **rat-salsa** (boucle d'application), **rat-widget** (widgets), **rat-focus** (focus des formulaires), **rat-theme** (palettes). |
| D12 | **Aucune chaîne affichée en dur** : la TUI passe par des catalogues **Fluent** (`fr` par défaut et repli, `en`, `de`). stationd n'envoie **aucun texte à afficher** : ce qu'il signale arrive en **opcodes** (énumérations proto) avec paramètres typés, chaque client traduit. |

## 1. Les quatre manques à combler

La TUI v1 ne permettait ni de piloter la station, ni de créer une playlist correctement, ni de lire l'agenda, et sa vue principale n'apportait rien. La refonte est construite autour de ces quatre points :

1. **Contrôler** : pause/reprise, suivant, veille, overrides, live, plugins, scan — tout accessible en deux touches, avec confirmation pour ce qui touche l'antenne.
2. **Créer et modifier des playlists** : formulaire par mode, validation en direct par stationd, aperçu du pool avant d'enregistrer.
3. **Lire et modifier l'agenda** : vue jour et semaine lisibles, pools dimensionnés, édition de règle avec diagnostics.
4. **Voir l'antenne** : ce qui passe, ce qui va passer, ce qui est passé, et quelle playlist mène.

## 2. Principes

- **Client pur.** La TUI affiche, saisit, envoie. Validation, sélection, projection, priorités : toujours stationd.
- **TOML source de vérité.** Une playlist ou une règle se crée/modifie en produisant du TOML que stationd valide, enregistre sur le nœud et applique (`Validate` pour le direct, `Save` pour enregistrer).
- **Lire n'a aucun effet.** Naviguer, prévisualiser, simuler ne fait jamais avancer un curseur, un groupe, un historique ou un compteur.
- **Pas de faux zéro.** Inconnu s'affiche `—`, théorique `~`, périmé en grisé avec son âge. Jamais 0 à la place de « inconnu ».
- **Aucune erreur avalée.** Toute réponse en erreur est visible dans la ligne de statut puis consultable dans Système.
- **Terminal.** Confort 120×35, minimum 80×24 : les widgets secondaires disparaissent avant les données principales. Quitter la TUI ne touche pas à stationd.

## 3. Contrat gRPC

### 3.1 Existant utilisé tel quel

| Service | RPC | Usage TUI |
|---|---|---|
| `Station` | `Status`, `Shutdown` | Bandeau (nom, uptime, fuseau) ; arrêt opérateur |
| `BroadcastService` | `GetState`, `Control`, `Skip`, `PushOverride`, `ListOverrides`, `ClearOverrides` | Contrôle, état, auditeurs |
| `LiveService` | `GetStatus`, `Kick`, `Open`, `Close` | Panneau live |
| `LiquidsoapService` | `GetStatus` | Système (et repli de l'Antenne tant que §3.2 n'existe pas) |
| `IcecastService` | `GetStatus` | Système, auditeurs par mount |
| `PlaylistService` (lot 3) | `List`, `Export`, `Validate`, `PreviewPool`, `Save`, `Remove`, `Sync`, `Reload`, `Add`, `Containing` (lot 4b) | Playlists (§3.5), fiche média |
| `ScheduleService` | `ListRules`, `Preview`, `CheckCoverage`, `ExportGrid`, `ValidateGrid`, `ApplyGrid`, `Enqueue` | Agenda, playlists `queue` |
| `LibraryService` | `Scan`, `ListMedia`, `ListGenres` | Médias |
| `StatsService` | `Plays` (`key` : une seule clé, lot 4b) | Statistiques, fiche média |
| `PluginService` | `List`, `Control`, `DbInfo`, `DbQuery` | Plugins |

`Station` ne garde que `Status`, `Quit`, `Shutdown` : ses six RPC `Playlist*` ont déménagé dans `PlaylistService` au lot 3. L'ancien `playlist_v1.proto` (contrat de conception jamais servi, 3 modes sur 5) a été réécrit.

### 3.2 `OnAirService` (vue principale) — fait au lot 1

Contrat : `proto/onair_v1.proto`. `Watch(upcoming, history, playlists_ahead)`
pousse un instantané complet au démarrage puis à chaque changement ;
`History(before, limit)` remonte au-delà. Plafonds : 30 à suivre, 100 joués,
20 playlists. CLI : `stationctl onair [--upcoming N] [--history N]
[--playlists N] [--follow]`, `stationctl onair history [--before E] [--limit N]`.

L'instantané porte : état de diffusion, auditeurs (absent = inconnu), nature
de l'antenne, **morceau à l'antenne** (titre/artiste/album/durée de l'index,
début réel, playlist, membre, règle, origine), **préparé** (certain),
**à suivre** (simulés, début estimé absent dès qu'une durée manque, `cut_at`
si un rendez-vous hard le coupe), **notes**, **joués** (plus récent d'abord,
issue diffusé / coupé / inconnu), **playlist en cours**, **playlists à venir**
(projection de la grille, heure et `at_local`), règles au compteur
(indicatif), DJ, overrides en attente, présence de Liquidsoap, fuseau.

Déclenchement (`src/onair.rs`) : deux compteurs dans `StationControl`
(`bump_air` : début de piste, préchargement, état, override, live, clock,
apply de grille, playlists ; `bump_meta` : auditeurs). Une seule tâche calcule
pour tous, seulement si quelqu'un écoute, regroupe les rafales (200 ms) et ne
relance la simulation que sur `air`. Tick de 30 s.

Issue des joués : migration `0022` (`broadcast_log.left_at`,
`played_to_end`), écrite quand le pont rapporte la fin d'une de nos pistes
(`GridEngine::track_left`, identifiant de ligne transporté par le pont).

Position : calculée par le client (`maintenant − started_at`), figée en pause.

### 3.3 Simulation des morceaux à suivre — fait au lot 1

`src/onair_sim.rs`. Le **vrai moteur** tourne sur une **copie en mémoire** de
la base (`db::memory_copy` : mêmes migrations, lignes copiées en une
transaction de lecture — le WAL ne bloque pas l'écrivain réel) avec une copie
détachée de `StationControl` (overrides en attente compris). Il tire les
morceaux comme Liquidsoap, en avançant l'horloge de la durée indexée de
chacun ; un rendez-vous hard qui tombe dans un morceau le coupe. Tous les
effets (curseurs, groupes, `Every`, jetons, files `queue`, journal qui
nourrit l'anti-répétition, marques `unplayed_only`) restent dans la copie :
la simulation est fidèle, la station intacte (testé : contenu de toutes les
tables et file d'override identiques avant/après). Journaux coupés.

Plugins : `filter_pool` appliqué, **actions refusées** par l'hôte pendant
l'appel, échec non compté mais signalé en note, aucun événement
(`Doc/plugin-hooks.md` « Mode simulation »).

Pas de simulation (et une note) quand la station est en pause, en veille, en
veille imminente (0 auditeur), ou qu'un DJ est à l'antenne. Sans Liquidsoap,
la simulation montre ce que la grille choisirait (note). Les tirages au sort
(shuffle, permutation d'un groupe, membre pondéré) sont reproductibles
(`src/draw.rs` : graine de la station + compteur par playlist, en base) : la
copie tire ce que tirera l'antenne, la liste ne change que si l'état change
(override, live, grille, rescan, fenêtre de contrainte franchie à un autre
instant).

### 3.3 bis Incidents de grille — fait au lot 2

Une source de la grille qui ne donne rien (pool vide, ou pool vidé par
l'anti-répétition / les plugins) n'est plus silencieuse. Le moteur note
l'incident dans `StationControl` (`record_incident`, fusion par règle et
type, compteur et première/dernière heure) :
- `HardNotCut` : un rendez-vous hard n'a pas coupé faute de contenu ;
- `SourceEmpty` : la source est passée à la priorité inférieure.

Deux usages, en opcodes `Note.Code` (paramètres `rule`, `playlist`, `at`,
`count`) :
- **prévu** (la simulation, sur sa copie) : `RENDEZVOUS_WILL_NOT_CUT`,
  `SOURCE_WILL_BE_EMPTY` ;
- **constaté** (l'antenne réelle, dernière heure) : `RENDEZVOUS_NOT_CUT`,
  `SOURCE_WAS_EMPTY`.

`PlaylistSlot.issue` marque la **ligne fautive** du panneau Playlists :
`POOL_EMPTY` (vu dès la projection : aucun média) ou `NOTHING_PLAYABLE` (vu
seulement par la simulation : le pool compte les médias avant
l'anti-répétition). `stationctl onair` : `!! EMPTY POOL` / `!! NOTHING
PLAYABLE`.

Arrêt de stationd : les flux `Watch` se terminent dès la demande d'arrêt,
sinon l'arrêt propre attendrait indéfiniment une TUI restée ouverte.

### 3.4 Médias

- **Fait au lot 3** — `LibraryService.SearchMedia` : mots cherchés dans titre/artiste/album/chemin (chaque mot doit apparaître ; repli de casse Unicode, fait en Rust car SQLite ne replie que l'ASCII), filtres genre (au moins un), dossier (préfixe), disponibilité, métadonnées manquantes (`missing` : titre, artiste, album, année, genre), tri par chemin/titre/artiste/album/année/durée, croissant ou non, **stable** (clé puis chemin), page de 50 (max 500), curseur opaque (dernière clé vue), total. `ListMedia` reste pour le CLI. CLI : `stationctl library search`.
- **Fait (2026-09-29)** — `LibraryService.GetTags` / `SetTags` : tags standard lus et écrits **dans le fichier** par stationd (`src/media_tags.rs`), révision = empreinte des champs + taille, conflit si le fichier a changé ; ID3v2 seulement (mp3, wav, aiff), par la trame ID3v2 concrète (le `Tag` générique de lofty perdrait les `TXXX`), version 2.3 / 2.4 conservée, 2.3 créée depuis l'ID3v1 si absente ; date de modification remise, relecture comparée (NFS), ligne d'index rafraîchie via `on_scan`, garde-fous `unplayed_only` suivis. CLI : `stationctl library tags|tag`. **Suite (2026-09-29)** : chaque genre s'écrit à sa source — genres du fichier en `TCON` multi-valeurs, sources `custom-tags` (les `TXXX` de `tags` dans sa config, ex. `Type`) multi-valeurs ; `TBPM` ; tempo et date de création manuels en `TXXX:tempo_manual` / `TXXX:creation_manual`, appliqués par l'hôte après les plugins (l'emportent sur les valeurs déduites, marchent sans plugin). `MediaTags` porte aussi `tempo`, `creation` effectifs et `tempo_choices` (libellés du plugin + valeurs connues) ; `genre` (6/7) réservé.
- À ajouter (lot 7) — `LibraryService.Scan` avec avancement : `ScanWatch` en flux (`found/skipped/total_estimé/fichier courant`, puis le `ScanResponse` final). Un seul scan à la fois, déjà garanti.

### 3.5 Playlists — fait au lot 3

Tranché : **`PlaylistService` dédié** (`proto/playlist_v1.proto`, réécrit), qui reprend les six RPC `Playlist*` de `Station` (`List`, `Export`, `Add`, `Sync`, `Remove`, `Reload`) et ajoute `Validate`, `PreviewPool`, `Save`. `stationctl` migré dans le même lot. Métier : `src/playlist_edit.rs` ; transport : `src/playlist_grpc.rs`.

- **Format d'échange = le TOML** (la grammaire des 5 modes), pas de message typé par mode : le client qui présente un formulaire lit et écrit du TOML (`toml_edit` côté TUI pour garder les commentaires), stationd seul valide.
- **Diagnostics** (`Diagnostic`) : sévérité (erreur / avertissement), `field_path` dans la grammaire TOML (`selection.order`, `selection.filter[2].value`, `selection.members[1].ref` — index à partir de 1 ; vide = le fichier), valeur rejetée, valeurs admises, message anglais, et un **`Code`** (`SYNTAX`, `UNKNOWN_FIELD`, `MISSING_FIELD`, `BAD_VALUE`, `NOT_ALLOWED`, `REQUIRED_FOR_MODE`, `CONFLICT`, `BAD_FILTER`, `BAD_DURATION`, `UNKNOWN_REF`, `BAD_REF`, `CYCLE`, `ID_CHANGED`, `EMPTY_POOL`) : la TUI traduit par code, jamais en analysant le message. La validation collecte **tous** les problèmes (elle s'arrêtait au premier) ; une erreur de lecture TOML ou de grammaire est rattachée à son champ par sa position dans le texte (ligne/colonne pour une erreur de syntaxe). Les fenêtres anti-répétition (`30m`, `2h`) sont désormais vérifiées à la validation, plus seulement à la diffusion.
- `Validate { toml, reference? }` : rien n'est appliqué ni écrit ; références de groupe et cycles jugés avec la **vue actuelle**, le brouillon à la place de `reference` (qui sert aussi de base aux refs `./x`).
- `PreviewPool { toml, reference?, sample }` : ce que l'index offre **aujourd'hui** (médias disponibles, avant anti-répétition et plugins) — nombre, durée, artistes distincts, échantillon trié par chemin (20, max 100) ; groupe : un bilan par membre. Pool vide = avertissement `EMPTY_POOL`, sur le pool entier ou **sur la ligne du membre vide** (le cas TOPH).
- `Save { reference, toml, expected_revision }` : révision = **empreinte du contenu** du fichier (`sha256:…`), pas sa date (fiable sur NFS). Révision attendue vide = création (conflit si le fichier existe) ; différente = conflit, rien n'est écrit. Invalide = `ok = false` + diagnostics (une réponse, pas une erreur gRPC). L'id est conservé (brouillon sans `id` → celui du fichier ou de la vue ; `id` différent → `ID_CHANGED`). Écriture atomique (fichier temporaire du même dossier, `fsync`, renommage) puis **relecture** comparée (NFS). Les écritures de la racine (`Save`, `Remove`, `Sync`, `Reload`) sont sérialisées.
- `Export` rend le TOML appliqué **et** le fichier (commentaires compris) avec sa révision, et dit s'ils diffèrent. `Remove` accepte une révision attendue (`ABORTED` si le fichier a changé). `Sync` / `Reload` rendent les diagnostics de chaque fichier écarté.
- `Add` reste pour un TOML qui vit hors de la racine (entrée sans chemin, désignée par son UUID) ; le client réécrit **son** fichier avec l'id. Pour la racine : `Save`.
- CLI : `stationctl playlist validate|preview|save`, `export --file`, `remove --revision`.
- **Ajouté au lot 4b** : `PlaylistSummary.rules` / `groups` (règles de grille et groupes qui référencent la playlist, calculés par stationd — c'est ce qui bloque `Remove`) ; `PlaylistService.Containing { media_path }` (playlists qui peuvent diffuser un média : statiques qui le listent, dynamiques dont les filtres le retiennent, disponible ou non ; `NOT_FOUND` si l'index ne le connaît pas) ; `StatsService.Plays.key` (une seule clé du regroupement : les diffusions d'un média). CLI : `playlist list` (ligne `used by:`), `playlist containing <media>`, `stats --key`.

### 3.6 À ajouter — Grille

**Fait (lot 6a, 2026-09-29)** — ce que l'Agenda lit sans éditer :
- `PreviewResponse.live` (`LiveWindow` : règle, DJ, ouverture, fermeture — absente = encore ouverte à la fin —, `open_before`) : les fenêtres de connexion des règles `live` sur la projection (`resolver::live_window` minute par minute), à part de la timeline. `stationctl schedule preview` les liste après les `every` au compteur.
- `CoverageEntry.reasons` / `CoverageMember.reasons` (`CoverageReason` : `Code` + paramètres typés — fenêtre, `pool_ms`, `need_ms`, `count`, `limit`, `refs`, `error` technique relayé) : les causes du verdict en opcodes (D12). `detail` reste leur rendu français pour `stationctl schedule check`, mot pour mot (`grid_engine::Reason::text`).

**Fait (lot 5, 2026-09-30)** — grilles en fichiers, une active :
- Les grilles sont les fichiers de `[grid] path` (`grid/` par défaut, dans le répertoire unique du nœud ; `install.sh` y déplace un ancien `grid.toml` de la racine). **Une seule est active** : choisie par `ActivateGrid` (validée, appliquée, choix gardé en base — table `grid_active`, migration `0025` ; aucune = `grid.toml`). Le fichier actif fait foi : relu et appliqué au démarrage et par `ReloadGrid` ; absent ou invalide, la dernière grille appliquée reste à l'antenne (jamais d'antenne vide) et `ListGrids` dit pourquoi. Métier : `src/grid_files.rs`.
- `ListGrids` (nom, révision, active, nombre de règles, premier problème en `GridDiagnostic`), `GetGrid` (fichier avec commentaires, révision, écart avec l'appliqué), `SaveGrid { name, toml, expected_revision }` (même mécanisme que le `Save` des playlists : révision = empreinte, conflit, écriture atomique relue ; appliquée si c'est l'active), `ActivateGrid`, `ReloadGrid`. `ApplyGrid` écrit désormais le TOML reçu comme fichier de la grille active (file-first : sinon un redémarrage l'aurait défait).
- `ValidateGrid` / `ApplyGrid` / `SaveGrid` / `ActivateGrid` rendent des `GridDiagnostic` : code (22 opcodes : syntaxe, champ inconnu / manquant / d'une autre nature, heure, date, jour, durée, id en double, deux planchers, fenêtre nulle, dates inversées, ref inconnue, DJ inconnu…), `field_path` (`rule[3].start`), règle, valeur rejetée, valeurs admises. Tous les problèmes : ceux de chaque règle, puis les refs et DJ des règles lisibles (`grid_toml::diagnose` / `diagnose_partial`).
- `Preview`, `CheckCoverage`, `ListRules` acceptent `grid` (un fichier du nœud) ou `draft_toml` (un brouillon) : projeter / dimensionner une grille avant de l'activer, rien d'appliqué.
- CLI : `schedule grids|show|save|activate|reload`, `list|preview|check --grid <nom>`, `preview|check --draft <fichier>` ; `validate` / `apply` listent les diagnostics.

### 3.7 Événements — fait (2026-09-30)

**Fait** : `proto/events_v1.proto`, `EventService.Watch { backlog, follow }` → flux d'`Event { seq, at_ms, level, component, code, params }`. `src/events.rs` : journal en mémoire (2000 derniers, vide à chaque démarrage), faits typés (codes + paramètres nommés, D12) et chaque ligne `warn` / `error` de `tracing` (code `LOG`, texte gardé tel quel ; une ligne qui porte un champ `event` est déjà un fait typé, pas doublée). Faits : démarrage / arrêt, état de diffusion, auditeurs (au changement), audience inconnue, piste choisie, override, grille appliquée / refusée, incident de grille (une fois par incident), live, scan (début, fin, échec), tags écrits, renommage, plugin en échec / en quarantaine / changé d'état. Le flux suivi se termine à l'arrêt de stationd. CLI : `stationctl events [--last n] [--follow] [--level]`. Avancement du scan : `LibraryService.WatchScan` (phase, fichiers lus / trouvés, fin du dernier), `stationctl library scan --progress`, `library scan-status [--follow]`.

Prévu à l'origine :

- `EventService.Watch` : flux des événements déjà émis en interne pour les plugins (`BroadcastStateChanged`, `ListenersSampled`, début/fin de piste, apply, erreurs plugin…), borné, avec niveau et composant. Alimente la vue Système et la ligne de statut.

### 3.8 Plugins (générique, sans notion de tag) — tags : voie native retenue (2026-09-30)

**Décision (2026-09-30)** : l'écriture des tags étant native (`SetTags`, sources `custom-tags` comme `Type`, genres `TCON`), l'écran Tags ne passe pas par un plugin `tags` : `PluginService.Call/Views`, `on_call` et `media_meta_write` ne sont pas faits (restent possibles pour des vues de plugins, écran Plugins). Ajouté à la place : `LibraryService.ListTagValues` (valeurs par origine — genre du fichier, puis chaque source —, effectifs, graphies, médias sans valeur ; table `media_tag`, migration 0028, remplie par le scan) et `LibraryService.RenameTagValue` (aperçu `dry_run` : fichiers, playlists dont un filtre `genre` nomme la valeur, fusion ; exécution fichier par fichier en flux, échecs rapportés). CLI : `library values`, `library rename`.

Prévu à l'origine :

- `PluginService.Call { plugin, method, payload_json } → { ok, payload_json | error }` : stationd transmet au plugin via un nouveau point d'entrée `on_call(method, payload)`. Borné (timeout), soumis au même régime d'échec/quarantaine que les hooks. CLI : `stationctl plugin call <plugin> <method> '<json>'`.
- `PluginService.Views { plugin }` : description **déclarative** des écrans qu'un plugin propose (§4.3).
- Capacité hôte `media_meta_write` : un plugin demande l'écriture d'un champ de métadonnées **non standard** dans un fichier média (ex. `TXXX:Tags`). stationd fait l'écriture (lofty), la **vérification après écriture** (NFS), puis rescanne ce fichier (le hook `on_scan` du plugin relit la valeur). Chemins relatifs à `media/`, mêmes règles que `push_override`. Les champs standard (titre, artiste…) restent hors de portée d'un plugin.
- Opérations longues (renommer un tag sur 300 fichiers) : le plugin les découpe ; stationd expose l'avancement via `EventService` ; un échec fichier par fichier est rapporté, jamais avalé.
- Le plugin `tags` gère deux familles :
  - **Type** : une valeur par média, choisie dans une liste de valeurs déclarée dans la config du plugin (`music`, `talk`, `id`, `instrumental`…), stockée dans `TXXX:Type`. Il reprend le rôle du mécanisme `on_scan` actuel.
  - **Tags libres** : plusieurs par média, créés à la volée, stockés dans `TXXX:Tags`.
  Les deux sont exposés aux playlists dynamiques comme aujourd'hui pour Type (le mapping vers le pool reste dans le plugin).

### 3.9 À ajouter — Authentification (`AuthService`)

Un seul login pour la TUI et le futur webadmin, implémenté dans stationd.

```proto
service AuthService {
  rpc Login(LoginRequest) returns (LoginResponse);    // user + mot de passe → jeton
  rpc Logout(LogoutRequest) returns (LogoutResponse); // révoque le jeton
  rpc WhoAmI(WhoAmIRequest) returns (WhoAmIResponse); // utilisateur, rôle, expiration
  rpc ChangePassword(ChangePasswordRequest) returns (ChangePasswordResponse);
  // Administration (rôle Owner)
  rpc ListUsers(ListUsersRequest) returns (ListUsersResponse);
  rpc SetUser(SetUserRequest) returns (SetUserResponse);       // créer / modifier rôle / désactiver
  rpc RemoveUser(RemoveUserRequest) returns (RemoveUserResponse);
}
```

- **Utilisateurs** dans la SQLite de stationd, famille B (préservés à tout apply) ; mots de passe hachés (argon2). Rôles : enum fixe `Owner` / `Operator` / `Viewer` ; la liste exacte des capacités par rôle reste à définir (cf. architecture).
- **Jeton** opaque, transmis en métadonnée gRPC (`authorization: Bearer …`), durée de vie bornée, révocable. Un intercepteur tonic le vérifie sur **tous** les RPC et classe chaque RPC : lecture (`Viewer`), pilotage de l'antenne et édition (`Operator`), administration — utilisateurs, plugins, arrêt (`Owner`). Un refus est une erreur `PERMISSION_DENIED` explicite.
- **Écoute** : socket Unix local (accès de confiance, `stationctl` sur le nœud, pas de login) + TCP réseau local optionnel (`[grpc] listen = "0.0.0.0:…"`), toujours authentifié. Pas d'exposition publique : le web externe passe par `api`.
- **TLS** sur le TCP, activable (`[grpc.tls]`) ; certificat auto-signé généré par stationd au premier démarrage s'il n'en existe pas (comme le `.liq` et `icecast.xml`). La TUI mémorise l'empreinte à la première connexion et refuse un changement non confirmé.
- **Premier compte** : `stationctl user add <nom> --role owner` en local (socket Unix), rien de créé par défaut.
- **`api`** : reçoit le login du navigateur, appelle `AuthService.Login`, garde le jeton dans une session HTTP, le transmet à chaque appel gRPC. Aucune vérification de droits propre à `api`.
- CLI : `stationctl login|logout|whoami`, `stationctl user list|add|set|rm`. Un `stationctl` en TCP s'authentifie comme la TUI.

## 4. Architecture de la TUI

### 4.1 Découpage

```
stationd-tui (binaire séparé, même dépôt)
├── app        boucle rat-salsa, pile de navigation, registre d'écrans
├── auth       profils de connexion, login, jeton en mémoire, empreintes TLS connues
├── rpc        clients tonic générés + intercepteur de jeton + tâche de connexion (reconnexion, backoff)
├── store      derniers instantanés reçus, horodatés, par source
├── screens/   un module par écran, chacun implémente Screen
├── widgets/   composants maison (chips, timeline, barre de progression d'antenne…)
└── theme      couleurs, densité, variantes compactes
```

La TUI ne dépend **que** des types générés depuis `proto/` (crate ou module partagé avec `stationctl`), jamais des modules internes de `stationd`.

```rust
trait Screen {
    fn id(&self) -> ScreenId;
    fn title(&self) -> &str;
    fn key(&self) -> Option<char>;                 // touche d'accès directe
    fn available(&self, caps: &Capabilities) -> bool; // ex. Tags seulement si le plugin est chargé
    fn on_enter(&mut self, ctx: &mut Ctx);          // lance ses requêtes
    fn on_event(&mut self, ev: &Event, ctx: &mut Ctx) -> Action;
    fn render(&self, f: &mut Frame, area: Rect, store: &Store);
    fn help(&self) -> &[KeyHelp];
}
```

Le trait `Screen` est une couche mince au-dessus du modèle de rat-salsa (état d'écran, rendu, traitement d'événements, messages applicatifs) : il porte ce que rat-salsa ne connaît pas (touche d'accès, disponibilité selon les capacités et le rôle, aide).

### 4.2 Boucle et données

- Boucle d'application : rat-salsa (événements terminal, minuteries pour l'horloge et la progression, tâches de fond, messages entre écrans). Le raccordement des flux tonic (tâches tokio) à cette boucle est à valider au lot 0.
- Une tâche tokio par flux (`OnAir.Watch`, `Event.Watch`), les lectures ponctuelles en tâches courtes. Tout converge dans un canal vers la boucle d'interface ; le rendu ne fait jamais d'I/O.
- Chaque requête porte une clé de contexte (écran, filtre, date) : une réponse devenue obsolète est ignorée.
- Sélection suivie par identité (`rel_path`, `rule_id`, `playlist_ref`), jamais par numéro de ligne.
- Déconnexion : dernières données gardées avec leur âge, actions désactivées, reconnexion avec délai croissant plafonné. Une action mutante n'est **jamais** rejouée automatiquement.
- Tant que `OnAir.Watch` n'existe pas : repli par interrogation (`Liquidsoap.GetStatus` + `GetState` toutes les 2 s), annoncé dans le bandeau.

### 4.3 Écrans apportés par un plugin

Un plugin décrit ses vues en données, la TUI les rend avec des widgets génériques. Pas de code du plugin dans la TUI.

```json
{ "views": [ {
    "id": "top", "title": "Top 24h",
    "source": { "method": "top", "payload": {"since":"24h"} },
    "layout": "table",               // table | kv | bars | sparkline | gauge | list
    "columns": ["Titre","Diffusions"],
    "actions": [ {"key":"r","label":"Réinitialiser","method":"reset","confirm":true} ]
} ] }
```

L'onglet « Plugins » liste les vues des plugins chargés. `stationctl plugin view <plugin> <view>` affiche la même chose en texte (complétude CLI). L'écran Tags (§5.6) est un écran natif de la TUI qui parle au plugin via `Call` ; les vues déclaratives servent aux plugins plus simples (`play-stats`, `stop-when-idle`).

### 4.4 Langues (D12)

- Catalogues `crates/stationd-tui/locales/<langue>/tui.ftl`, intégrés au binaire (`fluent-templates`). Langue : `--lang`, sinon `LC_ALL` / `LC_MESSAGES` / `LANG` (« de_DE.UTF-8 » → `de`), sinon français. Clé absente d'une langue → français ; absente partout → `⟦clé⟧` affiché, jamais un vide.
- Dans le code : `tr!("cle", var = valeur)` ; une clé rangée dans une table est marquée `k!("cle")`.
- Tests : les trois catalogues ont exactement les mêmes clés ; toute clé utilisée dans le code existe, toute clé du catalogue est utilisée ; pluriels et variables se résolvent dans chaque langue.
- Opcodes : `onair_v1.Note.Code` (+ `plugin`, `reason`, `dj`, `media`). Un `match` exhaustif côté client : un code ajouté sans traduction ne compile pas ; un code inconnu d'un client plus ancien s'affiche « note inconnue (code N) ». Les états (`running`…), origines (`AtClockHard`…) et issues (`Outcome`) sont aussi des codes, traduits côté client.
- Ne se traduisent pas : les noms (playlists, titres, DJ, règles), les messages d'erreur techniques relayés tels quels, l'aide `--help` de la ligne de commande (affichée avant le choix de la langue), les journaux de stationd (anglais, destinés au développeur). `stationctl` reste en anglais (hors périmètre pour l'instant).
- L'allemand est à faire relire par un germanophone.

### 4.5 Widgets

Base : **rat-widget** pour tout ce qui est saisie et structure, **rat-focus** pour l'ordre de tabulation et le focus des formulaires et dialogues, **rat-theme** pour les palettes (thème sombre par défaut, couleur toujours doublée d'un texte). Widgets ratatui standard et quelques widgets maison pour le reste.

| Besoin | Source | Où |
|---|---|---|
| Champs texte, masques (heure `HH:MM`, durée `2h30m`), nombres, dates | rat-widget | Formulaires playlist et règle, overrides, ouvertures live |
| Listes de choix, cases à cocher, boutons radio | rat-widget | Mode, ordre, stratégie, soft/hard, jours de la semaine |
| Tableaux avec sélection (simple et multiple) | rat-widget | Médias, playlists, règles, historique |
| Calendrier mensuel | rat-widget | Agenda, `g` |
| Barre de menus, menus contextuels, popups | rat-widget | Actions par écran |
| Dialogues (message, confirmation, fichier) | rat-widget | Confirmations d'antenne, suppression, erreurs |
| Panneaux redimensionnables, onglets, vues défilantes | rat-widget | Mise en page des écrans |
| Ligne de statut | rat-widget | Bas d'écran |

| Widget | Où | Pourquoi |
|---|---|---|
| `LineGauge` | Antenne | Progression du morceau ; absente pour un flux sans durée |
| `Gauge` | Scan, opérations en lot | Avancement réel |
| `Sparkline` | Bandeau / Antenne | Auditeurs sur la dernière heure (échantillons reçus pendant la session) |
| `BarChart` | Statistiques, Tags, Médias | Diffusions par playlist/règle, répartition par genre ou tag |
| `Calendar` (ratatui) | Agenda, `g` | Choisir une date |
| `Table` + `Scrollbar` | Partout | Listes longues, position visible |
| Timeline maison | Agenda jour | Blocs de base, repères `at_clock`, repères `~` pour `every` |
| Grille semaine maison | Agenda semaine | 7 colonnes, pas 15/30/60 min, compteur si repères empilés |
| Chips colorées | Médias, Tags, Playlists | Genres, tags, mode de playlist ; texte toujours présent |
| Arbre (`tui-tree-widget`) | Médias, Groupes | Dossiers de `media/`, membres de groupes imbriqués |
| Éditeur de texte multiligne | TOML brut (rat-widget si suffisant, sinon `tui-textarea`) | Édition directe d'un TOML |
| Recherche floue (`nucleo`) | Sélecteurs | Trouver playlist, média, règle, tag en 3 lettres |
| Spinner (`throbber-widgets-tui`) | Calculs | Aperçu de pool, simulation, scan en cours |

À vérifier au lot 0 (non vérifié à la rédaction) : versions de rat-salsa / rat-widget / rat-focus / rat-theme, compatibilité avec la version de ratatui retenue, cohabitation de la boucle rat-salsa avec tokio et les flux tonic, et pour chaque crate tierce restante si rat-widget couvre déjà le besoin (on n'en garde qu'une par besoin).

## 5. Écrans

### 5.0 Connexion

- Au lancement : `stationd-tui` sur le nœud se connecte au socket Unix local (pas de login). `stationd-tui --host devstationd.lan` (ou un profil nommé de `~/.config/stationd-tui/config.toml` : hôte, port, TLS, utilisateur) ouvre l'écran de login : utilisateur, mot de passe masqué, station visée.
- Première connexion TLS : affichage de l'empreinte du certificat, confirmation, mémorisation. Empreinte changée ensuite : refus, avec l'ancienne et la nouvelle affichées.
- Le jeton reste en mémoire (pas écrit sur disque). Expiré ou révoqué : retour à l'écran de login, **brouillons en cours conservés**.
- Le rôle connecté conditionne l'affichage : actions interdites grisées avec la raison (« rôle Operator requis »). stationd reste seul juge : un refus serveur s'affiche comme tel.

Bandeau permanent (2 lignes) : station · utilisateur et rôle (ou « local ») · état de diffusion (RUNNING / PAUSED / DRAINING / SLEEPING, en couleur **et** en texte) · connexion à stationd (hôte, TLS ou non) · auditeurs (`—` si inconnu) + sparkline · live (DJ à l'antenne) · overrides en attente · horloge et fuseau station · uptime.
Onglets : `1` Antenne · `2` Contrôle · `3` Playlists · `4` Agenda · `5` Médias · `6` Tags · `7` Système · `8` Plugins. Ligne de statut + ligne de raccourcis en bas.

### 5.1 Antenne (`1`) — vue principale

```
┌ À L'ANTENNE ─────────────────────────────────────────────────────────────┐
│ Daft Punk — Veridis Quo                                     ▶ RUNNING    │
│ ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━──────────────  3:12 / 5:44  -2:32 │
│ PL soirée-électro · membre deep-house · règle nuit · DAY_PART            │
├ PLAYLISTS ───────────────────────┬ À SUIVRE ─────────────────────────────┤
│ ● soirée-électro   depuis 22:00  │   22:41  Air — La femme d'argent  7:11 │
│   jingle-station   ~22:45 every  │ ~ 22:48  Moby — Porcelain         4:01 │
│   news             23:00 at_clock│ ~ 22:52  [jingle-station] ID 03   0:08 │
│   nuit-calme       00:00 day_part│ ~ 22:52  Röyksopp — Eple          3:41 │
│ Indicatif : pub · every 6 titres │ …  (10)   ⚠ ordre aléatoire           │
├ JOUÉS ───────────────────────────┴───────────────────────────────────────┤
│ 22:32 Justice — Genesis              soirée-électro  diffusé             │
│ 22:28 [jingle-station] ID 01         jingle-station  diffusé             │
│ 22:21 Kavinsky — Nightcall           soirée-électro  coupé (skip)        │
└──────────────────────────────────────────────────────────────────────────┘
```

- **À l'antenne** : titre/artiste (repli nom de fichier + « titre non renseigné »), progression, playlist, membre de groupe, règle, origine (OVERRIDE avec sa source, FALLBACK, LIVE avec le DJ).
- **Playlists** : en cours (●) puis les suivantes avec heure (issue de `Preview`) ; règles au compteur à part, sans heure.
- **À suivre** : 1re ligne = préchargé (certain, sans `~`), puis théoriques `~` ; notes d'incertitude en pied. Heure estimée absente dès qu'une durée est inconnue.
- **Joués** : 20 derniers, date et heure de début (`JJ/MM HH:MM`, heure de la station), issue (diffusé / coupé / sauté) ; `PageDown` en bas de liste charge la suite via `History`.
- **Incidents** : une ligne de playlist fautive est en rouge, suivie de sa raison (« pool vide : rien ne passera », « rien de jouable ») ; en pied d'« À suivre », les incidents prévus et constatés passent avant les autres notes, en rouge (§3.3 bis).
- Actions : `Espace` pause (confirmée) / reprise / réveil · `n` suivant (confirmation nommant le morceau et le suivant prévu) · `o` pousser un override (formulaire, puis confirmation) · `+`/`-` changer X — faits au lot 2. `Entrée` détail (fiche média + pourquoi ce titre) · `p` aller à la playlist : plus tard.
- 80×24 : À l'antenne + onglets Playlists / À suivre / Joués au lieu de trois panneaux.

### 5.2 Contrôle (`2`)

Tout ce qui agit sur la station, regroupé, avec l'état actuel à côté de chaque action.

| Bloc | Contenu | Actions |
|---|---|---|
| Diffusion | État, raison, auditeurs | pause, reprise, suivant, veille quand plus d'auditeurs, réveil |
| Overrides | File en attente (contenu, soft/hard, expiration, source, `degraded`) | pousser (média ou playlist, soft/hard, expiration, nb de pistes), vider |
| Live | DJ à l'antenne, dernier live et raison de fin, ouvertures actives, DJ refusés | couper le DJ, ouvrir un créneau ponctuel, le fermer |
| File `queue` | Playlists `queue` et leur tampon | mettre un média en file (sélecteur média) |
| Bibliothèque | Dernier scan (trouvés/écartés/disparus) | lancer un scan (jauge), voir les fichiers écartés et leur raison |
| Plugins | Nom, activé, état, raison, échecs, capacités | start, stop, restart, reload |
| Station | Nom, uptime, PID | arrêt opérateur (`Shutdown`, `--force` si live, double confirmation) |
| Utilisateurs (`Owner`) | Nom, rôle, actif, dernière connexion | créer, changer le rôle, désactiver, supprimer, réinitialiser le mot de passe |

Toute action qui touche l'antenne : dialogue qui nomme l'objet exact, « Annuler » sélectionné par défaut. Résultat affiché (`from → to`, `changed`, `degraded`), l'état réel confirmé par le prochain instantané.

**Fait au lot 2** (`screens/controle.rs`, `screens/ops.rs`, `action.rs`, `dialog.rs`) : sections à gauche (`Tab` / `Maj+Tab`), détail à droite, touches de la section sur la ligne du bas et dans `?`.
- Diffusion : `Espace` pause/reprise/réveil, `n` suivant, `v` veille dès 0 auditeur, `w` réveil.
- Overrides : tableau (n°, contenu, mode, reste, péremption, source), `↑↓` choisir, `o` pousser, `d` retirer, `D` vider.
- Live : DJ à l'antenne (accès, depuis, adresse), comptes, droit urgent, refroidissements, refusés, dernier refus, ouvertures ; `k` couper, `o` ouvrir (DJ, durée), `c` fermer l'ouverture choisie.
- File `queue` : `e` (playlist, chemin) ; stationd refuse une playlist qui n'est pas en mode `queue` (`FAILED_PRECONDITION`).
- Bibliothèque : `s` scan (confirmé, canal sans délai maximal), rapport du dernier scan lancé depuis la TUI et fichiers écartés avec leur raison.
- Plugins : tableau, `s` / `x` / `r` / `l` = start / stop / restart / reload (confirmés).
- Station : `a` arrêt opérateur, `A` forcé (coupe le DJ) — deux confirmations ; la perte de liaison qui suit ne masque pas le résultat.
- Hors lot 2 : sélecteur de média (lot 4, `SearchMedia`), liste des playlists `queue` et de leur tampon, jauge de scan (`ScanWatch`, lot 7), Utilisateurs (lot A).

Dialogues : confirmation (« Annuler » par défaut, `←/→`, cadre rouge si dangereuse, enchaînement pour la double confirmation) et formulaire (texte / choix fermé, `Tab`/`↑↓`, erreur de saisie affichée sans fermer, confirmation optionnelle après validation). Une modale capture tout. Une action mutante n'est jamais rejouée.

### 5.3 Playlists (`3`)

**Fait (lot 4b)** — `screens/playlists.rs`, `screens/editor.rs`, `screens/picker.rs`, `draft.rs` :
- Liste : ref, nom, mode, activée (grisée sinon), « utilisée par » (n règles · n groupes, de `PlaylistSummary`) ; filtre `/`, tri `s` (ref, nom, mode), `r` relire, `R` relire la racine (`Reload`, confirmé). Détail de la sélection (lu 200 ms après le dernier mouvement) : fichier et révision (`Export`), écart fichier / appliqué, qui la référence, pool du jour (`PreviewPool` du fichier), le TOML du fichier.
- `n` : choix du mode, puis brouillon neuf (ref saisi dans le formulaire). `Entrée` / `e` : ouvre le **fichier** (commentaires compris) avec sa révision ; une entrée sans fichier (`add`) est expliquée et non ouverte.
- Éditeur : formulaire à gauche (Identité, Sélection par mode, Diffusion), TOML et diagnostics à droite, pool du jour dessous ; à moins de 110 colonnes, formulaire / diagnostics / pool empilés et TOML via `Ctrl+T`. Le formulaire n'est qu'une vue du TOML (`toml_edit` : commentaires et mise en forme gardés, valeur mal typée écrite telle quelle, champs facultatifs « non précisé » = clé retirée, tables vides retirées). Changer de mode met de côté les champs de l'ancien mode et les rend si on y revient.
- Par mode : statique = médias listés (`Ctrl+N` ouvre la recherche de Médias en sélecteur, `Espace` marque, `Entrée` ajoute, sans doublon) ; dynamique = combinaison, ordre, `order_by` / `unplayed_only` si ordre daté (ou déjà présents), filtres (champ → opérateurs proposés pour ce champ → valeur retypée : liste pour `has_any/has_all/has_none`, nombre pour année/durée ; genres connus proposés sous la valeur, `ListGenres`) ; groupe = stratégie, `on_member_unavailable` (sequence / shuffle), membres (`Ctrl+N` : sélecteur de playlists) avec poids ou `take` / `runtime` selon la stratégie ; file = ordre, longueur ; relais = adresse. `Ctrl+D` retire, `Alt+↑/↓` déplace.
- `PreviewPool` 300 ms après la dernière frappe : diagnostics (traduits par `Code`, rattachés à leur ligne : ✗ / ⚠, détail sous la ligne qui a le focus, `F8` va au suivant), pool (nombre, durée, artistes, échantillon ; par membre pour un groupe ; vide en rouge ; « non mesurable » pour relais et file).
- `Ctrl+T` : TOML brut (éditeur de texte) ; un TOML illisible y reste modifiable, le formulaire attend.
- `Ctrl+S` : `Save` avec la révision lue. Invalide : rien d'écrit, focus sur la première erreur, saisie gardée. Conflit : « Garder le brouillon » (défaut) / « Comparer » (fichier du nœud et brouillon côte à côte, lignes différentes en orange) / « Recharger (brouillon perdu) » — jamais d'écrasement. `Échap` sur un brouillon modifié : confirmation.
- `d` : si des règles ou groupes la référencent, message qui les nomme (rien n'est envoyé) ; sinon confirmation qui nomme le fichier effacé, puis `Remove` avec la révision lue au détail.
- Écarts à la description ci-dessous : pas de `handle` ni de `member_only` (absents de la grammaire), pas d'arbre des dossiers ni de recherche floue (la recherche de Médias sert de sélecteur), pas d'éditeur externe, pas de `x` (le détail montre le TOML).

**Liste** : nom/ref, mode (chip), activée, `member_only`, taille du pool et durée (issues de `CheckCoverage` / `Occurrence`), règles qui la référencent, groupes qui la contiennent, état de validation. Filtre `/`, tri.

**Création / modification** (`n` / `e`) : formulaire à gauche, TOML généré + diagnostics à droite.

1. Identité : `name`, `handle` optionnel, `enabled`, `member_only`. Le fichier cible est proposé à partir du ref.
2. Mode : les 5 modes de la grammaire TOML (static, dynamic, remote, queue, group) ; chaque mode n'affiche **que** ses champs. La TUI ne fait que présenter : la validation reste celle de stationd (`Validate`, §3.5).
   - *static* : sélecteur de médias (recherche floue + arbre des dossiers, sélection multiple), ordre sequential/shuffle, réordonnancement `u`/`j`.
   - *dynamic* : éditeur de filtres ligne par ligne (champ → opérateurs valides pour ce champ → valeur typée ; valeurs de genre proposées depuis `ListGenres`), match all/any, ordre, `order_by` seulement si ordre daté, `unplayed_only` seulement si ordre daté.
   - *group* : stratégie, membres (sélecteur de playlists), `take` ou `weight` selon la stratégie, `on_member_unavailable`.
   - *queue / remote* : champs de leur grammaire (cf. `examples/`), même principe.
3. Diffusion : `limit`, `repeat` (interdit avec `unplayed_only`), `on_exhausted`, contraintes anti-répétition (durées saisies en `2h`, `30m`).
4. **Aperçu du pool en direct** (`PreviewPool` §3.5, 300 ms après la dernière frappe) : nombre, durée, durées inconnues, 20 premiers médias. Pool vide = alerte rouge.
5. `Ctrl+S` : `Save` (§3.5) avec la révision lue à l'ouverture → stationd valide, écrit le fichier sur le nœud et applique. La TUI n'écrit jamais de fichier, qu'elle soit locale ou distante. Une erreur place le curseur sur le champ indiqué par `field_path` et garde toute la saisie. Un conflit de révision (fichier modifié entre-temps) ouvre un dialogue : comparer, recharger (perte du brouillon annoncée) ou garder le brouillon ; jamais d'écrasement.

Autres actions : `E` éditer le TOML brut (éditeur intégré, puis `Save` — un éditeur externe n'a de sens qu'en local), `x` exporter, `d` supprimer (affiche d'abord règles et groupes qui la référencent), `r` recharger.

### 5.4 Agenda (`4`)

**Fait (lot 6a, lecture)** — `screens/agenda.rs`, `agenda.rs` (mise en forme pure, testée) ; une lecture = `Preview` de la période (commencée une heure plus tôt : les rendez-vous en retard qu'une projection neuve rejoue à sa première minute tombent hors de l'écran, et la base active à minuit est connue) + `ListRules` + `CheckCoverage`, en parallèle, n° de requête (une réponse tardive d'une autre période est ignorée) ; relecture en échec sur la même période = données gardées marquées anciennes, sur une autre période = rien sous de nouvelles dates.
- Jour : créneaux de 15/30/60 min en temps réel entre deux minuits civils (25 h → 25 lignes à 60 min, heure répétée `02:00+02` / `02:00+01`, heure sautée absente, décalage affiché sur toute la journée d'un changement d'heure) ; bande de couleur de la base (une couleur par playlist, nom écrit là où elle commence ; une base qui change sous un rendez-vous commence avec lui) ; colonne `♪` quand une fenêtre live est ouverte ; repères du créneau (`!` hard, `*` soft, `♪` ouverture live) puis `+N` ; `▶` maintenant. Inspecteur à droite (≥ 110 colonnes, sinon `Entrée` en plein écran) : bases, repères et fenêtres live du créneau, chacun avec sa règle décrite (`ListRules` : fenêtre, cadence, soft/hard, péremption, jours, dates, désactivée), son pool, la décomposition du groupe (take / runtime / offset) et son verdict de couverture avec ses causes traduites ; `Tab` choisit l'élément, `p` ouvre sa playlist dans Playlists (`Handoff::Select`). Sous la vue (jour et semaine) : la zone **hors horloge** — les `every` de la période, à l'intervalle comme au compteur de pistes (playlist, cadence, règle), car leur heure réelle dépend du dernier passage ou du nombre de pistes ; au plus 5 lignes, le reste via `l` — puis la légende.
- Semaine (`v`) : 7 colonnes (≥ 100 colonnes) aux heures civiles du pas, case = de cette heure à la ligne suivante (l'heure répétée tient dans la case de 02:00, l'heure sautée est marquée), base en couleur, nom là où elle commence, repères : leur signe s'il n'y en a qu'un, sinon leur nombre ; `Entrée` ouvre le jour sur ce créneau. Sous 100 colonnes : un résumé par jour (bases avec leur heure de début, repères par nature).
- Couverture (`c`) : `CheckCoverage` trié pire en tête (verdict, règle, nature, playlist, pool) ; détail de la règle choisie : description, causes, membres avec leur verdict. `p` ouvre la playlist.
- Navigation : `[` / `]` jour ou semaine, `t` aujourd'hui (curseur sur maintenant), `g` calendrier du mois (flèches, PgPréc/PgSuiv, `t`, Entrée), `+` / `-` pas (le curseur garde son heure), `r` relire.
- Les `every` à l'intervalle ne sont plus posés sur la ligne du temps (2026-09-30) : `stationctl schedule preview` les projette toujours depuis le début de la projection, l'Agenda les range hors horloge.

**Fait (lot 6b, 2026-09-30, édition)** — `screens/ruleform.rs`, `gridraft.rs` (le TOML de la grille modifié par `toml_edit`, une règle à la fois, commentaires gardés, valeurs écrites telles que saisies) :
- Grille regardée : l'active par défaut ; `G` liste les grilles du nœud (active ●, affichée, règles, premier problème traduit) : `Entrée` affiche une grille en préparation (bandeau orange « GRILLE EN PRÉPARATION », projection / couverture / règles de CE fichier), `a` l'active (confirmation), `c` la copie sous un nouveau nom.
- `n` (vue jour) : nature (tranche, rendez-vous, every, live, plancher), puis formulaire pré-rempli au créneau choisi — id libre `evt-AAAAMMJJ-HHMM`, heure de début / du rendez-vous ; aucune date imposée (règle récurrente par défaut). `l` (jour, semaine) : toutes les règles de la grille, y compris hors horloge, désactivées ou d'autres jours — `Entrée`/`e` modifier, `d` supprimer, `n` nouvelle. `e` : la règle de l'élément choisi dans l'inspecteur (`Tab`). `d` : suppression (confirmation qui dit si l'antenne change).
- Formulaire par nature (id, activée, playlist — `Entrée` ouvre le sélecteur —, DJ, début / fin, repère à heure fixe ou toutes les N min, soft / hard, péremption, cadence temps / pistes, jours à cocher, dates) ; aide de format sous le champ. 300 ms après une frappe : `ValidateGrid` du brouillon (✗ et raison traduite sous le champ fautif ; problèmes du reste de la grille listés à part : ils bloquent aussi) et `Preview` du brouillon sur la journée (bases avec leur règle, repères de CETTE règle, ou « ne joue pas ce jour-là »).
- `Ctrl+S` : `SaveGrid` avec la révision lue ; active = à l'antenne aussitôt. Conflit : rien d'écrit, `Ctrl+R` relit le fichier et y reporte la saisie (la règle retrouvée par son id ; disparue = ajoutée). `Échap` sur une saisie modifiée : deuxième `Échap` pour abandonner.
- `u` : heures en UTC ou en heure de la station (jours, créneaux, repères, inspecteur, semaine ; le curseur garde son instant). Les règles restent en heure civile de la station (la grammaire) : le formulaire le dit, la légende aussi.
- Écarts : pas de comparaison côte à côte au conflit (le report de la saisie la remplace) ; pas de changement de nature d'une règle (la supprimer, en créer une autre) ; DJ saisi à la main (pas de liste des DJ par RPC).

**Jour** (défaut) : timeline verticale 00:00→24:00 dans le fuseau station.
- Bandes de fond : base active (`day_part`, `base_rotation`), une couleur par playlist, nom dans la bande. Un `day_part` qui traverse minuit est dessiné sur les deux jours.
- Repères : `at_clock` (heure exacte, soft/hard), créneaux `live` (DJ).
- Sous la timeline : zone hors horloge, les `every` (à l'intervalle ou au compteur), sans heure.
- Inspecteur : règle, playlist, pool (`selected_count`, `total_duration`), décomposition de groupe (membres, `take`/`runtime`, offsets), verdict `CheckCoverage` (pool vide, anti-répétition intenable, source trop courte) en couleur.
- Heures répétées ou sautées au changement d'heure : affichées avec leur décalage UTC (`at_local` du preview).

**Semaine** (`v`) : 7 colonnes, pas 15/30/60 min (`+`/`-`), repères empilés → compteur puis liste. En 80 colonnes : liste des 7 jours avec résumé, `Entrée` ouvre le jour.

**Couverture** (`c`) : tableau `CheckCoverage` de toute la grille, pire verdict en tête.

**Édition de règle** (`n` / `e` / `d`) : formulaire par type (`base_rotation`, `day_part`, `at_clock`, `every`, `live`) + validité (jours, dates). `ExportGrid` → modification → `ValidateGrid` (diagnostics, §3.6) → aperçu de la journée **avec** la modification → `SaveGrid` (écriture par stationd, révision, conflit comme les playlists).

Navigation : `[`/`]` jour ou semaine précédente/suivante, `t` aujourd'hui, `g` calendrier, `Entrée` inspecteur, `p` aller à la playlist.

### 5.5 Médias (`5`)

**Fait (lot 4a)** : recherche (`/` ; mots + `genre:x` / `dossier:x`), tri `s` / `d`, filtre de métadonnée manquante `m`, disparus `a`, pages chargées en descendant, `o` override du média.
**Fait (lot 4b)** : fiche (`Entrée` : métadonnées, taille, disponibilité, playlists qui peuvent le diffuser — `Containing`, avec ce qui les référence —, diffusions 24 h / 7 j / 30 j / total en diffusées / choisies et dernier choix — `Plays` par `key` ; `↑↓` média voisin, `o` / `p` / `f` depuis la fiche). Sélection multiple (`Espace`, `c` tout démarquer) ; sur la sélection (ou la ligne) : `p` ajouter à une playlist statique — sélecteur des statiques ou « Nouvelle playlist… », puis l'écran Playlists s'ouvre sur le **brouillon** avec les médias ajoutés, rien n'est enregistré avant `Ctrl+S` ; `f` mettre en file d'une playlist `queue` (`Enqueue` un par un, arrêt au premier refus, compte rendu). `q` de la description ci-dessous est devenu `f` (`q` = quitter). `e` : modifier les tags (titre, artiste, album, année, genres du fichier, sources `custom-tags` comme `Type`, BPM, tempo, date de création) — formulaire `screens/tagform.rs` ; genres et sources choisis dans la liste des genres connus (`ListGenres`, filtre à la frappe, « ＋ nouveau » en dernier), tempo en choix (auto = tiré du BPM, ou un libellé) ; un média : valeurs lues dans le fichier, seuls les champs changés partent, vide = retiré ; plusieurs marqués : vide = inchangé, genres et sources en ajout / retrait (Espace : ajouter → retirer → inchangé) fusionnés fichier par fichier ; confirmation qui liste les changements, écriture par stationd (`SetTags`), la liste et la fiche se relisent. La fiche montre aussi les tags du fichier (`GetTags` : BPM, tempo et date de création effectifs, choisis à la main ou déduits, sources comme `Type`) et suit la ligne relue après une écriture ; l'écriture est vérifiée sur les tags relus renvoyés par `SetTags` (un champ non appliqué = échec nommé). Restent : tags libres (lot 8), panneaux de répartition, scan avec jauge (lot 7).

- Barre de recherche + filtres (**Type** en premier, genre, tag, dossier, disponible, titre/artiste/Type manquant) + tri. Colonnes : Type (chip colorée), artiste, titre, album, année, durée, genres, tags, dispo. Total / chargés. Pagination serveur (`SearchMedia`). Type et tags visibles seulement si le plugin `tags` est chargé.
- **`t` : affecter un Type** à la ligne ou à la sélection multiple, via un sélecteur des valeurs déclarées (une touche par valeur). C'est l'action la plus fréquente de l'écran : pas de dialogue supplémentaire pour un seul média, confirmation avec le nombre de fichiers pour un lot.
- Panneau de répartition par Type (barres), avec le nombre de médias sans Type.
- Panneau Genres : `ListGenres` en barres, graphies incohérentes signalées (`spellings > 1`).
- **Fiche** (`Entrée`) : métadonnées, fichier, disponibilité, tags (édition si plugin chargé), playlists statiques qui le contiennent, statistiques de diffusion (`Plays` by MEDIA), dernier passage.
- `s` scan (jauge, rapport des fichiers écartés), `o` pousser en override, `q` mettre en file.
- Sélection multiple (`Espace`) → actions en lot (tags, ajout à une playlist statique en brouillon).

### 5.6 Tags (`6`)

**Fait (2026-09-30, voie native)** : un onglet par origine (`Tab`) — genre du fichier, puis chaque source `custom-tags` ; valeurs avec effectif en barres, graphies incohérentes signalées, médias sans valeur ; tri nom / effectif (`s`), relire (`r`). `e` : renommer ou fusionner la valeur — saisie (préremplie, Entrée) → aperçu (fichiers, fusion, playlists qui filtrent dessus) → Entrée → avancement fichier par fichier (jauge) → bilan (réécrits, déjà sans la valeur, échecs listés) ; un renommage lancé n'est pas interrompu. Médias : `t` affecte une valeur de la première source (`Type`) à la ligne ou aux marqués — valeurs les plus employées en tête, « autre valeur… », « retirer » ; un média s'écrit sans question, un lot après confirmation (`SetTags`, comme `e`). Pas de tags libres `TXXX:Tags` (plugin non fait) : une source de plus dans `custom-tags` en tient lieu.

Prévu à l'origine :

Parle uniquement au plugin via `PluginService.Call`. Méthodes attendues du plugin (contrat à figer avec lui) : `types` (valeurs de Type + effectifs), `set_type` (par média ou par lot), `list` (tags libres + effectifs), `get` / `set` / `add` / `remove` (par média ou par lot), `create`, `rename`, `merge`.
- Deux panneaux : **Types** (valeurs déclarées, effectifs, médias sans Type) et **Tags** (tags libres, effectifs, médias sans tag).
- Types : la liste des valeurs vient de la config du plugin ; la TUI ne l'édite pas (modification de la config), elle en affiche l'usage.
- Tags : `n` créer un tag, puis l'affecter depuis Médias.
- Renommer / fusionner : aperçu (N fichiers, playlists dynamiques dont un filtre utilise ce tag) → confirmation → avancement fichier par fichier → rapport (échecs listés).
- Étiquetage en lot depuis Médias.
- Absent si le plugin n'est pas chargé ; son état apparaît dans Contrôle › Plugins.

### 5.7 Système (`7`)

**Fait (2026-09-30)** : trois sections (`Tab`). **État** : stationd (station, version — du fait de démarrage —, uptime, pid, fuseau, journal suivi), Liquidsoap (antenne, à l'antenne, dernière demande, pistes démarrées, socket de contrôle), Icecast (serveur, dernière lecture, auditeurs, problème, chaque mount : présent / sans source / alimenté, débit annoncé et reçu, auditeurs), live (à l'antenne, harbor, dernier refus), bibliothèque (scan en cours avec jauge, sinon le dernier). Icecast est lu avec le bandeau (2 s). **Journal** : suivi en continu par l'application (flux rouvert après une coupure, historique remplacé), défile seul, `↑` / PgPréc met en pause (« N nouveaux »), `Fin` reprend ; `l` niveau (tout / avertissements / erreurs), `c` composant, `/` recherche dans le texte traduit. **Statistiques** : `Plays`, `w` fenêtre 30 min / 24 h / 7 j / 30 j, `b` regroupement (playlist, feuille, règle, origine, média, artiste), barres des diffusés + diffusés / choisis / dernier, total. Contrôle › Bibliothèque montre aussi la jauge du scan en cours.

Prévu à l'origine :

- État : stationd (uptime, version), Liquidsoap (`GetStatus` : dernières demandes, dernier `/next`, `on_air_kind`), Icecast (serveur, dernière lecture, problème, audience, mounts : présent, source connectée, débit annoncé/réel), live (fichier des DJ lisible, nombre de DJ).
- Événements (`EventService.Watch`) : défilement auto, pause en remontant, filtres niveau/composant, recherche.
- Statistiques : `Plays` par playlist / feuille / règle / origine / média, fenêtre 30m / 24h / 7j, en barres + tableau.

### 5.8 Plugins (`8`)

Vues déclaratives des plugins chargés (§4.3). Base de chaque plugin : `DbInfo`, requête `DbQuery` en lecture seule.

## 6. Raccourcis communs

`1`–`8` écrans · `Tab` panneau suivant · `Entrée` détail · `Esc` retour (contexte restauré) · `/` rechercher · `?` aide de l'écran · `Ctrl+S` enregistrer · `q` quitter (hors saisie). En saisie, tous les caractères vont dans le champ. Une modale capture tout.

## 7. Lots

| Lot | Contenu | Dépend de |
|---|---|---|
| 0 | Crate proto partagée, squelette TUI (rat-salsa, `Screen`, registre, connexion socket Unix, bandeau), vérification des crates rat-* | — |
| A | stationd : `AuthService`, intercepteur de droits sur tous les RPC, écoute TCP + TLS auto-signé, `stationctl login/user` ; TUI : écran de login, profils, empreintes | Avant tout usage distant ; indépendant des lots 1–8 en local |
| 1 ✅ | stationd : `OnAirService` (Watch + History + simulation §3.3), `stationctl onair`, TUI : écran Antenne | — |
| 2 ✅ | TUI : Contrôle (+ actions depuis Antenne : pause, suivant, override) ; stationd : incidents de grille (prévus / constatés, ligne fautive) | 1 |
| 3 ✅ | stationd : `PlaylistService` (`Validate` / `PreviewPool` / `Save`, écriture + révision, diagnostics par champ), `SearchMedia` | — |
| 4 ✅ | TUI : Médias (4a : recherche, filtres, tri, pages, override ; 4b : fiche, sélection multiple, ajout à une statique, mise en file) ; Playlists (4b : liste + éditeur) ; stationd : références dans `List`, `Containing`, `Plays.key` | 3 |
| 5 ✅ | stationd : grilles en fichiers (une active), diagnostics de grille, `SaveGrid`, projection d'un brouillon | — |
| 6 ✅ | TUI : Agenda (6a jour, semaine, couverture ; 6b édition, grilles, UTC) | 5 pour l'édition |
| 7 ✅ | stationd : `WatchScan`, `EventService` ; `ListTagValues` / `RenameTagValue` (voie native, §3.8) — `PluginService.Call/Views` et `media_meta_write` non faits | — |
| 8 (✅ sauf Plugins) | TUI : Type dans Médias (`t`), Tags, Système ; Plugins (vues déclaratives) reste à faire | 7 |

Chaque RPC ajouté a sa commande `stationctl` dans le même lot.

## 8. Recette

- Naviguer partout, ouvrir tous les aperçus et la simulation : aucun curseur, groupe, `Every`, historique modifié (comparaison famille B avant/après).
- Antenne : le préchargé correspond à `next_media` de Liquidsoap ; après chaque changement de piste, l'ancien 1er « à suivre » est à l'antenne ou une note expliquait pourquoi non.
- Flux sans durée : ni barre ni heure de fin inventées ; heures estimées absentes au-delà.
- Auditeurs inconnus affichés `—`, jamais 0.
- Playlist créée par formulaire, relue par `Export` : identique champ à champ ; commentaires du TOML existant préservés par stationd au `Save`, ou édition par formulaire bloquée au profit de l'éditeur TOML intégré.
- Erreur de validation : curseur sur le bon champ, saisie conservée.
- Agenda sur les nuits de changement d'heure (mars/octobre) et un `day_part` 22:00→06:00.
- 120×35 et 80×24, redimensionnement pendant une saisie, coupure de stationd puis reprise.
- Quitter restaure le terminal et laisse la station tourner.
- TUI distante : login refusé (mauvais mot de passe, compte désactivé), jeton expiré en pleine édition (brouillon conservé), rôle `Viewer` (aucune action mutante possible, refus serveur si forcé), empreinte TLS changée (connexion refusée).
- Deux clients modifient la même playlist : le second `Save` est refusé pour conflit, rien n'est écrasé.

## 9. Ouvert

- Contrat exact du plugin `tags` : format de `TXXX:Tags` (séparateur NUL ID3v2.4, casse, espaces de noms type `mood:calm`), équivalent pour les formats non-MP3 (FLAC/OGG : champs Vorbis `TYPE` / `TAGS`).
- Capacités exactes par rôle (`Owner` / `Operator` / `Viewer`) : répartition RPC par RPC à fixer au lot A.
- Durée de vie des jetons et renouvellement (session TUI longue).
- Gestion des brouillons si la connexion est perdue longtemps : conservation en mémoire seulement, ou sauvegarde locale côté TUI ?
