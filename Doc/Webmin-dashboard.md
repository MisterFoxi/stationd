# Webmin — dashboard réseau (tranche C)

Après connexion, l'accueil affiche uniquement les stations autorisées :
état de diffusion, média en cours, audience et alertes. Cliquer sur leur nom
ouvre le détail de la station, dont la cible reste visible dans le titre.

Les mises à jour arrivent automatiquement. Aucun rechargement manuel n'est
nécessaire lors d'un changement de morceau ou d'audience.

## Lire les informations

- Station connectée : le RPC de présence de StationD répond.
- Station inaccessible : dernière tentative de présence en échec. Les
  informations d'antenne déjà reçues restent visibles comme données périmées.
- Audience inconnue : Icecast est absent, inaccessible, en erreur ou sa
  dernière lecture est trop ancienne. Une inconnue n'est jamais remplacée
  par zéro. `0 auditeurs` est une mesure connue.
- Connexion perdue : le navigateur a perdu son flux Webmin. La reconnexion
  est automatique ; les données affichées doivent être considérées comme
  anciennes jusqu'au retour du flux.

Le détail distingue le média préchargé, confirmé, des médias suivants,
calculés par simulation. Les notes expliquent les causes d'incertitude.
Il présente aussi la programmation à venir et les lectures des services :
présence StationD, erreurs signalées par le pont Liquidsoap, dernière
lecture Icecast et éventuels échecs de plugins. Il ne constitue pas une
inspection des processus du système d'exploitation.

Les contrôles de lecture n'exécutent aucune commande de diffusion.
La console et les actions restent les tranches suivantes.

## Plusieurs stations

Déclarer les stations dans le catalogue du plugin avec leurs IDs stables,
libellés et endpoints gRPC. Attribuer ensuite les droits au compte avec
`remote-user role NOM --station ID viewer|helper|admin`.
Une station inaccessible ne bloque pas les autres.

Les endpoints restent côté serveur. Par défaut, un endpoint HTTP doit être en loopback,
pour la station locale ou un tunnel. Une station distante peut utiliser
HTTPS avec un certificat accepté par le magasin de confiance du serveur.
La vérification TLS n'est jamais désactivée. Le transport opérateur doit
rester privé et réservé au gateway ; ne pas ouvrir le gRPC sur Internet.
Un certificat privé doit être installé dans le magasin système ou le
transport ramené à un tunnel loopback. La gestion des tunnels distants
n'est pas automatisée par cette tranche.

Sur un réseau interne de confiance, le HTTP distant peut être autorisé
explicitement pour chaque station (IP ou nom DNS). Cette option ne crée
aucune exposition publique et ne modifie pas l'écoute gRPC de la station cible :

```toml
[[plugin.config.stations]]
id = "home-stone"
label = "Home Stone EU"
grpc_endpoint = "http://EU-HomeStone.lan:50051"
allow_plaintext_grpc = true
```

La station cible doit écouter sur son adresse LAN et son pare-feu doit
permettre l'accès au port gRPC depuis le gateway. Aucun routage public ni
publication de ce port dans Traefik n'est nécessaire.

## Flux partagés et limites

Le gateway utilise un seul `OnAirService.Watch` par station configurée.
Les navigateurs partagent son dernier instantané : ils ne lancent pas de
simulation supplémentaire. Les lectures de présence et de services sont
mutualisées toutes les 5 secondes, avec un délai de 4 secondes par RPC.
Le flux d'antenne attend jusqu'à 20 secondes le premier instantané, puis
reconnecte après 45 secondes sans données. La reconnexion recule de 1 à
15 secondes. Un instantané observé ou reçu depuis plus de 45 secondes est
signalé comme périmé. Les horloges des stations doivent être synchronisées.

Dans `[plugin.config]` :

```toml
max_event_streams = 64
```

La valeur accepte 1 à 256 flux SSE simultanés. Chaque navigateur garde au
plus une trame en attente ; un client lent ne bloque pas les stations.
Le serveur vérifie la session et les droits toutes les secondes avant
chaque publication, puis ferme le flux après expiration, révocation ou
purge. Chaque connexion dure au maximum une heure ; le navigateur la
rouvre automatiquement avec une nouvelle vérification de la session.

L'arrêt du plugin ferme ses connexions et annule les tâches du runtime
HTTP dédié. Les DTO n'exposent ni endpoints, chemins de médias, sockets,
configurations, erreurs brutes de services ni données de credentials.

## Routes de lecture

| Route | Résultat |
|---|---|
| `/api/stations` | Synthèse des stations autorisées |
| `/api/network/events` | Flux SSE de la synthèse |
| `/station/ID` | Écran de détail |
| `/api/stations/ID` | Instantané du détail |
| `/api/stations/ID/events` | Flux SSE du détail |

Toutes ces routes exigent une session. Le détail et son flux contrôlent
également les droits sur la station demandée. Les flux envoient un événement
`snapshot` contenant un instantané complet et un événement `session-ended`
si la session ou ses droits deviennent invalides. Aucun token de session
n'est placé dans l'URL.

## Vérification

```sh
make webmin-test
```

Le test facultatif suivant compare l'adaptateur à une station déjà lancée,
sans créer de compte ni modifier les données :

```sh
docker compose exec -u dev -e STATIOND_WEBMIN_TEST_GRPC=http://127.0.0.1:50051 station \
  cargo test --locked --lib webmin_live_running_station_smoke -- --ignored --nocapture
make ctl A="onair --upcoming 3 --history 1 --playlists 2"
make ctl A="icecast status"
```

Les tests couvrent les ACL, les inconnues, les données périmées, la fermeture
après purge, les quotas SSE, un lecteur lent, le partage du flux, la
reconnexion après rupture et le refus d'un certificat TLS non fiable.
L'interface a été vérifiée dans un aperçu local, sur ordinateur et à 360 px.
