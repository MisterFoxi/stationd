# Statistiques de pool du preview

`stationctl schedule preview` affiche `pool: N media, duration HH:MM:SS[.mmm]`
pour chaque occurrence affichée et chaque membre de groupe.

- **dynamic/static** : tous les médias disponibles du pool matérialisé,
  somme de leurs `duration_ms`. Les filtres sont ceux de la sélection réelle ;
  les entrées statiques sont normalisées et dédoublonnées par le même code.
  L'ordre (`newest`, etc.), les quotas et les plugins ne réduisent pas ce pool.
- **group** : statistiques par membre et somme des membres. Un média partagé
  par deux membres compte une fois dans chacun, donc deux fois dans le total.
  Les quotas `take`/`runtime` et les offsets existants restent affichés à part.
  Les stratégies `weighted`/`rotate` exposent aussi leurs membres, sans quota
  ni offset inventé.
- **remote/queue** : nombre de médias inconnu. Si le membre porte un `runtime`,
  cette durée définie est affichée et contribue au total du groupe. Sans durée
  définie, la durée reste inconnue. Le modèle actuel n'a pas d'autre champ de
  durée pour ces sources.
- **vide** : `0 media, duration 00:00:00`. **Inconnu** : `unknown`, jamais zéro.
  Count et durée ont des présences indépendantes. Un membre de durée inconnue
  rend la durée totale du groupe inconnue ; même règle pour le nombre de médias.

Ces valeurs permettent de comparer le pool d'un membre local à son budget
`runtime`, ou celui d'une playlist à sa fenêtre `day_part`. Elles ne prédisent
ni la durée des pistes qui seront effectivement choisies, ni leur ordre.
Les offsets ne sont donc pas recalculés à partir des durées des pools.

Le moteur inspecte l'index média **actuel**. Les filtres actuels ne dépendent
pas de l'instant : une même référence est inspectée une fois par requête et
ses statistiques accompagnent chaque occurrence. Le cache est jeté après la
requête, afin qu'un prochain preview voie les rescans. Aucune lecture future,
importation future ou disponibilité future n'est simulée.

`pool_inspection::inspect_ref` est réutilisable par une future commande
`playlist resolve --pool`. Il appelle `materialize_dynamic`/`materialize_static`
puis additionne les durées du pool en mémoire ; il n'appelle aucun résolveur de
médias, ne lit ni n'écrit de curseur ou `group_state`, et n'appelle pas
`filter_pool`. La projection de grille conserve sa timeline et ses règles
`every` indicatives. Aucune migration ni modification du format TOML.

Le contrat protobuf ajoute `optional uint64 selected_count` et
`google.protobuf.Duration total_duration` à `Occurrence` et `GroupMember`.
La précision est conservée à la milliseconde. Recompiler régénère le contrat
avec `build.rs`. Les anciens clients ignorent ces champs ; un nouveau client
interprète les champs absents d'un ancien serveur comme inconnus.

Une référence absente, une définition ou un filtre invalide, un groupe imbriqué
(non pris en charge par la sélection actuelle) remonte une erreur explicite.
Ces erreurs ne sont pas présentées comme un pool vide ou une source inconnue.

Validation : `cargo test --locked` (169 tests OK),
`cargo build --locked --features tui` (OK).
La suite `cargo test --locked --features tui` est bloquée par cinq erreurs
préexistantes dans les tests de `playlist_form.rs` (ancien contrat Broadcast).
Les tests couvrent les pools filtrés, les doublons, les indisponibilités,
les durées définies des membres remote/queue, les totaux et les champs gRPC,
la précision sub-seconde et les previews successifs sur une connexion SQLite
ouverte en lecture seule, avec un plugin actif qui viderait le pool.
