#!/bin/bash
# Speed and output gate: run each task once with the small profile and check
# the wall time and the tiles against a baseline. Run it with no other benchmark.
# Usage: bench/wall-gate.sh [tool]      (default tool: vtmake)
# Env:   BASELINES  a file with one line per task: "task seconds tiles_sha"
#                   (tiles_sha as in bench/results/runs.jsonl). Make it on your
#                   own quiet host: times from other hosts do not apply.
#        BIN, WORK, P, PL and the other variables of bench/run.sh.
# A run passes if its exit code is 0, its wall time is at most 5% above the
# baseline, and its tiles_sha is equal.
set -euo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
: "${BASELINES:?set BASELINES to a file with lines \"task seconds tiles_sha\"}"
[ -f "$BASELINES" ] || { echo "no file $BASELINES" >&2; exit 2; }
tool=${1:-vtmake}
cd "$R"
trap '"$R/bench/host.sh" restore' EXIT
"$R/bench/host.sh" setup
fail=0
while read -r task baseline hash; do
  case $task in ''|'#'*) continue ;; esac
  log=$(mktemp)
  if "$R/bench/run.sh" "$tool" "$task" small wall-gate </dev/null | tee "$log"; then
    rc=0
    python3 -I - "$log" "$task" "$baseline" "$hash" <<'PY' || rc=$?
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1]) if line.startswith('{')]
assert len(rows) == 1, 'Missing benchmark result'
r = rows[0]
limit = float(sys.argv[3]) * 1.05
ok = r['rc'] == 0 and r['user_s'] is not None and r['wall_s'] <= limit and r['tiles_sha'] == sys.argv[4]
print(f"{'PASS' if ok else 'FAIL'} {sys.argv[2]}: {r['wall_s']:.3f} s; limit {limit:.3f} s; tiles {r['tiles_sha']}")
sys.exit(0 if ok else 1)
PY
    [ "$rc" = 0 ] || fail=1
  else
    fail=1
  fi
  rm -f "$log"
done <"$BASELINES"
exit "$fail"
