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
