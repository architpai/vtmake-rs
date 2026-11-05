#!/bin/bash
# One benchmark run of a tippecanoe-compatible binary under machine limits.
# Usage: bench/run.sh <tool> <task> <small|big> [note]
#   tool:  a folder name in BIN (felt, vtmake, ...) that holds ./tippecanoe
#          and, if you like, a REV file with the source revision
#   task:  a task in bench/tasks.sh (it reads P and PL)
#   small: a small 2-CPU profile: CPUs 0-1 (one core, SMT), 8 GB, 500 IOPS,
#          82 MB/s, sysconf reports 2 CPUs and 7.75 GiB. Run bench/host.sh setup first.
#   big:   all CPUs, no memory or disk limit.
# Env:   BIN     the folder with one folder per tool (no default)
#        WORK    the work folder (default ${TMPDIR:-/tmp}/vtmake-bench). The disk
#                limits of "small" apply to the disk of this folder, so for
#                "small" it must not be on tmpfs.
#        DEV     the disk for the IO limits (default: the disk of WORK)
#        VMSPEC  vmspec.so for "small" (default $BIN/vmspec.so). Build it with
#                cc -shared -fPIC -O2 -o vmspec.so bench/vmspec.c -ldl
# Each run starts with a cold page cache. Then the input is copied into the run
# directory under the same limits (a pipeline that writes the NDJSON just before
# tippecanoe has part of it still in the page cache). For small, bench.slice has
# MemoryMax=8G, so the page cache and the process share 8 GB, as on a small machine.
# One JSON line goes to bench/results/runs.jsonl.
set -euo pipefail
R=$(cd "$(dirname "$0")/.." && pwd)
[ $# -ge 3 ] || { echo "usage: bench/run.sh <tool> <task> <small|big> [note]" >&2; exit 2; }
: "${BIN:?set BIN to the folder with one folder per tool (BIN/<tool>/tippecanoe)}"
WORK=${WORK:-${TMPDIR:-/tmp}/vtmake-bench}
VMSPEC=${VMSPEC:-$BIN/vmspec.so}
tool=$1 task=$2 profile=$3 note=${4:-}
source "$R/bench/tasks.sh" "$task"
bin=$BIN/$tool/tippecanoe
[ -x "$bin" ] || { echo "no binary $bin" >&2; exit 2; }
id="$(date +%Y%m%d-%H%M%S)-$tool-$task-$profile"
out=$WORK/out/$id; tmp=$WORK/tmp/run-$id; in=$WORK/tmp/in-$id
mkdir -p "$out" "$tmp" "$in"
# The disk for the IO limits: a device, or any path on it (systemd finds the disk).
dev=${DEV:-$WORK}
props=(-p Slice=bench.slice -p MemorySwapMax=0)
envs=()
case $profile in
  small) [ -f "$VMSPEC" ] || { echo "set VMSPEC to vmspec.so (build it from bench/vmspec.c)" >&2; exit 2; }
         [ "$(stat -f -c %T "$WORK")" != tmpfs ] || { echo "set WORK to a folder on a disk (tmpfs has no IO limits)" >&2; exit 2; }
         props+=(-p AllowedCPUs=0,1 -p MemoryMax=8G
           -p "IOReadIOPSMax=$dev 500" -p "IOWriteIOPSMax=$dev 500"
           -p "IOReadBandwidthMax=$dev 82M" -p "IOWriteBandwidthMax=$dev 82M")
         envs=(-E TIPPECANOE_MAX_THREADS=2 -E BENCH_NPROC=2 -E BENCH_PHYS_BYTES=8321499136 -E "LD_PRELOAD=$VMSPEC") ;;
  big)   props+=(-p "AllowedCPUs=0-$(( $(nproc --all) - 1 ))") ;;
  *) echo "unknown profile $profile" >&2; exit 2 ;;
esac
case $profile in
  small) sudo systemctl set-property --runtime bench.slice MemoryMax=8G MemorySwapMax=0 ;;
  big)   sudo systemctl set-property --runtime bench.slice MemoryMax=infinity MemorySwapMax=infinity ;;
esac
sync && echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
c0=$(date +%s%N)
sudo systemd-run --wait --collect --quiet --uid="$(id -u)" --gid="$(id -g)" "${props[@]}" -- cp "$INPUT" "$in/input.ndjson"
c1=$(date +%s%N)
INPUT=$in/input.ndjson
t0=$(date +%s%N)
set +e
sudo systemd-run --wait --collect --uid="$(id -u)" --gid="$(id -g)" -p WorkingDirectory=$tmp "${props[@]}" "${envs[@]}" \
  -p StandardOutput=file:$out/stdout.log -p StandardError=file:$out/stderr.log \
  -- /usr/bin/time -v -o $out/time.txt "$bin" "${ARGS[@]}" -t "$tmp" --layer "$LAYER" -o "$out/out.mbtiles" "$INPUT" \
  > "$out/systemd.txt" 2>&1 &
runner=$!
# Temp peak: tippecanoe unlinks its temp files, so add up the deleted files that the
# process holds open, plus the files that are still in the temp directory.
python3 -I "$R/bench/tempwatch.py" "$tmp" "$runner" > "$out/temp_peak.txt" &
watcher=$!
wait $runner; rc=$?
t1=$(date +%s%N)
wait $watcher
set -e
num() { grep -m1 "$1" "$out/time.txt" | awk -F': ' '{print $2}'; }
tiles=na hash=na size=na
if [ -s "$out/out.mbtiles" ]; then
  tiles=$(sqlite3 "$out/out.mbtiles" "select count(*) from tiles")
  hash=$(sqlite3 "$out/out.mbtiles" "select hex(sha3_query('select zoom_level,tile_column,tile_row,tile_data from tiles order by 1,2,3'))" | cut -c1-16)
  size=$(stat -c %s "$out/out.mbtiles")
fi
peak_mem=$(grep -oP 'Memory peak: \K\S+' "$out/systemd.txt" || echo na)
cpu=$(grep -oP 'CPU time consumed: \K.*' "$out/systemd.txt" || echo na)
jq -cn --arg id "$id" --arg tool "$tool" --arg rev "$(cat "$BIN/$tool/REV" 2>/dev/null || echo na)" \
  --arg task "$task" --arg profile "$profile" --arg harness "$(git -C "$R" rev-parse --short HEAD)" \
  --argjson rc $rc --argjson copy_ms $(( (c1 - c0) / 1000000 )) --argjson wall_ms $(( (t1 - t0) / 1000000 )) \
  --arg user_s "$(num 'User time')" --arg sys_s "$(num 'System time')" --arg max_rss_kb "$(num 'Maximum resident')" \
  --arg mem_peak "$peak_mem" --arg cpu "$cpu" --arg temp_peak "$(cat $out/temp_peak.txt)" \
  --arg tiles "$tiles" --arg hash "$hash" --arg size "$size" --arg note "$note" \
  '{id:$id,tool:$tool,rev:$rev,task:$task,profile:$profile,harness:$harness,rc:$rc,copy_s:($copy_ms/1000),wall_s:($wall_ms/1000),
    user_s:($user_s|tonumber? // null),sys_s:($sys_s|tonumber? // null),max_rss_mb:(($max_rss_kb|tonumber? // 0)/1024|floor),
    mem_peak:$mem_peak,cpu:$cpu,temp_peak_mb:(($temp_peak|tonumber? // 0)/1048576|floor),
    tiles:($tiles|tonumber? // null),out_mb:(($size|tonumber? // 0)/1048576|floor),tiles_sha:$hash,note:$note}' \
  | tee -a "$R/bench/results/runs.jsonl"
rm -rf "$tmp" "$in"
[ -n "${KEEP:-}" ] || rm -f "$out/out.mbtiles"
exit $rc
