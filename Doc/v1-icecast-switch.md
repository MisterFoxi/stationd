# Bascule immédiate A/B — banc V1

Point d'écoute stable : http://192.168.1.118:18000/radio.mp3.
Entrées : /test-a.mp3 (440 Hz) et /test-b.mp3 (880 Hz).

Le daemon Rust station-router relaie une seule entrée vers /radio.mp3.
Il fonctionne indépendamment de station-admin. Stationctl transmet les
commandes en gRPC au daemon administratif, qui pilote le routeur en gRPC
avec un jeton distinct. Aucun StationD ni Liquidsoap n'est lancé.

Icecast permet de déplacer les auditeurs existants, mais ce déplacement
seul ne définit pas la source des futures connexions à une URL stable.
Ce banc conserve une connexion source unique sur le mount stable.
Référence : [administration Icecast](https://www.icecast.org/docs/icecast-latest/admin_interface/).

## Essai interactif

Depuis /data/dev sur stationdev1 :

```sh
make icecast-switch-up
make icecast-ctl A=route
make icecast-ctl A='switch a'
make icecast-ctl A='switch b'
```

Garder le lecteur ouvert sur /radio.mp3 : la tonalité change sans changer
d'URL ni reconnecter l'auditeur. Le délai audible dépend des buffers
d'Icecast et du lecteur. « Immédiat » signifie sans attente de fin de
morceau, pas un délai audio nul.

Les deux sources continuent leur propre flux. Aucun état de lecture
n'est transféré, avancé ou remis à zéro par la bascule.

## Contrat de ce premier banc

Le routeur de validation accepte uniquement du MPEG-1 Layer III,
64 kbit/s, 44,1 kHz, mono, avec réservoir de bits désactivé.
Les sources synthétiques sont configurées ainsi. Une cible absente ou
incompatible est refusée avant de quitter la source courante.

Ce contrat permet de tester le routage sans transcodage ; il ne représente
pas encore le support général des formats d'une station de production.
Le mode ad hoc / best effort reste une étape suivante.

## Configuration et états

- config/icecast/router.toml : sortie et entrées déclarées, dans Git.
- config/icecast/icecast.toml : mount /radio.mp3, dans Git.
- router-api-token et source-radio-password : secrets locaux hors Git.
- Volume Docker router-state : dernière sélection, hors Git.

La sélection est enregistrée atomiquement et reprise après redémarrage du
routeur. Redémarrer ce dernier recrée sa connexion source et peut couper
les auditeurs ; une bascule ordinaire ou un arrêt de l'administration
gardent la connexion audio.

Le port gRPC 50062 du routeur reste interne au réseau Docker.
Le routeur tourne sans privilèges, sans socket Docker ni accès NFS.

## Validation

```sh
make icecast-switch-test
```

Le test décode le MP3 reçu sur la même connexion HTTP et reconnaît
440 puis 880 puis 440 Hz. Il vérifie aussi l'identifiant d'auditeur,
le refus d'une entrée inconnue et d'une cible indisponible, la continuité
pendant un arrêt de l'administration, la reprise de B après redémarrage
du routeur et l'absence de redémarrage d'Icecast.

Il laisse le banc actif, sur B. Pour arrêter routeur et sources :

```sh
make icecast-switch-down
```

Arrêter ce banc avant les anciens tests icecast-test et
icecast-continuity-test, qui utilisent les mêmes entrées.

L'interface native Icecast conserve ses opérations. Un déplacement manuel
d'auditeurs dans cette interface ne modifie pas la sélection du routeur :
utiliser stationctl icecast switch a|b pour piloter le point stable.
La future console egui pourra utiliser les mêmes RPC.


## Résultat vérifié le 16 octobre 2026

Les cinq tests Rust du socle administratif/routeur et les trois tests du
générateur XML passent ; formatage et Clippy passent également.

Le test réel a mesuré environ 439,6 Hz puis 879,8 Hz sur la même connexion
Icecast (identifiant conservé), avant de revenir à A. Il a vérifié le
refus d'une cible absente, la récupération manuelle après arrêt de A,
la continuité pendant l'arrêt/reprise de l'administration et la reprise
de B après redémarrage du routeur. Icecast n'a pas redémarré pendant ce test.

## Vue graphique StationDeck

Le pupitre Icecast consomme GetRoute/SwitchRoute et les statuts de mounts.
RouteStatus expose également `repeated RouteInput input_streams = 6`, avec
`id` et `mount`, renseignés depuis la configuration effective du routeur.
Ajout protobuf compatible : les champs existants et les identifiants d'entrées
sont conservés. Un ancien routeur sans ce champ fonctionne encore dans le
pupitre, avec mount/disponibilité non renseignés plutôt qu'une correspondance
inventée. Les commandes ne sont jamais relancées automatiquement par l'UI.
