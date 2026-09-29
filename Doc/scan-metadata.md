# Custom scan metadata

The v1 genre-only hook is extended compatibly: `ScanEnrichment` accepts an
optional `metadata` object of string keys and scalar string values. Old replies
without it still work. `ScanExtras` merges both genres and metadata by path.
Keys must contain only lowercase ASCII letters, digits and underscores; blank
keys or values, invalid creation timestamps and unknown paths reject the whole
plugin reply. First plugin in configured order wins each metadata key.

The scanner also exposes standard Comment, Description and BPM fields through
`custom_tags`. Names are canonical for standard fields; existing custom names
and multi-values remain available. After enrichment, the host writes creation/tempo into MP3 TXXX frames; see
`Doc/mp3-writeback.md`. Other audio formats remain read only.

Migration 0023 creates `media_meta`. A full scan atomically replaces metadata
alongside media and genres. Removed tags, missing files and disabled plugins
cannot leave stale metadata. The source tags are not persisted. Arbitrary
custom keys are stored; the filter whitelist currently exposes these scalars:

- `tempo`: `eq`, `ne`, `has` (alias for `eq`), `=`, `==`, `!=`;
  case-sensitive label comparison. Missing values match none of these.
- `creation`: `eq`, `ne`, `=`, `==`, `!=`, `<`, `<=`, `>`, `>=`;
  quoted RFC3339 timestamps with an explicit offset. Storage and filter values
  normalize to UTC with nine fractional digits, so offsets compare as instants.
  Timestamp precision and leap-second handling follow StationD's jiff library.
- `age`: `<`, `<=`, `>`, `>=`; the age of `creation`, a duration (`30m`,
  `12h`, `10d`) resolved against the station clock at each pick — a sliding
  window: `age < 10d` = created less than ten days ago. Media without a
  creation date have no age and match none of these. Also in the media
  search (`library search --age "<10d"`, TUI `âge:<10d`).

```toml
[[selection.filter]]
field = "age"
op = "<"
value = "10d"
```

The custom-tags plugin keeps `tags = ["Type"]` as genre enrichment. Both new
rules are opt-in. Metadata-only configurations can omit `tags`. See the commented
configuration and playlist examples in `stationd.example.toml`.

`creation.match` is a literal case-sensitive phrase. Source tag names are matched
case-insensitively in configured order. Leading whitespace after the phrase is
ignored. A date token ends at whitespace, semicolon, comma or end of value. The
first valid RFC3339 token immediately after a phrase wins; invalid matches do not
prevent a later valid occurrence. Unrelated dates are ignored. `date_format`
currently accepts only `rfc3339`.

Tempo accepts finite positive decimal BPM. Bounds are inclusive and may be
omitted; overlapping or reversed ranges are configuration errors. Gaps and
invalid BPM produce no tempo. The first matching value in configured source
order wins. No default genre, phrase or tempo label is hardcoded.

Build and validation on a supported Unix host:

```sh
cargo test --lib
cargo test --manifest-path plugins/custom-tags-wasm/Cargo.toml
cargo build --release --target wasm32-unknown-unknown --manifest-path plugins/custom-tags-wasm/Cargo.toml
```

Rebuild StationD (to embed migration 0023), rebuild the plugin, restart and scan
the library after applying this patch. Re-scan after editing plugin rules.
