# vtmake-parse

Read NDJSON with the number and attribute rules from tippecanoe. The reader uses
borrowed strings, a byte scanner, and SIMD byte searches from `memchr`. It keeps
object members in source order, including duplicate keys. It does not use a map
for JSON objects.

- `parse_feature(line, &mut ParsedLine)` reads one line. It returns `Outcome` and
  stores zero or more `ParsedFeature` values in the caller's output.
- `ParsedFeature.geom` contains `Position { op, lon, lat }` values. A MOVETO starts
  each ring. CLOSEPATH marks the end of a whole Polygon. These markers let
  `vtmake-geom::fix_polygon` identify the outer ring of each Polygon.
- `ParsedFeature::project(fn)` makes a `vtmake_core::Feature`. The caller must then
  fix polygon winding and set the spatial index. The projection must accept
  nonfinite coordinate values, as required by spec 4.1.
- `read_parallel(path, threads)` maps the file and reads chunks with Rayon.
  `parse_parallel(bytes, threads)` reads an existing buffer. Both return features
  in source order. `seq` is the byte offset of the source line. Collection members
  share that offset and keep their member order. The mapped file must stay fixed.
- `ParseError.exit_code` reports 107 for JSON errors, 119 for invalid UTF-8, and
  110 when the parallel reader finds no valid geometry. A single skipped line is
  not an error.
- `Warning::message()` supplies the warning category text. `Warning::once()`
  identifies warnings that the caller must emit once across calls. The parallel
  reader does this reduction. Source paths, line numbers, and JSON context belong
  to the caller's diagnostic code.
- `dtoa::dtoa_milo(f64)` is a Rust port of Grisu2 and Prettify. Its license and the
  C++ source are in `vendor/`.

The output keeps null attributes and embedded NUL characters. The feature store
must apply the C-string truncation rule before interning strings. The reader does
not project coordinates, fix rings, or compute the feature index by itself.

The JSON reader has a nesting limit of 128. It accepts one JSON value per line.
Bare geometry roots are outside this API's Feature input path. Invalid escaped
surrogates use the JSON string decoder's error path; their diagnostic details
have not been compared with jsonpull. Warning text is structured category text,
not a copy of tippecanoe's complete stderr output.

To build the C++ test reference, enable `reference` and set `TIPPECANOE_SRC` to a
felt/tippecanoe source tree (`4f26211`). Without `TIPPECANOE_SRC` the reference is
not built and the reference tests do not link. The default library build does not
need that source tree.

```sh
source ~/.cargo/env
export TIPPECANOE_SRC=/path/to/tippecanoe
taskset -c 2-31 cargo test --release -p vtmake-parse --features reference
VTMAKE_CORPUS=/path/to/public.ndjson taskset -c 2-31 cargo test --release \
  -p vtmake-parse --features reference corpus_attributes -- --ignored --nocapture
taskset -c 2-31 cargo run --release -p vtmake-parse --features reference \
  --example parse_throughput -- /path/to/public.ndjson
```
