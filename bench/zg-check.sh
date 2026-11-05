#!/bin/bash
# Compare public input subsets with felt on -zg: all tile bytes must match.
# Env:   FELT    felt binary (no default)
#        PUBLIC  the public corpus folder (points-plain.ndjson, polygons-plain.ndjson; no default)
#        PL      pub-lines.ndjson from bench/prep_lines.py (no default); the
#                script reads pub-lines-plain.ndjson in the same folder
#        VTMAKE  our binary (default target/release/vtmake)
#        WORK    work folder (default ${TMPDIR:-/tmp}/vtmake-zg-check)
set -euo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
W=${WORK:-${TMPDIR:-/tmp}/vtmake-zg-check}
F=${FELT:?set FELT to the felt tippecanoe binary}
V=${VTMAKE:-$R/target/release/vtmake}
P=${PUBLIC:?set PUBLIC to the public corpus folder (points-plain.ndjson, polygons-plain.ndjson)}
PL=${PL:?set PL to pub-lines.ndjson from bench/prep_lines.py}
mkdir -p "$W"
export TIPPECANOE_MAX_THREADS=2
for layer in points polygons lines; do
  input=$P/$layer-plain.ndjson
  if [ "$layer" = lines ]; then input=${PL%.ndjson}-plain.ndjson; fi
  n=100000
  if [ "$layer" = lines ]; then n=500000; fi
  head -n "$n" "$input" > "$W/$layer.ndjson"
  flags=(-q -zg --read-parallel --force --drop-densest-as-needed --extend-zooms-if-still-dropping -l "$layer")
  "$F" "${flags[@]}" -o "$W/$layer-felt.mbtiles" "$W/$layer.ndjson"
  "$V" build "${flags[@]}" -o "$W/$layer-vtmake.mbtiles" "$W/$layer.ndjson"
  "$R/target/release/vtmake-compare" "$W/$layer-felt.mbtiles" "$W/$layer-vtmake.mbtiles"
  query="select hex(sha3_query('select zoom_level,tile_column,tile_row,tile_data from tiles order by 1,2,3'))"
  test "$(sqlite3 "$W/$layer-felt.mbtiles" "$query")" = "$(sqlite3 "$W/$layer-vtmake.mbtiles" "$query")"
  echo "PASS $layer: all tile bytes match"
done
