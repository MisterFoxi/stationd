# stationd-tui — Deutsch.
# Gleiche Schlüssel wie fr/tui.ftl (per Test geprüft).
# Übersetzung ohne Muttersprachler-Korrektur: bitte gegenlesen lassen.

## Bildschirme (Reiter)
screen-antenne = Sendung
screen-control = Steuerung
screen-playlists = Playlists
screen-agenda = Sendeplan
screen-media = Medien
screen-tags = Tags
screen-system = System
screen-plugins = Plugins

## Tasten und Hilfe
key-digits = 1…8
key-help = ? / F1
key-quit = q
key-force-quit = Strg+Q
key-plus-minus = + / -
help-switch-screen = Bildschirm wechseln
help-screen-help = Hilfe zu diesem Bildschirm
help-quit = beenden (stationd läuft weiter)
help-force-quit = beenden, auch während einer Eingabe
help-upcoming-count = angezeigte kommende Titel
help-title-screen = Bildschirm: { $screen }
help-legend = — unbekannt · ~ geschätzt oder veraltet
help-box-title = Hilfe — Esc zum Schließen

## Statuszeile
status-connecting = Verbindung zu stationd…
status-connected = Mit stationd verbunden
status-error = Fehler: { $reason }
status-screen = Bildschirm: { $screen }
terminal-too-small =
    Terminal zu klein: { $width }×{ $height } (mindestens { $min_width }×{ $min_height }).
    Fenster vergrößern oder q zum Beenden.

## Dauern
duration-days = { $d }T { $h }h { $m }m
duration-hours = { $h }h { $m }m
duration-minutes = { $m }m { $s }s

## Kopfzeile
banner-station-unknown = Station ?
banner-live = LIVE { $dj }
banner-overrides = { $n ->
    [one] { $n } Override wartet
   *[other] { $n } Overrides warten
}
banner-overrides-short = { $n } Ovr
banner-state-unknown = Sendung: unbekannt
banner-stale = ~veraltet
banner-listeners = Hörer
banner-listeners-short = Hör.
banner-listeners-unknown = { $label } —
banner-listeners-stale = { $label } ~{ $n } (veraltet)
banner-listeners-count = { $label } { $n }
banner-on-air = auf Sendung:
banner-tz-unknown = Zeitzone „{ $tz }“ nicht gefunden
banner-tz-none = Zeitzone —
banner-uptime = Laufzeit { $t }
banner-uptime-short = up { $t }

## Sendezustände (Kopfzeile)
state-running = AUF SENDUNG
state-paused = PAUSE
state-draining = RUHE GEPLANT
state-draining-long = RUHE GEPLANT (bei 0 Hörern)
state-sleeping = RUHEZUSTAND
state-sleeping-long = RUHEZUSTAND (Hintergrundrauschen)
state-unknown = Zustand ?

## Verbindung zu stationd
link-connecting = Verbindung zu { $host }…
link-connected = verbunden
link-lost = stationd nicht erreichbar seit { $since }
link-lost-short = nicht erreichbar { $since }

## Art der Sendung (Liquidsoap-Ersatzansicht)
kind-fallback = FALLBACK
kind-halted = angehalten

## gRPC-Zugriff
rpc-bad-address = ungültige gRPC-Adresse „{ $addr }“: { $reason }
rpc-timeout = keine Antwort innerhalb von { $s } s
rpc-unreachable = stationd nicht erreichbar ({ $reason })
rpc-status = { $code }: { $reason }
rpc-no-onair = dieser stationd bietet die Sendeansicht nicht an (bitte aktualisieren)

## Datenstrom der Sendung
onair-stream-opening = Sendeansicht: wird geöffnet…
onair-stream-error = Sendeansicht: { $reason }
onair-link-lost = { $reason } — letzter Stand wird angezeigt
onair-waiting = warte auf den ersten Stand…

## Bildschirm Sendung — läuft gerade
onair-title = Auf Sendung
onair-state-running = AUF SENDUNG
onair-state-paused = PAUSE
onair-state-draining = RUHE GEPLANT
onair-state-sleeping = RUHEZUSTAND
onair-stream-continuous = Endlosstream
onair-duration-unknown = Dauer unbekannt
onair-since = seit { $time }
onair-live = LIVE — { $dj }
onair-fallback = FALLBACK (Sicherheitsnetz)
onair-halted = angehalten (Hintergrundrauschen)
onair-no-liquidsoap = kein [liquidsoap]: es wird nichts gesendet
onair-nothing-reported = nichts von Liquidsoap gemeldet

## Titel
track-relay = Weiterleitung { $url }
track-untitled = { $file } (ohne Titel)
track-pushed-by = von { $source }

## Herkunft (Regel, die den Titel gewählt hat)
origin-at-clock-hard = Termin (Schnitt)
origin-at-clock-soft = Termin
origin-every = every
origin-day-part = Zeitfenster
origin-base-rotation = Basis
origin-override = Override
origin-fallback = FALLBACK

## Bildschirm Sendung — Playlists, als Nächstes, gespielt
playlists-title = Playlists
playlists-by-track-count = nach Titelzahl:
next-title = Als Nächstes
next-prepared = vorbereitet
next-cut-at = geschnitten { $time }
next-nothing = nichts
played-title = Gespielt
played-nothing = noch nichts
played-aired = gesendet
played-cut = geschnitten
played-end-unknown = Ende ?

## Hinweise zur Sendung (Opcodes von stationd)
note-station-paused = Station pausiert: nichts folgt, bis sie fortgesetzt wird
note-station-sleeping = Station im Ruhezustand: nichts folgt bis zum Aufwachen
note-sleep-at-track-end = Ruhezustand am Ende dieses Titels (0 Hörer)
note-sleep-armed = Ruhe geplant: die Station stoppt, sobald niemand mehr zuhört
note-live-on-air = DJ { $dj } auf Sendung: das Weitere hängt vom Ende der Live-Sendung ab
note-no-liquidsoap = kein [liquidsoap]: es wird nichts gesendet; dies würde der Sendeplan wählen
note-simulated = simuliert: eine mögliche Abfolge (Zufallsreihenfolge, Overrides, Live-Sendung, neuer Sendeplan können sie ändern)
note-pool-empty = im Sendeplan ist dann nichts mehr sendbar: Stille (Liquidsoap überbrückt)
note-fallback = FALLBACK: keine Regel deckt diesen Zeitpunkt ab
note-stream-unknown-duration = Weiterleitung { $media }: Dauer unbekannt, keine Zeitschätzung darüber hinaus
note-unknown-duration = { $media }: Dauer unbekannt (nicht indexiert), keine Zeitschätzung darüber hinaus
note-simulation-failed = Simulation fehlgeschlagen: { $reason }
note-plugin-filter-failed = Plugin { $plugin } ist während der Simulation fehlgeschlagen: { $reason }
note-grid-projection-failed = Projektion des Sendeplans fehlgeschlagen: { $reason }
note-history-unreadable = Verlauf nicht lesbar: { $reason }
note-unknown = unbekannter Hinweis (Code { $code })

## Ersatzansicht ohne Datenstrom
fallback-title = Auf Sendung (Ersatz)
fallback-on-air = Auf Sendung
fallback-since = Seit
fallback-since-value = { $time } (vor { $ago })
fallback-prepared = Vorbereitet
fallback-no-liquidsoap = Liquidsoap nicht konfiguriert
fallback-unknown = Sendung unbekannt

## Kommende Bildschirme
planned-coming = Demnächst
planned-lot = Paket { $lot }
planned-plugin-missing = Plugin „{ $plugin }“ nicht geladen
planned-unavailable = Nicht verfügbar: { $reason }
planned-agenda-1 = Tag: Zeitleiste, Basen, Termine, geschätzte every
planned-agenda-2 = Woche: 7 Spalten, Raster 15/30/60 Min.
planned-agenda-3 = Abdeckung des Sendeplans, Regeln bearbeiten
planned-tags-1 = Typen: deklarierte Werte, Anzahlen, Medien ohne Typ
planned-tags-2 = Freie Tags: anlegen, umbenennen, zusammenführen
planned-system-1 = Zustand: stationd, Liquidsoap, Icecast (Mounts), Live
planned-system-2 = Ereignisse live, Sendestatistiken
planned-plugins-1 = Von geladenen Plugins deklarierte Ansichten
planned-plugins-2 = Datenbank jedes Plugins (Info, Nur-Lese-Abfrage)

## Rasterstörungen (Sendehinweise)
note-rendezvous-will-not-cut = erwartet: Termin { $rule } um { $time } schneidet nicht — „{ $playlist }“ hat nichts zu senden
note-source-will-be-empty = erwartet: um { $time } hat „{ $playlist }“ ({ $rule }) nichts zu senden — die niedrigere Priorität übernimmt
note-rendezvous-not-cut = festgestellt: Termin { $rule } hat um { $time } nicht geschnitten — „{ $playlist }“ hatte nichts zu senden ({ $count ->
    [one] 1 Mal
   *[other] { $count } Mal
})
note-source-was-empty = festgestellt: „{ $playlist }“ ({ $rule }) hatte um { $time } nichts zu senden ({ $count ->
    [one] 1 Mal
   *[other] { $count } Mal
})
slot-pool-empty = leerer Pool: nichts wird gesendet
slot-nothing-playable = nichts spielbar (Regeln, Plugins)

## Tasten (Aktionen)
key-space = Leertaste
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
help-pause-resume = Pause / Fortsetzen / Wecken
help-skip = zum nächsten springen
help-override = Override einreihen
help-drain = Ruhe bei 0 Hörern
help-wake = aufwecken
help-section = nächster Abschnitt
help-select = Zeile wählen
help-override-remove = gewählten Override entfernen
help-override-clear = Warteschlange leeren
help-live-kick = DJ trennen
help-live-open = Slot öffnen
help-live-close = gewählte Öffnung schließen
help-enqueue = zu einer Queue-Playlist hinzufügen
help-scan = Bibliothek scannen
help-shutdown = Bedienerstopp
help-shutdown-force = erzwungener Stopp (trennt den DJ)

## Aktionen: Statuszeile
status-action-running = Aktion läuft…
status-action-failed = fehlgeschlagen: { $reason }
status-cancelled = abgebrochen
done-state = Sendung: { $from } → { $to }
done-state-unchanged = Sendung bereits { $state }
done-skip = Sprung angefordert
done-override = Override #{ $id } eingereiht ({ $pending } wartend)
done-override-degraded = Override #{ $id } eingereiht, SOFT gespielt (kein Schnitt möglich) — { $pending } wartend
done-overrides-cleared = { $n ->
    [one] 1 Override entfernt
   *[other] { $n } Overrides entfernt
}
done-live-kicked = DJ { $dj } getrennt
done-live-opened = Slot für { $dj } geöffnet bis { $until }
done-live-closed = Öffnung für { $dj } geschlossen
done-enqueued = zu „{ $playlist }“ hinzugefügt ({ $len } in der Queue)
done-enqueue-full = Queue „{ $playlist }“ ist voll ({ $len }): nichts hinzugefügt
done-scan = Scan: { $found } gefunden, { $skipped } übersprungen, { $vanished } bei diesem Scan verschwunden ({ $unavailable } insgesamt)
done-plugin = Plugin { $name }: { $state }
done-plugin-reason = Plugin { $name }: { $state } ({ $reason })
done-shutdown = Bedienerstopp: Liquidsoap auf Hintergrundrauschen geparkt, stationd beendet sich
done-shutdown-fallback = Bedienerstopp: stationd beendet sich, das Sicherheitsnetz von Liquidsoap übernimmt

## Dialoge
dialog-cancel = Abbrechen
dialog-confirm-keys = ←/→ wählen · Enter bestätigen · Esc abbrechen
dialog-form-keys = Tab/↑↓ Feld · ←/→ Auswahl · Enter bestätigen · Esc abbrechen
form-required = „{ $field }“ ist erforderlich
form-positive-integer = „{ $field }“: eine ganze Zahl ≥ 1
confirm-pause-title = Pausieren
confirm-pause-body = Die Sendung hält bei „{ $track }“ an, bis sie fortgesetzt wird.
confirm-pause-yes = Pausieren
confirm-skip-title = Zum nächsten springen
confirm-skip-body = Schneidet „{ $track }“ jetzt ab.
confirm-skip-next = Voraussichtlich als nächstes: „{ $track }“.
confirm-skip-yes = Springen
confirm-drain-title = Ruhe bei 0 Hörern
confirm-drain-body = Der Sender geht in Ruhe, sobald kein Hörer mehr da ist (Hintergrundrauschen, kein Raster).
confirm-drain-yes = Ruhe scharf schalten
form-override-title = Override einreihen
form-override-kind = Inhalt
form-override-kind-media = Medium
form-override-kind-playlist = Playlist
form-override-target = Pfad unter media/ oder Playlist-Referenz
form-override-mode = Modus
form-override-soft = SOFT: beim nächsten Titel
form-override-hard = HARD: schneidet sofort
form-override-expiry = Verfall (30s, 5m, 2h; leer = nie)
form-override-tracks = Gehaltene Titel (Playlist)
override-media = Medium: { $path }
override-playlist = Playlist: { $playlist }
confirm-override-title = Diesen Override einreihen?
confirm-override-soft = Läuft an der nächsten Titelgrenze.
confirm-override-hard = Schneidet den laufenden Titel SOFORT ab.
confirm-override-tracks = { $n ->
    [one] Hält 1 Titel.
   *[other] Hält { $n } Titel.
}
confirm-override-no-expiry = Ohne Verfall.
confirm-override-expiry = Verfällt, wenn nicht innerhalb von { $expiry } gesendet.
confirm-override-yes = Einreihen
confirm-clear-one-title = Override entfernen
confirm-clear-one-body = Override #{ $id } ({ $what }) aus der Warteschlange entfernen?
confirm-clear-all-title = Override-Warteschlange leeren
confirm-clear-all-body = { $n ->
    [one] Den wartenden Override entfernen?
   *[other] Die { $n } wartenden Overrides entfernen?
}
confirm-clear-yes = Entfernen
confirm-kick-title = DJ trennen
confirm-kick-body = { $dj } jetzt trennen? Bis zum Ende des Slots wird er abgewiesen.
confirm-kick-yes = Trennen
form-live-open-title = Live-Slot öffnen
form-live-dj = DJ
form-live-duration = Dauer (30m, 2h, 1d)
confirm-live-close-title = Öffnung schließen
confirm-live-close-body = Die Öffnung für { $dj } jetzt schließen?
confirm-live-close-yes = Schließen
form-enqueue-title = Zu einer Queue-Playlist hinzufügen
form-enqueue-playlist = Playlist (Queue-Modus)
form-enqueue-media = Pfad unter media/
confirm-scan-title = Bibliothek scannen
confirm-scan-body = Liest ganz media/ neu ein: kann auf einer großen Platte oder einem NFS-Mount dauern. Die Sendung bleibt unberührt.
confirm-scan-yes = Scannen
plugin-verb-start = starten
plugin-verb-stop = stoppen
plugin-verb-restart = neu starten
plugin-verb-reload = neu laden
confirm-plugin-start = Plugin { $name } starten
confirm-plugin-stop = Plugin { $name } stoppen
confirm-plugin-restart = Plugin { $name } neu starten
confirm-plugin-reload = Plugin { $name } neu laden
confirm-plugin-body = Der neue Zustand erscheint in der Plugin-Liste.
confirm-shutdown-title = Bedienerstopp
confirm-shutdown-body = stationd beendet sich und Liquidsoap wird auf Hintergrundrauschen geparkt. Neustart: stationctl station start.
confirm-shutdown-kicks = DJ { $dj } wird getrennt.
confirm-shutdown-refused-live = DJ { $dj } ist auf Sendung: der Stopp wird abgewiesen (A erzwingt).
confirm-shutdown-continue = Weiter
confirm-shutdown-again-title = Stopp bestätigen
confirm-shutdown-again-body = Letzte Bestätigung: der Sender hört auf, das Raster zu senden.
confirm-shutdown-yes = Sender stoppen

## Bildschirm Steuerung
control-broadcast = Sendung
control-overrides = Overrides
control-live = Live
control-queue = Queue
control-library = Bibliothek
control-plugins = Plugins
control-station = Sender
control-state = Zustand
control-listeners = Hörer
control-on-air = Auf Sendung
control-broadcast-hint = Leertaste pausiert oder setzt fort, n springt weiter, v schaltet die Ruhe scharf (bei 0 Hörern), w weckt auf.
control-read-failed = nicht lesbar: { $reason }
control-not-read = noch nicht gelesen
control-more = … und { $n } weitere
control-overrides-none = kein wartender Override
control-overrides-hint = o reiht ein Medium oder eine Playlist vor dem Raster ein.
control-col-content = Inhalt
control-col-mode = Modus
control-col-left = Rest
control-col-expires = Verfällt
control-col-source = Von
control-col-plugin = Plugin
control-col-state = Zustand
control-col-failures = Fehler
control-col-reason = Grund
control-live-disabled = Live nicht konfiguriert (kein Abschnitt [live])
control-live-on-air = Auf Sendung
control-live-nobody = niemand
control-live-session = Zugang { $access }, seit { $since }, { $address }
control-live-accounts = DJ-Konten
control-live-djs = { $n } angelegt
control-live-djs-error = DJ-Datei nicht lesbar: { $reason }
control-live-urgent = Notfallrecht
control-live-cooldown = Sperrzeit
control-live-until = { $dj } bis { $time }
control-live-refused = Abgewiesen
control-live-last-refusal = Letzte Abweisung
control-live-refusal = { $dj } um { $time }: { $reason }
control-live-openings = Öffnungen:
control-live-no-opening = keine
control-live-cut = getrennt
control-queue-hint = e fügt ein Medium in den Puffer einer Playlist im Queue-Modus ein (Hörerwünsche, Widmungen). Die Playlist muss existieren und im Queue-Modus sein; sonst lehnt stationd ab und sagt es.
control-scan-none = kein Scan aus dieser TUI gestartet
control-scan-hint = s liest media/ neu ein und aktualisiert den Index (Dateien hinzugefügt, entfernt, geändert).
control-scan-last = Letzter Scan:
control-scan-found = Gefunden
control-scan-present = Verfügbar
control-scan-vanished = Bei diesem Scan verschwunden
control-scan-unavailable = Insgesamt verschwunden
control-scan-skipped = Übersprungen
scan-skip-unreadable = unlesbar
scan-skip-zero-duration = Länge null
scan-skip-walk-error = Durchlauf
scan-skip-unknown = ?
control-plugins-none = kein Plugin angelegt
control-plugin-off = deaktiviert
control-shutdown-hint = a: Bedienerstopp. stationd beendet sich, Liquidsoap wird auf Hintergrundrauschen geparkt; nichts startet vor „stationctl station start“ neu. A: dasselbe, der DJ auf Sendung wird getrennt. Zwei Bestätigungen.
control-shutdown-live = DJ { $dj } auf Sendung: a wird abgewiesen, A trennt ihn.

## Bildschirm Medien
media-search-title = Suche
media-search-hint = / zum Suchen: Wörter (Titel, Interpret, Album, Pfad), genre:x, dossier:x
media-count = { $shown } / { $total }
media-sorted-by = sortiert nach { $field } { $dir }
media-missing = ohne { $field }
media-with-unavailable = verschwundene Dateien inbegriffen
media-loading = lädt…
media-none = kein passendes Medium
media-field-path = Pfad
media-field-title = Titel
media-field-artist = Interpret
media-field-album = Album
media-field-year = Jahr
media-field-duration = Dauer
media-field-genre = Genre
key-slash = /
key-enter = Enter
key-esc = Esc
key-m = m
help-media-search = suchen
help-media-apply = Suche übernehmen
help-media-cancel = zurück zur vorherigen Suche
help-media-sort = Sortierung wechseln
help-media-desc = Reihenfolge umkehren
help-media-missing = Filter „ohne Titel / Interpret / Genre / Jahr“
help-media-unavailable = verschwundene Dateien einbeziehen
help-media-reload = neu laden

## --- Lot 4b: Playlists, Editor, Medienkarte --------------------------------

key-p = p
key-f = f
key-shift-r = R
key-ctrl-s = Strg+S
key-ctrl-t = Strg+T
key-ctrl-n = Strg+N
key-ctrl-d = Strg+D
key-alt-updown = Alt+↑ / ↓
key-left-right = ← / → / Leertaste
key-f8 = F8

help-close = schließen
help-pl-edit = bearbeiten
help-pl-new = neue Playlist
help-pl-delete = löschen
help-pl-filter = filtern
help-pl-reload-root = alle Dateien neu einlesen
help-ed-save = speichern
help-ed-next = nächstes Feld (Umschalt+Tab: voriges)
help-ed-choice = Auswahl ändern
help-ed-add = hinzufügen (Filter, Mitglied, Medien)
help-ed-remove = Eintrag entfernen
help-ed-move = Eintrag verschieben
help-ed-raw = rohes TOML bearbeiten
help-ed-form = zurück zum Formular
help-ed-next-diag = zum nächsten Problem
help-media-card = Medienkarte
help-media-mark = markieren / Markierung aufheben
help-media-clear-marks = alle Markierungen aufheben
help-media-to-playlist = zu einer statischen Playlist hinzufügen
help-media-enqueue = in die Queue stellen
help-card-prev-next = voriges / nächstes Medium
help-picker-add = markierte Medien (oder dieses) hinzufügen

dialog-info-keys = Enter / Esc: schließen
picker-keys = ↑↓ wählen · Enter bestätigen · Esc abbrechen
picker-loading = Liste wird geladen…
picker-new = Neue Playlist…
picker-none = keine passende Playlist

mode-static = statisch
mode-dynamic = dynamisch
mode-remote = Relay
mode-queue = Queue
mode-group = Gruppe

val-shuffle = zufällig
val-sequential = der Reihe nach
val-newest = neueste zuerst
val-oldest = älteste zuerst
val-fifo = zuerst eingereiht zuerst
val-lifo = zuletzt eingereiht zuerst
val-all = alle Filter
val-any = mindestens ein Filter
val-filename = Dateiname
val-mtime = Dateidatum
val-published = Veröffentlichungsdatum
val-weighted = gewichtet
val-rotate = abwechselnd
val-sequence = nacheinander
val-abort = die ganze Gruppe weicht
val-skip = zum nächsten springen
val-fallthrough = Platz machen
val-stop = anhalten
val-disable = sich deaktivieren
val-hold = auf Sendung bleiben
val-yes = ja
val-no = nein
val-duration-s = Dauer (s)
val-prefix = beginnt mit
val-eq = gleich
val-ne = ungleich
val-contains = enthält
val-has = hat das Genre
val-has-any = eines von
val-has-all = alle von
val-has-none = keines von

pl-summary = { $n ->
    [one] { $n } Playlist
   *[other] { $n } Playlists
} · Sortierung: { $sort } · / filtern
pl-none = keine Playlist
pl-col-ref = Datei (Ref)
pl-col-name = Name
pl-col-mode = Modus
pl-col-used = verwendet von
pl-pool = Pool
pl-used-rules-n = { $n ->
    [one] { $n } Regel
   *[other] { $n } Regeln
}
pl-used-groups-n = { $n ->
    [one] { $n } Gruppe
   *[other] { $n } Gruppen
}
pl-used-none = nichts (weder Rasterregel noch Gruppe)
pl-used-rules = Rasterregeln: { $list }
pl-used-groups = Gruppen: { $list }
pl-disabled = deaktiviert
pl-detail = Details
pl-file = Datei
pl-no-file = ohne Datei
pl-no-file-long = keine (Eintrag über stationctl add)
pl-file-differs = die Datei weicht vom Angewendeten ab (von Hand geändert oder ungültig)
pl-edit-no-file = Diese Playlist wurde ohne Datei hinzugefügt (stationctl add): bearbeite sie dort, wo ihr TOML liegt, dann erneut stationctl add.
pl-open-failed = Öffnen nicht möglich
pl-opening = wird geöffnet…
pl-new-title = Neue Playlist
pl-new-mode = Modus der Playlist (später änderbar):
pl-delete-title = Playlist löschen
pl-delete-refused = „{ $playlist }“ kann nicht gelöscht werden, solange sie referenziert wird:
pl-delete-body = „{ $playlist }“ ({ $name }) löschen?
pl-delete-file = Ihre Datei { $file } wird auf dem Knoten gelöscht.
pl-delete-no-file = Sie hat keine Datei: nur ihr Eintrag wird entfernt.
pl-delete-yes = Löschen
pl-reload-title = Playlists neu einlesen
pl-reload-body = stationd liest alle Playlist-Dateien des Knotens neu ein und entfernt die, deren Datei verschwunden ist (außer eine Regel oder Gruppe verweist noch darauf).
pl-reload-yes = Neu einlesen
pl-busy-title = Entwurf offen
pl-busy-body = Ein anderer Playlist-Entwurf ist offen: speichere oder schließe ihn und versuche es dann erneut aus Medien.

pl-h-identity = Identität
pl-h-selection = Auswahl
pl-h-broadcast = Sendung
pl-h-files = { $n ->
    [one] Medium ({ $n })
   *[other] Medien ({ $n })
}
pl-h-filters = { $n ->
    [one] Filter ({ $n })
   *[other] Filter ({ $n })
}
pl-h-members = { $n ->
    [one] Mitglied ({ $n })
   *[other] Mitglieder ({ $n })
}
pl-f-ref = Datei (Ref)
pl-f-name = Name
pl-f-enabled = aktiviert
pl-f-mode = Modus
pl-f-order = Reihenfolge
pl-f-match = Verknüpfung
pl-f-order-by = verwendetes Datum
pl-f-unplayed = nur einmal senden
pl-f-url = Stream-Adresse
pl-f-max-len = maximale Länge
pl-f-strategy = Strategie
pl-f-on-member-unavailable = Mitglied ohne Medien
pl-f-filter = Filter { $n }
pl-f-op = Operator
pl-f-value = Wert
pl-f-member = Mitglied { $n }
pl-f-weight = Gewicht
pl-f-take = Titel
pl-f-runtime = Dauer
pl-f-limit = Titel pro Durchgang
pl-f-repeat = von vorn beginnen
pl-f-on-exhausted = wenn erschöpft
pl-f-no-same-artist = gleicher Interpret, nicht vor
pl-f-no-same-track = gleiche Datei, nicht vor
pl-f-no-same-title = gleicher Titel, nicht vor
pl-absent = — (nicht gesetzt)
pl-add-files = Medien hinzufügen
pl-add-filter = Filter hinzufügen
pl-add-member = Mitglied hinzufügen

ed-title = Playlist { $reference }
ed-title-new = Neue Playlist { $reference }
ed-modified = geändert
ed-revision = Rev. { $rev }
ed-raw-mode = rohes TOML
ed-form = Formular
ed-toml = TOML
ed-toml-keys = Strg+T zum Bearbeiten
ed-toml-keys-raw = Esc oder Strg+T: Formular · Strg+S: speichern
ed-unreadable = Das TOML ist nicht mehr lesbar: das Formular wartet, bis es korrigiert ist.
ed-unreadable-hint = Strg+T zurück zum Texteditor; stationd nennt die betroffene Zeile.
ed-file-differs = die Datei { $file } weicht vom Angewendeten ab: geöffnet ist die Datei
ed-no-file = keine Datei auf dem Knoten: Speichern legt sie an
ed-files-added = { $n } von { $total } Medien hinzugefügt
ed-ref-required = gib die Datei (Ref) der neuen Playlist an, z. B. sendung/intro
ed-saving = wird gespeichert…
ed-save-failed = Speichern nicht möglich: { $reason }
ed-saved = gespeichert ({ $file })
ed-created = angelegt ({ $file })
ed-reloaded = Entwurf durch die Datei des Knotens ersetzt
ed-not-saved = { $n ->
    [one] nicht gespeichert: { $n } Fehler
   *[other] nicht gespeichert: { $n } Fehler
}
ed-conflict-short = Konflikt: die Datei hat sich geändert
ed-conflict-title = Die Datei hat sich inzwischen geändert
ed-conflict-body = { $file } wurde seit dem Öffnen geändert (von jemand anderem oder von Hand). Nichts wurde geschrieben.
ed-conflict-never = Speichern überschreibt nie eine andere Änderung: vergleichen, dann neu laden und die eigenen Änderungen wiederholen.
ed-conflict-keep = Entwurf behalten
ed-conflict-compare = Vergleichen
ed-conflict-reload = Neu laden (Entwurf verloren)
ed-compare-title = Datei des Knotens / Entwurf
ed-compare-keys = ↑↓ Bild↑ Bild↓ blättern · Esc schließen · orange Zeilen: verschieden
ed-compare-disk = Auf dem Knoten (Rev. { $rev })
ed-compare-draft = Dein Entwurf
ed-discard-title = Geänderter Entwurf
ed-discard-body = Die Änderungen an diesem Entwurf sind nicht gespeichert.
ed-discard-keep = Weiter bearbeiten
ed-discard-yes = Änderungen verwerfen
ed-pick-media = Medien hinzufügen
ed-pick-member = Mitglied hinzufügen
ed-genres = Genres: { $list }
ed-genres-none = kein bekanntes Genre beginnt so
ed-diags = { $errors } Fehler, { $warnings } Warnung(en)
ed-diags-none = Diagnosen
ed-diags-ok = stationd sieht kein Problem
ed-diag-label = { $label }:
ed-diag-file = Datei
ed-pool = Pool heute
ed-pool-pending = stationd rechnet…
ed-pool-invalid = ungültiger Entwurf: keine Vorschau, solange Fehler bestehen
ed-pool-unmeasured = Pool für diesen Modus nicht messbar (weitergeleiteter Stream, Queue)
ed-pool-unmeasured-short = nicht messbar
ed-pool-empty = leerer Pool: nichts wird gesendet
ed-pool-member-empty = keine Medien
ed-pool-count = { $n ->
    [one] { $n } Medium
   *[other] { $n } Medien
}
ed-pool-artists = { $n ->
    [one] { $n } Interpret
   *[other] { $n } Interpreten
}

diag-syntax = unlesbares TOML ({ $detail })
diag-unknown-field = der Grammatik unbekanntes Feld
diag-missing-field = Pflichtfeld fehlt
diag-bad-value = ungültiger Wert
diag-not-allowed = Feld in diesem Modus oder dieser Strategie nicht erlaubt
diag-required-for-mode = von diesem Modus oder dieser Strategie verlangt
diag-conflict = mit einem anderen Feld unvereinbar
diag-bad-filter = ungültiger Filter (Feld, Operator oder Wert)
diag-bad-duration = ungültige Dauer (30s, 15m, 2h, 1d)
diag-unknown-ref = bezeichnet keine Playlist
diag-bad-ref = ungültige Referenz
diag-cycle = die Gruppe enthält sich selbst
diag-id-changed = die Kennung kann sich nicht ändern
diag-empty-pool = heute passt kein Medium
diag-unknown = unbekanntes Problem (Code { $code })
diag-rejected = : „{ $value }“
diag-expected = {" "}(erwartet: { $values })

media-marked = { $n } markiert
media-choose-static = { $n } Medien zu einer statischen Playlist hinzufügen
media-choose-queue = { $n } Medien in die Queue stellen

card-title = Medienkarte
card-keys = Esc schließen · ↑↓ voriges / nächstes · o Override · p Playlist · f Queue
card-no-title = kein Titel-Tag
card-size = Größe
card-size-mb = { $mb } MB
card-state = Zustand
card-available = verfügbar
card-unavailable = von der Platte verschwunden
card-playlists = Playlists, die es senden können
card-no-playlist = keine: dieses Medium läuft nur per Override oder Queue
card-plays = Sendungen (gesendet / gewählt)
card-plays-legend = gesendet: wirklich auf Sendung gegangen · gewählt: von stationd ausgewählt
card-24h = 24 Std.
card-7d = 7 T.
card-30d = 30 T.
card-all = gesamt
card-last = zuletzt gewählt:
card-never = nie

done-enqueued-many = { $n } Medien zu „{ $playlist }“ hinzugefügt ({ $len } in der Queue)
done-enqueue-many-full = Queue „{ $playlist }“ voll nach { $n } von { $total } ({ $len } in der Queue)
done-enqueue-partial = { $n } von { $total } eingereiht, dann: { $reason }
done-playlist-removed = Playlist „{ $playlist }“ gelöscht
done-playlist-removed-file = Playlist „{ $playlist }“ gelöscht (Datei { $file } entfernt)
done-playlists-reloaded = Playlists neu eingelesen: { $added } angewendet, { $removed } entfernt, { $errors } Datei(en) mit Fehler
