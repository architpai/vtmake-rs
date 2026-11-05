#!/bin/bash
# Compare vtmake build with felt tippecanoe on the command line and the input
# formats: exit code, the mbtiles metadata
# rows, metadata.json, the tiles (vtmake-compare: byte-identical), and for the
# argument cases also stderr.
#
# Usage: bench/input-check.sh [case...]      (no case: all cases)
# Env:   FELT, VTMAKE, WORK, CPUS, FELT_THREADS as in bench/meta-check.sh
#        T  felt's test folder (no default; read only)
# Exit:  0 if all cases are equal, 1 if not.
set -uo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
FELT=${FELT:?set FELT to the felt tippecanoe binary}
VTMAKE=${VTMAKE:-$R/target/release/vtmake}
mkdir -p "$R/target/compat" && ln -sf "$VTMAKE" "$R/target/compat/tippecanoe"
BT=$R/target/compat/tippecanoe
WORK=${WORK:-${TMPDIR:-/tmp}/vtmake-input-check}
CPUS=${CPUS:-2-31}
FELT_THREADS=${FELT_THREADS:-2}
CMP=$R/target/release/vtmake-compare
T=${T:?set T to the tests folder of the felt source}
IN=$WORK/in
mkdir -p "$IN"
python3 -I "$R/bench/input-variants.py" "$T" "$IN" || exit 1
H="--drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit --no-tile-size-limit"

# name | how | arguments | standard input
# how: "t" (tiles: the seed flags ($H) and "-f -o out.mbtiles" are added), "te"
# (the same with "-f -e outdir"), "e" (an error: the exit code and stderr must
# be equal; nothing is added). Standard input: "<file" (a file) or "pipe:file" (a pipe).
CASES=(
  # GeoJSON forms
  "fc-pretty|t|-z5 $T/nullisland/in.json|"
  "fc-pretty-P|t|-P -z5 $T/nullisland/in.json|"
  "fc-crs-3857|t|-z4 -s EPSG:3857 $T/epsg-3857/in.json|"
  "fc-crs-3857-alias|t|-z4 --projection=urn:ogc:def:crs:EPSG::3857 $T/epsg-3857/in.json|"
  "fc-crs-warn|t|-z4 $T/epsg-3857/in.json|"
  "bare|t|-z5 $T/geometry/bare.json|"
  "gc|t|-z5 $T/geometry/geometrycollection.json|"
  "multipoint|t|-z5 $T/geometry/multipoint.json|"
  "nested|t|-z3 $T/nested/in.json|"
  "stringid|t|-z3 $T/stringid/in.json|"
  "stringid-aI|t|-z3 -aI $T/stringid/in.json|"
  "stringid-long|t|-z3 --convert-stringified-ids-to-numbers $T/stringid/in.json|"
  "dateline|t|-z5 $T/dateline/in.json|"
  "dateline-P|t|-z5 -P $T/dateline/in.json|"
  "longjson|t|-z3 $T/longjson/sherlock.json|"
  "onefeature|t|-z3 $T/onefeature/in.json|"
  "hackspots|t|-z9 $T/raw-tiles/hackspots.geojson|"
  "wineries|t|-z8 $T/wineries/in.json|"
  "wineries-P|t|-z8 -P $T/wineries/in.json|"
  "wraparound|t|-z5 $T/wraparound/in.json|"
  "geometry-all|t|-z4 $T/geometry/bare.json $T/geometry/geometrycollection.json $T/geometry/multipoint.json|"
  "nonascii|t|-z4 $T/nonascii/@@@.json $T/nonascii/堤防.json|"
  "high-longitude|t|-z5 $T/high-longitude/in.json|"
  "polygon-winding|t|-z5 $T/polygon-winding/in.json|"
  "invalid-polygon|t|-z5 $T/invalid-polygon/in.json|"
  "empty-linestring|t|-z5 $T/empty-linestring/in.json|"
  "too-few|t|-z5 $T/invalid-linestring/too-few.json|"
  "states-lines|t|-z5 $T/ne_110m_admin_1_states_provinces_lines/in.json|"
  "countries-gz|t|-z3 $T/ne_110m_admin_0_countries/in.json.gz|"
  "countries-gz-P|t|-z3 -P $T/ne_110m_admin_0_countries/in.json.gz|"
  "tract-gz|t|-Z10 -z11 $T/tl_2022_11_tract/in.json.gz|"
  "populated|t|-z6 $T/ne_110m_populated_places/in.json|"
  "populated-P|t|-z6 -P $T/ne_110m_populated_places/in.json|"
  # Generated variants (bench/input-variants.py)
  "v-pretty|t|-z6 $IN/places-pretty.json|"
  "v-pretty-P|t|-z6 -P $IN/places-pretty.json|"
  "v-fc-lines|t|-z6 $IN/places-fc-lines.json|"
  "v-fc-lines-P|t|-z6 -P $IN/places-fc-lines.json|"
  "v-fc-oneline|t|-z6 $IN/places-fc-oneline.json|"
  "v-rs|t|-z6 $IN/places-rs.json|"
  "v-rs-P|t|-z6 -P $IN/places-rs.json|"
  "v-ndjson|t|-z6 $IN/places.ndjson|"
  "v-ndjson-gz|t|-z6 $IN/places.ndjson.gz|"
  "v-ndjson-gz-P|t|-z6 -P $IN/places.ndjson.gz|"
  "v-ndjson-error|t|-z6 $IN/places-error.ndjson|"
  "v-ndjson-error-P|t|-z6 -P $IN/places-error.ndjson|"
  "v-mixed|t|-z6 $IN/mixed.json|"
  "v-mixed-P|t|-z6 -P $IN/mixed.json|"
  "v-bom|t|-z6 $IN/places-bom.ndjson|"
  "v-nested-feature|t|-z6 $IN/nested-feature.json|"
  "v-props-type|t|-z6 $IN/props-type.ndjson|"
  "v-unclosed|t|-z6 $IN/unclosed.json|"
  "v-numbers|t|-z6 $IN/numbers.ndjson|"
  "dir-input|t|-z3 $T/nested $T/multilayer/places.json|"
  "devnull|t|-z3 /dev/null $T/multilayer/places.json|"
  "empty-file|t|-z3 $IN/empty.json|"
  # Standard input
  "stdin-redirect|t|-z6|<$IN/places.ndjson"
  "stdin-pipe|t|-z6|pipe:$IN/places.ndjson"
  "stdin-pipe-P|t|-z6 -P|pipe:$IN/places.ndjson"
  "stdin-pipe-pretty|t|-z6|pipe:$IN/places-pretty.json"
  "stdin-pipe-pretty-P|t|-z6 -P|pipe:$IN/places-pretty.json"
  "stdin-name|t|-z6 -n named-from-n|<$IN/places.ndjson"
  "stdin-dash|t|-z6 -L x:$IN/places.ndjson|"
  # Layers, several inputs, metadata
  "two-files|t|-z5 $T/multilayer/places.json $T/multilayer/lines.json|"
  "two-files-l|t|-z5 -l one $T/multilayer/places.json $T/multilayer/lines.json|"
  "L-name|t|-z5 -L pl:$T/multilayer/places.json --named-layer=li:$T/multilayer/lines.json|"
  "L-json|t|-z5 -L '{\"file\":\"$T/multilayer/places.json\",\"layer\":\"p\",\"description\":\"Places\"}' -L '{\"file\":\"$T/multilayer/lines.json\",\"description\":\"lines here\"}'|"
  "L-same-layer|t|-z5 -L a:$T/multilayer/places.json -L a:$T/multilayer/lines.json|"
  "L-gz-layer|t|-z5 -L a:$T/multilayer/places.json -L a:$IN/places.ndjson.gz|"
  "L-missing|t|-z5 -L a:$T/multilayer/places.json -L b:/nonexistent.json|"
  "n-N-A|t|-z5 -n Name -N Description -A Attribution $T/multilayer/places.json|"
  "n-N-A-e|te|-z5 --name=Name --description=Description --attribution='<a href=x>A</a>' $T/multilayer/places.json|"
  "Y|t|-z5 -Y NAME:The\ name -Y SCALERANK:rank $T/multilayer/places.json|"
  "quiet|t|-z5 -q -Q -U 5 $T/multilayer/places.json|"
  # CSV and FlatGeobuf
  "csv|t|-z5 $T/csv/ne_110m_populated_places_simple.csv|"
  "csv-null|t|-z5 --empty-csv-columns-are-null $T/csv/ne_110m_populated_places_simple.csv|"
  "csv-pe|t|-z5 -pe $T/csv/ne_110m_populated_places_simple.csv|"
  "csv-stdin|t|-z5 -L '{\"file\":\"\",\"format\":\"csv\",\"layer\":\"ne\"}'|<$T/csv/ne_110m_populated_places_simple.csv"
  "csv-quotes|t|-z5 $IN/quotes.csv|"
  "csv-nocols|t|-z5 $IN/nocols.csv|"
  "csv-mismatch|t|-z5 $IN/mismatch.csv|"
  "fgb|t|-z5 $T/flatgeobuf/numeric-properties.fgb|"
  "fgb-L|t|-z5 -L '{\"file\":\"$T/flatgeobuf/numeric-properties.fgb\",\"format\":\"fgb\",\"layer\":\"n\"}'|"
  "fgb-3857|t|-z5 -s EPSG:3857 $T/flatgeobuf/numeric-properties.fgb|"
  "fgb-points|t|-z5 $IN/points.fgb|"
  "fgb-points-noindex|t|-z5 $IN/points-noindex.fgb|"
  "fgb-points-node4|t|-z5 $IN/points-node4.fgb|"
  "fgb-multipoint|t|-z5 $IN/multipoint.fgb|"
  "fgb-lines|t|-z5 $IN/lines.fgb|"
  "fgb-multilines|t|-z5 $IN/multilines.fgb|"
  "fgb-polygon|t|-z5 $IN/polygon.fgb|"
  "fgb-multipolygon|t|-z5 $IN/multipolygon.fgb|"
  "fgb-unknown|t|-z5 $IN/unknown.fgb|"
  "fgb-collection|t|-z5 $IN/collection.fgb|"
  "fgb-binary|t|-z5 $IN/binary.fgb|"
  "fgb-mercator|t|-z5 -s EPSG:3857 $IN/mercator.fgb|"
  "fgb-and-json|t|-z5 $IN/lines.fgb $T/multilayer/places.json|"
  # Command line errors
  "e-none|e||"
  "e-help|e|--help|"
  "e-unknown|e|--foo -o x.mbtiles|"
  "e-unknown-eq|e|--foo=bar -o x.mbtiles|"
  "e-ambiguous|e|--no-tile -o x.mbtiles|"
  "e-ambiguous-eq|e|--no-t=3 -o x.mbtiles|"
  "e-noarg|e|--force=1 -o x.mbtiles|"
  "e-needarg|e|--output|"
  "e-short-needarg|e|-o|"
  "e-invalid|e|-W -o x.mbtiles|"
  "e-tilde|e|-~x -o x.mbtiles|"
  "e-p|e|-pZ -o x.mbtiles|"
  "e-a|e|-aA -o x.mbtiles|"
  "e-no-output|e|in.json|"
  "e-both|e|-o a.mbtiles -e b|"
  "e-two-o|e|-o a.mbtiles -o b.mbtiles|"
  "e-zoom|e|-zx -o x.mbtiles|"
  "e-zoom-empty|e|-z '' -o x.mbtiles|"
  "e-minzoom|e|-Z5 -z3 -o x.mbtiles|"
  "e-buffer|e|-b200 -o x.mbtiles|"
  "e-detail|e|-d31 -o x.mbtiles|"
  "e-B|e|-Bx -o x.mbtiles|"
  "e-Bg0|e|-Bg0 -o x.mbtiles|"
  "e-rp|e|-rp -o x.mbtiles|"
  "e-K|e|-K300 -o x.mbtiles|"
  "e-S|e|-S0 -o x.mbtiles|"
  "e-L|e|-L foo -o x.mbtiles|"
  "e-L-json|e|-L '{\"layer\":\"x\"}' -o x.mbtiles|"
  "e-L-badjson|e|-L '{\"file\":' -o x.mbtiles|"
  "e-T|e|-Tname -o x.mbtiles|"
  "e-T-type|e|-Tname:double -o x.mbtiles|"
  "e-E|e|-Ename:avg -o x.mbtiles|"
  "e-Y|e|-Yname -o x.mbtiles|"
  "e-j|e|-j {x -o x.mbtiles|"
  "e-J|e|-J /nonexistent -o x.mbtiles|"
  "e-s|e|-s EPSG:1234 -o x.mbtiles|"
  "e-R|e|-R 1/2 -o x.mbtiles|"
  "e-clip|e|--clip-bounding-box=1,2,3 -o x.mbtiles|"
  "e-order|e|--drop-by-attribute-order=up -o x.mbtiles|"
  "e-dba|e|--drop-by-attribute-as-needed= -o x.mbtiles|"
  "e-smzg|e|--smallest-maximum-zoom-guess=30 -o x.mbtiles|"
  "e-extra|e|--extra-detail=31 -o x.mbtiles|"
  "e-dd|e|--drop-denser=101 -o x.mbtiles|"
  "e-set|e|--set-attribute=x -o x.mbtiles|"
  "e-set-json|e|--set-attribute={1:2} -o x.mbtiles|"
  "e-unidecode|e|--unidecode-data=/nonexistent -o x.mbtiles|"
  "e-sms|e|--simplification-at-maximum-zoom=0 -o x.mbtiles|"
  "e-tmpdir|e|-z3 -t /nonexistent/dir $H -f -o x.mbtiles $T/nullisland/in.json|"
  "e-version|e|--version|"
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
    t) out="$H -f -o out.mbtiles" ;;
    te) out="$H -f -e outdir" ;;
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
  if [ "$how" = e ]; then
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
  if [ $ok = 1 ]; then
    echo "PASS $name (exit $(cat "$F/exit.txt")${tiles:+, $tiles})"; pass=$((pass + 1))
  else
    echo "FAIL $name"; fail=$((fail + 1))
  fi
done
echo "input-check: pass $pass fail $fail"
[ $fail = 0 ]
