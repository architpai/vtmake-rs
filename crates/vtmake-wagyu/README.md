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
and the cross product must exceed `abs(dx) + abs(dy)`. Other inputs continue to
the simple-ring test below. Holes, multiple rings, duplicate points, collinear
points, and spikes use C++.

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

## Simple ring fast path

After the convex test, `Cleaner::clean` tests one closed ring with 3 to 64
vertices. A final CLOSEPATH is permitted. Coordinates, including CLOSEPATH
coordinates, must be within ±2^20. The test rejects equal consecutive points,
collinear turns, crossings, and repeated vertices. It also rejects more than
two vertices at the smallest y. Other input uses C++.

For each edge, the test first checks the bounding box of each nonincident
vertex, expanded by one unit. Within that box, it rejects a vertex if the
absolute cross product is at most `abs(dx) + abs(dy)`. If an edge meets the
square of radius one about a vertex, both conditions hold. This test can
reject extra rings, but it cannot accept an edge that meets that square.
Nonadjacent edges also get a segment crossing test. Together these tests
exclude crossings, touches, repeated vertices, and hot-pixel changes.

`build_edges.hpp` removes no vertices because no consecutive points are equal
or collinear. The simple ring has no intersections that can make new hot
pixels. No nonincident edge can meet a vertex's hot-pixel square at scale 16;
its radius is only one half in scaled units. The output thus has only input
vertices at multiples of 16. The scale-1 retry cannot occur. The one-unit
margin also excludes hot-pixel changes at scale 1.

A positive simple ring has winding count one inside and zero outside. Positive
union keeps its boundary. The sweep can join temporary bounds at local extrema,
but it cannot split this boundary or create a hole. The last left-bound point
sets `ring->points` at the smallest y. With two such points, the sweep keeps
the larger x. `build_result.hpp` walks `prev` in the positive input direction
and adds the first point as the closing point. The Rust path makes this same
sequence. A negative simple ring has winding count minus one inside. Positive
fill keeps no interior, so both paths return an empty result. The tests check
both directions and all start points for L and U shapes.

The Rust path uses `Draw::new` to clear every `necessary` flag. All differences,
cross products, and the sum of at most 64 area terms fit within i64 at ±2^20.
At scale 16 the C++ products and sums are exact multiples of 256 within the
double precision limit for those multiples. The test uses no floating-point
arithmetic. Each cleaner has its own output buffer.

`VTMAKE_WAGYU_MODE=verify` checks both Rust paths against the full wrapper.
Set `VTMAKE_WAGYU_MODE=stats` to print aggregate counts at the end of a build.
The counts describe misses from the original convex path: single or multiple
rings, negative rings, convex or non-convex rings, and vertex counts in six
bins (at most 3, 4–16, 17–32, 33–64, 65–128, and more than 128). A negative
ring can be a hole or an isolated clockwise ring. These counts store no source
coordinates. Both diagnostic modes add work; do not use them for speed tests.
