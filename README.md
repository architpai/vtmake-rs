<img src="assets/icon.svg" width="64" height="64" alt="">

# vtmake-rs

`vtmake` makes Mapbox Vector Tiles (MVT) from GeoJSON. It is a Rust
reimplementation of [felt/tippecanoe](https://github.com/felt/tippecanoe), and
it is a drop-in replacement for tippecanoe v2.82.0 and its tools: felt's own
test suite passes 200 of 205 targets (see "Compatibility" below). For the
plain seed command line (below) it makes **byte-identical tiles**, and on a small
2-CPU machine it is 6x to 28x faster than felt (see "Speed" below).

vtmake would not exist without tippecanoe. Its tiling logic is a port of
tippecanoe's code, and it uses wagyu and zlib. See [NOTICE](NOTICE) for the
credits and licences.

## Commands

| Command | Replaces | Status |
|---|---|---|
| `vtmake build` | `tippecanoe` | all options of felt's tippecanoe (see [docs/build-options.md](docs/build-options.md)) |
| `vtmake join` | `tile-join` | byte-identical tiles and metadata; all options (see [docs/tools.md](docs/tools.md)) |
| `vtmake enumerate` | `tippecanoe-enumerate` | lists MBTiles tile coordinates in felt order |
| `vtmake decode` | `tippecanoe-decode` | byte-identical output; includes PMTiles input (see [docs/tools.md](docs/tools.md)) |
| `vtmake overzoom` | `tippecanoe-overzoom` | byte-identical tiles; all options |
| `vtmake json-tool` | `tippecanoe-json-tool` | byte-identical output; all options |

Each command takes the flags of the tool it replaces. If the binary is started
under the name `tippecanoe`, `tile-join`, `tippecanoe-decode`,
`tippecanoe-enumerate`, `tippecanoe-overzoom`, or `tippecanoe-json-tool` (for
example through a symlink), it acts as that tool.

`vtmake build` reads all options of felt's `tippecanoe` with its parsing rules,
checks, messages and exit codes. Options with their effect now: `-o`, `-e`,
`-f`, `-n`, `-N`, `-A`, `-l`, `-L`/`--named-layer`, `-P`, `-s`, `-Z`, `-z`,
`-b`, `-Y`, `-t`, `-q`, `-Q`, `-U`, `-u`, `-pe`, `-aI`, the shell filters
`-C`/`-c`, and the seed flags
`--drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit
--no-tile-size-limit`. Default tile mode and zoom selection also work,
with limits listed in [docs/build-options.md](docs/build-options.md). An option without its effect yet stops
with exit 101 (`option <x> is not supported yet`). Input: GeoJSON in all forms
(NDJSON, FeatureCollection, pretty-printed, RS sequences, `.gz`), CSV and
FlatGeobuf, from files or standard input, with tippecanoe's exact feature
order. See [docs/build-options.md](docs/build-options.md).

```sh
vtmake build -Z12 -z18 --read-parallel --buffer=5 --force \
  --drop-densest-as-needed --extend-zooms-if-still-dropping \
  --no-feature-limit --no-tile-size-limit --layer buildings -o out.mbtiles in.ndjson
```

Environment: `TIPPECANOE_MAX_THREADS` (thread count), `VTMAKE_GZIP=fast`
(libdeflate: faster, other gzip bytes, same decoded tiles), `VTMAKE_VERBOSE=1`
(phase times and memory).

See [selective output](docs/selective-output.md) for `--only-tiles`,
`--known-hashes` and `--hashes-out`, their formats and option restrictions.
See [Postgres COPY input](docs/pgcopy.md) for `--pgcopy-schema`: read
`COPY ... TO STDOUT (FORMAT binary)` directly, with no GeoJSON step.

## Speed

Wall time of the same task on public OpenStreetMap data (vtmake: the mean of 2
runs; the `-zg` rows: 1 run). **felt** = felt/tippecanoe `4f26211` (v2.82.0).
"Same tiles" = the SHA3 of all tiles is equal to felt's.

Machine: a Docker container with 2 CPUs, 8 GB for the process and the page
cache, 500 IOPS and 82 MB/s disk (`bench/run.sh`). Full table, with the
all-CPU runs: [bench/results/public-speed.md](bench/results/public-speed.md).

| Task | felt | vtmake | vtmake vs felt | Same tiles |
|---|---:|---:|---:|---|
| Polygon seed (7.1 M OSM buildings) | 840 s | **63.3 s** | 13.3x | yes |
| Point seed (7.1 M points) | 390 s | **14.1 s** | 27.6x | yes |
| Road seed (2.43 M OSM roads) | 354 s | **31.9 s** | 11.1x | yes |
| felt's common `-zg` command, points | 491 s | **51.6 s** | 9.5x | yes |
| felt's common `-zg` command, polygons | 1,034 s | **157 s** | 6.6x | yes |
| felt's common `-zg` command, roads | 368 s | **64.9 s** | 5.7x | yes |

## Compatibility

- felt's own test suite (`bench/felt-tests/`): 200 of 205 targets pass. The other
  5: 4 known differences (polygon ring start order, variable-depth metadata) that
  render identically in MapLibre (0 changed pixels), and felt's C++ unit-test
  binary. felt itself also fails one target here (`-aP` is not deterministic).
- Known differences and their causes: [docs/build-options.md](docs/build-options.md).

## Build

```sh
cargo build --release -p vtmake      # target/release/vtmake
cargo test --workspace
```

vtmake builds on Linux (x86-64 and aarch64). macOS is not supported yet.
It needs a recent stable Rust: 1.91 is too old (`AtomicUsize::try_update`).
Tested with Rust 1.99.1.

Some tests need extra data. Without it, they skip:

- `VTMAKE_CORPUS_DIR`: a folder with `points.ndjson`, `polygons.ndjson` and
  `lines.ndjson` (public data, for example OpenStreetMap extracts).
- `TIPPECANOE_SRC`: the felt/tippecanoe source tree at `4f26211`. With the
  `cpp-ref` or `reference` feature, the tests compile tippecanoe's C++ code and
  compare its results with vtmake's results.

## Layout

- `crates/vtmake` — the CLI and the tiling pipeline
- `crates/vtmake-core` — shared types
- `crates/vtmake-parse` — GeoJSON reader (tippecanoe's semantics)
- `crates/vtmake-geom` — projection, clipping, simplification (exact port)
- `crates/vtmake-wagyu` — polygon cleaning (wagyu through FFI, plus a fast path)
- `crates/vtmake-mvt`, `crates/vtmake-zlib` — MVT encoding and gzip (exact bytes)
- `crates/vtmake-out` — MBTiles, PMTiles, and tile directory output
- `crates/vtmake-filter` — feature filters `-j`/`-J` (port of `evaluator.cpp`)
- `crates/vtmake-tools` — `vtmake join`, `decode`, `enumerate`, `overzoom`, and `json-tool` (ports of felt's tools)
- `crates/vtmake-compare`, `tools/render-diff` — output checks (content and MapLibre images)
- `bench/` — benchmark harness and results; `docs/` — spec, architecture, options

## Licence

vtmake's own code: BSD 2-Clause (see [LICENSE](LICENSE)). Third-party code keeps
its licence (see [NOTICE](NOTICE)).
