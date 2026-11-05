# vtmake build: command line and input formats

Status: 2026-11-04. The sections marked "partial" are not complete.
Reference: felt/tippecanoe `4f26211` (v2.82.0).

## Command line

`vtmake build` (or the binary under the name `tippecanoe`) reads all options of
felt's `tippecanoe`. The parser is a port of `main.cpp:main` (`crates/vtmake/src/cli.rs`):

- GNU `getopt_long` rules (`crates/vtmake-tools/src/getopt.rs`, `GetoptLong`):
  `-x value` and `-xvalue`, `--option=value` and `--option value`, groups of
  short options (`-fq`, `-pk`, `-pfk`), unique prefixes of long options
  (`--no-tile` is ambiguous, `--drop-by-attribute` is not, as in glibc),
  `--` ends the options, the operands can come between the options.
- The same defaults, the same checks in the same order, the same messages
  (also glibc's messages and the usage text) and the same exit codes.
- Repeated options: the same result as tippecanoe (the last value, or a list
  for `-L`, `-x`, `-y`, `-T`, `-E`, `-Y`, `--order-by`, `--clip-bounding-box`).
- `-v` writes `tippecanoe v2.82.0 (vtmake v<version>)`.

All settings go into `cli::Options`. The other parts of vtmake use them.

### Options with their effect in vtmake

| Option | Effect |
|---|---|
| `-o`, `-e`, `-f` | output (MBTiles, PMTiles if the name ends with `.pmtiles`, or a tile directory) |
| `-F` / `--allow-existing` | add the tiles to an old MBTiles file or tile directory (see "Output options") |
| `-pC` / `--no-tile-compression` | tiles without gzip |
| `-pg` / `--no-tile-stats` | no `tilestats` in the `json` metadata |
| `--tile-stats-attributes-limit`, `--tile-stats-sample-values-limit`, `--tile-stats-values-limit` | the limits of `vector_layers` and `tilestats` |
| `-n`, `-N`, `-A` | metadata `name`, `description`, `attribution` |
| `-l`, `-L` / `--named-layer` | layer names; `-L` as `name:file` or as JSON (`file`, `layer`, `description`, `format`) |
| `-P` | tippecanoe's segment rules (see below) |
| `-s` | input projection `EPSG:4326` or `EPSG:3857` (also by their `crs` names) |
| `-Z`, `-z` (a number) | zoom range; defaults: 0 to 14 |
| `-zg`, `--smallest-maximum-zoom-guess` | select maximum zoom from feature and vertex spacing |
| `-R` | make one tile |
| `-r`, `-B`, `-Bg` | drop rate and base zoom |
| `-g` | gamma spacing |
| `-al`, `-ap`, `-pb` | rate drops for lines and polygons; base zoom limit |
| `--preserve-point-density-threshold` | keep features that exceed the spacing threshold |
| `-d`, `-D`, `-m` | full, low, and minimum detail |
| `-M`, `-O` | tile byte limit and feature limit |
| `-b` | buffer |
| `-Y` | attribute descriptions in `vector_layers` |
| `-t` | only tippecanoe's check: the directory must let vtmake make a file, else exit 111 |
| `-q`, `-Q`, `-U` | no effect (vtmake has no progress display) |
| `-pe` / `--empty-csv-columns-are-null` | CSV |
| `-aI` / `--convert-stringified-ids-to-numbers` | GeoJSON `id` strings |
| `-as`, `-ae`, `-pf`, `-pk` | the plain command line flags; each flag can be absent (see below) |
| `-y`, `-x`, `-X`, `-T`, `-E`, `-j`, `-J`, `--use-attribute-for-id`, `--set-attribute`, `--maximum-string-attribute-length`, `-pN`, `-ai` | attributes and filters (see "Attributes and filters") |
| `--unidecode-data` | only tippecanoe's check: the file must open, else exit 111 (felt's `tippecanoe` reads the file, but its filters do not use it) |
| `--extra-detail` | the detail of the tile units at the maximum zoom |
| `-S`, `--simplification-at-maximum-zoom`, `-ps`, `-pS`, `-av` | simplification |
| `-pt`, `-pT`, `--tiny-polygon-size` | tiny polygons |
| `-pn`, `-ab` | shared nodes and shared borders |
| `-aL`, `-pc`, `-pD` | grid at low zooms, no clipping, no duplication |
| `-aw`, `-pw`, `-pW`, `--clip-bounding-box` | source geometry |
| `-aP` | label points (see "Geometry options") |
| `-a@`, `-pp`, `-aR` | checks: `-a@`; `-pp` and `-aR` have no effect on the output in tippecanoe 2.82 |
| `-C` / `--prefilter`, `-c` / `--postfilter` | shell filters for each tile (see "Shell filters") |
| `-u` / `--json-progress` | JSON progress lines on the standard error (see "JSON progress") |
| `-v`, `-H` / `--help` | version, usage |

### Options that stop with exit 101

vtmake does not ignore an option that changes tippecanoe's output. If the
command line has an option without its effect in vtmake, vtmake writes
`vtmake build: option <option> is not supported yet` and stops with exit 101.
The check comes after tippecanoe's own checks of the command line, so a wrong
value still gives tippecanoe's message and exit code.

Tile options that still stop with exit 101:
`-aX`. The output options that are not listed in the supported table
also stop with exit 101.

### Shell filters

`-C FILTER` (`--prefilter`) and `-c FILTER` (`--postfilter`) work as in
felt (`plugin.cpp`). The code is in `crates/vtmake/src/plugin.rs`.

- For each tile at `-Z` or above, and for each detail that the tile tries,
  vtmake starts `sh -c FILTER sh z x y`. Thus the filter command gets the
  zoom and the tile numbers as `$1`, `$2` and `$3`. vtmake also starts the
  filter for a tile that has no features after the clip and the zoom checks.
- A second thread writes the input of the filter: one GeoJSON `Feature`
  for each line, with a newline between features and no newline at the
  end. The `"tippecanoe"` member has `layer` and `dropped`, and `index`,
  `sequence` and `extent` if they are not 0. The text is the same as
  felt's text.
- The prefilter gets the features of the tile after the clip, the zoom
  checks and `-j`, with felt's rate-drop marks (`"dropped": true`), in
  world coordinates. Its output features replace the features of the tile,
  and then go through all the other tile steps (drops, coalescing, `-E`,
  simplification, limits). A feature without `"tippecanoe"` has
  `"dropped": true`; a feature without a layer goes to the layer
  `unknown`.
- The postfilter gets the layers of the finished tile, in tile
  coordinates. Its output layers replace the layers before the tile size
  check. Polygons are cleaned again with felt's clip ring. The feature
  limit uses the feature count before the filter. If the count before the
  filter is 0, there is no tile.
- With a filter, the layer statistics of the metadata come from the filter
  output (as felt: the postfilter output if there is a postfilter, else
  the prefilter output; each try of a tile counts). The bounds come from
  the input.
- The exit status of the filter has no effect. Bad JSON from the filter
  stops vtmake with felt's message (`Filter output:LINE: ...`) and exit
  107. If the filter stops before it reads all of its input, vtmake writes
  `Warning: broken pipe in prefilter` (or `postfilter`) once. This warning
  depends on the timing, in felt and in vtmake.

Known differences:

- If the first feature of a layer in the prefilter output has
  `"dropped": true` (also if it has no `"tippecanoe"` member), vtmake keeps
  it as felt does, but the gamma (`-g`), cluster, multiplier and as-needed
  drop steps also look at it. In felt, they do not. The other features are
  the same.
- The prefilter output does not have felt's shared-node data (`-pn`).
- With `-at` and a prefilter, felt does not block the full-detail trial
  of a tile for rate drops or for features with a deeper minzoom (its tile
  thread does not see them). vtmake blocks the trial as it does without a
  filter. (Variable-depth tiles are still partial.)

### JSON progress

`-u` (`--json-progress`) sets `-q` and writes lines `{"progress":12.3}` to
the standard error while vtmake makes the tiles (felt `json_logger.cpp`).
The code is in `crates/vtmake/src/progress.rs`. As in felt, each try of a
tile sets the last value to 0, and a line comes when the value is 0.1 or
more above the last value. felt checks at each feature of the try and at
the end of the try. vtmake does the same checks at the start and at the
end of each try. `-U` gives the minimum time between two lines.

The value is `floor((f + z) / (maxzoom + 1) * 1000) / 10`, as in felt. In
felt, `f` is the byte position in the compressed temporary file of zoom
`z`, divided by the size of that file. vtmake has no such files. Its `f` is
the position of the tile in quadkey order at zoom `z`, and inside the tile,
the fraction of its features. Thus the values are near felt's values, but
they are not the same. For `ne_110m_populated_places` with `-z5` and one
thread, the first 172 of felt's 885 lines are the same (vtmake: 569 lines). Also, vtmake makes each tile and then its
child tiles, so without tile limits the zoom of the lines goes up and down.
With more than one thread, the order of the lines changes from run to run,
in felt and in vtmake.

### Variable-depth tiles (partial)

`-at` / `--generate-variable-depth-tile-pyramid` tries a full-detail leaf
tile before the normal tile pass. It stops the child tiles if the leaf
keeps all required features and fits the limits. Four of the six felt
variable-depth targets pass, with identical compressed tiles and metadata.
Skipped tiles at the last zoom take part in center selection. Two polygon
cases still differ in trial selection and strategy metadata.

These are the two known differences, with two threads:

- `tl_2022_11_tract`: `-z17 -Z10 --coalesce-densest-as-needed
  --generate-variable-depth-tile-pyramid -M15000`.
- The same input: `-z17 -Z10 --generate-variable-depth-tile-pyramid
  --drop-by-attribute-as-needed=ALAND -M15000`.

Both cases have 36 identical compressed tiles. Only `strategies` differs.
Felt selects a full-detail trial if
`0.6693 * estimated_complexity - 33600 < 0.9 * max_tile_size`.
It measures `estimated_complexity` from child queue bytes before it ends
compression (`tile.cpp:write_tile`). These bytes exclude pending compressor
output. Rust stores feature references and geometry in another queue format.
Thus a geometry count or a finished compressed size cannot give the same
trial decisions. Extra failed trials increase `tile_size_desired` at z10
and z11. They do not change the accepted tiles in these two cases.
An exact port needs felt's serial queue format and compression state.
vtmake does not make this change because it would affect the queue design.

### Retained-point multipliers (partial)

`--retain-points-multiplier` keeps secondary features near each lead feature.
The selection uses felt's fractional feature zoom and a count for each layer.
`--preserve-multiplier-density-threshold` can keep more secondary features.
Each kept feature gets its sequence number within the layer. Each lead gets
the `tippecanoe:retain_points_multiplier_first` marker. Input order and attribute
order apply to whole groups. Secondary features use felt's drop-sequence order.
The marker follows the sequence attribute after a group sort, as in felt.

The eight suite cases for these options pass. This includes the case with
`--drop-smallest-as-needed -M10000`. Dynamic limits grow with the ratio of
all group features to lead features. A lead that meets a drop threshold can
stay as a secondary feature if the group has space. Once the group is full,
the dropped lead also removes its secondary features. Features kept by the
density threshold do not use group space.

### Clustering and coalescing (partial)

`-K`, `-k`, `-aa`, `-aC`, `-aS`, `-aD`, `-aN`, `-ag`,
`-ac`, `-ao`, `-ar`, `--drop-denser`, `--order-smallest-first`, and `--order-largest-first`
are available. Clusters get the four felt attributes: `clustered`,
`point_count`, `sqrt_point_count`, and `point_count_abbreviated`.
The attribute drop hook (`drop_onto` in `tile.rs`) preserves accumulated
attributes.

This is a partial implementation. Polygon and line edge cases, general coalescing,
thresholds other than density, and some retry counters still differ. Do not use
this checkpoint as proof of full parity for these options.

#### Known polygon ring start differences

These cases have the same features, attributes, ring coordinates, winding,
and geometry, but different ring start vertices:

- `coalesce-tract`: `-P --coalesce --reorder -z11 -Z11 -y STATEFP10
  -y COUNTYFP10 -l merged`. Tile `11/327/791` has one rotated ring
  with three vertices. The other 24 tiles have identical bytes.
- `ne_110m_admin_0_countries`: `-z5 -M5000
  --coalesce-fraction-as-needed`. Tile `3/6/5` has two rotated rings,
  with 13 and 23 vertices. The other 872 tiles have identical bytes.

The changed start changes the MVT MoveTo and relative coordinate bytes.
It does not change the ring edges. Full Wagyu cleaning gives the same
Rust output as the default cleaning path. Disabling the convex fast path
does not remove the difference. The step that first changes the start
has not been isolated. A rule that rotates every ring cannot safely
reproduce felt's union output. vtmake keeps the present code.

The Rust comparison passes at geometry distance 0 in both cases.
MapLibre checks use both styles, each stored zoom, and both changed tiles.
The coalesce/reorder debug check has one pixel with a channel change of 1
at z11.5 and pixel ratio 2. This is 0 changed pixels at the documented
color threshold of 3. Thus this check does not prove exact RGBA identity.

### Dynamic tile limits (partial)

`-as`, `-ad`, `-an`, `-aG`, `-pd`, `--drop-by-attribute-as-needed`, and
`--drop-by-attribute-order` work with active `-M` and `-O` limits.
Each tile can repeat with a higher threshold. The whole zoom then repeats
with the largest threshold of all its tiles. Thresholds start again at each
zoom. The metadata includes the final drop counters and the largest size
and feature count that caused a retry.

`-ae` and `--extend-zooms-if-still-dropping-maximum` extend the zoom range
when a retry changes a drop threshold. Each new zoom starts with no dynamic
drops. `-pi`, `--order-by`, `--order-descending-by`, `-ah`,
`--limit-tile-feature-count`, and `--limit-tile-feature-count-at-maximum-zoom`
also work. The feature limits apply to each layer, after ordering.

The reference uses two threads. This is a partial checkpoint.
Coalescing, clustering, multiplier points, variable depth, size ordering,
density attributes, and feature index attributes remain open.

### Default tile mode (partial)

The four plain command line flags are no longer necessary. Without `-ae`, vtmake rounds
stored coordinates to the grid set by `32 - (full_detail + maxzoom)`.
It does this before polygon ring setup and when it writes child geometry.
Without `-as`, line and polygon order uses the vertex selected by felt's
geometry hash.

Without `-pf`, the default feature limit is 200,000. Without `-pk`, the
default compressed tile size limit is 500,000 bytes. A tile that exceeds the
size limit is made again with lower detail, down to `-m`. A tile that
exceeds the feature limit fails if no drop strategy is set. Earlier zooms
are kept. The exit code is 110 if no complete zoom range is available, or
100 if only part of the zoom range is complete.

The default tile mode is not complete. Dynamic drop retries and zoom
extension after real drops remain open. Failed zooms can contain a different set of partial
tiles because vtmake and felt assign work to threads differently. The
checks on subsets of the public corpus pass.

## Output options

The code is in `crates/vtmake-out` and `crates/vtmake/src/meta.rs`. The checks,
messages, and exit codes are those of felt's `main.cpp`, `mbtiles_open`,
`check_dir`, and `check_pmtiles`.

- `-f`: delete the old MBTiles or PMTiles file, or the old `metadata.json`
  and tiles of the directory.
- `-F` with MBTiles: the old file stays. tippecanoe writes a message for each
  table, index, and metadata row that is there already, and the old metadata
  rows stay. A tile that is there already stops with exit 116
  (`sqlite3 map insert failed`).
- `-F` with a directory: the old tiles and the old `metadata.json` stay. A
  tile that is there already stops with exit 104 (`Can't write tile to already
  existing`). The first such tile in the message can be a different one,
  because it depends on the thread order.
- `-F` with PMTiles: an old file stops with exit 104 (felt: "--allow-existing
  is not supported for pmtiles").
- PMTiles output: the tiles go to an MBTiles database at the output path, and
  at the end to `<output>.tmp`. Then the PMTiles file replaces both (as felt).
  The memory use does not depend on the tileset size.
- `--tile-stats-*-limit`: as in felt, a negative value is a very large limit,
  the sample limit is at least the values limit, and `vector_layers` has at
  least one field per layer (also with `--tile-stats-attributes-limit=0`).
  `--tile-stats-values-limit=0` removes `tilestats`, as `-pg` does.


## Attributes and filters

The code is in `crates/vtmake/src/attrs.rs` and the crate `crates/vtmake-filter`. The
rules are tippecanoe's rules, in tippecanoe's order.

When vtmake reads a feature (`serial.cpp:serialize_feature`):

1. `--set-attribute`: the value replaces the first attribute with the same
   name, else vtmake adds the attribute at the end. The names are in byte
   order. A value in the `name:value` form is a number if it starts with a
   digit or `-`, else a string.
2. For each attribute, from the last to the first:
   - `-T`: `int` (`round` of the number, written with `%f`, if the text is
     not only digits), `float`, `string`, `bool` (`false` for `false`, `0`,
     `null`, an empty text and the number 0). A null can become a string or
     a bool.
   - `-pN`: a number with a `.` becomes the nearest `float` value.
   - `--use-attribute-for-id`: a number (or, with `-aI`, any text) becomes
     the feature ID if tippecanoe's test accepts it (`strtoull`, and the
     result as a signed number is the text without its leading zeros), and
     the attribute is removed. Else one warning (once for each kind).
   - `-X` with `-y` (`-y` sets `-X`): only the `-y` attributes stay. Else
     `-x` removes the attribute.
3. The tile statistics of the metadata come from the result.
4. `--maximum-string-attribute-length`: a longer string is cut at the start
   of a UTF-8 character. The tile statistics keep the full string.
5. `-ai`: a feature without an ID gets the ID `seq + 1`. `seq` is
   tippecanoe's number of the feature: with one reader, the count of the
   features before it (of all sources); with `--read-parallel` (or RS input),
   the byte position of its reader segment plus the count of the features
   before it in the segment; for FlatGeobuf, the batches of
   `CPUS * 500 + 1` features in `CPUS` parts. A feature that tippecanoe does
   not keep (for example an empty geometry) does not count.

In each tile (`tile.cpp:next_feature`, `write_tile`):

1. `-j` and `-J`: after the zoom checks of the feature and before the dot
   dropping. The filter of the layer and the filter of `*` must be true. The
   feature still goes to the child tiles. The keys `$id`, `$type` and `$zoom`
   are available; an attribute with the same name comes first. The
   operators: `has`, `!has`, `==`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `!in`,
   `all`, `any`, `none` and `attribute-filter` (it removes the attribute from
   the feature in this tile if its expression is false). `true`, `null`, a
   number that is not 0 and a text that is not empty are true. A key that is
   not there or a type that is not the same gives false (`!=` and `!in`:
   true). For `bool`, tippecanoe compares `a > b`, so `false == true`.
   Errors stop with tippecanoe's message and exit code (105, 107 for a
   filter that is not an object), when the first feature comes to the filter.
2. Nulls are removed.
3. `-E`: a dropped feature (dot dropping) adds its attributes to the last
   kept feature of the same layer and type in the tile: `sum`, `product`,
   `mean`, `max`, `min`, `concat`, `comma`, `count`. A changed attribute moves
   to the end of the feature. `product`, `concat` and `comma` on a feature
   without the attribute stop with exit 106, as in tippecanoe. The changed
   values also go into the tile statistics of the metadata.

A number text that is not a JSON number (from `--set-attribute` or `-T`, for
example `08`, `0x1F` or `9x`) gets the MVT type of tippecanoe's C functions
(`strtoull`, `strtoll` with base 0, `strtof`, `strtod`).

Without these options, vtmake uses the same code paths as before (no cost).

Known difference: vtmake reads the lines of a file in parallel, so a warning
that tippecanoe writes once can name another feature.

## Inputs

The sources are in tippecanoe's order: the `-L` sources, then the file
operands; no source: standard input. A file that does not open gives
tippecanoe's message, and the reading continues with the next source. The
layer of a source: `-l`, else the `-L` name, else the file name
(`layer_from_path`), and for standard input the `-n` name or the output name.
The description of a layer (`vector_layers`) comes from the first `-L` source
of that layer.

| Input | Rule (tippecanoe's) |
|---|---|
| `.fgb` file, or `-L` `"format":"fgb"` | FlatGeobuf (`vtmake-parse/src/fgb.rs`) |
| `.csv` file, or `-L` `"format":"csv"` | CSV (`vtmake-parse/src/csv.rs`) |
| `.geobuf` file, or `"format":"geobuf"` | stops with exit 101: not supported yet |
| other | GeoJSON |

### GeoJSON

The parser is a port of tippecanoe's jsonpull (`vtmake-parse/src/pull.rs`) and
GeoJSON loop (`vtmake-parse/src/geojson.rs`): any JSON layout (NDJSON, one
large FeatureCollection, pretty-printed, values over many lines, more values on
one line), features in containers of any kind (but not in `properties`), bare
geometries before the first feature, `GeometryCollection`, `crs` warnings, the
byte-order mark, RS (0x1E) as white space, and the same warnings and fatal
errors. A syntax error stops the stream; the features before it stay.

How tippecanoe splits a source into streams (each stream has its own parser):

| Source | Without `-P` | With `-P` |
|---|---|---|
| regular file (also standard input from a file) | one stream | CPUS segments, split at `\n` |
| file that starts with RS | CPUS segments, split at RS | the same |
| pipe or `.gz` file | one stream | chunks of more than 10 MB that end at a 2000-byte read with `\n`, each split into CPUS segments |

CPUS is tippecanoe's thread count: `TIPPECANOE_MAX_THREADS`, else the number of
online CPUs, rounded down to a power of 2. For a valid NDJSON file the result
does not depend on the split. For other input it can, in tippecanoe too: use
the same `TIPPECANOE_MAX_THREADS` for the same result. For a pipe, tippecanoe
also waits for its parser threads before a chunk ends; vtmake takes the first
possible end.

`.gz`: the decision comes from the file name of the first source with the same
layer name (as `sources[layer].file` in tippecanoe). zlib reads data that is not
gzip as it is; vtmake does the same.

vtmake reads each stream in two phases (`crates/vtmake/src/read.rs`):

1. In parallel, line by line, the fast parser reads each line that is one
   simple Feature (also with a comma after it, as in a FeatureCollection with
   one feature per line).
2. In order, the jsonpull port reads the other lines, and all lines that are not
   at the top level or in a list of features. It removes the phase 1 features of
   these lines.

So NDJSON is read fully in parallel, and other GeoJSON gets the result of
tippecanoe. Text that is not UTF-8, or strings with UTF-16 surrogates in keys
or values, stop with exit 101 (tippecanoe keeps the bytes; vtmake keeps
UTF-8 text only). Invalid UTF-8 in a property value stops with exit 119, as in
tippecanoe.

### Memory of the input text

vtmake does not keep all the input text in memory (`crates/vtmake/src/read.rs`,
`crates/vtmake/src/read/stream.rs`). The memory for the text has a limit;
the features that vtmake reads stay in memory (they are the input of the
tiling).

| Input | How vtmake reads it |
|---|---|
| regular file | `pread`, pieces of 128 MB (4 pieces at a time) |
| pipe or `.gz` file, one stream | a reader thread fills windows of 64 to 256 MB that end after a `\n`; the jsonpull state goes from one window to the next |
| pipe or `.gz` file, `-P` or RS | windows of complete tippecanoe chunks (see the table above); a chunk of more than 512 MB goes into a temporary file in `-t` (tippecanoe puts all chunks there) |
| CSV, FlatGeobuf | blocks of 4 to 32 MB; the pieces of a block are parsed in parallel, then the messages and the first error are written in input order |

A line that is longer than 64 MB (for example a FeatureCollection on one
line) is cut; the jsonpull port reads the parts. The result is the same as
with the full text: the same features in the same order, the same `seq` and
`-ai` IDs, the same messages and exit codes. Known difference (as before):
for CSV and FlatGeobuf, the attribute warnings (for example of
`--use-attribute-for-id`) can come in a different order than in tippecanoe
relative to the read messages; with a read error, vtmake does not write the
attribute warnings of the features in the same block.

### CSV

A port of `geocsv.cpp` and `csv.cpp`: the column of the latitude (`y`, `lat`,
`*latitude*`) and of the longitude (`x`, `lon`, `lng`, `long`, `*longitude*`),
quotes, numbers in JSON syntax, empty values (null with `-pe`), the messages
and the exit codes (103, 119).

### FlatGeobuf

A port of `flatgeobuf.cpp` with a small FlatBuffers reader (no new crate): all
column types (Binary stops with exit 106, as in tippecanoe), Point, MultiPoint,
LineString, MultiLineString, Polygon, MultiPolygon, the per-feature type if the
header type is Unknown, and the feature IDs from the sequence number of an
indexed file.

## Tests

`bench/attr-check.sh` compares vtmake with felt on 124 cases of the attribute
and filter options (felt's tests in place, and the generated inputs of
`bench/attr-inputs.py` with all value types, IDs of all kinds, `-ai` with all
reader paths, the filter operators and their errors, `-E` with dot dropping).

`bench/input-check.sh` compares vtmake with felt on 145 cases: GeoJSON forms
(felt's `tests/` inputs in place and the variants of `bench/input-variants.py`),
standard input, `.gz`, RS, `-P`, layers and metadata, CSV, FlatGeobuf (generated
files of all geometry and column types), and 49 command line errors (exit code
and stderr). It compares the exit code, the metadata, and the tiles
(byte-identical, with vtmake-compare).

## Geometry options

The per-feature geometry steps of `tile.cpp:write_tile` and the read steps
of `serial.cpp:serialize_feature` have all geometry options of felt. The
ports are in `crates/vtmake-geom` (`GeomOpts`, `visvalingam.rs`,
`label.rs`, `shared_borders.rs`, the `-pn` clip in `clip.rs`). The reader
does `-aw`, `-pw`, `-pW`, `--clip-bounding-box` and the global nodes of
`-pn` (`crates/vtmake/src/read.rs`). The tile steps are in
`crates/vtmake/src/tile.rs` (`Geo`).

| Option | Effect in vtmake (as in felt) |
|---|---|
| `-d`, `-D` | the detail at `-z` and below `-z` (`main.cpp` reduces them above `-z20`) |
| `--extra-detail` | at `-z`: the tile units and the layer extent use this detail (not more than `30 - z`); the simplification uses `-d` |
| `-S`, `--simplification-at-maximum-zoom` | the tolerance of the simplification; at `-z` the second value if it is set |
| `-ps`, `-pS` | no simplification of lines and polygons; at `-z` only |
| `-av` | Visvalingam (a port of `visvalingam.cpp`, with the same heap) |
| `-pt`, `-pT`, `--tiny-polygon-size` | no tiny-polygon reduction; at `-z` only; the size limit |
| `-aL` | below `-z`: stair steps on the tile grid, no simplification, no tiny-polygon reduction (also not at `-z`) |
| `-pc` | a feature keeps its full geometry in each tile where its clipped geometry is not empty (not at z0) |
| `-pD` | a feature keeps its full geometry only in the tile that has the center of its bbox (not at z0) |
| `-pn` | `remove_noop` at read time; the global nodes (vertex mid points with two different pairs of neighbours, the line ends, the ring starts and two far points of each ring); the polygon clip adds points on the tile edge and keeps them as nodes; the simplification keeps all nodes; lines do not get `remove_noop` before the simplification |
| `-ab` | `find_common_edges` for the polygons of each layer of a tile: each arc between two points where the rings on the two sides change is simplified once (felt 2.82 does not merge polygons: its merge fraction is 1) |
| `-aw` | in each part, a jump of more than 180 degrees moves the rest of the part by 360 degrees; the metadata bounds come from the input geometry |
| `-pw`, `-pW` | the ring winding of the input gives the outer and inner rings (`-pw`); no ring is reversed (`-pW`) |
| `--clip-bounding-box` | each box (in order) clips the features at read time; the features that the boxes remove count in the metadata bounds |
| `-aP` | polygons become label points (`polygon_to_anchor` at read time, `checkerboard_anchors` in each tile) |
| `-a@` | after the polygon cleaning: `remove_noop` and felt's messages for rings that are not valid |
| `-m` | the value only: it has an effect only in the retry loop of a tile that is too large |
| `-pp`, `-aR` | no effect on the output in felt 2.82 |

### `-aP` is not deterministic in felt

In the label point step of `write_tile`, felt reads `sf.simplification` before
it sets it (this field of `serial_feature` is not initialized). Thus felt's
output with `-aP` changes from run to run, and felt fails its own `-aP` test
(`tests/ne_110m_admin_0_countries/out/-z2_--convert-polygons-to-label-points.json`)
in each baseline run. vtmake uses the value that felt sets in this field
later: the simplification of the zoom. The label point functions themselves
are equal to felt's (cpp-ref differential tests).

### Tests

- `bench/geom-check.sh` compares vtmake with felt (2 threads, with the plain
  command line flags added) on felt's own test option sets, on subsets of the public
  corpus and on generated edge cases (`bench/geom-inputs.py`). It compares the
  exit code, the metadata, the tiles (byte-identical) and, for the `ts` cases,
  stderr.
- `cargo test -p vtmake-geom --features cpp-ref --release --test diff_opts`:
  differential tests of the new geometry functions against felt's C++ code
  with the same `prevent[]` and `additional[]` flags.
