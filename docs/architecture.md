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
   The key/value index of a layer has the dedup slots of tippecanoe (one slot
   for each hash, and a collision adds a duplicate). The feature store keeps
   the slot of each key and value, so the tile code does not calculate a hash.
   Each key and value from the store has a source id (the text for a short
   text, else the chunk and the value id). When the ids are equal, the index
   does not compare the text or parse the number.
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

## Large tiles with active limits

Tile size and feature limits need ordered zoom passes. Each zoom repeats until
all tiles use the same drop thresholds. The next zoom starts after this rule
finishes. Child geometry stays in disk queues in `-t`.

For the default geometry options, large tiles clip features in parts of 16,384
features. vtmake joins the results in feature order. Each child refers to the
parent's clipped geometry until the queue writes it. This removes the expanded
geometry copy for each child. One compact clip record holds the candidate and
child flags. The largest child reuses the input feature list. Variable-depth
tiles keep the original clip path.

Queue records have independent blocks of 8,192 features. Large records encode
and decode those blocks in parallel. A batch holds at most 64 encoded blocks.
Geometry arrays reserve their known size before they grow.

Selection, rate drops, tiny-polygon accumulation, and attribute accumulation
stay in feature order. Early simplification and final geometry steps can run
in parallel after selection. These steps need at least 2,048 kept features.
The results return in feature order. vtmake then adds layer keys, values, and
features in that order. Shared-node data stays fixed during each parallel step.

The split clip path, the child references to the parent geometry, and the block
queue format apply at all thread counts when limits are active. Only the
parallel work on the parts and blocks requires more than two threads. Small
tiles use sequential work. The plain path has no active limits and keeps its existing tile tasks.
`VTMAKE_VERBOSE=1` also shows the time and RSS of each zoom pass with limits.
