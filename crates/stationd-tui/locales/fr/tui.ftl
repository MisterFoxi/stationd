# stationd-tui — français (langue par défaut et de repli).
# Les trois catalogues (fr, en, de) ont exactement les mêmes clés : un test
# le vérifie. Pas d'espace insécable « magique » : le terminal les rend mal.

## Écrans (onglets)
screen-antenne = Antenne
screen-control = Contrôle
screen-playlists = Playlists
screen-agenda = Agenda
screen-media = Médias
screen-tags = Tags
screen-system = Système
screen-plugins = Plugins

## Touches et aide
key-digits = 1…8
key-help = ? / F1
key-quit = q
key-force-quit = Ctrl+Q
key-plus-minus = + / -
help-switch-screen = changer d'écran
help-screen-help = aide de l'écran
help-quit = quitter (stationd continue)
help-force-quit = quitter, même en saisie
help-upcoming-count = morceaux à suivre affichés
help-title-screen = Écran : { $screen }
help-legend = — inconnu · ~ projeté ou ancien
help-box-title = Aide — Échap pour fermer

## Ligne de statut
status-connecting = Connexion à stationd…
status-connected = Connecté à stationd
status-error = Erreur : { $reason }
status-screen = écran : { $screen }
terminal-too-small =
    Terminal trop petit : { $width }×{ $height } (minimum { $min_width }×{ $min_height }).
    Agrandir la fenêtre, ou q pour quitter.

## Durées
duration-days = { $d }j { $h }h { $m }m
duration-hours = { $h }h { $m }m
duration-minutes = { $m }m { $s }s

## Bandeau
banner-station-unknown = station ?
banner-live = LIVE { $dj }
banner-overrides = { $n ->
    [one] { $n } override en attente
   *[other] { $n } overrides en attente
}
banner-overrides-short = { $n } ovr
banner-state-unknown = diffusion : inconnue
banner-stale = ~ancien
banner-listeners = auditeurs
banner-listeners-short = aud.
banner-listeners-unknown = { $label } —
banner-listeners-stale = { $label } ~{ $n } (ancien)
banner-listeners-count = { $label } { $n }
banner-on-air = à l'antenne :
banner-tz-unknown = fuseau « { $tz } » introuvable
banner-tz-none = fuseau —
banner-uptime = uptime { $t }
banner-uptime-short = up { $t }

## États de diffusion (bandeau)
state-running = EN COURS
state-paused = PAUSE
state-draining = VEILLE ARMÉE
state-draining-long = VEILLE ARMÉE
state-sleeping = VEILLE
state-sleeping-long = VEILLE (bruit de fond)
state-unknown = état ?

## Liaison avec stationd
link-connecting = connexion à { $host }…
link-connected = connecté
link-lost = stationd injoignable depuis { $since }
link-lost-short = injoignable { $since }

## Nature de l'antenne (repli Liquidsoap)
kind-fallback = FALLBACK
kind-halted = à l'arrêt

## Accès gRPC
rpc-bad-address = adresse gRPC invalide « { $addr } » : { $reason }
rpc-timeout = pas de réponse en { $s } s
rpc-unreachable = stationd injoignable ({ $reason })
rpc-status = { $code } : { $reason }
rpc-no-onair = ce stationd ne sert pas la vue de l'antenne (à mettre à jour)

## Flux de l'antenne
onair-stream-opening = flux de l'antenne : ouverture…
onair-stream-error = flux de l'antenne : { $reason }
onair-link-lost = { $reason } — dernier instantané affiché
onair-waiting = en attente du premier instantané…

## Écran Antenne — à l'antenne
onair-title = À l'antenne
onair-state-running = EN COURS
onair-state-paused = PAUSE
onair-state-draining = VEILLE ARMÉE
onair-state-sleeping = VEILLE
onair-stream-continuous = flux continu
onair-duration-unknown = durée inconnue
onair-since = depuis { $time }
onair-live = LIVE — { $dj }
onair-fallback = FALLBACK (filet de sécurité)
onair-halted = à l'arrêt (bruit de fond)
onair-no-liquidsoap = pas de [liquidsoap] : rien n'est diffusé
onair-nothing-reported = rien reçu de Liquidsoap

## Morceaux
track-relay = relais { $url }
track-untitled = { $file } (titre non renseigné)
track-pushed-by = par { $source }

## Origines (règle qui a choisi le morceau)
origin-at-clock-hard = rendez-vous (coupe)
origin-at-clock-soft = rendez-vous
origin-every = every
origin-day-part = tranche
origin-base-rotation = base
origin-override = override
origin-fallback = FALLBACK

## Écran Antenne — playlists, à suivre, joués
playlists-title = Playlists
playlists-by-track-count = au compteur de titres :
next-title = À suivre
next-prepared = préparé
next-cut-at = coupé { $time }
next-nothing = rien
played-title = Joués
played-nothing = rien encore
played-aired = diffusé
played-cut = coupé
played-end-unknown = fin ?

## Notes de l'antenne (opcodes envoyés par stationd)
note-station-paused = station en pause : rien ne suit tant qu'elle n'est pas reprise
note-station-sleeping = station en veille : rien ne suit avant le réveil
note-sleep-at-track-end = veille à la fin du morceau en cours (0 auditeur)
note-sleep-armed = veille armée : arrêt au bord de piste si les conditions de veille restent remplies
note-live-on-air = DJ { $dj } à l'antenne : la suite dépend de la fin du live
note-no-liquidsoap = pas de [liquidsoap] : rien n'est diffusé, la suite montre ce que la grille choisirait
note-simulated = suite simulée : ce que la station jouera si rien ne change d'ici là (un override, un live, une grille modifiée, un rescan peuvent la changer)
note-pool-empty = plus rien de diffusable dans la grille à ce moment : blanc (Liquidsoap comble)
note-fallback = FALLBACK : aucune règle ne couvre ce moment de la grille
note-stream-unknown-duration = relais { $media } : durée inconnue, pas d'heure estimée au-delà
note-unknown-duration = { $media } : durée inconnue (hors index), pas d'heure estimée au-delà
note-simulation-failed = simulation impossible : { $reason }
note-plugin-filter-failed = le plugin { $plugin } a échoué pendant la simulation : { $reason }
note-grid-projection-failed = projection de la grille impossible : { $reason }
note-history-unreadable = historique illisible : { $reason }
note-unknown = note inconnue (code { $code })

## Repli sans flux de l'antenne
fallback-title = À l'antenne (repli)
fallback-on-air = À l'antenne
fallback-since = Depuis
fallback-since-value = { $time } (il y a { $ago })
fallback-prepared = Préparé
fallback-no-liquidsoap = Liquidsoap non configuré
fallback-unknown = Antenne inconnue

## Écrans à venir
planned-coming = À venir
planned-lot = lot { $lot }
planned-plugin-missing = plugin « { $plugin } » non chargé
planned-unavailable = Indisponible : { $reason }

## Incidents de grille (notes de l'antenne)
note-rendezvous-will-not-cut = prévu : le rendez-vous { $rule } de { $time } ne coupera pas — « { $playlist } » n'a rien à diffuser
note-source-will-be-empty = prévu : { $time }, « { $playlist } » ({ $rule }) n'aura rien à diffuser — la priorité inférieure prendra le relais
note-rendezvous-not-cut = constaté : le rendez-vous { $rule } n'a pas coupé à { $time } — « { $playlist } » n'avait rien à diffuser ({ $count ->
    [one] 1 fois
   *[other] { $count } fois
})
note-source-was-empty = constaté : « { $playlist } » ({ $rule }) n'avait rien à diffuser à { $time } ({ $count ->
    [one] 1 fois
   *[other] { $count } fois
})
slot-pool-empty = pool vide : rien ne passera
slot-nothing-playable = rien de jouable (contraintes, plugins)

## Touches (actions)
key-space = Espace
key-tab = Tab
key-up-down = ↑ / ↓
key-a = a
key-shift-a = A
key-c = c
key-d = d
key-shift-d = D
key-e = e
key-k = k
key-l = l
key-n = n
key-o = o
key-r = r
key-s = s
key-v = v
key-w = w
key-x = x
help-pause-resume = pause / reprise / réveil
help-skip = passer au suivant
help-override = pousser un override
help-drain = veille dès 0 auditeur
help-wake = réveiller
help-section = section suivante
help-select = choisir la ligne
help-override-remove = retirer l'override choisi
help-override-clear = vider la file
help-live-kick = couper le DJ
help-live-open = ouvrir un créneau
help-live-close = fermer l'ouverture choisie
help-enqueue = ajouter à une file queue
help-scan = scanner la bibliothèque
help-shutdown = arrêt opérateur
help-shutdown-force = arrêt forcé (coupe le DJ)

## Actions : ligne de statut
status-action-running = action en cours…
status-action-failed = échec : { $reason }
status-cancelled = annulé
done-state = diffusion : { $from } → { $to }
done-state-unchanged = diffusion déjà { $state }
done-skip = passage au suivant demandé
done-override = override #{ $id } en file ({ $pending } en attente)
done-override-degraded = override #{ $id } en file, joué en SOFT (pas de coupe possible) — { $pending } en attente
done-overrides-cleared = { $n ->
    [one] 1 override retiré
   *[other] { $n } overrides retirés
}
done-live-kicked = DJ { $dj } coupé
done-live-opened = créneau ouvert pour { $dj } jusqu'à { $until }
done-live-closed = ouverture de { $dj } fermée
done-enqueued = ajouté à « { $playlist } » ({ $len } en file)
done-enqueue-full = file « { $playlist } » pleine ({ $len }) : rien ajouté
done-scan = scan : { $found } trouvés, { $skipped } écartés, { $vanished } disparus à ce scan ({ $unavailable } disparus au total)
done-plugin = plugin { $name } : { $state }
done-plugin-reason = plugin { $name } : { $state } ({ $reason })
done-shutdown = arrêt opérateur : Liquidsoap garé sur le bruit de fond, stationd s'arrête
done-shutdown-fallback = arrêt opérateur : stationd s'arrête, le filet de sécurité de Liquidsoap prend l'antenne

## Dialogues
dialog-cancel = Annuler
dialog-confirm-keys = ←/→ choisir · Entrée valider · Échap annuler
dialog-form-keys = Tab/↑↓ champ · ←/→ choix · Entrée valider · Échap annuler
form-required = « { $field } » est obligatoire
form-positive-integer = « { $field } » : un entier ≥ 1
confirm-pause-title = Mettre en pause
confirm-pause-body = L'antenne s'arrête sur « { $track } » jusqu'à la reprise.
confirm-pause-yes = Mettre en pause
confirm-skip-title = Passer au suivant
confirm-skip-body = Coupe « { $track } » maintenant.
confirm-skip-next = Suivant prévu : « { $track } ».
confirm-skip-yes = Passer au suivant
confirm-drain-title = Veille dès 0 auditeur
confirm-drain-body = La station passera en veille dès qu'il n'y aura plus d'auditeur (bruit de fond, plus de grille).
confirm-drain-yes = Armer la veille
form-override-title = Pousser un override
form-override-kind = Contenu
form-override-kind-media = Média
form-override-kind-playlist = Playlist
form-override-target = Cible (Entrée : choisir dans la liste, ou saisir un chemin / une référence)
form-override-mode = Mode
form-override-soft = SOFT : au prochain morceau
form-override-hard = HARD : coupe maintenant
form-override-expiry = Péremption (30s, 5m, 2h ; vide = jamais)
form-override-tracks = Pistes tenues (playlist ; vide = tout le cycle d'un groupe, sinon 1)
override-media = Média : { $path }
override-playlist = Playlist : { $playlist }
confirm-override-title = Pousser cet override ?
confirm-override-soft = Passe au prochain bord de morceau.
confirm-override-hard = Coupe le morceau en cours MAINTENANT.
confirm-override-tracks = { $n ->
    [one] Tient 1 piste.
   *[other] Tient { $n } pistes.
}
confirm-override-tracks-auto = Tient tout le cycle si c'est un groupe (repris du début), sinon 1 piste.
override-left-auto = auto
form-pick-media = Choisir un média — / chercher · Entrée choisir · Échap annuler
form-pick-playlist = Choisir une playlist
confirm-override-no-expiry = Sans péremption.
confirm-override-expiry = Périmé s'il n'est pas passé dans { $expiry }.
confirm-override-yes = Pousser
confirm-clear-one-title = Retirer l'override
confirm-clear-one-body = Retirer l'override #{ $id } ({ $what }) de la file ?
confirm-clear-all-title = Vider la file d'overrides
confirm-clear-all-body = { $n ->
    [one] Retirer l'override en attente ?
   *[other] Retirer les { $n } overrides en attente ?
}
confirm-clear-yes = Retirer
confirm-kick-title = Couper le DJ
confirm-kick-body = Déconnecter { $dj } maintenant ? Il sera refusé jusqu'à la fin de son créneau.
confirm-kick-yes = Couper
form-live-open-title = Ouvrir un créneau live
form-live-dj = DJ
form-live-duration = Durée (30m, 2h, 1d)
confirm-live-close-title = Fermer l'ouverture
confirm-live-close-body = Fermer maintenant l'ouverture de { $dj } ?
confirm-live-close-yes = Fermer
form-enqueue-title = Ajouter à une file queue
form-enqueue-playlist = Playlist (mode queue)
form-enqueue-media = Chemin sous media/
confirm-scan-title = Scanner la bibliothèque
confirm-scan-body = Relit tout media/ : peut durer sur un gros disque ou un montage NFS. L'antenne n'est pas touchée.
confirm-scan-yes = Scanner
plugin-verb-start = démarrer
plugin-verb-stop = arrêter
plugin-verb-restart = redémarrer
plugin-verb-reload = recharger
confirm-plugin-start = Démarrer le plugin { $name }
confirm-plugin-stop = Arrêter le plugin { $name }
confirm-plugin-restart = Redémarrer le plugin { $name }
confirm-plugin-reload = Recharger le plugin { $name }
confirm-plugin-body = Le nouvel état se lit dans la liste des plugins.
confirm-shutdown-title = Arrêt opérateur
confirm-shutdown-body = stationd s'arrête et Liquidsoap est garé sur le bruit de fond. Pour relancer : stationctl station start.
confirm-shutdown-kicks = Le DJ { $dj } sera déconnecté.
confirm-shutdown-refused-live = Le DJ { $dj } est à l'antenne : l'arrêt sera refusé (A pour forcer).
confirm-shutdown-continue = Continuer
confirm-shutdown-again-title = Confirmer l'arrêt
confirm-shutdown-again-body = Dernière confirmation : la station cesse de diffuser la grille.
confirm-shutdown-yes = Arrêter la station

## Écran Contrôle
control-broadcast = Diffusion
control-overrides = Overrides
control-live = Live
control-queue = File queue
control-library = Bibliothèque
control-plugins = Plugins
control-station = Station
control-state = État
control-listeners = Auditeurs
control-on-air = À l'antenne
control-broadcast-hint = Espace met en pause ou reprend, n passe au suivant, v arme la veille (dès 0 auditeur), w réveille.
control-read-failed = lecture impossible : { $reason }
control-not-read = pas encore lu
control-more = … et { $n } de plus
control-overrides-none = aucun override en attente
control-overrides-hint = o pousse un média ou une playlist devant la grille.
control-col-content = Contenu
control-col-mode = Mode
control-col-left = Reste
control-col-expires = Périme
control-col-source = Par
control-col-plugin = Plugin
control-col-state = État
control-col-failures = Échecs
control-col-reason = Raison
control-live-disabled = live non configuré (pas de section [live])
control-live-on-air = À l'antenne
control-live-nobody = personne
control-live-session = accès { $access }, depuis { $since }, { $address }
control-live-accounts = Comptes DJ
control-live-djs = { $n } déclarés
control-live-djs-error = fichier des DJ illisible : { $reason }
control-live-urgent = Droit urgent
control-live-cooldown = Refroidissement
control-live-until = { $dj } jusqu'à { $time }
control-live-refused = Refusé
control-live-last-refusal = Dernier refus
control-live-refusal = { $dj } à { $time } : { $reason }
control-live-openings = Ouvertures :
control-live-no-opening = aucune
control-live-cut = coupé
control-queue-hint = e ajoute un média au tampon d'une playlist en mode queue (demandes d'auditeurs, dédicaces). La playlist doit exister et être en mode queue ; stationd refuse sinon, et le dit.
control-scan-none = aucun scan lancé depuis cette TUI
control-scan-hint = s relit media/ et met l'index à jour (fichiers ajoutés, retirés, modifiés).
control-scan-last = Dernier scan :
control-scan-found = Trouvés
control-scan-present = Disponibles
control-scan-vanished = Disparus à ce scan
control-scan-unavailable = Disparus au total
control-scan-skipped = Écartés
scan-skip-unreadable = illisible
scan-skip-zero-duration = durée nulle
scan-skip-walk-error = parcours
scan-skip-unknown = ?
control-plugins-none = aucun plugin déclaré
control-plugin-off = désactivé
control-shutdown-hint = a : arrêt opérateur. stationd s'arrête, Liquidsoap est garé sur le bruit de fond ; rien ne repart avant « stationctl station start ». A : idem en coupant le DJ à l'antenne. Deux confirmations.
control-shutdown-live = DJ { $dj } à l'antenne : a sera refusé, A le coupe.

## Écran Médias
media-search-title = Recherche
media-search-hint = / pour chercher : mots (titre, artiste, album, chemin), genre:x, dossier:x, âge:<10d (création)
media-count = { $shown } / { $total }
media-sorted-by = tri : { $field } { $dir }
media-missing = sans { $field }
media-with-unavailable = disparus inclus
media-loading = chargement…
media-none = aucun média ne correspond
media-field-path = chemin
media-field-title = titre
media-field-artist = artiste
media-field-album = album
media-field-year = année
media-field-duration = durée
media-field-genre = genre
key-slash = /
key-enter = Entrée
key-esc = Échap
key-m = m
help-media-search = chercher
help-media-apply = garder la recherche
help-media-cancel = revenir à la recherche d'avant
help-media-sort = changer le tri
help-media-desc = inverser l'ordre
help-media-missing = filtre « sans titre / artiste / genre / année »
help-media-unavailable = inclure les fichiers disparus
help-media-reload = recharger

## --- Lot 4b : playlists, éditeur, fiche média -----------------------------

key-p = p
key-f = f
key-shift-r = R
key-ctrl-s = Ctrl+S
key-ctrl-t = Ctrl+T
key-ctrl-n = Ctrl+N
key-ctrl-d = Ctrl+D
key-alt-updown = Alt+↑ / ↓
key-left-right = ← / → / Espace
key-f8 = F8

help-close = fermer
help-pl-edit = modifier
help-pl-new = nouvelle playlist
help-pl-delete = supprimer
help-pl-filter = filtrer
help-pl-reload-root = relire tous les fichiers
help-ed-save = enregistrer
help-ed-next = champ suivant (Maj+Tab : précédent)
help-ed-choice = changer le choix
help-ed-add = ajouter (filtre, membre, médias)
help-ed-remove = retirer l'élément
help-ed-move = déplacer l'élément
help-ed-raw = éditer le TOML brut
help-ed-form = revenir au formulaire
help-ed-next-diag = aller au problème suivant
help-media-card = fiche du média
help-media-mark = marquer / démarquer
help-media-clear-marks = tout démarquer
help-media-to-playlist = ajouter à une playlist statique
help-media-enqueue = mettre en file
help-card-prev-next = média précédent / suivant
help-picker-add = ajouter les médias marqués (ou celui-ci)

dialog-info-keys = Entrée / Échap : fermer
picker-keys = ↑↓ choisir · Entrée valider · Échap annuler
picker-loading = chargement de la liste…
picker-new = Nouvelle playlist…
picker-none = aucune playlist ne correspond

mode-static = statique
mode-dynamic = dynamique
mode-remote = relais
mode-queue = file
mode-group = groupe

val-shuffle = aléatoire
val-sequential = dans l'ordre
val-newest = le plus récent d'abord
val-oldest = le plus ancien d'abord
val-fifo = premier arrivé d'abord
val-lifo = dernier arrivé d'abord
val-all = tous les filtres
val-any = au moins un filtre
val-filename = nom de fichier
val-mtime = date du fichier
val-published = date de publication
val-weighted = pondéré
val-rotate = à tour de rôle
val-sequence = à la suite
val-abort = tout le groupe cède
val-skip = passer au suivant
val-fallthrough = céder la place
val-stop = s'arrêter
val-disable = se désactiver
val-hold = garder l'antenne
val-yes = oui
val-no = non
val-duration-s = durée (s)
val-age = âge (création, ex. 10d)
val-creation = date de création
val-tempo = tempo
val-genre-ai = Genre IA
val-mood = Mood
val-prefix = commence par
val-eq = égal à
val-ne = différent de
val-contains = contient
val-has = porte le genre
val-has-any = au moins un de
val-has-all = tous ces genres
val-has-none = aucun de

pl-summary = { $n ->
    [one] { $n } playlist
   *[other] { $n } playlists
} · tri : { $sort } · / filtrer
pl-none = aucune playlist
pl-col-ref = fichier (ref)
pl-col-name = nom
pl-col-mode = mode
pl-col-used = utilisée par
pl-pool = pool
pl-used-rules-n = { $n ->
    [one] { $n } règle
   *[other] { $n } règles
}
pl-used-groups-n = { $n ->
    [one] { $n } groupe
   *[other] { $n } groupes
}
pl-used-none = rien (ni règle de grille, ni groupe)
pl-used-rules = règles de grille : { $list }
pl-used-groups = groupes : { $list }
pl-disabled = désactivée
pl-detail = Détail
pl-file = fichier
pl-no-file = sans fichier
pl-no-file-long = aucun (entrée ajoutée par stationctl add)
pl-file-differs = le fichier diffère de ce qui est appliqué (modifié à la main, ou invalide)
pl-edit-no-file = Cette playlist a été ajoutée sans fichier (stationctl add) : elle se modifie là où vit son TOML, puis par stationctl add.
pl-open-failed = Ouverture impossible
pl-opening = ouverture…
pl-new-title = Nouvelle playlist
pl-new-mode = Mode de la playlist (modifiable ensuite) :
pl-delete-title = Supprimer une playlist
pl-delete-refused = « { $playlist } » ne peut pas être supprimée tant qu'elle est référencée :
pl-delete-body = Supprimer « { $playlist } » ({ $name }) ?
pl-delete-file = Son fichier { $file } sera effacé du nœud.
pl-delete-no-file = Elle n'a pas de fichier : seule sa ligne sera retirée.
pl-delete-yes = Supprimer
pl-reload-title = Relire les playlists
pl-reload-body = stationd relit tous les fichiers de playlists du nœud et retire celles dont le fichier a disparu (sauf si une règle ou un groupe les référence encore).
pl-reload-yes = Relire
pl-busy-title = Brouillon en cours
pl-busy-body = Un autre brouillon de playlist est ouvert : enregistre-le ou ferme-le, puis recommence depuis Médias.

pl-h-identity = Identité
pl-h-selection = Sélection
pl-h-broadcast = Diffusion
pl-h-files = { $n ->
    [one] Média ({ $n })
   *[other] Médias ({ $n })
}
pl-h-filters = { $n ->
    [one] Filtre ({ $n })
   *[other] Filtres ({ $n })
}
pl-h-members = { $n ->
    [one] Membre ({ $n })
   *[other] Membres ({ $n })
}
pl-f-ref = fichier (ref)
pl-f-name = nom
pl-f-enabled = activée
pl-f-mode = mode
pl-f-order = ordre
pl-f-match = combinaison
pl-f-order-by = date prise
pl-f-unplayed = une seule diffusion
pl-f-url = adresse du flux
pl-f-max-len = longueur maximale
pl-f-strategy = stratégie
pl-f-on-member-unavailable = membre sans média
pl-f-filter = filtre { $n }
pl-f-op = opérateur
pl-f-value = valeur
pl-f-member = membre { $n }
pl-f-weight = poids
pl-f-take-random-min = pistes aléatoires : minimum
pl-f-take-random-max = pistes aléatoires : maximum
ag-take-random = { $min } à { $max } pistes (aléatoire)
pl-f-take = pistes
pl-f-runtime = durée
pl-f-limit = pistes par passage
pl-f-repeat = reprendre au début
pl-f-on-exhausted = une fois épuisée
pl-f-no-same-artist = même artiste, pas avant
pl-f-no-same-track = même fichier, pas avant
pl-f-no-same-title = même morceau, pas avant
pl-absent = — (non précisé)
pl-add-files = ajouter des médias
pl-add-filter = ajouter un filtre
pl-add-member = ajouter un membre

ed-title = Playlist { $reference }
ed-title-new = Nouvelle playlist { $reference }
ed-modified = modifiée
ed-revision = rév. { $rev }
ed-raw-mode = TOML brut
ed-form = Formulaire
ed-toml = TOML
ed-toml-keys = Ctrl+T pour l'éditer
ed-toml-keys-raw = Échap ou Ctrl+T : formulaire · Ctrl+S : enregistrer
ed-unreadable = Le TOML ne se lit plus : le formulaire attend qu'il soit corrigé.
ed-unreadable-hint = Ctrl+T pour revenir à l'éditeur de texte ; stationd indique la ligne en cause.
ed-file-differs = le fichier { $file } diffère de ce qui est appliqué : c'est lui qui est ouvert
ed-no-file = pas de fichier sur le nœud : l'enregistrement le créera
ed-files-added = { $n } média(s) ajouté(s) sur { $total }
ed-ref-required = indique le fichier (ref) de la nouvelle playlist, ex. emission/intro
ed-saving = enregistrement…
ed-save-failed = enregistrement impossible : { $reason }
ed-saved = enregistrée ({ $file })
ed-created = créée ({ $file })
ed-reloaded = brouillon remplacé par le fichier du nœud
ed-not-saved = { $n ->
    [one] non enregistrée : { $n } erreur
   *[other] non enregistrée : { $n } erreurs
}
ed-conflict-short = conflit : le fichier a changé
ed-conflict-title = Le fichier a changé entre-temps
ed-conflict-body = { $file } a été modifié depuis son ouverture (par quelqu'un d'autre, ou à la main). Rien n'a été écrit.
ed-conflict-never = Un enregistrement n'écrase jamais une autre modification : compare, puis recharge et reprends tes changements.
ed-conflict-keep = Garder le brouillon
ed-conflict-compare = Comparer
ed-conflict-reload = Recharger (brouillon perdu)
ed-compare-title = Fichier du nœud / brouillon
ed-compare-keys = ↑↓ PgUp PgDn défiler · Échap fermer · lignes en orange : différentes
ed-compare-disk = Sur le nœud (rév. { $rev })
ed-compare-draft = Ton brouillon
ed-discard-title = Brouillon modifié
ed-discard-body = Les modifications de ce brouillon ne sont pas enregistrées.
ed-discard-keep = Continuer l'édition
ed-discard-yes = Abandonner les modifications
ed-pick-media = Ajouter des médias
ed-pick-member = Ajouter un membre
ed-genres = genres : { $list }
ed-genres-none = aucun genre connu ne commence ainsi
ed-diags = { $errors } erreur(s), { $warnings } avertissement(s)
ed-diags-none = Diagnostics
ed-diags-ok = stationd ne voit aucun problème
ed-diag-label = { $label } :
ed-diag-file = fichier
ed-pool = Pool aujourd'hui
ed-pool-pending = calcul par stationd…
ed-pool-invalid = brouillon invalide : pas d'aperçu tant qu'il y a des erreurs
ed-pool-unmeasured = pool non mesurable pour ce mode (relais d'un flux, file d'attente)
ed-pool-unmeasured-short = non mesurable
ed-pool-empty = pool vide : rien ne passera
ed-pool-member-empty = aucun média
ed-pool-count = { $n ->
    [one] { $n } média
   *[other] { $n } médias
}
ed-pool-artists = { $n ->
    [one] { $n } artiste
   *[other] { $n } artistes
}

diag-syntax = TOML illisible ({ $detail })
diag-unknown-field = champ inconnu de la grammaire
diag-missing-field = champ obligatoire absent
diag-bad-value = valeur invalide
diag-not-allowed = champ interdit dans ce mode ou cette stratégie
diag-required-for-mode = requis par ce mode ou cette stratégie
diag-conflict = incompatible avec un autre champ
diag-bad-filter = filtre invalide (champ, opérateur ou valeur)
diag-bad-duration = durée invalide (30s, 15m, 2h, 1d)
diag-unknown-ref = ne désigne aucune playlist
diag-bad-ref = référence invalide
diag-cycle = le groupe se contient lui-même
diag-id-changed = l'identifiant ne peut pas changer
diag-empty-pool = aucun média ne correspond aujourd'hui
diag-unknown = problème inconnu (code { $code })
diag-rejected = {" "}: « { $value } »
diag-expected = {" "}(attendu : { $values })

media-marked = { $n ->
    [one] { $n } marqué
   *[other] { $n } marqués
}
media-choose-static = Ajouter { $n } média(s) à une playlist statique
media-choose-queue = Mettre { $n } média(s) en file

card-title = Fiche du média
card-keys = Échap fermer · ↑↓ précédent / suivant · e tags · o override · p playlist · f file
card-no-title = titre non renseigné
card-size = taille
card-size-mb = { $mb } Mo
card-state = état
card-available = disponible
card-unavailable = disparu du disque
card-playlists = Playlists qui peuvent le diffuser
card-no-playlist = aucune : ce média ne passera que par override ou file d'attente
card-plays = Diffusions (diffusées / choisies)
card-plays-legend = diffusées : réellement parties à l'antenne · choisies : retenues par stationd
card-24h = 24 h
card-7d = 7 j
card-30d = 30 j
card-all = total
card-last = dernier choix :
card-never = jamais

done-enqueued-many = { $n } médias ajoutés à « { $playlist } » ({ $len } en file)
done-enqueue-many-full = file « { $playlist } » pleine après { $n } sur { $total } ({ $len } en file)
done-enqueue-partial = { $n } sur { $total } mis en file, puis : { $reason }
done-playlist-removed = playlist « { $playlist } » supprimée
done-playlist-removed-file = playlist « { $playlist } » supprimée (fichier { $file } effacé)
done-playlists-reloaded = playlists relues : { $added } appliquée(s), { $removed } retirée(s), { $errors } fichier(s) en erreur

## --- Tags des médias ---

help-media-edit-tags = modifier les tags (dans le fichier)
form-tags-title = Tags de { $path }
form-tags-many-title = Tags de { $n } fichiers (vide = inchangé)
form-tags-nothing = rien n'a changé
form-tags-read-failed = Lecture des tags impossible
confirm-tags-title = Écrire dans le fichier
confirm-tags-body = Écrire ces tags dans { $path } :
confirm-tags-many-body = Écrire ces tags dans { $n } fichiers :
confirm-tags-set = { $field } → « { $value } »
confirm-tags-remove = { $field } : retiré
confirm-tags-yes = Écrire
done-tags-written = tags écrits dans { $n } fichier(s) sur { $total }
done-tags-conflicts = modifiés entre-temps, rien écrit : { $list }
done-tags-failed = { $n } échec(s), dont { $first }
done-tags-not-applied = { $path } : stationd n'a pas écrit { $fields } (stationd plus ancien que la TUI ?)
done-tags-no-readback = { $path } : stationd n'a pas renvoyé les tags relus, écriture non vérifiée
card-tags = Tags du fichier
card-tags-manual = choisi à la main
card-tags-auto = déduit
help-tags-write = écrire dans le(s) fichier(s)
help-tags-list = choisir dans la liste (genres)
tags-form-keys = ↑↓ champ · Entrée liste · Suppr vider la liste · ←→ tempo · Ctrl+S écrire · Échap annuler
tags-genres = genres
tags-bpm = BPM
tags-tempo = tempo
tags-creation = date de création
tags-unchanged = inchangé
tags-cleared = tout retiré (dans chaque fichier)
tags-tempo-auto = auto (tiré du BPM)
tags-tempo-auto-now = auto (tiré du BPM, actuellement { $tempo })
tags-creation-now = auto (actuellement { $date })
tags-bad-number = { $field } : un nombre de 1 à { $max }, ou vide
tags-bad-creation = date de création : au format 2026-06-14T06:36:48Z (RFC 3339), ou vide
tags-help-single = vide = champ retiré du fichier
tags-help-batch = vide = inchangé dans chaque fichier
tags-help-list = Entrée : choisir (les valeurs des fichiers en tête), ou en saisir une nouvelle · Suppr : tout retirer
tags-help-tempo = ←→ : un libellé choisi l'emporte sur celui tiré du BPM ; « auto » revient au BPM
tags-help-bpm = le tempo en est tiré (plages de custom-tags) sauf s'il est choisi à la main
tags-help-creation = saisie : l'emporte sur la date tirée du commentaire ; vide : revenir à celle-ci
tags-picker-title = { $field } : cocher (Espace)
tags-picker-title-batch = { $field } : Espace = retirer (valeur présente) ou ajouter, puis inchangé
tags-picker-keys = taper pour filtrer · Espace cocher · Entrée valider · Échap annuler
tags-picker-new = ＋ nouveau : « { $genre } »
tags-picker-none = aucun genre ne correspond : Espace l'ajoute
confirm-tags-merge = { $field } : + { $add } / − { $remove }

## --- Lot 6a : agenda --------------------------------------------------------
key-brackets = [ / ]
key-t = t
key-g = g
key-arrows = ← → ↑ ↓
key-page = PgPréc / PgSuiv
help-ag-slot = créneau précédent / suivant
help-ag-day = jour précédent / suivant
help-ag-week-shift = semaine précédente / suivante
help-ag-today = aujourd'hui, maintenant
help-ag-goto = aller à une date (calendrier)
help-ag-week = vue semaine
help-ag-day-view = vue jour
help-ag-coverage = couverture de la grille
help-ag-back = revenir à l'agenda
help-ag-step = pas de 15 / 30 / 60 min
help-ag-item = élément suivant du créneau
help-ag-inspector = inspecteur (terminal étroit)
help-ag-playlist = ouvrir la playlist
help-ag-cell = choisir une case
help-ag-open-day = ouvrir ce jour
help-ag-rule = règle précédente / suivante
help-ag-cal-move = jour / semaine
help-ag-cal-month = mois précédent / suivant
help-ag-cal-pick = afficher ce jour
ag-no-tz = fuseau de la station inconnu : en attente de stationd…
ag-bad-date = date hors calendrier
ag-view-day = Jour
ag-view-week = Semaine
ag-view-coverage = Couverture
ag-weekday-short = { $wd ->
    [1] lun.
    [2] mar.
    [3] mer.
    [4] jeu.
    [5] ven.
    [6] sam.
   *[7] dim.
}
ag-date-long = { $wd ->
    [1] lundi
    [2] mardi
    [3] mercredi
    [4] jeudi
    [5] vendredi
    [6] samedi
   *[7] dimanche
} { $date }
ag-date-short = { ag-weekday-short } { $date }
ag-week-of = semaine du { $from } au { $to }
ag-month = { $month ->
    [1] janvier
    [2] février
    [3] mars
    [4] avril
    [5] mai
    [6] juin
    [7] juillet
    [8] août
    [9] septembre
    [10] octobre
    [11] novembre
   *[12] décembre
} { $year }
ag-step = pas { $n } min
ag-utc = heures UTC (station : { $tz })
ag-utc-rules = les règles restent en heure de la station
ag-dst = changement d'heure : journée de { $h } h
ag-dst-week = changement d'heure cette semaine
ag-stale = données anciennes, relecture en échec : { $reason }
ag-gap = (sautée)
ag-inspector = Créneau
ag-slot-empty = rien de projeté dans ce créneau
ag-until-end = fin de la période
ag-origin-daypart = tranche (day_part)
ag-origin-base = base (base_rotation)
ag-origin-hard = rendez-vous hard
ag-origin-soft = rendez-vous soft
ag-origin-every = every
ag-origin-fallback = filet de sécurité
ag-kind-at-clock = rendez-vous (at_clock)
ag-kind-live = créneau live
ag-rule = règle
ag-group = groupe
ag-coverage = couverture
ag-pool-unmeasured = non mesurable
ag-take = take { $n }
ag-runtime = runtime { $d }
ag-rule-base = plancher, toute la journée
ag-rule-daypart = de { $start } à { $end }
ag-rule-daypart-open = à partir de { $start }, jusqu'à la tranche suivante
ag-rule-at-hourly = à { $marks } de chaque heure
ag-rule-at-step = toutes les { $n } min à partir de { $first }
ag-rule-at = à { $at }
ag-hard = hard (coupe)
ag-soft = soft (en fin de morceau)
ag-rule-expiry = périmé après { $d }
ag-rule-every-elapsed = toutes les { $d } depuis le dernier passage
ag-rule-every-tracks = toutes les { $n } pistes
ag-rule-live = DJ { $dj } à partir de { $start }
ag-rule-dates = du { $start } au { $end }
ag-rule-from = à partir du { $start }
ag-rule-until = jusqu'au { $end }
ag-rule-disabled = désactivée
ag-live-before = ouverte avant
ag-live-window = fenêtre de connexion { $opens } – { $closes }
ag-floating-title = Hors horloge — au dernier passage ou au compteur de pistes
ag-floating-more = … et { $n } de plus (l : toutes les règles)
ag-legend = ! rendez-vous hard · * soft · ♪ live · ▶ maintenant
ag-day-summary = { $hard } hard · { $soft } soft · { $live } live
ag-calendar = Aller à une date
ag-calendar-keys = flèches · PgPréc/PgSuiv · t · Entrée
cov-grid = Couverture de la grille :
cov-rules = { $n ->
    [one] { $n } règle
   *[other] { $n } règles
}
cov-none = aucune règle active dans la grille
cov-ok = ok
cov-thin = juste
cov-insufficient = insuffisant
cov-reason-ok = le pool couvre ce que la règle demande
cov-pool-empty = pool vide
cov-track-repeat = no_same_track_within { $window } : pool de { $pool } seulement (une piste repassera)
cov-title-repeat = no_same_title_within { $window } : pool de { $pool } seulement (un morceau repassera)
cov-artist-repeat = no_same_artist_within : { $n } artiste(s) distinct(s) (un artiste repassera)
cov-artist-not-evaluated = no_same_artist_within non évalué (agrégat de groupe)
cov-limit-unmet = limit { $limit } : { $n } média(s) distinct(s) dans le pool
cov-finite-short = source finie de { $pool } pour un créneau de { $need } (ne le remplit pas)
cov-members-empty-abort = membre(s) vide(s) : { $list } → le groupe s'arrête (abort)
cov-members-empty-skip = membre(s) vide(s) : { $list } → sautés (dégradé)
cov-members-loop = membre(s) sous-dimensionné(s) : { $list } → boucle dans le créneau
cov-bad-ref = référence invalide : { $reason }
cov-unknown-playlist = playlist inconnue (référence cassée)
cov-unreadable-playlist = playlist illisible : { $reason }
cov-unresolvable = pool non résolvable : { $reason }
cov-runtime-loop = budget runtime { $need } > pool de { $pool } → boucle dans le créneau
cov-take-repeat = take { $take } > { $n } piste(s) distincte(s) → répétition
cov-unknown = cause inconnue (code { $code }) : stationd plus récent que la TUI ?
pl-reveal-missing-title = Playlist introuvable
pl-reveal-missing = La playlist « { $playlist } » n'est pas dans la liste (référence de la grille cassée ?).

## --- Lot 6b : édition de la grille dans l'agenda ------------------------------
key-shift-g = G
key-u = u
key-ctrl-r = Ctrl+R
key-left-right-space = ← → / Espace
help-ag-new = nouvel event au créneau
help-ag-edit = modifier la règle
help-ag-delete = supprimer la règle
help-ag-grids = grilles (voir, activer, copier)
help-ag-utc = heures locales / UTC
help-ag-grid-move = grille précédente / suivante
help-ag-grid-view = afficher cette grille
help-ag-grid-activate = la rendre active
help-ag-grid-copy = copier sous un nouveau nom
help-ag-kind-move = nature précédente / suivante
help-ag-kind-pick = créer
help-ag-rules = toutes les règles de la grille
ag-rules-title = Règles de la grille { $grid }
ag-rules-keys = Entrée modifier · d supprimer · n nouvelle · Échap
ag-rules-none = aucune règle
help-ag-cancel = fermer
help-rf-field = champ précédent / suivant
help-rf-choice = changer le choix, cocher un jour
help-rf-enter = choisir la playlist / champ suivant
help-rf-save = enregistrer la grille
help-rf-rebase = relire le fichier (conflit) en gardant la saisie
help-rf-close = fermer
ag-grid-active = grille { $grid } (active)
ag-grid-other = GRILLE EN PRÉPARATION : { $grid } (pas à l'antenne)
ag-grids-title = Grilles du nœud
ag-grids-keys = Entrée voir · a activer · c copier · Échap
ag-grids-none = aucune grille (le répertoire des grilles est vide)
ag-grids-no-file = pas de fichier (la dernière grille appliquée reste)
ag-grids-rules = { $n ->
    [one] { $n } règle
   *[other] { $n } règles
}
ag-grids-unreadable = illisible
ag-grids-active = active
ag-grids-viewed = affichée
ag-activate-title = Activer une grille
ag-activate-body = La grille { $grid } passe à l'antenne : elle remplace la grille active, dès maintenant.
ag-activate-yes = Activer
ag-copy-title = Copier la grille { $grid }
ag-copy-name = Nom de la nouvelle grille
ag-copy-need-name = Donner un nom (ex. ete)
ag-new-title = Nouvel event
ag-new-kind = Nature de la règle :
ag-grid-unreadable = Grille illisible
ag-grid-toml-broken = Le fichier { $grid } ne se lit pas : le corriger à la main (ou `stationctl schedule show`).
ag-rule-missing-title = Règle introuvable
ag-rule-missing = La règle { $rule } n'est pas dans le fichier { $grid } (modifié à la main depuis ?) : `r` pour relire.
ag-delete-title = Supprimer une règle
ag-delete-body = La règle { $rule } est retirée du fichier { $grid }.
ag-delete-active = C'est la grille active : l'antenne change aussitôt.
ag-delete-other = Grille en préparation : l'antenne ne change pas.
ag-delete-yes = Supprimer
done-rule-deleted = règle { $rule } supprimée de { $grid }
done-grid-activated = grille active : { $grid } ({ $n } règles)
done-grid-not-activated = { $grid } n'est pas activée
done-grid-not-saved = { $grid } n'est pas enregistrée
done-grid-conflict = { $grid } a changé depuis la lecture : rien n'est écrit, relire (r) et recommencer
done-grid-copied = grille { $to } créée (copie de { $from }), pas active
done-grid-exists = la grille { $grid } existe déjà : rien n'est écrit
done-grid-more = { $n ->
    [one] {" "}(et { $n } autre problème)
   *[other] {" "}(et { $n } autres problèmes)
}
gdiag-not-allowed = champ d'une autre nature de règle
gdiag-bad-time = heure au format HH:MM (00:00 à 23:59)
gdiag-bad-date = date au format AAAA-MM-JJ
gdiag-bad-weekday = jour : mon, tue, wed, thu, fri, sat, sun, sans doublon
gdiag-schema-version = version de grille non prise en charge
gdiag-duplicate-id = cet id est déjà celui d'une autre règle
gdiag-several-floors = un seul plancher (base_rotation) par grille
gdiag-zero-window = début et fin identiques : la tranche ne couvre rien
gdiag-dates-reversed = la date de fin précède la date de début
gdiag-out-of-range = nombre hors limites
gdiag-unknown-dj = DJ absent du fichier des DJ
gdiag-no-live = créneau live sans section [live] dans stationd.toml
gdiag-dj-file = fichier des DJ illisible : { $detail }
gdiag-file = fichier de grille illisible : { $detail }
rf-title-new = Nouvelle règle : { $kind }
rf-title-edit = Règle { $id }
rf-grid-active = dans la grille active { $grid } (appliquée à l'enregistrement)
rf-grid-other = dans la grille { $grid } (en préparation, pas à l'antenne)
rf-modified = modifiée
rf-local-times = heures et dates dans le fuseau de la station
rf-id = id
rf-playlist = playlist
rf-dj = DJ
rf-start = début
rf-end = fin
rf-anchor = repère
rf-anchor-at = une fois par jour, à heure fixe
rf-anchor-minute = chaque heure, à la minute
rf-anchor-every = plusieurs fois par heure
rf-at = heure
rf-every-minutes = toutes les (min)
rf-minute = minute
rf-mode = mode
rf-expiry = péremption
rf-cadence = cadence
rf-cadence-elapsed = temps écoulé
rf-cadence-tracks = nombre de pistes
rf-min-elapsed = au plus toutes les
rf-min-tracks = au plus toutes les (pistes)
rf-days = jours
rf-days-all = (aucun coché = tous les jours)
rf-date-start = à partir du
rf-date-end = jusqu'au
rf-hint-time = HH:MM, heure de la station
rf-hint-end = HH:MM ; vide = jusqu'à la tranche suivante ; avant le début = passe minuit
rf-hint-every-minutes = 1 à 1439, comptées depuis minuit (45 → 00:45 01:30 … 23:15)
rf-hint-minute = 0 à 59, un repère par heure (0 → :00, 58 → :58)
rf-hint-expiry = 30s, 5m, 2h ; vide = jamais périmé
rf-hint-duration = 30s, 15m, 2h, 1d
rf-hint-tracks = un nombre de pistes (1 ou plus)
rf-hint-date = AAAA-MM-JJ ; vide = sans limite ; les deux égales = ce jour seulement
rf-hint-days = ← → pour choisir, Espace pour cocher
rf-hint-playlist = Entrée : choisir dans la liste, ou taper le ref
rf-pick-playlist = Playlist de la règle
rf-preview = La journée avec la modification
rf-preview-bases = Bases :
rf-preview-marks = { $n ->
    [one] Cette règle, { $n } fois :
   *[other] Cette règle, { $n } fois :
}
rf-preview-none = Cette règle ne joue pas ce jour-là (jours, dates, heure, ou une règle prioritaire).
rf-preview-invalid = la journée sera projetée dès que la grille n'aura plus d'erreur
rf-preview-others = { $n ->
    [one] { $n } autre repère ce jour-là
   *[other] { $n } autres repères ce jour-là
}
rf-other-problems = Autres problèmes de la grille (ils bloquent aussi l'enregistrement) :
rf-saving = enregistrement…
rf-saved = règle { $id } enregistrée dans { $grid } (grille en préparation)
rf-saved-applied = règle { $id } enregistrée dans { $grid } : à l'antenne
rf-invalid = rien n'est écrit : corriger les champs marqués ✗
rf-conflict = le fichier a changé depuis l'ouverture : rien n'est écrit — Ctrl+R le relit en gardant la saisie
rf-rebasing = relecture du fichier…
rf-rebased = fichier relu, saisie reportée : vérifier puis Ctrl+S
rf-confirm-quit = modifications non enregistrées : Échap encore pour les abandonner

# ── Journal, scan, Tags, Système (lot 7–8) ──────────────────────────────
key-b = b
key-end = Fin
rpc-too-old = ce stationd ne connaît pas cette fonction (à mettre à jour)
stream-closed = flux fermé par stationd
journal-opening = journal : ouverture…
help-cancel = annuler
help-back = revenir
help-reload = relire
help-search-apply = garder la recherche
help-search-clear = effacer la recherche
control-scan-running = scan en cours
scan-phase-listing = parcours des dossiers…
scan-phase-reading = lecture des fichiers { $done } / { $total }
scan-phase-analyzing = estimation des BPM { $done } / { $total }
scan-phase-plugins = plugins (on_scan)…
scan-phase-writing = écriture des valeurs dérivées…
scan-phase-indexing = mise à jour de l'index…

sys-state = État
sys-journal = Journal
sys-stats = Statistiques
sys-stationd = stationd
sys-station = station
sys-version = version
sys-uptime = en marche depuis
sys-pid = processus
sys-timezone = fuseau
sys-journal-field = journal
sys-journal-open = suivi ({ $n } événements)
sys-liquidsoap = Liquidsoap
sys-not-configured = non configuré
sys-ls-air = antenne
sys-ls-on-air = à l'antenne
sys-ls-last-pull = dernière demande
sys-ls-started = pistes démarrées
sys-ls-control = socket de contrôle
sys-ok = ok
sys-icecast = Icecast
sys-ic-server = serveur
sys-ic-last-read = dernière lecture
sys-ic-audience = auditeurs
sys-ic-problem = problème
sys-mount-absent = absent
sys-mount-no-source = sans source
sys-mount-ok = alimenté
sys-mount-rate = { $declared } kbit/s annoncés, { $real } reçus
sys-mount-rate-declared = { $declared } kbit/s annoncés
sys-live = Live
sys-live-on-air = à l'antenne
sys-live-nobody = personne
sys-live-harbor = harbor
sys-live-refused = dernier refus
sys-scan = Bibliothèque
sys-scan-running = scan
sys-scan-last = dernier scan
sys-scan-last-ok = { $when } : { $found } fichiers, { $skipped } écartés
sys-scan-last-failed = { $when } : échec — { $error }
sys-scan-none = aucun scan depuis le démarrage de stationd

help-journal-scroll = remonter / redescendre (pause)
help-journal-follow = suivre en direct
help-journal-level = niveau
help-journal-component = composant
help-journal-search = rechercher
journal-level = niveau
journal-level-all = tout
journal-level-warn = avertissements et erreurs
journal-level-error = erreurs
journal-component = composant
journal-component-all = tous
journal-following = en direct
journal-paused = en pause — { $n } nouveaux (Fin pour reprendre)
journal-search = recherche
journal-empty = rien à montrer

evc-station = station
evc-broadcast = diffusion
evc-grid = grille
evc-library = bibliothèque
evc-plugin = plugin
evc-live = live
evc-liquidsoap = Liquidsoap
evc-icecast = Icecast
evc-unknown = ?

ev-started = stationd démarré (version { $version }, « { $station } »)
ev-stopping-request = arrêt demandé
ev-stopping-signal = arrêt ({ $signal })
ev-broadcast-state = diffusion : { $from } → { $to } (par { $by })
ev-listeners = auditeurs : { $count }
ev-audience-unknown = audience inconnue (lecture Icecast impossible)
ev-track-chosen = choisi : { $media } ({ $playlist }, { $origin })
ev-track-fallback = rien à diffuser : filet de sécurité ({ $origin })
ev-override-pushed = override { $mode } : { $what } (par { $by })
ev-override-dropped = override abandonné, n'a pas pu passer : { $what } (par { $by }) — { $reason }
ev-grid-applied = grille { $grid } appliquée ({ $rules } règles)
ev-grid-refused = grille { $grid } refusée ({ $problems } problèmes) : la grille d'avant reste à l'antenne
ev-grid-unreadable = grille { $grid } illisible : { $error }
ev-incident-hard-not-cut = rendez-vous hard { $rule } sans rien à diffuser ({ $playlist }) : pas de coupe
ev-incident-source-empty = { $rule } : { $playlist } n'a rien donné, la grille passe au niveau suivant
ev-live-started = live : { $dj } à l'antenne ({ $rule })
ev-live-ended = live terminé : { $dj } ({ $reason })
ev-scan-started = scan de la bibliothèque lancé
ev-scan-finished = scan terminé : { $found } fichiers, { $skipped } écartés, { $vanished } disparus
ev-scan-failed = scan en échec : { $error }
ev-tags-written = tags écrits : { $media }
ev-tag-renamed = { $origin } : « { $from } » → « { $to } » ({ $files } fichiers, { $failed } échecs)
ev-plugin-failed = plugin { $plugin } en échec ({ $phase }) : { $reason }
ev-plugin-quarantined = plugin { $plugin } en quarantaine ({ $failures } échecs)
ev-plugin-state = plugin { $plugin } : { $state }
ev-unknown = événement inconnu (stationd plus récent que la TUI ?)

stats-window = fenêtre
stats-window-30m = 30 dernières minutes
stats-window-24h = 24 dernières heures
stats-window-7d = 7 derniers jours
stats-window-30d = 30 derniers jours
stats-by = par
stats-by-playlist = playlist
stats-by-leaf = playlist feuille
stats-by-rule = règle
stats-by-origin = origine
stats-by-media = média
stats-by-artist = artiste
stats-aired = diffusés
stats-picked = choisis
stats-last = dernier
stats-none = rien de diffusé sur cette fenêtre
stats-unknown = (inconnu)
stats-total = total : { $aired } diffusés, { $picked } choisis
help-stats-window = fenêtre
help-stats-by = regroupement

tags-origin-file = genre du fichier
tags-no-source = aucune source custom-tags (Type…) : seul le genre du fichier est montré
tags-value = valeur
tags-count = médias
tags-spellings = graphies : { $list }
tags-empty = aucune valeur connue — un scan de la bibliothèque les relève (Contrôle › Bibliothèque)
tags-without = { $origin } : { $n ->
    [one] 1 média sans valeur
   *[other] { $n } médias sans valeur
}
tags-summary = { $n } valeurs · tri par { $sort }
tags-sort-name = nom
tags-sort-count = effectif
help-tags-origin = origine suivante
help-tags-rename = renommer / fusionner
help-tags-sort = trier par nom / effectif
help-tags-preview = voir ce qui change
help-tags-apply = renommer
tags-rename-title = { $origin } : renommer « { $from } »
tags-rename-hint = Nouvelle valeur (une valeur existante = fusion). Toutes les graphies sont remplacées.
tags-rename-preview-title = { $origin } : aperçu
tags-rename-unexpected = réponse inattendue de stationd
tags-rename-nothing = aucun média disponible ne porte cette valeur
tags-rename-files = { $n ->
    [one] 1 fichier sera réécrit
   *[other] { $n } fichiers seront réécrits
}
tags-rename-merges = « { $to } » existe déjà : les deux valeurs seront fusionnées
tags-rename-no-playlist = aucune playlist ne filtre sur cette valeur
tags-rename-playlists = { $n ->
    [one] 1 playlist filtre sur cette valeur (elle choisira autrement) :
   *[other] { $n } playlists filtrent sur cette valeur (elles choisiront autrement) :
}
tags-rename-confirm = Entrée : renommer · Échap : annuler
tags-rename-running-title = { $origin } : renommage en cours
tags-rename-progress = { $done } / { $total } fichiers
tags-rename-failed-n = { $n ->
    [one] 1 échec
   *[other] { $n } échecs
}
tags-rename-done-title = { $origin } : renommage terminé
tags-rename-report = { $changed } réécrits, { $unchanged } déjà sans la valeur, { $failed } échecs
tags-rename-broken = interrompu : { $reason }

help-media-type = affecter un Type
help-media-type-write = écrire
media-type-title = { $source } — { $n ->
    [one] 1 média
   *[other] { $n } médias
}
media-type-title-loading = Type
media-type-no-source = aucune source custom-tags (Type…) déclarée : utiliser e pour les tags
media-type-other = ＋ autre valeur…
media-type-remove = — retirer
media-type-new = nouvelle valeur
media-type-confirm = { $source } = « { $value } » pour { $n } médias
media-type-confirm-remove = retirer { $source } de { $n } médias
ev-bpm-analyzed = BPM estimés : { $estimated }
ev-bpm-analyzed-failed = BPM estimés : { $estimated }, non estimés : { $failed } ({ $why })
bpm-fail-decode = décodage impossible
bpm-fail-too-short = trop court
bpm-fail-no-rhythm = pas de rythme
bpm-fail-weak = pulsation faible
bpm-fail-competing = rythmes concurrents
bpm-fail-disagree = sections en désaccord
ev-connection-started = connexion { $id } sur { $mount } observée ; début estimé { $start } ; âge { $age } s
ev-connection-ended = connexion { $id } sur { $mount } terminée ; début estimé { $start } ; dernière présence { $last } ; fin observée { $end } ; durée ≥ { $age } s
mode-auto-sleep-age-short = VEILLE AUTO ≥ { $age }
mode-auto-sleep-age = Veille automatique active : si toutes les connexions ont au moins { $age }
mode-auto-sleep-zero-short = VEILLE AUTO À 0
mode-auto-sleep-zero = Veille automatique active : dès zéro auditeur
mode-unknown = Mode opérateur inconnu
ev-auto-sleep-age-enabled = { $plugin } : veille automatique activée, toutes les connexions doivent avoir au moins { $age }
ev-auto-sleep-zero-enabled = { $plugin } : veille automatique activée dès zéro auditeur

key-cycle-tabs = F6 / Maj+F6
help-cycle-tabs = Onglet suivant / précédent
help-plugin-columns = Faire défiler les colonnes
help-plugin-open = Ouvrir la vue du plugin
plugins-directory = Vues déclarées par les plugins. Entrée ouvre la première vue ; F6 parcourt tous les onglets.
plugins-capabilities = Capacités
plugins-tabs = Onglets
plugins-stale = Données anciennes / indisponibles
plugins-loading = Chargement…
plugins-row-count = { $count } lignes · actualisation toutes les 5 s
plugins-empty = Aucune donnée pour cette vue
key-plugin-columns = ← / →

key-plugin-next = Tab / n
key-plugin-previous = Maj+Tab / p
help-plugin-next = Vue suivante
help-plugin-previous = Vue précédente
config-draft = Brouillon modifié
config-draft-kept = Brouillon conservé. Esc pour abandonner avant de changer de plugin.
config-default = { $value } (défaut)
config-absent = absent
config-manager-stopped = Le plugin de configuration doit être chargé
config-preview = Prévisualisation validée — choisir une action
config-discarded = Brouillon abandonné
config-no-changes = Aucun changement de valeur
config-cancel = Annuler
config-save = Enregistrer
config-save-reload = Enregistrer et recharger
config-confirm-keys = ←/→ choisir · Entrée confirmer · Esc annuler
config-no-schema = Ce plugin ne propose pas de formulaire disponible.
config-target = Plugin : { $plugin } · ←/→ changer de plugin
config-pending = Configuration à appliquer.
config-busy = Traitement…
config-edit-keys = ↑/↓ champ · Entrée modifier · Suppr défaut · Ctrl+S prévisualiser
    Esc abandonner · r actualiser · Tab/n onglet suivant · Maj+Tab/p précédent
config-saved = Configuration enregistrée ; rechargement nécessaire
config-applied = Configuration enregistrée et appliquée
config-panel-title = Configuration de { $plugin }
media-tree-title = Dossiers
media-tree-root = Médias
media-tree-content = Contenu de { $folder }
media-tree-tags = Artiste / titre
media-tree-genres = Genres : { $genres }
help-media-tree = Liste / arborescence
help-media-tree-focus = Dossiers / fichiers
help-media-tree-expand = Ouvrir / fermer le dossier
help-media-tree-files = Afficher les fichiers
