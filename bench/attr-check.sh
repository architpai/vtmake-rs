#!/bin/bash
# Compare vtmake build with felt tippecanoe on the attribute and filter options:
# exit code, the mbtiles metadata rows,
# metadata.json, the tiles (vtmake-compare: byte-identical), and for the
# error cases also stderr. The cases are felt's tests with these options (its
# inputs read in place, with the seed flags added), and generated inputs with
# all value types (bench/attr-inputs.py).
#
# Usage: bench/attr-check.sh [case...]      (no case: all cases)
# Env:   FELT, VTMAKE, WORK, CPUS, FELT_THREADS as in bench/meta-check.sh
#        T  felt's test folder (no default; read only)
# Exit:  0 if all cases are equal, 1 if not.
set -uo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
FELT=${FELT:?set FELT to the felt tippecanoe binary}
VTMAKE=${VTMAKE:-$R/target/release/vtmake}
mkdir -p "$R/target/compat" && ln -sf "$VTMAKE" "$R/target/compat/tippecanoe"
BT=$R/target/compat/tippecanoe
WORK=${WORK:-${TMPDIR:-/tmp}/vtmake-attr-check}
CPUS=${CPUS:-2-31}
FELT_THREADS=${FELT_THREADS:-2}
CMP=$R/target/release/vtmake-compare
T=${T:?set T to the tests folder of the felt source}
IN=$WORK/in
mkdir -p "$IN"
python3 -I "$R/bench/attr-inputs.py" "$T" "$IN" || exit 1
H="--drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit --no-tile-size-limit"
V=$IN/values.ndjson
D=$IN/dense.ndjson

# name | how | arguments | standard input
# how: "t" (tiles: the seed flags ($H) and "-f -o out.mbtiles" are added), "te"
# (the same with "-f -e outdir"), "e" (an error: the exit code and stderr must
# be equal; nothing is added), "ts" (as "t", and the kinds of the warnings must
# be equal: vtmake reads the lines in another order, so a warning that
# tippecanoe writes once can name another feature).
CASES=(
  # felt's tests (without -a@, -r, -K, --retain-points-multiplier: not in this check)
  "f-set-comma|t|--set-attribute thecomma:NEWVALUE --accumulate-attribute thecomma:comma $T/accumulate/in.json|"
  "f-set-json|t|-z0 --set-attribute '{\"num\":5,\"str\":\"abc\"}' $T/accumulate/in.json|"
  "f-set-two|t|-z0 --set-attribute num:5 --set-attribute str:abc $T/accumulate/in.json|"
  "f-acc-json|t|-z3 --accumulate-attribute '{\"thesum\":\"sum\",\"theproduct\":\"product\"}' $T/accumulate/in.json|"
  "f-acc-all|t|-z5 -Ethesum:sum -Etheproduct:product -Ethemax:max -Ethemin:min -Ethemean:mean -Etheconcat:concat -Ethecomma:comma $T/accumulate/in.json|"
  "f-acc-all-z3|t|-z3 -Ethesum:sum -Etheproduct:product -Ethemax:max -Ethemin:min -Ethemean:mean -Etheconcat:concat -Ethecomma:comma $T/accumulate/in.json|"
  "f-T|t|-z0 -Tinttype:int -Tfloattype:float -Tbooltype:bool -Tstringtype:string $T/attribute-type/in.json|"
  "f-pN|t|-z0 -pN $T/attribute-type/in.json|"
  "f-y-3857|t|-yNAME -z5 -sEPSG:3857 $T/epsg-3857/in.json|"
  "f-J|t|-z0 -J$T/feature-filter/filter $T/feature-filter/in.json|"
  "f-J-z3|t|-z3 -J$T/feature-filter/filter $T/feature-filter/in.json|"
  "f-longattr|t|-z0 --maximum-string-attribute-length 20 $T/longattr/sherlock.json|"
  "f-mean|t|-z1 --accumulate-attribute value:mean $T/mean-accumulation/in.json|"
  "f-ai|t|-z3 -ai $T/ne_110m_admin_0_countries/in.json.gz|"
  "f-ai-P|t|-z3 -ai -P $T/ne_110m_admin_0_countries/in.json.gz|"
  "f-yname|t|-z4 -yname $T/ne_110m_admin_0_countries/in.json.gz|"
  "f-X|t|-X -z4 $T/ne_110m_admin_1_states_provinces_lines/in.json|"
  "f-x|t|-lcountries -P -Z1 -z7 -b4 -xfeaturecla -xscalerank $T/ne_110m_admin_1_states_provinces_lines/in.json|"
  "f-ymap|t|-z5 -ymapcolor13 -ymapcolor9 $T/ne_110m_admin_1_states_provinces_lines/in.json|"
  "f-J-places|t|-z8 -J $T/feature-filter/places-filter $T/ne_110m_populated_places/in.json|"
  "f-yNAME|t|-yNAME $T/ne_110m_populated_places/in.json|"
  "f-yNAME-z5|t|-yNAME -z5 $T/ne_110m_populated_places/in.json|"
  "f-y-Y|t|-z0 -yNAME -ySOV0NAME -yELEVATION -YNAME:City -YSOV0NAME:Country $T/ne_110m_populated_places/in.json|"
  "f-minzoom|t|-yNAME -ySCALERANK -z5 $T/ne_110m_populated_places-minzoom/in.json|"
  "f-nulls-mean|t|-z1 --accumulate-attribute POP2000:mean -yNAME -yPOP2000 $T/ne_110m_populated_places_nulls/in.json|"
  "f-id|ts|-q -z0 --use-attribute-for-id special $T/stringid/in.json|"
  "f-id-X|ts|-q -z0 --use-attribute-for-id special -X $T/stringid/in.json|"
  "f-id-x|ts|-q -z0 --use-attribute-for-id special -xspecial $T/stringid/in.json|"
  "f-id-y|ts|-q -z0 --use-attribute-for-id special -yother $T/stringid/in.json|"
  "f-id-aI|ts|-q -z0 -aI --use-attribute-for-id special $T/stringid/in.json|"
  "f-muni|t|-z11 -ycount --set-attribute count:1 --accumulate-attribute count:sum $T/muni/muni.json|"
  "f-layer-json|t|-z0 -yNAME -L'{\"file\":\"$T/ne_110m_populated_places/in.json\", \"description\":\"World cities\", \"layer\":\"places\"}'|"
  # Generated inputs: all value types
  "g-plain|t|-z6 $V|"
  "g-T-int|t|-z3 -Ts:int -Tn:int -Tb:int -Tz:int -Tmixed:int -Tmixed2:int $V|"
  "g-T-float|t|-z3 -Ts:float -Tn:float -Tb:float -Tz:float -Tmixed:float -Tmixed2:float $V|"
  "g-T-bool|t|-z3 -Ts:bool -Tn:bool -Tb:bool -Tz:bool -Tmixed:bool -Tmixed2:bool $V|"
  "g-T-string|t|-z3 -Ts:string -Tn:string -Tb:string -Tz:string -Tmixed:string -Tmixed2:string $V|"
  "g-T-first|t|-z3 -Tn:int -Tn:string $V|"
  "g-pN|t|-z3 -pN $V|"
  "g-pN-T|t|-z3 --single-precision -Ts:float -Tmixed:float $V|"
  "g-id|ts|-q -z3 --use-attribute-for-id special $V|"
  "g-id-aI|ts|-q -z3 -aI --use-attribute-for-id special $V|"
  "g-id-T|ts|-q -z3 --use-attribute-for-id rank -Trank:float $V|"
  "g-id-n|ts|-q -z3 --use-attribute-for-id n $V|"
  "g-ai|t|-z3 -ai $V|"
  "g-ai-P|t|-z3 -ai -P $V|"
  "g-ai-id|t|-z3 -ai --use-attribute-for-id special $V|"
  "g-ai-rs|t|-z3 -ai $IN/values-rs.json|"
  "g-ai-gz|t|-z3 -ai $IN/values.ndjson.gz|"
  "g-ai-gz-P|t|-z3 -ai -P $IN/values.ndjson.gz|"
  "g-ai-pipe|t|-z3 -ai|pipe:$V"
  "g-ai-pipe-P|t|-z3 -ai -P|pipe:$V"
  "g-ai-redirect-P|t|-z3 -ai -P|<$V"
  "g-ai-files|t|-z3 -ai $V $D $IN/dense.csv $IN/many.fgb $IN/values-rs.json|"
  "g-ai-files-P|t|-z3 -ai -P $V $D $IN/dense.csv $IN/many.fgb $IN/values-rs.json|"
  "g-ai-fgb|t|-z3 -ai $IN/many.fgb|"
  "g-ai-fgb-idx|t|-z3 -ai $IN/points-node4.fgb $IN/points-noindex.fgb|"
  "g-ai-csv|t|-z3 -ai $IN/dense.csv|"
  "g-ai-geojson|t|-z3 -ai $IN/places-pretty.json $IN/mixed.json|"
  "g-ai-geojson-P|t|-z3 -ai -P $IN/places-pretty.json $IN/mixed.json $IN/places-fc-lines.json|"
  "g-y|t|-z3 -ys -yn -ycat $V|"
  "g-x|t|-z3 -xs -xn -xmixed $V|"
  "g-X|t|-z3 -X $V|"
  "g-X-x|t|-z3 -X -ycat -xcat $V|"
  "g-include-long|t|-z3 --include=cat --include=rank --exclude=rank $V|"
  "g-set|t|-z3 --set-attribute cat:new --set-attribute added:-5 --set-attribute zzz:9x $V|"
  "g-set-json|t|-z3 --set-attribute '{\"cat\":\"j\",\"num\":1.50,\"t\":true,\"f\":false,\"nul\":null,\"obj\":{\"a\":[1,2]},\"arr\":[\"x\"],\"big\":12345678901234567890}' $V|"
  "g-set-T|t|-z3 --set-attribute rank:1.5 -Trank:int -Tadded:bool --set-attribute added:0 $V|"
  "g-set-x|t|-z3 --set-attribute a1:1 --set-attribute a2:x -xa1 $V|"
  "g-maxlen|t|-z3 --maximum-string-attribute-length 5 $V|"
  "g-maxlen-1|t|-z3 --maximum-string-attribute-length=1 $V|"
  "g-maxlen-T|t|-z3 --maximum-string-attribute-length 3 -Tn:string -Tmixed:string $V|"
  "g-unidecode|t|-z3 --unidecode-data=$IN/values.ndjson $V|"
  # Filters
  "j-eq|t|-z4 -j '{\"*\":[\"==\",\"cat\",\"a\"]}' $V|"
  "j-ops|t|-z4 -j '{\"*\":[\"any\",[\"<\",\"n\",1],[\">=\",\"rank\",8],[\"<=\",\"s\",\"a\"],[\">\",\"s\",\"x\"]]}' $V|"
  "j-type|t|-z4 -j '{\"*\":[\"==\",\"\$type\",\"Polygon\"]}' $V|"
  "j-type-ne|t|-z4 -j '{\"*\":[\"!=\",\"\$type\",\"Point\"],\"one\":[\"==\",\"b\",true]}' $V|"
  "j-zoom|t|-z6 -j '{\"*\":[\"any\",[\">=\",\"\$zoom\",4],[\"==\",\"cat\",\"b\"]]}' $V|"
  "j-zoom-rank|t|-z6 -j '{\"*\":[\"<=\",\"rank\",[\"\$zoom\"]]}' $V|"
  "j-id|t|-z4 -j '{\"*\":[\"any\",[\"<\",\"\$id\",500],[\"!has\",\"\$id\"]]}' $V|"
  "j-in|t|-z4 -j '{\"*\":[\"in\",\"cat\",\"a\",\"c\",5,true]}' $V|"
  "j-notin|t|-z4 -j '{\"*\":[\"!in\",\"mixed\",\"\",0,1,true,null]}' $V|"
  "j-has|t|-z4 -j '{\"*\":[\"all\",[\"has\",\"extra\"],[\"!has\",\"nothing\"]]}' $V|"
  "j-none|t|-z4 -j '{\"two\":[\"none\",[\"==\",\"cat\",\"a\"],[\"==\",\"b\",false]]}' $V|"
  "j-null|t|-z4 -j '{\"*\":[\"any\",[\"==\",\"z\",null],[\"==\",\"mixed\",null]]}' $V|"
  "j-bool|t|-z4 -j '{\"*\":[\"any\",[\"==\",\"b\",true],[\"<\",\"mixed\",true]]}' $V|"
  "j-mismatch|t|-z4 -j '{\"*\":[\"any\",[\"==\",\"s\",1],[\"!=\",\"n\",\"x\"],[\"<\",\"b\",0]]}' $V|"
  "j-literals|t|-z4 -j '{\"*\":[\"all\",true,1,\"x\",null],\"one\":false,\"two\":[\"any\",0,\"\"]}' $V|"
  "j-attr-filter|t|-z5 -j '{\"*\":[\"all\",[\"attribute-filter\",\"s\",[\">=\",\"\$zoom\",3]],[\"attribute-filter\",\"mixed\",[\"==\",\"cat\",\"a\"]]]}' $V|"
  "j-attr-filter-E|t|-z5 -Enum:sum -Ename:comma -Ecount:sum -j '{\"*\":[\"attribute-filter\",\"name\",[\">=\",\"\$zoom\",3]]}' $D|"
  "j-E|t|-z5 -Enum:sum -Ecount:sum -j '{\"*\":[\"!=\",\"name\",\"Beta\"]}' $D|"
  "j-y|t|-z4 -ycat -j '{\"*\":[\"==\",\"rank\",3]}' $V|"
  "j-dense|t|-z6 -j '{\"*\":[\"any\",[\">\",\"num\",0],[\"==\",\"flag\",true]]}' $D|"
  "J-file|t|-z4 -J $T/feature-filter/places-filter $V|"
  # -E with dot dropping (the dense points)
  "E-sum|t|-z6 -Enum:sum -Ecount:sum $D|"
  "E-all|t|-z6 -Enum:sum -Emean:product -Elabel:concat -Ename:comma -Emean:mean -En2:max -Eflag:min $D|"
  "E-mean|t|-z6 -Emean:mean -Enum:mean $D|"
  "E-count|t|-z6 -Ecount:count -Elabel:count -Enum:count $D|"
  "E-max-min|t|-z6 -Enum:max -Emean:min -Ename:max -Eflag:max $D|"
  "E-y|t|-z6 -ycount -ynum -Ecount:sum $D|"
  "E-set|t|-z6 --set-attribute count:1 -Ecount:sum -Elabel:comma $D|"
  "E-T|t|-z6 -Tnum:string -Enum:concat -Tflag:int -Eflag:sum $D|"
  "E-json|t|-z6 --accumulate-attribute '{\"num\":\"sum\",\"label\":\"comma\"}' $D|"
  "E-maxlen|t|-z6 --maximum-string-attribute-length 4 -Elabel:comma -Ename:concat $D|"
  "E-values|t|-z5 -Emixed:max -Emixed2:min -Erank:sum -Es:comma -Ecat:concat -Eb:count -En:mean $V|"
  "E-csv|t|-z6 -Enum:sum -Ename:comma $IN/dense.csv|"
  "E-ai|t|-z6 -ai -Enum:sum $D|"
  "E-e|te|-z6 -Enum:sum -Ecount:sum $D|"
  # Errors
  "e-j-not-hash|e|-q -z3 -j '[\"==\",\"a\",1]' $H -f -o x.mbtiles $V|"
  "e-j-not-array|e|-q -z3 -j '{\"*\":{\"a\":1}}' $H -f -o x.mbtiles $V|"
  "e-j-empty|e|-q -z3 -j '{\"*\":[]}' $H -f -o x.mbtiles $V|"
  "e-j-op|e|-q -z3 -j '{\"*\":[1,2]}' $H -f -o x.mbtiles $V|"
  "e-j-unknown|e|-q -z3 -j '{\"*\":[\"match\",\"a\"]}' $H -f -o x.mbtiles $V|"
  "e-j-has|e|-q -z3 -j '{\"*\":[\"has\",1]}' $H -f -o x.mbtiles $V|"
  "e-j-nothas|e|-q -z3 -j '{\"*\":[\"!has\",\"a\",\"b\"]}' $H -f -o x.mbtiles $V|"
  "e-j-cmp|e|-q -z3 -j '{\"*\":[\"==\",\"a\"]}' $H -f -o x.mbtiles $V|"
  "e-j-cmpkey|e|-q -z3 -j '{\"*\":[\"<\",3,\"a\"]}' $H -f -o x.mbtiles $V|"
  "e-j-in|e|-q -z3 -j '{\"*\":[\"in\"]}' $H -f -o x.mbtiles $V|"
  "e-j-inkey|e|-q -z3 -j '{\"*\":[\"!in\",null,1]}' $H -f -o x.mbtiles $V|"
  "e-j-af|e|-q -z3 -j '{\"*\":[\"attribute-filter\",\"a\"]}' $H -f -o x.mbtiles $V|"
  "e-j-afkey|e|-q -z3 -j '{\"*\":[\"attribute-filter\",1,true]}' $H -f -o x.mbtiles $V|"
  "e-j-other-layer|t|-z3 -j '{\"nolayer\":[\"bad\"]}' $V|"
  "e-E-product-new|e|-q -z6 -Emissing:product $H -f -o x.mbtiles $D|"
  "e-E-concat-new|e|-q -z6 -En2:concat $H -f -o x.mbtiles $D|"
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
    t|ts) out="$H -f -o out.mbtiles" ;;
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
  if [ "$how" = ts ]; then
    for d in "$F" "$B"; do
      grep -a '^Warning' "$d/stderr.txt" | sed -E 's/(feature ID|as feature ID is not a number|Attribute) .*/\1/' | sort -u >"$d/warn.norm"
    done
    cmp -s "$F/warn.norm" "$B/warn.norm" || { echo "  $name: warnings differ"; diff "$F/warn.norm" "$B/warn.norm" | head -5; ok=0; }
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
echo "attr-check: pass $pass fail $fail"
[ $fail = 0 ]
