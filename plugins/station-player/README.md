# station-player — lecteur public

Plugin Rust natif dans `plugins/station-player/`, déclaré comme `remote-supervision`.
Aucune capacité `db`, `control` ou `push_override` nécessaire. Le plugin ne
filtre pas les pools. `plugin start|stop|restart|reload station-player` utilise
le cycle de vie existant ; port occupé = échec de chargement visible.

## Architecture et emplacement

```text
plugins/station-player/
  Cargo.toml        crate natif membre du workspace
  src/lib.rs        passerelle HTTP/SSE, adaptation gRPC, cache public
  src/daemon.rs     adaptateur au registre natif, compilé par le daemon
  src/tests.rs      tests Rust du véritable crate
  web/player.html   frontend public
  web/home.html     accueil et choix des stations
  web/player.css    thème et responsive
  web/player.js     lecture HTML5 et mises à jour SSE
  tests/web.test.cjs tests du frontend
  README.md         contrat, configuration et publication
```

`src/plugin.rs` enregistre le plugin et référence son adaptateur de 34 lignes
dans `plugins/station-player/src/daemon.rs`. Cet adaptateur raccorde le registre aux méthodes
`start`/`stop` du crate. Le player ne dépend pas du daemon, de sa base, de son
scheduler ni de ses fonctions de contrôle. Il dépend du contrat partagé
`crates/stationd-proto`, comme la TUI. Les tests utilisent ce même contrat avec
sa fonctionnalité optionnelle `server`, sans copie de protobuf ni faux `Host`.

```text
Navigateur ── HTTP/SSE public ── station-player ── Watch gRPC interne ── stationd
     └────────────────────── audio HTTP(S) ────────────────────────── Icecast
```

La sélection, le préchargement, les projections et le journal restent dans
stationd. Le player adapte ces lectures à ses DTO et à son affichage ; il ne
reconstruit pas une vérité métier. Le cache et le quota SSE sont propres au
transport public.

Comme Webmin dans l'implémentation actuelle du dépôt, ce plugin natif est du
code de confiance lié au processus du daemon, avec son runtime séparé. Il ne
bénéficie pas du sandbox WASM ni d'une isolation de processus. Le listener
public est une façade en lecture seule, configurable sur une IP ou un nom DNS ;
le port gRPC d'administration reste privé. Une passerelle dans un processus
distinct serait une autre décision de déploiement, pas réalisée ici.

Le crate natif est compilé avec stationd. Les commandes de compilation et de
packaging WASM ignorent `station-player` ; `make player-test` lance ses tests,
et `make test` les inclut avant les tests du daemon.

## Configuration

Un gateway peut présenter plusieurs stations. Chaque station pointe sur son
propre daemon gRPC ; le slug public ne permet pas de choisir un endpoint.

```toml
[[plugin]]
name = "station-player"
enabled = true

[plugin.config]
bind = "127.0.0.1:8091"
# Accès réseau direct : bind = "192.168.1.111:8091" ou "devstationd.example.org:8091"
max_event_streams = 128

[[plugin.config.stations]]
slug = "home-stone"
label = "Home Stone"
description = "Votre bande-son, toute la journée."
# Visuel optionnel par station, absent par défaut (aucune pochette nécessaire) :
# artwork_url = "https://radio.example.org/visual.jpg"
stream_url = "https://radio.example.org/home-stone.mp3"
grpc_endpoint = "http://127.0.0.1:50051"
# allow_plaintext_grpc = true # uniquement sur un réseau privé de confiance

[plugin.config.stations.theme]
accent = "#a3e635"
background = "#111827"
foreground = "#f9fafb"
```

Les trois couleurs sont strictement `#RRGGBB`. Adapter la palette pour garder
un contraste lisible. Les slugs sont uniques, minuscules, ASCII (lettres,
chiffres, tirets), 64 caractères maximum. Entre 1 et 64 stations ; entre 1 et
4096 connexions SSE simultanées. Les clés inconnues sont refusées.
La configuration utilise les tables TOML existantes du dépôt. Après une
modification du fichier, recharger la configuration selon le workflow stationd.

`bind` attend toujours **hôte:port**, sans `http://` ni chemin. Il accepte une
IP locale/réseau, `0.0.0.0` (toutes les interfaces IPv4), une IPv6 entre crochets
(`"[::]:8091"`) ou un nom DNS/FQDN (`"devstationd.example.org:8091"`).
Les noms internationaux s'écrivent en punycode. Le port doit être entre 1 et
65535. Le défaut reste `127.0.0.1:8091`.

Un nom DNS est résolu au démarrage du plugin, avec attente maximale de 3 s.
Les adresses IPv4/IPv6 retournées sont essayées dans l'ordre ; le premier bind
réussi est retenu. Le nom doit résoudre vers une interface du serveur où tourne
le plugin. Il ne peut pas désigner l'adresse d'un reverse proxy ou d'une autre
machine. Une erreur DNS ou de bind échoue au chargement avec sa raison.
Un changement DNS est pris en compte au prochain démarrage/rechargement.

## Routes publiques

| Route GET | Résultat |
| --- | --- |
| `/` | Ouvre directement le player s'il n'y a qu'une station, sinon présente le choix des stations |
| `/player/:slug` | Page responsive, lecteur HTML5 Icecast |
| `/player/:slug?compact=1` | Lecteur compact sans listes, adapté à un iframe |
| `/api/public/player/:slug` | Instantané JSON en lecture seule |
| `/api/public/player/:slug/events` | SSE, événements `snapshot` contenant le JSON complet |

Les méthodes de mutation sont refusées (405). Un slug absent retourne 404.
Le player utilise des URL absolues à la racine : conserver ces chemins dans le
reverse proxy. L'API est de même origine, sans accès CORS arbitraire. Pas
d'authentification nécessaire : ces données sont volontairement publiques.

```html
<iframe src="https://radio.example.org/player/home-stone?compact=1"
        title="Écouter Home Stone" width="100%" height="340"
        style="border:0" loading="lazy"></iframe>
```

La lecture reste déclenchée par l'utilisateur. Les boutons lecture/pause et
silence ainsi que le curseur de volume sont accessibles au clavier. Si le
JavaScript ne peut pas s'exécuter, les contrôles audio natifs restent visibles.
Les mises à jour de métadonnées ne rechargent pas le flux.
La pause ferme la connexion audio ; la prochaine lecture reprend au direct.
Icecast doit fournir un format pris en charge par le navigateur. Sur une page
HTTPS utiliser impérativement un `stream_url` HTTPS pour éviter le contenu mixte.

## Présentation

Le titre et l'artiste sont au centre d'une présentation typographique, sans
pochette nécessaire. Les couleurs du thème sont reprises par les contrôles,
les cartes et la signature SVG de la station. L'indicateur animé accompagne
uniquement la lecture ; il est décoratif et ne mesure pas le signal audio.
Les animations respectent la préférence de réduction des mouvements.

`artwork_url` est une option de visuel **par station**, pas une pochette
automatiquement associée au titre en cours. Il accepte une URL HTTP(S) sans
identifiants. Sans visuel ou si l'image échoue, le design garde sa signature
graphique et sa mise en page. Aucun visuel n'est configuré par défaut.

La page affiche six prévisions, cinq rendez-vous et huit passages à l'ouverture.
Les autres rendez-vous s'ouvrent dans un panneau et l'historique peut être
déplié. Les données complètes restent dans l'API. Les états de veille, pause,
reconnexion et erreurs audio sont explicites. Le mode compact conserve le
lecteur et les contrôles, avec un lien vers la page complète.

## Vérité des données

Le plugin consomme exclusivement `OnAirService.Watch`, contrat existant de
`proto/onair_v1.proto`. Un abonnement gRPC partagé par station, pas par visiteur.

* `air.now_playing` : antenne connue, uniquement si Liquidsoap est présent.
* `air.coming_next` : **seulement `prefetched`**, déjà remis à Liquidsoap.
  Le scheduler actuel ne publie qu'un titre engagé. Une file vide reste vide.
* `air.indicative` : `upcoming`, simulation du même moteur sur une copie de la
  base ; affichée séparément, jamais mélangée à la file réelle.
* `air.agenda` : changements de programme projetés sur 24 h (20 maximum), avec
  horaires affichés dans le fuseau de la station. Ces rendez-vous sont indicatifs.
* `air.indicative_agenda` : règles au compteur de titres, sans heure certaine.
* `air.history` : derniers passages du journal de diffusion, 50 maximum.

Les champs texte sont bornés. Les chemins médias, URLs de relais internes,
endpoints gRPC, auteurs d'overrides et diagnostics internes ne sont pas exposés.
Les références de playlists de l'agenda sont publiques : choisir des noms
adaptés aux auditeurs. Aucun appel de sélection, aucun pop, aucune mutation.

`status` vaut `connecting`, `online` ou `offline`. `stale` est vrai si le flux
gRPC est déconnecté ou si la réception/le calcul a plus de 60 s. L'instantané
ancien reste consultable avec cet indicateur, y compris après une déconnexion.
`observed_at`, `received_at`, `started_at`, `estimated_at`, `at` sont en secondes
UTC ; les durées sont en millisecondes. Inconnu = `null`, pas zéro.

SSE envoie un instantané initial puis les changements ; un rafraîchissement
toutes les 15 s actualise aussi l'indicateur d'ancienneté. EventSource reconnecte
automatiquement. Chaque reconnexion repart d'un instantané complet ; pas de
journal d'événements ni besoin de `Last-Event-ID`. Le tampon par client est borné
à un message. Quota épuisé = 503, permis libéré au départ du client.
Le gRPC reconnecte avec temporisation exponentielle plafonnée à 15 s.
L'arrêt du plugin ferme les connexions et son runtime après 1 s au maximum.

## Publication

Pour un accès réseau direct, configurer `bind` sur l'IP ou le nom du serveur,
puis accéder à `http://hôte:port/player/:slug`. Le listener sert du HTTP.
Pour publier en HTTPS, utiliser un reverse proxy, avec limitation
du débit et du nombre de connexions HTTP au niveau du proxy. Pour SSE désactiver
la mise en tampon et choisir un timeout supérieur aux keepalives (15 s).
Ne pas publier le port gRPC d'administration. Les pages permettent l'intégration
iframe ; si le proxy ajoute `X-Frame-Options` ou `frame-ancestors 'self'`, retirer
ou adapter cette politique sur les seules routes publiques du player.

## Validation

```sh
cargo test --locked -p station-player
cargo build --locked
node plugins/station-player/tests/web.test.cjs
```

Les tests Rust couvrent configuration, séparation réel/simulation, absence de
chemins internes, routes en lecture seule, ancienneté, SSE/quota et cycle de vie.
Les tests navigateur simulés couvrent rendu texte, iframe, source audio stable
et conflit de l'instantané initial avec une mise à jour SSE.
