#!/bin/bash
# Run a list of tasks for a list of tools, A/B order inside each task.
# Usage: bench/suite.sh <profile> <reps> "<tools>" <task>...
set -uo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
profile=$1 reps=$2 tools=$3; shift 3
for task in "$@"; do
  for ((i = 1; i <= reps; i++)); do
    for tool in $tools; do
      "$R/bench/run.sh" "$tool" "$task" "$profile" "suite rep $i" || echo "FAILED $tool $task $profile" >&2
    done
  done
done
echo SUITE DONE
