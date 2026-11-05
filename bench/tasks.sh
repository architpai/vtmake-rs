# Benchmark tasks. Sourced by bench/run.sh. Sets INPUT, LAYER, ARGS for task $1.
# Env:  P   the public corpus folder: points.ndjson, polygons.ndjson,
#           points-plain.ndjson, polygons-plain.ndjson
#       PL  pub-lines.ndjson from bench/prep_lines.py (pub-lines-plain.ndjson
#           must be in the same folder)
need() { [ -n "${!1:-}" ] || { echo "set $1 to $2" >&2; exit 2; }; }
need_p() { need P "the public corpus folder (points.ndjson, polygons.ndjson, ...)"; }
need_pl() { need PL "pub-lines.ndjson from bench/prep_lines.py"; }
# The seed flags: the plain command line for one layer at fixed zooms.
seed=(--read-parallel --buffer=5 --force --drop-densest-as-needed
      --extend-zooms-if-still-dropping --no-feature-limit --no-tile-size-limit)
# felt's common command: the dropping code runs.
common=(-zg --read-parallel --force --drop-densest-as-needed --extend-zooms-if-still-dropping)
case $1 in
  pub-points-seed)   need_p;  INPUT=$P/points.ndjson;   LAYER=points;   ARGS=(-Z9 -z18 "${seed[@]}") ;;
  pub-polygons-seed) need_p;  INPUT=$P/polygons.ndjson; LAYER=polygons; ARGS=(-Z12 -z18 "${seed[@]}") ;;
  pub-lines-seed)    need_pl; INPUT=$PL;                LAYER=lines;    ARGS=(-Z9 -z18 "${seed[@]}") ;;
  pub-points-zg)     need_p;  INPUT=$P/points-plain.ndjson;   LAYER=points;   ARGS=("${common[@]}") ;;
  pub-polygons-zg)   need_p;  INPUT=$P/polygons-plain.ndjson; LAYER=polygons; ARGS=("${common[@]}") ;;
  pub-lines-zg)      need_pl; INPUT=${PL%.ndjson}-plain.ndjson; LAYER=lines; ARGS=("${common[@]}") ;;
  *) echo "unknown task $1" >&2; exit 2 ;;
esac
