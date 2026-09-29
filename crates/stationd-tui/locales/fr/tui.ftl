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
key-help = ?
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
planned-playlists-1 = Liste : mode, pool, règles et groupes qui la référencent
planned-playlists-2 = Formulaire par mode, aperçu du pool en direct
planned-playlists-3 = Enregistrement par stationd (révision, conflits)
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
