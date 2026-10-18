# StationDeck — contrat V1, tranche 0

## Décision complémentaire : authoring et distribution de configuration

[Autorité de configuration, découverte et plugins](stationdeck-configuration.md) :
StationDeck assure l'authoring ; son backend permanent distribue les versions
publiées aux démons/adaptateurs en gRPC. Endpoint FQDN:port explicite prioritaire,
sinon découverte séparée du transport gRPC. Dernière configuration valide locale,
versions souhaitée/appliquée distinctes. Tous les plugins suivent le même contrat
avec schéma, permissions et adaptateur ; aucune compatibilité automatique supposée.
Le service/DNS Icecast V1 est nommé **icecastd** dans le Compose livré.
Le backend persistant, l'authoring par schéma, le pull/report gRPC et la découverte
IPv4 sont maintenant livrés avec un consommateur de démonstration et un hook
local d'application. Les adaptateurs réels SD/icecastd, le résolveur complet et
les comptes utilisateurs ne sont pas implicitement réalisés par cette verticale.
Guide et recette : [configuration](stationdeck-config-dev.md).

Décisions du 17 octobre 2026. Contrat documentaire `1.0.0`, à implémenter
avec les tranches suivantes du [plan](stationdeck-plan.md). Les manifestes
sont chargés à exécution par le registre ajouté le 18 octobre 2026. Ce code et ses nouveaux tests restent à valider ; voir le guide web.

## Principe de réutilisation de StationD (17 octobre 2026)

La 0.5 qui tourne est la référence ; StationD doit normalement être
réutilisable tel quel dans la V1. StationDeck se raccorde en premier lieu
aux interfaces existantes. La réécriture du daemon ou une extraction de ses
composants ne constitue pas un prérequis au démarrage de StationDeck.
Les séparations serveur décrites plus bas sont une cible architecturale :
leur nécessité et leur périmètre doivent être établis à partir des besoins
concrets, en préservant le comportement de la 0.5. La recette V1 distingue
la réutilisation de StationD de la validation de son nouvel assemblage.
Cette précision prévaut sur toute lecture imposant une refonte préalable.

## État du contrat et implémentation courante

Ce document distingue la cible V1 des premiers composants livrés. Le socle
web, le catalogue de modules compilés, le module de test puis le module gRPC
sont implémentés. Leur première verticale partagée est la lecture de statut,
pas encore la supervision complète de diffusion ni ses commandes.
La passerelle appelle uniquement le RPC Station.Status de SD 0.5 inchangé.
Le jeton de passerelle accorde cette seule lecture ; il n'est pas une identité
utilisateur SD ni une attribution de rôle administrateur. Pas de jeton stocké
dans le navigateur. Le rôle de lecture seule du module de test reste simulé.
Les contrats internes SD décrits plus bas sont une cible conditionnelle, pas
une extraction déjà réalisée ou un préalable à la WebUI. Manifestes TOML, dépendances et services typés locaux sont codés, sans exécution des nouveaux tests. Les attributions de rôles distants et abonnements complets restent à implémenter. Le catalogue ne prétend pas offrir un sandbox aux
modules de confiance compilés dans le même WASM.
Messages et diagnostics : anglais au minimum. Guide : [modules web](stationdeck-dev.md).
## Cible web retenue (17 octobre 2026)

StationDeck est une WebUI egui/eframe compilée en WebAssembly, exécutée dans
le navigateur et servie par un container HTTP. Cette décision remplace la
première interprétation native X11 de la tranche 1. Aucun socket X11,
forwarding graphique Windows ou Xvfb dans la cible et sa recette.
WebAssembly, JavaScript et WebGL requis. Chromium est la cible initiale de
recette ; les autres navigateurs restent à qualifier. Compilation, génération
JS/WASM et tests en containers. `V:` = `/data/dev` sur `stationdev1.lan`,
branche `v1-dev`.

Modules Rust de confiance compilés ensemble dans le même WASM puis activés
par configuration. Aucun chargement externe V1. Les plugins StationD restent
côté daemon ; leurs vues distantes ne deviennent pas du code Rust chargé
dans le navigateur. Préférences locales dans le localStorage, sans secrets ;
taille du canvas liée à la page. Le registre local ajoute résolution, services typés et manifestes exécutables ; validation à effectuer. Ses hooks synchrones ne prennent pas en charge le nettoyage asynchrone avec délai global de cinq secondes. Guide : [StationDeck web](stationdeck-dev.md).

## Frontière navigateur / serveur

Le navigateur ne lance pas un client tonic natif. Le raccordement passe par
un adaptateur web et une passerelle serveur vers le gRPC existant de SD.
HTTP/WebSocket ou gRPC-Web à choisir en tranche 4. Secrets côté serveur,
données limitées à la session autorisée, aucun accès à SQLite de SD.
StationD 0.5 réutilisable tel quel ; son extraction interne reste une cible
conditionnelle à justifier, pas un préalable au raccordement de la WebUI.

## Carte des dépendances actuelles

| Sources existantes | Constat et travail à réaliser |
| --- | --- |
| `Cargo.toml`, `build.rs`, `src/proto.rs` | Le paquet stationd dépend de tonic/prost et génère les RPC : le cœur actuel reste couplé au protocole. |
| `src/lib.rs`, `src/grpc.rs`, `src/*_grpc.rs` | Handlers exportés avec les services métier : extraire handlers et conversions. |
| `src/main.rs` | Assemble tonic et les douze services : isoler composition, hébergement réseau et initialisation métier. |
| `src/library_actor.rs`, `src/media_preview.rs` | Requête protobuf dans l'acteur et types tonic dans la préécoute : déplacer aussi ces frontières internes. |
| `src/live.rs`, `src/plugin.rs`, `src/plugin/config_host.rs` | DTO protobuf pour état DJ et configuration : introduire des types métier indépendants. |
| `src/bin/stationctl.rs` | Importe stationd/proto et utilise des opérations locales : séparer client réseau et commandes locales. |
| `crates/stationd-proto` | Contrat wire partagé, feature server disponible ; réservé aux adaptateurs. |
| `crates/stationd-tui` | Client tonic/proto sans dépendance au daemon : préserver sa compatibilité. |
| `crates/stationd-client-config` | Configuration sans daemon ni tonic : réutiliser la normalisation d'adresse côté adaptateur. |
| `crates/stationd-admin` | Workspace Icecast V1 autonome ; lib.rs mélange Admin, RPC, Status, Bearer et accès Icecast ; bin/ assemble les processus. Prévoir une séparation équivalente sans le rattacher au vieux daemon. |

Carte issue des sources, pas d'une compilation du découpage cible. À la
tranche 4, auditer les dépendances transitives et distinguer production et
références protobuf dans les tests. Les services Icecast V1 restent autonomes.
Le contrat Icecast V1 n'est pas celui généré par stationd-proto.

## Frontières cibles

| Composant | Responsabilité et dépendances permises |
| --- | --- |
| `stationdeck-core` | Manifestes, graphe, cycle de vie, services typés, politique locale et diagnostics ; sans egui, tonic/protobuf ou métier SD. |
| `stationdeck-ui` | Contributions egui et contexte de rendu ; dépend du core, sans transport. |
| `stationdeck-services` | Contrats client, DTO, erreurs, sessions et abonnements ; sans egui, tonic/protobuf ni implémentation SD. |
| `stationdeck` | Hôte eframe web, canvas, disposition et assemblage des modules WASM. |
| `stationdeck-demo` | Fournisseur simulé et contributions locales ; sans réseau ni daemon. |
| `stationdeck-grpc` | Adaptateur client vers la passerelle web, sans tonic natif dans le navigateur. |
| `stationdeck-config-ui` | Authoring générique par schéma et client HTTP navigateur ; permissions contrôlées par le backend. |
| `stationdeck-control` | Autorité persistante, API authoring, gRPC de configuration, découverte et agent ; sans egui et sans dépendance au daemon 0.5. |
| `stationdeck-gateway` | Passerelle serveur web vers le gRPC existant ; tonic/proto ici, en tranche 4. |
| `stationdeck-overview`, `stationdeck-plugins` | Rendu métier via services injectés ; sans fournisseur concret, protobuf ou SQL SD. |
| `stationd-admin-contract` | Commandes, lectures, événements, erreurs et contexte vérifié serveur ; sans egui, tonic/protobuf ou SQLx. |
| Cœur SD | Implémentation du contrat, persistance, règles métier et droits ; sans adaptateur ou types wire après tranche 4. |
| `stationd-grpc` | Traduction et hébergement gRPC ; reçoit une implémentation du contrat interne, sans connaître le cœur concret. |
| Assemblage serveur | Configure et branche cœur et adaptateur ; aucune dépendance inverse vers lui. |

Ces crates sont des destinations, pas des crates ajoutées à la tranche 0.
L'assemblage serveur peut devenir un paquet distinct pour que les binaires
ne réintroduisent pas tonic dans la compilation du paquet cœur.
L'extraction Icecast V1 conserve son workspace et son protocole propres.

Contrat client et contrat interne serveur sont distincts : le premier décrit
une session distante et sa fraîcheur, le second une opération autorisée sur
l'état serveur. Aucun n'est généré depuis protobuf. Changer le wire reste
dans les adaptateurs ; ajouter un métier peut faire évoluer contrats et cœur.

## Contrat interne SD

DTO Rust possédés : StationId, ResourceId, OperationId, instant UTC, révision,
état d'antenne et piste optionnelle. Pas de Request/Status tonic, message
prost ou pool SQL. Absence, zéro et donnée inconnue restent distincts.
Première verticale normative, syntaxe indicative :

```text
read_overview(VerifiedCallContext, StationId) -> Future<Result<Overview, AdminError>>
watch_overview(VerifiedCallContext, StationId, optional revision)
    -> Result<bounded Subscription<OverviewEvent>, AdminError>
control_broadcast(VerifiedCallContext, OperationId, StationId, Pause | Resume)
    -> Future<Result<CommandReceipt, AdminError>>
```

Overview porte station, état, piste optionnelle, santé, observation UTC et
révision. CommandReceipt porte l'identifiant d'opération et son résultat connu,
sans garantie d'exécution exactement une fois. L'adaptateur mémoire utilise
ces opérations et le même contrôle des droits que l'adaptateur gRPC.
Futures exécutables hors rendu ; choix précis des traits asynchrones en tranche 4.

AdminError distingue Unauthenticated, PermissionDenied, InvalidArgument,
NotFound, Conflict, Unavailable, DeadlineExceeded, Cancelled, OutcomeUnknown
et Internal : code stable, diagnostic et message expurgé. La conversion wire
appartient à l'adaptateur. Une mutation interrompue après envoi peut avoir
réussi : résultat inconnu, consulter l'état, aucun rejeu automatique.
L'annulation ne promet pas une annulation métier ; chaque mutation documente
son idempotence.

VerifiedCallContext porte principal utilisateur ou service, station/session,
module validé, ressource, requête, échéance et annulation. L'autorité est
construite par une frontière de confiance ; un module/rôle transmis par le
client n'est pas une preuve. Le cœur consulte les droits courants avant
l'opération et pendant les flux. Identité/délégation concrètes à implémenter
à la tranche 4 : les APIs actuelles et le jeton partagé Icecast V1 ne prouvent
pas encore une identité utilisateur avec rôles. Sur la nouvelle surface,
sans cette preuve, refuser ; ne pas inventer d'administrateur implicite.
Les opérations en mémoire suivent la même règle.

Bibliothèque, grille, playlists, plugins/configuration, statistiques, journal,
Liquidsoap, Icecast et DJ/live suivent cette frontière ; leurs DTO exhaustifs
seront fixés avec leur extraction.

## Manifeste fermé, version 1

Exemples : [démo](stationdeck/demo.module.toml) et
[gRPC](stationdeck/grpc.module.toml). Même schéma, aucune exception de socle.
Clé inconnue ou référence invalide : rejet. Les tableaux vides sont explicites.

| Champ | Règle |
| --- | --- |
| manifest_version | Entier 1 ; version du schéma indépendante des versions de modules. |
| module | id stable ASCII `[a-z][a-z0-9-]*`, 1–64 octets ; version SemVer ; contract exigence SemVer (`^1.0.0`) ; entrypoint clé d'un constructeur compilé, jamais chemin de chargement. |
| dependencies | id de module, version requise, optional booléen. |
| provides | id de service stable qualifié par domaine, version exacte SemVer. |
| requires | id de service, exigence version SemVer, optional booléen. |
| capabilities | id qualifié, kind, resource, operations non vides, limites selon type. |
| permissions | id qualifié, description, resource, actions non vides. |
| roles | id qualifié, description, permissions déclarées par ce module. |
| contributions | id qualifié, kind, title, slot, order entier, services et permissions requis ; action pour un menu. |

Capacités, permissions, rôles et contributions : `<module.id>.<nom-local>`,
nom local `[a-z][a-z0-9-]*` de 1–64 octets, unicité dans chaque catégorie.
Services : noms par domaine, ex. stationdeck.admin.overview version 1.0.0,
indépendants du fournisseur. L'assemblage associe chaque service/version à
une interface Rust typée ; une chaîne ou un Any non contrôlé ne suffit pas.
Compatibilité SemVer, changement incompatible = majeur supérieur.

Contributions : view/navigation, action/toolbar, menu/main-menu,
indicator/status. Un menu référence une action du même module. Tri par
(order croissant, module.id ASCII, contribution.id ASCII), par slot ; jamais
par titre traduit ou ordre de chargement. Identifiants stables de navigation
et widgets. L'hôte garde disposition et entrées ; raccourcis globaux réservés,
conflits désactivés avec diagnostic. Bouton et raccourci vérifient les mêmes droits.

Validation : schéma, unicité, références, types, versions, slots, rôles,
permissions et capacités, puis graphe. Plusieurs fournisseurs compatibles
exigent un binding explicite par consommateur dans le déploiement ; sinon
consommateur bloqué. Démo et gRPC peuvent coexister mais un consommateur
choisit un seul fournisseur de chaque service. Dépendance de module et
exigence de service sont distinctes. Dépendance optionnelle absente acceptée ;
présente, elle doit être compatible et acyclique. Aucun cycle accepté.

## Capacités et autorisations

Une capacité borne le module ; une permission borne le principal. Une opération
exige les deux sur la ressource/action. Déclarer demande, la politique de
déploiement accorde un sous-ensemble. Capacité nécessaire refusée : module bloqué.

Types initiaux : service (service connu, read/watch/command), network (profil
connu, connect), filesystem (racine logique, read/write), db (plugin:<id>,
opérations explicites, limites de lignes et de durée). Types/opérations
inconnus refusés ; aucun wildcard V1. URLs, racines et secrets sont résolus
par la politique, pas accordés par une chaîne du manifeste. Aucun module
StationDeck n'accède à SQLite de SD. Les bases propres des plugins serveur
restent régies par leur host.

Les rôles ne sont que des définitions : aucune attribution automatique.
Attributions distantes gérées par l'autorité serveur, droits de démo par une
politique locale explicite. Permission inconnue/étrangère dans un rôle ou
redéfinition incompatible : rejet. Le socle local n'accorde aucun droit serveur.

Les permissions demo.* couvrent la simulation locale ; grpc.* couvre la
connexion locale. Les opérations distantes exigent en plus les permissions
serveur station.overview.read et station.broadcast.control, à introduire avec
le contrat serveur. Les futurs modules métier déclareront leurs propres
contributions et opérations distantes requises. Fournir un service ne donne
pas les droits métier du principal qui l'appelle.

Sans identité vérifiée ou autorisation courante : refus. Une tâche interne
utilise une identité de service explicitement dotée. Révocation : fermer les
flux interdits, purger les données des caches/vues concernés et recontrôler
chaque appel. Journaux sans secrets ni données interdites. Les modules compilés de confiance
restent de confiance : ces contrôles ne constituent pas un sandbox mémoire.

## Cycle de vie et disponibilité

| État | Comportement |
| --- | --- |
| disabled | Non activé, aucune ressource ou contribution active. |
| validated | Manifeste valide, résolution en attente. |
| blocked | Dépendance, binding ou capacité nécessaire absent/incompatible ; pas de démarrage. |
| initializing | Ressources privées en création, pas de service publié. |
| starting | Injection et démarrage ; publication atomique après succès. |
| running | Services et contributions enregistrés, disponibilité propre observable. |
| stopping | Refus des nouveaux appels, retrait des contributions, annulation et attente bornée. |
| stopped | Ressources libérées ; redémarrage explicite avec validation/résolution. |
| failed | Échec init/start/exécution : nettoyage, retrait et diagnostic. |

Fournisseurs avant consommateurs, départage ASCII module.id ; arrêt inverse.
Nettoyage idempotent, délai global de fermeture V1 de 5 secondes. Files bornées,
saturation explicite. Fournisseur en échec : services retirés, consommateurs
obligatoires arrêtés puis blocked ; optionnels notifiés et actifs sans cette
fonctionnalité. Retour du fournisseur : nouvelle résolution explicite, pas de
boucle automatique. Un échec fatal du WASM peut arrêter la WebUI de cet onglet.

Le module gRPC peut rester running pendant une connexion disconnected,
connecting, connected, reconnecting ou error. Ses services restent enregistrés
et retournent Unavailable sans réseau ; les consommateurs restent actifs.
Présence locale du fournisseur et capacité distante sont distinctes.
Activer le module, établir la connexion et être autorisé sont trois états
indépendants. L'accueil fonctionne sans module ni identité distante.

## Rendu et sessions

Rendu egui sur le thread graphique avec services limités ; aucun RPC, SQL ou
attente bloquante dans update. Tâches asynchrones compatibles navigateur hors rendu, files bornées, notification
repaint et annulation. Snapshot : session/génération, révision, fraîcheur ;
ignorer les réponses d'une ancienne session. Changement de station ou session :
annulation des flux et invalidation des données associées.

Vue privée inaccessible sans droit ; service obligatoire absent = vue
indisponible ; retrait d'une vue sélectionnée = retour à l'accueil. États
explicites : vide, chargement, données anciennes, erreur, refus. Un snapshot
ancien n'autorise pas une commande. Il peut rester affiché après une coupure
seulement si session et droits restent valides ; une révocation le purge.

## Recette et suites

Fin de tranche 0 : deux exemples TOML avec même schéma et références cohérentes,
frontières documentées, contrat interne défini, règles de droits, cycle et
cible container fixés. Aucun registre, binaire ou découplage livré à ce stade.

Contrôles à réaliser en containers aux tranches suivantes :

- 1 : navigateur, accueil sans serveur/module, redimensionnement et rechargement.
- 2–3 : démo/gRPC sans branche spéciale ; doublons, cycles, incompatibilités,
  fournisseurs ambigus, capacité refusée, rôle non attribué et retrait de vue.
- 4 : paquet cœur compilable avec graphe sans tonic/prost/protobuf ; même
  autorisation via mémoire et gRPC ; compatibilité CLI/TUI existants ; extraction
  Icecast V1 conservant son autonomie.
- 4–5 : déconnexion sans gel, sessions séparées, mutation incertaine sans rejeu,
  révocation active et observateur incapable de commander par appel direct.
- 8 : packaging HTTP/WASM et recette navigateur ; ne pas modifier les membres par
  défaut ni imposer egui aux builds habituels du daemon.

Les deux exemples publient le même service de suivi pour prouver la substitution
du fournisseur ; leurs vues et droits locaux restent propres à chaque module.

Validation documentaire du 17 octobre 2026 : les deux manifestes sont parsés
avec `tomllib` (Python 3.14.4) dans un container éphémère basé sur
`stationd-v1-icecast-recovery:dev`, avec `--network none --read-only` et
`/data/dev:/repo:ro`. Schéma commun, identifiants, références de services,
permissions/rôles, slots et actions des menus vérifiés ; liens locaux du
contrat, du plan et de la passation vérifiés. `git diff --check` passe.
Les tests du futur registre et le découplage Rust restent aux tranches prévues.
