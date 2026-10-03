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
key-help = ? / F1
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
state-draining-long = SLEEP ARMED
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
note-sleep-armed = sleep armed: stops at a track boundary if the sleep conditions still hold
note-live-on-air = DJ { $dj } on air: what follows depends on the end of the live
note-no-liquidsoap = no [liquidsoap]: nothing airs; this is what the grid would pick
note-simulated = simulated: what the station will play if nothing changes meanwhile (an override, a live, a new grid, a rescan can change it)
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
done-scan = scan: { $found } found, { $skipped } skipped, { $vanished } gone at this scan ({ $unavailable } gone in all)
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
form-override-target = Target (Enter: pick from the list, or type a path / reference)
form-override-mode = Mode
form-override-soft = SOFT: at the next track
form-override-hard = HARD: cuts now
form-override-expiry = Expiry (30s, 5m, 2h; empty = never)
form-override-tracks = Tracks held (playlist; empty = a group's whole cycle, else 1)
override-media = Media: { $path }
override-playlist = Playlist: { $playlist }
confirm-override-title = Push this override?
confirm-override-soft = Airs at the next track boundary.
confirm-override-hard = Cuts the current track NOW.
confirm-override-tracks = { $n ->
    [one] Holds 1 track.
   *[other] Holds { $n } tracks.
}
confirm-override-tracks-auto = Holds the whole cycle if it is a group (from the top), else 1 track.
override-left-auto = auto
form-pick-media = Pick a media — / search · Enter pick · Esc cancel
form-pick-playlist = Pick a playlist
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
control-scan-vanished = Gone at this scan
control-scan-unavailable = Gone in all
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
media-search-hint = / to search: words (title, artist, album, path), genre:x, dossier:x, age:<10d (creation)
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

## --- Lot 4b: playlists, editor, media card --------------------------------

key-p = p
key-f = f
key-shift-r = R
key-ctrl-s = Ctrl+S
key-ctrl-t = Ctrl+T
key-ctrl-n = Ctrl+N
key-ctrl-d = Ctrl+D
key-alt-updown = Alt+↑ / ↓
key-left-right = ← / → / Space
key-f8 = F8

help-close = close
help-pl-edit = edit
help-pl-new = new playlist
help-pl-delete = delete
help-pl-filter = filter
help-pl-reload-root = re-read every file
help-ed-save = save
help-ed-next = next field (Shift+Tab: previous)
help-ed-choice = change the choice
help-ed-add = add (filter, member, media)
help-ed-remove = remove the item
help-ed-move = move the item
help-ed-raw = edit the raw TOML
help-ed-form = back to the form
help-ed-next-diag = go to the next problem
help-media-card = media card
help-media-mark = mark / unmark
help-media-clear-marks = unmark all
help-media-to-playlist = add to a static playlist
help-media-enqueue = enqueue
help-card-prev-next = previous / next media
help-picker-add = add the marked media (or this one)

dialog-info-keys = Enter / Esc: close
picker-keys = ↑↓ choose · Enter confirm · Esc cancel
picker-loading = loading the list…
picker-new = New playlist…
picker-none = no playlist matches

mode-static = static
mode-dynamic = dynamic
mode-remote = relay
mode-queue = queue
mode-group = group

val-shuffle = random
val-sequential = in order
val-newest = newest first
val-oldest = oldest first
val-fifo = first in first
val-lifo = last in first
val-all = every filter
val-any = at least one filter
val-filename = file name
val-mtime = file date
val-published = publication date
val-weighted = weighted
val-rotate = in turn
val-sequence = one after another
val-abort = the whole group yields
val-skip = skip to the next
val-fallthrough = yield
val-stop = stop
val-disable = disable itself
val-hold = keep the air
val-yes = yes
val-no = no
val-duration-s = duration (s)
val-age = age (creation, e.g. 10d)
val-creation = creation date
val-tempo = tempo
val-genre-ai = AI genre
val-mood = Mood
val-prefix = starts with
val-eq = equals
val-ne = differs from
val-contains = contains
val-has = has the genre
val-has-any = any of
val-has-all = all of
val-has-none = none of

pl-summary = { $n ->
    [one] { $n } playlist
   *[other] { $n } playlists
} · sort: { $sort } · / filter
pl-none = no playlist
pl-col-ref = file (ref)
pl-col-name = name
pl-col-mode = mode
pl-col-used = used by
pl-pool = pool
pl-used-rules-n = { $n ->
    [one] { $n } rule
   *[other] { $n } rules
}
pl-used-groups-n = { $n ->
    [one] { $n } group
   *[other] { $n } groups
}
pl-used-none = nothing (no grid rule, no group)
pl-used-rules = grid rules: { $list }
pl-used-groups = groups: { $list }
pl-disabled = disabled
pl-detail = Detail
pl-file = file
pl-no-file = no file
pl-no-file-long = none (entry added by stationctl add)
pl-file-differs = the file differs from what is applied (edited by hand, or invalid)
pl-edit-no-file = This playlist was added without a file (stationctl add): edit it where its TOML lives, then stationctl add again.
pl-open-failed = Cannot open
pl-opening = opening…
pl-new-title = New playlist
pl-new-mode = Playlist mode (can be changed later):
pl-delete-title = Delete a playlist
pl-delete-refused = "{ $playlist }" cannot be deleted while it is referenced:
pl-delete-body = Delete "{ $playlist }" ({ $name })?
pl-delete-file = Its file { $file } will be erased from the node.
pl-delete-no-file = It has no file: only its entry is removed.
pl-delete-yes = Delete
pl-reload-title = Re-read the playlists
pl-reload-body = stationd re-reads every playlist file of the node and drops those whose file is gone (unless a rule or a group still references them).
pl-reload-yes = Re-read
pl-busy-title = Draft in progress
pl-busy-body = Another playlist draft is open: save or close it, then try again from Media.

pl-h-identity = Identity
pl-h-selection = Selection
pl-h-broadcast = Broadcast
pl-h-files = { $n ->
    [one] Media ({ $n })
   *[other] Media ({ $n })
}
pl-h-filters = { $n ->
    [one] Filter ({ $n })
   *[other] Filters ({ $n })
}
pl-h-members = { $n ->
    [one] Member ({ $n })
   *[other] Members ({ $n })
}
pl-f-ref = file (ref)
pl-f-name = name
pl-f-enabled = enabled
pl-f-mode = mode
pl-f-order = order
pl-f-match = combination
pl-f-order-by = date used
pl-f-unplayed = air once only
pl-f-url = stream address
pl-f-max-len = maximum length
pl-f-strategy = strategy
pl-f-on-member-unavailable = member without media
pl-f-filter = filter { $n }
pl-f-op = operator
pl-f-value = value
pl-f-member = member { $n }
pl-f-weight = weight
pl-f-take-random-min = random tracks: minimum
pl-f-take-random-max = random tracks: maximum
ag-take-random = { $min } to { $max } tracks (random)
pl-f-take = tracks
pl-f-runtime = duration
pl-f-limit = tracks per turn
pl-f-repeat = restart from the top
pl-f-on-exhausted = once exhausted
pl-f-no-same-artist = same artist, not before
pl-f-no-same-track = same file, not before
pl-f-no-same-title = same song, not before
pl-absent = — (not set)
pl-add-files = add media
pl-add-filter = add a filter
pl-add-member = add a member

ed-title = Playlist { $reference }
ed-title-new = New playlist { $reference }
ed-modified = modified
ed-revision = rev. { $rev }
ed-raw-mode = raw TOML
ed-form = Form
ed-toml = TOML
ed-toml-keys = Ctrl+T to edit it
ed-toml-keys-raw = Esc or Ctrl+T: form · Ctrl+S: save
ed-unreadable = The TOML no longer parses: the form waits until it is fixed.
ed-unreadable-hint = Ctrl+T to go back to the text editor; stationd names the faulty line.
ed-file-differs = the file { $file } differs from what is applied: the file is what is open
ed-no-file = no file on the node: saving will create it
ed-files-added = { $n } of { $total } media added
ed-ref-required = give the file (ref) of the new playlist, e.g. show/intro
ed-saving = saving…
ed-save-failed = cannot save: { $reason }
ed-saved = saved ({ $file })
ed-created = created ({ $file })
ed-reloaded = draft replaced by the node's file
ed-not-saved = { $n ->
    [one] not saved: { $n } error
   *[other] not saved: { $n } errors
}
ed-conflict-short = conflict: the file changed
ed-conflict-title = The file changed in the meantime
ed-conflict-body = { $file } was modified since it was opened (by someone else, or by hand). Nothing was written.
ed-conflict-never = A save never overwrites another change: compare, then reload and redo your changes.
ed-conflict-keep = Keep the draft
ed-conflict-compare = Compare
ed-conflict-reload = Reload (draft lost)
ed-compare-title = Node file / draft
ed-compare-keys = ↑↓ PgUp PgDn scroll · Esc close · orange lines: different
ed-compare-disk = On the node (rev. { $rev })
ed-compare-draft = Your draft
ed-discard-title = Modified draft
ed-discard-body = The changes to this draft are not saved.
ed-discard-keep = Keep editing
ed-discard-yes = Discard the changes
ed-pick-media = Add media
ed-pick-member = Add a member
ed-genres = genres: { $list }
ed-genres-none = no known genre starts like this
ed-diags = { $errors } error(s), { $warnings } warning(s)
ed-diags-none = Diagnostics
ed-diags-ok = stationd sees no problem
ed-diag-label = { $label }:
ed-diag-file = file
ed-pool = Pool today
ed-pool-pending = stationd is computing…
ed-pool-invalid = invalid draft: no preview while there are errors
ed-pool-unmeasured = pool not measurable for this mode (relayed stream, queue)
ed-pool-unmeasured-short = not measurable
ed-pool-empty = empty pool: nothing will air
ed-pool-member-empty = no media
ed-pool-count = { $n ->
    [one] { $n } media
   *[other] { $n } media
}
ed-pool-artists = { $n ->
    [one] { $n } artist
   *[other] { $n } artists
}

diag-syntax = unreadable TOML ({ $detail })
diag-unknown-field = field unknown to the grammar
diag-missing-field = required field missing
diag-bad-value = invalid value
diag-not-allowed = field not allowed in this mode or strategy
diag-required-for-mode = required by this mode or strategy
diag-conflict = incompatible with another field
diag-bad-filter = invalid filter (field, operator or value)
diag-bad-duration = invalid duration (30s, 15m, 2h, 1d)
diag-unknown-ref = names no playlist
diag-bad-ref = invalid reference
diag-cycle = the group contains itself
diag-id-changed = the id cannot change
diag-empty-pool = no media matches today
diag-unknown = unknown problem (code { $code })
diag-rejected = : "{ $value }"
diag-expected = {" "}(expected: { $values })

media-marked = { $n } marked
media-choose-static = Add { $n } media to a static playlist
media-choose-queue = Enqueue { $n } media

card-title = Media card
card-keys = Esc close · ↑↓ previous / next · e tags · o override · p playlist · f queue
card-no-title = no title tag
card-size = size
card-size-mb = { $mb } MB
card-state = state
card-available = available
card-unavailable = gone from the disk
card-playlists = Playlists that can air it
card-no-playlist = none: this media only airs by override or queue
card-plays = Plays (aired / picked)
card-plays-legend = aired: really started on air · picked: chosen by stationd
card-24h = 24 h
card-7d = 7 d
card-30d = 30 d
card-all = total
card-last = last picked:
card-never = never

done-enqueued-many = { $n } media added to "{ $playlist }" ({ $len } queued)
done-enqueue-many-full = queue "{ $playlist }" full after { $n } of { $total } ({ $len } queued)
done-enqueue-partial = { $n } of { $total } enqueued, then: { $reason }
done-playlist-removed = playlist "{ $playlist }" deleted
done-playlist-removed-file = playlist "{ $playlist }" deleted (file { $file } erased)
done-playlists-reloaded = playlists re-read: { $added } applied, { $removed } dropped, { $errors } file(s) in error

## --- Tags des médias ---

help-media-edit-tags = edit the tags (in the file)
form-tags-title = Tags of { $path }
form-tags-many-title = Tags of { $n } files (empty = unchanged)
form-tags-nothing = nothing changed
form-tags-read-failed = Cannot read the tags
confirm-tags-title = Write into the file
confirm-tags-body = Write these tags into { $path }:
confirm-tags-many-body = Write these tags into { $n } files:
confirm-tags-set = { $field } → "{ $value }"
confirm-tags-remove = { $field }: removed
confirm-tags-yes = Write
done-tags-written = tags written into { $n } of { $total } file(s)
done-tags-conflicts = changed meanwhile, nothing written: { $list }
done-tags-failed = { $n } failure(s), including { $first }
done-tags-not-applied = { $path }: stationd did not write { $fields } (stationd older than the TUI?)
done-tags-no-readback = { $path }: stationd sent no tags read back, write not verified
card-tags = File tags
card-tags-manual = set by hand
card-tags-auto = derived
help-tags-write = write into the file(s)
help-tags-list = pick from the list (genres)
tags-form-keys = ↑↓ field · Enter list · Del clear the list · ←→ tempo · Ctrl+S write · Esc cancel
tags-genres = genres
tags-bpm = BPM
tags-tempo = tempo
tags-creation = creation date
tags-unchanged = unchanged
tags-cleared = all removed (in each file)
tags-tempo-auto = auto (from the BPM)
tags-tempo-auto-now = auto (from the BPM, now { $tempo })
tags-creation-now = auto (now { $date })
tags-bad-number = { $field }: a number from 1 to { $max }, or empty
tags-bad-creation = creation date: as 2026-06-14T06:36:48Z (RFC 3339), or empty
tags-help-single = empty = field removed from the file
tags-help-batch = empty = unchanged in each file
tags-help-list = Enter: pick (the files' own values first), or type a new one · Del: remove all
tags-help-tempo = ←→: a chosen label wins over the one from the BPM; "auto" goes back to the BPM
tags-help-bpm = the tempo comes from it (custom-tags ranges) unless chosen by hand
tags-help-creation = typed: wins over the date from the comment; empty: back to that one
tags-picker-title = { $field }: check (Space)
tags-picker-title-batch = { $field }: Space = remove (value present) or add, then unchanged
tags-picker-keys = type to filter · Space check · Enter confirm · Esc cancel
tags-picker-new = ＋ new: "{ $genre }"
tags-picker-none = no genre matches: Space adds it
confirm-tags-merge = { $field }: + { $add } / − { $remove }

## --- Lot 6a: agenda ---------------------------------------------------------
key-brackets = [ / ]
key-t = t
key-g = g
key-arrows = ← → ↑ ↓
key-page = PgUp / PgDn
help-ag-slot = previous / next slot
help-ag-day = previous / next day
help-ag-week-shift = previous / next week
help-ag-today = today, now
help-ag-goto = go to a date (calendar)
help-ag-week = week view
help-ag-day-view = day view
help-ag-coverage = grid coverage
help-ag-back = back to the agenda
help-ag-step = 15 / 30 / 60 min step
help-ag-item = next item of the slot
help-ag-inspector = inspector (narrow terminal)
help-ag-playlist = open the playlist
help-ag-cell = pick a cell
help-ag-open-day = open this day
help-ag-rule = previous / next rule
help-ag-cal-move = day / week
help-ag-cal-month = previous / next month
help-ag-cal-pick = show this day
ag-no-tz = station timezone unknown: waiting for stationd…
ag-bad-date = date out of the calendar
ag-view-day = Day
ag-view-week = Week
ag-view-coverage = Coverage
ag-weekday-short = { $wd ->
    [1] Mon
    [2] Tue
    [3] Wed
    [4] Thu
    [5] Fri
    [6] Sat
   *[7] Sun
}
ag-date-long = { $wd ->
    [1] Monday
    [2] Tuesday
    [3] Wednesday
    [4] Thursday
    [5] Friday
    [6] Saturday
   *[7] Sunday
} { $date }
ag-date-short = { ag-weekday-short } { $date }
ag-week-of = week of { $from } to { $to }
ag-month = { $month ->
    [1] January
    [2] February
    [3] March
    [4] April
    [5] May
    [6] June
    [7] July
    [8] August
    [9] September
    [10] October
    [11] November
   *[12] December
} { $year }
ag-step = { $n } min step
ag-utc = UTC times (station: { $tz })
ag-utc-rules = rules stay in station time
ag-dst = clock change: { $h } h day
ag-dst-week = clock change this week
ag-stale = old data, reload failed: { $reason }
ag-gap = (skipped)
ag-inspector = Slot
ag-slot-empty = nothing projected in this slot
ag-until-end = end of the period
ag-origin-daypart = day part
ag-origin-base = base rotation
ag-origin-hard = hard rendez-vous
ag-origin-soft = soft rendez-vous
ag-origin-every = every
ag-origin-fallback = safety fallback
ag-kind-at-clock = rendez-vous (at_clock)
ag-kind-live = live slot
ag-rule = rule
ag-group = group
ag-coverage = coverage
ag-pool-unmeasured = not measurable
ag-take = take { $n }
ag-runtime = runtime { $d }
ag-rule-base = floor, all day
ag-rule-daypart = from { $start } to { $end }
ag-rule-daypart-open = from { $start }, until the next day part
ag-rule-at-hourly = at { $marks } every hour
ag-rule-at-step = every { $n } min from { $first }
ag-rule-at = at { $at }
ag-hard = hard (cuts in)
ag-soft = soft (at the end of the track)
ag-rule-expiry = stale after { $d }
ag-rule-every-elapsed = every { $d } since the last play
ag-rule-every-tracks = every { $n } tracks
ag-rule-live = DJ { $dj } from { $start }
ag-rule-dates = from { $start } to { $end }
ag-rule-from = from { $start }
ag-rule-until = until { $end }
ag-rule-disabled = disabled
ag-live-before = opened earlier
ag-live-window = connection window { $opens } – { $closes }
ag-floating-title = Off the clock — since the last play or by track count
ag-floating-more = … and { $n } more (l: all rules)
ag-legend = ! hard rendez-vous · * soft · ♪ live · ▶ now
ag-day-summary = { $hard } hard · { $soft } soft · { $live } live
ag-calendar = Go to a date
ag-calendar-keys = arrows · PgUp/PgDn · t · Enter
cov-grid = Grid coverage:
cov-rules = { $n ->
    [one] { $n } rule
   *[other] { $n } rules
}
cov-none = no active rule in the grid
cov-ok = ok
cov-thin = thin
cov-insufficient = insufficient
cov-reason-ok = the pool covers what the rule asks for
cov-pool-empty = empty pool
cov-track-repeat = no_same_track_within { $window }: pool of only { $pool } (a track will replay)
cov-title-repeat = no_same_title_within { $window }: pool of only { $pool } (a song will replay)
cov-artist-repeat = no_same_artist_within: { $n } distinct artist(s) (an artist will replay)
cov-artist-not-evaluated = no_same_artist_within not evaluated (group aggregate)
cov-limit-unmet = limit { $limit }: { $n } distinct media in the pool
cov-finite-short = finite source of { $pool } for a { $need } slot (does not fill it)
cov-members-empty-abort = empty member(s): { $list } → the group stops (abort)
cov-members-empty-skip = empty member(s): { $list } → skipped (degraded)
cov-members-loop = undersized member(s): { $list } → loops within the slot
cov-bad-ref = invalid reference: { $reason }
cov-unknown-playlist = unknown playlist (broken reference)
cov-unreadable-playlist = unreadable playlist: { $reason }
cov-unresolvable = pool cannot be resolved: { $reason }
cov-runtime-loop = runtime budget { $need } > pool of { $pool } → loops within the slot
cov-take-repeat = take { $take } > { $n } distinct track(s) → repeats
cov-unknown = unknown cause (code { $code }): stationd newer than the TUI?
pl-reveal-missing-title = Playlist not found
pl-reveal-missing = Playlist “{ $playlist }” is not in the list (broken grid reference?).

## --- Lot 6b: editing the grid in the agenda ------------------------------------
key-shift-g = G
key-u = u
key-ctrl-r = Ctrl+R
key-left-right-space = ← → / Space
help-ag-new = new event at this slot
help-ag-edit = edit the rule
help-ag-delete = delete the rule
help-ag-grids = grids (view, activate, copy)
help-ag-utc = local / UTC times
help-ag-grid-move = previous / next grid
help-ag-grid-view = show this grid
help-ag-grid-activate = make it active
help-ag-grid-copy = copy under a new name
help-ag-kind-move = previous / next kind
help-ag-kind-pick = create
help-ag-rules = all rules of the grid
ag-rules-title = Rules of grid { $grid }
ag-rules-keys = Enter edit · d delete · n new · Esc
ag-rules-none = no rule
help-ag-cancel = close
help-rf-field = previous / next field
help-rf-choice = change the choice, tick a day
help-rf-enter = pick the playlist / next field
help-rf-save = save the grid
help-rf-rebase = reread the file (conflict), keeping the input
help-rf-close = close
ag-grid-active = grid { $grid } (active)
ag-grid-other = GRID IN PREPARATION: { $grid } (not on air)
ag-grids-title = Grids of the node
ag-grids-keys = Enter view · a activate · c copy · Esc
ag-grids-none = no grid (the grid directory is empty)
ag-grids-no-file = no file (the grid last applied stays)
ag-grids-rules = { $n ->
    [one] { $n } rule
   *[other] { $n } rules
}
ag-grids-unreadable = unreadable
ag-grids-active = active
ag-grids-viewed = shown
ag-activate-title = Activate a grid
ag-activate-body = Grid { $grid } goes on air: it replaces the active grid, right now.
ag-activate-yes = Activate
ag-copy-title = Copy grid { $grid }
ag-copy-name = Name of the new grid
ag-copy-need-name = Give a name (e.g. summer)
ag-new-title = New event
ag-new-kind = Kind of rule:
ag-grid-unreadable = Unreadable grid
ag-grid-toml-broken = File { $grid } does not parse: fix it by hand (or `stationctl schedule show`).
ag-rule-missing-title = Rule not found
ag-rule-missing = Rule { $rule } is not in file { $grid } (edited by hand since?): `r` to reload.
ag-delete-title = Delete a rule
ag-delete-body = Rule { $rule } is removed from file { $grid }.
ag-delete-active = This is the active grid: the air changes right away.
ag-delete-other = Grid in preparation: the air does not change.
ag-delete-yes = Delete
done-rule-deleted = rule { $rule } deleted from { $grid }
done-grid-activated = active grid: { $grid } ({ $n } rules)
done-grid-not-activated = { $grid } is not activated
done-grid-not-saved = { $grid } is not saved
done-grid-conflict = { $grid } changed since it was read: nothing written, reload (r) and try again
done-grid-copied = grid { $to } created (copy of { $from }), not active
done-grid-exists = grid { $grid } already exists: nothing written
done-grid-more = { $n ->
    [one] {" "}(and { $n } other problem)
   *[other] {" "}(and { $n } other problems)
}
gdiag-not-allowed = field of another kind of rule
gdiag-bad-time = time as HH:MM (00:00 to 23:59)
gdiag-bad-date = date as YYYY-MM-DD
gdiag-bad-weekday = day: mon, tue, wed, thu, fri, sat, sun, no duplicate
gdiag-schema-version = unsupported grid version
gdiag-duplicate-id = this id is already another rule's
gdiag-several-floors = only one floor (base_rotation) per grid
gdiag-zero-window = same start and end: the day part covers nothing
gdiag-dates-reversed = the end date is before the start date
gdiag-out-of-range = number out of range
gdiag-unknown-dj = DJ not in the DJ file
gdiag-no-live = live slot without a [live] section in stationd.toml
gdiag-dj-file = DJ file unreadable: { $detail }
gdiag-file = grid file unreadable: { $detail }
rf-title-new = New rule: { $kind }
rf-title-edit = Rule { $id }
rf-grid-active = in the active grid { $grid } (applied on save)
rf-grid-other = in grid { $grid } (in preparation, not on air)
rf-modified = modified
rf-local-times = times and dates in the station timezone
rf-id = id
rf-playlist = playlist
rf-dj = DJ
rf-start = start
rf-end = end
rf-anchor = mark
rf-anchor-at = once a day, at a fixed time
rf-anchor-minute = every hour, at a minute
rf-anchor-every = several times an hour
rf-at = time
rf-every-minutes = every (min)
rf-minute = minute
rf-mode = mode
rf-expiry = expiry
rf-cadence = cadence
rf-cadence-elapsed = elapsed time
rf-cadence-tracks = number of tracks
rf-min-elapsed = at most every
rf-min-tracks = at most every (tracks)
rf-days = days
rf-days-all = (none ticked = every day)
rf-date-start = from
rf-date-end = until
rf-hint-time = HH:MM, station time
rf-hint-end = HH:MM; empty = until the next day part; before the start = crosses midnight
rf-hint-every-minutes = 1 to 1439, counted from midnight (45 → 00:45 01:30 … 23:15)
rf-hint-minute = 0 to 59, one mark per hour (0 → :00, 58 → :58)
rf-hint-expiry = 30s, 5m, 2h; empty = never stale
rf-hint-duration = 30s, 15m, 2h, 1d
rf-hint-tracks = a number of tracks (1 or more)
rf-hint-date = YYYY-MM-DD; empty = no limit; both equal = that day only
rf-hint-days = ← → to move, Space to tick
rf-hint-playlist = Enter: pick from the list, or type the ref
rf-pick-playlist = Playlist of the rule
rf-preview = The day with the change
rf-preview-bases = Bases:
rf-preview-marks = { $n ->
    [one] This rule, { $n } time:
   *[other] This rule, { $n } times:
}
rf-preview-none = This rule does not play that day (days, dates, time, or a rule with priority).
rf-preview-invalid = the day is projected once the grid has no error left
rf-preview-others = { $n ->
    [one] { $n } other mark that day
   *[other] { $n } other marks that day
}
rf-other-problems = Other problems of the grid (they also block saving):
rf-saving = saving…
rf-saved = rule { $id } saved in { $grid } (grid in preparation)
rf-saved-applied = rule { $id } saved in { $grid }: on air
rf-invalid = nothing written: fix the fields marked ✗
rf-conflict = the file changed since it was opened: nothing written — Ctrl+R rereads it, keeping the input
rf-rebasing = rereading the file…
rf-rebased = file reread, input carried over: check, then Ctrl+S
rf-confirm-quit = unsaved changes: Esc again to drop them

# ── Journal, scan, Tags, System (lots 7–8) ──────────────────────────────
key-b = b
key-end = End
rpc-too-old = this stationd does not know this function (update it)
stream-closed = stream closed by stationd
journal-opening = journal: opening…
help-cancel = cancel
help-back = back
help-reload = reload
help-search-apply = keep the search
help-search-clear = clear the search
control-scan-running = scan running
scan-phase-listing = walking the folders…
scan-phase-reading = reading files { $done } / { $total }
scan-phase-analyzing = estimating BPMs { $done } / { $total }
scan-phase-plugins = plugins (on_scan)…
scan-phase-writing = writing derived values…
scan-phase-indexing = updating the index…

sys-state = State
sys-journal = Journal
sys-stats = Statistics
sys-stationd = stationd
sys-station = station
sys-version = version
sys-uptime = up for
sys-pid = process
sys-timezone = time zone
sys-journal-field = journal
sys-journal-open = following ({ $n } events)
sys-liquidsoap = Liquidsoap
sys-not-configured = not configured
sys-ls-air = air
sys-ls-on-air = on air
sys-ls-last-pull = last request
sys-ls-started = tracks started
sys-ls-control = control socket
sys-ok = ok
sys-icecast = Icecast
sys-ic-server = server
sys-ic-last-read = last read
sys-ic-audience = listeners
sys-ic-problem = problem
sys-mount-absent = missing
sys-mount-no-source = no source
sys-mount-ok = fed
sys-mount-rate = { $declared } kbit/s declared, { $real } received
sys-mount-rate-declared = { $declared } kbit/s declared
sys-live = Live
sys-live-on-air = on air
sys-live-nobody = nobody
sys-live-harbor = harbor
sys-live-refused = last refusal
sys-scan = Library
sys-scan-running = scan
sys-scan-last = last scan
sys-scan-last-ok = { $when }: { $found } files, { $skipped } skipped
sys-scan-last-failed = { $when }: failed — { $error }
sys-scan-none = no scan since stationd started

help-journal-scroll = scroll up / down (pause)
help-journal-follow = follow live
help-journal-level = level
help-journal-component = component
help-journal-search = search
journal-level = level
journal-level-all = all
journal-level-warn = warnings and errors
journal-level-error = errors
journal-component = component
journal-component-all = all
journal-following = live
journal-paused = paused — { $n } new (End to resume)
journal-search = search
journal-empty = nothing to show

evc-station = station
evc-broadcast = broadcast
evc-grid = grid
evc-library = library
evc-plugin = plugin
evc-live = live
evc-liquidsoap = Liquidsoap
evc-icecast = Icecast
evc-unknown = ?

ev-started = stationd started (version { $version }, “{ $station }”)
ev-stopping-request = shutdown requested
ev-stopping-signal = shutting down ({ $signal })
ev-broadcast-state = broadcast: { $from } → { $to } (by { $by })
ev-listeners = listeners: { $count }
ev-audience-unknown = audience unknown (Icecast unreadable)
ev-track-chosen = chosen: { $media } ({ $playlist }, { $origin })
ev-track-fallback = nothing to air: safety fallback ({ $origin })
ev-override-pushed = { $mode } override: { $what } (by { $by })
ev-override-dropped = override dropped, could not air: { $what } (by { $by }) — { $reason }
ev-grid-applied = grid { $grid } applied ({ $rules } rules)
ev-grid-refused = grid { $grid } refused ({ $problems } problems): the previous grid stays on air
ev-grid-unreadable = grid { $grid } unreadable: { $error }
ev-incident-hard-not-cut = hard rendez-vous { $rule } with nothing to air ({ $playlist }): no cut
ev-incident-source-empty = { $rule }: { $playlist } gave nothing, the grid falls through
ev-live-started = live: { $dj } on air ({ $rule })
ev-live-ended = live ended: { $dj } ({ $reason })
ev-scan-started = library scan started
ev-scan-finished = scan finished: { $found } files, { $skipped } skipped, { $vanished } vanished
ev-scan-failed = scan failed: { $error }
ev-tags-written = tags written: { $media }
ev-tag-renamed = { $origin }: “{ $from }” → “{ $to }” ({ $files } files, { $failed } failed)
ev-plugin-failed = plugin { $plugin } failed ({ $phase }): { $reason }
ev-plugin-quarantined = plugin { $plugin } quarantined ({ $failures } failures)
ev-plugin-state = plugin { $plugin }: { $state }
ev-unknown = unknown event (stationd newer than the TUI?)

stats-window = window
stats-window-30m = last 30 minutes
stats-window-24h = last 24 hours
stats-window-7d = last 7 days
stats-window-30d = last 30 days
stats-by = by
stats-by-playlist = playlist
stats-by-leaf = leaf playlist
stats-by-rule = rule
stats-by-origin = origin
stats-by-media = media
stats-by-artist = artist
stats-aired = aired
stats-picked = picked
stats-last = last
stats-none = nothing aired in this window
stats-unknown = (unknown)
stats-total = total: { $aired } aired, { $picked } picked
help-stats-window = window
help-stats-by = grouping

tags-origin-file = file genre
tags-no-source = no custom-tags source (Type…): only the file genre is shown
tags-value = value
tags-count = media
tags-spellings = spellings: { $list }
tags-empty = no value known yet — a library scan collects them (Control › Library)
tags-without = { $origin }: { $n ->
    [one] 1 media without a value
   *[other] { $n } media without a value
}
tags-summary = { $n } values · sorted by { $sort }
tags-sort-name = name
tags-sort-count = count
help-tags-origin = next origin
help-tags-rename = rename / merge
help-tags-sort = sort by name / count
help-tags-preview = see what changes
help-tags-apply = rename
tags-rename-title = { $origin }: rename “{ $from }”
tags-rename-hint = New value (an existing value = merge). Every spelling is replaced.
tags-rename-preview-title = { $origin }: preview
tags-rename-unexpected = unexpected answer from stationd
tags-rename-nothing = no available media carries this value
tags-rename-files = { $n ->
    [one] 1 file will be rewritten
   *[other] { $n } files will be rewritten
}
tags-rename-merges = “{ $to }” already exists: both values will be merged
tags-rename-no-playlist = no playlist filters on this value
tags-rename-playlists = { $n ->
    [one] 1 playlist filters on this value (it will select differently):
   *[other] { $n } playlists filter on this value (they will select differently):
}
tags-rename-confirm = Enter: rename · Esc: cancel
tags-rename-running-title = { $origin }: renaming
tags-rename-progress = { $done } / { $total } files
tags-rename-failed-n = { $n } failed
tags-rename-done-title = { $origin }: rename finished
tags-rename-report = { $changed } rewritten, { $unchanged } already without it, { $failed } failed
tags-rename-broken = interrupted: { $reason }

help-media-type = set a Type
help-media-type-write = write
media-type-title = { $source } — { $n } media
media-type-title-loading = Type
media-type-no-source = no custom-tags source (Type…) declared: use e for tags
media-type-other = ＋ other value…
media-type-remove = — remove
media-type-new = new value
media-type-confirm = { $source } = “{ $value }” for { $n } media
media-type-confirm-remove = remove { $source } from { $n } media
ev-bpm-analyzed = BPMs estimated: { $estimated }
ev-bpm-analyzed-failed = BPMs estimated: { $estimated }, not estimated: { $failed } ({ $why })
bpm-fail-decode = cannot decode
bpm-fail-too-short = too short
bpm-fail-no-rhythm = no rhythm
bpm-fail-weak = weak pulse
bpm-fail-competing = competing rhythms
bpm-fail-disagree = sections disagree
ev-connection-started = connection { $id } on { $mount } observed; estimated start { $start }; age { $age } s
ev-connection-ended = connection { $id } on { $mount } ended; estimated start { $start }; last seen { $last }; end observed { $end }; duration ≥ { $age } s
mode-auto-sleep-age-short = AUTO SLEEP ≥ { $age }
mode-auto-sleep-age = Automatic sleep active: when all connections are at least { $age } old
mode-auto-sleep-zero-short = AUTO SLEEP AT 0
mode-auto-sleep-zero = Automatic sleep active: at zero listeners
mode-unknown = Unknown operating mode
ev-auto-sleep-age-enabled = { $plugin }: automatic sleep enabled when all connections are at least { $age } old
ev-auto-sleep-zero-enabled = { $plugin }: automatic sleep enabled at zero listeners

key-cycle-tabs = F6 / Shift+F6
help-cycle-tabs = Next / previous tab
help-plugin-columns = Scroll columns
help-plugin-open = Open plugin view
plugins-directory = Plugin-declared views. Enter opens the first view; F6 cycles through all tabs.
plugins-capabilities = Capabilities
plugins-tabs = Tabs
plugins-stale = Stale / unavailable
plugins-loading = Loading…
plugins-row-count = { $count } rows · refresh every 5 s
plugins-empty = No data for this view
key-plugin-columns = ← / →

key-plugin-next = Tab / n
key-plugin-previous = Shift+Tab / p
help-plugin-next = Next view
help-plugin-previous = Previous view
config-draft = Draft modified
config-draft-kept = Draft retained. Esc discards it before switching plugins.
config-default = { $value } (default)
config-absent = absent
config-manager-stopped = The configuration plugin must be loaded
config-preview = Preview validated — choose an action
config-discarded = Draft discarded
config-no-changes = No value changes
config-cancel = Cancel
config-save = Save
config-save-reload = Save and reload
config-confirm-keys = ←/→ choose · Enter confirm · Esc cancel
config-no-schema = This plugin has no available configuration form.
config-target = Plugin: { $plugin } · ←/→ switch plugin
config-pending = Configuration waiting to be applied.
config-busy = Working…
config-edit-keys = ↑/↓ field · Enter edit · Delete reset · Ctrl+S preview
    Esc discard · r refresh · Tab/n next tab · Shift+Tab/p previous
config-saved = Configuration saved; reload required
config-applied = Configuration saved and applied
config-panel-title = Configuration for { $plugin }
media-tree-title = Folders
media-tree-root = Media
media-tree-content = Contents of { $folder }
media-tree-tags = Artist / title
media-tree-genres = Genres: { $genres }
help-media-tree = List / tree
help-media-tree-focus = Folders / files
help-media-tree-expand = Expand / collapse folder
help-media-tree-files = Browse files

help-pl-tree = List / groups
help-pl-tree-expand = Expand / collapse group
