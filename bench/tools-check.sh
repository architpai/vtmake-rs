#!/bin/bash
# Compare `vtmake decode`, `vtmake join`, `vtmake enumerate`, `vtmake overzoom`
# and `vtmake json-tool` with felt, byte for byte. The PMTiles and enumerate
# cases are in bench/tools-pmtiles-check.py.
#
# The script makes tilesets (felt tippecanoe and vtmake build, -o and -e) from
# subsets of the public corpus and from the synthetic inputs of
# bench/meta-inputs.py. Then it runs each decode and join case with the felt
# tool and with vtmake, through symlinks with the felt names, so that both get
# the same argv. It compares:
# - decode: the standard output (bytes) and the exit code;
# - join: the exit code, the tiles (bytes) and the metadata rows of -o output,
#   or the tile files and metadata.json of -e output. The `generator` value is
#   the only permitted difference.
# - overzoom, json-tool: the exit code, the standard output and error, and
#   the output tile.
#
# Usage: bench/tools-check.sh
# Env:   FELT_DIR  the folder with the felt tools (no default)
#        FELT_SRC  felt's source, for its test tiles (no default)
#        P         the public corpus folder (points.ndjson, polygons.ndjson,
#                  points-plain.ndjson; no default)
#        PL        pub-lines.ndjson from bench/prep_lines.py (no default)
#        VTMAKE    our binary (default target/release/vtmake)
#        WORK      work folder (default ${TMPDIR:-/tmp}/vtmake-tools-check)
#        N         lines in each real-data subset (default 3000)
#        CPUS      taskset list (default 2-31)
# Exit:  0 if all cases are identical, 1 if not.
set -uo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
FELT_DIR=${FELT_DIR:?set FELT_DIR to the folder with the felt tools (tippecanoe, tile-join, ...)}
FELT_SRC=${FELT_SRC:?set FELT_SRC to the felt source tree}
VTMAKE=${VTMAKE:-$R/target/release/vtmake}
WORK=${WORK:-${TMPDIR:-/tmp}/vtmake-tools-check}
N=${N:-3000}
CPUS=${CPUS:-2-31}
PUB=${P:?set P to the public corpus folder (points.ndjson, polygons.ndjson, points-plain.ndjson)}
PL=${PL:?set PL to pub-lines.ndjson from bench/prep_lines.py}
IN=$WORK/in
TS=$WORK/ts
mkdir -p "$IN" "$TS" "$WORK/bin-felt" "$WORK/bin-vt"
for t in tippecanoe tile-join tippecanoe-decode tippecanoe-enumerate tippecanoe-overzoom tippecanoe-json-tool; do
  ln -sf "$FELT_DIR/$t" "$WORK/bin-felt/$t"
  ln -sf "$VTMAKE" "$WORK/bin-vt/$t"
done

# Inputs.
subset() { # name source lines
  [ -s "$IN/$1.ndjson" ] || { [ -f "$2" ] && head -n "$3" "$2" >"$IN/$1.ndjson"; }
}
subset pub-poly "$PUB/polygons.ndjson" "$N"
subset pub-points "$PUB/points.ndjson" "$N"
subset pub-lines "$PL" "$N"
# Many points in one tile: a gzip tile larger than 500000 bytes.
subset big "$PUB/points-plain.ndjson" 120000
python3 -I "$R/bench/meta-inputs.py" "$IN" || exit 1

# Tilesets: name | -Z | -z | layer ("" for none) | inputs (in IN)
SETS=(
  "pub-poly|12|16|bldg|pub-poly.ndjson"
  "pub-points|12|18|pois|pub-points.ndjson"
  "pub-lines|9|14|lines|pub-lines.ndjson"
  "types|12|16|types|types.ndjson"
  "multi|12|15|deflt|multi.ndjson"
  "multigeom|11|16|mg|multigeom.ndjson"
  "wide|12|14|wide|wide.ndjson"
  "numbers|9|12|num|numbers.ndjson"
  "two-files|12|16||multi.ndjson maxz-low.ndjson"
  "antimeridian|0|8|am|antimeridian.ndjson"
  "big|0|1|big|big.ndjson"
)
build() { # tool(felt|vt) mode(o|e) out Z z layer inputs...
  local tool=$1 mode=$2 out=$3 Z=$4 z=$5 layer=$6; shift 6
  local args=() lflag=()
  for f in "$@"; do args+=("$IN/$f"); done
  [ -n "$layer" ] && lflag=(--layer "$layer")
  env TIPPECANOE_MAX_THREADS=2 PATH="$WORK/bin-$tool:$PATH" taskset -c "$CPUS" tippecanoe -q -Z"$Z" -z"$z" --read-parallel --buffer=5 \
    --force --drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit --no-tile-size-limit \
    "${lflag[@]}" -"$mode" "$out" "${args[@]}" >/dev/null 2>&1
}
for s in "${SETS[@]}"; do
  IFS='|' read -r name Z z layer inputs <<<"$s"
  for tool in felt vt; do
    # shellcheck disable=SC2086
    [ -f "$TS/$name-$tool.mbtiles" ] || build $tool o "$TS/$name-$tool.mbtiles" "$Z" "$z" "$layer" $inputs
    # shellcheck disable=SC2086
    [ -d "$TS/$name-$tool.dir" ] || build $tool e "$TS/$name-$tool.dir" "$Z" "$z" "$layer" $inputs
  done
done
# A tile directory without metadata.json, as some scripts make it.
if [ ! -d "$TS/nometa.dir" ]; then
  cp -r "$TS/pub-poly-felt.dir" "$TS/nometa.dir" && rm "$TS/nometa.dir/metadata.json"
fi

rm -rf "$WORK/dec" "$WORK/join"
total=0
same=0
diffs=()
result() { # case ok(0|1)
  total=$((total + 1))
  if [ "$2" = 0 ]; then same=$((same + 1)); else diffs+=("$1"); echo "DIFF $1"; fi
}

# decode: case args...
dec() {
  local name=$1; shift
  local d=$WORK/dec/$name tool
  [ -e "$d" ] && { echo "duplicate case: $name"; exit 2; }
  mkdir -p "$d/felt" "$d/vt"
  for tool in felt vt; do
    (cd "$d/$tool" && env PATH="$WORK/bin-$tool:$PATH" taskset -c "$CPUS" tippecanoe-decode "$@" >out.json 2>err.txt; echo $? >rc.txt)
  done
  local ok=0
  cmp -s "$d/felt/rc.txt" "$d/vt/rc.txt" || ok=1
  cmp -s "$d/felt/out.json" "$d/vt/out.json" || ok=1
  result "decode $name" $ok
}

# The z/x/y of some tiles of an mbtiles file: first, middle and last.
some_tiles() {
  python3 -I -c '
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
t = list(c.execute("select zoom_level, tile_column, (1 << zoom_level) - 1 - tile_row from tiles order by zoom_level, tile_column, tile_row"))
for i in sorted({0, len(t) // 2, len(t) - 1}):
    print(*t[i])
' "$1"
}

for s in "${SETS[@]}"; do
  IFS='|' read -r name _ _ layer _ <<<"$s"
  for tool in felt vt; do
    m=$TS/$name-$tool.mbtiles
    dec "$name-$tool-whole" "$m"
    dec "$name-$tool-3857" -s EPSG:3857 "$m"
    dec "$name-$tool-c" -c "$m"
    dec "$name-$tool-S" -S "$m"
    while read -r z x y; do
      [ -n "$z" ] || continue
      dec "$name-$tool-$z-$x-$y" "$m" "$z" "$x" "$y"
      dec "$name-$tool-$z-$x-$y-3857" -s EPSG:3857 "$m" "$z" "$x" "$y"
      dec "$name-$tool-$z-$x-$y-c" -c "$m" "$z" "$x" "$y"
      # A child tile below the tileset: tippecanoe-decode uses the parent.
      dec "$name-$tool-$z-$x-$y-child" "$m" $((z + 2)) $((x * 4 + 1)) $((y * 4 + 2))
      p=$TS/$name-$tool.dir/$z/$x/$y.pbf
      [ -f "$p" ] && dec "$name-$tool-$z-$x-$y-pbf" -s EPSG:3857 "$p" "$z" "$x" "$y"
    done < <(some_tiles "$m")
  done
  dec "$name-felt-dir" "$TS/$name-felt.dir"
  dec "$name-vt-dir-3857" -s EPSG:3857 "$TS/$name-vt.dir"
done
m=$TS/types-felt.mbtiles
dec "types-Zz" -Z13 -z14 "$m"
dec "types-l" -l types "$m"
dec "types-l-none" -l nothing "$m"
dec "types-x" -x generator -x json "$m"
dec "types-y" -y int -y uni "$m"
dec "types-F" -F "$m"
dec "types-I" -I "$m"
dec "types-f" -f "$m"
dec "types-urn" -s urn:ogc:def:crs:EPSG::3857 "$m" 14 14549 6451
dec "multi-l" -l alpha -l Beta "$TS/multi-felt.mbtiles"
dec "multi-c-dir" -c "$TS/multi-vt.dir"
dec "multi-S-dir" -S -z13 "$TS/multi-vt.dir"
dec "nometa-dir" "$TS/nometa.dir"
dec "dir-zxy" "$TS/multi-felt.dir" 12 3638 1613
dec "err-missing" "$WORK/nonexistent.mbtiles"
dec "err-pbf-no-zxy" "$(find "$TS/types-felt.dir" -name '*.pbf' | head -1)"
dec "err-option" --nothing "$m"
dec "err-projection" -s EPSG:1234 "$m"
dec "err-no-args"

# join: case outmode(o|e) args... (the output is out.mbtiles or outdir)
join() {
  local name=$1 mode=$2; shift 2
  local d=$WORK/join/$name tool
  [ -e "$d" ] && { echo "duplicate case: $name"; exit 2; }
  mkdir -p "$d/felt" "$d/vt"
  local out=out.mbtiles
  [ "$mode" = e ] && out=outdir
  for tool in felt vt; do
    (cd "$d/$tool" && env PATH="$WORK/bin-$tool:$PATH" taskset -c "$CPUS" tile-join -q -f -"$mode" $out "$@" >out.txt 2>err.txt; echo $? >rc.txt)
  done
  python3 -I - "$d/felt" "$d/vt" <<'EOF'
import os, sqlite3, sys
a, b = sys.argv[1], sys.argv[2]
def norm_meta(rows):
    return [(k, t, "GEN" if k == "generator" else v) for k, t, v in rows]
def load(d):
    out = {"rc": open(os.path.join(d, "rc.txt")).read()}
    p = os.path.join(d, "out.mbtiles")
    if os.path.exists(p):
        c = sqlite3.connect(p)
        try:
            out["tiles"] = sorted(c.execute("select zoom_level, tile_column, tile_row, tile_data from tiles"))
            out["meta"] = norm_meta(list(c.execute("select name, typeof(value), value from metadata order by rowid")))
        except sqlite3.Error as e:
            out["error"] = str(e)
    p = os.path.join(d, "outdir")
    if os.path.isdir(p):
        files = {}
        for root, _, names in os.walk(p):
            for n in names:
                f = os.path.join(root, n)
                data = open(f, "rb").read()
                if n == "metadata.json":
                    data = b"\n".join(b'"generator": "GEN",' if l.startswith(b'"generator": ') else l for l in data.split(b"\n"))
                files[os.path.relpath(f, p)] = data
        out["files"] = files
    return out
x, y = load(a), load(b)
bad = [k for k in sorted(set(x) | set(y)) if x.get(k) != y.get(k)]
if "tiles" in x and "tiles" in y:
    print(f"  tiles felt {len(x['tiles'])} vtmake {len(y['tiles'])}", file=sys.stderr) if bad else None
sys.exit(1 if bad else 0)
EOF
  result "join $name" $?
}

for s in "${SETS[@]}"; do
  IFS='|' read -r name _ _ _ _ <<<"$s"
  join "$name-felt-o" o "$TS/$name-felt.mbtiles"
  join "$name-vt-o" o "$TS/$name-vt.mbtiles"
  join "$name-felt-dir-o" o "$TS/$name-felt.dir"
  join "$name-vt-e" e "$TS/$name-vt.mbtiles"
  join "$name-felt-dir-e" e "$TS/$name-felt.dir"
done
P=$TS/pub-poly-felt.mbtiles
T=$TS/types-felt.mbtiles
M=$TS/multi-felt.mbtiles
join "two-layers" o "$P" "$TS/pub-points-felt.mbtiles"
join "same-twice" o "$P" "$P"
join "felt-and-vt" o "$P" "$TS/pub-poly-vt.mbtiles"
join "four-inputs" e "$TS/multi-felt.dir" "$T" "$TS/multigeom-vt.mbtiles" "$TS/numbers-felt.dir"
join "pub-all" o "$TS/pub-points-felt.mbtiles" "$TS/pub-poly-vt.mbtiles" "$TS/pub-lines-felt.dir"
join "nometa" o "$TS/nometa.dir"
join "nometa-e" e "$TS/nometa.dir"
join "big-skip" o "$TS/big-felt.mbtiles"
join "big-pk" o -pk "$TS/big-felt.mbtiles"
join "big-pC" o --no-tile-compression --no-tile-size-limit "$TS/big-vt.mbtiles"
join "l" o -l alpha -l Beta "$M"
join "L" o -L zeta "$M"
join "R" o -R alpha:renamed -R zeta:Beta "$M"
join "x" o -x int -x uni "$T"
join "y" o -y cat -y flag "$T"
join "X" o -X "$T"
join "pg" o -pg "$T"
join "nNA" o -n "a name" -N "a description" -A "<b>attr</b>" "$T"
join "Zz" o -Z13 -z14 "$T"
join "stats-limits" o --tile-stats-values-limit=5 --tile-stats-sample-values-limit=3 --tile-stats-attributes-limit=2 "$T"
join "stats-values-0" o --tile-stats-values-limit=0 "$T"
join "exclude-all-attrs" o --exclude-all-tile-attributes "$T"
join "if-matched" o -i "$T"
join "wide" o "$TS/wide-felt.mbtiles" "$TS/wide-vt.dir"
printf '%s\n' "$P" "$T" >"$WORK/read-from.txt"
join "read-from" o --read-from="$WORK/read-from.txt"
join "err-missing" o "$WORK/nonexistent.mbtiles"
join "err-zooms" o -Z5 -z3 "$T"
join "err-p" o -px "$T"
join "err-R" o -R nothing "$T"

# PMTiles input, then the PMTiles and enumerate comparisons.
for name in types multi multigeom numbers antimeridian pub-poly pub-lines pub-points; do
  pm=$TS/$name-input.pmtiles
  "$FELT_DIR/tile-join" -q -f -o "$pm" "$TS/$name-felt.mbtiles" || exit 1
  dec "$name-pmtiles" "$pm"
  dec "$name-pmtiles-stats" -S "$pm"
  dec "$name-pmtiles-pipeline" -c "$pm"
  while read -r z x y; do
    dec "$name-pmtiles-$z-$x-$y" "$pm" "$z" "$x" "$y"
  done < <(some_tiles "$TS/$name-felt.mbtiles")
  join "$name-pmtiles-input" o "$pm"
done
printf 'int,joined,quoted,empty\n0,12.5,"007",\n1,-3,"text",\n2,4,"a,b",\n' >"$WORK/join.csv"
join "csv" o -c "$WORK/join.csv" "$T"
join "csv-matched" o -i -c "$WORK/join.csv" "$T"
join "csv-null" o -pe -c "$WORK/join.csv" "$T"
join "csv-exclude" o -x joined -c "$WORK/join.csv" "$T"
join "csv-include" o -y joined -c "$WORK/join.csv" "$T"
join "csv-no-tile-attrs" o --exclude-all-tile-attributes -c "$WORK/join.csv" "$T"
pmout=$(python3 -I "$R/bench/tools-pmtiles-check.py" "$WORK" "$FELT_DIR" "$VTMAKE" "$CPUS")
echo "$pmout" | head -n -1
read -r pmtotal pmsame <<<"$(tail -1 <<<"$pmout")"
total=$((total + pmtotal)); same=$((same + pmsame))
[ "$pmtotal" = "$pmsame" ] || diffs+=(pmtiles)

# Join filters, -O, --exclude-all-tile-geometries,
# --unidecode-data; overzoom; json-tool.
join "j-eq" o -j '{"*":["==","cat","c0"]}' "$T"
join "j-layer" o -j '{"types":[">","int",5],"other":false}' "$T"
join "j-zoom" o -j '{"*":[">=","$zoom",14]}' "$T"
join "j-type" o -j '{"*":["==","$type","Polygon"]}' "$TS/multigeom-felt.mbtiles"
join "j-id" o -j '{"*":["<","$id",50]}' "$T"
join "j-in" o -j '{"*":["in","cat","c1","c3",5]}' "$T"
join "j-notin" o -j '{"*":["!in","int",1,2,3]}' "$T"
join "j-has" o -j '{"*":["all",["has","sometimes"],["!has","nothing"]]}' "$T"
join "j-any-none" o -j '{"*":["any",["none",["==","flag",true]],["<=","float",0.5]]}' "$T"
join "j-mismatch" o -j '{"*":["==","int","text"]}' "$T"
join "j-nokey" o -j '{"*":["==","missing",1]}' "$T"
join "j-attr-filter" o -j '{"*":["attribute-filter","uni",["==","cat","c1"]]}' "$T"
join "j-attr-filter-e" e -j '{"*":["all",["attribute-filter","int",[">","$zoom",13]],["attribute-filter","long",false]]}' "$T"
join "j-rename" o -R alpha:renamed -j '{"renamed":["==","n",1],"zeta":false}' "$M"
join "j-csv" o -c "$WORK/join.csv" -j '{"*":["attribute-filter","joined",["==","int",0]]}' "$T"
join "j-dir" o -j '{"*":["has","name"]}' "$TS/pub-points-felt.dir" "$TS/pub-poly-felt.dir"
printf '{"*":["==","cat","c2"]}' >"$WORK/filter.json"
join "J-file" o -J "$WORK/filter.json" "$T"
join "err-j-parse" o -j '{"*":' "$T"
join "err-j-not-hash" o -j '["==","a",1]' "$T"
join "err-j-unknown-op" o -j '{"*":["~","a",1]}' "$T"
join "err-j-small" o -j '{"*":[]}' "$T"
join "err-J-missing" o -J "$WORK/nonexistent.json" "$T"
join "O-two" o -O "$M" "$TS/pub-poly-felt.mbtiles"
join "O-two-b" o -O -b 0 "$M" "$TS/pub-poly-felt.mbtiles"
join "O-dir-pmtiles" e -O "$TS/multi-felt.dir" "$TS/pub-lines-input.pmtiles" "$TS/pub-poly-vt.mbtiles"
join "O-high" o -O "$T" "$TS/pub-points-felt.mbtiles"
join "O-z" o -O -z15 "$T" "$TS/pub-points-felt.mbtiles"
join "O-one" o -O "$T"
join "O-same" o -O "$P" "$P"
join "err-b" o -b -1 "$T"
join "no-geometries" o --exclude-all-tile-geometries "$T"
join "no-geometries-e" e --exclude-all-tile-geometries "$TS/multigeom-felt.mbtiles"
join "no-geometries-attrs" o --exclude-all-tile-geometries --exclude-all-tile-attributes "$M"
join "unidecode" o --unidecode-data="$WORK/join.csv" "$T"
join "err-unidecode" o --unidecode-data="$WORK/nonexistent.txt" "$T"

# cmd: case tool args... (runs in a new folder; out.pbf is the output tile)
cmd() {
  local name=$1 t=$2; shift 2
  local d=$WORK/cmd/$t-$name tool
  [ -e "$d" ] && { echo "duplicate case: $name"; exit 2; }
  mkdir -p "$d/felt" "$d/vt"
  for tool in felt vt; do
    (cd "$d/$tool" && env PATH="$WORK/bin-$tool:$PATH" taskset -c "$CPUS" "$t" "$@" >out.txt 2>err.txt <"${STDIN:-/dev/null}"; echo $? >rc.txt)
  done
  local ok=0 f
  for f in rc.txt out.txt err.txt out.pbf; do
    if [ -e "$d/felt/$f" ] || [ -e "$d/vt/$f" ]; then cmp -s "$d/felt/$f" "$d/vt/$f" || ok=1; fi
  done
  result "$t $name" $ok
}
rm -rf "$WORK/cmd"
FP=$FELT_SRC/tests/pbf
OZ=tippecanoe-overzoom
cmd basic $OZ -o out.pbf "$FP/11-327-791.pbf" 11/327/791 13/1310/3166
cmd order $OZ -b20 --preserve-input-order -o out.pbf "$FP/11-327-791.pbf" 11/327/791 11/327/791
cmd source-tile $OZ -o out.pbf -t 13/1310/3166 "$FP/11-327-791.pbf" 11/327/791
cmd two-sources $OZ --no-tile-compression -o out.pbf -t 13/1310/3166 "$FP/11-327-791.pbf" 11/327/791 "$FP/0-0-0.pbf" 0/0/0
cmd detail $OZ -d8 -b30 -y NAME -y name -y scalerank -o out.pbf "$FP/11-327-791.pbf" 11/327/791 13/1310/3166
cmd empty $OZ -o out.pbf "$FP/11-327-791.pbf" 11/327/791 14/2616/6331
cmd thin $OZ -y NAME -m -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 0/0/0
cmd thin-child $OZ -m -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 2/0/1
for a in sum product mean max min concat comma count; do
  cmd "accum-$a" $OZ -m --accumulate-attribute "POP_MAX:$a" -E "NAME:$a" -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 0/0/0
done
cmd accum-json $OZ -m -E '{"POP_MIN":"sum","NAME":"comma"}' -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 0/0/0
cmd filter $OZ -y NAME -j '{"*":["==","SCALERANK",0]}' -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 0/0/0
cmd filter-file $OZ -y NAME -J "$FP/scalerank-0-filter.json" -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 0/0/0
cmd filter-mult $OZ -y NAME -y SCALERANK -j '{"*":["==","SCALERANK",0]}' -m -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 0/0/0
cmd filter-order $OZ -y NAME -y LATITUDE -j '{"*":[">","LATITUDE",40]}' -m --preserve-input-order -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 0/0/0
cmd filter-zoom $OZ -j '{"*":["all",[">=","$zoom",1],["==","$type","Point"]]}' -o out.pbf "$FP/0-0-0-pop.pbf" 0/0/0 1/1/0
cmd tiny $OZ --line-simplification=5 --tiny-polygon-size=50 -o out.pbf "$FP/countries-0-0-0.pbf" 0/0/0 0/0/0
cmd countries-1 $OZ -o out.pbf "$FP/countries-0-0-0.pbf" 0/0/0 1/1/0
cmd countries-8 $OZ -b 0 -o out.pbf "$FP/countries-0-0-0.pbf" 0/0/0 8/135/86
cmd simplify-lines $OZ -S 10 -o out.pbf "$FP/roads-1-1-0.pbf" 1/1/0 3/5/2
cmd exclude $OZ -x name -x scalerank --exclude-prefix=wiki -o out.pbf "$FP/places-1-1-0.pbf" 1/1/0 2/2/1
cmd yearbuilt $OZ -E yearbuilt:max -m -o out.pbf "$FP/yearbuilt.pbf" 0/0/0 0/0/0
cmd squirrels $OZ -d14 -b0 -o out.pbf "$FP/squirrels-13-2413-3077.pbf" 13/2413/3077 13/2413/3077
cmd unidecode $OZ --unidecode-data="$FP/1.json" -o out.pbf "$FP/0-0-0.pbf" 0/0/0 1/0/0
# Overzoom: tile:pz:the child zoom: a parent at zoom pz of tiles that are in the
# tileset at the child zoom, so the result has features.
oz_case() { # set pz cz count extra-args...
  local set=$1 pz=$2 cz=$3 count=$4 f cx cy px py tag; shift 4
  tag=$(printf '%s' "$*" | tr -c 'A-Za-z0-9' '_')
  for f in $(cd "$TS/$set-felt.dir" && find "$cz" -name '*.pbf' | sort | awk 'NR % 7 == 1' | head -"$count"); do
    IFS=/ read -r _ cx cy <<<"${f%.pbf}"
    px=$((cx >> (cz - pz))); py=$((cy >> (cz - pz)))
    [ -f "$TS/$set-felt.dir/$pz/$px/$py.pbf" ] || continue
    cmd "$set-$pz-$cz-$cx-$cy$tag" $OZ "$@" -o out.pbf "$TS/$set-felt.dir/$pz/$px/$py.pbf" "$pz/$px/$py" "$cz/$cx/$cy"
  done
}
oz_case pub-poly 14 15 3
oz_case pub-poly 14 16 3 -d10
oz_case pub-poly 14 16 2 -b0 --preserve-input-order
oz_case pub-lines 12 14 3
oz_case pub-lines 10 14 2 -S 4
oz_case pub-points 17 18 3 -y name
oz_case pub-poly 15 16 3
oz_case pub-poly 13 16 2 --tiny-polygon-size=4
oz_case pub-points 12 15 2 -j '{"*":["has","name"]}'
oz_case multigeom 14 16 3
for f in $(cd "$TS/pub-poly-felt.dir" && find 14 -name '*.pbf' | sort | head -2); do
  IFS=/ read -r tz tx ty <<<"${f%.pbf}"
  t=$TS/pub-poly-felt.dir/$f
  cmd "pub-poly-$tz-$tx-$ty-same" $OZ --preserve-input-order -o out.pbf "$t" "$tz/$tx/$ty" "$tz/$tx/$ty"
  cmd "pub-poly-$tz-$tx-$ty-dedup" $OZ --deduplicate-by-id -o out.pbf -t "$tz/$tx/$ty" "$t" "$tz/$tx/$ty" "$t" "$tz/$tx/$ty"
  cmd "pub-poly-$tz-$tx-$ty-nodedup" $OZ -o out.pbf -t "$tz/$tx/$ty" "$t" "$tz/$tx/$ty" "$t" "$tz/$tx/$ty"
done
cmd err-no-o $OZ "$FP/0-0-0.pbf" 0/0/0 0/0/0
cmd err-args $OZ -o out.pbf "$FP/0-0-0.pbf" 0/0/0
cmd err-zxy $OZ -o out.pbf "$FP/0-0-0.pbf" 0/0 0/0/0
cmd err-unpaired $OZ -o out.pbf -t 1/0/0 "$FP/0-0-0.pbf"
cmd err-option $OZ --nothing
cmd err-missing $OZ -o out.pbf "$WORK/nonexistent.pbf" 0/0/0 0/0/0
cmd err-filter $OZ -j '{"*":' -o out.pbf "$FP/0-0-0.pbf" 0/0/0 0/0/0
cmd err-accum $OZ -E NAME:bad -o out.pbf "$FP/0-0-0.pbf" 0/0/0 0/0/0
cmd err-accum-form $OZ -E NAME -o out.pbf "$FP/0-0-0.pbf" 0/0/0 0/0/0
cmd err-unidecode $OZ --unidecode-data="$WORK/nonexistent.txt" -o out.pbf "$FP/0-0-0.pbf" 0/0/0 0/0/0

JT=tippecanoe-json-tool
JP=$FELT_SRC/tests/join-population
"$FELT_DIR/$JT" -e GEOID10 "$JP/tabblock_06001420.json" | LC_ALL=C sort >"$WORK/sorted.json"
cmd extract $JT -e GEOID10 "$JP/tabblock_06001420.json"
cmd csv $JT -c "$JP/population.csv" "$WORK/sorted.json"
cmd csv-null $JT --empty-csv-columns-are-null -c "$JP/population.csv" "$WORK/sorted.json"
cmd csv-pe $JT -pe -c "$JP/population.csv" "$WORK/sorted.json"
cmd csv-unsorted $JT -c "$JP/population.csv" "$JP/tabblock_06001420.json"
cmd plain $JT "$JP/tabblock_06001420.json"
cmd wrap $JT -w "$JP/tabblock_06001420.json"
STDIN=$JP/tabblock_06001420.json cmd stdin $JT -w
cmd extract-missing $JT -e nothing "$JP/tabblock_06001420.json"
for f in types multigeom numbers wide antimeridian pub-points pub-lines; do
  cmd "plain-$f" $JT "$IN/$f.ndjson"
  cmd "wrap-$f" $JT --wrap "$IN/$f.ndjson"
  cmd "extract-$f" $JT --extract=name "$IN/$f.ndjson"
done
cmd extract-number $JT -e int "$IN/types.ndjson"
cmd extract-uni $JT -e uni "$IN/types.ndjson"
cmd geometries $JT -w "$FP/clip-poly.json" "$FP/region.json"
cmd err-wrap-extract $JT -w -e x "$JP/tabblock_06001420.json"
cmd err-p $JT -px
cmd err-option $JT --nothing
cmd err-missing $JT "$WORK/nonexistent.json"
cmd err-csv-missing $JT -c "$WORK/nonexistent.csv" "$WORK/sorted.json"

# Build output options: -pC, -pg, --tile-stats-*, -F. The seed flags stay on.
SEED=(--read-parallel --buffer=5 --drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit --no-tile-size-limit)
# bld: case out(out.mbtiles|out.pmtiles|outdir) input args...
bld() {
  local name=$1 out=$2 input=$3; shift 3
  local d=$WORK/bld/$name tool
  [ -e "$d" ] && { echo "duplicate case: $name"; exit 2; }
  mkdir -p "$d/felt" "$d/vt"
  local o=-o
  [ "$out" = outdir ] && o=-e
  for tool in felt vt; do
    (cd "$d/$tool" && env TIPPECANOE_MAX_THREADS=2 PATH="$WORK/bin-$tool:$PATH" taskset -c "$CPUS" tippecanoe -q -f "${SEED[@]}" "$@" "$o" "$out" "$IN/$input" >out.txt 2>err.txt; echo $? >rc.txt
     env PATH="$WORK/bin-$tool:$PATH" tippecanoe-decode -x generator "$out" >dec.json 2>&1)
  done
  local ok=0 f
  for f in rc.txt err.txt dec.json; do cmp -s "$d/felt/$f" "$d/vt/$f" || ok=1; done
  # A PMTiles file must have the same bytes (vtmake writes felt's generator there).
  if [ "$out" = out.pmtiles ]; then cmp -s "$d/felt/$out" "$d/vt/$out" || ok=1; fi
  result "build $name" $ok
}
rm -rf "$WORK/bld"
for out in out.mbtiles outdir out.pmtiles; do
  bld "pC-$out" $out types.ndjson -Z12 -z14 -pC
  bld "no-tile-compression-$out" $out multi.ndjson -Z12 -z15 --no-tile-compression
  bld "pg-$out" $out types.ndjson -Z12 -z14 -pg
  bld "no-tile-stats-$out" $out numbers.ndjson -Z9 -z12 --no-tile-stats
  bld "stats-limits-$out" $out types.ndjson -Z12 -z13 --tile-stats-values-limit=3 --tile-stats-sample-values-limit=1 --tile-stats-attributes-limit=2
  bld "stats-values-0-$out" $out types.ndjson -Z12 -z13 --tile-stats-values-limit=0
  bld "stats-attributes-0-$out" $out wide.ndjson -Z12 -z13 --tile-stats-attributes-limit=0
  bld "stats-samples-big-$out" $out types.ndjson -Z12 -z13 --tile-stats-sample-values-limit=5000
  bld "stats-negative-$out" $out types.ndjson -Z12 -z13 --tile-stats-sample-values-limit=-1 --tile-stats-values-limit=2
done
bld "pmtiles-points" out.pmtiles pub-points.ndjson -Z12 -z18 --layer pois
bld "pmtiles-lines" out.pmtiles pub-lines.ndjson -Z9 -z14 --layer lines
bld "pmtiles-pC-poly" out.pmtiles pub-poly.ndjson -Z12 -z16 -pC
# -F (--allow-existing): make, fail, replace, add zooms, add the same zooms again.
for out in out.mbtiles outdir out.pmtiles; do
  d=$WORK/bld/allow-existing-$out
  mkdir -p "$d/felt" "$d/vt"
  o=-o; [ "$out" = outdir ] && o=-e
  for tool in felt vt; do
    (cd "$d/$tool" && export PATH="$WORK/bin-$tool:$PATH" TIPPECANOE_MAX_THREADS=2
     for step in "-f -Z12 -z12" "-Z13 -z13" "-f -Z12 -z12" "-F -Z13 -z13" "-F -Z13 -z13"; do
       # shellcheck disable=SC2086
       taskset -c "$CPUS" tippecanoe -q "${SEED[@]}" $step "$o" "$out" "$IN/types.ndjson" 2>>err.txt; echo $? >>rc.txt
     done
     # The first tile that is there already depends on the thread order.
     sed -i 's/^Can.t write tile to already existing .*/Can'"'"'t write tile to already existing TILE/' err.txt
     tippecanoe-decode -x generator -x generator_options "$out" >dec.json 2>&1)
  done
  ok=0
  for f in rc.txt err.txt dec.json; do cmp -s "$d/felt/$f" "$d/vt/$f" || ok=1; done
  result "build allow-existing-$out" $ok
done

echo "cases $total, identical $same, different $((total - same))"
[ ${#diffs[@]} -eq 0 ]
