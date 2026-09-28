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
planned-playlists-1 = List: mode, pool, rules and groups referencing it
planned-playlists-2 = Form per mode, live pool preview
planned-playlists-3 = Saved by stationd (revision, conflicts)
planned-agenda-1 = Day: timeline, bases, appointments, projected every
planned-agenda-2 = Week: 7 columns, 15/30/60 min steps
planned-agenda-3 = Grid coverage, rule editing
planned-tags-1 = Types: declared values, counts, media without a Type
planned-tags-2 = Free tags: create, rename, merge
planned-system-1 = Health: stationd, Liquidsoap, Icecast (mounts), live
planned-system-2 = Live events, broadcast statistics
planned-plugins-1 = Views declared by the loaded plugins
planned-plugins-2 = Each plugin's database (info, read-only query)

## Grid incidents (on-air notes)
note-rendezvous-will-not-cut = expected: rendez-vous { $rule } at { $time } will not cut — "{ $playlist }" has nothing to air
note-source-will-be-empty = expected: at { $time }, "{ $playlist }" ({ $rule }) will have nothing to air — the lower priority takes over
note-rendezvous-not-cut = seen: rendez-vous { $rule } did not cut at { $time } — "{ $playlist }" had nothing to air ({ $count ->
    [one] once
   *[other] { $count } times
})
note-source-was-empty = seen: "{ $playlist }" ({ $rule }) had nothing to air at { $time } ({ $count ->
    [one] once
   *[other] { $count } times
})
slot-pool-empty = empty pool: nothing will air
slot-nothing-playable = nothing playable (constraints, plugins)

## Keys (actions)
key-space = Space
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
help-pause-resume = pause / resume / wake
help-skip = skip to next
help-override = push an override
help-drain = sleep at 0 listeners
help-wake = wake up
help-section = next section
help-select = pick a row
help-override-remove = remove the picked override
help-override-clear = clear the queue
help-live-kick = kick the DJ
help-live-open = open a slot
help-live-close = close the picked opening
help-enqueue = add to a queue playlist
help-scan = scan the library
help-shutdown = operator stop
help-shutdown-force = forced stop (kicks the DJ)

## Actions: status line
status-action-running = action running…
status-action-failed = failed: { $reason }
status-cancelled = cancelled
done-state = broadcast: { $from } → { $to }
done-state-unchanged = broadcast already { $state }
done-skip = skip requested
done-override = override #{ $id } queued ({ $pending } pending)
done-override-degraded = override #{ $id } queued, played SOFT (no cut possible) — { $pending } pending
done-overrides-cleared = { $n ->
    [one] 1 override removed
   *[other] { $n } overrides removed
}
done-live-kicked = DJ { $dj } kicked
done-live-opened = slot opened for { $dj } until { $until }
done-live-closed = opening for { $dj } closed
done-enqueued = added to "{ $playlist }" ({ $len } queued)
done-enqueue-full = queue "{ $playlist }" is full ({ $len }): nothing added
done-scan = scan: { $found } found, { $skipped } skipped, { $unavailable } gone
done-plugin = plugin { $name }: { $state }
done-plugin-reason = plugin { $name }: { $state } ({ $reason })
done-shutdown = operator stop: Liquidsoap parked on the background noise, stationd exits
done-shutdown-fallback = operator stop: stationd exits, Liquidsoap's safety fallback takes the air

## Dialogs
dialog-cancel = Cancel
dialog-confirm-keys = ←/→ choose · Enter confirm · Esc cancel
dialog-form-keys = Tab/↑↓ field · ←/→ choice · Enter confirm · Esc cancel
form-required = "{ $field }" is required
form-positive-integer = "{ $field }": an integer ≥ 1
confirm-pause-title = Pause
confirm-pause-body = The air stops on "{ $track }" until resumed.
confirm-pause-yes = Pause
confirm-skip-title = Skip to next
confirm-skip-body = Cuts "{ $track }" now.
confirm-skip-next = Expected next: "{ $track }".
confirm-skip-yes = Skip
confirm-drain-title = Sleep at 0 listeners
confirm-drain-body = The station will go to sleep as soon as there is no listener left (background noise, no grid).
confirm-drain-yes = Arm sleep
form-override-title = Push an override
form-override-kind = Content
form-override-kind-media = Media
form-override-kind-playlist = Playlist
form-override-target = Path under media/ or playlist reference
form-override-mode = Mode
form-override-soft = SOFT: at the next track
form-override-hard = HARD: cuts now
form-override-expiry = Expiry (30s, 5m, 2h; empty = never)
form-override-tracks = Tracks held (playlist)
override-media = Media: { $path }
override-playlist = Playlist: { $playlist }
confirm-override-title = Push this override?
confirm-override-soft = Airs at the next track boundary.
confirm-override-hard = Cuts the current track NOW.
confirm-override-tracks = { $n ->
    [one] Holds 1 track.
   *[other] Holds { $n } tracks.
}
confirm-override-no-expiry = No expiry.
confirm-override-expiry = Dropped if not aired within { $expiry }.
confirm-override-yes = Push
confirm-clear-one-title = Remove the override
confirm-clear-one-body = Remove override #{ $id } ({ $what }) from the queue?
confirm-clear-all-title = Clear the override queue
confirm-clear-all-body = { $n ->
    [one] Remove the pending override?
   *[other] Remove the { $n } pending overrides?
}
confirm-clear-yes = Remove
confirm-kick-title = Kick the DJ
confirm-kick-body = Disconnect { $dj } now? They will be refused until the end of their slot.
confirm-kick-yes = Kick
form-live-open-title = Open a live slot
form-live-dj = DJ
form-live-duration = Duration (30m, 2h, 1d)
confirm-live-close-title = Close the opening
confirm-live-close-body = Close the opening for { $dj } now?
confirm-live-close-yes = Close
form-enqueue-title = Add to a queue playlist
form-enqueue-playlist = Playlist (queue mode)
form-enqueue-media = Path under media/
confirm-scan-title = Scan the library
confirm-scan-body = Rereads all of media/: can take a while on a big disk or an NFS mount. The air is not touched.
confirm-scan-yes = Scan
plugin-verb-start = start
plugin-verb-stop = stop
plugin-verb-restart = restart
plugin-verb-reload = reload
confirm-plugin-start = Start plugin { $name }
confirm-plugin-stop = Stop plugin { $name }
confirm-plugin-restart = Restart plugin { $name }
confirm-plugin-reload = Reload plugin { $name }
confirm-plugin-body = The new state shows in the plugin list.
confirm-shutdown-title = Operator stop
confirm-shutdown-body = stationd exits and Liquidsoap is parked on the background noise. To restart: stationctl station start.
confirm-shutdown-kicks = DJ { $dj } will be disconnected.
confirm-shutdown-refused-live = DJ { $dj } is on air: the stop will be refused (A to force).
confirm-shutdown-continue = Continue
confirm-shutdown-again-title = Confirm the stop
confirm-shutdown-again-body = Last confirmation: the station stops airing the grid.
confirm-shutdown-yes = Stop the station

## Control screen
control-broadcast = Broadcast
control-overrides = Overrides
control-live = Live
control-queue = Queue
control-library = Library
control-plugins = Plugins
control-station = Station
control-state = State
control-listeners = Listeners
control-on-air = On air
control-broadcast-hint = Space pauses or resumes, n skips, v arms sleep (at 0 listeners), w wakes up.
control-read-failed = cannot read: { $reason }
control-not-read = not read yet
control-more = … and { $n } more
control-overrides-none = no pending override
control-overrides-hint = o pushes a media or a playlist ahead of the grid.
control-col-content = Content
control-col-mode = Mode
control-col-left = Left
control-col-expires = Expires
control-col-source = By
control-col-plugin = Plugin
control-col-state = State
control-col-failures = Fails
control-col-reason = Reason
control-live-disabled = live not configured (no [live] section)
control-live-on-air = On air
control-live-nobody = nobody
control-live-session = { $access } access, since { $since }, { $address }
control-live-accounts = DJ accounts
control-live-djs = { $n } declared
control-live-djs-error = DJ file unreadable: { $reason }
control-live-urgent = Urgent right
control-live-cooldown = Cooldown
control-live-until = { $dj } until { $time }
control-live-refused = Refused
control-live-last-refusal = Last refusal
control-live-refusal = { $dj } at { $time }: { $reason }
control-live-openings = Openings:
control-live-no-opening = none
control-live-cut = cut
control-queue-hint = e adds a media to the buffer of a queue-mode playlist (listener requests, dedications). The playlist must exist and be in queue mode; stationd refuses otherwise, and says so.
control-scan-none = no scan started from this TUI
control-scan-hint = s rereads media/ and updates the index (files added, removed, changed).
control-scan-last = Last scan:
control-scan-found = Found
control-scan-present = Available
control-scan-unavailable = Gone
control-scan-skipped = Skipped
scan-skip-unreadable = unreadable
scan-skip-zero-duration = zero length
scan-skip-walk-error = walk
scan-skip-unknown = ?
control-plugins-none = no plugin declared
control-plugin-off = disabled
control-shutdown-hint = a: operator stop. stationd exits, Liquidsoap is parked on the background noise; nothing restarts before "stationctl station start". A: the same, kicking the DJ on air. Two confirmations.
control-shutdown-live = DJ { $dj } on air: a will be refused, A kicks them.

## Media screen
media-search-title = Search
media-search-hint = / to search: words (title, artist, album, path), genre:x, dossier:x
media-count = { $shown } / { $total }
media-sorted-by = sorted by { $field } { $dir }
media-missing = without { $field }
media-with-unavailable = missing files included
media-loading = loading…
media-none = no media matches
media-field-path = path
media-field-title = title
media-field-artist = artist
media-field-album = album
media-field-year = year
media-field-duration = length
media-field-genre = genre
key-slash = /
key-enter = Enter
key-esc = Esc
key-m = m
help-media-search = search
help-media-apply = keep the search
help-media-cancel = back to the previous search
help-media-sort = change the sort
help-media-desc = reverse the order
help-media-missing = filter "no title / artist / genre / year"
help-media-unavailable = include missing files
help-media-reload = reload
