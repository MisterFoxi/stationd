Proposition de contrat TOML de la grille — version 1
Statut : proposition à valider, non implémentée.

Équivalent grille de proposition-grammaire-playlists-v1.md. S'appuie sur resolver.rs (modèle de domaine des 4 familles) et la migration 0006 (grid_index.rs). Le format n'existe pas encore ; le chargement grille se fait aujourd'hui depuis SQLite uniquement. Ce document fige la source-fichier des règles, préalable à ApplyGrid / ValidateGrid / ExportGrid (actuellement UNIMPLEMENTED).

Le TOML définit la syntaxe ; ce document définit le contrat : structure, types, valeurs, dépendances, sémantique. Chaque champ mappe 1:1 sur le modèle Rust (Rule / RuleKind) et sur une colonne de 0006 — même modèle, deux sérialisations.

1. Principes
Un seul fichier grid.toml, conteneur [[rule]]. Divergence assumée avec les playlists (un fichier par playlist, pas de conteneur) : une règle n'est jamais référencée de l'extérieur — c'est la playlist qui l'est. La grille est un objet top-level unique et cohérent, pas une collection d'objets adressables. Cf. webradio-architecture.md : « la grille est un concept top-level distinct qui référence des playlists ».
stationd seul writer, modèle apply/export (comme les playlists). Le TOML est canonique ; les tables grid_* (famille A) sont une projection reconstructible : apply fait DROP + rebuild. L'état de lecture (famille B, compteurs Every / tokens AtClock) n'est jamais dérivé du fichier et apply n'y touche pas.
No-silent-failure : clé inconnue, champ inapplicable au kind, ou combinaison interdite → rejet. Une grille invalide ne remplace pas la dernière grille valide.
playlist_ref = chemin relatif de playlist, sans extension .toml, résolution insensible à la casse (cf. contrat playlists). Une ref introuvable est rejetée à l'apply — jamais avalée.
Fuseau = config de station (IANA, obligatoire), jamais répété dans la règle. Toutes les heures civiles (start/end, at, repères every_minutes) s'entendent dans ce fuseau ; la conversion epoch↔civil (le seul point DST) vit dans clock.rs. Cf. time.md.
2. Racine du document
Champ	Type	Obligatoire	Règle
schema_version	entier	oui	exactement 1
rule	tableau de tables ([[rule]])	oui	au moins une règle
Pas de fuseau à la racine (config station). Une option facultative n'accepte pas null : elle est omise.

3. Règle : champ commun
Champ	Type	Obligatoire	Défaut / règle
id	chaîne	oui	stable, unique, non blanc. Clé de l'état famille B (compteurs Every, tokens AtClock) → un renommage réinitialise la cadence : à traiter comme un changement d'identité, pas un simple libellé
enabled	booléen	non	true
kind	chaîne	oui	base_rotation | day_part | at_clock | every
playlist_ref	chaîne	oui	cf. §1.4
days	tableau mon..sun	non	vide = tous les jours ; sans doublon
date_start	date YYYY-MM-DD	non	inclusive
date_end	date YYYY-MM-DD	non	inclusive, ≥ date_start
days + date_start/date_end = la portée de validité (Validity). Récurrent (jours) et override daté (dates) sont le même mécanisme : une règle fixe hebdo et une émission spéciale un lundi précis ne diffèrent que par ces champs. Cf. modele-programmation.md.

Les champs spécifiques au kind sont gouvernés par les sections suivantes. Un champ appartenant à un autre kind est une erreur, même vide.

4. Champs par kind
kind	Ancrage	Champs propres	Rôle
base_rotation	aucun	—	le plancher : toujours résolvable, priorité la plus basse
day_part	plage horaire	start, end	sélectionne la base active sur un créneau
at_clock	horloge murale	every_minutes | at, mode, expiry	rendez-vous absolu qui ponctue la base
every	dernier passage	min_tracks | min_elapsed	cooldown glissant qui ponctue la base
Sélection vs injection. base_rotation et day_part résolvent la base (« quel plancher maintenant »). at_clock et every n'écrasent pas la base, ils l'injectent ponctuellement ; la base reprend derrière.

4.1 base_rotation
Aucun champ propre. Plusieurs base_rotation = odeur de config ; comportement défini (dernière par id) mais à éviter — une seule attendue.

4.2 day_part
Champ	Type	Règle
start	HH:MM	obligatoire, 00:00–23:59
end	HH:MM	facultatif ; présent : ≠ start (end < start = passage de minuit)
Fenêtre [start, end) en minutes locales. end est une borne de validité molle évaluée en frontière de piste, jamais une coupe : la dernière piste déborde, le changement se fait au titre suivant. Cf. resolver.rs.

Cross-minuit : end < start couvre [start, 24:00) ∪ [00:00, end) (implémenté le 2026-09-22 ; la validité `days` d'une fenêtre explicite s'évalue sur le jour courant).

Tranche ouverte (sans end, 2026-09-25) : elle court de son start jusqu'au prochain start d'une AUTRE tranche (ouverte ou non) — grille de programmes, chaque émission dure jusqu'à la suivante. Ses days / dates s'évaluent sur le jour où elle a COMMENCÉ (un samedi 08:00 peut courir jusqu'au lundi 06:00 ; recherche du dernier début sur 7 jours). Une fenêtre explicite qui démarre dedans la termine pour de bon (pas de reprise après). Seule tranche de la grille : elle tourne en permanence une fois commencée. Cf. resolver::open_part_covers.

Chevauchement : la fenêtre la plus étroite gagne (la plus spécifique), égalités départagées par id ; une tranche ouverte compte comme la plus large (une fenêtre explicite déjà commencée l'emporte). Défini, pas ambigu.

4.3 at_clock
Exactement un ancrage (jamais fusionnés — le piège AzuraCast d'un champ « intervalle » lu de deux façons) :

Champ	Type	Sémantique
every_minutes	entier 1–60	repères à :00, :N, :2N … < 60 dans chaque heure. Ex. 15 → :00 :15 :30 :45. N doit diviser proprement (repère résiduel plus court accepté si non).
at	HH:MM	un repère fixe unique (ex. 08:00 flash légal)
Plus :

Champ	Type	Défaut	Rôle
mode	soft | hard	soft	soft = attend la prochaine frontière de piste ; hard = préempte (duck/fade) pour taper l'instant pile
expiry	durée	absent = jamais périmé	tolérance de rattrapage d'un repère manqué (redémarrage à XX:04). Au-delà, occurrence périmée et sautée. Par règle : un ID news périmé à 4 min → poubelle ; un jingle → encore bon
soft sans expiry peut se déclencher tard (repères intermédiaires fusionnés, pas de rafale de rattrapage). Taper l'heure pile = mode = "hard"

expiry court.
4.4 every
Exactement un de (glissant, ancré sur le dernier passage — état persisté famille B, sinon un restart casse la cadence) :

Champ	Type	Sémantique
min_tracks	entier ≥ 1	au moins N pistes station depuis la dernière diffusion de cette règle
min_elapsed	durée	au moins cette durée écoulée depuis la dernière diffusion
every est par nature soft (évalué en frontière de piste) — pas de mode.

5. Formats
Heure civile : HH:MM, 00:00–23:59.
Date : YYYY-MM-DD, date civile valide.
Durée (expiry, min_elapsed) : [1-9][0-9]*(s|m|h|d), une seule unité (30s, 15m, 2h, 1d). Pas de durée nulle ni composée en v1.
every_minutes : entier, pas une durée (colonne dédiée every_minutes).
6. Ordre de résolution (collisions)
Fixe et documenté, priorité décroissante :

AtClock hard  >  AtClock soft  >  Every  >  base (DayPart → BaseRotation)  >  fallback

Le plus prioritaire actif gagne. Pas de priority: i32 explicite en v1 : l'ordre fixe suffit et se raisonne (cf. modele-programmation.md) ; un champ de finesse pourra s'ajouter plus tard sans casser le contrat.

fallback n'est pas une règle du fichier : c'est le filet de sécurité Liquidsoap quand rien n'a produit de source. Jamais un trou muet.

7. Répartition des validations
Niveau	Contrôles
Parseur TOML	syntaxe, types, doublons de clés
Schéma (local)	clés autorisées par kind, types, enums, bornes, XOR ancrage (at_clock) et cadence (every), end > start
Service (fichier seul)	dates civiles, date_end ≥ date_start, id uniques, formats durée/heure
Service (ensemble)	résolution des playlist_ref (existence), présence d'au moins un base_rotation recommandée
Moteur	état de lecture, occurrences, incidents de diffusion
Erreurs qualifiées : fichier, chemin logique du champ, valeur rejetée, valeurs attendues. Ex. rule[2].kind="every" : min_tracks et min_elapsed exclusifs ; fournir exactement l'un des deux.

8. Exemple de référence
schema_version = 1

# Le plancher : tourne quand rien d'autre ne s'applique.
[[rule]]
id = "floor"
kind = "base_rotation"
playlist_ref = "general"

# Jazz le matin en semaine ; borne molle à 10:00.
[[rule]]
id = "morning-jazz"
kind = "day_part"
playlist_ref = "jazz"
start = "08:00"
end = "10:00"
days = ["mon", "tue", "wed", "thu", "fri"]

# Flash info légal à 08:00 pile : préempte, périme vite.
[[rule]]
id = "news-8h"
kind = "at_clock"
playlist_ref = "flash-info"
at = "08:00"
mode = "hard"
expiry = "2m"

# ID station tous les quarts d'heure, calé horloge, en douceur.
[[rule]]
id = "station-id"
kind = "at_clock"
playlist_ref = "jingles"
every_minutes = 15
mode = "soft"

# Jingle cadencé : au moins 4 pistes entre deux passages.
[[rule]]
id = "sweeper"
kind = "every"
playlist_ref = "sweepers"
min_tracks = 4

# Override daté : émission spéciale un dimanche précis (même mécanisme
# qu'un day_part, borné par des dates au lieu d'être récurrent).
[[rule]]
id = "special-2026-12-25"
kind = "day_part"
playlist_ref = "noel"
start = "18:00"
end = "20:00"
date_start = "2026-12-25"
date_end = "2026-12-25"

9. Écarts / travail induit
Sujet	État actuel	Proposition
Chargement grille	SQLite seul (grid_index::load_grid)	ajouter parser TOML → Grid, puis apply (DROP+rebuild famille A)
Version	absente	schema_version = 1 obligatoire
at_clock ancrage	XOR en colonne + CHECK 0006	XOR au schéma TOML (every_minutes | at)
every cadence	XOR en colonne + CHECK 0006	XOR au schéma TOML (min_tracks | min_elapsed)
RPC	ApplyGrid/ValidateGrid/ExportGrid UNIMPLEMENTED	câbler sur le parser + projection
Périmètre v1 recommandé : parser + ValidateGrid (fichier seul + refs), puis ApplyGrid (rebuild famille A, famille B intacte), puis ExportGrid.

10. Encore ouvert
day_part cross-minuit : bloquant pour une base de nuit 22:00→06:00. Deux règles ne couvrent pas proprement (perte de la « plus étroite gagne »). À trancher avec le carry de jour dans window_covers.
Round-trip lossless (toml_edit) pour export : conserver commentaires/ordre, ou export = régénération brute assumée ? (les playlists tranchent lossless ; la grille, un seul fichier, peut différer).
min_elapsed vs frontière de piste : la cadence temps est évaluée en frontière ; préciser si un dépassement long déclenche au prochain bord (comportement actuel every_due) sans rattrapage — à documenter côté HMI.
Plusieurs base_rotation : interdire au schéma (recommandé) ou garder le « dernière par id » du résolveur ? Pencher pour rejet à la validation.
priority: i32 par règle : différé, à n'ajouter que si un besoin réel de finesse apparaît au-delà de l'ordre fixe.