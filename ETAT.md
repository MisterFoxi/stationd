# État d'avancement — passation entre sessions

Suivi courant V1. Lire ce fichier au début de session ; consulter
[etat-v0.5.md](etat-v0.5.md) uniquement pour un détail ciblé de la référence 0.5.
Les décisions durables sont dans `Doc/`.

Dernière mise à jour : 2026-10-18 (premiers adaptateurs réels câblés ; recette à effectuer).

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
