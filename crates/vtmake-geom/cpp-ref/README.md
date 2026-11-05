# C++ reference for the vtmake-geom differential tests

Test use only. With the cargo feature `cpp-ref`, `build.rs` compiles the
tippecanoe source tree (without change) and `shim.cpp`, and links them into
the test binaries.

- Source tree: `TIPPECANOE_SRC` (felt/tippecanoe `4f26211`; no default). The
  build reads it and does not write to it. Without it the build prints a
  warning, the reference is not built and the test binaries do not link.
- Flags: the Release flags of the tippecanoe Makefile
  (`g++ -O3 -DNDEBUG -std=c++17 -fPIE`).
- All object files of the `tippecanoe` binary are compiled, except `tile.o`
  (`shim.cpp` includes `tile.cpp`, so that its static functions are visible)
  and `main.o` (compiled alone; its `main` symbol is renamed with `objcopy`).
- Needs `g++`, `objcopy`, zlib and SQLite development files.

Run:

```
TIPPECANOE_SRC=/path/to/tippecanoe VTMAKE_CORPUS_DIR=/path/to/corpus \
  taskset -c 2-31 cargo test -p vtmake-geom --features cpp-ref --release -- --nocapture
```

Environment variables:

- `VTMAKE_DIFF_SCALE`: multiplies the number of random cases (default 1).
- `VTMAKE_CORPUS_DIR`: a directory with the public OSM corpus files
  `points.ndjson`, `polygons.ndjson` and `lines.ndjson`, read in place. Without
  it the corpus tests print a message and skip.
- `VTMAKE_CORPUS_LIMIT`: features for each corpus file (default 20000, 0 = all).
- `VTMAKE_EXTRA_CORPUS`: more NDJSON files, separated by commas, read in place
  (for example private data; never copy it into the repository).

tippecanoe is BSD-2-Clause; `shim.cpp` keeps its license text.
