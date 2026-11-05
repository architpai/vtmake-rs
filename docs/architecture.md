# Architecture draft

Status: draft, 2026-11-03. This changes when the tippecanoe spec
(`docs/spec/tippecanoe-plain-path.md`) and the first profiles of tippecanoe are ready.

## Pipeline

```
NDJSON ──mmap──► chunk split ──rayon──► parse + project ──► feature store
                                                              │
                     ┌────────────────────────────────────────┘
                     ▼
        for each z=minzoom tile (parallel, rayon)
          depth-first over its subtree, z = min..max:
            clip to tile + buffer ──► simplify ──► clean polygons
            ──► MVT encode ──► gzip ──► writer channel
                     │
                     ▼
        writer thread: MBTiles (rusqlite, batched) or -e directory
```

## Main ideas

1. **Parse in parallel.** mmap the input. Split it into chunks at line ends.
   Parse each chunk on its own thread with a SIMD JSON parser (`sonic-rs` or
   `simd-json`). Project the coordinates to 32-bit world coordinates at once.
2. **Compact feature store.** Each feature has: geometry (u32 world coordinates,
   delta and varint coded), bounding box, minzoom, maxzoom, and an index into an
   attribute table. Keys and values are interned once for each layer. If the
   store does not fit in the memory budget (for example 8 GB), spill sorted runs to disk
   with LZ4. tippecanoe uses zlib for its temp files; this cost 19 to 40% of CPU
   time in the earlier profiles.
3. **Sort once by tile.** Sort features by the z=minzoom tile that holds them
   (Hilbert or Z order inside the tile). Features that cross tiles go to each
   tile that they touch.
4. **Subtrees in parallel.** Each z=minzoom tile and its subtree is independent.
   Go depth-first: clip the parent tile's geometry for each child, not the
   original features. The memory use is the size of one path from the root to a
   leaf for each thread.
5. **Clip, simplify, clean** with the same rules as tippecanoe (see the spec).
   Polygon cleaning is the hardest part. Options: a Rust crate (`i_overlay`),
   a port of wagyu, or wagyu through FFI (it is header-only C++) for exact
   results first.
6. **Encode MVT by hand.** Write the protobuf bytes directly. Keep the feature
   order and the key/value order of tippecanoe where cheap (second goal).
7. **Compress tiles fast.** gzip with `libdeflate` (much faster than zlib).
   The bytes differ from zlib's bytes, but the decoded tile is the same.
8. **One writer thread.** MBTiles inserts in large transactions, in a fixed
   order, so the output is deterministic for any thread count.

## Determinism

- Output must not depend on the thread count or the memory budget.
- Each tile's content depends only on the input features, sorted by input order.
- The writer sorts tiles by (z, x, y) inside each batch, or the MBTiles file is
  made with a final ordered pass.

## CLI

- Binary name `tippecanoe` (drop-in), also `bt`. Accept the flags of the plain
  command line first. Fail with a clear error for flags that are not supported yet.
