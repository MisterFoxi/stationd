## Gestion du temps

Principe : **epoch UTC en interne, conversion uniquement à la présentation.**
Les maths de temps (comparaisons, ordonnancement des interrupts, « ce titre
finit-il avant la borne ») se font sur une droite monotone, sans heure qui
existe deux fois ou zéro fois. L'ambiguïté DST ne peut pas vivre dans le
core ; elle est repoussée à la seule frontière où elle a un sens : la
saisie/affichage.

Deux natures de temps distinctes, portées par deux types distincts :

- **Instants** (one-shot daté, top horaire, log de diffusion, échantillon
  listeners) → epoch UTC. Aucune ambiguïté. Majorité des données.
- **Règles récurrentes** de la grille (« jazz 08:00, lun–ven ») →
  `(heure civile, fuseau IANA, récurrence)`. Une règle en wall-clock ne se
  stocke PAS en offset gelé (`08:00 Europe/Paris` = 07:00 UTC l'hiver, 06:00
  l'été) : on conserve le nom de fuseau IANA, et l'expansion règle → instants
  epoch se fait à la volée en appliquant le DST en vigueur pour chaque
  occurrence.

Représentation Rust :

- Jamais d'`i64` epoch nu (temporel « stringly-typed » → réintroduit l'échec
  silencieux : addition de deux timestamps, s/ms mélangés). Type wrappé
  obligatoire.
- **`std::time::Instant` (horloge monotone) réservé aux mesures de durée**
  (échantillonnage, timeouts, « dans N s ») — jamais pour pointer un instant
  du calendrier (non sérialisable, pas de date). Wall-clock/epoch = « quand » ;
  monotone = « depuis combien de temps ».
- Crate temps à choisir (statut = décision de stack, cf. tableau) : la
  séparation instant/règle mappe sur `jiff` (`Timestamp` vs `Zoned`/`civil`)
  ou `chrono` + `chrono-tz` (`DateTime<Utc>` vs civil + `Tz`). À trancher.

Frontières :

- Fil gRPC : `google.protobuf.Timestamp` pour les instants ; message
  `{ heure civile, fuseau IANA, récurrence }` pour les règles (les deux
  doivent exister au contrat, cf. CLI complet).
- SQLite : epoch en `INTEGER` ; règle récurrente = heure civile + chaîne de
  fuseau.

Fuseau de la station = **config de station** (un seul fuseau de référence par
nœud, cohérent avec « un nœud = une station autonome »). « Local » n'a de sens
que défini une fois pour la station — une webradio est écoutée depuis
plusieurs fuseaux.

HMI :

- Double horloge si local ≠ UTC, avec **fuseau nommé** et non offset
  (« 08:00 CET / 07:00 UTC », pas « 08:00 +01:00 »).
- La double horloge est aussi le **point de validation de saisie** : à la
  création d'un événement sur la borne de bascule, l'UI lève l'ambiguïté au
  moment de la saisie (« 02:30 n'existe pas / existe deux fois cette nuit-là »)
  — seul instant où un humain peut trancher l'intention. En aval, en epoch,
  l'information est perdue par construction.

CLI : `stationctl schedule preview --at … / --next 24h` simule la grille sans
attendre le wall-clock, affiche chaque occurrence en UTC **et** en local, et
sert de test anti-DST (une fenêtre sur la nuit de bascule révèle trou ou
doublon).