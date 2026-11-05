# Compare vector tiles

Use the image test first to check the main goal: equal output in MapLibre.
Use the Rust test to locate feature and geometry changes. A sample of images
cannot prove that all tiles render the same.

## Decode and compare

```sh
cargo build --release -p vtmake-compare
RAYON_NUM_THREADS=30 taskset -c 2-31 target/release/vtmake-compare A.mbtiles B.mbtiles \
  --json /tmp/compare.json --max-dist 1 --list-limit 20
```

Each input can be an MBTiles file or a directory with `z/x/y.pbf` files.
Directory coordinates use XYZ. MBTiles coordinates use TMS. Both gzip and raw
PBF tiles are supported, including a mixture in one input. MBTiles views, such
as tippecanoe's `tiles` view, are supported. Inputs are read only.

The default match key is the full attribute map plus geometry type. Each group
is a multiset: duplicate features are counted. Feature IDs and feature order
are not part of this key. Use `--match-key osm_id` to match by one attribute
instead. That attribute must exist on every feature. This option can expose
attribute and geometry type changes that otherwise appear as missing features.
Within a duplicate group, exact geometry matches come first. Remaining features
use a greedy match by the first vertex. This is not a global optimal assignment.
Use a unique match key when one is available.

Coordinates are scaled to a 4096-unit tile extent before comparison. Points use
symmetric nearest-point maximum and mean distances. Lines and polygons use
symmetric vertex-to-segment distances. This is a Hausdorff-like measure, not an
exact continuous Hausdorff distance. Vertices between sample points can differ
without a large reported distance. Polygon area uses the absolute signed sum of
ring areas; hole winding must follow MVT rules. Area difference is in square
tile units. Counts include the repeated endpoint added by ClosePath. Feature
mean distances are averaged without weighting; total mean is the mean over
matched features. Layer totals sum layer occurrences, features, and vertices
over all tiles, including tiles present in only one input.

Exit codes:

- `0`: tile and layer sets, attributes, types, and semantic metadata match;
  all matched geometry distances are at most `--max-dist` (default 1).
- `1`: at least one comparison fails.
- `2`: input, decode, or output error.

Vertex counts and area differences are measurements, not separate pass/fail
limits. Different gzip bytes, feature IDs, feature order, ring order, or MVT
encoding can still pass. Attribute value wire types are retained, so `int`,
`sint`, and `uint` are distinct. An absent tile or layer fails even if it has
no features. Empty geometry compared to nonempty geometry is an input error.

Metadata output includes both full metadata objects and `all_equal`. The
`equal` pass/fail test checks `json` content (including `vector_layers` and
`tilestats`), `minzoom`, `maxzoom`, `bounds`, and `center`. JSON strings are
parsed; bounds and center strings are converted to arrays. The two standard
JSON sections can also be at the top level of directory metadata. Build
commands, names, descriptions, and other top-level provenance fields do not
fail the test. Array order in metadata is significant.

The JSON report has `tiles`, `tile_sets`, `metadata`, `totals`, and
`layer_totals`. Tile records contain layer counts, key sets, matched feature
attribute maps and metrics, and full unmatched features. Reports can be large.
Details are written as parallel tasks finish, so tile record order is not
stable. Tile coordinates identify records. The tool retains tile summaries,
not all feature details, in memory. SQLite readers are separate for each
parallel chunk. Geometry distance and duplicate matching can take quadratic
time for large individual features or groups. Performance on 300,000
unusually dense tiles is not guaranteed.

## Render and compare

Node 22 is required. Install from the lock file:

```sh
cd tools/render-diff
npm ci
npx playwright install chromium
# On Ubuntu 26.04, use this platform override for installation:
PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=ubuntu24.04-x64 npx playwright install chromium
node index.mjs A.mbtiles B.mbtiles --out /tmp/images \
  --samples 3 --seed 1 --compare /tmp/compare.json --worst 3 --debug
```

Use absolute paths for inputs if the current directory changes. Omit `--debug`
to use the viewer paint settings. Outputs are `index.html`, `summary.json`,
the two generated styles, A and B PNG files for each view, and diff PNG files
for failed views. Open `index.html` in a browser. Each report row shows A, B,
and the diff. Pink pixels mark a change; other pixels show reference gray.
Exit is 0 for a pass, 1 for a pixel failure, and 2 for an execution error.

The renderer is real MapLibre GL JS 6.16.0 in headless Chromium with SwiftShader.
MapLibre Native 6.2.0 was tried on Ubuntu 26.04 but failed to load:
`libOpenGL.so.0: cannot open shared object file`. No replacement rasterizer is
used. The HTTP server binds only to `127.0.0.1` on an available port and closes
after the run. JavaScript, workers, and tile data are local. No public tile,
font, or style service is needed.

Style settings: fill opacity 0.25 with a matching outline, circle radius 4
with a white 1-pixel stroke, and line width 1.5 (2 for `lines`). The layers of the public
benchmark inputs have fixed colors and types (see `tools/render-diff/lib.mjs`).
Other layer IDs use blue fill, line, and circle layers with geometry filters, so
all layers render. Each tileset
gets a style from its own `vector_layers` metadata. This metadata is required.
The viewer's external OSM raster background is replaced with white so that
network and base map changes do not affect the test. The reference viewer has
no symbol/text layers. Neither generated style uses text or glyphs. Debug
uses all layers, 0.5-pixel lines, 1-pixel circles, and faint polygon fill.
Label placement and glyph rendering are therefore not tested.

The Rust JSON report is read as a stream. Only the top ranking records are
kept in memory; one full tile record must fit in memory.

Selection uses the union of both tile sets, a seeded sample per occupied zoom,
and the largest geometry distances in the Rust report. Up to `--worst` missing
tiles from that report are also included. Each selected tile has four views:
zoom z and z+0.5, each at pixel ratio 1 and 2. The viewport is 512 by 512 CSS
pixels, so ratio 2 produces 1024 by 1024 images. The view is at the tile center;
MapLibre can request adjacent tiles. Viewport tests do not cover all buffered
tile edges. At the tileset maximum zoom, fractional views use overzooming.

A pixel differs if any RGBA channel differs by more than
`--color-threshold` (default 3 out of 255). A view fails if the changed pixel
count exceeds `--max-diff-pixels` (default 0). These are absolute pixel counts,
not percentages; ratio 2 has four times as many pixels. Do not compare image
baselines from different browser, MapLibre, driver, or font versions.

## Tests

```sh
cargo test -p vtmake-compare
cargo clippy -p vtmake-compare -- -D warnings
npm test --prefix tools/render-diff
```

The tools were tested on the public corpus only. The generated tilesets and
image reports are not in this repository.
