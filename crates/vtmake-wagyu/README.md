# vtmake-wagyu

Clean tile-unit polygon draws with the Wagyu 0.5.0 headers from tippecanoe.
The vendored headers and their licenses are in `vendor/`.

- `Cleaner::new()` creates state for one thread. Construct a separate cleaner
  within each worker. No state is shared between workers.
- `Cleaner::clean(input, output)` removes unused draws in Rust, runs a positive
  union at scale 16, and retries at scale 1 if an output point is off the integer
  grid. It writes outer rings before holes and keeps explicit closing LINETO
  points. Output `necessary` flags are false.
- `Cleaner::coalesce(&mut geom)` applies tippecanoe's recursive split at 100,000
  draws, then cleans the combined output.
- `clean_polygon(input)` is a convenience function for one call.
- `snap_round_changes(geom)` is the snap-round test of tippecanoe
  `check_polygon` (`-a@`). It uses the vendored `snap_rounding.hpp` and
  mapbox/variant headers.

Input coordinates must be in tile units. The input can contain CLOSEPATH draws;
these do not add a point to a Wagyu ring. Coordinates outside +/-2^38 return
`CleanError`. This bound is much larger than a tile and its buffer. Wagyu errors
also return `CleanError`, with no process exit or shared log file.

The FFI passes flat arrays of op/x/y triples. Input and output buffers are reused.
No ring allocation crosses the FFI boundary. Each union uses a fresh Wagyu engine,
as tippecanoe does. The C++ exceptions stay inside the shim.

The optional `reference` feature compiles tippecanoe's own `clip.cpp`. The linker
keeps the functions that the test shim calls. Set `TIPPECANOE_SRC` to a
felt/tippecanoe source tree (`4f26211`). Without it the reference is not built and
the reference tests do not link.

```sh
source ~/.cargo/env
export TIPPECANOE_SRC=/path/to/tippecanoe
taskset -c 2-31 cargo test --release -p vtmake-wagyu --features reference
VTMAKE_CORPUS=/path/to/public/polygons.ndjson taskset -c 2-31 cargo test \
  --release -p vtmake-wagyu --features reference corpus_polygons \
  -- --ignored --nocapture
taskset -c 2-31 cargo run --release -p vtmake-wagyu --features reference \
  --example wagyu_throughput
```

## Convex ring fast path

`Cleaner::clean` can bypass C++ for one closed convex outer ring with 3 to 16
vertices. A final CLOSEPATH is permitted. All coordinates must be within
±2^20. For each directed edge, every other vertex must be strictly to its left,
and the cross product must exceed `abs(dx) + abs(dy)`. Other inputs use the full
wrapper without a change. In particular, holes, multiple rings, negative rings,
duplicate points, collinear points, and spikes use C++.

The supporting half-plane tests prove convexity, positive winding, and no
intersection. They also put each nonincident vertex outside the square of
radius one about each edge. Wagyu uses hot-pixel squares of radius one half.
Thus no hot pixel from another vertex can enter an edge at scale 16. There
are no intersections to make other hot pixels. `build_edges.hpp` removes no
points because there are no equal or collinear points. The result has only
input vertices, all at multiples of 16, so no scale-1 retry is necessary.

For this class, Vatti has two bounds and one output ring. The last left-bound
point has the smallest y; a horizontal edge at that y ends at the largest x.
`add_point_to_ring` in `ring_util.hpp` keeps this point at `ring->points`.
Topology correction has no points or rings to split or join, and the positive
outer orientation is already correct. `push_ring_to_polygon` in
`build_result.hpp` walks `prev`, which gives the positive input direction,
then appends the first point. The Rust path uses the same start, direction,
and explicit closing point. It resets the `necessary` flag with `Draw::new`,
as the full wrapper does. Coordinate and vertex limits keep all products
within i64 and the C++ double operations far below the integer precision limit.

Set `VTMAKE_WAGYU_MODE=full` to disable the fast path. Set `VTMAKE_WAGYU_MODE=verify` to
run the full wrapper on every cleaning call and compare every fast result as a
complete DrawVec. A mismatch stops the process. The `tippecanoe` binary prints
the call count and hit count at the end. Verification stores no source data.
`Cleaner::clean_full` is also available for differential tests.
