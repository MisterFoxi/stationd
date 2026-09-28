# stationd-tui — English.
# Same keys as fr/tui.ftl (checked by a test).

## Screens (tabs)
screen-antenne = On air
screen-control = Control
screen-playlists = Playlists
screen-agenda = Schedule
screen-media = Media
screen-tags = Tags
screen-system = System
screen-plugins = Plugins

## Keys and help
key-digits = 1…8
key-help = ?
key-quit = q
key-force-quit = Ctrl+Q
key-plus-minus = + / -
help-switch-screen = switch screen
help-screen-help = help for this screen
help-quit = quit (stationd keeps running)
help-force-quit = quit, even while typing
help-upcoming-count = tracks shown ahead
help-title-screen = Screen: { $screen }
help-legend = — unknown · ~ projected or stale
help-box-title = Help — Esc to close

## Status line
status-connecting = Connecting to stationd…
status-connected = Connected to stationd
status-error = Error: { $reason }
status-screen = screen: { $screen }
terminal-too-small =
    Terminal too small: { $width }×{ $height } (minimum { $min_width }×{ $min_height }).
    Enlarge the window, or press q to quit.

## Durations
duration-days = { $d }d { $h }h { $m }m
duration-hours = { $h }h { $m }m
duration-minutes = { $m }m { $s }s

## Banner
banner-station-unknown = station ?
banner-live = LIVE { $dj }
banner-overrides = { $n ->
    [one] { $n } pending override
   *[other] { $n } pending overrides
}
banner-overrides-short = { $n } ovr
banner-state-unknown = broadcast: unknown
banner-stale = ~stale
banner-listeners = listeners
banner-listeners-short = lst.
banner-listeners-unknown = { $label } —
banner-listeners-stale = { $label } ~{ $n } (stale)
banner-listeners-count = { $label } { $n }
banner-on-air = on air:
banner-tz-unknown = time zone “{ $tz }” not found
banner-tz-none = time zone —
banner-uptime = uptime { $t }
banner-uptime-short = up { $t }

## Broadcast states (banner)
state-running = ON AIR
state-paused = PAUSED
state-draining = SLEEP ARMED
state-draining-long = SLEEP ARMED (sleeps at 0 listeners)
state-sleeping = ASLEEP
state-sleeping-long = ASLEEP (background noise)
state-unknown = state ?

## Link with stationd
link-connecting = connecting to { $host }…
link-connected = connected
link-lost = stationd unreachable for { $since }
link-lost-short = unreachable { $since }

## On-air kind (Liquidsoap fallback view)
kind-fallback = FALLBACK
kind-halted = halted

## gRPC access
rpc-bad-address = invalid gRPC address “{ $addr }”: { $reason }
rpc-timeout = no answer within { $s } s
rpc-unreachable = stationd unreachable ({ $reason })
rpc-status = { $code }: { $reason }
rpc-no-onair = this stationd does not serve the on-air view (update it)

## On-air stream
onair-stream-opening = on-air stream: opening…
onair-stream-error = on-air stream: { $reason }
onair-link-lost = { $reason } — showing the last snapshot
onair-waiting = waiting for the first snapshot…

## On-air screen — now playing
onair-title = On air
onair-state-running = ON AIR
onair-state-paused = PAUSED
onair-state-draining = SLEEP ARMED
onair-state-sleeping = ASLEEP
onair-stream-continuous = continuous stream
onair-duration-unknown = unknown duration
onair-since = since { $time }
onair-live = LIVE — { $dj }
onair-fallback = FALLBACK (safety net)
onair-halted = halted (background noise)
onair-no-liquidsoap = no [liquidsoap]: nothing airs
onair-nothing-reported = nothing reported by Liquidsoap

## Tracks
track-relay = relay { $url }
track-untitled = { $file } (no title)
track-pushed-by = by { $source }

## Origins (the rule that picked the track)
origin-at-clock-hard = appointment (cut)
origin-at-clock-soft = appointment
origin-every = every
origin-day-part = day part
origin-base-rotation = base
origin-override = override
origin-fallback = FALLBACK

## On-air screen — playlists, next, played
playlists-title = Playlists
playlists-by-track-count = by track count:
next-title = Up next
next-prepared = prepared
next-cut-at = cut { $time }
next-nothing = nothing
played-title = Played
played-nothing = nothing yet
played-aired = aired
played-cut = cut
played-end-unknown = end ?

## On-air notes (opcodes sent by stationd)
note-station-paused = station paused: nothing follows until it resumes
note-station-sleeping = station asleep: nothing follows until it wakes
note-sleep-at-track-end = falls asleep at the end of this track (0 listeners)
note-sleep-armed = sleep armed: the station stops as soon as nobody listens
note-live-on-air = DJ { $dj } on air: what follows depends on the end of the live
note-no-liquidsoap = no [liquidsoap]: nothing airs; this is what the grid would pick
note-simulated = simulated: one possible sequence (shuffles, overrides, a live, a new grid can change it)
note-pool-empty = nothing left to air in the grid at that point: dead air (Liquidsoap fills it)
note-fallback = FALLBACK: no rule covers that moment of the grid
note-stream-unknown-duration = relay { $media }: unknown duration, no estimated time beyond
note-unknown-duration = { $media }: unknown duration (not indexed), no estimated time beyond
note-simulation-failed = simulation failed: { $reason }
note-plugin-filter-failed = plugin { $plugin } failed during the simulation: { $reason }
note-grid-projection-failed = grid projection failed: { $reason }
note-history-unreadable = history unreadable: { $reason }
note-unknown = unknown note (code { $code })

## Fallback without the on-air stream
fallback-title = On air (fallback)
fallback-on-air = On air
fallback-since = Since
fallback-since-value = { $time } ({ $ago } ago)
fallback-prepared = Prepared
fallback-no-liquidsoap = Liquidsoap not configured
fallback-unknown = On air: unknown

## Screens to come
planned-coming = Coming
planned-lot = batch { $lot }
planned-plugin-missing = plugin “{ $plugin }” not loaded
planned-unavailable = Unavailable: { $reason }
planned-control-1 = Broadcast: pause, resume, next, sleep, wake
planned-control-2 = Overrides: push, list, clear
planned-control-3 = Live: cut the DJ, open / close a slot
planned-control-4 = Queue playlists, library scan, plugins, operator stop
planned-playlists-1 = List: mode, pool, rules and groups referencing it
planned-playlists-2 = Form per mode, live pool preview
planned-playlists-3 = Saved by stationd (revision, conflicts)
planned-agenda-1 = Day: timeline, bases, appointments, projected every
planned-agenda-2 = Week: 7 columns, 15/30/60 min steps
planned-agenda-3 = Grid coverage, rule editing
planned-media-1 = Search, filters (Type first), sorting, paging
planned-media-2 = Media sheet, broadcast statistics
planned-media-3 = Scan with progress, Type in bulk (key t)
planned-tags-1 = Types: declared values, counts, media without a Type
planned-tags-2 = Free tags: create, rename, merge
planned-system-1 = Health: stationd, Liquidsoap, Icecast (mounts), live
planned-system-2 = Live events, broadcast statistics
planned-plugins-1 = Views declared by the loaded plugins
planned-plugins-2 = Each plugin's database (info, read-only query)
