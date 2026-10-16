# V1 — historique géographique et route Traefik

La route publique du banc est `https://v1stream.homestone.rp-radio.live/`.
La racine sert `/radio.mp3`. `/admin/` conserve l'administration native,
authentifiée par Icecast. Pour Second Life, HTTP sert directement / et
/radio.mp3, sans redirection. Les autres chemins HTTP, dont /admin/, restent
redirigés vers HTTPS. Le certificat wildcard existant couvre ce nom. La route n'utilise pas le middleware historique qui
réécrit tous les chemins : les chemins natifs restent disponibles.

Le fragment versionné est `config/icecast/traefik.yaml`. Il cible
`http://192.168.1.118:18000`, avec des noms de routes, service et middlewares
propres à V1. La fusion refuse toute collision avec une configuration différente
et préserve les valeurs des routes existantes. L'installation sur Traefik
sauvegarde son ancien fichier sous `/home/ubuntu/.local/stationd-v1/` (privé).
Le provider file existant recharge la configuration sans redémarrer Traefik.
Son fichier est monté individuellement dans Docker : une modification sur
l'hôte doit préserver l'inode du fichier monté pour être visible.

## Plugin administratif chargé dynamiquement

`config/icecast/admin.toml` déclare le WASM existant `listener-stats`, ses
capacités et sa politique de rétention. Le WASM est construit depuis
`plugins/listener-stats-wasm` et chargé à l'exécution par Extism, avec le même
contrat `ListenerSnapshot`, les mêmes quatre migrations et le même hôte
SQLite confiné que StationD. Le daemon StationD n'est pas lancé.

Ce premier hôte administratif fournit les imports `db_batch` et `geoip_lookup`.
Les capacités autorisées sont `db`, `geoip`, `listener_details` ; un plugin sans
`listener_details` ne reçoit pas les observations individuelles. Les fichiers
WASM sont locaux ; pas de WASI, de répertoires invités ou de réseau invités.
Chaque appel invité est borné à deux secondes et la mémoire à 64 Mio.
Les plugins sont chargés au démarrage du daemon administratif ; remplacer un
WASM ou modifier sa déclaration demande de recréer uniquement `admin`.
Une déclaration, un WASM ou une migration invalide refuse le démarrage de
l'administration ; Icecast et le routeur continuent de fonctionner.
Les points d'extension de présentation pour egui restent à raccorder.

Le lecteur GeoLite est partagé avec l'administration. Aucun service de
lookup externe n'est appelé. Seuls les mounts `audience = true` sont collectés,
par défaut toutes les 30 secondes. Chaque résultat de collecte donne une
observation datée ; il ne s'agit pas d'un relevé simultané de tous les flux.
Les tâches manquées pendant une indisponibilité ne sont pas recréées.

Le volume Docker `admin-state` conserve
`/var/lib/station-admin/plugins/listener-stats.db` entre les recréations du
daemon. Les connexions individuelles et IP restent transitoires ; le plugin
stocke seulement les effectifs et agrégats pays/région/ville/statut. Les IP
privées restent `not_found` dans l'historique compatible avec le plugin existant.
La commande géographique directe distingue toujours explicitement `private`.
Les journaux natifs Icecast contiennent leurs informations d'accès habituelles.

La base V1 est distincte de celle du StationD 0.5 ; aucune base existante n'est
déplacée ni réinitialisée. Pour une reprise historique ultérieure, arrêter les
deux écrivains et préparer une copie contrôlée, jamais deux écritures sur le
même fichier. Une migration ne se retourne pas avec le seul rollback du TOML
Icecast : sauvegarder également le volume avant un changement de migrations.

## Administration et lecture

Sur stationdev1, depuis `/data/dev` :

```sh
make icecast-ctl A='history-status'
make icecast-ctl A='history --since 24h --by hour'
make icecast-ctl A='history --since 7d --by day --mount /radio.mp3'
make icecast-ctl A='history --since 4w --by week'
```

Le gRPC authentifié expose `GetHistoryStatus` et `QueryHistory`. Les résultats
JSON tabulaires conviennent aussi à la future console native. Les requêtes
sont prédéfinies ; aucune écriture SQL n'est exposée au client administratif.
Le statut indique la dernière tentative, la dernière réussite et les erreurs
invitées sans recopier leurs messages susceptibles de contenir des données
clients. Les compteurs de diagnostic repartent au démarrage ; les observations
conservées restent accessibles dans la couverture des requêtes.

`coverage` affiche les observations réussies et échouées et leurs dates par
mount, y compris lorsqu'aucun effectif positif ne figure dans `statistics`.
Une réponse vide ne signifie donc pas automatiquement zéro auditeur.
Une collecte inconnue est stockée avec `NULL`, distinct d'un succès à zéro.
Les moyennes comptent les zéros réussis, excluent les collectes échouées et
ne sont pas pondérées par la durée. Les dates et semaines du lundi sont UTC.
Ce sont des audiences simultanées observées, pas des auditeurs uniques.

Par défaut, la rétention est de 30 jours ; `plugin.config.retention_days`
accepte 1 à 365 jours. Le plugin purge les anciennes observations à chaque
écriture ; un arrêt administratif laisse un trou de collecte. Les erreurs de
base ou du WASM pendant la collecte laissent le flux audio indépendant et sont
signalées dans le statut historique. Augmenter la rétention ne récupère pas les données effacées.

Limites administratives : au plus 8 plugins, un fichier WASM de 32 Mio,
base SQLite de 256 Mio maximum (64 Mio configurés), requêtes de 1000 lignes
et une seconde maximum. Réduire la période ou filtrer le flux si la requête
atteint une limite ; les réponses ne sont pas tronquées silencieusement.

Pour recréer seulement l'administration après modification :

```sh
docker compose -f docker/icecast/compose.yaml up -d --build --no-deps --wait admin
```

Ne pas supprimer `admin-state` pour une mise à jour. Un `down` normal conserve
le volume ; `down --volumes` efface également cet historique.

## Validation

```sh
make icecast-traefik-test
make icecast-geography-test
make icecast-history-test
```

Le test Traefik passe par le DNS public, vérifie TLS, l'accès aux statistiques,
le refus de l'administration anonyme, la racine audio, l'IP effective et le
refus d'un `X-Forwarded-For` forgé ; le MP3 reçu est décodé. Un test depuis le
LAN peut traverser le NAT loopback : dans ce cas l'IP effective est celle du
routeur LAN, sans localisation géographique inventée.

Le test géographique isolé utilise la fixture MaxMind, deux clients et un
proxy déclaré, puis attend qu'un relevé GB/London soit réellement écrit par
le WASM et relu via gRPC. Ses ressources Docker ont un nom unique et sont
retirées ensuite. Il ne remplace pas la base GeoLite du banc habituel.

## Résultats vérifiés le 16 octobre 2026

- 24 tests Rust du socle administratif et du stockage partagé réussis, plus
  le test du vrai WASM exécuté explicitement avec son binaire compilé ; Clippy
  sans avertissements et formatage vérifiés.
- Requêtes historiques communes extraites à l'identique du client existant ;
  migrations et logique d'agrégation du plugin conservées.
- Route Traefik installée, TLS valide, redirection HTTP vers HTTPS, statistiques
  et audio accessibles par le DNS public ; MP3 décodé, administration anonyme
  refusée et en-tête d'IP forgé ignoré.
- Sur le banc isolé, le WASM a écrit un relevé GB/London et le client l'a relu
  via gRPC. Le client non déclaré comme proxy est resté privé.
- Sur le banc réel, un auditeur via Traefik a été enregistré ; les observations
  ont survécu à la recréation d'admin et la collecte a repris. Le même auditeur
  a reçu 192 512 octets MP3 durant le contrôle, sans redémarrage d'Icecast.
- Bascule A/B rejouée avec l'historique actif : même auditeur à 440,2 puis
  880,3 Hz, refus des cibles invalides/absentes, continuité pendant l'arrêt
  administratif, récupération vers B et persistance de B après redémarrage
  du routeur. Icecast n'a pas redémarré ; le statut historique est sans erreur.

État final : services Icecast, administration, sources A/B et routeur actifs ;
point stable sur B. Aucun StationD lancé. Modifications conservées sur `v1-dev`,
sans commit ni push automatique.

## Écoute HTTP pour Second Life — 16 octobre 2026

URL à renseigner : http://v1stream.homestone.rp-radio.live/radio.mp3

Traefik sert directement le MP3 en HTTP sur la racine et ce mount. Le test
a confirmé HTTP 200 sans redirection, le type audio/mpeg et le décodage du
MP3 sur les deux chemins. HTTPS reste fonctionnel et l'administration HTTP
redirige vers HTTPS. Aucun redémarrage d'Icecast, du routeur ou de Traefik
n'a été nécessaire. La lecture dans le viewer Second Life reste à essayer.
