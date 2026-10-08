# Rotation shuffle des playlists feuilles

`order = "shuffle"` utilise un sac SQLite par référence canonique de playlist
static/dynamic. L'identité d'un membre est son UUID, jamais son chemin.
Chaque membre est réservé une fois avant de mélanger le cycle suivant.
Les groupes gardent leur permutation et leurs quotas existants ; réactiver
une feuille, changer de créneau, déclencher un TOPh ou rescanner ne remet
pas son sac à zéro.

## État et sélection

Migration 0035 : `shuffle_cycle` (numéro du cycle), `shuffle_member`
(UUID, position, réservé, présent dans le pool), `shuffle_pick` (réservation,
cycle, lien vers broadcast_log, confirmation ou abandon). Aucune dépendance
au cache media qui est reconstruit lors des scans. Le RNG et le sac sont
modifiés dans une même transaction ; un échec de durée annule les deux.

Le pool normal est conservé avant les plugins et les contraintes. Les
membres non réservés sont ensuite intersectés avec les candidats autorisés.
Parmi eux, les UUID sans aucune ligne `broadcast_log.aired_at IS NOT NULL`
passent en premier, dans l'ordre aléatoire persisté. Un choix jamais démarré
ne compte pas comme un passage. Le nombre de passages des autres titres
n'est pas utilisé pour modifier leur priorité : chacun a sa place par cycle.

L'historique permet aussi de calculer les statistiques par UUID sans dépendre
du chemin :

```sql
SELECT media_uuid, count(*) AS played, max(aired_at) AS last_played
FROM broadcast_log
WHERE aired_at IS NOT NULL
GROUP BY media_uuid;
```

La sélection teste seulement l'existence d'un passage réel, avec l'index
partiel `broadcast_log_aired_uuid`, sans recalculer tous les compteurs.
Un membre momentanément interdit reste en attente. Si tous les membres
restants sont interdits, la source est vide et le moteur utilise ses règles
de repli habituelles ; il ne redémarre pas le cycle pour contourner les
contraintes. Le cycle suivant commence uniquement quand tous les membres
actuellement présents ont été réservés.

## Pool dynamique

Les nouveaux UUID sont insérés à des positions aléatoires parmi les membres
en attente, sans changer l'ordre relatif de ces derniers. Les membres sortis
du pool sont inactifs ; leur état reste jusqu'à la fin du cycle pour qu'un
retrait puis retour via un changement de tags ne redonne pas immédiatement
un titre déjà choisi. Les inactifs sont supprimés au cycle suivant. Une
suppression définitive n'empêche donc pas le cycle de se terminer.

## Frontières et TOPh

Le budget de durée est appliqué aux membres restants du sac. Le meilleur
ajustement (durée maximale qui tient) précède la priorité « jamais diffusé ».
Les titres trop longs restent dans le sac. Quand aucun titre restant ne tient,
le moteur existant privilégie la continuité : le morceau va à son terme,
puis le rendez-vous est résolu selon sa validité. Aucune coupure horaire
n'est ajoutée par le sac.

## Préparation et simulation

Une réservation évite que deux préparations consécutives prennent le même
membre du cycle. Le jeton exact de réservation est transmis jusqu'au journal, même si des
appels terminent dans un ordre différent ou si le chemin change entre-temps.
`mark_aired` confirme le passage. Liquidsoap rapporte les demandes abandonnées
lors d'un flush à `/ls/v1/discard` : elles reviennent dans le sac. Le média
courant n'appartient pas à cette file et ne peut pas être restitué ainsi.
Les fichiers disparus avant la validation sont également restitués.
Les annotations des demandes conservent leur log ID après redémarrage de
StationD ; les réservations ne sont donc pas annulées au démarrage.

La simulation copie les trois tables et les compteurs RNG, utilise le même
algorithme et confirme ses démarrages dans sa copie. Le titre déjà préparé
est comptabilisé avant de simuler la suite, y compris sa première diffusion.
Elle n'écrit rien dans la base réelle. Les priorités de passage sont propres à la station, même
quand les fichiers et UUID proviennent d'une bibliothèque NFS partagée.

### Best effort avant un rendez-vous horaire

La recherche respecte le sac, les plugins et les contraintes. Si le membre courant
ne propose aucun titre assez court, les autres membres sont essayés uniquement
pour un groupe shuffle. Un groupe sequence conserve strictement son ordre :
il ne rejoue pas son intro et ne joue pas son outro avant le contenu.
Le groupe reste différé pendant que la grille cherche un autre titre compatible.
Un titre emprunté consomme son sac feuille, sans avancer la position ni le quota
du membre différé. Un seul emprunt est autorisé pendant ce report ; les pulls
suivants ne peuvent pas répéter en boucle un membre à un seul média.
Cet état est persisté dans group_state.boundary_borrowed ;
la simulation le copie et applique le même algorithme.

Si le groupe ne fournit rien d'assez court, toutes les autres sources applicables
de la grille sont essayées. En cas d'échec, une deuxième recherche choisit un titre
finissant au plus tard à l'expiration du rendez-vous (dans une limite de dix minutes).
Elle reste soumise aux mêmes sacs et contraintes, et vérifie que l'occurrence reste
applicable à cette heure. Le repli sans limite de durée vient seulement après ces
deux recherches. Ce repli conserve
la continuité : aucun titre commencé n'est coupé.

« À suivre » expose deux notes spécifiques (rule, playlist, at) :
- BOUNDARY_NO_FIT : aucun titre admissible assez court ; rendez-vous retardé.
- BOUNDARY_MISSED : le titre retenu finit après l'expiration du rendez-vous ;
  le rendez-vous sera manqué.

Ces messages sont disponibles dans l'API, stationctl et la TUI (fr/en/de). Ils
décrivent la prévision dans l'état actuel, qui peut changer avec un ajout au
catalogue, une modification de grille ou un override. Aucun titre temporairement
exclu n'est retiré du sac pour résoudre la frontière.