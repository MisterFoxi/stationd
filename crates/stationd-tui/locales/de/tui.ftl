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
key-help = ?
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
planned-control-1 = Sendung: Pause, Fortsetzen, Weiter, Ruhe, Aufwecken
planned-control-2 = Overrides: einreihen, auflisten, leeren
planned-control-3 = Live: DJ trennen, Zeitfenster öffnen / schließen
planned-control-4 = Warteschlangen, Bibliotheksscan, Plugins, Betreiber-Stopp
planned-playlists-1 = Liste: Modus, Pool, verweisende Regeln und Gruppen
planned-playlists-2 = Formular je Modus, Live-Vorschau des Pools
planned-playlists-3 = Speichern durch stationd (Revision, Konflikte)
planned-agenda-1 = Tag: Zeitleiste, Basen, Termine, geschätzte every
planned-agenda-2 = Woche: 7 Spalten, Raster 15/30/60 Min.
planned-agenda-3 = Abdeckung des Sendeplans, Regeln bearbeiten
planned-media-1 = Suche, Filter (Typ zuerst), Sortierung, Seiten
planned-media-2 = Medienblatt, Sendestatistiken
planned-media-3 = Scan mit Fortschritt, Typ für mehrere (Taste t)
planned-tags-1 = Typen: deklarierte Werte, Anzahlen, Medien ohne Typ
planned-tags-2 = Freie Tags: anlegen, umbenennen, zusammenführen
planned-system-1 = Zustand: stationd, Liquidsoap, Icecast (Mounts), Live
planned-system-2 = Ereignisse live, Sendestatistiken
planned-plugins-1 = Von geladenen Plugins deklarierte Ansichten
planned-plugins-2 = Datenbank jedes Plugins (Info, Nur-Lese-Abfrage)
