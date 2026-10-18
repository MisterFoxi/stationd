# StationDeck — plan de réalisation du socle d’administration

## Décision : configuration centrale et plugins

StationDeck assure l'authoring pour les démons et leurs plugins ; un backend
permanent conserve et distribue la configuration publiée par gRPC. Découverte
automatique séparée, FQDN:port explicite prioritaire, cache valide côté cible et
retour de version appliquée. Nom cible du service Icecast V1 : **icecastd**.
Voir [le contrat de configuration](stationdeck-configuration.md) pour les frontières,
les plugins et la recette. Première verticale livrée : backend persistant,
authoring de champs de plugins, pull/report gRPC, broadcast IPv4 ou endpoint
explicite, cache autonome et historique. Recette avec consommateur de démonstration ;
premiers adaptateurs réels câblés le 18 octobre, recette à effectuer :
[SD listener-stats et icecastd](stationdeck-daemon-adapters.md). Guide :
[configuration en containers](stationdeck-config-dev.md).

Travaux ajoutés : tranche 2, services typés et contrats de schémas ; tranche 4,
backend permanent, identités, bootstrap, découverte et adaptateurs de récupération ;
tranche 6, authoring des plugins selon leurs capacités d'application ; tranche 7,
persistance centrale, publication, conflits et diagnostic souhaité/appliqué.
Le renommage du service/DNS Docker V1 vers icecastd est réalisé ; commandes
make icecast-* conservées. Préserver SD 0.5 via un adaptateur.

Statut : socle web (0–1), module de test et première lecture gRPC livrés le 17 octobre 2026. Tranches 2 à 4 amorcées, non intégralement réalisées.

Contrat retenu : [StationDeck V1](stationdeck-contract.md), avec manifestes
[démo](stationdeck/demo.module.toml) et [gRPC](stationdeck/grpc.module.toml).
Les règles du contrat précisent les propositions ci-dessous. Le squelette
graphique est livré ; le daemon reste inchangé et le registre reste à implémenter.

## Réalisation vérifiée le 17 octobre 2026

Messages/diagnostics en anglais. Module de test autonome livré avant le module
gRPC : catalogue compilé, deux vues de test et tâche annulable, puis lecture
de Station.Status par passerelle authentifiée. DTO et rendu du statut partagés.
La passerelle ne propose aucune mutation et StationD 0.5 reste inchangé.
Validation : 12 tests Rust, formatage/Clippy natif et WASM, recette Chromium
avec véritable serveur gRPC de test, refus d'accès, panne amont, annulation et
absence de persistance des jetons. Le résolveur de dépendances, le registre de
services typés et la politique complète des manifestes restent à implémenter.
Guide et limites : [StationDeck web](stationdeck-dev.md).
## Cible retenue : navigateur web

StationDeck est une WebUI egui/eframe compilée en WebAssembly et servie par
un container HTTP. La cible X11 initiale est remplacée, avec recette Chromium
en container. Modules Rust compilés dans le WASM ; préférences navigateur.
Les appels futurs passent par une passerelle web vers le gRPC existant, sans
client tonic natif côté navigateur ni refonte obligatoire de StationD 0.5.
Cette décision prévaut sur les mentions historiques de fenêtre native.
Guide : [StationDeck web](stationdeck-dev.md).

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

## Nom proposé

**StationDeck** : le pupitre d’administration de StationD. « Deck » évoque
la console de radio et une surface sur laquelle on assemble des outils.
Nom du binaire proposé : `stationdeck`. Préfixe des crates : `stationdeck-`.
Ce nom est une proposition interne ; sa disponibilité publique n’a pas été vérifiée.

## Raccordement retenu : préserver StationD 0.5

La WebUI utilise des services client indépendants du protocole. Un adaptateur
web parle à une passerelle serveur, qui appelle le gRPC existant de StationD.

```text
Modules egui/WASM → contrat client → adaptateur HTTP navigateur
                                            ↕
                                   passerelle serveur
                                            ↕ gRPC existant
                                       StationD 0.5
```

Les règles métier restent dans SD. Le navigateur ne reçoit ni tonic natif,
ni secrets de station, ni accès SQLite. Changer le transport web concerne
les adaptateurs. Le premier module gRPC est limité à Station.Status ; les
commandes de contrôle et les comptes utilisateurs restent à implémenter.
La séparation interne de SD en stationd-admin-contract/stationd-grpc reste
une option à justifier par un besoin concret, pas un préalable à StationDeck.
On conserve la compatibilité des clients et la référence 0.5 telle quelle.

## Ordre demandé pour les premiers modules

1. Messages et diagnostics au minimum en anglais ; finaliser la cible web.
2. Module de test autonome : données simulées, deux vues et tâche annulable.
3. Module gRPC : lecture du statut par passerelle authentifiée, sans mutation.

Le catalogue compilé et son activation sont un premier socle. Ils ne valent
pas réalisation de toute la tranche 2 (résolution des dépendances, services
typés, validation complète des manifestes et politique de droits).

## Objectif et règles d’architecture

Construire une application d’administration en Rust avec egui, composée de
modules. Le socle accueille les modules et leurs contributions ; les modules
apportent les fonctionnalités, y compris la communication gRPC.

- Le socle gère le registre des modules, leur cycle de vie, les dépendances,
  les services et les points d’intégration de l’interface.
- Le pupitre utilise egui/eframe compilé en WebAssembly pour le navigateur.
- gRPC est un module facultatif. Le socle ne dépend ni de tonic, ni des types
  protobuf, ni des services métier de StationD.
- Les modules fonctionnels déclarent les services dont ils ont besoin. Le
  registre fournit ces services sans référence globale au module gRPC.
- Les modules contribuent des vues, actions, menus et indicateurs ; l’hôte
  maîtrise la disposition globale et le traitement des entrées.
- Les appels réseau et les traitements longs s’exécutent hors du rendu egui.
- Le daemon reste responsable des règles métier, des autorisations et des
  écritures de son état. L’interface n’accède pas directement à sa SQLite.

### Deux familles de plugins à distinguer

Les **modules StationDeck** étendent l’application d’administration.
Les **plugins StationD** s’exécutent dans le daemon et peuvent déjà déclarer
leurs vues, exposées par son API. Un module StationDeck pourra afficher ces
vues sans transformer un plugin du daemon en code egui exécuté localement.

Références existantes : [vues de plugins](plugin-ui.md),
[surface hôte du daemon](plugin-host.md), [administration](admin.md).
Réutiliser `crates/stationd-proto` pour le contrat gRPC client et examiner
`crates/stationd-client-config` pour les paramètres de connexion partagés.

### Hypothèse de livraison V1

Commencer avec des modules Rust de confiance, sélectionnés et liés à la
compilation, puis activés par configuration. Cela permet de valider le
contrat et la composition sans imposer d’emblée une ABI dynamique.
Le chargement de fichiers externes, WASM ou bibliothèques natives, fait
l’objet d’une tranche ultérieure. Une bibliothèque Rust ne constitue pas
à elle seule une ABI stable ni une isolation des plantages.

## Découpage des composants proposé

| Composant | Responsabilité |
| --- | --- |
| `stationdeck-core` | Registre, résolution des dépendances, cycle de vie ; sans egui ni transport. |
| `stationdeck-ui` | Contrat des contributions egui et composants visuels communs. |
| `stationdeck` | Exécutable, fenêtre et assemblage des modules disponibles. |
| `stationdeck-grpc` | Connexion StationD, clients gRPC et publication des services d’administration. |
| `stationdeck-demo` | Module de démonstration et services simulés, sans réseau. |
| `stationdeck-overview` | Première vue métier de suivi de la station. |
| `stationdeck-plugins` | Catalogue des plugins du daemon et rendu de leurs vues déclarées. |

Les noms et frontières précis sont à confirmer à la tranche 0. Un module
peut fournir un service sans fournir de vue, ou fournir une vue sans réseau.
Les contrats partagés décrivent les services ; les implémentations restent
dans les modules fournisseurs. La configuration de l’exécutable compose
l’ensemble, sans faire dépendre le socle des modules concrets.

## Périmètre des modules, permissions et rôles utilisateurs

Chaque module/plugin déclare dans son manifeste son périmètre technique,
ses permissions fonctionnelles et les rôles utilisateurs autorisés à les
exercer. Cette règle concerne aussi le module gRPC.

### Déclarations obligatoires

| Déclaration | Contenu attendu |
| --- | --- |
| Périmètre technique | Services, ressources et capacités requis ; accès DB avec base/espace concerné, opérations et limites ; accès réseau/fichiers si nécessaire. |
| Permissions fonctionnelles | Identifiants qualifiés par module, description, actions et ressources couvertes : consulter, modifier, administrer, etc. |
| Rôles utilisateurs | Rôles reconnus ou proposés par le module et correspondance explicite entre rôles et permissions. |
| Contributions UI | Permissions requises par vue, action, menu et indicateur. |

Déclarer un accès ne l’accorde pas. Le socle valide le manifeste puis
accorde uniquement le périmètre autorisé par la politique de déploiement.
Déclarer un rôle ne l’attribue pas à un utilisateur : les affectations sont
gérées par l’administration des utilisateurs. Un module ne peut ni
s’attribuer des capacités ni promouvoir un utilisateur.

Exemple de manifeste indicatif, à formaliser dans la tranche 0 :

```toml
[module]
id = "audience"

[[capabilities]]
kind = "db"
resource = "plugin:audience"
operations = ["read"]

[[permissions]]
id = "audience.consulter"
resource = "audience.stats"
actions = ["read"]

[[permissions]]
id = "audience.configurer"
resource = "audience.config"
actions = ["read", "update"]

[[roles]]
id = "audience.observateur"
permissions = ["audience.consulter"]

[[roles]]
id = "audience.gestionnaire"
permissions = ["audience.consulter", "audience.configurer"]
```

L’accès DB ci-dessus autorise uniquement la lecture de la base du plugin.
La permission de configuration passe par un service distinct autorisé ;
elle ne donne pas implicitement un accès d’écriture DB. Un module egui
accède aux données distantes par les services de SD, jamais directement
à sa SQLite. Les rôles proposés ici sont des exemples, pas des rôles
existants du projet.

### Contrôle à l’exécution

Une opération est autorisée seulement si elle est dans le périmètre déclaré
et accordé au module **et** si l’utilisateur possède la permission requise
sur la ressource visée. Les accès manquants ou inconnus sont refusés par
défaut. Les tâches internes sans utilisateur passent par une identité de
service explicite avec leur propre périmètre, sans contournement implicite.

- Le registre remet des services/capacités limités au module ; aucun accès
  DB global n’est accordé par simple déclaration.
- Le contexte d’appel associe identité utilisateur vérifiée, identité du
  module et ressource. Une identité de module fournie par le client doit
  être validée selon le modèle de confiance retenu, jamais crue aveuglément.
- SD contrôle les droits avant l’opération et limite les données retournées.
  Masquer ou désactiver une action dans egui améliore l’interface mais
  ne remplace pas ce contrôle, applicable également hors de la GUI.
- Le transport transmet le contexte d’identité vérifié ; la politique
  d’autorisation reste indépendante de gRPC et vaut pour tout adaptateur.
- Une révocation s’applique aux appels suivants et aux abonnements actifs ;
  les données devenues interdites sont retirées des vues et caches locaux.
- Les refus et changements de droits sont diagnostiqués sans exposer de secrets.

Les modules compilés V1 restent de confiance : les contrôles des services
n’offrent pas un sandbox au code exécuté dans le même processus. Le
confinement de code non fiable relève de la tranche 9.

## Tranche 0 — fixer le contrat et les frontières

**Réalisée le 17 octobre 2026.** Cartographie du daemon historique et du
serveur Icecast V1, contrats client/interne séparés, manifeste commun,
droits, cycle de vie et cible navigateur en container fixés dans le
[contrat V1](stationdeck-contract.md). Les tranches suivantes portent le code.

**But :** transformer l’intention en décisions explicites avant le code.

Actions :
- Définir ce qui appartient au socle, à l’hôte egui et aux modules.
- Décrire un manifeste : identifiant stable, version, version du contrat,
  services fournis/requis et dépendances obligatoires ou facultatives.
- Définir les contributions UI : identifiant de vue qualifié par module,
  navigation, actions et indicateurs ; ordre déterministe.
- Définir les états des modules et le comportement quand un service manque.
- Confirmer les plateformes visées et la stratégie V1 de chargement.
- Documenter la différence entre activer un module, établir une connexion
  et disposer des droits nécessaires à une action.

- Formaliser les capacités, permissions qualifiées et rôles du manifeste ;
  définir leur validation, les attributions utilisateurs et le refus par défaut.

Livrable : document de contrat et exemple de manifeste.

**Critère de fin :** un module de démonstration et le module gRPC peuvent
être décrits avec le même mécanisme, sans exception dans le socle.

## Tranche 1 — créer le squelette egui web

**Cible corrigée le 17 octobre 2026 : WebUI, remplaçant le lancement X11.**

**But :** disposer du pupitre dans un navigateur sans serveur métier.

Actions :
- Ajouter les crates au workspace sans changer ses membres par défaut.
- Compiler egui/eframe en WASM et servir HTML/JS/CSS/WASM dans un container.
- Installer navigation, contenu, barre de statut, accueil et préférences.
- Préparer vide, chargement, erreur, indisponible, non connecté et refus.
- Définir thèmes, raccourcis compatibles navigateur et identifiants stables.
- Préparer les contributions des modules sans encore implémenter le registre.
- Persister les préférences versionnées dans le navigateur, sans secrets.
- Tester dans un vrai navigateur en container : MIME WASM, échecs de
  chargement, rechargement, redimensionnement et persistance.

Livrable : WebUI StationDeck et container HTTP indépendant de StationD.

**Critère de fin :** une URL ouvre un accueil cohérent sans gRPC ni module
métier ; la page se redimensionne et conserve les préférences après rechargement.
Guide : [StationDeck web en container](stationdeck-dev.md).

## Tranche 2 — implémenter le registre et le cycle de vie

**But :** faire vivre les modules avec un ordre et des erreurs prévisibles.

Actions :
- Définir le contrat du module : déclaration, initialisation, démarrage,
  arrêt et libération de ses ressources.
- Résoudre les dépendances avant démarrage ; détecter identifiants dupliqués,
  cycles, versions incompatibles et fournisseurs ambigus.
- Introduire un registre de services typés et des règles de disponibilité.
- Initialiser les fournisseurs avant leurs consommateurs et arrêter les
  consommateurs avant les fournisseurs.
- Définir le retrait des contributions et services après arrêt ou échec.
- Afficher le diagnostic des modules bloqués et de leurs dépendants.

- Valider les déclarations de périmètre et de rôles au chargement ; refuser
  références inconnues et permissions en conflit. Injecter uniquement les
  capacités accordées et retirer les accès à l’arrêt du module.

Livrable : gestionnaire de modules et diagnostics locaux.

**Critère de fin :** les tests du registre couvrent cycles, dépendance
absente, doublons et échec d’initialisation ; aucun consommateur ne démarre
avec une dépendance obligatoire indisponible.

## Tranche 3 — ouvrir les points d’extension egui

**But :** ajouter une fonctionnalité sans modifier le socle.

Actions :
- Raccorder navigation, vues, actions, menus et barre d’état au registre.
- Passer à chaque module son contexte UI et les services autorisés.
- Garder le rendu sur le thread graphique et les états propres à chaque vue.
- Définir le passage des résultats asynchrones vers les vues : files bornées,
  notifications de rafraîchissement et annulation des tâches.
- Retirer une vue devenue indisponible sans laisser une sélection invalide.
- Créer un module de démonstration avec deux vues et une action asynchrone
  simulée, sans dépendance au réseau.

- Associer les contributions UI à leurs permissions et vérifier les actions
  locales à l’exécution, y compris celles déclenchées par raccourci.

Livrable : contrat UI et module de démonstration complet.

**Critère de fin :** activer ou désactiver la démonstration change l’interface
par configuration ; ses tâches ne bloquent pas le rendu.

## Tranche 4 — raccorder la WebUI au gRPC existant

**But :** connecter le navigateur à StationD 0.5 réutilisé tel quel.

Actions :
- Définir les contrats client indépendants du transport et leurs DTO web.
- Choisir HTTP/WebSocket ou gRPC-Web et une passerelle serveur vers le gRPC
  existant ; tonic/protobuf hors du socle et des vues navigateur.
- Réutiliser stationd-proto et client-config côté serveur lorsque compatibles.
- Garder les règles métier dans SD ; traduire commandes et erreurs.
- Gérer sessions/authentification, droits, TLS, limites, délais, reconnexion
  temporisée et annulation sans bloquer le rendu.
- Ignorer les réponses des anciennes sessions ; aucun rejeu de mutation
  dont le résultat est inconnu après coupure.
- Fournir des services simulés du même contrat pour tester sans serveur.
- Préserver les clients CLI/TUI existants. Toute modification de SD répond
  à un besoin concret ; extraction interne conditionnelle, distincte du
  raccordement web et non nécessaire pour commencer.

Livrable : passerelle serveur, adaptateur web, panneau de connexion et recette
avec fournisseur simulé puis serveur réel. Secrets côté serveur.

**Critère de fin :** même vue avec simulation et serveur réel, sans tonic ou
protobuf dans le rendu ; coupure sans gel, reconnexion sans mélange de sessions.
Autorisations contrôlées côté serveur : un jeton partagé ne prouve pas une
identité utilisateur vérifiée.

## Tranche 5 — valider une première tranche métier complète

**But :** prouver la composition avec une fonctionnalité réellement utile.

Actions :
- Créer un module de vue d’ensemble de la station : état de diffusion,
  titre à l’antenne et informations de santé déjà disponibles dans l’API.
- Déclarer sa dépendance au service de suivi publié par gRPC.
- Prévoir les états sans données, données à jour, données anciennes et erreur.
- Ajouter une seule action existante, par exemple pause/reprise, avec état
  en cours, résultat explicite et prévention des clics simultanés.
- Fournir un service simulé du même contrat pour vérifier la vue sans serveur.

- Déclarer le périmètre, les permissions et les rôles de la vue métier ;
  vérifier lecture autorisée et commande refusée avec un rôle observateur.

Livrable : vue de supervision fonctionnelle sur une station de développement.

**Critère de fin :** la même vue fonctionne avec le fournisseur simulé et
le fournisseur gRPC ; ni le socle ni le rendu métier ne manipulent tonic.
Vérifier lecture, commande, refus serveur, déconnexion et retour du serveur.

## Tranche 6 — intégrer les vues des plugins du daemon

**But :** exploiter les extensions StationD déjà exposées par l’API.

Actions :
- Ajouter le catalogue des plugins distants : état, capacités et erreurs.
- Mapper les descripteurs existants vers les vues et filtres egui : tableaux,
  configuration et autres types réellement pris en charge par le contrat.
- Qualifier les identifiants par station/session, plugin et vue.
- Respecter les limites du serveur et afficher les données tronquées,
  indisponibles ou anciennes de façon explicite.
- Gérer l’apparition, le retrait et le rechargement des vues distantes.
- Exposer les commandes de cycle de vie existantes et confirmer les actions
  destructrices lorsqu’elles sont ajoutées.

- Appliquer les permissions aux vues et actions des plugins distants ;
  limiter les données côté SD et afficher les refus sans révéler leur contenu.

Livrable : module générique de gestion des plugins StationD.

**Critère de fin :** une vue déclarée par un plugin du daemon apparaît sans
modification de StationDeck ; son retrait ou son échec ne laisse pas la
navigation dans un état invalide.

## Tranche 7 — stabiliser configuration, diagnostics et fermeture

**But :** rendre le socle utilisable au quotidien et explicable en cas d’erreur.

Actions :
- Versionner la configuration locale et définir ses migrations.
- Persister modules activés, préférences de fenêtre et paramètres autorisés.
- Définir le stockage des secrets et les exclure des journaux et diagnostics.
- Ajouter un écran local de diagnostic : modules, dépendances, services,
  versions, état de connexion et erreurs récentes.
- Borner les files, caches et journaux ; annuler les tâches à la fermeture.
- Documenter la limite d’isolation des modules natifs : une erreur retournée
  est gérable, un crash du processus n’est pas confiné par le registre.

- Persister les attributions de rôles côté autorité compétente ; gérer
  révocation, invalidation des caches et diagnostic des refus d’accès.

Livrable : configuration stable, diagnostic et procédure de support.

**Critère de fin :** redémarrage avec préférences conservées, configuration
invalide expliquée, aucune fuite de secrets dans les diagnostics et aucune
tâche du module conservée après son arrêt.

## Tranche 8 — livrer le SDK et la première version du socle

**But :** permettre de développer et distribuer les prochains modules.

Actions :
- Publier dans le dépôt un guide de création de module et un exemple minimal.
- Documenter contrats, dépendances, contributions UI et versionnement.
- Ajouter les commandes de build et les contrôles CI adaptés aux plateformes
  retenues, sans imposer le build graphique aux commandes du daemon.
- Préparer le paquet applicatif et sa liste de modules inclus.
- Exécuter la recette d’ensemble sur un environnement de développement.

- Documenter dans le SDK les déclarations de périmètre, les rôles et le
  contrôle des permissions ; fournir un exemple complet de module restreint.

Livrable : première version StationDeck et SDK interne documenté.

**Critère de fin :** créer un nouveau module à partir de l’exemple nécessite
seulement son ajout à l’assemblage et à la configuration ; aucun changement
n’est nécessaire dans le registre ou la disposition commune.

## Tranche 9 — décider du chargement externe des modules

**But :** étendre la distribution une fois le contrat V1 éprouvé.

Actions :
- Préciser le besoin : modules indépendants du binaire, mises à jour séparées,
  code non fiable ou véritable rechargement à chaud.
- Comparer WASM avec UI déclarative, processus séparé et ABI native stable.
- Évaluer les échanges de services, la compatibilité egui, les performances
  et l’isolation attendue ; ne pas exporter directement des traits Rust dans
  une bibliothèque dynamique distribuée indépendamment.
- Prototyper un module externe et définir validation, permissions,
  compatibilité, arrêt et retour à une version précédente.

- Vérifier que le mode de chargement externe choisi permet d’appliquer
  réellement les capacités accordées, y compris les restrictions DB.

Livrable : décision d’architecture et preuve de faisabilité.

**Critère de fin :** un module externe fournit une fonctionnalité via un
contrat versionné, avec des limites d’isolation et de compatibilité vérifiées.
Cette tranche ne conditionne pas la livraison V1.

## Ordre et jalons

Ordre : **0 → 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8**, puis **9** selon le besoin.
Les règles de diagnostics, d’annulation et de configuration sont introduites
dès les premières tranches ; la tranche 7 les consolide.

| Jalon | Tranches | Démonstration attendue |
| --- | --- | --- |
| A — socle modulaire | 0 à 3 | Interface egui et module de démonstration, sans réseau. |
| B — administration connectée | 4 et 5 | gRPC facultatif et première vue métier avec fournisseur simulé. |
| C — plugins distants | 6 | Découverte et rendu générique des vues StationD. |
| D — V1 livrable | 7 et 8 | Configuration, diagnostics, paquet et exemple de nouveau module. |
| E — extensions externes | 9 | Stratégie et prototype de chargement indépendant. |

Le backbone est validé au jalon B : accueil des modules, contributions egui,
services injectés et transport gRPC réellement interchangeable. Les jalons
C et D rendent ce socle immédiatement exploitable dans StationD.

## Recette commune

- Démarrer sans module, avec démonstration seule, puis avec gRPC et une vue métier.
- Vérifier dépendances absentes, cycles, identifiants dupliqués et version incompatible.
- Vérifier serveur absent, réponse lente, coupure pendant une action et reconnexion.
- Vérifier retrait d’une vue active et arrêt d’un fournisseur avec consommateurs.
- Vérifier réactivité egui pendant un abonnement et pendant une commande réseau.
- Compiler le daemon avec ses commandes habituelles pour vérifier son indépendance.
- Vérifier les contrats du registre et les adaptations gRPC par des tests ciblés ;
  compléter par une recette visuelle navigateur et une intégration serveur.

- Vérifier qu’une capacité déclarée mais non accordée reste inutilisable.
- Vérifier qu’un rôle déclaré ne crée aucune attribution automatique.
- Vérifier qu’un rôle de lecture ne peut pas écrire, même par appel direct
  sans egui ; vérifier aussi qu’un rôle puissant ne dépasse pas le périmètre du module.
- Vérifier le refus d’accès à la base d’un autre plugin et aux ressources hors périmètre.
- Vérifier identité manquante/invalide, révocation et arrêt des abonnements interdits.
- Vérifier les mêmes autorisations avec gRPC et l’adaptateur en mémoire.
Chaque tranche se termine par une démonstration de son critère de fin et une
mise à jour du document avec les décisions effectivement retenues.


