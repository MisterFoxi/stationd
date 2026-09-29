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
state-draining-long = VEILLE ARMÉE (veille dès 0 auditeur)
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
note-sleep-armed = veille armée : la station s'arrêtera dès qu'il n'y aura plus d'auditeur
note-live-on-air = DJ { $dj } à l'antenne : la suite dépend de la fin du live
note-no-liquidsoap = pas de [liquidsoap] : rien n'est diffusé, la suite montre ce que la grille choisirait
note-simulated = suite simulée : une suite possible (ordres aléatoires, overrides, live, grille modifiée peuvent la changer)
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
planned-agenda-1 = Jour : timeline, bases, rendez-vous, every projetés
planned-agenda-2 = Semaine : 7 colonnes, pas 15/30/60 min
planned-agenda-3 = Couverture de la grille, édition de règle
planned-tags-1 = Types : valeurs déclarées, effectifs, médias sans Type
planned-tags-2 = Tags libres : créer, renommer, fusionner
planned-system-1 = Santé : stationd, Liquidsoap, Icecast (mounts), live
planned-system-2 = Événements en direct, statistiques de diffusion
planned-plugins-1 = Vues déclarées par les plugins chargés
planned-plugins-2 = Base de chaque plugin (info, requête en lecture seule)

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
form-override-target = Chemin sous media/ ou référence de playlist
form-override-mode = Mode
form-override-soft = SOFT : au prochain morceau
form-override-hard = HARD : coupe maintenant
form-override-expiry = Péremption (30s, 5m, 2h ; vide = jamais)
form-override-tracks = Pistes tenues (playlist)
override-media = Média : { $path }
override-playlist = Playlist : { $playlist }
confirm-override-title = Pousser cet override ?
confirm-override-soft = Passe au prochain bord de morceau.
confirm-override-hard = Coupe le morceau en cours MAINTENANT.
confirm-override-tracks = { $n ->
    [one] Tient 1 piste.
   *[other] Tient { $n } pistes.
}
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
media-search-hint = / pour chercher : mots (titre, artiste, album, chemin), genre:x, dossier:x
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
card-keys = Échap fermer · ↑↓ précédent / suivant · o override · p playlist · f file
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
