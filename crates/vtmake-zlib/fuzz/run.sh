#!/bin/sh
# Fuzz vtmake-zlib against the system zlib (it must be zlib 1.3.1).
# Usage: fuzz/run.sh [CASES] [SEED]
set -e
D=$(cd "$(dirname "$0")/.." && pwd)
OUT=${TMPDIR:-/tmp}/vtmake-zlib-fuzz
Z=$D/zlib
gcc -O2 -DZ_PREFIX -DBT_CHECK_SPARSE -I"$Z" -o "$OUT" "$D/fuzz/zh.c" \
    "$Z/deflate.c" "$Z/trees.c" "$Z/crc32.c" "$Z/adler32.c" "$Z/zutil.c" -lz
"$OUT" fuzz "${1:-30000}" "${2:-1}"
