# Speed on public data (2026-11-07)

Machine profile `small`: a Docker container with 2 CPUs, 8 GB of memory for the
process and the page cache, 500 IOPS and 82 MB/s disk (`bench/run.sh`).
felt = felt/tippecanoe `4f26211` (v2.82.0), one run. vtmake: the mean of 2 runs
for the seed tasks and 1 run for the `-zg` tasks; RSS is the largest of the
runs. "Same tiles" = the SHA3 of all tiles is equal to felt's in every run.

Inputs: OSM buildings (7.1 M polygons), 7.1 M points, and
OSM roads (2.43 M lines). "seed" tasks use the plain command line
(`--drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit
--no-tile-size-limit`). "zg" tasks use felt's common `-zg` command.

This version adds a simple-ring fast path to wagyu, a key/value index with
source ids for the MVT encoder, and parallel work inside large low-zoom tiles.

| Task | felt wall s | vtmake wall s | vtmake vs felt | vtmake CPU s | vtmake RSS MB | Same tiles |
|---|---:|---:|---:|---:|---:|---|
| pub-lines-seed | 354.0 | 31.9 | 11.1x | 62 | 1223 | yes |
| pub-points-seed | 390.3 | 14.1 | 27.6x | 27 | 1705 | yes |
| pub-polygons-seed | 840.2 | 63.3 | 13.3x | 124 | 1683 | yes |
| pub-points-zg | 491.1 | 51.6 | 9.5x | 67 | 1860 | yes |
| pub-polygons-zg | 1034.3 | 157.3 | 6.6x | 176 | 4116 | yes |
| pub-lines-zg | 367.9 | 64.9 | 5.7x | 68 | 1553 | yes |

Profile `big` (all CPUs, no memory or disk limit), one run:

| Task | vtmake wall s | vtmake CPU s | vtmake RSS MB | Same tiles |
|---|---:|---:|---:|---|
| pub-points-zg | 17.9 | 147 | 2591 | yes |
| pub-polygons-zg | 51.0 | 375 | 4375 | yes |

## Earlier results (2026-11-05, the first public release)

Same machine profile `small`, one run each.

| Task | felt wall s | vtmake wall s | vtmake vs felt | vtmake CPU s | vtmake RSS MB | Same tiles |
|---|---:|---:|---:|---:|---:|---|
| pub-lines-seed | 354.0 | 48.5 | 7.3x | 95.5 | 1226 | yes |
| pub-points-seed | 390.3 | 21.4 | 18.2x | 41.1 | 1659 | yes |
| pub-polygons-seed | 840.2 | 139.0 | 6.0x | 275.4 | 1684 | yes |
| pub-points-zg | 491.1 | 94.5 | 5.2x | 124.0 | 2416 | yes |
| pub-polygons-zg | 1034.3 | 282.5 | 3.7x | 340.9 | 5264 | yes |
| pub-lines-zg | 367.9 | 112.2 | 3.3x | 127.6 | 2907 | yes |
