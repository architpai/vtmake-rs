#!/bin/bash
# Compare vtmake build with felt tippecanoe on the geometry options: exit
# code, the mbtiles metadata rows, the tiles
# (vtmake-compare: byte-identical), and for the "ts" cases also stderr.
#
# Usage: bench/geom-check.sh [case...]      (no case: all cases)
# Env:   H  the flags added to the "t" cases (default: the four seed flags;
#           H= runs the default tile mode)
# Env:   FELT, VTMAKE, WORK, CPUS, FELT_THREADS as in bench/meta-check.sh
#        P  the public corpus folder (points.ndjson, polygons.ndjson; no default)
#        PL pub-lines.ndjson from bench/prep_lines.py (no default)
#        T  felt's test folder (no default; read only)
# Exit:  0 if all cases are equal, 1 if not.
set -uo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
FELT=${FELT:?set FELT to the felt tippecanoe binary}
VTMAKE=${VTMAKE:-$R/target/release/vtmake}
WORK=${WORK:-${TMPDIR:-/tmp}/vtmake-geom-check}
mkdir -p "$WORK/compat" && ln -sf "$VTMAKE" "$WORK/compat/tippecanoe"
BT=$WORK/compat/tippecanoe
P=${P:?set P to the public corpus folder (points.ndjson, polygons.ndjson)}
PL=${PL:?set PL to pub-lines.ndjson from bench/prep_lines.py}
CPUS=${CPUS:-2-31}
FELT_THREADS=${FELT_THREADS:-2}
CMP=$R/target/release/vtmake-compare
T=${T:?set T to the tests folder of the felt source}
IN=$WORK/in
mkdir -p "$IN"
python3 -I "$R/bench/geom-inputs.py" "$IN" "$P" "$PL" || exit 1
H=${H-"--drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit --no-tile-size-limit"}

# name | how | arguments | standard input
# how: "t" (tiles: the seed flags ($H) and "-f -o out.mbtiles" are added), "ts"
# (the same, and stderr must be equal), "e" (an error: the exit code and
# stderr must be equal; nothing is added). Standard input: "<file" or empty.
CASES=(
  # Controls: the same inputs without a geometry option
  "c-countries|t|-z4 $T/ne_110m_admin_0_countries/in.json.gz|"
  "c-islands|t|-z7 $T/islands/in.json|"
  "c-curve|t|-z2 $T/curve/in.json|"
  "c-muni|t|-Z11 -z11 $T/muni/muni.json|"
  "c-wrap|t|-z5 $IN/wrap.ndjson|"
  # felt's own test option sets (tests/*/out), with the seed flags
  "grid-aligned|ts|-q -a@ -z11 -D7 --grid-low-zooms $T/grid-aligned/in.json|"
  "grid-unaligned|ts|-q -a@ -z11 -D7 --grid-low-zooms $T/grid-unaligned/in.json|"
  "islands-pT|ts|-q -a@ -d7 -z7 --no-tiny-polygon-reduction-at-maximum-zoom -pp $T/islands/in.json|"
  "islands-pp|ts|-q -a@ -d7 -z7 -pp $T/islands/in.json|"
  "islands-pt|ts|-q -a@ -d7 -z7 -pt -pp $T/islands/in.json|"
  "countries-tiny100|ts|-q -a@ -z0 --tiny-polygon-size 100 $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-tiny50-S50|ts|-q -a@ -z0 --tiny-polygon-size 50 --simplification 50 $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-aL-D8|ts|-q -a@ -z4 --grid-low-zooms -D8 $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-pt|ts|-q -a@ -z4 --no-tiny-polygon-reduction $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-S4|ts|-q -a@ -z4 -S4 $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-S4-smz2|ts|-q -a@ -z4 -S4 --simplification-at-maximum-zoom 2 $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-S4-av|ts|-q -a@ -z4 -S4 --visvalingam $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-pD|ts|-q -a@ -z4 -pD $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-pc|ts|-q -a@ -z4 -pc $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-d8|ts|-q -a@ -z3 -d8 $T/ne_110m_admin_0_countries/in.json.gz|"
  "states-ps|ts|-q -a@ -Z1 -z7 -b4 -ps $T/ne_110m_admin_1_states_provinces_lines/in.json|"
  "states-pS-d8-D16|ts|-q -a@ -z5 -pS -d8 -D16 $T/ne_110m_admin_1_states_provinces_lines/in.json|"
  "county-pp|ts|-q -a@ -z8 -pp $T/tl_2015_us_county/piscataquis.json $T/tl_2015_us_county/somerset.json|"
  "curve-pc|ts|-q -a@ -z2 --no-clipping $T/curve/in.json|"
  "curve-pD|ts|-q -a@ -z2 --no-duplication $T/curve/in.json|"
  "muni-aR|ts|-q -a@ -Z11 -z11 --prefer-radix-sort $T/muni/muni.json|"
  "aR|ts|-q -a@ -z8 -aR $T/tl_2015_us_county/piscataquis.json $T/tl_2015_us_county/somerset.json|"
  "bathymetry-aw|ts|-q -a@ -z0 --detect-longitude-wraparound $T/bathymetry-6000/in.json|"
  "wraparound-aw|ts|-q -a@ -z5 --detect-longitude-wraparound $T/wraparound/in.json|"
  "wraparound2-aw|ts|-q -a@ -z0 --detect-longitude-wraparound $T/wraparound2/in.json|"
  "winding|ts|-q -a@ -z0 $T/polygon-winding/in.json|"
  "winding-pW|ts|-q -a@ -z0 --reverse-source-polygon-winding $T/polygon-winding/in.json|"
  "winding-pw|ts|-q -a@ -z0 --use-source-polygon-winding $T/polygon-winding/in.json|"
  "countries-clip|ts|-q -a@ -z0 --clip-bounding-box -110,27,-92,52 $T/ne_110m_admin_0_countries/in.json.gz|"
  "states-clip|ts|-q -a@ -z0 --clip-bounding-box -110,27,-92,52 $T/ne_110m_admin_1_states_provinces_lines/in.json|"
  "places-clip|ts|-q -a@ -z0 --clip-bounding-box -110,27,-92,52 $T/ne_110m_populated_places/in.json|"
  "countries-clip2|t|-z3 --clip-bounding-box=-110,27,-92,52 --clip-bounding-box=-100,20,0,40 $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-aP|tn|-q -a@ -z2 --convert-polygons-to-label-points $T/ne_110m_admin_0_countries/in.json.gz|"
  "pub-poly-aP|tn|-Z8 -z15 -aP $IN/polygons-nominzoom.ndjson|"
  "pub-poly-aP-minzoom|tn|-Z10 -z15 -aP $IN/polygons.ndjson $IN/points.ndjson|"
  "edge-aP|tn|-q -z14 -aP $IN/edge.ndjson|"
  "wrap-aP|ts|-q -z5 -aP -aw $IN/wrap.ndjson|"
  "intersection-pn|ts|-q -a@ -Z11 -z11 --extra-detail 13 --no-simplification-of-shared-nodes $T/intersection/in.json|"
  "countries-pn|ts|-q -a@ -z1 --no-simplification-of-shared-nodes $T/ne_110m_admin_0_countries/in.json.gz|"
  "roads-pn|ts|-q -a@ -Z11 -z11 --no-simplification-of-shared-nodes $T/tl_2018_51685_roads/in.json|"
  "countries-pn-z4|t|-z4 -pn $T/ne_110m_admin_0_countries/in.json.gz|"
  "states-pn|t|-z6 -pn $T/ne_110m_admin_1_states_provinces_lines/in.json|"
  "pub-lines-pn|t|-Z8 -z14 -pn $IN/lines.ndjson|"
  "pub-poly-pn|t|-Z10 -z15 -pn $IN/polygons-nominzoom.ndjson|"
  "edge-pn|ts|-q -a@ -z14 -pn $IN/edge.ndjson|"
  "edge-pn-pc|t|-z13 -pn -pc $IN/edge.ndjson|"
  "edge-pn-pD|t|-z13 -pn -pD $IN/edge.ndjson|"
  "edge-pn-av|t|-z14 -pn -av $IN/edge.ndjson|"
  "edge-pn-aP|tn|-z14 -pn -aP $IN/edge.ndjson|"
  "wrap-pn|t|-z5 -pn $IN/wrap.ndjson|"
  "countries-pn-clip|t|-z3 -pn --clip-bounding-box=-110,27,-92,52 $T/ne_110m_admin_0_countries/in.json.gz|"
  "border-ab|ts|-q -a@ -z1 --detect-shared-borders $T/border/in.json|"
  "border-ab-z5|t|-z5 -ab $T/border/in.json|"
  "countries-ab|ts|-q -a@ -z4 -ab $T/ne_110m_admin_0_countries/in.json.gz|"
  "county-ab|t|-z9 -ab $T/tl_2015_us_county/piscataquis.json $T/tl_2015_us_county/somerset.json|"
  "tract-ab|t|-Z10 -z12 -ab $T/tl_2022_11_tract/in.json.gz|"
  "pub-poly-ab|t|-Z10 -z15 -ab $IN/polygons-nominzoom.ndjson $IN/lines.ndjson|"
  "edge-ab|ts|-q -a@ -z14 -ab $IN/edge.ndjson|"
  "edge-ab-av|t|-z14 -ab -av $IN/edge.ndjson|"
  "edge-ab-pn|t|-z14 -ab -pn $IN/edge.ndjson|"
  "edge-ab-ps|t|-z14 -ab -ps -pt $IN/edge.ndjson|"
  "edge-ab-aL|t|-z14 -ab -aL $IN/edge.ndjson|"
  "edge-ab-pc|t|-z13 -ab -pc $IN/edge.ndjson|"
  "countries-ab-y|ts|-q -a@ -z3 -ab -yname -yadmin $T/ne_110m_admin_0_countries/in.json.gz|"
  "pub-lines-pn-E|t|-Z9 -z13 -pn -S2 -Eosm_id:max $IN/lines.ndjson|"
  "edge-aP-E|tn|-z13 -aP -pt -Ecell:sum $IN/edge.ndjson|"
  "edge-pw|t|-z14 -pw $IN/edge.ndjson|"
  "edge-pW|t|-z14 -pW $IN/edge.ndjson|"
  "wrap-aw|ts|-q -a@ -z5 -aw $IN/wrap.ndjson|"
  "wrap-aw-pc|t|-z4 -aw -pc $IN/wrap.ndjson|"
  "pub-clip|t|-Z10 -z15 --clip-bounding-box=139.6,35.6,139.8,35.75 $IN/polygons.ndjson $IN/lines.ndjson|"
  "pub-clip-3857|t|-Z5 -z12 -s EPSG:3857 --clip-bounding-box=15500000,4200000,15600000,4300000 $T/epsg-3857/in.json|"
  # Public subsets and generated edge cases
  "pub-poly-a@|ts|-q -a@ -Z10 -z16 $IN/polygons.ndjson|"
  "pub-poly-d10-D9|t|-Z10 -z16 -d10 -D9 $IN/polygons.ndjson|"
  "pub-poly-d14|t|-Z12 -z16 -d14 $IN/polygons.ndjson|"
  "pub-poly-extra|t|-Z12 -z16 --extra-detail=14 $IN/polygons.ndjson|"
  "pub-poly-extra30|t|-Z12 -z14 --extra-detail=30 $IN/polygons.ndjson|"
  "pub-poly-extra8|t|-Z12 -z14 --extra-detail=8 $IN/polygons.ndjson|"
  "pub-poly-m|t|-Z12 -z14 -m5 -d10 $IN/polygons.ndjson|"
  "pub-poly-S3|t|-Z10 -z16 -S3 $IN/polygons.ndjson|"
  "pub-poly-S3-smz|t|-Z10 -z16 -S3 --simplification-at-maximum-zoom=0.5 $IN/polygons.ndjson|"
  "pub-poly-ps|t|-Z10 -z15 -ps $IN/polygons.ndjson|"
  "pub-poly-pS|t|-Z10 -z15 -pS $IN/polygons.ndjson|"
  "pub-poly-pt|t|-Z8 -z14 -pt $IN/polygons-nominzoom.ndjson|"
  "pub-poly-pT|t|-Z8 -z14 -pT $IN/polygons-nominzoom.ndjson|"
  "pub-poly-tiny9|t|-Z8 -z14 --tiny-polygon-size=9 $IN/polygons-nominzoom.ndjson|"
  "pub-poly-av|t|-Z8 -z15 -av -S5 $IN/polygons-nominzoom.ndjson|"
  "pub-poly-aL|t|-Z8 -z14 -aL $IN/polygons-nominzoom.ndjson|"
  "pub-poly-pc|t|-Z10 -z14 -pc $IN/polygons-nominzoom.ndjson|"
  "pub-poly-pD|t|-Z10 -z14 -pD $IN/polygons-nominzoom.ndjson|"
  "pub-lines-S2|t|-Z8 -z14 -S2 $IN/lines.ndjson|"
  "pub-lines-av|t|-Z8 -z14 -av $IN/lines.ndjson|"
  "pub-lines-ps|t|-Z10 -z13 -ps $IN/lines.ndjson|"
  "pub-lines-aL|t|-Z8 -z13 -aL -D9 $IN/lines.ndjson|"
  "pub-lines-pc|t|-Z9 -z13 -pc $IN/lines.ndjson|"
  "pub-lines-pD|t|-Z9 -z13 -pD $IN/lines.ndjson|"
  "pub-lines-extra|t|-Z11 -z13 --extra-detail 15 $IN/lines.ndjson|"
  "pub-points-pD|t|-Z5 -z12 -pD $IN/points.ndjson|"
  "pub-points-pc|t|-Z5 -z12 -pc $IN/points.ndjson|"
  "pub-points-extra|t|-Z5 -z12 --extra-detail 16 $IN/points.ndjson|"
  "edge-a@|ts|-q -a@ -z14 $IN/edge.ndjson|"
  "edge-av|t|-z14 -av $IN/edge.ndjson|"
  "edge-aL|t|-z14 -aL $IN/edge.ndjson|"
  "edge-pt-pS|t|-z14 -pt -pS $IN/edge.ndjson|"
  "edge-pc|t|-z13 -pc $IN/edge.ndjson|"
  "edge-pD|t|-z13 -pD $IN/edge.ndjson|"
  "edge-d4-D3|t|-z14 -d4 -D3 $IN/edge.ndjson|"
  "edge-extra|t|-z14 --extra-detail=18 $IN/edge.ndjson|"
  "wrap|ts|-q -a@ -z5 $IN/wrap.ndjson|"
  "wrap-pc|t|-z4 -pc $IN/wrap.ndjson|"
  "wrap-pD|t|-z4 -pD $IN/wrap.ndjson|"
  # Command line errors
  "e-D|e|-D31 -o x.mbtiles|"
)

norm_gen() { sed -E 's/^(generator\|text\|).*/\1GEN/; s/^("generator": ).*/\1"GEN",/'; }

run_one() { # dir binary how args stdin
  local d=$1 bin=$2 how=$3 args=$4 stdin=$5
  mkdir -p "$d/bin" && cd "$d" || return
  ln -sf "$bin" bin/tippecanoe
  # The same thread count for both: with --read-parallel, tippecanoe splits the
  # input into that many segments, and the result depends on it.
  local env=(TIPPECANOE_MAX_THREADS="$FELT_THREADS")
  local out=""
  case $how in
    t|ts|tn) out="$H -f -o out.mbtiles" ;;
  esac
  local cmd="tippecanoe $args $out"
  case $stdin in
    "<"*) cmd="$cmd <${stdin#<}" ;;
    pipe:*) cmd="cat ${stdin#pipe:} | $cmd" ;;
    *) cmd="$cmd </dev/null" ;;
  esac
  env "${env[@]}" PATH="$d/bin:$PATH" taskset -c "$CPUS" bash -c "$cmd" >stdout.txt 2>stderr.txt
  echo $? >exit.txt
}

fail=0
pass=0
known=0
want=("$@")
for c in "${CASES[@]}"; do
  IFS='|' read -r name how args stdin <<<"$c"
  if [ ${#want[@]} -gt 0 ] && [[ ! " ${want[*]} " == *" $name "* ]]; then continue; fi
  rm -rf "${WORK:?}/$name"
  (run_one "$WORK/$name/felt" "$FELT" "$how" "$args" "$stdin")
  (run_one "$WORK/$name/bt" "$BT" "$how" "$args" "$stdin")
  F=$WORK/$name/felt B=$WORK/$name/bt
  ok=1
  if ! cmp -s "$F/exit.txt" "$B/exit.txt"; then
    echo "  $name: exit code felt $(cat "$F/exit.txt") bt $(cat "$B/exit.txt")"; ok=0
  fi
  if [ "$how" = e ] || [ "$how" = ts ]; then
    # vtmake's version line names vtmake too.
    # The temporary file name is random.
    sed -E 's/pool\.[A-Za-z0-9]{8}/pool.XXXXXXXX/' "$F/stderr.txt" >"$F/stderr.norm"
    sed -E 's/^(tippecanoe v[0-9.]+) \(vtmake v[0-9.]+\)$/\1/' "$B/stderr.txt" >"$B/stderr.norm"
    cmp -s "$F/stderr.norm" "$B/stderr.norm" || { echo "  $name: stderr differs"; diff "$F/stderr.norm" "$B/stderr.norm" | head -5; ok=0; }
  fi
  tiles=""
  if [ -f "$F/out.mbtiles" ] || [ -f "$B/out.mbtiles" ]; then
    q="select name, typeof(value), value from metadata order by rowid"
    for d in "$F" "$B"; do
      if [ -f "$d/out.mbtiles" ]; then sqlite3 "$d/out.mbtiles" "$q" 2>&1 | norm_gen >"$d/meta.txt"; else echo "(no out.mbtiles)" >"$d/meta.txt"; fi
    done
    diff "$F/meta.txt" "$B/meta.txt" >"$WORK/$name/meta.diff" || { echo "  $name: metadata differs ($WORK/$name/meta.diff)"; ok=0; }
    if [ -f "$F/out.mbtiles" ] && [ -f "$B/out.mbtiles" ]; then
      taskset -c "$CPUS" "$CMP" "$F/out.mbtiles" "$B/out.mbtiles" --json "$WORK/$name/compare.json" >"$WORK/$name/compare.txt" 2>&1
      tiles=$(python3 -I -c 'import json,sys; t=json.load(open(sys.argv[1]))["tile_sets"]; print(t["a"], t["b"], t["byte_identical"], len(t["only_a"]), len(t["only_b"]))' "$WORK/$name/compare.json" 2>/dev/null)
      read -r ta tb ti oa ob <<<"${tiles:-x y z 1 1}"
      if [ "$ta" != "$tb" ] || [ "$ta" != "$ti" ] || [ "$oa" != 0 ] || [ "$ob" != 0 ]; then
        echo "  $name: tiles differ: felt $ta bt $tb byte-identical $ti only-felt $oa only-bt $ob"; ok=0
      fi
      tiles="tiles $ta"
    fi
  fi
  if [ -d "$F/outdir" ] || [ -d "$B/outdir" ]; then
    for d in "$F" "$B"; do
      if [ -f "$d/outdir/metadata.json" ]; then norm_gen <"$d/outdir/metadata.json"; else echo "(no metadata.json)"; fi >"$d/metadata.json"
    done
    diff "$F/metadata.json" "$B/metadata.json" >"$WORK/$name/metadata-json.diff" || { echo "  $name: metadata.json differs"; ok=0; }
    diff -r -x metadata.json "$F/outdir" "$B/outdir" >"$WORK/$name/dir.diff" 2>&1 || { echo "  $name: -e tiles differ"; ok=0; }
    tiles="dir tiles $(find "$F/outdir" -name '*.pbf' | wc -l)"
  fi
  if [ "$how" = tn ]; then
    echo "KNOWN $name (felt is not deterministic): felt $ta bt $tb byte-identical $ti"; known=$((known + 1)); continue
  fi
  if [ $ok = 1 ]; then
    echo "PASS $name (exit $(cat "$F/exit.txt")${tiles:+, $tiles})"; pass=$((pass + 1))
  else
    echo "FAIL $name"; fail=$((fail + 1))
  fi
done
echo "geom-check: pass $pass fail $fail known $known"
[ $fail = 0 ]
