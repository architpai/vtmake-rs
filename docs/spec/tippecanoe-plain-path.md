# tippecanoe plain path: implementation spec

Status: research result, 2026-11-03. Source: the felt/tippecanoe `4f26211` source (v2.82.0).

## 0. Scope and conventions

The spec covers one command line only, the plain command line:

```
tippecanoe -Z <min> -z <max> --read-parallel --buffer=5 --force \
  --drop-densest-as-needed --extend-zooms-if-still-dropping \
  --no-feature-limit --no-tile-size-limit --layer <id> \
  (-e <dir> | -o <file.mbtiles>) <input.ndjson>
```

Each claim has a tag:

- **VERIFIED**: an experiment with the felt binary confirms it (see section 14).
- **READ**: the source code shows it. No experiment.
- **UNSURE**: not fully confirmed.

Terms:

- **World coordinates**: integers on a 2^32 x 2^32 plane. x = 0 is 180°W. y = 0 is the north
  edge of the Mercator plane. y increases to the south.
- **Tile-relative world coordinates**: world coordinates minus the tile origin
  `(tx << (32-z), ty << (32-z))`. Range of the tile proper: `[0, 2^(32-z)]`.
- **Tile units**: output MVT coordinates, extent 4096. One tile unit is
  `res = 2^(32-12-z)` world units.
- `round()` means C `std::round` (half away from zero). Rust `f64::round` is the same.
- All `long long` arithmetic is signed 64-bit. Integer `/` truncates toward zero.

Code references use `file:function`.

## 1. Effective settings for this command line

| Setting | Value | Source |
|---|---|---|
| full_detail (`-d`), low_detail (`-D`) | 12, 12 (extent 4096 at every zoom) | `main.cpp:main` (defaults; `full_detail <= 0` → 12) |
| min_detail | 7 (never used: no retry loop runs) | `main.cpp` globals |
| simplification (`-S`) | 1.0 | `main.cpp` globals |
| maxzoom_simplification | -1 (not set, so maxzoom uses 1.0) | `tile.cpp:traverse_zooms` |
| tiny_polygon_size | 2 | `main.cpp` globals |
| buffer | 5 (in 1/256 of a tile width) | `main.cpp:main` |
| droprate (`-r`) | 2.5 | `main.cpp:main` |
| basezoom (`-B`) | = maxzoom (`-z`) | `main.cpp:main` |
| gamma | 0 | `main.cpp:main` |
| geometry_scale | **0** (forced by `--extend-zooms-if-still-dropping`) | `main.cpp:main` |
| max_tile_size, max_tile_features | 500000, 200000, but both limits are off | `prevent[P_KILOBYTE_LIMIT]`, `prevent[P_FEATURE_LIMIT]` |
| index function | quadkey (not Hilbert) | `main.cpp:main` |
| CPUS | `TIPPECANOE_MAX_THREADS` or available CPUs, rounded down to a power of 2 | `main.cpp:init_cpus` |

### 1.1 Effect of each flag on the output

| Flag | Effect on tile bytes | Tag |
|---|---|---|
| `-Z`, `-z` | Zoom range of written tiles. | READ |
| `--read-parallel` | No effect (output identical with and without it). | VERIFIED |
| `--buffer=5` | Clip box = tile ± 5/256 tile = ±80 tile units. | VERIFIED |
| `--force` | Delete old output first (section 9.4). | READ |
| `--drop-densest-as-needed` | **No drops occur, but it changes the feature order.** It sets a spatial index for every feature, and polygons use the center of mass as the index point. Without the flag, lines and polygons use a hashed vertex. | VERIFIED |
| `--extend-zooms-if-still-dropping` | **No extra zooms occur, but it sets geometry_scale = 0** (full 32-bit coordinates). Without the flag, geometry_scale = 32 − (12 + maxzoom) and many tiles change. It also makes tippecanoe write unused z = maxzoom+1 temp data. | VERIFIED |
| `--no-feature-limit`, `--no-tile-size-limit` | No tile is ever retried, dropped or reduced in detail. | READ |
| `--layer <id>` | Default layer name. A per-feature `tippecanoe.layer` overrides it. | VERIFIED |

## 2. Pipeline overview

1. Read and parse each NDJSON line (section 3).
2. Project coordinates to world coordinates (section 4).
3. Fix polygon rings (section 7.1). Compute the index point and the bbox.
4. Sort all features by `(index, seq)` (section 8.1).
5. For each zoom `z` from `iz` to `maxzoom`, for each tile (section 5):
   1. Read the features of the tile in sorted order.
   2. Clip each feature to the tile plus buffer (section 6).
   3. Write the clipped geometry to the child tiles (next zoom).
   4. Skip the feature if the zoom rules exclude it (section 5).
   5. Polygons: tiny polygon reduction (section 7.3).
   6. Lines and polygons: drop if all vertices fall in one tile unit.
   7. Simplify (section 7.4). Scale to tile units. Clean polygons (section 7.5).
   8. Encode MVT, gzip, write (sections 8 and 9).

## 3. Parsing

### 3.1 Input loop (`geojson-loop.cpp:parse_json`)

- The parser reads JSON values one by one. An object with `"type":"Feature"` is a feature. READ
- With `--read-parallel` and a regular file, tippecanoe maps the file and splits it into CPUS
  segments. Each split point moves forward to the next `\n`. Each segment has its own parser.
  READ (`main.cpp:do_read_parallel`)
- `"type":"FeatureCollection"`: only its `crs` is checked (warning only). READ
- Members read from a Feature: `geometry`, `properties`, `id`, `tippecanoe`. All other members
  are ignored. READ
- `properties` missing, or not an object and not `null`: the feature is **skipped** with
  the warning "feature without properties hash". VERIFIED
- `properties: null`: the feature is kept with no attributes. VERIFIED
- `geometry` missing: skipped with a warning. `geometry.type` missing: skipped (warning once).
  READ (`geojson.cpp:serialize_geojson_feature`)
- `GeometryCollection` (`geometry.geometries` is an array): each member geometry becomes a
  separate feature with the same properties, id and `tippecanoe` object. READ
- Geometry types: Point, MultiPoint, LineString, MultiLineString, Polygon, MultiPolygon.
  Point/MultiPoint → VT_POINT (1), (Multi)LineString → VT_LINE (2), (Multi)Polygon →
  VT_POLYGON (3). Other types: skipped with a warning. READ (`read_json.cpp`)
- A position with fewer than 2 numbers: **exit** (`EXIT_JSON` = 107). Extra dimensions are
  ignored (warning once). READ (`read_json.cpp:parse_coordinates`)
- An input with no valid feature: exit 110 ("Did not read any valid geometries"). READ

### 3.2 The `tippecanoe` object (`geojson.cpp:serialize_geojson_feature`)

- `minzoom`, `maxzoom`: used only if they are JSON numbers. Other types are ignored. VERIFIED
  (`"maxzoom":"x"` is ignored).
- The number is formatted with `dtoa_milo`, then parsed with `atof`. It must be finite,
  integral and in `[0, 32]`. Otherwise tippecanoe **exits** with code 107. VERIFIED (17.6 → exit).
- `layer`: used only if it is a JSON string. It replaces the `--layer` name for this feature.
  VERIFIED.
- Absent values are stored as -1 ("not set").

### 3.3 Feature id (`geojson.cpp:serialize_geojson_feature`)

- `id` is a JSON number ≥ 0 and integral: `has_id = true`, `id = uint64`. Integers > 2^53 keep
  their exact value (the parser keeps a 64-bit copy). VERIFIED (42 → id 42).
- Negative, fractional, too large, or string id: no id (one warning per kind). VERIFIED
  (−7, 1.5, "abc").
- No id generation (`-ai` is off).

### 3.4 Attribute values (`read_json.cpp:stringify_value`)

Each property becomes `(type, string)`. The order is the input order. Keys that appear twice
are both kept (section 8.4). VERIFIED.

| JSON value | Internal type | String |
|---|---|---|
| string | string | the decoded string (UTF-8 is checked; invalid UTF-8 → exit 119) |
| number, integral, \|v\| > 2^53, no `.`/`e` | number | exact decimal (`%llu` / `%lld`) |
| other number | number | `dtoa_milo(v)` (section 3.5) |
| `true` / `false` | bool | `"true"` / `"false"` |
| `null` | null | removed before the tile is written (not in the tile) |
| object or array | **string** | compact JSON (section 3.6) |

At tile time the string becomes an MVT value (`mvt.cpp:stringified_to_mvt_value`):

| Condition on the string `s` (number type) | MVT value |
|---|---|
| `strtoull(s, 0)` parses fully, no `-` sign (a `.000…` tail is allowed), value ≤ 2^63−1 | `int_value` (field 4) |
| same, value > 2^63−1 | `uint_value` (field 5) |
| `strtoll(s, 0)` parses fully (negative integers) | `sint_value` (field 6, zigzag) |
| `(double)strtof(s) == atof(s)` | `float_value` (field 2) |
| otherwise | `double_value` (field 3) |
| bool | `bool_value` (field 7) |
| string | `string_value` (field 1) |

VERIFIED: `5`→int, `-3`→sint, `0.5`→float, `139.68762665`→double, `1e21`→double, `1e-7`→double,
`3.0`→int 3, `-0.0`→int 0, `12345678901234567890`→uint, `true`→bool.

Notes:

- `strtoull`/`strtoll` use base 0 (C rules). `dtoa_milo` never makes hex or leading zeros, so
  this has no effect for JSON numbers. READ
- A float is used only if it equals the double exactly. So the value in the tile equals the
  parsed double. Only the wire type differs. READ
- Strings go into a C-string pool. **A string or key with an embedded NUL is cut at the NUL.**
  READ (`serial.cpp:serialize_feature`, `addpool`)

### 3.5 Number formatting (`milo/dtoa_milo.h:dtoa_milo`)

- Grisu2 digits, then `Prettify` with `kk = length + K`:
  - `length ≤ kk ≤ 21`: integer digits plus trailing zeros (`1e20` → `100000000000000000000`).
  - `0 < kk ≤ 21`: decimal point inside (`12.34`).
  - `-6 < kk ≤ 0`: `0.000ddd`.
  - one digit, else: `de±X` (`1e+21`, `1e-7`). Many digits: `d.ddde±X`. The exponent has no
    leading zeros and always has a sign.
- `0` and `-0` → `"0"`. NaN → `"nan"`, ±inf → `"inf"`/`"-inf"`.
- Grisu2 is not always the shortest round-trip string, but it always round-trips. READ
- Impact: the output string matters only for nested JSON strings (3.6) and for tilestats
  in the metadata. For plain numbers, only the integer/non-integer decision matters, and that
  depends on the `Prettify` rules (no exponent below 1e21). VERIFIED (`1e-7` in tilestats).

### 3.6 Nested objects and arrays (`jsonpull/jsonpull.cpp:json_stringify`)

- No spaces. Object keys keep the source order. Separators `,` and `:`.
- Strings: `\` → `\\`, `"` → `\"`, bytes 0x00–0x1F → `\u00xx` (lowercase hex). All other
  bytes (also non-ASCII, `/`) are written raw.
- Numbers: like 3.4/3.5. `true`, `false`, `null` as literals.
- VERIFIED: `{"a":1,"b":[1,2,"x"]}` → `{"a":1,"b":[1,2,"x"]}`; `[1.5,"q\"z",null]` → same text.

## 4. Projection and bounding box

### 4.1 Projection (`projection.cpp:lonlat2tile`, zoom 32)

```
if lat is inf/NaN: lat = 89.9
if lon is inf/NaN: lon = 720 (no clamp)
lat = clamp(lat, -89.9, 89.9); lon = clamp(lon, -360, 360)
lat_rad = lat * M_PI / 180
n = 2^32
x = (long long) round(n * ((lon + 180) / 360))
y = (long long) round(n * (1 - (log(tan(lat_rad) + 1 / cos(lat_rad)) / M_PI)) / 2)
```

- **Note the y formula:** the division by 2 is **inside** `round()`. (An earlier version of
  this spec put it outside; that formula gives a different y for about 25% of latitudes.)
  VERIFIED (differential test against the C++ function, `vtmake-geom` `spec_differences`).
- Use the same libm functions (`log`, `tan`, `cos`). Rust `f64::ln/tan/cos` on
  x86_64-linux-gnu call glibc libm. VERIFIED on this machine: bit-identical for 12 M random
  arguments and for all 7,095,930 public points (plus about 100 M polygon vertices).
  UNSURE across machines: glibc selects FMA or non-FMA variants at run time, but tippecanoe
  and the Rust code always call the same glibc on the same machine.
- geometry_scale = 0, so `SHIFT_RIGHT`/`SHIFT_LEFT` do nothing. Full 32-bit precision is kept.
  READ (`serial.cpp`, `tile.cpp`)

### 4.2 Feature bbox (`serial.cpp:scale_geometry`)

- `bbox = [min x, min y, max x, max y]` of all MOVETO/LINETO points, in world coordinates.
- This bbox goes into the file bbox (section 9.3). READ

## 5. Which zooms a feature appears in

### 5.1 Rules (`tile.cpp:next_feature`)

A feature is in the tile at zoom z only if all are true:

1. `-Z ≤ z ≤ -z`.
2. `tippecanoe.minzoom` not set, or `z ≥ minzoom`.
3. `tippecanoe.maxzoom` not set, or `z ≤ maxzoom`.
4. The clipped geometry is not empty (section 6), and section 7 does not remove it.
5. Only for points **without** `tippecanoe.minzoom`: the drop-rate rule (5.2).

- Features with `minzoom < -Z` appear from `-Z`. No tile is written below `-Z`. VERIFIED (no
  z12/z13 directories when all features have minzoom 14 and `-Z12`).
- Features with `minzoom > -z` never appear. READ
- Features are passed to child tiles only while `tippecanoe.maxzoom` is not set or
  `maxzoom ≥ nextzoom`. READ (`tile.cpp:next_feature` → `rewrite`)

### 5.2 Default point dropping (rate 2.5, basezoom = maxzoom)

This is not drop-densest. It is the default `-r2.5` dot dropping.

- If a feature has `tippecanoe.minzoom` (any value), it is always "kept". The drop rule does
  not apply. VERIFIED (2000 points with minzoom 9: all points at z9).
- If a **point** feature has no `tippecanoe.minzoom` (also when only `maxzoom` is set), the
  drop rule applies. VERIFIED: 2000 points without minzoom gave 16 / 186 / 1182 / 1918 / 2174
  features at z9 / 12 / 15 / 17 / 18 (counts include buffer copies).
- Lines and polygons are never dropped by rate (`-al`, `-ap` are off).
- Algorithm (`main.cpp:prep_drop_states`, `calc_feature_minzoom`), run over **all** features
  in global sort order (section 8.1):
  - `interval[i] = exp(log(2.5) * (maxzoom - i))` for `i < maxzoom`, else 0. `seq[i] = 0`.
  - For each VT_POINT feature (also points with a minzoom): add 1 to every `seq[i]`. Then for
    `i = maxzoom` down to 0: if `seq[i] < 0`: `feature_minzoom = i + 1`, stop. Else
    `seq[i] -= interval[i]`.
  - Non-point features do not change the state.
- In a tile, a point with `z < feature_minzoom` is dropped only if an earlier kept feature of
  the same type and layer is in the tile (`find_feature_to_accumulate_onto` on the layer's
  feature list). A feature goes into that list only after tiny-polygon reduction, the
  too-small test and a non-empty geometry. A point with an empty clip (a MultiPoint with no
  member in the tile, a date-line copy at z0) is not in the list. VERIFIED (`multipoint-rate`
  and `polar` cases of `bench/meta-check.sh`). Each drop adds 1 to
  `strategies[z].dropped_by_rate`. The first feature **without `tippecanoe.minzoom`**
  (any type) in each tile and layer is always kept. It counts after the clip and the zoom
  checks, also when its clipped geometry is empty and it is removed later. READ
  (`tile.cpp:next_feature` `multiplier_state`, `find_feature_to_accumulate_onto`)
- The test is `z >= feature_minzoom - bit_reverse(index >> 2) / 2^64` in `double`. This is
  `z >= feature_minzoom`, except when the fraction rounds to 1.0 (bits 2..55 of the index all
  set). READ (`vtmake-geom` `rate_keeps`)
- Inputs that set `tippecanoe.minzoom` on every feature are not affected by this rule.

### 5.3 Drop-densest and extend-zooms never change the tile set

- `mingap` starts at 0, and the test is `gap < mingap`. It is never true. READ
  (`tile.cpp:write_tile`)
- `mingap` increases only when a tile is too big. Both limits are off, so this never happens.
  So `still_dropping` stays false and `maxzoom` never grows. READ (`tile.cpp:traverse_zooms`)
- But both flags change bytes in other ways (section 1.1). VERIFIED

### 5.4 Zooms below `-Z` (`main.cpp:choose_first_zoom`, `tile.cpp:write_tile`)

- `iz` = the largest z ≤ `-Z` where the file bbox ± buffer
  (`buffer * 2^(32-z) / 256`) fits in one tile. Tiling starts at `iz`.
- For `z < -Z`: features are clipped and passed down, but no tile is written.
- From z to the next zoom, tippecanoe can skip zooms below `-Z`:
  `nextzoom = min(z + max_zoom_increment, -Z)` if `z + 1 < -Z`, where
  `max_zoom_increment = floor(log4(child_shards))`. child_shards depends on CPUS and the
  number of non-empty temp files.
- From `-Z` up, every zoom is processed.
- The skip pattern depends on CPUS. `max_zoom_increment = floor(log4(TEMP_FILES / threads))`,
  `TEMP_FILES = 4 * CPUS`. With CPUS 1 or 2 (for example a 2-CPU machine) the increment is
  always 1: every zoom from `iz` to `-Z` is clipped. With more CPUS some zooms are skipped.
- The clip chain below `-Z` changes bytes. VERIFIED: long lines and large polygons
  (`cascade` case, 198721 tiles): a clip from the original geometry at `-Z` gave 4365 other
  tiles than felt with 1 or 2 threads; felt with 16 threads gave 488 other tiles than felt with
  1 thread. The cause: a clip point at the edge of a parent buffer is rounded, and the child
  clip starts from it. Building footprints and points are small, so these inputs did not
  change (100k polygons, 62651 tiles, the same with 1, 2 or 32 threads).
- vtmake clips at every zoom from `iz` to `-Z` (the CPUS 1 or 2 chain), only for
  the features that cross an edge of the parent tile of their `-Z` tile. The output does not
  depend on the CPU count. It is the same as felt with 1 or 2 threads; with more threads it
  can be different in rare tiles, as felt is different from itself.
- If `iz` = 0, the z0 date-line copies (6.8) come to the `-Z` tiles on the other side.
  VERIFIED (`antimeridian-z2`, `dateline-z3`).

### 5.5 Metadata maxzoom

- `maxzoom` in the metadata = the highest zoom where tippecanoe tiled at least one tile
  (non-empty temp data), not always `-z`. VERIFIED: `-z18` with all features
  `"maxzoom":16` → metadata `maxzoom` 16, exit code 0, no z17/z18 tiles.
- This includes the zooms below `-Z` (`wrote_zoom`). If all features have a maxzoom below
  `-Z`, no tile is written and the metadata maxzoom is a zoom below `-Z` that depends on the
  skip pattern (CPUS). vtmake uses the highest feature maxzoom (at least `iz`).
  VERIFIED for felt with 2 threads (`maxz-below`).
- When the metadata maxzoom is not `-z`, `tippecanoe_decisions` is written (9.3).

## 6. Tile assignment and clipping (buffer 5)

### 6.1 Buffer

- Buffer in world units at zoom z: `b = 5 * 2^(32-z) / 256` (exact integer).
- In tile units: 80. Output coordinates are in `[-80, 4176]`. VERIFIED (lines and points:
  exactly −80…4176). Polygons can reach 4177 (UNSURE: probably tiny-polygon squares, 7.3).

### 6.2 Clip cascade

- At each zoom, the feature geometry is the parent tile's **clipped** geometry. tippecanoe does
  not clip the original geometry again. READ (`tile.cpp:next_feature` → `rewrite`)
- Coordinates stay in world units (tile-relative) through all zooms. Scaling to tile units
  happens only at output.
- For byte identity, use the same cascade: clip at each zoom from the parent result.

### 6.3 Child assignment (`tile.cpp:rewrite`)

- The candidate children come from the feature bbox **before** clipping at the parent, in
  child pixels (256 per child): `bbox / 2^(32-nextzoom-8)` (division truncates toward zero),
  then `left -= 6`, `top -= 6`, `right += 5`, `bottom += 5`, clamp to the span, `/ 256`.
- This is only a candidate set. The exact clip at the child decides. A Rust version can use any
  superset. It must include a point that lies exactly on the buffer edge (clip is inclusive).
- Each child keeps the parent order of features. READ

### 6.4 Quick check (`geometry.cpp:quick_check`)

- `area = 2^(32-z)`. bbox strictly inside `(0, area)` on all sides → 1: **no clip at all**.
- bbox strictly inside `(-b, area+b)` → 3; partial overlap → 2. Both run the clip.
- `bbox[0] > area+b`, `bbox[1] > area+b`, `bbox[2] < -b` or `bbox[3] < -b` → 0: feature
  not in tile. READ
- Note: bbox here is the bbox of the incoming (parent-clipped) geometry.

### 6.5 Points (`clip.cpp:clip_point`)

- Keep a point if `-b ≤ x ≤ area+b` and `-b ≤ y ≤ area+b` (inclusive). Each MultiPoint
  member is tested alone. Points in the buffer are copied into every tile that covers them.

### 6.6 Lines (`clip.cpp:clip_lines`, `clip.cpp:clip`)

- Cohen–Sutherland per segment, box `[-b, area+b]`, inclusive. **Integer arithmetic**:
  `x = x0 + (x1 - x0) * (ymax - y0) / (y1 - y0)` (64-bit, truncating). Test order of the out
  code: TOP (y > ymax), BOTTOM (y < ymin), RIGHT, LEFT. The outside point to move is
  `outcode0` if non-zero, else `outcode1`.
- Output per segment i (from point i−1 to point i):
  - accepted, changed: `MOVETO p0'`, `LINETO p1'`, `MOVETO p_i`.
  - accepted, unchanged: push `p_i` with its own op.
  - rejected: `MOVETO p_i`.
- Then `remove_noop(type=line, shift=0)` (section 7.6). A line that leaves and enters again
  becomes a multi-line. VERIFIED (1885 of 23476 z15 line features were multi-part).

### 6.7 Polygons (`clip.cpp:simple_clip_poly`, `clip_poly1`)

- Sutherland–Hodgman per ring, in `double`. Edges in this order: 0 top (`y > -b` is
  inside), 1 right (`x < area+b`), 2 bottom (`y < area+b`), 3 left (`x > -b`). The tests are
  **strict**: a point exactly on the buffer edge is "outside".
- Input ring includes the closing point. Start with `S = last point`.
  - S out, E in: push `intersect(S,E)`, push E. S in, E out: push `intersect(S,E)`.
    Both in: push E. Both out: nothing.
- `intersect` for the top edge: `(a.x + (double)(b.x - a.x) * (miny - a.y) / (b.y - a.y), miny)`;
  other edges are the same pattern.
- After the 4 edges: if first ≠ last, append first. If size < 3, the ring is empty.
- Each point becomes `(round(x), round(y))`.
- The inner "tile edge" branches only add points when `prevent[P_SIMPLIFY_SHARED_NODES]`
  is on. It is off. READ
- After the clip: `remove_noop(type=polygon, 0)`.
- No polygon cleaning happens at clip time. A ring can become degenerate. Cleaning comes at
  output (7.5).

### 6.8 z = 0 and the date line

- At z = 0, a feature near x = 0 or x = 2^32 is copied ±2^32. READ
  (`tile.cpp:clip_to_tile`)

## 7. Geometry processing at each tile

Order in `tile.cpp:write_tile` for each feature (after 5.1 passes):

1. Remove null attributes.
2. Polygon: `reduce_tiny_poly` (7.3). Empty → drop.
3. Line or polygon: `line_is_too_small` (7.2) → drop.
4. Append to the layer list.

Then per layer, for each feature (`tile.cpp:simplification_worker`):

5. `simplify_feature` (7.4), unless the polygon was fully "reduced".
6. `to_tile_scale` (7.7).
7. Polygon: `coalesce_polygon(geom, scale_up=true)` (7.5). If the result has < 3 points:
   `revive_polygon` if the world area > 0, else drop.
8. Polygon: `close_poly` (7.6). Drop features with empty geometry.
9. At encoding: line/polygon `remove_noop(shift 0)`. Empty → drop.

### 7.1 Ring fixes at input (`clip.cpp:fix_polygon`)

Runs once at read time on world coordinates.

- Each GeoJSON Polygon: first ring is outer, the next rings are holes. A Polygon ends at a
  CLOSEPATH marker from the parser.
- A ring is closed if needed (append the first point). Pad to ≥ 4 points with copies of the
  first point.
- Winding: the source winding is ignored. With `get_area` (shoelace, y down), an outer ring
  must have area > 0 and a hole area ≤ 0. Else the ring is reversed.
- Rotation: centroid = integer mean of the points (without the closing point; truncating
  division). `furthest` = point with the largest `((x-cx)/16)^2 + ((y-cy)/16)^2` (integer
  division by 16; tie → smaller point by `(y, x)`). `furthestb` = point furthest from
  `furthest`, same rule. The ring is rotated to start at `furthestb`.
- `get_area`: shoelace relative to the first point, in `double`. If an intermediate sum
  reaches 2^53, the sign comes from a scaled-down retry. READ (`clip.cpp:get_area`)

### 7.2 Too small lines and polygons (`clip.cpp:line_is_too_small`)

- Round each vertex to tile units: `llround(x / 2^(32-12-z))`. If all vertices of the
  feature round to the MOVETO point of their part, drop the feature. Empty geometry → drop.

### 7.3 Tiny polygon reduction (`clip.cpp:reduce_tiny_poly`)

Runs at every zoom, also at maxzoom.

- `pixel = 2^(32-12-z) * 2` world units (2 tile units). Threshold area `pixel²` = 4 tile
  units².
- `accum_area` is **one accumulator per tile**, shared by all polygon features in the tile,
  in feature order.
- Per ring (area from `get_area`, world units², relative coordinates):
  - `area == 0`: drop the ring. No other effect.
  - `(area > 0 and area ≤ pixel²)` or `(area < 0 and the last outer ring was not kept)`:
    `accum_area += area`, the feature is "reduced". If `area > 0` and
    `accum_area > pixel²`: emit a square (MOVETO, 4 LINETO) with corner
    `(x0 - pixel/2, y0 - pixel/2)` and side `pixel`, where `(x0, y0)` is the first point of
    this ring. Then `accum_area -= pixel²`. If `area > 0`: last outer = not kept.
  - Else: copy the ring. The feature still needs simplification. If `area > 0`: last outer =
    kept.
- If no ring was copied, the feature is marked `reduced` and is not simplified (7.4).
- Each feature with at least one reduced ring adds 1 to `strategies[z].tiny_polygons` in
  the metadata. VERIFIED (counts appear in `strategies`).

### 7.4 Simplification (`tile.cpp:simplify_feature`, `geometry.cpp:simplify_lines`, `clip.cpp:douglas_peucker`)

Applies to lines and polygons at **every zoom including maxzoom** (`-pS` is off). VERIFIED (some
z18 lines have fewer vertices than the input).

1. Lines only: `remove_noop(geom, line, shift = 32 - z - 12)`. This drops LINETOs that round
   to the same tile unit as the last kept point. Coordinates stay in world units.
2. Mark necessary: MOVETO and the last point of each part.
3. `impose_tile_boundaries(geom, area = 2^(32-z))`: for each segment that crosses the tile
   proper `[0, area]`, run the integer `clip()` of 6.6. Insert the clipped end points (if they
   differ from the originals) as LINETO with `necessary = 1`, before the segment end point.
   This adds vertices on the tile edges. VERIFIED (7514 vertices exactly on x or y = 0 or 4096
   in z15 lines).
4. Douglas–Peucker per part (`j - i > 1`), tolerance `e = 2^(32-12-z) * 1.0` world units
   (= 1 tile unit), `kept = 2`, `retain = 4` for polygons, `0` for lines.
   - Start: push a segment for each pair of consecutive necessary points.
   - Pop `(first, second)` from a stack (LIFO).
   - Scan direction: if point[first] < point[second] (compare `y`, then `x`), scan
     `first+1 … second-1` and measure from line (first, second). Else scan
     `second-1 … first+1` and measure from line (second, first).
   - Candidate if `distance > e || kept < retain`. Take the largest distance; tie → the
     smaller point by `(y, x)`.
   - If a candidate exists: mark it, `kept++`. Push sub-segments. Order: if
     point[first] < point[second], push (first, k) then (k, second); else push (k, second)
     then (first, k). Only push a sub-segment if it has interior points.
   - `distance_from_line` (integer-safe): `p2 = B - A`;
     `s = ((p2x/4)*(p2x/8) + (p2y/4)*(p2y/8)) * 32.0`;
     `u = s == 0 ? 0 : ((Px-Ax)/4*(p2x/8) + (Py-Ay)/4*(p2y/8)) * 32.0 / s` (integer `/`
     inside the products), clamp `u` to `[0, 1]`,
     `d = round(sqrt(dx² + dy²) * 16) / 16` with `dx = Ax + u*p2x - Px`.
5. Keep only necessary points.
6. Polygons: the C++ code has the test "if the result has < 3 points, keep the unsimplified
   geometry", but it has **no effect**: `simplify_lines` changes its argument in place, so
   the simplified result is always kept (cleaning then revives it, 7.5). VERIFIED
   (differential test of `tile.cpp:simplify_feature`).
7. Note: `distance_from_line` divides by 4 and 8 before the products. For short segments
   (high zooms, where 1 world unit is near 1 tile unit) the distance is not exact, so some
   collinear points are kept. READ, VERIFIED.

Not used: Visvalingam (`-av` off), shared-node logic (`-pn` off), line reversal.

### 7.5 Polygon cleaning (`clip.cpp:coalesce_polygon`, `clean_or_clip_poly`, wagyu 0.5.0)

Runs on tile units (after 7.7), once per polygon feature per tile.

- If the geometry has ≥ 100000 draw entries, it is split at the first outer ring after the
  midpoint. Each half is cleaned, and then the whole is cleaned again (recursive). READ
- `clean_or_clip_poly(geom, z=0, buffer=0, clip=false, try_scaling=true)`:
  1. `remove_noop(polygon, 0)`.
  2. `scale = 16`. Add each ring with ≥ 4 draws (`j >= i + 4`) to wagyu as `(x*16, y*16)`.
  3. `execute(union, fill_type_positive, fill_type_positive)`.
  4. If any output coordinate is not a multiple of 16: set `scale = 1` and run again from 2.
  5. Divide by scale (`round`). Each output polygon: outer ring first (area > 0), then holes.
     Wagyu output closes each ring (last = first).
- Effect: self-intersections are removed, overlapping parts of a MultiPolygon are merged,
  holes outside their outer are removed, rings are re-wound and re-started at a point chosen
  by wagyu.
- A wagyu exception: exit with "Polygon cleaning failed". READ
- The output ring order and start points come from wagyu internals (`build_result.hpp`).
  To get the same bytes, port wagyu 0.5.0 exactly (about 6150 lines of header code in
  `mapbox/geometry/wagyu/`).

Revive (`tile.cpp:revive_polygon`): if cleaning leaves < 3 points and the world area > 0:
`area_t = area / (2^(32-12-z))²`; `h = ceil(sqrt(area_t))`; `w = round(area_t / h)`, min 1;
center = integer mean of all points of the pre-clean geometry (truncating); square from
`(cx - w/2, cy - h/2)`, side w × h (integer `/`). READ

Early clean (`write_tile`): the kept geometry of the tile (after tiny-polygon reduction and
the too-small test) is added up, `unsimplified_geometry_size += draws * 16`. Each time it is
more than 10 MB (> 655360 draws), `simplify_feature` runs on all features since the last time,
and the polygons go through `coalesce_polygon(geom, false)` (world coordinates, scale 1, no
retry at 16). Then the sum starts again at 0. Later the normal steps run on these features
again (a second simplification, scale, clean). VERIFIED (`early-clean` case: a z10 tile with
1.29 M draws; without the early clean, 1 of 39 tiles had other bytes). It can happen only for
very dense low-zoom tiles.

### 7.6 Small helpers

- `remove_noop(geom, type, shift)` (`clip.cpp`): (1) drop a LINETO whose rounded coordinates
  (`round(x / 2^shift)`) equal the last kept MOVETO/LINETO; (2) for lines and polygons, drop
  a MOVETO that is last, or followed by MOVETO, or followed by CLOSEPATH (both removed);
  (3) lines only: drop a MOVETO (index > 1) at the same rounded point as the previous LINETO.
- `close_poly` (`clip.cpp`): each ring → MOVETO, LINETOs without the closing point,
  CLOSEPATH.

### 7.7 Scale to tile units (`clip.cpp:to_tile_scale`)

- `x_tile = round((double) x / 2^(32-12-z))`, same for y. x is tile-relative world units
  (can be negative in the buffer). At z = 20 the divisor is 1.

## 8. Feature order, layers, attributes, MVT encoding

### 8.1 Feature order

- Global sort key: `(index, seq)` ascending. READ (`main.cpp:indexcmp`)
- Every tile keeps this order. The order is **not** the input order. VERIFIED.
- `index` = `encode_quadkey(midx, midy)`: interleave the bits of x and y, from the top bit;
  x bit is the high bit of each pair (`(x_bit << 1) | y_bit`). READ (`projection.cpp`)
- Index point (`serial.cpp:serialize_feature`, world coordinates after `fix_polygon`):
  - Point/MultiPoint: `midx = (bbox[0]/2 + bbox[2]/2) & 0xFFFFFFFF` (each half divided
    first, so one bit is lost), same for y.
  - Polygon (because of `--drop-densest-as-needed`): `center_of_mass_mp` (8.2),
    `& 0xFFFFFFFF`.
  - Line: `ix = Σ (x + y)` over all draws (unsigned 64-bit, wrapping), `ix %= draw count`.
    Use vertex `ix`.
- `seq` = input order. With `--read-parallel`, segment k starts at its byte offset. So the
  order of `seq` is the input order for any CPU count. READ, VERIFIED (same output for
  1/2/32 threads).
- The sort is stable for equal index: input order. VERIFIED.

### 8.2 Polygon center of mass (`geometry.cpp:center_of_mass_mp`, `centerOfMass`)

Per ring (MOVETO … LINETO, closed):

- `centroid = (llround(Σx / n), llround(Σy / n))` in `double`.
- `centerOfMass(ring, centroid)`: translate by the centroid (integer), then
  `a = xi*yj − xj*yi`, `sA += a`, `sx += (xi+xj)*a`, `sy += (yi+yj)*a` over consecutive
  pairs (double). If `sA == 0` return the centroid. Else, with `f = 1 / (6 * (sA * 0.5))`:
  `(cx + f * sx, cy + f * sy)`, truncated to integer (the multiplication by `f` rounds
  differently from a division by `6 * sA * 0.5`).
- `ringx += center.x * area`, `ringy += center.y * area` (area from `get_area`).
  `ringcount += area` where `ringcount` is `size_t` (conversion through `double` at each add).
- Result `(ringx / ringcount, ringy / ringcount)`, truncated. A zero total area gives a
  division by zero: on x86-64 the conversion of NaN/inf gives `LLONG_MIN`, and the 40-bit
  `draw` field keeps 0, so the center is (0, 0). A negative sum wraps the `size_t`. VERIFIED
  (differential tests with zero-area and hole-larger-than-outer polygons).

### 8.3 Layers

- Layer name = `tippecanoe.layer` if it is a string, else `--layer`. VERIFIED.
- In a tile, layers are sorted by name (C++ `std::map<std::string>`, byte order). Empty layers
  are not written. A tile with no layers is not written. VERIFIED (`lyr` before `override`).
- Layer fields: `version = 2`, `extent = 4096`. VERIFIED.

### 8.4 Keys, values, tags (`mvt.cpp:mvt_layer::tag`, `mvt_tile::encode`)

- Feature tags: one `(key, value)` pair per attribute, in the input order, nulls removed.
  Duplicate keys in one feature give two pairs. VERIFIED (`{"k":1,"k":2}` → tags `[0,0,0,1]`).
- **Key table**: order of first use in the layer. Dedup uses a 65536-slot table indexed by
  `fnv1a64(key) % 65536`. A slot holds one key index. On a collision with a different key,
  the new key is appended and takes the slot. So after collisions a key can appear twice in
  the key table. READ. Copy this exactly for byte identity.
- **Value table**: dedup with `std::hash<mvt_value> % 65536` (fnv1a64 over the string bytes plus a
  trailing 0 byte for strings; over the raw 4/8-byte little-endian number for float, double,
  int, uint, sint; 1 byte for bool). Then at encode time each value is encoded as a Value
  message. The list is **sorted by the encoded bytes** (stable) and exact duplicates are
  merged. Tags are remapped. VERIFIED (strings first, shorter first; then float, double, int,
  uint, sint, bool).
- fnv1a64: offset `14695981039346656037`, prime `1099511628211`, xor then multiply. READ
  (`text.cpp:fnv1a`)

### 8.5 Protobuf layout (`mvt.cpp:mvt_tile::encode`, protozero)

- Tile: field 3 (layers), in layer order.
- Layer: `15 version`, `1 name`, `5 extent`, `3 key` × n, `4 value` × n, `2 feature` × n.
  VERIFIED.
- Value: one of `1 string`, `2 float`, `3 double`, `4 int64`, `5 uint64`, `6 sint64`,
  `7 bool`.
- Feature: `3 type`, `2 tags` (packed; omitted if empty), `1 id` (only if has_id),
  `4 geometry` (packed). VERIFIED (`[3,2,1,4]`, `[3,4]` without tags).
- Geometry: standard MVT commands. A command header is `(count << 3) | cmd`. Runs of the
  same op share one header (MultiPoint → one MOVETO with count n). Coordinates are deltas,
  zigzag32. CLOSEPATH has count 1. Polygon rings: `MoveTo(1) LineTo(n) ClosePath(1)`.
  VERIFIED.
- Winding in output: outer rings have positive shoelace area in tile coordinates (y down,
  clockwise on screen); holes negative. VERIFIED (44545 positive outer, 2568 negative holes in
  z16 sample).

## 9. Compression and output

### 9.1 Tile compression (`mvt.cpp:compress`)

- zlib `deflateInit2(level Z_DEFAULT_COMPRESSION (6), Z_DEFLATED, windowBits 31 (gzip),
  memLevel 8, Z_DEFAULT_STRATEGY)`. Header `1f 8b 08 00 00000000 00 03` (no name, mtime 0,
  OS = 3). VERIFIED: Python `zlib.compressobj(6, DEFLATED, 31, 8, 0)` with zlib 1.3.1 gives the
  same bytes for 2000 of 2000 tiles.
- The reference machine uses classic zlib 1.3.1 (`zlib1g 1.3.1`). Other deflate libraries
  (miniz_oxide, libdeflate, zlib-ng) make other bytes. `zlib-rs` 0.6.8 and `zlib-ng` 2.x do
  not match: the same bytes for only 37% of 342 357 tiles (their output is the same as each
  other, 1.6% larger than zlib). VERIFIED.
- Compiler flags (`-O2`, `-O3`, `-march=x86-64-v3`, `UNALIGNED_OK`, PGO) do not change the
  bytes and give no speed gain (within 2%). A raw deflate (windowBits -15) with a hand-written
  gzip header and trailer and `crc32fast` gives the same bytes, but no speed gain. VERIFIED.
- vtmake uses `crates/vtmake-zlib`: zlib 1.3.1, vendored, with three changes that keep
  the output bytes (sparse hash clear on reset, a faster Huffman heap, an 8-byte match
  compare; see `crates/vtmake-zlib/PATCHES.md`). 1.25 times the speed of the system zlib on
  tippecanoe tiles. VERIFIED: same bytes for 342 357 of 342 357 tiles, 280 000 fuzz cases.
- Optional (`VTMAKE_GZIP=fast`): libdeflate level 6. Other gzip bytes (header OS byte 255, other
  deflate), the same decoded tiles, 1.35% smaller, approximately 1.7 times the speed of
  `vtmake-zlib`.

### 9.2 `-e` directory

- File: `<dir>/<z>/<x>/<y>.pbf`, y in XYZ (not TMS). The content is the gzip tile (same bytes
  as in mbtiles). VERIFIED.
- Directories are made with mode 0777 (umask applies). An existing tile file is an error
  (exit). READ (`dirtiles.cpp:dir_write_tile`)
- `metadata.json` at the end (9.3).

### 9.3 Metadata (`mbtiles.cpp:make_metadata`, `dirtiles.cpp:dir_write_metadata`)

All rows are VERIFIED byte by byte with `bench/meta-check.sh` (38 cases, 9 error cases and three
full-size inputs: points, polygons and public lines; the points and polygons are private data,
not in this repo). Only `generator` is different (`vtmake v…`).

| Key | Value |
|---|---|
| name, description | the `-e`/`-o` argument as given |
| version | `2` |
| minzoom, maxzoom | `-Z`; highest zoom that tiled (5.5) |
| center | `lon,lat,maxzoom`: the `-z` tile with the most output draws (`geometry.size()`, tie: smaller `(x<<32)|y`; tile 0/0 if no tile at `-z`). `lon` = mean of the tile's two edge longitudes, `lat` = mean of its two edge latitudes (degrees, at the metadata maxzoom), clamped to the bounds, `sixdig`, `%f` |
| bounds | file bbox (union of the feature bboxes of the kept features; off the plane in x → the whole width; y clamped) via `tile2lonlat`, `sixdig`, `%f` |
| antimeridian_adjusted_bounds | `file_bbox1` (all vertices, x masked to 32 bits, y clamped) or `file_bbox2` (the same, x < 2^31 moved by +2^32): the narrower one |
| type | `overlay` |
| strategies | JSON array, one `{}` per zoom 0…`-z` (not the metadata maxzoom); only non-zero counters in this order: `dropped_by_rate`, `tiny_polygons` (the others are 0 here). Omitted if all are zero |
| tippecanoe_decisions | only if the metadata maxzoom is not `-z`: `{"basezoom":<-z>,"droprate":2.5,"retain_points_multiplier":1}` |
| format | `pbf` |
| generator | `tippecanoe v2.82.0` |
| generator_options | `argv` joined by spaces; an argument with chars other than `[A-Za-z0-9/-_@:.%,]` is put in `'…'` (so `'--buffer=5'`); a `'` in it becomes `'"'"'` |
| json | `{"vector_layers":[…],"tilestats":{…}}` |

- The statistics come from the input (`serialize_feature`, at read time), not from the tiles:
  each feature with a non-empty geometry after `fix_polygon` counts (also with a minzoom above
  `-z`). A GeometryCollection member is one feature. Null values are not counted; an
  attribute with only nulls is not listed.
- Sample values per (layer, attribute): the 1000 smallest distinct `(text, type)` pairs
  (byte order of the text, then type string < number < boolean). The text is the stringified
  value of 3.4 (so numbers keep their `dtoa_milo` text). `min`/`max`: `atof` of all number
  values, written with `dtoa_milo`.
- The attribute type (`fields` and tilestats `type`) comes from the **sample** types only:
  one type → `Number`/`Boolean`/`String` (`number`/`boolean`/`string`), else `Mixed`.
- vector_layers: one entry per layer name in the input, sorted by name: `id`,
  `description ""`, `minzoom` = `-Z`, `maxzoom` = metadata maxzoom, `fields` (key order
  sorted, at most 1000).
- tilestats: `layerCount`, per layer `count` = feature count, `geometry` = most frequent type
  (tie: Point, then LineString), `attributeCount` (at most 1000), at most 100 attributes:
  `count` = number of samples (at most 1000), `type`, `values` (the first 100 samples; a
  string longer than 256 UTF-16 units is not written and not counted), and `min`/`max` if a
  sample is a number. Layer and attribute names in tilestats are cut to 256 UTF-16 units
  (`truncate16`), and equal names are merged.
- metadata.json layout: `{\n`, then one `"key": "value",\n` per field, last field without
  comma, then `\n}\n`. The `json` value is an escaped JSON string. The field order is not the
  mbtiles order: `strategies` and `tippecanoe_decisions` come after `type`, before `format`.
  VERIFIED.
- No valid feature (exit 110): the mbtiles file has the schema and `sqlite_stat1`, but no
  metadata rows; the `-e` directory is made, with no `metadata.json`. VERIFIED.
- The metadata does not depend on the thread count. VERIFIED.

### 9.4 mbtiles (`mbtiles.cpp`)

- Schema: `metadata(name text, value text)` + unique index `name`; `map(zoom_level,
  tile_column, tile_row, tile_id)` + unique index; `images(zoom_level, tile_data blob,
  tile_id text)` + unique index `(zoom_level, tile_id)`; view `tiles`. VERIFIED.
- `tile_row = 2^z - 1 - y` (TMS). `tile_id` = decimal `fnv1a64(tile bytes)`. Equal tiles in
  one zoom share one `images` row. READ
- PRAGMAs: `synchronous=0`, `locking_mode=EXCLUSIVE`, `journal_mode=DELETE`. At close:
  `ANALYZE` (so a `sqlite_stat1` table exists). VERIFIED. The system SQLite has no STAT4, so
  there is no `sqlite_stat4` table; vtmake builds its bundled SQLite without STAT4
  (`.cargo/config.toml`).
- `version`, `minzoom`, `maxzoom` are inserted as integers, but the `value text` column
  stores them as text. All values have SQL type `text`. VERIFIED.
- Without `--force`, an output with a `metadata` table is an error: exit 104. VERIFIED.
- Metadata rows in this order: name, description, version, minzoom, maxzoom, center, bounds,
  antimeridian_adjusted_bounds, type, format, generator, generator_options, strategies, json.
  VERIFIED.
- The mbtiles **file** bytes differ between runs (row insert order depends on threads). The
  rows are the same. VERIFIED.

### 9.5 `--force`

- `-o`: `unlink()` the file first, then create a new database. READ
- `-e`: delete `metadata.json` and every `<z>/<x>/<y>.pbf|.mvt` file with numeric names.
  Directories stay. Other files stay. READ (`dirtiles.cpp:check_dir`)

## 10. Determinism

| Factor | Effect on tile bytes | Tag |
|---|---|---|
| thread count (`TIPPECANOE_MAX_THREADS` 1/2/32) | none | VERIFIED |
| `--read-parallel` on/off | none | VERIFIED |
| repeated run | none (tiles) | VERIFIED |
| memory size (radix sort chunks) | none expected; sort key is total | READ |
| skip pattern below `-Z` (depends on CPUS) | none seen | VERIFIED (UNSURE in theory, 5.4) |
| mbtiles file layout | differs | VERIFIED |
| zlib version | changes gzip bytes | READ |

## 11. Other items that affect MapLibre rendering

- **Feature order** in a tile: MapLibre draws fills in this order and places symbols in this
  order (when there is no `symbol-sort-key`). So the order changes which label wins a
  collision. Keep the order of 8.1.
- **Buffer content**: lines and polygons in the 80-unit buffer avoid seams at tile edges.
  Points in the buffer are copies. MapLibre does not place a symbol whose anchor is outside
  the tile, but the copy matters for icons that cross the edge. Keep the buffer.
- **Tile-edge vertices** (7.4 step 3): no visual effect alone, but they change later
  simplification.
- **Coordinate rounding** (7.7): one tile unit is 1/8 px for a 512 px tile. A ±1 error is
  sub-pixel but can change anti-aliased edge pixels.
- **Attribute types**: values are equal for float vs double (3.4). A number vs string change
  would break style filters. Keep the type rules.
- **Feature id**: only from a non-negative integer `id`. MapLibre `promoteId` /
  feature-state need it.
- **Ring classification**: MapLibre groups a hole with the outer ring before it. Write each
  outer ring, then its holes (wagyu does this).
- **Tiny polygon squares** (7.3): at z12–z14 small buildings become 2 × 2 unit squares at
  accumulated positions. They are visible as specks.
- **Tile set**: a tile with only buffer content is written. A missing tile renders as empty.
- No label points (`-aP` off), no clustering, no coalescing.

## 12. Notes for the Rust design (performance only, no output effect)

- With `--extend-zooms-if-still-dropping`, tippecanoe writes child data for z = maxzoom+1
  at maxzoom and never reads it. The Rust version can skip this. READ (`tile.cpp:rewrite`)
- Children are chosen from the unclipped bbox (6.3). A tighter candidate set is fine.
- No tile is retried (limits are off), so each tile is processed once.

## 13. Hard parts, most difficult first

1. **Polygon cleaning (wagyu 0.5.0) with the ×16 retry** (7.5). Ring start points, ring
   order and snap rounding come from wagyu internals. Only a faithful port gives the same
   bytes. Other union libraries (i_overlay, clipper2) give the same area, but other vertex
   order and sometimes ±1 unit at self-intersections.
2. **Douglas–Peucker variant** (7.4): stack order, direction rule, tie rules, `retain`, the
   1/16 rounding of the distance, and the integer-safe distance. Easy to port, easy to get
   wrong.
3. **Tiny polygon accumulation** (7.3): tile-wide state that depends on feature order and
   on the clip result.
4. **Clip cascade and rounding** (6.2, 6.6, 6.7): integer Cohen–Sutherland for lines,
   double Sutherland–Hodgman with strict tests for polygons, each zoom from the parent
   result.
5. **Feature order** (8.1, 8.2): quadkey of a type-specific index point. The polygon center
   of mass uses double math with truncation and a `size_t` accumulator.
6. **Projection** (4.1): the `round(.../2)` y formula and libm results.
7. **Number formatting** (3.5): Grisu2 + Prettify. Needed for nested JSON strings and
   tilestats; for plain numbers only the integer test matters.
8. **Key/value tables** (8.4): hash-slot dedup with collisions and byte-sorted values.
   Simple once known.
9. **gzip bytes** (9.1): use classic zlib 1.3.1 (vtmake vendors it in
   `crates/vtmake-zlib`) to get the same bytes.
10. **Metadata** (9.3): many small format rules; counts of `tiny_polygons` per zoom.

## 14. Recommendation: what must be exact

Must be exact for visual identity (primary goal):

- Zoom rules (5.1), including the drop rule for points without `tippecanoe.minzoom` (5.2).
- Buffer size and clip semantics (6). The tile set (which tiles exist).
- Projection and `to_tile_scale` rounding (4.1, 7.7). Cheap to copy, and it removes a
  source of pixel noise.
- Simplification tolerance and algorithm (7.4). A different simplifier changes shapes at
  z12–z15 by up to 1 tile unit; copy it.
- Tiny polygon reduction (7.3) and `line_is_too_small` (7.2): they decide which small
  features exist at low zooms.
- Feature order (8.1) and layer order (8.3): label collision and fill overlap.
- Attribute keys, value types and values (3.4); feature id (3.3).
- Polygon validity: outer ring then holes, outer positive area, no self-intersections (7.5).

Can differ without a visual change (second goal or bonus only):

- Wagyu ring start point and ring order inside a feature, if the filled area is the same.
  Start with a faithful port anyway if the byte-identical bonus is wanted; else use a robust
  union and compare rendered images.
- Key and value table order and the hash-collision duplicates (8.4).
- Tile-edge vertices (7.4 step 3), if the simplifier result is otherwise the same.
- gzip bytes (9.1): any valid gzip decodes the same. Note: a tile store that uploads only the
  changed tiles must upload all tiles again once if the gzip bytes change.
- Metadata text (9.3), the mbtiles file layout (9.4), `strategies` counts.
- Behaviour in the "undefined" corners: zero-area polygon center of mass (8.2), the
  > 10 MB early clean (7.5), NUL in strings (3.4).

## 15. Experiment log

All runs used felt `tippecanoe`, built from the felt/tippecanoe `4f26211` source, with the full
flag set. The inputs were cut from the public corpus (not in this repo). Decoders:
`tippecanoe-decode` and a small raw protobuf dumper (not in this repo).

| Test | Result |
|---|---|
| 2000 polygons, `-e` and `-o` | 10633 tiles; mbtiles blobs = directory files; gzip header; schema; metadata order |
| 100k polygons, `TIPPECANOE_MAX_THREADS` = 1, 2, 32, and a repeat | all 62651 tiles byte-identical; metadata identical apart from the name |
| repeat `-o` | different file SHA-1, same rows |
| same input without `--extend-zooms-if-still-dropping` | 9086 tiles differ (precision) |
| same input without `--drop-densest-as-needed` | 436 tiles differ (order) |
| same input without `--read-parallel` | 0 tiles differ |
| 20k polygons with `seq` property | tile order ≠ input order; order changes without drop-densest |
| handmade type test (5 features) | types of 3.4, id rules, layer override, duplicate keys, null properties, missing properties, non-integer minzoom exit |
| 2000 points with / without `tippecanoe.minzoom` | drop rule of 5.2 |
| 2000 points with `"maxzoom":16`, `-z18` | metadata maxzoom 16, exit 0 |
| 20k lines (polygon outer rings) | coordinate range −80…4176, tile-edge vertices, multi-part clips, vertex reduction at z18 |
| z16 polygon tiles | outer rings positive area, holes negative, range −80…4177 |
| zlib recompression of 2000 tiles | identical bytes with zlib 1.3.1 level 6, windowBits 31 |
