# Proposition de contrat TOML des playlists — version 1

**Statut : proposition à valider, non implémentée.**

Cette proposition s'appuie sur `playlist.rs` et `playlists.md` fournis. Elle conserve
la séparation entre la sélection des contenus et leur diffusion. Elle tranche
les ambiguïtés qui empêchent notamment de décrire une émission intro → épisode →
outro. Les exemples utilisant les nouveautés ne sont pas compatibles avec le
schéma précédemment extrait du Rust actuel.

Le TOML définit déjà la syntaxe. Ce document définit le **contrat de configuration** :
structure, types, valeurs, dépendances et sémantique. Un JSON Schema pourra en
contrôler la partie locale ; le moteur devra contrôler les références et exécuter
les règles de diffusion.

## 1. Principes

1. Un fichier TOML décrit une seule playlist. Aucun conteneur `[[playlist]]`.
2. Les fichiers TOML sont la source de vérité de la configuration, conformément
   au Rust fourni. SQLite conserve l'état d'exécution et un index reconstruisible
   de la configuration. L'historique d'exécution, lui, n'est pas reconstructible
   à partir des TOML et doit être conservé.
3. Une modification par une interface doit passer par le service de gestion et
   être persistée dans le fichier. Aucun deuxième canal de configuration SQLite.
4. Toute clé inconnue, option inapplicable ou combinaison interdite est rejetée.
   Une configuration invalide ne remplace pas la dernière configuration valide.
5. Les chemins de médias sont relatifs à la racine `media/`. Les références de
   playlists sont relatives à la racine des playlists. Les deux espaces sont distincts.
6. La timezone est une configuration obligatoire de la station, sous forme IANA.
   Elle n'est pas répétée dans chaque playlist.

Exemple : `thecronicle/` désigne le dossier `media/thecronicle/`.

## 2. Racine du document

| Champ | Type | Obligatoire | Défaut / règle |
|---|---|---|---|
| `schema_version` | entier | oui | exactement `1` |
| `id` | chaîne UUID | non à la création | attribué par le système, stable ensuite |
| `name` | chaîne | oui | au moins un caractère non blanc |
| `enabled` | booléen | non | `true` |
| `selection` | table | oui | définit la source |
| `broadcast` | table | oui | définit les déclenchements et la diffusion |

Un renommage de fichier conserve son UUID. Les références par chemin doivent être
mises à jour dans la même opération ; une référence devenue introuvable est rejetée.
L'UUID existant n'est pas limité à la version 4 ; le système génère de nouveaux UUID v4.

Les valeurs par défaut sont appliquées par le service, pas par le validateur de schéma.
Une option facultative n'accepte pas `null` : elle est omise dans le TOML.

## 3. Sélection : champs autorisés par mode

| Mode | Champs autorisés en plus de `mode` | Champs obligatoires | Valeurs par défaut |
|---|---|---|---|
| `static` | `files`, `order` | `files` non vide | `order = "sequential"` |
| `dynamic` | `filter`, `match`, `order`, `order_by`, `unplayed_only` | `filter` non vide | `match = "all"`, `order = "shuffle"` |
| `remote` | `url` | `url` | — |
| `queue` | `order`, `max_len` | — | `order = "fifo"`, `max_len = 0` |
| `group` | `strategy`, `members`, `on_member_unavailable` | `strategy`, `members` non vide | `on_member_unavailable = "abort"` |

Un champ appartenant à un autre mode est une erreur, même si sa valeur est vide.
Pour sélectionner toute la bibliothèque en dynamique, le filtre explicite
`field = "path", op = "prefix", value = ""` est permis. L'absence accidentelle de
filtre ne sélectionne donc pas toute la bibliothèque.

### 3.1 Ordre

| Mode | `order` autorisés |
|---|---|
| `static` | `sequential`, `shuffle` |
| `dynamic` | `sequential`, `shuffle`, `newest`, `oldest` |
| `queue` | `fifo`, `lifo` |
| `remote`, `group` | champ interdit |

- `static/sequential` suit l'ordre de `files`.
- `dynamic/sequential` suit le chemin relatif complet en ordre lexical croissant.
- `shuffle` produit une permutation du pool, sans répétition dans un même parcours.
- `fifo` et `lifo` suivent l'ordre d'entrée dans la queue ; une entrée jouée est consommée.
- Les ordres datés sont réservés à `dynamic`.

### 3.2 Ordre des épisodes

`order_by` est obligatoire pour `newest` et `oldest`, interdit autrement.
`unplayed_only` est autorisé uniquement pour ces deux ordres, avec défaut `false`.

| `order_by` | Définition proposée |
|---|---|
| `filename` | tri lexical du nom de fichier, extension comprise ; égalités départagées par chemin relatif complet |
| `mtime` | instant de dernière modification ; égalités départagées par chemin relatif complet |
| `published` | date de publication explicite dans le catalogue ; jamais remplacée silencieusement par `mtime` |

Le tri lexical compare les chaînes Unicode telles que stockées, sensible à la casse,
sans tri naturel des chiffres. Utiliser `ep009`, `ep010` ou `2026-09-13_titre.mp3`.
`newest` prend l'ordre décroissant ; `oldest` l'ordre croissant. Les égalités sur
la clé principale sont départagées par chemin croissant.

Un élément sans la métadonnée de tri requise est exclu, avec diagnostic. Si aucun
élément n'est éligible, la sélection est indisponible. `published` ne fonctionne
que si un importeur renseigne effectivement cette métadonnée.

| Combinaison | Comportement |
|---|---|
| `newest`, `unplayed_only = false` | repartir du plus récent à chaque activation ; utiliser un quota de 1 pour le podcast en cours |
| `oldest`, `unplayed_only = true` | prendre les épisodes non encore diffusés, du plus ancien au plus récent |
| `newest`, `unplayed_only = true` | prendre les épisodes non encore diffusés, du plus récent au plus ancien |
| `oldest`, `unplayed_only = false` | repartir du plus ancien à chaque activation |

Le pool et son ordre sont figés au début du passage. Un nouveau fichier devient
éligible au passage suivant. Une diffusion complète marque l'épisode comme joué ;
un simple échec d'ouverture ne le marque pas. Un épisode interrompu reste éligible.
Le déjà-joué est suivi par UUID de playlist et identité d'épisode du catalogue,
y compris lorsque la playlist joue depuis un groupe.

### 3.3 Chemins et URL

- `files` : tableau non vide de chemins relatifs de fichiers, sans doublon.
- Séparateur canonique `/` ; séparateurs Windows convertis à l'entrée.
- Chemins absolus, lettres de lecteur, segments `..` et octets NUL interdits.
- Le moteur vérifie aussi le confinement après résolution des liens symboliques.
- La casse des chemins médias est conservée ; pas de conversion forcée en minuscules.
- `url` : URL absolue `http` ou `https` avec un hôte, sans identifiants incorporés.
- `max_len` : entier de 0 à 4294967295 ; 0 signifie illimité.

## 4. Filtres dynamiques

Chaque `[[selection.filter]]` contient exactement `field`, `op`, `value`.
`match = "all"` signifie ET ; `match = "any"` signifie OU. Pas de groupes booléens imbriqués en v1.

### 4.1 Catalogue initial proposé

Ce catalogue est une proposition d'implémentation, pas une affirmation sur le
moteur actuel. Les seuls couples illustrés dans le document fourni sont notamment
`path/prefix` et `year/>=`.

| `field` | Type de la donnée | Opérateurs autorisés |
|---|---|---|
| `path` | chemin relatif complet | `eq`, `ne`, `prefix`, `in`, `not_in` |
| `title`, `artist`, `album` | chaîne | `eq`, `ne`, `contains`, `prefix`, `in`, `not_in` |
| `genre` | ensemble de chaînes | `has`, `has_any`, `has_all` |
| `year` | entier de 1 à 9999 | `=`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `not_in` |
| `duration` | durée en secondes, nombre positif ou nul | `=`, `!=`, `<`, `<=`, `>`, `>=` |

| Opérateur | Type de `value` |
|---|---|
| `eq`, `ne`, `contains`, `prefix` | chaîne |
| `has` | chaîne non vide |
| `has_any`, `has_all` | tableau non vide de chaînes non vides |
| `in`, `not_in` | tableau non vide de valeurs du type du champ |
| `=`, `!=`, `<`, `<=`, `>`, `>=` | nombre du type du champ |

Les tableaux ne mélangent pas les types et ne contiennent pas de doublons.
Les comparaisons textuelles sont sensibles à la casse, sans normalisation implicite.
Les chemins sont normalisés avant comparaison. Une métadonnée absente rend le
filtre faux, y compris pour `ne`, `!=` et `not_in`.

`prefix` est un préfixe littéral, pas un glob ni une expression régulière.
Utiliser `thecronicle/` pour cibler ce dossier et ses descendants sans inclure
`thecronicle-old/`. Le chemin vide est permis uniquement comme préfixe de `path`,
pour sélectionner explicitement toute la bibliothèque.

```toml
[[selection.filter]]
field = "path"
op = "prefix"
value = "thecronicle/"
```

## 5. Diffusion et activation

Une **activation** est une demande de diffusion adressée à une playlist. Elle
commence par sa sélection et finit lorsque son quota est atteint, sa source est
épuisée ou un arrêt intervient. Sa durée dépend du contenu et des quotas, pas d'une heure de fin programmée.

### 5.1 Types de participation

| `broadcast.type` | Déclenchement | Champs spécifiques |
|---|---|---|
| `general` | choisie par la rotation générale | `weight`, défaut 15, plage 0 à 50 |
| `interval` | cadence en pistes, durée écoulée ou repères dans chaque heure | exactement un de `every_tracks`, `every_time` ; `alignment` avec `every_time` |
| `scheduled` | une activation par départ programmé | `schedule`, obligatoire et non vide |
| `member` | uniquement à la demande d'un groupe | aucun déclenchement autonome |

`member` est **nouveau**. Il évite d'utiliser une playlist programmée sans horaire
pour simuler une playlist interne à un groupe.

Une playlist d'un autre type peut aussi être référencée par un groupe : elle garde
ses déclenchements autonomes. Ce partage est donc explicite. Une playlist désactivée
n'est disponible ni seule ni comme membre.

Les champs propres à un autre type sont interdits : par exemple `weight` sur
`scheduled`, ou `schedule` sur `member`.

`weight` est un entier de **0 à 50**, avec **15 par défaut**. Cette règle s'applique
au poids de rotation générale (`broadcast.weight`) et au poids d'un membre de
groupe `weighted` (`selection.members[].weight`).

- `50` : priorité relative maximale dans le tirage pondéré.
- `15` : poids normal, utilisé si le champ est omis.
- `0` : exclu de ce tirage ; ce n'est pas une désactivation globale de la playlist.
- Les nombres négatifs, supérieurs à 50 ou non entiers sont rejetés.

Le poids conserve une sémantique de préférence relative : parmi les candidats
éligibles de poids positif, la probabilité de choix est le poids du candidat
divisé par la somme des poids éligibles. Un poids de 50 ne préempte pas une
activation en cours et ne garantit pas de passer avant tous les poids inférieurs.
Si tous les candidats ont un poids nul, aucune sélection n'est effectuée dans ce
pool ; le moteur le signale comme indisponible et rend la main. Dans un groupe,
le poids d'un membre est indépendant de son éventuel poids de rotation autonome.

`every_tracks`, `limit`, `take` restent des entiers de 1 à 4294967295.
Les durées textuelles suivent `[1-9][0-9]*(s|m|h|d)`, avec une seule unité :
`30s`, `15m`, `2h`, `1d`. Pas de durée nulle ou composée en v1.

#### Cadence des intervalles

Avec `every_tracks`, les pistes complètes diffusées par la station incrémentent
le compteur hors de la propre activation de cette playlist. Il repart de zéro à
la fin de son activation. `alignment` est interdit dans ce cas.

Avec `every_time`, le nouveau champ facultatif `alignment` précise l'ancrage :

| `alignment` | Signification | Défaut |
|---|---|---|
| `elapsed` | attendre la durée indiquée après la fin de la précédente activation | oui |
| `hour` | calculer des repères depuis chaque heure pleine, dans le fuseau de la station | non |

**Intervalle écoulé de 15 minutes :**

```toml
[broadcast]
type = "interval"
every_time = "15m"
alignment = "elapsed"
```

Si la précédente activation se termine à 10:07, la prochaine devient due à 10:22.
Au premier chargement, le décompte part de l'activation de la configuration.
L'état est persisté lors des redémarrages ; les échéances dépassées se regroupent
en une seule demande, sans rattrapage en rafale.

**Tous les quarts d'heure, calés sur l'heure pleine :**

```toml
[broadcast]
type = "interval"
every_time = "15m"
alignment = "hour"
```

Les échéances sont 10:00, 10:15, 10:30, 10:45, 11:00, etc., indépendamment de
l'heure d'activation de la configuration et de la durée des passages précédents.

Pour `alignment = "hour"`, `every_time` doit être une durée en minutes entières
de `1m` à `60m`. Les autres unités et valeurs sont rejetées. À chaque heure,
les minutes de déclenchement sont `0, N, 2N, ...`, strictement inférieures à 60.
Le calcul recommence à zéro à l'heure suivante.

| `every_time` | Minutes de déclenchement dans chaque heure |
|---|---|
| `15m` | `:00`, `:15`, `:30`, `:45` |
| `20m` | `:00`, `:20`, `:40` |
| `30m` | `:00`, `:30` |
| `60m` | `:00` |
| `17m` | `:00`, `:17`, `:34`, `:51` |

Si N ne divise pas 60, le dernier intervalle de l'heure est plus court : pour
17 minutes, il y a 9 minutes entre `:51` et l'heure suivante. Pour un espacement
écoulé de 17 minutes après chaque passage, choisir `alignment = "elapsed"`.

Le calage fixe les échéances, pas une coupure forcée de l'antenne : conformément
à l'arbitrage décrit plus loin, la demande attend la fin de l'activation en cours.
Un démarrage tardif ne décale jamais les repères suivants. Il n'y a au plus qu'une
demande en attente par playlist ; plusieurs repères dépassés sont fusionnés.
Les repères atteints pendant la propre activation de cette playlist sont consommés
sans programmer un deuxième passage. Une fin exactement sur un repère est traitée
avant ce repère, qui peut donc déclencher le passage suivant.

À l'activation de la configuration ou après redémarrage, le mode `hour` vise le
prochain repère ; il ne rejoue pas les repères passés. Une occurrence est identifiée
par UUID de playlist, cadence, date locale, heure et minute ; les occurrences déjà
prises en compte sont persistées. Une heure locale inexistante ne déclenche rien ;
une heure répétée ne double pas les occurrences locales.

`alignment` est autorisé uniquement pour `type = "interval"` avec `every_time`.
Il est interdit sur les autres types et avec `every_tracks`.

### 5.2 Quotas et répétition des sources locales

Pour une playlist `static`, `dynamic` ou `queue` autonome :

- `broadcast.limit` vaut 1 par défaut : nombre maximal de pistes par activation.
- `broadcast.repeat` vaut `false`, autorisé uniquement pour `static` et `dynamic`.
- `repeat = true` autorise un nouveau parcours du pool pendant la même activation,
  jusqu'au quota. Il ne crée pas une nouvelle activation.
- `repeat = true` est interdit avec `unplayed_only = true`.
- À l'activation suivante, les sources locales sont à nouveau sélectionnées selon
  leur ordre ; le déjà-joué persiste si `unplayed_only = true`.

Pour une playlist utilisée comme membre, le groupe impose son quota de passage ;
le `limit` autonome du membre est ignoré. Les critères de sélection, les contraintes
et la politique de répétition du membre continuent de s'appliquer.
`limit` est interdit sur `type = "member"` afin de ne pas présenter un quota inutilisé.

Pour une source `remote`, `limit` et `repeat` sont interdits.
`broadcast.max_duration` est obligatoire : durée maximale du relais par activation.
Le relais est arrêté à cette durée, ou plus tôt si son entrée tombe.

Pour un groupe, `limit` et `repeat` sont interdits : sa stratégie définit le passage.
Un groupe référencé par un autre groupe compte comme une unité complète, pas comme
un nombre de pistes qui risquerait de couper l'outro.

### 5.3 Épuisement et contraintes

`broadcast.on_exhausted` vaut `fallthrough` par défaut.

| Valeur | Résultat |
|---|---|
| `fallthrough` | terminer l'activation et rendre la main |
| `disable` | terminer et suspendre les prochaines activations dans l'état d'exécution, jusqu'à réarmement explicite |

Proposition : retirer `stop` et `hold` de la v1 tant que leurs effets sur l'antenne
et leurs conditions de sortie ne sont pas spécifiés. `disable` ne réécrit pas le
TOML : `enabled` reste l'intention de configuration et la suspension est un état
visible, avec sa raison.

Sur un groupe, `on_exhausted` s'applique à l'impossibilité de produire le passage,
pas à sa fin normale. Sur un membre indisponible, sa politique propre est appliquée,
puis le groupe suit `on_member_unavailable`.

`broadcast.constraints` est autorisé pour `static`, `dynamic` et `queue` :

- `no_same_artist_within` : durée ;
- `no_same_track_within` : durée.

Ces exclusions sont évaluées avant chaque piste, y compris en groupe. Elles portent
sur l'historique de toute la station et ne sont jamais relâchées implicitement.
L'identité de piste vient du catalogue ; celle de l'artiste est la valeur normalisée
par le catalogue. Un artiste absent ne provoque pas d'exclusion d'artiste.
S'il manque des pistes éligibles, le quota peut ne pas être atteint ; le passage
se termine selon les politiques d'indisponibilité. Une entrée de queue exclue reste
présente et le moteur cherche la prochaine entrée éligible dans l'ordre déclaré.

## 6. Groupes

Chaque élément de `selection.members` contient `ref` et éventuellement `weight` ou `take`.

| Stratégie | Comportement d'une activation | Options des membres |
|---|---|---|
| `sequence` | parcourir tous les membres, dans l'ordre déclaré, une seule fois | `take`, défaut 1 ; `weight` interdit |
| `rotate` | jouer un passage du prochain membre ; position persistée entre activations | `take` et `weight` interdits |
| `weighted` | tirer un membre disponible au hasard, puis jouer un passage | `weight`, défaut 15, plage 0 à 50 ; `take` interdit |

Un passage d'un membre feuille est une piste, ou un relais limité par `max_duration`.
Un passage d'un membre groupe est une activation complète de ce groupe.
Ainsi `take = 2` sur un groupe enfant exécute deux séquences complètes.
Pour `rotate` et `weighted`, un seul passage est demandé par activation.

Une référence est un chemin relatif sans extension `.toml`. Pour rester cohérent
avec le Rust fourni, sa résolution est insensible à la casse. Deux fichiers dont
les références normalisées sont identiques sont rejetés. Les références inconnues,
les chemins non sûrs et les cycles sont également rejetés avant application.

### Membre indisponible

Nouveau champ : `selection.on_member_unavailable`.

- `abort` (défaut) : abandonner le reste de la séquence, avec diagnostic.
- `skip` : passer au membre suivant ; aucun silence artificiel ni attente illimitée.

En `sequence/abort`, le moteur vérifie avant le début du groupe qu'un plan de
sélection peut fournir les quotas demandés, contraintes et répétitions comprises.
Il simule l'ordre du passage pour les contraintes entre ses propres pistes.
Si le podcast est déjà absent, l'intro n'est pas jouée. Cette vérification ne
peut pas empêcher une disparition de fichier ou une panne réseau après démarrage :
dans ce cas le reste est abandonné et l'incident est signalé.

En `rotate`, `skip` cherche le prochain membre disponible au plus une fois par
membre ; `abort` termine sur le premier membre indisponible. La position avance
après les membres examinés. En `weighted`, le tirage porte sur les membres
éligibles de poids strictement positif ; un échec après tirage termine l'activation, sans nouvelle boucle de tirage.

Si aucun membre ne peut produire, le groupe est épuisé. Un passage réussi se
termine normalement, même si tous ses membres ont été parcourus.

## 7. Départs programmés

Chaque `[[broadcast.schedule]]` définit une heure de déclenchement, sans heure de fin.
La durée du segment résulte de son contenu : elle n'a pas à être connue à l'avance.

| Champ | Type | Règle |
|---|---|---|
| `start` | chaîne | obligatoire, `HH:MM`, de `00:00` à `23:59` |
| `days` | tableau de chaînes | obligatoire, non vide, sans doublon : `mon` à `sun` |
| `date_start` | chaîne date | facultative, date civile valide `YYYY-MM-DD`, inclusive |
| `date_end` | chaîne date | facultative, même format, inclusive et ≥ `date_start` |

**Le champ `end` est supprimé et interdit.** Il n'y a ni heure de coupure ni fenêtre
de démarrage à calculer à partir d'une durée supposée.
`date_end` reste autorisé : c'est le dernier jour où la programmation s'applique,
pas la fin d'un segment à l'antenne.

- À `start`, une seule demande d'activation est émise pour cette occurrence.
- Si la station est occupée, cette demande attend la fin de l'activation en cours,
  conformément à l'arbitrage ci-dessous ; le retard est signalé.
- Une fois démarrée, l'activation va jusqu'à son terme normal : quota de pistes,
  fin de la séquence du groupe ou épuisement de la source.
- Un groupe joue sa séquence complète, outro comprise, quelle que soit sa durée.
- La fin du segment ne déclenche pas un nouveau passage de la même occurrence.
- Un segment peut finir le lendemain. `days` et les bornes de dates désignent le
  jour du départ programmé, pas celui de la fin effective.
- Deux règles produisant le même départ pour la même playlist et la même date
  sont rejetées comme doublons. Aucun chevauchement de durées n'est calculé,
  puisque les durées sont inconnues.

L'occurrence est identifiée par UUID de playlist, date locale et heure de départ.
Le moteur persiste sa prise en compte et son démarrage avant diffusion. Après
redémarrage, une occurrence déjà démarrée n'est pas rejouée automatiquement.
Les départs survenus pendant l'arrêt de la station sont signalés comme manqués,
sans rattrapage automatique. Une demande déjà en attente avant l'arrêt reste
persistée ; si plusieurs demandes de la même playlist s'accumulent, seule la plus
récente est conservée et les précédentes sont signalées comme remplacées.

Heure d'été : une heure locale inexistante entraîne une occurrence manquée ;
une heure répétée ne produit qu'une occurrence, ancrée au premier instant.

Arbitrage minimal proposé : aucune préemption d'une activation en cours ; les
activations programmées dues passent avant les intervalles dus, puis la rotation
générale. À priorité égale, échéance la plus ancienne, puis UUID pour départager.
Une playlist déjà active n'est pas lancée une seconde fois en parallèle.

## 8. Exemple : Homestone chronicles

Quatre fichiers à la racine des playlists. Les chemins de génériques et l'heure de départ
ci-dessous sont des exemples à adapter. Les épisodes ont des noms chronologiques
et résident seuls dans `media/thecronicle/`.

### `homestone-chronicles.toml`

```toml
schema_version = 1
name = "homestone chronicles"
enabled = true

[selection]
mode = "group"
strategy = "sequence"
on_member_unavailable = "abort"
members = [
  { ref = "homestone-chronicles-intro", take = 1 },
  { ref = "homestone-chronicles-podcast", take = 1 },
  { ref = "homestone-chronicles-outro", take = 1 },
]

[broadcast]
type = "scheduled"

[[broadcast.schedule]]
start = "20:00"
days = ["sun"]
```

Le dimanche à 20:00, le groupe devient dû. Il joue l'intro, le dernier podcast,
puis l'outro, et rend la main. Aucune heure de fin n'est requise : la durée dépend
des fichiers effectivement joués. Si l'antenne est occupée à 20:00, son départ
attend selon la règle d'arbitrage proposée ; l'heure effective est journalisée.

### `homestone-chronicles-podcast.toml`

```toml
schema_version = 1
name = "Homestone chronicles - podcast en cours"

[selection]
mode = "dynamic"
order = "newest"
order_by = "filename"
unplayed_only = false
match = "all"

[[selection.filter]]
field = "path"
op = "prefix"
value = "thecronicle/"

[broadcast]
type = "member"
```

### `homestone-chronicles-intro.toml`

```toml
schema_version = 1
name = "Homestone chronicles - intro"

[selection]
mode = "static"
order = "sequential"
files = ["jingles/homestone-chronicles-intro.mp3"]

[broadcast]
type = "member"
```

### `homestone-chronicles-outro.toml`

```toml
schema_version = 1
name = "Homestone chronicles - outro"

[selection]
mode = "static"
order = "sequential"
files = ["jingles/homestone-chronicles-outro.mp3"]

[broadcast]
type = "member"
```

Pas de `repeat = true` nécessaire pour rejouer un générique à l'émission suivante :
chaque nouvelle activation sélectionne à nouveau son fichier. `repeat` concerne
uniquement le parcours supplémentaire pendant une même activation.

## 9. Répartition des validations

| Niveau | Contrôles |
|---|---|
| Parseur TOML | syntaxe, types TOML, doublons de clés |
| JSON Schema Draft 4 | clés autorisées, types, champs requis, enums, bornes, dépendances mode/ordre/stratégie, couples field/op/value |
| Service, fichier seul | dates civiles, bornes de dates, doublons de départ, chemins et URL, règles impossibles à exprimer proprement dans le schéma |
| Service, ensemble des fichiers | UUID uniques, références normalisées uniques, résolution des membres, absence de cycles |
| Moteur | disponibilité des médias, métadonnées, quotas, historique, contraintes, occurrences et incidents de lecture |

Les erreurs doivent inclure le fichier, le chemin logique du champ, la valeur
rejetée et les valeurs attendues quand elles sont finies. Exemple :
`selection.order : "shuffle" interdit pour mode "queue" ; attendu : fifo ou lifo`.

Un schéma ne prouve ni la présence des fichiers médias ni la bonne exécution du
séquenceur. Le service doit exposer distinctement configuration valide, source
indisponible et erreur de diffusion.

## 10. Écarts à implémenter par rapport au Rust fourni

| Sujet | Rust fourni | Proposition |
|---|---|---|
| Version | absente | `schema_version = 1` obligatoire |
| Membre exclusif | absent | `broadcast.type = "member"` |
| Filtres | `field/op` libres, `value` quelconque | catalogue typé et validé |
| Champs par mode | contrôles partiels | rejet exhaustif des champs inapplicables |
| Dates, durées, heures | chaînes libres | formats et sémantique explicites |
| Ordres et quotas | nombreuses options sans défaut explicite | défauts documentés et appliqués par le service |
| Poids | `u32` facultatif sans borne métier | entier de 0 à 50, défaut 15 ; 0 exclut du tirage, 50 est la priorité relative maximale |
| Relais | URL présente, pas de quota temporel | URL contrôlée, `max_duration` obligatoire |
| Épuisement | quatre enums | deux comportements définis : `fallthrough`, `disable` |
| Groupe indisponible | politique absente | `on_member_unavailable = "abort"` ou `"skip"` |
| Intervalles temporels | `every_time` sans ancrage explicite | `alignment = "elapsed"` ou `"hour"`, repères horaires indépendants des passages |
| Programme horaire | `start` et `end` obligatoires | suppression de `end` ; départ unique persisté, fin déterminée par le contenu |
| Références | résolution et cycles présents | ajouter détection des collisions de références et UUID |

Cette proposition nécessite une évolution du modèle Rust, du contrat gRPC, du
séquenceur et du JSON Schema. Elle ne constitue pas un patch. Les fichiers existants
sans version doivent être traités comme format historique ; leur conversion doit
être explicite, notamment parce que la répétition et les départs programmés changent de sens.

Le premier périmètre recommandé est : format individuel, mode `member`, filtres
`path/prefix`, épisode `newest`, groupe `sequence`, puis occurrences programmées.
Les autres modes doivent conserver leur contrat actuel jusqu'à leur mise en
conformité explicite ; ne pas annoncer une prise en charge complète de la v1 avant cela.
