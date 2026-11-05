# vtmake-zlib: changes to zlib 1.3.1

`zlib/` contains files from the zlib 1.3.1 release
(`zlib-1.3.1.tar.gz`, SHA-256
`9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23`).
The files `deflate.c`, `deflate.h`, `trees.c` and `zconf.h` are changed. Each
changed file starts with a "CHANGED FILE" note (the zlib license requires that
changed files are marked). All other files are not changed.

## Rule: the output bytes must not change

tippecanoe links classic zlib 1.3.1. The tile bytes of vtmake must be
the same as the tile bytes of tippecanoe. Thus each change must give the same
deflate output as zlib 1.3.1 for all inputs and all settings. Compiler flags do
not change the output. Other zlib versions and forks (zlib-ng, zlib-rs,
Chromium, Cloudflare, libdeflate) give different bytes.

## Changes

1. **Sparse hash clear** (`deflate.c: lm_init`, `fill_window`,
   `deflate.h: bt_hi, bt_dirty`). `deflateReset` cleared all of `head[]`
   (64 K entries, 128 KB) for each stream. Most tiles are approximately 1 KB.
   Now `fill_window` records the end of the window data (`bt_hi`). If the window
   did not move (no slide, no stored block, no dictionary, no
   `deflateParams`), each string that the stream put in `head[]` starts at a
   position `p` with `p + 2 < bt_hi`. Its hash is a function of its first 3
   bytes only (`MIN_MATCH * hash_shift >= hash_bits`). The reset sets these
   entries to `NIL`, and then `head[]` is all `NIL`, as after `CLEAR_HASH`. If
   `bt_hi > hash_size / 8` or `bt_dirty` is set, the reset uses `CLEAR_HASH`.
   Compile with `-DBT_CHECK_SPARSE` to make the reset examine all of `head[]`
   after the sparse clear (`abort()` if an entry is not `NIL`).
2. **Huffman heap** (`trees.c: build_tree`, `bt_pqdownheap`). The heap keeps
   packed entries `((Freq << 8 | depth) << 16) | node` in a local array. One
   integer compare of `(Freq << 8 | depth)` has the same result as the
   `smaller()` macro. The child select has no branch. The heap operations are
   the same as before and in the same order, so the tree is the same.
   `bi_reverse` uses a table (same result).
3. **Match search** (`deflate.c: longest_match`). The same hash chain walk with
   the same stop conditions. The filter tests bytes 0-1 and
   `best_len - 1`..`best_len` two bytes at a time (as the `UNALIGNED_OK` code of
   zlib). The match length is found eight bytes at a time. As in zlib, byte 2
   is not compared, and `len` is the index of the first different byte from
   index 3, or `MAX_MATCH`. The last 8-byte load reads bytes 251..258; these are
   in the window because `strstart <= window_size - MIN_LOOKAHEAD`.
4. **Symbol prefix** (`zconf.h`). With `Z_PREFIX` (set by `build.rs`), all
   symbols have the prefix `vtmake_z_` (zlib uses `z_`). `z_errmsg` also has the
   prefix. This copy cannot clash with the system zlib or with another copy.

## Tests

- `cargo test -p vtmake-zlib`: a golden hash of the gzip output of 90 inputs
  (0 to 140 000 bytes, 4 types of data, one encoder in sequence) from stock
  zlib 1.3.1.
- `fuzz/`: a C harness that compares this zlib with the system zlib 1.3.1
  (random data, all levels, memLevels, windowBits, gzip/zlib/raw, input and
  output in random pieces, `Z_SYNC_FLUSH`, dictionaries, `deflateParams`).
  Run `fuzz/run.sh 30000 1` (number of cases, seed). The system zlib must be
  1.3.1.
- `VTMAKE_MBTILES=ref.mbtiles cargo test --release -p vtmake-mvt --test roundtrip`:
  all tiles of a tippecanoe tileset.

## Speed

Single thread, CPU time, 342 357 tippecanoe tiles (mean 1 KB, from 1 M
polygons and 1 M points), i9-14900HX: system zlib 1.3.1 40.7 MB/s, this copy
51.0 MB/s (1.25 times). Time is now in the hash chain walk (approximately 20
dependent loads for each `longest_match` call), `build_tree` and
`deflate_slow`.
