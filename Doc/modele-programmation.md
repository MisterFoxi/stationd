# Modèle de programmation (grille) — décisions d'architecture

Document de référence, complément du doc d'architecture général.
Traite du cœur de ce qui distingue la réécriture d'AzuraCast : **comment la
station décide quoi jouer à l'instant `now`**. Récapitule les décisions
prises, pas le détail d'implémentation.

## Cadrage : ce n'est pas un choix de lib « scheduler »

Le réflexe « quelle crate de scheduling ? » est un faux problème. Aucune lib
cron ne modélise les concepts de programmation *radio* (rotation pondérée sans
heure, insertion « toutes les N pistes », top d'heure calé montre). Ce sont
des concepts métier, pas du scheduling système.

Décision : **un seul résolveur maison dans `stationd`**, fonction (quasi pure
+ état de lecture) du type `resolve_next(now, état) -> Décision`. Pas de lib
cron pour la grille. La grille est une donnée en SQLite, éditable via le
contrat gRPC (donc atteignable au CLI — cohérent avec la garantie de
complétude). Une lib de jobs n'intervient que pour les tâches de maintenance
(voir dernière section), qui sont un problème séparé.

## Frontière avec Liquidsoap

Rappel des contraintes qui tranchent : `stationd` est *single writer* sur
Liquidsoap, la source de vérité de la grille est SQLite, et toute
fonctionnalité doit exister dans le contrat gRPC.

Conséquence : **Liquidsoap est exécutant, pas ordonnanceur.** On n'utilise ni
son day-parting natif (`switch` + prédicats `{20h-22h}`), ni son `rotate`
« un jingle toutes les N ». Le faire dédoublerait la source de vérité et
sortirait la logique du contrat gRPC — exactement le mélange qui rend
AzuraCast confus.

Modèle retenu :

- Liquidsoap tourne en `request.dynamic` et **redemande « next ? » à
  `stationd` à chaque frontière de piste**. C'est le résolveur qui répond.
- Liquidsoap ne conserve qu'un **`fallback` de sécurité** vers une rotation
  par défaut, pour qu'un crash ou un restart de `stationd` ne produise jamais
  de blanc.

Toute la logique de programmation reste donc en Rust : testable, dans le
proto, atteignable au CLI.

## Les quatre familles de règles

L'usage réel combine plusieurs natures de règles. Elles diffèrent par leur
**point d'ancrage** (ce qui déclenche l'évaluation) :

| Type | Ancrage | Rôle |
|---|---|---|
| `BaseRotation` | aucun — toujours résolvable | le plancher : la rotation pondérée qui tourne quand rien d'autre ne s'applique |
| `DayPart` (fixe) | plage horaire | sélectionne quelle base est active sur un créneau |
| `AtClock` (top d'heure / TOPTH) | horloge murale (XX:00, XX:15…) | rendez-vous absolu, indépendant de ce qui précède |
| `Every` (cadencé) | dernier passage (temps écoulé **ou** compteur de pistes) | cooldown : « au moins X depuis ma dernière diffusion » |

Deux clarifications importantes issues des échanges :

**Sélection vs injection.** Les règles ne font pas toutes la même chose.
`DayPart` et `BaseRotation` *résolvent quelle source de base est active* — ils
répondent à « quel est le plancher maintenant ». `AtClock` et `Every`
n'écrasent pas la base : ils la *ponctuent* (injection ponctuelle, la base
reprend derrière).

**Le plancher est un type à part entière.** Il faut une source qui tourne
*toujours*, priorité la plus basse, toujours résolvable, sinon on a des trous
entre les règles. AzuraCast l'a implicitement (« General Rotation ») ; on
l'explicite (`BaseRotation`).

**Fixe et override daté = la même règle.** Un day-parting hebdomadaire et une
émission spéciale un lundi précis sont le *même mécanisme* (une plage qui
sélectionne une base), avec seulement une **portée de validité** différente :
récurrente vs bornée à des dates. Un seul type (`DayPart`), deux formes de
validité. Ça simplifie le proto.

## `AtClock` vs `Every` : deux ancrages, à ne jamais fusionner

Le piège d'AzuraCast est un champ « intervalle » unique interprété tantôt
comme délai, tantôt comme horloge. On les nomme distinctement.

|  | `Every` (cadencé) | `AtClock` (calé horloge) |
|---|---|---|
| Référence | `last_played_at` ou `tracks_since` (glissante) | l'horloge, absolue |
| État à persister | **oui** — dernier passage / compteur | non — recalculable depuis `now` seul |
| Effet d'un restart | doit reprendre le compteur (sinon cadence cassée) | rien à reprendre, se recale seul |
| Rattrapage après un trou | « en retard → j'insère dès que possible » | **choix explicite par règle** (voir péremption) |

Les deux cohabitent sans se marcher dessus parce qu'ils **ne lisent pas la
même source de vérité** : l'un la montre, l'autre l'état de lecture persisté.

## `soft` vs `hard`

Knob à prévoir dès le schéma, là où il a du sens (surtout `AtClock`) :

- **soft** — attend la fin de la piste en cours, s'insère à la prochaine
  frontière. Cas du jingle station.
- **hard** — préempte (coupe/fond) pour taper l'instant cible pile. Cas du
  news en tête d'heure.

`Every` est par nature soft (évalué en frontière de piste).

## Péremption / tolérance des `AtClock`

Le rattrapage d'un `AtClock` manqué est le piège classique. Si `stationd`
redémarre à XX:04 et que le top de XX:00 n'a pas été joué : on le joue en
retard, ou on le considère périmé et on attend XX:15 ?

Décision : **par règle, pas globalement.** Un flag de tolérance/péremption sur
les règles `AtClock`. Un ID de news périmé à 4 min → poubelle ; un jingle
station → encore bon. Chaque règle porte sa fenêtre de validité.

## Ordre de résolution (collisions)

Un `AtClock` hard et un `Every` dû peuvent tomber sur la même frontière. À
priorité égale, c'est l'ambiguïté d'AzuraCast. Ordre fixe, documenté :

```
AtClock hard  >  AtClock soft  >  Every  >  base (DayPart → BaseRotation)
```

Commencer par cet ordre fixe (suffisant, prévisible, se raisonne). Un
`priority: i32` explicite par règle est possible plus tard si un besoin de
finesse apparaît — pas au départ.

## Boucle du résolveur

`resolve_next` est réveillé par **deux horloges** :

1. **Frontières de piste** — Liquidsoap demande « next ? ». On évalue :
   `AtClock` soft dus → `Every` satisfaits → sinon on tire dans la base active
   (le `DayPart` qui couvre `now`, sinon `BaseRotation`).
2. **Timer mural** — pour les `AtClock` **hard** qui doivent préempter même au
   milieu d'une piste (news à XX:00:00). Calculé par `sleep_until` jusqu'à la
   prochaine frontière horloge, puis recalcul.

Pas de boucle cron périodique : on calcule l'instant de la prochaine frontière
et on dort jusque-là. Précis, introspectable, pas de dérive.

## État runtime à persister

Pas seulement les règles — aussi l'**état de lecture** en SQLite : compteurs
`tracks_since` et `last_played_at` par règle `Every`. Sinon un restart remet à
zéro « 3 pistes depuis le dernier jingle » et casse la cadence.

## Réconciliation au démarrage

Le résolveur doit être **idempotent / catch-up**, pas seulement réactif. Au
démarrage (ou après un trou), `stationd` calcule « qu'est-ce qui devrait jouer
*maintenant* » à partir des règles + état persisté, et se resynchronise — il
n'attend pas passivement la prochaine transition.

## Datetime

**`jiff`** pour la gestion des fuseaux et de l'heure d'été (DST), dans
l'esprit « échec explicite » du projet. Une émission « 20h » est en heure
locale ; les transitions DST cassent les calculs naïfs sur `chrono` brut. À
défaut, `chrono-tz`.

## Tâches de maintenance (problème séparé)

Scan de bibliothèque, **retry d'écriture de métadonnées NFS** (point de
vigilance du doc général), nettoyage, récupérations diverses. Ici on veut de
la **durabilité et des retries**, pas de la précision temps réel — donc une
lib de jobs, pas le résolveur.

Choix par défaut : **`apalis`** (file de jobs, backend SQLite, retries, jobs
différés). L'écriture NFS devient un job durable qui survit à un restart :
écriture → vérification post-écriture → retry avec backoff. `tokio-cron-
scheduler` reste une option plus légère si les tâches sont purement
périodiques et qu'on accepte de perdre l'état des jobs en cours à un restart.

## Encore ouvert

- Schéma exact du proto pour une règle : `kind` (`AtClock` / `Every` /
  `DayPart` / `BaseRotation`), portée de validité (toujours / plage récurrente
  / bornée à des dates), mode `soft|hard`, flag de péremption. **C'est le
  premier proto à écrire ; le reste du scheduling en découle.**
- Pour `Every` : unité de la cadence (temps écoulé *et* compteur de pistes —
  les deux sont nécessaires, à modéliser sans les confondre).
- Modèle de pondération de `BaseRotation` (poids, anti-répétition,
  artist/title separation).
- Articulation avec les hooks plugins (`stationd`) : un plugin peut-il
  proposer/filtrer une sélection ? — renvoie à la question des points
  d'accroche encore ouverte dans le doc général.
