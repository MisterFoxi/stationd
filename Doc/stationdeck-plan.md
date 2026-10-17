# StationDeck — plan de réalisation du socle d’administration

Statut : proposition de découpage, avant implémentation.

## Nom proposé

**StationDeck** : le pupitre d’administration de StationD. « Deck » évoque
la console de radio et une surface sur laquelle on assemble des outils.
Nom du binaire proposé : `stationdeck`. Préfixe des crates : `stationdeck-`.
Ce nom est une proposition interne ; sa disponibilité publique n’a pas été vérifiée.

## Principe directeur corrigé : gRPC est découplé de SD

Le module gRPC doit pouvoir être modifié ou remplacé sans toucher au cœur
 de StationD (SD). Le rendre facultatif dans le client graphique ne suffit
pas à satisfaire cette exigence.

Architecture cible :

```text
Modules egui → contrat client indépendant du protocole
                         ↓
                  adaptateur client gRPC
                         ↕
                  module serveur gRPC
                         ↓
              contrat interne de SD
                         ↓
                    cœur de SD
```

Le contrat interne de SD expose commandes, lectures, événements, erreurs
et contexte d’autorisation avec des types indépendants de tonic/protobuf.
SD implémente ce contrat ; le module serveur traduit gRPC vers ce contrat.
Les règles métier restent dans SD. L’assemblage branche le module choisi
sans ajouter de connaissance du protocole dans le cœur.

Changer la bibliothèque, la sérialisation ou le transport concerne les
adaptateurs. Ajouter une fonctionnalité métier peut nécessiter de faire
évoluer le contrat et SD. La modularité ne signifie pas nécessairement
chargement dynamique : le mode de livraison reste à choisir.

**Complément à la tranche 0 :** cartographier les dépendances actuelles,
définir le contrat interne de SD et distinguer cœur, adaptateur serveur,
adaptateur client et assemblage. Dans le dépôt actuel, `src/lib.rs` expose
les services `*_grpc` et `src/main.rs` assemble tonic : cette séparation
est à réaliser, elle ne découle pas de la seule existence de `stationd-proto`.

**Composants supplémentaires proposés :** `stationd-admin-contract` pour
le contrat interne sans protocole et `stationd-grpc` pour le module serveur.
`stationdeck-grpc` désigne seulement l’adaptateur client de l’interface.

Cette règle remplace les formulations ci-dessous qui réduisaient l’objectif
à un module gRPC facultatif côté GUI. Elle fait partie du jalon B et de la
recette : vérifier que le cœur SD compile sans tonic/protobuf, puis vérifier
la compatibilité des clients gRPC existants.

## Objectif et règles d’architecture

Construire une application d’administration en Rust avec egui, composée de
modules. Le socle accueille les modules et leurs contributions ; les modules
apportent les fonctionnalités, y compris la communication gRPC.

- Le socle gère le registre des modules, leur cycle de vie, les dépendances,
  les services et les points d’intégration de l’interface.
- L’hôte graphique utilise egui ; eframe est proposé pour la fenêtre native.
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

Les modules natifs V1 restent de confiance : les contrôles des services
n’offrent pas un sandbox au code exécuté dans le même processus. Le
confinement de code non fiable relève de la tranche 9.

## Tranche 0 — fixer le contrat et les frontières

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

## Tranche 1 — créer le squelette egui

**But :** disposer d’une application native qui démarre sans serveur.

Actions :
- Ajouter les crates au workspace sans modifier ses membres par défaut.
- Installer la fenêtre egui/eframe et la structure commune : navigation,
  zone principale, barre d’état, accueil et paramètres locaux.
- Prévoir les états vide, chargement, erreur et indisponibilité.
- Définir thème, raccourcis et identifiants persistants des widgets.
- Préparer un point d’entrée pour les contributions des modules.

- Prévoir les états utilisateur non connecté et accès refusé dans la GUI.

Livrable : exécutable StationDeck avec interface vide utilisable.

**Critère de fin :** l’application se lance sans gRPC, se redimensionne,
se ferme proprement et affiche un accueil cohérent sans module métier.

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

## Tranche 4 — découpler gRPC de SD et raccorder l’interface

**But :** modifier ou remplacer le module gRPC sans toucher au cœur de SD.

Actions :
- Extraire un contrat interne : commandes, lectures, événements, erreurs
  et contexte d’autorisation, sans tonic ni protobuf.
- Faire implémenter ce contrat par les services de SD.
- Déplacer handlers, conversions protobuf et hébergement du serveur dans
  le module gRPC ; séparer son assemblage de la logique du cœur.
- Préserver le comportement des services utilisés par les clients existants.
- Fournir un adaptateur en mémoire sur le même contrat pour vérifier
  l’indépendance de SD par rapport au transport.
- Côté egui, réutiliser `stationd-proto` dans l’adaptateur client uniquement
  et publier aux modules des services indépendants du protocole.
- Gérer paramètres de connexion, TLS/authentification selon le serveur,
  délais, reconnexion temporisée et annulation des tâches.
- Identifier les sessions pour ignorer les réponses anciennes ; ne pas
  rejouer une mutation dont le résultat est incertain après une coupure.

- Introduire un contexte d’identité vérifié dans le contrat interne ;
  appliquer dans SD le contrôle périmètre du module et permissions utilisateur,
  indépendamment de gRPC. Tester le refus via les deux adaptateurs.

Livrable : contrat interne SD, module serveur gRPC séparé, adaptateur
client StationDeck, panneau de connexion et adaptateur de test en mémoire.

**Critère de fin :** le même cœur SD fonctionne derrière gRPC et derrière
l’adaptateur de test sans changement de son code. Le cœur ne dépend plus
de tonic/protobuf. Une modification limitée au protocole reste dans le
module gRPC. Les clients actuels continuent à fonctionner. Une perte du
serveur laisse egui utilisable et la reconnexion ne mélange pas les sessions.

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
  compléter par une recette visuelle egui et une intégration sur serveur de développement.

- Vérifier qu’une capacité déclarée mais non accordée reste inutilisable.
- Vérifier qu’un rôle déclaré ne crée aucune attribution automatique.
- Vérifier qu’un rôle de lecture ne peut pas écrire, même par appel direct
  sans egui ; vérifier aussi qu’un rôle puissant ne dépasse pas le périmètre du module.
- Vérifier le refus d’accès à la base d’un autre plugin et aux ressources hors périmètre.
- Vérifier identité manquante/invalide, révocation et arrêt des abonnements interdits.
- Vérifier les mêmes autorisations avec gRPC et l’adaptateur en mémoire.
Chaque tranche se termine par une démonstration de son critère de fin et une
mise à jour du document avec les décisions effectivement retenues.


