# vtmake join, decode, enumerate, overzoom, and json-tool

These commands replace felt's tools (felt/tippecanoe 4f26211, v2.82.0):

| vtmake | felt tool | Port of |
|---|---|---|
| `vtmake join` | `tile-join` | `tile-join.cpp`, the metadata part of `mbtiles.cpp` |
| `vtmake decode` | `tippecanoe-decode` | `decode.cpp`, `write_json.cpp` |
| `vtmake enumerate` | `tippecanoe-enumerate` | `enumerate.cpp` |
| `vtmake overzoom` | `tippecanoe-overzoom` | `overzoom.cpp`, `overzoom()` of `clip.cpp` |
| `vtmake json-tool` | `tippecanoe-json-tool` | `jsontool.cpp`, `geojson-loop.cpp` |

The code is in `crates/vtmake-tools`. The feature filter (`-j`, `-J`) is in
`crates/vtmake-filter` (a port of `evaluator.cpp`); `vtmake build` uses the
same crate.

Start the binary as `vtmake <command>`, or through a symlink with the name of
the felt tool. The options are the options of the felt tool.

## Result

- `decode`: the standard output is byte-identical to felt's tool: the JSON
  layout, the spaces, the numbers (`%f` for coordinates, `dtoa_milo` for other
  numbers), the order of features and properties, and the metadata header of a
  whole tileset. The exit codes and the error messages are also the same.
- `join`: the tiles (bytes) and the metadata rows (`-o`) or `metadata.json`
  (`-e`) are byte-identical to felt's tool. Only the `generator` value is
  different: `vtmake v<version>` (felt writes `tile-join v2.82.0`).

`bench/tools-check.sh` keeps the original 668 cases and adds PMTiles, CSV,
enumerate, filter, overzoom, json-tool, and build output cases (see the end
of this file).

## vtmake decode

Input: an `.mbtiles` file, a tile directory (`-e` output; the metadata comes
from `metadata.json`), an `.pmtiles` file, or one `.pbf` tile (with `z x y`). The tiles can be
gzip, zlib or not compressed.

```sh
vtmake decode file.mbtiles                  # the whole tileset
vtmake decode -s EPSG:3857 file.mbtiles z x y   # one tile
```

If there is no tile at `z/x/y`, decode uses the parent tile (as felt's tool)
and writes a warning.

| Option | Status |
|---|---|
| `-z`/`--maximum-zoom`, `-Z`/`--minimum-zoom` | supported |
| `-l`/`--layer` | supported |
| `-s`/`--projection` (`EPSG:4326`, `EPSG:3857`) | supported |
| `-F`/`--fractional-coordinates`, `-I`/`--integer-coordinates` | supported |
| `-c`/`--tag-layer-and-zoom` | supported |
| `-S`/`--stats` | supported |
| `-y`/`--include`, `-x`/`--exclude-metadata-row` | supported |
| `-f`/`--force` | supported |
| PMTiles input | supported |

## vtmake join

Input: one or more `.mbtiles` files, `.pmtiles` files, and tile directories (with or without
`metadata.json`). Output: `-o file.mbtiles`, `-o file.pmtiles`, or `-e directory`.

What join does (as felt's tool):

- It decodes each tile and encodes it again. The layers of all inputs with the
  same tile are merged, in the order of the input tile bytes. The key and value
  tables are made again, and the tile gets gzip again.
- It skips a tile that is larger than 500000 bytes after gzip (not with `-pk`).
- The bounds come from the tile geometry. The center, the attribution and the
  description come from the input metadata (the last input in tile order). The
  names of the inputs are joined with ` + `. The zooms are the zooms of the
  tiles that it wrote.
- `vector_layers` and `tilestats` come from the tile features. The layer
  descriptions and the field descriptions come from the input metadata.

| Option | Status |
|---|---|
| `-o`/`--output`, `-e`/`--output-to-directory`, `-f`/`--force` | supported |
| `-n`/`--name`, `-N`/`--description`, `-A`/`--attribution` | supported |
| `-r`/`--read-from` | supported |
| `-z`/`--maximum-zoom`, `-Z`/`--minimum-zoom` | supported |
| `-b`/`--buffer` | accepted (felt uses it only with `--overzoom`) |
| `-l`/`--layer`, `-L`/`--exclude-layer`, `-R`/`--rename-layer` | supported |
| `-x`/`--exclude`, `-y`/`--include`, `-X`/`--exclude-all` | supported |
| `--exclude-all-tile-attributes` | supported |
| `-i`/`--if-matched` | supported (without `-c`, no feature matches) |
| `-pk`/`--no-tile-size-limit`, `-pC`/`--no-tile-compression`, `-pg`/`--no-tile-stats` | supported |
| `-pe`/`--empty-csv-columns-are-null` | supported (it has an effect only with `-c`) |
| `--tile-stats-attributes-limit`, `--tile-stats-sample-values-limit`, `--tile-stats-values-limit` | supported |
| `-q`/`--quiet` | supported |
| `-c`/`--csv` | supported |
| `-j`/`--feature-filter`, `-J`/`--feature-filter-file` | supported (see below) |
| `-O`/`--overzoom` | supported (see below) |
| `--exclude-all-tile-geometries` | supported: the features have no type and no geometry |
| `--unidecode-data` | supported: felt's tool reads the file, but its filter does not use the data. Only an open error changes the result (exit 111) |
| PMTiles input and output | supported |

Filters in `join`: tile-join tests each feature of each input tile, after
`-R` renames the layer. A filter can use the attributes, `$id`, `$type`, and
`$zoom` (the zoom of the tile). `$id`, `$type`, and `$zoom` come before an
attribute with the same name. `attribute-filter` removes the attribute from
the feature (also from the CSV columns).

`-O` in `join`: an input that is done continues with overzoomed tiles. At the
first zoom after its highest zoom, these are the 4 children of each tile at
its highest zoom. After that, they are the children that have features. Each
overzoomed tile comes from the parent tile at the highest zoom (`overzoom()`
with `-b`, the source detail, and no compression). The join stops when all
inputs are done and no input gave an overzoomed tile at its current zoom. Thus
`-O` has an effect only when another input has tiles at higher zooms. With
one input, the output does not change (as felt's tool).

`TIPPECANOE_MAX_THREADS` sets the number of threads. The output does not
change with the number of threads.

Limits: a tile string (layer name, key or value) that is not UTF-8 stops join
and overzoom with exit 101. tippecanoe always writes UTF-8, so this does not
occur with tippecanoe tiles.

## vtmake overzoom

```sh
vtmake overzoom -o new.pbf tile.pbf 11/327/791 13/1310/3166
vtmake overzoom -o new.pbf -t 13/1310/3166 a.pbf 11/327/791 b.pbf 0/0/0
```

It makes the tile nz/nx/ny from the source tiles that contain it. Each
feature goes to world coordinates, then it is clipped to the new tile and the
buffer. Then come the filter, the tiny polygon reduction, the simplification,
the scaling to the new extent, and the cleaning of polygons (wagyu, scale 1).
The output tile is gzip (not with `--no-tile-compression`) or empty if no
feature is left.

All options of felt's tool are supported: `-o`, `-t`/`--source-tile`,
`--no-tile-compression`, `-d`/`--full-detail`, `-b`/`--buffer`,
`-y`/`--include`, `-x`/`--exclude`, `--exclude-prefix`,
`-E`/`--accumulate-attribute` (all 8 methods), `-j`, `-J`,
`-m`/`--filter-points-multiplier`, `--deduplicate-by-id`,
`-S`/`--line-simplification`, `--tiny-polygon-size`,
`--preserve-input-order`, and `--unidecode-data` (as in `join`).

`-m` makes one feature from each cluster of `--retain-points-multiplier`
(the attribute `tippecanoe:retain_points_multiplier_first`). `-E` adds the
attributes of the other features of the cluster to it. As in felt's tool,
`concat`, `comma`, and `product` stop with exit 106 if the first feature of
the cluster does not have the attribute.

## vtmake json-tool

```sh
vtmake json-tool -e GEOID10 in.json | LC_ALL=C sort > sorted.json
vtmake json-tool -c data.csv sorted.json > joined.json
```

It writes each GeoJSON feature (or bare geometry) of the input as one line of
compact JSON (the JSON text of felt's parser). Without files, it reads the
standard input. Options: `-w`/`--wrap` (one FeatureCollection or
GeometryCollection), `-e`/`--extract` (a sort key before each feature; the
key text sorts in UTF-32 order), `-c`/`--csv` (join the CSV columns of the
first CSV column to the features; the GeoJSON and the CSV must be in order),
`--empty-csv-columns-are-null` and `-pe`. The messages, the warnings, and the
exit codes are the same as felt's tool.

## Checks

`bench/tools-check.sh` makes tilesets with felt tippecanoe and with
`vtmake build` (`-o` and `-e`) from subsets of the public corpora, private
test data (not in this repository), and the synthetic inputs of
`bench/meta-inputs.py` (all value types, ids, multi-geometries, many layers,
long names, edge numbers, a tile larger than 500000 bytes). Then it runs each
case with felt's tool and with vtmake under the felt name, and compares the
bytes.

Original result (2026-11-04): 668 cases (569 decode, 99 join), 668 identical, 0
different. The standard error is also the same in all cases.

## PMTiles and enumerate

`vtmake enumerate file.mbtiles ...` lists each tile as `file z x y`. Start the
binary through a `tippecanoe-enumerate` symlink to use the felt tool name.
The order is zoom, column, then TMS row, as in felt. There are no options.

The PMTiles code uses v3 headers, Hilbert tile IDs, clustered tile data,
content reuse, run lengths, and root and leaf directories. Internal data uses
gzip. Tile data can use gzip or no compression. PMTiles output uses felt's generator value so that the complete file can match felt's bytes.
MBTiles output keeps the vtmake generator value.

The PMTiles reader makes a temporary database in memory. The writer works as
felt: the tiles go to an MBTiles database at the output path. At the end, the
tile data goes to `<output>.tmp` in tile ID order, and then the PMTiles file
replaces the database. Thus the writer keeps only the directory entries in
memory, not the tile data. The same writer makes the PMTiles output of
`vtmake build` and `vtmake join`. `-pC` sets the tile compression of the
header to none.

Output and tools checks, part 1 (2026-11-04): the added cases include PMTiles
decoding, parent tile lookup, joins from PMTiles, CSV attribute joins, complete
PMTiles output files, and enumerate output. The PMTiles fixtures include 18,000 distinct tiles with
leaf directories, repeated tile content, no tile compression, no tile statistics,
and an empty zoom range. Eight build checks use the plain command line flags.

The felt-suite runner on this checkout has 205 targets: 1 PASS (`pbf-test`),
203 FAIL, and 1 ERROR (the missing C++ `unit` tool). The PMTiles, decode-PMTiles,
enumerate, and join targets stop at build flags that are not supported yet.
Overzoom and json-tool are not present. These results do not establish that the
complete felt-suite targets pass after the build work is merged.

Extended result: 787 cases, 787 identical, 0 different. This includes all 668
original cases, 617 decode cases in total, 147 join cases, 8 build cases, and
15 enumerate cases. Complete PMTiles files are compared without normalization.
The MBTiles generator value is normalized, as in the original checks.

Workspace Clippy passes with warnings denied. All 15 tests in `vtmake-out`
and `vtmake-tools` pass. Two-thread smoke timings on CPUs 6 and 7 show no
regression against a build of the original branch HEAD. Other work on the
host can affect these short timings.

## Output and tools checks, part 2 (2026-11-04)

`bench/tools-check.sh`: 963 cases, 963 identical, 0 different. These are
the 787 cases of part 1 and 176 new cases:

- 35 `join` cases: filters (all operators, `$id`, `$type`, `$zoom`,
  `attribute-filter`, `-R`, CSV, tile directories, `-J`, errors), `-O`
  (MBTiles, directory and PMTiles inputs, `-b`, `-z`, one input, the same
  input two times), `--exclude-all-tile-geometries`, `--unidecode-data`.
- 70 `overzoom` cases: the commands of felt's `overzoom-test`, all `-E`
  methods, filters, `-S`, `--tiny-polygon-size`, `--exclude-prefix`,
  `--deduplicate-by-id`, `-d`, `-b`, and tiles of the public and private test
  tilesets at higher zooms; errors.
- 38 `json-tool` cases: the commands of felt's `json-tool-test`, `-w`, `-e`,
  standard input, the synthetic inputs; errors.
- 33 `vtmake build` cases: `-pC`, `-pg`, the `--tile-stats-*` limits, PMTiles
  output (the same bytes), and `-F` with MBTiles, a directory, and PMTiles.

felt's test suite (`bench/felt-tests/run.py`, 205 targets):

- All six tools are vtmake: 2 PASS (`pbf-test`, `json-tool-test`). The
  other tool targets stop at a `tippecanoe` command without the plain command
  line flags (these build options were not complete at that time).
  `overzoom-test` passes all its `tippecanoe-overzoom` commands up to the
  first `tippecanoe` command (`--retain-points-multiplier`).
- felt's `tippecanoe` with the five vtmake tools: 203 PASS. The two other
  cases also fail with felt's own tools: `unit` (the C++ unit test binary)
  and `-z2_--convert-polygons-to-label-points` (its expected file differs).
  Thus `overzoom-test`, `json-tool-test`, `join-test`, `join-filter-test`,
  `enumerate-test`, `decode-test`, `pmtiles-test`, `decode-pmtiles-test`,
  `allow-existing-test`, `csv-test`, `layer-json-test`, `raw-tiles-test`
  and `pbf-test` pass with the vtmake tools.
