#!/bin/bash
# Compare vtmake build with felt tippecanoe on the seed flags (the plain command line):
# exit code, the mbtiles metadata rows (order, SQL type, value), metadata.json
# (bytes), and the tile bytes (vtmake-compare and diff -r).
#
# The `generator` value is the only permitted difference (vtmake writes its
# own name). The script replaces it before the comparison.
#
# Usage: bench/meta-check.sh [case...]      (no case: all cases)
# Env:   FELT  felt binary   (no default)
#        VTMAKE  our binary  (default target/release/vtmake; run as "vtmake build")
#        WORK  work folder   (default ${TMPDIR:-/tmp}/vtmake-meta-check)
#        CPUS  taskset list  (default 2-31; CPUs 0-1 run benchmarks)
#        FELT_THREADS  TIPPECANOE_MAX_THREADS for felt (default 2, as the small
#              2-CPU profile). With more threads, felt skips zooms below -Z and
#              its tiles can change (spec 5.4); vtmake clips at every zoom.
#        P     the public corpus folder (points.ndjson, polygons.ndjson,
#              points-plain.ndjson). Not set: the pub-* cases are skipped.
#        PL    pub-lines.ndjson from bench/prep_lines.py. Not set: the
#              pub-lines case is skipped.
# Exit:  0 if all cases are equal, 1 if not.
set -uo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
FELT=${FELT:?set FELT to the felt tippecanoe binary}
VTMAKE=${VTMAKE:-$R/target/release/vtmake}
# Run vtmake under the name "tippecanoe" (it then acts as `vtmake build`), so
# that both binaries take the same arguments.
mkdir -p "$R/target/compat" && ln -sf "$VTMAKE" "$R/target/compat/tippecanoe"
BT=$R/target/compat/tippecanoe
WORK=${WORK:-${TMPDIR:-/tmp}/vtmake-meta-check}
CPUS=${CPUS:-2-31}
FELT_THREADS=${FELT_THREADS:-2}
CMP=$R/target/release/vtmake-compare
IN=$WORK/in
mkdir -p "$IN"

# Inputs. Public data: the first 200000 lines (in WORK, not in the repository).
# Synthetic data: bench/meta-inputs.py.
subset() { # name source
  [ -s "$IN/$1.ndjson" ] || { [ -f "$2" ] && head -n 200000 "$2" >"$IN/$1.ndjson"; }
}
if [ -n "${P:-}" ]; then
  subset pub-points "$P/points.ndjson"
  subset pub-points-plain "$P/points-plain.ndjson"
  subset pub-polygons "$P/polygons.ndjson"
fi
[ -n "${PL:-}" ] && subset pub-lines "$PL"
python3 -I "$R/bench/meta-inputs.py" "$IN" || exit 1

# name | -Z | -z | layer ("" for no --layer) | inputs (relative to IN; "-" is stdin
# from the next file) | how: "" (one run), "noforce" (a second run without --force
# must fail and keep the output), "rerun" (a second run with --force over the output)
CASES=(
  "pub-points|9|18|pois|pub-points.ndjson"
  "pub-points-plain|9|18|pois|pub-points-plain.ndjson"
  "pub-polygons|12|18|bldg|pub-polygons.ndjson"
  "pub-lines|9|18|lines|pub-lines.ndjson"
  "types|12|16|types|types.ndjson"
  "multi|12|15|deflt|multi.ndjson"
  "maxz-low|12|18|low|maxz-low.ndjson"
  "rate|9|18|rate|rate.ndjson"
  "multigeom|11|16|mg|multigeom.ndjson"
  "edge|12|16|edge|edge.ndjson"
  "one|9|18|one|one.ndjson"
  "zoomrange|12|16|zr|zoomrange.ndjson"
  "antimeridian|0|8|am|antimeridian.ndjson"
  "antimeridian-z2|2|6|am|antimeridian.ndjson"
  "cascade|12|16|cas|cascade.ndjson"
  "dateline-z0|0|5|dl|dateline.ndjson"
  "dateline-z3|3|7|dl|dateline.ndjson"
  "early-clean|10|12|ec|early-clean.ndjson"
  "z22|18|22|z22|multigeom.ndjson"
  "polar|0|4|polar|polar.ndjson"
  "multipoint-rate|10|16|mpr|multipoint-rate.ndjson"
  "z30|19|30|z30|edge.ndjson"
  "missing-file|12|14|mf|multi.ndjson nonexist.ndjson"
  "degenerate|12|14|deg|degenerate.ndjson"
  "two-files|12|16|two|multi.ndjson maxz-low.ndjson"
  "stdin|12|15|si|- multi.ndjson"
  "nolayer|12|15||multi.ndjson maxz-low.ndjson"
  "stdin-nolayer|12|14||- one.ndjson"
  "wide|12|14|wide|wide.ndjson"
  "numbers|9|12|num|numbers.ndjson"
  "maxz-below|12|14|mb|maxz-below.ndjson"
  "noforce|12|14|nf|one.ndjson|noforce"
  "rerun|12|15|rr|multi.ndjson|rerun"
  "invalid|12|14|inv|invalid.ndjson"
  "empty|12|14|emp|empty.ndjson"
)

norm_gen() { sed -E 's/^(generator\|text\|).*/\1GEN/; s/^("generator": ).*/\1"GEN",/'; }

run_one() { # tool-dir binary mode(o|e) force(--force or "") Z z layer inputs...
  local d=$1 bin=$2 mode=$3 force=$4 Z=$5 z=$6 layer=$7; shift 7
  mkdir -p "$d" && cd "$d" || return
  local out=out.mbtiles flag=-o args=() stdin=/dev/null
  [ "$mode" = e ] && { out=outdir; flag=-e; }
  while [ $# -gt 0 ]; do
    if [ "$1" = - ]; then stdin=$IN/$2; shift 2; else args+=("$IN/$1"); shift; fi
  done
  mkdir -p bin && ln -sf "$bin" bin/tippecanoe
  local env=()
  [ "$bin" = "$FELT" ] && env=(TIPPECANOE_MAX_THREADS="$FELT_THREADS")
  local lflag=(--layer "$layer")
  [ -z "$layer" ] && lflag=()
  # shellcheck disable=SC2086
  env "${env[@]}" PATH="$d/bin:$PATH" taskset -c "$CPUS" tippecanoe -Z"$Z" -z"$z" --read-parallel --buffer=5 $force \
    --drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit --no-tile-size-limit \
    "${lflag[@]}" $flag $out "${args[@]}" <"$stdin" >stdout.txt 2>stderr-$mode.txt
  echo $? >exit-$mode.txt
}

fail=0
want=("$@")
for c in "${CASES[@]}"; do
  IFS='|' read -r name Z z layer inputs how <<<"$c"
  if [ ${#want[@]} -gt 0 ] && [[ ! " ${want[*]} " == *" $name "* ]]; then continue; fi
  ok=1
  missing=0
  for f in $inputs; do [ "$f" = - ] || [ "$f" = nonexist.ndjson ] || [ -f "$IN/$f" ] || missing=1; done
  [ $missing = 1 ] && { echo "SKIP $name (no input)"; continue; }
  rm -rf "$WORK/$name"
  for mode in o e; do
    for t in felt bt; do
      bin=$FELT; [ $t = bt ] && bin=$BT
      # shellcheck disable=SC2086
      (run_one "$WORK/$name/$t" "$bin" $mode --force "$Z" "$z" "$layer" $inputs)
      if [ "$how" = noforce ] || [ "$how" = rerun ]; then
        f=--force; [ "$how" = noforce ] && f=
        (cd "$WORK/$name/$t" && find out.mbtiles outdir -type f -exec md5sum {} + 2>/dev/null | sort -k2 >sums-before-$mode.txt
         mv exit-$mode.txt exit1-$mode.txt
         # shellcheck disable=SC2086
         run_one "$WORK/$name/$t" "$bin" $mode "$f" "$Z" "$z" "$layer" $inputs
         find out.mbtiles outdir -type f -exec md5sum {} + 2>/dev/null | sort -k2 >sums-after-$mode.txt)
      fi
    done
  done
  if [ "$how" = noforce ]; then
    for t in felt bt; do
      cmp -s "$WORK/$name/$t/sums-before-o.txt" "$WORK/$name/$t/sums-after-o.txt" || { echo "  $name: $t changed the old -o output"; ok=0; }
      cmp -s "$WORK/$name/$t/sums-before-e.txt" "$WORK/$name/$t/sums-after-e.txt" || { echo "  $name: $t changed the old -e output"; ok=0; }
    done
  fi
  F=$WORK/$name/felt B=$WORK/$name/bt
  for mode in o e; do
    if ! cmp -s "$F/exit-$mode.txt" "$B/exit-$mode.txt"; then
      echo "  $name -$mode: exit code felt $(cat "$F/exit-$mode.txt") bt $(cat "$B/exit-$mode.txt")"; ok=0
    fi
  done
  if [ -f "$F/out.mbtiles" ] || [ -f "$B/out.mbtiles" ]; then
    q="select name, typeof(value), value from metadata order by rowid"
    for d in "$F" "$B"; do
      if [ -f "$d/out.mbtiles" ]; then
        sqlite3 "$d/out.mbtiles" "$q" 2>&1 | norm_gen >"$d/meta.txt"
        sqlite3 "$d/out.mbtiles" .schema >"$d/schema.txt" 2>&1
      else
        echo "(no out.mbtiles)" | tee "$d/meta.txt" >"$d/schema.txt"
      fi
    done
    diff "$F/meta.txt" "$B/meta.txt" >"$WORK/$name/meta.diff" || { echo "  $name: mbtiles metadata differs ($WORK/$name/meta.diff)"; ok=0; }
    cmp -s "$F/schema.txt" "$B/schema.txt" || { echo "  $name: mbtiles schema differs"; ok=0; }
    if [ -f "$F/out.mbtiles" ] && [ -f "$B/out.mbtiles" ]; then
      taskset -c "$CPUS" "$CMP" "$F/out.mbtiles" "$B/out.mbtiles" --json "$WORK/$name/compare.json" >"$WORK/$name/compare.txt" 2>&1
      tiles=$(python3 -I "$R/bench/compare-summary.py" "$WORK/$name/compare.json" 2>/dev/null)
      read -r ta tb ti oa ob <<<"${tiles:-x y z 1 1}"
      if [ "$ta" != "$tb" ] || [ "$ta" != "$ti" ] || [ "$oa" != 0 ] || [ "$ob" != 0 ]; then
        echo "  $name: tiles differ: felt $ta bt $tb byte-identical $ti only-felt $oa only-bt $ob"; ok=0
      fi
    fi
  fi
  if [ -d "$F/outdir" ] || [ -d "$B/outdir" ]; then
    for d in "$F" "$B"; do
      if [ -f "$d/outdir/metadata.json" ]; then norm_gen <"$d/outdir/metadata.json"; else echo "(no metadata.json)"; fi >"$d/metadata.json"
    done
    diff "$F/metadata.json" "$B/metadata.json" >"$WORK/$name/metadata-json.diff" || { echo "  $name: metadata.json differs ($WORK/$name/metadata-json.diff)"; ok=0; }
    diff -r -x metadata.json "$F/outdir" "$B/outdir" >"$WORK/$name/dir.diff" 2>&1 || { echo "  $name: -e tiles differ ($WORK/$name/dir.diff)"; ok=0; }
  fi
  if [ $ok = 1 ]; then
    echo "PASS $name (exit $(cat "$F/exit-o.txt"), tiles ${ta:-0})"
  else
    echo "FAIL $name"; fail=1
  fi
  unset ta
done
# Command line errors: the exit code must be the same (101 for most).
ARGCASES=(
  "unknown-option|-Z1 -z2 --foo -o o.mbtiles one.ndjson"
  "no-output|-Z1 -z2 one.ndjson"
  "minzoom-gt-maxzoom|-Z5 -z2 -o o.mbtiles one.ndjson"
  "minzoom-gt-24|-Z30 -z33 -o o.mbtiles one.ndjson"
  "two-outputs|-Z1 -z2 -o o.mbtiles -e od one.ndjson"
  "bad-buffer|-Z1 -z2 --buffer=abc -o o.mbtiles one.ndjson"
  "big-buffer|-Z1 -z2 --buffer=200 -o o.mbtiles one.ndjson"
  "bad-zoom|-Zx -z2 -o o.mbtiles one.ndjson"
  "no-arguments|"
)
if [ ${#want[@]} -eq 0 ] || [[ " ${want[*]} " == *" args "* ]]; then
  for c in "${ARGCASES[@]}"; do
    IFS='|' read -r name args <<<"$c"
    codes=()
    for bin in "$FELT" "$BT"; do
      d=$WORK/args/$name; rm -rf "$d"; mkdir -p "$d"
      # shellcheck disable=SC2086
      (cd "$d" && cp "$IN/one.ndjson" . && taskset -c "$CPUS" "$bin" $args >/dev/null 2>&1; echo $? >code)
      codes+=("$(cat "$d/code")")
    done
    if [ "${codes[0]}" = "${codes[1]}" ]; then echo "PASS args $name (exit ${codes[0]})"; else echo "FAIL args $name: felt ${codes[0]} bt ${codes[1]}"; fail=1; fi
  done
fi
exit $fail
