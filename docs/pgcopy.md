# Postgres binary COPY input (`--pgcopy-schema`)

vtmake only (not in felt/tippecanoe).

`vtmake build` can read the Postgres binary COPY format
(`COPY (...) TO STDOUT (FORMAT binary)`) instead of GeoJSON. Each row gives one
feature: a geometry column with GeoJSON text (from `ST_AsGeoJSON`) and typed
property columns. Postgres does not make JSON for the properties, and no other
program converts the rows.

The COPY input makes the same tiles as a Node.js NDJSON exporter that reads the
same query (see "The reference path" below). For such an exporter, the tiles
are **byte-identical** to the NDJSON path (exporter + `vtmake build`).

```sh
psql ... -c "COPY (<select>) TO STDOUT (FORMAT binary)" \
  | vtmake build -Z12 -z18 --read-parallel --buffer=5 --force \
      --drop-densest-as-needed --extend-zooms-if-still-dropping \
      --no-feature-limit --no-tile-size-limit --layer buildings \
      --pgcopy-schema schema.json -e tiles
```

- `--pgcopy-schema=<file or inline JSON>`: all sources (files, or standard
  input if there is no file) are COPY streams with this schema.
- Standard input can be a pipe: vtmake reads and decodes while Postgres still
  sends. A file works the same.
- The stream must end with the COPY trailer, with nothing after it. If not,
  vtmake stops with exit 107 and writes no tiles. Thus an export that fails
  before its end never gives partial tiles. Other errors: 101 (schema), 107
  (data), 119 (invalid UTF-8), 111 (cannot open the schema file).
- Unless `-q`: one line on standard error, `<input>: pgcopy: <rows> rows, <n> with
  geometry, <features> features`. `<n>` is the number of rows that the exporter
  does not skip (`if (!geometry) continue`). If it is 0, the region is empty.
  Then vtmake stops with exit 110 ("Did not read any valid geometries"), as for
  an empty NDJSON file.
- Text columns must be UTF-8: run the COPY with `client_encoding = UTF8`.

## Schema

```json
{
  "format": "vtmake-pgcopy/1",
  "layer": "buildings",
  "stripNulls": true,
  "keyOrder": "jsonb",
  "columns": [
    {"name": "geom", "oid": 25, "role": "geometry"},
    {"name": "building_id", "oid": 2950},
    {"name": "height", "type": "int4"},
    {"name": "debug", "type": "text", "role": "ignore"}
  ],
  "attributes": {
    "before": {"layer": "buildings"},
    "minzoom": "tippecanoe_minzoom",
    "maxzoom": "tippecanoe_maxzoom"
  },
  "zoom": {
    "zoomField": "min_zoom",
    "fallbackZoomFields": ["zoom_hint"],
    "defaultMinZoom": 12,
    "minZoom": 12,
    "maxZoom": 18
  }
}
```

| Member | Meaning |
|---|---|
| `columns` | One entry for each COPY column, in COPY order. `type` (a name) or `oid` (or both, if they agree). `role`: `geometry` (exactly one; text, json or jsonb), `property` (default), `ignore`. |
| Types | `text`/`varchar`/`bpchar`/`name` (25, 1043, 1042, 19), `uuid` (2950), `bool` (16), `int2` (21), `int4` (23), `int8` (20), `numeric` (1700), `jsonb` (3802). Send all other types as `to_jsonb(col)` (jsonb). Also send `json` that way: Postgres validates a json value only when it converts it to jsonb (`\u0000` fails, duplicate keys, numeric range). Thus a `json` property is a schema error (101). A `json` geometry column is permitted. |
| `layer` | `tippecanoe.layer` of each feature. |
| `stripNulls` | The query used `jsonb_strip_nulls`: vtmake removes NULL columns and null object members (at all depths). If false, a NULL column is a `null` attribute. |
| `keyOrder` | `jsonb` (default): the properties in jsonb key order (shorter keys first, then by bytes), as `to_jsonb(src)`. `columns`: COPY order. |
| `attributes.before` | Constant attributes before the properties (`{layer, ...props}`). |
| `attributes.minzoom`, `.maxzoom` | Attribute names for the feature minzoom and maxzoom (after the properties). These need `zoom`. |
| `zoom` | The feature minzoom rule: `Number(value)` of every field of `zoomField` and `fallbackZoomFields` (all of them, as `.map`), then the first finite one. If none is finite: `Number(defaultMinZoom)` if finite, else `minZoom`. Then the value is clamped to `[minZoom, maxZoom]`. `tippecanoe.minzoom` is that value, `tippecanoe.maxzoom` is `maxZoom`. `minZoom` and `maxZoom` default to `-Z` and `-z`. Set them when the layer range is not the `-Z`/`-z` range of this build. |

## The reference path

The COPY input makes the same features as this NDJSON path:

1. Postgres `to_jsonb(src) - 'geom'` (optionally `jsonb_strip_nulls`).
2. node-pg `JSON.parse`.
3. The feature object `{type, geometry, properties: {layer, ...props,
   tippecanoe_minzoom, tippecanoe_maxzoom}, tippecanoe: {layer, minzoom,
   maxzoom}}`, with the zoom rule of the schema.
4. `JSON.stringify`, one line for each row.
5. vtmake's NDJSON reader.

## The rules that give identical tiles

The COPY reader applies the same steps as the reference path
(`crates/vtmake-parse/src/pgcopy.rs`, `js.rs`):

- **Numbers.** int2/int4/int8 and numeric become the double that `JSON.parse`
  gives for the jsonb text (correctly rounded; int8 above 2^53 loses digits).
  The attribute text is the text of `Number::toString` (shortest digits, ties to
  even, exponent from 1e21 and below 1e-6), then the reader's number rule
  (integers above 2^53 keep the JavaScript digits, for example
  `4611686018427388000`). A value that is not finite (numeric `1e400`) is `null`
  (`JSON.stringify`). numeric NaN and the infinities are strings (`to_jsonb`).
- **Keys.** jsonb order, then the JavaScript order: array index keys (`"0"` to
  `"4294967294"`) first, in numeric order, then the other keys. Object spread:
  a property with the name of a constant keeps the position of the constant.
  The minzoom and maxzoom keys come last, unless a property has that name.
- **Nested values** (jsonb, and `to_jsonb` of json, arrays and other types):
  the text that the jsonpull port gives for the `JSON.stringify` text.
- **Nesting.** A scan without recursion first measures the depth of each JSON
  value. Up to 256 levels: vtmake decodes the row on its worker thread. Deeper:
  vtmake decodes the row again on a thread with a 1 GiB stack (virtual; only the
  used part is resident), up to 100,000 levels. Deeper than that: exit 107, no
  tiles (the scan rejects the row before any recursion). Measured stack use of
  a deep row in check mode (all recursive steps: parse, strip nulls, `Number()`,
  attribute text, NDJSON line, reference reader): 2.0 KB for each level in a
  debug build (192 MiB at 100,000 levels: 5x margin), 0.4 KB in a release build
  (38 MiB). Test: `pgcopy::tests::deep_stack_probe` (ignored; run it with
  `VTMAKE_PROBE_DEPTH` and `VTMAKE_PROBE_STACK`).
- **Accepted difference: very deep values.** Node.js fails for values deeper
  than a few thousand levels (RangeError in `JSON.parse` or `JSON.stringify`).
  The limit depends on the V8 stack size, so it is not a stable contract. The
  COPY path does not copy that failure. It writes the correct tiles (the tiles
  that vtmake's NDJSON reader makes for the same value) up to 100,000 levels.
  This difference is intentional.
- **`Number()`** (zoom fields): `ToPrimitive` as in JavaScript. An array is
  `join(",")` of its elements (`[5]` → 5, `["5"]` → 5, `[]` → 0, `[1,2]` → NaN,
  nested arrays too). An object from `JSON.parse` is NaN. But if it has an own
  `"toString"` member (never a function), JavaScript throws a `TypeError` (also
  for an object in an array). Then vtmake stops with exit 107 and writes no
  tiles, as a Node.js export stops. vtmake converts every candidate field, also
  after a finite one. vtmake does not convert a row whose geometry it skips.
- **Strings**: the Postgres text (the escapes of the JSON text decode to it).
- **Geometry**: vtmake reads the GeoJSON text with the fast geometry parser;
  `-0` becomes `0` (`JSON.stringify`). A NULL geometry or a JSON `null`/falsy
  value skips the row (`if (!geometry) continue`).
- **Order**: feature order = row order (`seq` = row number), as in the NDJSON
  path (line order).
- **Fallback.** Some rows go through the NDJSON reader: the decoder writes the
  line of the reference path and reads it with the fast parser, else with the
  jsonpull port. These rows are:
  - a geometry that is not `{"type": ..., "coordinates": ...}`
    (GeometryCollection, more than two dimensions, a `bbox` member, other
    member order)
  - a coordinate that is not finite
  - a zoom that is not an integer from 0 to 32 (a fatal error on both paths)
  - objects with `"type": "Feature"` or `"FeatureCollection"` in the properties
    (tippecanoe has special rules for them)
- `VTMAKE_PGCOPY_CHECK=1` sends each row through both paths and stops (exit 107)
  at the first difference.

Some column types give session-dependent text in `to_jsonb` (timestamptz uses
`TimeZone`, floats use `extra_float_digits`). Send these columns as
`to_jsonb(col)`. Then Postgres applies the same session settings as in
`to_jsonb(src)`.

## Rules for the query

- **Use one transaction.** If you read the column types first (for example with
  `SELECT * FROM src LIMIT 0`), do it in the same transaction as the COPY, on
  the same connection. That read takes `AccessShareLock` on the tables, so a
  concurrent `ALTER TABLE` waits until the COPY transaction ends. Thus the
  schema always agrees with the stream.
- **Do not keep a schema between transactions.** A schema that is out of date
  can give different tiles with exit 0. The binary format has no type data, and
  some type changes keep the field length (for example int4 → real). vtmake
  stops (exit 107, no tiles) when a row has a different field count from the
  schema, or when a fixed-width field has a different length (int2 2, int4 4,
  int8 8, bool 1, uuid 16, numeric header). It cannot find other differences.
- **Keep the row order stable.** If the query has no `ORDER BY`, a parallel plan
  can change the row order, and thus the feature order and the tile bytes. Use
  `SET LOCAL max_parallel_workers_per_gather = 0` (or an `ORDER BY`) for the
  COPY and for any NDJSON export that you compare with it.
