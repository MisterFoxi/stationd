# MP3 metadata writeback

A full `stationctl library scan` now writes plugin-derived `creation` and
`tempo` values into the MP3 itself, as `TXXX:creation` and `TXXX:tempo`.
No extra config switch is needed. The existing enabled custom-tags rules
control which values are derived. Genre enrichment remains database-only.

The WASM plugin still computes values without filesystem access. The host
writes the MP3 after enrichment and before committing the scan to SQLite.
Existing Comment/Description/Type/BPM and other ID3 frames are preserved.
Existing ID3v2.3/2.4 versions are kept. No standard date field is repurposed.
The filesystem modification time is restored, not set to the creation date.

Only derived values are written: no BPM means no new tempo; no matching date
means no new creation. An absent derived value does not remove an existing
TXXX frame. Matching values are a no-op. Existing case variants of the target
TXXX names are replaced by one lowercase target name.

Each changed file is copied into a unique sibling temporary MP3, edited, and
read back before an atomic rename replaces the original. The directory must
be writable, with space for one additional MP3. Symbolic-link files and paths
resolving outside the configured media root are refused. Size/mtime changes
since the scan abort that write. Failures include the path and are surfaced
as scan errors. Earlier successful file replacements are not rolled back if
a later file fails; the filesystem and SQLite are not one transaction.

Only MP3 is changed by this extension. Library tag edits and non-MP3 formats
retain their previous behavior; use a full library scan for this writeback.
The indexed size/mtime and matching played-episode guards follow each write,
so metadata growth alone does not make a played episode eligible again.

Apply this incremental patch AFTER the original metadata patch and its dev
migration fix (media_meta must use 0023, leaving dev's existing 0022 alone).
It is based on dev commit 026207e8470f337e56992d7523859f7d9ec41234.

```sh
git apply --check stationd-mp3-writeback.patch
git apply stationd-mp3-writeback.patch
make test
make restart
make ctl A="library scan"
```

This extension changes the HOST: rebuilding only custom-tags-wasm does not
activate it. The previous metadata-capable plugin remains compatible.
No migration or new dependency is introduced by this incremental patch.

For a file with the demonstrated comment and BPM 125 (the example ranges),
eyeD3 should show these additional user text frames after the scan:

```text
creation: 2026-06-14T06:36:48Z
tempo: fast
```
