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
planned-control-1 = Diffusion : pause, reprise, suivant, veille, réveil
planned-control-2 = Overrides : pousser, lister, vider
planned-control-3 = Live : couper le DJ, ouvrir / fermer un créneau
planned-control-4 = File queue, scan de la bibliothèque, plugins, arrêt opérateur
planned-playlists-1 = Liste : mode, pool, règles et groupes qui la référencent
planned-playlists-2 = Formulaire par mode, aperçu du pool en direct
planned-playlists-3 = Enregistrement par stationd (révision, conflits)
planned-agenda-1 = Jour : timeline, bases, rendez-vous, every projetés
planned-agenda-2 = Semaine : 7 colonnes, pas 15/30/60 min
planned-agenda-3 = Couverture de la grille, édition de règle
planned-media-1 = Recherche, filtres (Type en premier), tri, pagination
planned-media-2 = Fiche média, statistiques de diffusion
planned-media-3 = Scan avec avancement, Type en lot (touche t)
planned-tags-1 = Types : valeurs déclarées, effectifs, médias sans Type
planned-tags-2 = Tags libres : créer, renommer, fusionner
planned-system-1 = Santé : stationd, Liquidsoap, Icecast (mounts), live
planned-system-2 = Événements en direct, statistiques de diffusion
planned-plugins-1 = Vues déclarées par les plugins chargés
planned-plugins-2 = Base de chaque plugin (info, requête en lecture seule)
