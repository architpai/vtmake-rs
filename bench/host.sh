#!/bin/bash
# Prepare or restore the host for benchmark runs.
# setup: keep CPUs 0-1 (one core, both threads) free for bench.slice: move all
#        other work to CPUs 2 and up, set the performance governor, and cap the
#        clock at MAXFREQ (kHz, default 3500000: near a server CPU boost clock).
#        Stop other heavy services yourself.
# Usage: bench/host.sh setup|restore|status
set -euo pipefail
last=$(( $(nproc --all) - 1 ))
MAXFREQ=${MAXFREQ:-3500000}
case ${1:-} in
  setup)
    for s in user.slice system.slice init.scope; do sudo systemctl set-property --runtime $s AllowedCPUs=2-$last; done
    for c in 0 1; do
      echo performance | sudo tee /sys/devices/system/cpu/cpu$c/cpufreq/scaling_governor >/dev/null
      echo "$MAXFREQ" | sudo tee /sys/devices/system/cpu/cpu$c/cpufreq/scaling_max_freq >/dev/null
    done ;;
  restore)
    for s in user.slice system.slice init.scope; do sudo systemctl set-property --runtime $s AllowedCPUs=; done
    for c in 0 1; do
      echo powersave | sudo tee /sys/devices/system/cpu/cpu$c/cpufreq/scaling_governor >/dev/null
      cat /sys/devices/system/cpu/cpu$c/cpufreq/cpuinfo_max_freq | sudo tee /sys/devices/system/cpu/cpu$c/cpufreq/scaling_max_freq >/dev/null
    done ;;
  status)
    for s in user.slice system.slice init.scope; do echo "$s $(systemctl show -p AllowedCPUs --value $s)"; done
    for c in 0 1; do echo "cpu$c $(cat /sys/devices/system/cpu/cpu$c/cpufreq/scaling_governor) $(cat /sys/devices/system/cpu/cpu$c/cpufreq/scaling_max_freq)"; done ;;
  *) echo "usage: bench/host.sh setup|restore|status" >&2; exit 2 ;;
esac
