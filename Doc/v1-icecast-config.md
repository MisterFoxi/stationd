# V1 — configuration Icecast et suivi géographique

Le TOML central est `config/icecast/icecast.toml`. Le générateur Icecast et
le daemon administratif partagent le même schéma Rust strict. Le XML généré
est privé et temporaire ; secrets, base GeoLite et états ne vont pas dans Git.
Les services restent indépendants de StationD et de Liquidsoap.

## Vérifier et appliquer sur stationdev1

Depuis `/data/dev`, après `make icecast-init`, installer la base GeoLite2 City
sous `data/geoip/GeoLite2-City.mmdb` puis construire avec `make icecast-up`.
Si la géolocalisation n'est pas souhaitée, mettre `[geoip] enabled = false`.
Avec `enabled = true`, une base absente, illisible, invalide ou d'une édition
incompatible empêche le démarrage et l'application.

```sh
make icecast-ctl A='config show'          # TOML réellement chargé
make icecast-ctl A='config revision'      # SHA-256 de ce document
make icecast-ctl A='config check -' < config/icecast/icecast.toml
make icecast-check                       # schéma + tous les secrets + MMDB
make icecast-apply                       # applique si différent
```

Pour préparer une modification sans toucher au fichier courant :

```sh
make icecast-check CONFIG=.local/icecast/candidate.toml
make icecast-apply CONFIG=.local/icecast/candidate.toml
```

`config check` est une prévalidation gRPC : le daemon n'a pas accès aux
mots de passe des sources. `icecast-check` et `icecast-apply` vérifient
également tous les secrets dans un conteneur éphémère avant tout arrêt.
Les nouveaux secrets doivent être déclarés dans Compose et provisionnés.
L'application refuse aussi de retirer les mounts dont dépend le routeur actif.

L'application est verrouillée pour éviter deux opérations concurrentes.
Elle sauvegarde la configuration **chargée** dans `.local/icecast/apply/`,
écrit atomiquement le TOML canonique, recrée Icecast et l'administration,
relance les sources de test et le routeur précédemment actifs, puis vérifie
la révision chargée et leur reconnexion. Sur échec, elle restaure le TOML
chargé précédent et tente de rétablir les services. Une panne empêchant
également ce rétablissement est signalée avec le chemin de sauvegarde.

Cette opération coupe les connexions des auditeurs : ce n'est pas un
rechargement sans interruption. Une configuration identique ne redémarre rien.
`make icecast-reconfigure` force le rechargement après validation, par exemple
pour renouveler les secrets ou la base. Le retour arrière concerne le TOML ;
il ne restaure ni secrets ni fichier MMDB. Aucun commit Git automatique.
Le script hôte accepte `--expect-revision SHA256` pour refuser une application
si la configuration chargée a changé depuis sa lecture.

## Champs pris en charge

| Élément | Contraintes et comportement |
|---|---|
| Identité | `location`, `hostname`, `admin_email`, `admin_user` non vides, sans caractères de contrôle |
| Port | `port = 8000` interne ; publication hôte par `ICECAST_BIND` et `ICECAST_PORT` |
| Connexions | `max_clients` de 1 à 32768 ; de 1 à 64 mounts déclarés |
| Secrets | `admin_password_file` et `mounts.password_file` : noms de fichiers simples sous `/run/secrets` |
| Buffers | `[limits] queue_size` de 8192 à 16777216 octets ; `burst_size` inférieur ou égal à la queue |
| Délais | `client_timeout`, `header_timeout`, `source_timeout` de 2 à 600 secondes |
| Logs | `[logging] level` : `debug`, `information`, `warning`, `error` ; rotation Docker 3 × 10 Mo |
| Mounts | `hidden`, `max_listeners` de 1 à `max_clients`, `fallback_mount` déclaré, sans cycle |
| Audience | `audience = true` inclut le mount dans l'agrégat géographique ; indépendant de `hidden` |
| Proxies | `trusted_proxies` : au plus 16 adresses IP exactes distinctes |
| GeoLite | `[geoip] enabled`, `database_file` : nom MMDB simple sous `/data/geoip` |

Les champs inconnus, secrets inline et chemins de secrets hors répertoire
sont refusés. Le document est limité à 256 Kio. Un fallback natif Icecast
n'est pas une bascule du point stable ; les paramètres du routeur restent
dans `config/icecast/router.toml`.

## Traefik et IP de l'auditeur

Le modèle reprend celui du StationD existant : Traefik `192.168.1.94`
est le seul proxy déclaré. Icecast n'accepte l'adresse `X-Forwarded-For`
que depuis l'adresse exacte d'un proxy déclaré. Une connexion directe
reste possible sur le bind de test `192.168.1.118:18000`.

La route V1 sur Traefik envoie vers cette adresse et transmet l'IP effective.
Son installation et sa validation sont décrites dans [Historique et Traefik](v1-icecast-history.md).
Si Traefik sort par une autre adresse ou par un NAT, déclarer l'adresse
réellement vue par Icecast, puis contrôler `stationctl icecast listeners`.
Traefik doit également maîtriser les en-têtes reçus de ses propres proxies.

## Géographie courante

```sh
make icecast-geography
make icecast-ctl A='listeners /radio.mp3'
```

Le daemon réutilise le lecteur GeoLite existant de StationD pour obtenir
pays, région et ville à partir des IP retournées par Icecast. Il expose
la disponibilité, l'édition et la date de construction de la base.
Les statuts distinguent `found`, `not_found`, `private`, `unavailable` et
`error` ; une source indisponible produit un compteur `null` et un instantané
incomplet, jamais un faux zéro. Les IP LAN ne sont pas placées dans une ville.

Les entrées techniques A/B sont exclues de l'agrégat ; `/radio.mp3` est inclus.
L'agrégat contient des comptes par pays/région/ville/statut sans IP individuelle.
La commande authentifiée `listeners` conserve les IP utiles au dépannage.
Le plugin de statistiques existant est maintenant raccordé à l'administration
V1 pour la conservation des observations ; voir [Historique et Traefik](v1-icecast-history.md).
Les journaux d'accès natifs Icecast continuent, eux, à contenir des IP.

La même base est fournie en lecture seule à Icecast pour ses vues natives.
Sur le banc Icecast 2.5, l'IP transmise par proxy apparaît correctement dans
l'interface native, mais son agrégat géographique ne renseigne pas le pays
pour cette connexion. La répartition centrale effectue son propre lookup
sur l'IP effective et constitue la référence de suivi géographique.

Mise à jour manuelle (identifiants MaxMind demandés au terminal) :

```sh
make icecast-geoip-update
# Après remplacement manuel du fichier :
make icecast-geoip-reload
```

Remplacer la base atomiquement, garder une copie valide pour retour arrière,
et ne pas placer les identifiants MaxMind dans le TOML ou Git.

## Vérifications

```sh
cargo test --locked --manifest-path tools/icecast-config/Cargo.toml
cargo test --locked --manifest-path crates/stationd-admin/Cargo.toml
python3 -B tests/icecast-config-apply.py
make icecast-geography-test
```

Le test géographique utilise un projet Docker unique sans ports publiés
et la petite [base officielle de test MaxMind](https://github.com/maxmind/MaxMind-DB/tree/main/test-data),
téléchargée sous `.local/`. Deux clients présentent la même IP publique dans
un en-tête : seul le proxy déclaré est géolocalisé à Londres ; le second
reste identifié comme client privé. Les conteneurs isolés sont retirés ensuite.
Le banc habituel continue à fonctionner.

## Validation sur stationdev1 — 16 octobre 2026

- Schéma/générateur : 13 tests réussis ; deux tests supplémentaires ont été
  exécutés avec la fixture officielle et la vraie base GeoLite, tous deux réussis.
- Administration/routeur : 9 tests réussis ; formatage et Clippy sans avertissements.
- Application : deux tests de retour arrière réussis. Sur le banc réel, un
  TOML invalide n'a pas recréé Icecast ; passage de 32 à 40 connexions, vérification
  de la révision, réapplication sans redémarrage puis restauration à 32 réussis.
- Proxy/géographie : deux clients simultanés, en-tête accepté uniquement pour
  le proxy déclaré, lookup GB/London, agrégats corrects et nettoyage du projet isolé.
- Administration native et gRPC : sources, auditeurs, métadonnées, déconnexions,
  authentification et décodage MP3 validés. Le même auditeur a reçu 36 864 octets
  pendant une indisponibilité de l'administration, sans redémarrage d'Icecast.

La route Traefik réelle est désormais installée et testée de bout en bout ;
voir [Historique et Traefik](v1-icecast-history.md) pour les résultats et commandes.

Le test de bascule a également été rejoué : même auditeur sur A (439,6 Hz),
B (879,8 Hz), puis A ; cible invalide ou absente refusée ; continuité pendant
l'arrêt administratif ; récupération manuelle de A en panne vers B ; sélection
B conservée après redémarrage du routeur. Icecast n'a pas redémarré durant ce test.
Le banc est laissé actif sur B, `/radio.mp3`, GeoLite prêt. Aucun StationD lancé.
