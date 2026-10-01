# Veille selon l'âge des connexions Icecast

Les plugins natif `stop-when-idle` et WASM `stop-when-idle-wasm` peuvent considérer comme inactives des connexions
restées ouvertes longtemps. Cette heuristique ne distingue pas un appareil oublié
d'une personne qui écoute longtemps : le seuil doit rester assez grand.

## Configuration

```toml
[icecast]
# Garder les paramètres admin existants.
listener_snapshots = true

[[plugin]]
name = "stop-when-idle-wasm"
wasm = "plugins/stop-when-idle-wasm/target/wasm32-unknown-unknown/release/stop_when_idle_wasm.wasm"
enabled = true
capabilities = ["control"]

[plugin.config]
max_connection_age = "12h"
min_zero_samples = 2
```

`max_connection_age` est optionnelle, désactivée par omission. Elle accepte une
durée positive en s, m, h ou d. Le plugin vérifie au chargement que la collecte
détaillée Icecast est activée, via l'API hôte générique. Sinon il passe à Failed
avec une explication ; stationd peut démarrer. config.rs ne connaît ni son nom
ni ses options. Le natif accepte les mêmes options :
remplacer le nom par `stop-when-idle` et retirer le chemin `wasm`.
Activer une seule de ces deux versions. Sans `max_connection_age`, chacune
conserve la politique à zéro auditeur. Ne pas activer en parallèle une autre
politique qui réveille sur count > 0.

## Collecte et confidentialité

Réutiliser la boucle `listclients` déjà utilisée par les statistiques.
Aucune deuxième requête n'est ajoutée. Après chaque tour complet sur les mounts
distincts, publier `ConnectionsSampled` et actualiser la vue hôte
`listener_connections`. Chaque entrée contient seulement :

- `mount` : scope de l'identifiant ;
- `id` : identifiant de connexion Icecast, sans identité persistante ;
- `connected_seconds` : durée rapportée par Icecast.

Les IP et user-agents restent dans le circuit existant de statistiques, protégé
par `listener_details`. La vue de veille ne les copie pas. Les identifiants,
durées et baseline de veille restent en mémoire, sans table de sessions.
Le journal borné en mémoire expose désormais les débuts et fins de connexion
à la demande de l'opérateur, avec seulement mount, id, durées et horodatages.
Aucune IP ni aucun user-agent ne figure dans ces événements.
La baseline de veille est conservée jusqu'au réveil ou à une reprise opérateur.

Une réponse manquante ou invalide sur un mount invalide le relevé global :
`None` / JSON null signifie inconnu ; une liste vide signifie zéro connexion
observée avec succès. Un relevé périme après deux périodes de collecte normale,
comptées depuis le début de sa collecte, avec une horloge monotone.

## Mise en veille

Le plugin ignore les événements de compteur quand l'option est active.
Il consulte la vue hôte actuelle à chaque `ConnectionsSampled`, pour ne pas
prendre une décision à partir d'un événement ancien en file d'attente.
Un relevé est éligible si toutes les connexions ont une durée supérieure ou
égale au seuil ; le relevé vide est éligible. Une connexion récente ou un relevé
inconnu remet la série à zéro. `min_zero_samples` désigne alors le nombre de
relevés éligibles consécutifs.

Le plugin arme une seule fois par période inactive. Le core conserve le seuil
comme condition du drain et revérifie au prochain bord de piste :
audience connue, relevé complet et frais, toutes les connexions assez anciennes.
Une arrivée entre l'armement et le bord empêche donc l'endormissement.
L'âge reste celui fourni par Icecast : aucun timer individuel à persister.

## Réveil et contrôle opérateur

À l'entrée en veille, le core capture les couples (mount, id) et leurs durées.
Les connexions présentes à cet instant restent servies par le bruit de fond.
Un couple absent de la baseline, ou une durée redevenue inférieure à sa durée
initiale (réutilisation d'id), provoque `Wake`. Une disparition seule ne
réveille pas. Les IP et user-agents ne participent à aucune comparaison.

Une collecte détaillée devenue inconnue réveille également cette veille.
Les réveils existants sur audience inconnue ou prise d'antenne DJ restent actifs.
`poll_interval_sleeping` contrôle la fréquence des relevés pendant la veille.

`Resume` et `Pause` annulent le garde d'âge. La veille manuelle
`station stop-when-idle` conserve sa condition zéro auditeur.
Un nouvel auditeur ne dépause jamais la station et ne relance jamais stationd
après un arrêt opérateur. Au chargement ou rechargement du plugin avec l'option
active, une station sleeping est réveillée par prudence : la baseline de
connexion n'est pas persistée. Elle pourra se rendormir sur de nouveaux relevés.

Aucun client n'est expulsé : une reconnexion automatique déclencherait sinon
une succession de coupures et de réveils.

## API hôte

Native : `Host::listener_connections()` retourne la vue fraîche optionnelle ;
`Host::stop_when_connections_old(max_age_seconds)` nécessite `control`.

WASM : `listener_connections("{}")` retourne
`{"ok":true,"enabled":true,"connections":[...]}` ou une liste null si inconnue.
`enabled` indique la disponibilité de la collecte, indépendamment de la réussite
du dernier relevé. Il permet au plugin de vérifier lui-même son prérequis.
`station_control({"action":"stop_when_idle","max_connection_age":43200})`
arme le garde d'âge (secondes positives). La clé est refusée sur les autres
actions. Les autres formes de `station_control` restent compatibles.
Le runtime appelle aussi l'export WASM optionnel `on_load` : le guest valide
sa configuration, initialise sa série de relevés en mémoire et applique la
récupération prudente en mode âge. Une erreur de chargement rend le plugin Failed.

Pour compiler le guest : `make plugins P=stop-when-idle-wasm`.
Le test avec le vrai WASM est exécuté après compilation :
`cargo test --lib wasm_connection_age_matches_native_policy -- --ignored`.

## Suivre les débuts et fins de connexion

Avec `[icecast] listener_snapshots = true`, le journal expose
`connection_started` et `connection_ended`, indépendamment du plugin actif :

```bash
make ctl A="events --last 20 --follow"
```

Les lignes indiquent le mount, l'id, le début estimé et la durée connue. Le début
est estimé à partir de Connected fourni par Icecast : une connexion déjà ouverte
au démarrage apparaît avec son âge réel, pas comme une nouvelle écoute à cet instant.
La fin est détectée au premier relevé complet où la connexion a disparu.
`last_seen` et `end_observed` bornent donc l'instant de fin ; la durée affichée
est un minimum, celle du dernier relevé où la connexion était présente.

Un échec de collecte ne produit pas de fausses fins. Le relevé valide suivant
reprend la comparaison. Une réutilisation d'id avec une durée redevenue plus
courte termine l'ancienne session observée puis annonce la nouvelle.
La comparaison est faite par (mount, id), sans IP ni user-agent.
Une connexion ouverte puis fermée entre deux relevés peut ne pas être vue.
Le journal garde au maximum 2000 événements en mémoire et se vide au redémarrage.


## Indication du mode actif

Le chargement réussi du plugin publie une indication opérateur typée :
« veille automatique active », avec le seuil d'âge ou la condition zéro auditeur.
La TUI l'affiche dans son bandeau dès la première lecture de l'état des plugins,
et dans l'écran Contrôle. Le journal contient aussi l'événement d'activation.
`stationctl plugin list` expose cette indication.

Cette indication reste présente tant que le plugin est chargé. Elle disparaît
à l'arrêt du plugin, en cas d'échec ou de quarantaine. Elle est publiée par le
plugin via la fonction hôte `operator_notice`, pas déduite de son nom, de son
chemin WASM ou de sa configuration par le core ou la TUI.

« Veille automatique active » signifie que la règle surveille les connexions.
« Veille armée » correspond à DRAINING : les conditions ont été atteintes et
le core attend un bord de piste où elles restent remplies. Le mode actif ne
fait donc pas passer la station en DRAINING dès le chargement.

