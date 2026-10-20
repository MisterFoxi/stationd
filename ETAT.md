# État d'avancement — passation entre sessions

Suivi courant V1. Lire ce fichier au début de session ; consulter
[etat-v0.5.md](etat-v0.5.md) uniquement pour un détail ciblé de la référence 0.5.
Les décisions durables sont dans `Doc/`.

Dernière mise à jour : 2026-10-19 (Routing unifié et continuité des URL d'écoute).

## Repères de version

| Version | Situation |
| --- | --- |
| **0.5** | Version qui tourne et référence fonctionnelle/technique. StationD doit normalement être réutilisable tel quel en V1. Historique : [etat-v0.5.md](etat-v0.5.md). |
| **V1** | Construction autour de cette référence ; une recette V1 valide son nouvel assemblage, sans imposer une réécriture de StationD. |

Toute modification de SD doit répondre à un besoin concret identifié ; une
cible architecturale ne justifie pas une refonte préalable. Les anciens « reste
à faire » 0.5 ne deviennent pas automatiquement la feuille de route V1.
Toujours compiler, tester et exécuter les services dans des containers.

## Contexte V1

Travail sur `V:` = `/data/dev`, hôte `stationdev1.lan`, branche `v1-dev`,
propagation prévue vers `V1`. Les notes de devstationd/septembre sont dans la
référence 0.5, distinctes des validations V1 sur stationdev1.

Socle Icecast commun et administration autonome : [Icecast V1](Doc/v1-icecast.md),
[historique](Doc/v1-icecast-history.md), [supervision](Doc/v1-icecast-monitor.md),
[sauvegarde](Doc/v1-icecast-backup.md), [livraison](Doc/v1-icecast-deploy.md).

## StationDeck — cible et réalisations

**Cible retenue : WebUI egui/eframe compilée en WASM, dans le navigateur,
servie par un container HTTP.** Elle remplace la première interprétation X11.
Aucun X11/Xvfb ni installation de client natif requis.
Messages UI et diagnostics au minimum en anglais.

Contrat/tranche 0 : [contrat](Doc/stationdeck-contract.md),
[plan](Doc/stationdeck-plan.md), exemples de manifestes sous `Doc/stationdeck/`.
Ces exemples restent documentaires ; le résolveur complet n'est pas encore livré.

Socle web/tranche 1 : accueil, navigation, paramètres, sombre/clair, zoom et
modules activés persistés dans le localStorage. Erreurs de stockage/chargement
expliquées. Container HTTP et passerelle distincts ; StationD 0.5 inchangé.

Ordre demandé et implémenté :
1. **Module de test** : données simulées, deux vues, tâche non bloquante et
   annulable, rôle de test en lecture seule, sans réseau ni effet sur la station.
2. **Module gRPC** : connexion asynchrone et lecture de `Station.Status` par
   passerelle HTTP même origine vers le gRPC existant. Aucun contrôle de station.

Catalogue de modules compilés : identifiants et namespaces vérifiés, vues
ordonnées, activation/désactivation, arrêt/annulation et retrait des vues.
Le module de test est activé par défaut ; gRPC est présent mais désactivé.
Les DTO et générations de requêtes sont partagés, sans egui/tonic dans leur crate.

Passerelle : cible fixe configurée côté serveur, jeton requis, secret initialisé
une fois dans un volume privé, pas de port de passerelle publié. Jeton navigateur
en mémoire seulement, effacé à la déconnexion/désactivation. Pas de jeton transmis
à SD. Ce mécanisme protège la lecture de statut ; comptes/rôles utilisateurs et
contrôle métier distant ne sont pas implémentés. Aucun droit admin SD implicite.

Guide : [StationDeck web et modules](Doc/stationdeck-dev.md).
Commandes : `make deck`, `make deck-check`, `make deck-smoke`, `make deck-down`.
URL de développement : [StationDeck](http://stationdev1.lan:18081/).
`make deck-token` affiche le jeton dans le terminal de l'opérateur uniquement.
La cible SD se configure avec `STATIONDECK_GRPC_ADDR` ; aucune cible par défaut.

## Décision : authoring central et icecastd

StationDeck assure l'authoring, backend permanent persistant et distribution gRPC
aux démons/adaptateurs ; FQDN:port explicite prioritaire, sinon découverte séparée.
Plugins inclus via schémas versionnés, droits et adaptateurs ; cache valide local,
versions souhaitée/appliquée distinctes. SD 0.5 inchangé via adaptateur extérieur.
Service/DNS Icecast V1 renommé **icecastd** ; commandes make icecast-* conservées.
Contrat : [configuration et plugins](Doc/stationdeck-configuration.md).
Renommage appliqué au banc stationdev1 : sources A/B et /radio.mp3 reconnectés,
sélection B conservée. 20 tests ciblés et recette audio/gRPC isolée passent.
Backend persistant, authoring par schéma, gRPC pull/report et broadcast IPv4 livrés.
Agent de démo actif sur stationdev1 ; endpoint prioritaire, cache valide et historique.
SD listener-stats par RPC existantes ; agent et monitor icecastd au démarrage normal.
Guide et commandes : [adaptateurs](Doc/stationdeck-daemon-adapters.md).
Images construites ; démarrage normal et valeurs Icecast vérifiés. Suites de tests à lancer.

## Validation et suite

Avant ce câblage : 24 tests Rust, formatage, Clippy natif/WASM,
graphe frontend sans daemon/tonic/protobuf, recette Chromium avec vrai contrat
gRPC et serveur de test isolé. Chargement WASM/MIME, thème/rechargement, tâche
simulée, lecture seule, modules désactivés, authentification, réponse gRPC,
échec amont expurgé, annulation et effacement du jeton sont couverts.
Captures `.local/stationdeck-web-qa/` et `.local/stationdeck-config-qa/`, hors Git.
Cette recette Chromium ne raccorde pas le SD 0.5 réel.

Suite : compléter la tranche 2 (dépendances, services typés, validation complète
des manifestes et politique), puis les services métier et autorisations serveur.
Le catalogue actuel et le statut gRPC ne valent pas réalisation intégrale des
tranches 2 à 4. Autres navigateurs et accessibilité restent à qualifier.

## GUI — espace de fenêtres de plugins (18 octobre 2026)

Direction visuelle validée : pupitre sombre graphite/teal, fenêtres egui par
plugin, plusieurs plugins visibles simultanément et bandeau permanent des
informations essentielles. Implémentation dans le shell StationDeck :

- Une fenêtre par module, onglets pour ses contributions, fermeture sans arrêt
  du module, réouverture et mise au premier plan depuis le lanceur.
- Déplacement, redimensionnement, repli egui, épinglage de position/taille et
  réorganisation en colonnes. Modules/Settings restent au-dessus des plugins.
- Ouverture, onglet, épinglage, position et taille de contenu persistés dans les
  préférences locales existantes ; migration par défaut des anciennes préférences.
- Contribution optionnelle `DeckModule::summary` : bandeau permanent des vrais
  instantanés StationD/Icecast, même lorsque les fenêtres sont fermées. États
  pending/indisponible distincts ; aucune valeur fictive ni nouveau polling.

Validation effectuée en containers sur stationdev1 : 9 tests Rust ciblés passent,
Clippy ciblé natif/WASM sans avertissement, construction WASM normale et `ui-test`,
recette Chromium complète réussie (multi-fenêtrage, fermeture/réouverture,
épinglage/rechargement, thème, gRPC, perte de fournisseur, annulation, nettoyage
des jetons et plugin Icecast autonome). Captures `.local/stationdeck-gui-qa/`.
Le détail des vues métier reste à enrichir progressivement ; aucun contrôle de
lecture ou chiffre simulé ajouté aux services réels. Guide :
[espace de plugins](Doc/stationdeck-dev.md#plugin-workspace).

### GUI — voyants et informations vitales

Voyants dessinés (sans dépendance à des glyphes Unicode) et libellés communs :
vert = disponible, orange = lecture/attente, dépendance bloquée ou dégradation,
rouge = échec, noir bordé = désactivé/arrêté ou pas encore connecté. Tous les
plugins restent dans le lanceur ; une entrée sans vue ouvre Modules. La couleur
est également affichée dans les fenêtres et dans la gestion des modules.

`DeckModule::health` distingue le cycle de vie local des résultats de service :
activer gRPC, Icecast provider ou Configuration ne les rend pas verts avant une
lecture réussie. Les résumés StationD/Icecast utilisent de petits instruments
pour le nom de station, l'uptime, l'audience, les sources et les mounts sans source.
Les instantanés restent explicitement libellés ; une erreur retire les anciennes
valeurs. Les informations vitales restent présentes quand la fenêtre est masquée.

Validation de cette finition en containers : 15 tests Rust ciblés passent,
Clippy natif/WASM sans avertissement et recette Chromium complète réussie,
avec assertions sur les états déconnecté/vert/orange/rouge et sur le bandeau
Icecast lorsque sa fenêtre est fermée. Captures `.local/stationdeck-colors-qa/`.

### GUI — liste latérale compacte

Le lanceur affiche désormais uniquement le nom et le voyant d'état général de
chaque plugin. Les libellés détaillés restent au survol et dans les fenêtres ;
les sous-vues se choisissent dans leurs onglets internes. Un clic sur le plugin
rouvre sa fenêtre en conservant le dernier onglet choisi.

Validation en containers : tests StationDeck, Clippy natif/WASM et recette
Chromium complète passent avec la nouvelle navigation par onglets internes.

### GUI — fenêtre Tokens et sauvegarde locale (19 octobre 2026)

À la demande de l'utilisateur, la règle antérieure « jetons en mémoire seulement »
est remplacée : fenêtre système Tokens disponible sans plugins, deux champs
masqués, Deck gateway partagé StationD/Icecast et Configuration author séparé.
Sauvegarde automatique dans `stationdeck.tokens.v1`, distincte des préférences,
et restauration au rechargement, sans connexion automatique.

Déconnexion/désactivation : annulation et effacement des données privées, mais
conservation du jeton sauvegardé. Oublier/Oublier tous retire les valeurs et
invalide les requêtes/snapshots, même fenêtres fermées. Stockage localStorage de
ce navigateur/origine (pas un coffre chiffré). Erreurs de stockage signalées et
versions invalides/futures conservées jusqu'au remplacement explicite.

Validation en containers : 17 tests Rust ciblés et Clippy natif/WASM passent.
Recette Chromium StationD/Icecast réussie (sauvegarde, rechargement, partage,
oubli pendant une requête et séparation des préférences). Recette Configuration
réussie : jeton auteur restauré, édition, publication/appli agent, déconnexion
et oubli explicite. Captures dans .local/stationdeck-tokens-qa/ et
.local/stationdeck-tokens-config-qa/. WebUI de développement reconstruite.

### GUI — commandes de gestion Icecast (19 octobre 2026)

Fenêtre Icecast enrichie d'un onglet Management : sélection d'un mount réel,
édition du titre, liste des auditeurs (ID/IP/client/durée), déconnexion ciblée
d'un auditeur ou d'une source avec confirmation/annulation, état du routeur et
bascule entre ses entrées configurées. Rafraîchissement unique du statut après
mutation ; aucune répétition automatique des commandes en échec/timeout.

Service typé optionnel `stationdeck.icecast.control` v1, capacité dédiée et
permission `icecastd.commander`. Refus de cette permission : supervision toujours
accessible. Provider HTTP séparé du plugin ; passerelle vers les RPC IcecastAdmin
existantes. Admin Icecast et endpoint restent côté serveur. Nouvelle route POST
fermée/validée ; jeton de lecture systématiquement refusé pour la gestion.

Troisième champ sauvegardé dans Tokens : Icecast control, indépendant du token
Deck gateway. `make deck-icecast-control-token` affiche cette valeur uniquement
dans le terminal opérateur. Init ajoute le nouveau secret sans remplacer le
jeton de lecture ; deux jetons identiques sont rejetés. Oublier le contrôle annule
ses requêtes et résultats sans retirer les instantanés de lecture.

Validation en containers sur stationdev1 : 24 tests Rust frontend/services et
6 tests gateway, Clippy ciblé natif/WASM sans avertissement. Recette Chromium
complète réussie : tous les RPC de gestion, annulation sans appel amont, refus du
token lecture sans appel gRPC, erreur expurgée et oubli indépendant du contrôle.
Captures `.local/stationdeck-operations-qa/`, dont `icecast-management.png`.

Images normales reconstruites et gateway/WebUI déployés, containers healthy.
Sur le vrai banc de dev sans auditeur : lecture, liste des auditeurs et mutation
métadonnées validées, titre d'origine restauré. Le routeur de ce banc est arrêté
(actuellement aucun container router) : GetRoute renvoie indisponible ; bascule
A/B validée sur le serveur protobuf isolé. Sources réelles laissées connectées.
Guide : [gestion Icecast](Doc/stationdeck-dev.md#icecast-stream-management).

### GUI — traces de session avec egui_tracing (19 octobre 2026)

Intégration de `egui_tracing` 0.3.0, qui impose egui 0.34 : egui/eframe et toutes
les crates frontend alignés sur 0.34.3 ; shell migré vers root Ui, Panel et App::ui.
Fenêtre système Logs indépendante des plugins, ouverte depuis le bandeau ou le
lanceur, déplaçable/redimensionnable/fermable. Filtres niveau/cible, détails,
effacement et retour au bas fournis par le widget. Taille bornée à l'espace de
travail ; fermeture vérifiée sur une vue de 1120 pixels.

Un subscriber tracing par instance WASM, réutilisé si WebRunner est recréé ;
échec d'installation signalé dans Logs. Derniers 1 000 événements en mémoire,
vidage du tampon pendant les frames même fenêtre fermée, nouvelle session au
rechargement. État ouvert/fermé seul sauvegardé avec les préférences.

Premières traces : démarrage navigateur, transitions cycle de vie/état général
des plugins (sans répétition par frame), soumission et résultat des commandes
Icecast. Champs limités aux identifiants, états statiques et noms de commandes ;
aucun token, titre saisi, auditeur, payload ou erreur amont brut enregistré.
Cibles `stationdeck` uniquement. Les logs des démons restent dans leurs containers ;
aucune collecte distante ajoutée. Guide :
[traces de session](Doc/stationdeck-dev.md#session-logs).

Validation en containers : 37 tests Rust ciblés passent, Clippy ciblé natif/WASM
sans avertissement, graphe frontend sans daemon/tonic/prost. Tests de rétention,
filtrage des cibles, effacement, absence de répétition et fenêtre sans plugins.
Recette Chromium générale complète réussie, y compris Logs sans plugin,
réouverture après reload, fermeture, et commandes Icecast. Recette Configuration
complète réussie (droits, persistance, découverte, offline/reprise, édition et
publication UI). Captures `.local/stationdeck-tracing-qa/`. Image WebUI normale
reconstruite et déployée sur stationdev1 (container healthy) ; aucun changement du daemon.

### Correctif Management — routeur du banc (19 octobre 2026)

Signalement utilisateur : « Operation failed or its outcome is unknown ».
Reproduit avec GetRoute : container router absent. Router démarré avec son profil
switch et sa sélection persistée B. Second blocage identifié : découverte audio
ambiguë (plusieurs adresses pour le même service sur le banc multi-réseaux).
Le binding audio du routeur utilise désormais l'endpoint Docker explicite
`http://icecastd:8000` dans `config/icecast/discovery-router.json`, conformément
à la priorité endpoint explicite du contrat. Aucun choix arbitraire d'adresse
ni assouplissement de la vérification de découverte.

Route réelle à nouveau connectée sur /radio.mp3. GetRoute et bascule B → A → B
validés via la passerelle HTTP de la GUI ; B restauré et aucun auditeur sur la
sortie. Le compteur d'un auditeur sur /test-b.mp3 représente le routeur lui-même.

Passerelle : erreur de lecture de route identifie maintenant le routeur
indisponible et invite à vérifier son service ; la liste des auditeurs a son
message propre. Une lecture/son timeout ne prétend plus qu'une mutation a un
résultat inconnu. L'avertissement d'issue incertaine reste pour les mutations.
Diagnostics amont bruts toujours expurgés. 8 tests gateway et Clippy passent,
dont deux régressions lecture/mutation. Gateway redéployé et healthy.

### GUI — pupitre graphique Icecast (19 octobre 2026)

Maquette validée par l'utilisateur transposée dans le plugin : cartes des entrées
configurées, lettre/nom/mount/titre/type réels, état Ready/ON AIR/déconnecté/inconnu,
liaisons dessinées vers la carte PROGRAM OUTPUT. Bouton Take on air par entrée,
sélection active verte seulement si route connectée sans source connue déconnectée.
Carte sortie avec mount, titre, connexion et audience propre à ce mount.
Aucune jauge, waveform, valeur de bitrate ou donnée manquante simulée.

Détails et résumé auditeurs sous le diagramme, disposition empilée si fenêtre
étroite, titre modifiable à la demande avec Save/Cancel, auditeurs détaillés
repliables. Déconnexion source déplacée dans Advanced actions ; confirmations
ciblées et annulation conservées. Retours de commande et pending intégrés.
Le dernier état de route est conservé pendant les commandes sans rapport ;
échec de commande de route et retrait de credential l'invalident.

Ajout protobuf compatible `RouteStatus.input_streams` (champ 6, RouteInput id/mount)
alimenté par la configuration du routeur, borné/expurgé dans la passerelle et
optionnel côté DTO frontend pour les anciens serveurs. Le plugin ne devine pas
l'association mount/entrée. Pas de nouvelle crate : Frames/contrôles et painter
egui suffisent. Sidebar/window Icecast renommés simplement « Icecast ».

Validation en containers : 26 tests Rust frontend/services/plugin, 8 tests gateway
et 3 tests routeur passent ; Clippy ciblé natif/WASM sans avertissement. Rendu
large/étroit et absence des nouveaux champs sur ancien statut couverts. Recette
Chromium complète réussie, incluant bascule directe par carte, métadonnées,
liste auditeurs, annulation et déconnexion via menu ; navigation, Tokens et Logs
continuent de passer. Images admin/routeur, gateway et WebUI reconstruites et
déployées, containers healthy. GetRoute et B → A → B réels validés, B restauré.
Captures de recette dans `.local/stationdeck-desk-qa/` (données de fixture).

Recette supplémentaire contre les vrais services : navigateur Chromium avec les
mêmes assets instrumentés, gateway réelle et credentials confinés au container.
Mounts A/B exacts et état ON AIR de B vérifiés dans le canvas ; capture réelle
`.local/stationdeck-desk-live.png`. Sortie /radio.mp3 à zéro auditeur, B conservé.
Containers de recette/aperçu retirés, volumes de recette isolés supprimés.

### GUI — maintenance des groupes d'auditeurs (19 octobre 2026)

Cas demandé : déplacer les connexions de la station VM2 vers le mount de VM3,
puis rappeler uniquement ce groupe après maintenance, sans toucher à VM1 ni aux
auditeurs natifs de VM3. Même serveur Icecast partagé ; aucun changement du
routeur audio, de la sélection programme ou des URLs publiques.

Onglet Icecast « Listener maintenance » : origine/destination parmi les mounts
réels, préparation sans déplacement, confirmation avant départ et retour,
annulation d'un plan, journal et compteurs. Le groupe capture les connexions
présentes à la préparation ; nouveaux arrivants et reconnexions exclus. Les IDs
Icecast sont des identifiants de connexion, pas des tokens humains durables.

Cinq RPC compatibles ajoutés : Plan/Run/Return/Cancel/ListListenerMigration(s),
DTO/commandes explicites, passerelle expurgée, token de contrôle séparé, fixture
et contrôles canvas de recette. IDs individuels conservés uniquement côté admin,
SQLite dans le volume admin-state (data-dir/plugins du banc). WAL + synchronous
FULL, journal privé ; transitions persistées avant action, exécution backend
indépendante du navigateur, commandes sérialisées, aucune répétition automatique.
Reprise après redémarrage admin : état interrompu, IDs incertains conservés.
Redémarrage Icecast détecté par son démarrage annoncé et l'origine : anciens IDs
expirés. Pas de rapprochement par IP/UA. Limites et périmètre détaillés dans
[Icecast V1](Doc/v1-icecast.md#maintenance-des-auditeurs-depuis-stationdeck).

Correction transport reproduite sur Icecast 2.5 réel : /admin annonce Keep-Alive
mais ferme la connexion après réponse. Réutilisation immédiatement suivante :
RemoteDisconnected/reqwest indisponible. Les requêtes admin demandent maintenant
Connection: close, sans retry implicite d'une mutation. Le mock de régression
annonce lui aussi Keep-Alive puis ferme, et vérifie ce header client.

Validation en containers : 26 tests frontend/services/plugin, 8 tests gateway,
32 tests admin passent (1 test préexistant ignoré), Clippy ciblé natif/WASM et
admin sans avertissement. Cinq tests ciblent le journal, les chevauchements,
les réponses perdues après effet, le retour sélectif et les IDs invalidés.

Essai réel via la passerelle déployée : deux auditeurs jetables de /test-a.mp3
vers /test-b.mp3, maintien des IDs et des auditeurs natifs (routeur inclus),
redémarrage admin pendant le séjour et récupération du journal. Fermeture d'un
membre puis nouvelle connexion sur B : retour du seul ancien ID encore connecté,
nouvel ID et natifs préservés ; compteurs returned=1/missing=1. Auditeurs jetables
fermés en fin de recette ; B reste sélectionné, /radio.mp3 sans auditeur.
Affichage réel du journal terminé validé dans Chromium ; capture
`.local/icecast-live-maintenance.png`. Images admin, gateway et WebUI déployées.
Recette Chromium générale complète réussie, incluant préparation, annulation de
confirmation sans RPC, déplacement, rechargement du journal depuis le backend,
retour et état terminé. Routing, métadonnées, déconnexions, Tokens et Logs
continuent de passer. Captures .local/stationdeck-migration-qa/.

### Correction de périmètre — Routing unifié et continuité des URL (19 octobre 2026)

Correction utilisateur : rerouter le flux même à zéro auditeur, dans un seul
Routing graphique. Suppression de Listener maintenance. Cartes origine/cible,
états et compteurs réels, liaison PREVIEW/ACTIVE, activation confirmée, retour
sur la route et historique repliable. L'ancien pupitre programme reste un outil
avancé repliable dans le même écran. Frames/painter egui, aucune nouvelle crate.

RPC/DTO continuous rétrocompatible. La règle reste active vide, prend les arrivées
suivantes et survit au redémarrage admin. Conflits, chaînes et cycles sont refusés
même sans aucun ID. Anciens groupes conservés pour compatibilité/historique.

Implémentation finale : entrée TCP du container géré sur 8001 publiée au même
port hôte 18000 ; Icecast 8000 reste privé pour les services/sources internes.
GET d'origine aiguillés vers la cible même avec source arrêtée, uploads SOURCE/PUT
inchangés. Marqueur interne aléatoire par règle pour retrouver les connexions ;
ni IP ni user-agent humain utilisé pour reconnaître une personne. Confiance XFF
locale explicite et nettoyage des chaînes des pairs non approuvés. Socket Unix
sur volume routing-control entre admin et superviseur, aucun port de commande
supplémentaire exposé. Relais audio en flux, ressources bornées. Retour : désarmer
l'entrée, collecter les dernières arrivées, déplacer les IDs vérifiés seulement.

La tentative de rechargement natif Icecast n'a pas garanti la continuité sur ce
banc et a été retirée : aucun SIGHUP, max-listeners modifié ou pause de l'agent
dans l'implémentation finale. L'attachement d'un débogueur mémoire a été refusé
par la revue automatique ; diagnostic poursuivi par code/tests réseau. Aucun
débogueur privilégié lancé.

Validation : 35 tests admin (1 ancien ignoré), 26 frontend/services/plugin,
8 gateway, cinq tests TCP/réécriture/confiance IP/propriété de règle et régression
de démarrage géré passent. Clippy ciblé natif/WASM et admin passent. Recette
Chromium générale passée avec Routing unique, confirmations et retour.

Recette réelle : activation A→B à zéro, écoutes suivantes sur URL A ; arrêt de
la source A puis nouvelle écoute toujours disponible ; redémarrage admin avec
règle/journal conservés ; retour des trois connexions suivies, natifs B et route
programme B préservés. Une nouvelle connexion A après retour reste sur A.
Auditeurs jetables fermés. Images admin, gateway, serveur géré et WebUI déployées.
Détails/périmètre : Doc/v1-icecast.md, section reroutage.

### GUI — correction du premier plan des fenêtres (20 octobre 2026)

Logs et les fenêtres système n'imposent plus une couche supérieure aux plugins.
La sélection d'une fenêtre est résolue après le rendu de toutes les fenêtres,
avec un identifiant stable ; une fenêtre existante revient bien devant les autres.
Sélectionner Logs déjà ouvert le remonte, sa croix le ferme. L'ouverture/réactivation
via Modules conserve le gestionnaire accessible au frame créant les fenêtres.
Les positions, tailles, onglets et jetons sauvegardés ne sont pas modifiés.

Deux tests de recouvrement vérifient la vraie couche egui à l'intersection des
fenêtres : plugins face à Logs, puis Settings/Tokens/Modules et activation.
La recette Chromium sélectionne alternativement Logs et deux plugins et vérifie
que leurs vrais boutons reçoivent les clics malgré le recouvrement.

Validation en containers : 13 tests StationDeck et Clippy natif/WASM passent.
Le test de recouvrement échouait avant correction ; la recette Chromium complète
passe avec sélection alternée Logs/plugins et activation depuis Modules.
La demande de premier plan est renouvelée au passage suivant pour résoudre les
demandes simultanées d'egui. Captures : .local/stationdeck-focus-qa/.
