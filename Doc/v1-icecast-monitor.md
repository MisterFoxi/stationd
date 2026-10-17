# V1 — supervision centrale indépendante

`station-watch`, dans le crate Rust `stationd-admin`, interroge en lecture seule
le gRPC administratif. Il tourne dans son propre conteneur `monitor`, sans port
publié, sans socket Docker et sans dépendance de démarrage envers `admin`.
Il peut donc constater une administration absente. Aucun appel de commande,
bascule, reconfiguration ou démarrage de StationD/Liquidsoap n'est effectué.
Les plugins dynamiques et l'administration native Icecast sont conservés.

## Démarrage et consultation

Depuis `/data/dev` :

```sh
make icecast-monitor-test
make icecast-monitor-up
make icecast-monitor-integration-test
make icecast-monitor-status        # via admin gRPC
make icecast-monitor-direct-status # gRPC direct si admin tombe
make icecast-monitor-logs
```

`icecast-monitor-up` construit et démarre **seulement** `monitor`, avec
`--no-deps`. Les autres conteneurs ne sont pas recréés. La configuration est
`config/icecast/monitor.toml`. Modifier les seuils nécessite de recréer uniquement
ce conteneur. Les configurations Icecast, plugins et la MMDB sont relues à
chaque observation. Elles restent montées en lecture seule.

Le superviseur dispose uniquement du jeton gRPC administratif. Le service gRPC
actuel utilise ce même jeton pour les opérations de lecture et d'écriture ;
un rôle limité à la lecture reste une amélioration d'authentification à prévoir.
Il ne monte ni les mots de passe des sources, ni le répertoire des secrets.

## Conditions suivies

- `source:<mount>` : source déconnectée sur un mount **déclaré** dans le TOML,
  y compris les entrées techniques. Aucune liste A/B, aucun inventaire des
  conteneurs émetteurs, aucun nombre de stations n'est codé en dur.
  `ignored_mounts` permet de déclarer les mounts volontairement inactifs.
  Un nom ignoré absent du TOML déclenche une anomalie de configuration.
- `admin:unavailable` : connexion, authentification ou réponse administrative
  impossible. `icecast:status_unavailable` distingue l'échec de la lecture du
  statut Icecast lorsque l'administration reste joignable.
- `collection:<plugin>` : dernière tentative ou dernière écriture réussie
  trop ancienne, plugin attendu absent, ou problème invité signalé. Les plugins
  attendus viennent de `admin.toml`, avec la capacité `listener_details`.
  Sans mount d’audience déclaré, la collecte est volontairement inactive.
  Un ancien compteur d'erreurs non nul ne suffit pas à maintenir une alerte.
  `collection:status_unavailable` signale une lecture de statut impossible.
- `disk:low:<path>` : espace **disponible** sous le seuil en octets **ou** en
  pourcentage. `disk:unavailable:<path>` signale une mesure impossible.
  Les emplacements observés sont les sauvegardes centrales, les volumes
  administratifs et du routeur, ainsi que GeoLite. Les médias NFS et données
  locales des futurs StationD ne sont pas couverts par ce superviseur central.
- `geolite:stale` : âge d'après le `build_epoch` interne de la MMDB, jamais
  sa date de copie. Base illisible : `geolite:unavailable`. Un horodatage futur
  est aussi signalé. GeoLite explicitement désactivée n'est pas une anomalie.
- `configuration:*` : configuration illisible/invalide, exclusion incorrecte
  ou délai de collecte inférieur à deux périodes configurées.

`last_success` conserve son sens : l'événement invité a été traité, même
si les observations étaient inconnues (`NULL`). Le nouvel horodatage gRPC
`last_complete_collection` avance seulement lorsqu'un lot non vide contient
des observations disponibles sur tous les mounts d'audience et que leur
traitement invité réussit. Un succès à zéro auditeur est une collecte valide.
La supervision vérifie aussi la fraîcheur de cet horodatage ; des écritures
continues de relevés inconnus déclenchent donc une alerte de collecte.
Ces diagnostics restent en mémoire, sans IP persistée et sans migration de base.
Ce contrôle ne remplace pas une vérification du contenu audio ni une sonde
depuis Internet.

## Temporisation et sorties

Valeurs initiales : observation toutes les 30 s, anomalie persistante 60 s,
collecte âgée de plus de 120 s, GeoLite de plus de 45 jours, espace disponible
sous 2 Gio ou 10 %. Les RPC ont un délai de 5 s et la connexion de 3 s.
La période s'ajoute au temps des sondes ; les délais ne sont pas des garanties
temps réel. Au démarrage, les conditions repartent avec leur temporisation.

Une transition produit une ligne JSON dans les journaux Docker :

```json
{"at":1792179000,"event":"alert","condition":"source:/radio.mp3","since":1792178940}
{"at":1792179060,"event":"resolved","condition":"source:/radio.mp3","since":1792178940}
```

Pas de répétition tant que la condition reste active. Une lecture impossible
ne résout jamais les anciennes alertes de source/collecte. Un inventaire relu
avec succès retire les conditions qui ne sont plus déclarées. Les événements
ne contiennent ni IP d'auditeur ni texte d'erreur invité ni secret.
Les journaux Docker sont limités à trois fichiers de 10 Mo.

Le service gRPC conserve le dernier état en mémoire. Le volume `monitor-state`
conserve aussi une copie diagnostique JSON, remplacée
atomiquement. Il expose `active`, les conditions en attente, les contrôles
observés et les mesures disque. Une clé de contrôle absente signifie inconnue.
Le volume est un diagnostic régénérable, hors sauvegarde des états centraux.

`station-watch --check` interroge le service gRPC indépendant et imprime cet état et termine avec 0 sans alerte active,
1 si alerte active, 2 si l'état manque ou est périmé (trois périodes + 30 s).
Le healthcheck Docker utilise cette commande via gRPC en boucle locale ;
il ne lit pas le fichier d’état. Un état `unhealthy` n'entraîne
pas de réparation automatique. `--once` fournit une observation immédiate,
sans toucher à l'état du daemon, sans temporisation ni émission d'événement.

Les alertes sont locales : aucun SMTP, webhook, message externe ou transfert
de sauvegarde n'est configuré. Les clients et la future console consultent les alertes via gRPC.
Une notification SMTP/webhook hors VM reste une extension distincte. Une panne de la VM elle-même nécessite une sonde
sur un autre hôte ; un conteneur local ne peut pas la signaler pendant l'arrêt.

## Validation sans toucher à la diffusion

Les tests Rust couvrent la temporisation, la déduplication, les retours à la
normale, les observations inconnues, les dates, les erreurs historiques,
les mesures disque et le retrait dynamique de conditions (sept mounts).
Ils ne lancent aucun service audio. L'observation du banc réel passe par
les lectures gRPC et disque ; les pannes doivent être simulées sur une instance
de supervision isolée, sans arrêter les sources actives pour tester une alerte.

La supervision ne clôture pas les autres travaux : copie protégée hors VM avec
rétention, revue des ajouts de sauvegarde/supervision, commit explicite, puis
propagation vers `V1`. `main` et `dev` restent inchangées.


Le test Docker `icecast-monitor-integration-test` utilise les images déjà
construites et une petite fixture officielle MaxMind téléchargée dans un
répertoire temporaire. Il crée son propre réseau et ses volumes, sans port
publié ni source audio : sept mounts déclarés, plugin attendu absent, MMDB
ancienne malgré une date de copie récente, espace insuffisant/inconnu, puis
arrêt et reprise de sa seule administration de test. Les ressources sont
retirées ensuite. Il ne modifie ni la MMDB ni les conteneurs du banc actif.


## Contrat gRPC et console native

Toutes les consultations passent par gRPC authentifié, avec les mêmes délais
et le jeton administratif existant. Le JSON tabulaire suit le contrat déjà
utilisé par `GetHistoryStatus` (`JsonReply`). Aucun fichier d'état partagé n'est
nécessaire entre administration et superviseur.

- `IcecastAdmin.GetMonitoringStatus` relaie l'état depuis le superviseur ;
  client : `stationctl icecast monitoring` et `make icecast-monitor-status`.
- `Monitoring.GetStatus` est servi indépendamment par `station-watch`, sur
  `monitor:50063` dans le réseau Docker, sans publication sur l'hôte ;
  client direct : `stationctl --addr http://monitor:50063 monitor-status`.
  Sur la VM, `make icecast-monitor-direct-status` exécute cette lecture dans
  le conteneur de supervision avec l'adresse de boucle locale.
- Un premier relevé encore absent ou trop ancien renvoie `UNAVAILABLE`.
  Une alerte active reste une réponse normale, avec la liste `active` ; les
  commandes `stationctl` terminent avec succès si la lecture RPC réussit.
- Un jeton incorrect renvoie `UNAUTHENTICATED`. La future console egui peut
  employer le même contrat administratif et afficher ces conditions.

À la première installation de cette extension du protocole, construire les
deux images et recréer uniquement l'administration, puis lancer le superviseur :

```sh
docker compose -f docker/icecast/compose.yaml build admin monitor
docker compose -f docker/icecast/compose.yaml up -d --no-deps --wait admin
make icecast-monitor-up
```

Cette recréation administrative ne redémarre ni Icecast ni le routeur. Les
plugins dynamiques sont rechargés et leur volume reste conservé. Le test isolé
vérifie les deux chemins RPC, le refus d'un jeton incorrect, l'accès direct
durant la panne d'administration et le cas sans mount d'audience.


## Validation du 16 octobre 2026

- 31 tests Rust usuels et le test explicite du vrai WASM : 32 réussis.
- Clippy sur toutes les cibles sans avertissement, formatage et diff vérifiés.
- Les 7 tests de sauvegarde restent réussis.
- Test Docker isolé réussi : sept mounts sans émetteur, plugin attendu absent,
  vraies écritures inconnues du WASM, collecte volontairement inactive,
  MMDB ancienne malgré sa copie récente, disque faible/inaccessible,
  arrêt/reprise de l'administration de test, déduplication et retours à la normale.
- Accès gRPC via l'administration et direct validés ; mauvais jeton refusé.
  Réseau/volumes de test retirés, aucun port publié ni source audio lancée.
- Images de production reconstruites avec recompilation des crates locaux du
  profil release ; les dépendances tierces restent dans le cache Docker.
- Seule l'administration a été recréée ; superviseur ajouté et sain, zéro alerte.
  Une même connexion HTTP audio a reçu 61 440 octets pendant cette opération.
  Identifiants et dates de démarrage d'Icecast, du routeur et des deux sources
  de banc conservés ; sélection B inchangée, collecte complète reprise sans erreur.
- Aucun StationD ni Liquidsoap lancé. Travail conservé sur `v1-dev`, socle
  `05104af`, sans commit, push ni propagation automatique vers `V1`.
