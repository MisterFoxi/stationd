# Offline MP3 BPM analysis

> **Retiré.** Le module `bpm_analysis` (heuristique BPM maison) a été **supprimé**,
> remplacé par l'analyse Essentia (tempo + tonalité + loudness + labels) :
> voir `Doc/offline-analysis.md`. Ce document est conservé pour l'historique.

During a full library scan, the loaded `custom-tags` plugin can request host
analysis for MP3s without a finite positive BPM tag. The host decodes local
audio with FFmpeg; it never sends the track to Liquidsoap or to the antenna.
The WASM guest still has no filesystem or decoder access.

```toml
[plugin.config.tempo]
enabled = true
analyze_missing = true
source_tags = ["BPM"]
[[plugin.config.tempo.range]]
max = 79.999
value = "slow"
[[plugin.config.tempo.range]]
min = 80
max = 119.999
value = "medium"
[[plugin.config.tempo.range]]
min = 120
value = "fast"
```

`analyze_missing` defaults to true when tempo is enabled. Set it to false
to classify existing BPM tags only. The plugin must be loaded, named
`custom-tags`, and its source_tags must include BPM (the default).
Restart stationd after changing configuration.

An accepted estimate is supplied as BPM to the existing configurable ranges.
The host writes the integer BPM into standard ID3 TBPM and the resulting
classification into TXXX:tempo. Creation remains TXXX:creation. The existing
Type-to-genres behavior is unchanged. Tempo and creation remain in media_meta
for playlist filters; genre is not used to store them.

Valid existing BPM tags take priority over analysis. A subsequent scan reads
the saved TBPM without decoding the file again. Writes use the existing staged
copy, verification and atomic replacement, preserving MPEG audio bytes, source
tags, mtime and play-once guards. MP3s without ID3v2 use the standard editor's
ID3v1-to-ID3v2 preservation policy.

## Limits and failures

Analysis is sequential and examines at most the first 120 seconds per file,
decoded to mono at 11025 Hz. Each FFmpeg process has a 45-second timeout and
bounded output. A file needs at least 30 seconds of decoded audio. Spectral
flux periodicity is compared across three sections, in the 45-240 BPM range.
This is a heuristic, not a guarantee: half/double-tempo ambiguity is possible,
and live, irregular or quiet introductions may not yield an estimate.
The first scan of a large untagged library can take time.

Missing FFmpeg, corrupt audio, silence or inconsistent sections produce a
warning naming the file and reason; no estimated BPM is written. Existing
tags are retained. Such files can be retried at a later scan. A successful BPM
estimate with no matching configured range writes TBPM but no new tempo value.

FFmpeg is included in both Dockerfiles. For a non-Docker installation, put
ffmpeg on PATH or set STATIOND_FFMPEG to its executable path. The subprocess
uses explicit arguments, without a shell. Rebuilding only the WASM plugin is
insufficient: rebuild/restart the host too.

## Verification

Run the normal Rust tests and the plugin's tests. Two integration tests require
FFmpeg and are explicitly ignored in the ordinary test run:

```sh
cargo test --locked --lib bpm_analysis
cargo test --locked --lib scan_writeback
cargo test --locked --manifest-path plugins/custom-tags-wasm/Cargo.toml
cargo test --locked --lib real_mp3_analysis_writeback_and_rescan -- --ignored
STATIOND_CUSTOM_TAGS_WASM="$PWD/plugins/custom-tags-wasm/target/wasm32-unknown-unknown/release/custom_tags_wasm.wasm" \
  cargo test --locked --lib offline_mp3_scan_through_real_wasm -- --ignored
```

The last test runs the real WASM plugin through the library actor, checks
TBPM/tempo/creation, the SQLite tempo value and Type genre, then verifies that
rescanning does not rewrite the MP3. Its audio is generated, not copyrighted.
