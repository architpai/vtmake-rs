# Selective output and raw tile hashes

The three build options are opt-in. The normal build path does not hash tiles.

* `--only-tiles FILE`: one XYZ key (`z/x/y`) per line. Empty lines are
  allowed; duplicate keys are removed. Coordinates must be in range, zoom 0–24.
  Missing or empty targets produce no tile. Targets outside the build zoom
  range produce no tile.
* `--hashes-out FILE`: a complete manifest of nonempty produced tiles,
  including unchanged tiles that were not written. With `--only-tiles`, this
  is the produced target set, not the full region set.
* `--known-hashes FILE`: compare each final raw MVT hash with this manifest.
  Equal tiles are not compressed or written. Requires `--hashes-out`.

Both manifests use this exact first line:

```text
vtmake-hashes-v1 blake3-256 raw-mvt
```

Each following line is `z/x/y SPACE 64-hex-digit-hash`. Output uses lowercase
hex, LF line ends and numeric (z,x,y) order. Input accepts whitespace between
fields and either hex case. Duplicate keys, unknown versions and malformed
lines fail before the tile sink opens. The hash is BLAKE3-256 of the complete
uncompressed protobuf bytes, with no added prefix. It does not depend on gzip
mode, output format or thread count. A successful build publishes the manifest
with a temporary file and atomic rename, after the tile sink finishes.
A failed build does not replace a previous manifest. The caller must check the
exit code before use. Hash equality assumes collision resistance.

Use a fresh output, or `--force`. Tile selection rejects `--allow-existing`
because it can retain tiles outside the target set. With hashes alone and
`--allow-existing`, an unchanged tile
already in the sink remains there. An MBTiles file that has no changed tiles
still has its schema and metadata. Directory output still has metadata.json.
A manifest is not an upload log: do not delete skipped keys during stale
reconciliation. Update the stored known manifest only after uploads succeed.
Keep manifests separate by layer, source and build settings.

## Identity analysis and supported options

The input read, global feature order, default zoom and drop-rate selection,
shared node collection, feature extents, density setup and metadata input
statistics remain complete. Selection occurs after this setup.

`tile::process_with` clips a parent's geometry before it copies it to each
child. A child uses the stored, rounded coordinates of that parent. Starting
directly at a target would change clipping and rounding. Selection therefore
keeps all ancestors, including the existing clip cascade below the minimum
output zoom. Only branches with no target are removed. Child clipping needs
the features of its own parent; it does not read sibling output. All features
in a kept branch remain in input order. Ancestors need clipping, but do not
need simplification, MVT encoding, gzip or output. The target tile encoder
receives exactly the same candidates as in the full run.

Tile limits are different: `process_zoom` merges drop thresholds across all
tiles, retries the zoom, and can extend the maximum zoom. The compressed size
can itself determine these decisions. Full-detail variable-depth trials can
stop a child pyramid. Shell filters can have external state. Pruning these
cases without a complete evaluation would be unsafe. These options therefore
require `--no-feature-limit --no-tile-size-limit`, and reject variable-depth
pyramids and shell filters. This is an explicit restriction, not a promise of
identity for those combinations. The plain seed command line satisfies it.
With both limits disabled, `--drop-densest-as-needed` cannot tighten a retry;
`--extend-zooms-if-still-dropping` cannot extend (there is no pending zoom
queue). Feature min/max zooms and rate drops still use the full input setup.

Hashing occurs after final MVT encoding. With limits disabled there are no size
retries. A match bypasses only gzip and the tile sink, so tile counts, center
selection and metadata are the same as in an equivalent run without known
hashes. With tile selection, metadata describes the selected traversal:
input bounds and source statistics remain complete, but center, strategies,
generated attribute statistics and maximum visited zoom can differ from the
full render. Metadata is not part of the tile-byte identity contract.

## Future in-process entry point

A library extraction is not a small safe change. The current pipeline initializes
process-global Rayon, attribute/filter/plugin state, reads environment variables,
and exits on errors in several crates. An in-process API must replace these with
per-build state and Result propagation before concurrent builds are safe.

Proposed API: BuildOptions, InputSource (NDJSON reader now, typed COPY reader
later), and a TileSink trait callback. The callback should receive raw bytes,
XYZ and the versioned hash, plus a separate completion manifest and metadata.
Let the caller decide compression and upload. Cancellation and a per-build
thread pool/memory budget must be part of that API. Do not call the present
CLI build function concurrently in one process.
