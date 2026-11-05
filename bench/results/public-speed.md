# Speed on public data (2026-11-05)

Machine profile `small`: a Docker container with 2 CPUs, 8 GB of memory for the
process and the page cache, 500 IOPS and 82 MB/s disk (`bench/run.sh`).
felt = felt/tippecanoe `4f26211` (v2.82.0). One run each. "Same tiles" = the
SHA3 of all tiles is equal to felt's.

Inputs: OSM buildings (7.1 M polygons), 7.1 M points, and
OSM roads (2.43 M lines). "seed" tasks use the plain command line
(`--drop-densest-as-needed --extend-zooms-if-still-dropping --no-feature-limit
--no-tile-size-limit`). "zg" tasks use felt's common `-zg` command.

| Task | felt wall s | vtmake wall s | vtmake vs felt | vtmake CPU s | vtmake RSS MB | Same tiles |
|---|---:|---:|---:|---:|---:|---|
| pub-lines-seed | 354.0 | 48.5 | 7.3x | 95.5 | 1226 | yes |
| pub-points-seed | 390.3 | 21.4 | 18.2x | 41.1 | 1659 | yes |
| pub-polygons-seed | 840.2 | 139.0 | 6.0x | 275.4 | 1684 | yes |
| pub-points-zg | 491.1 | 94.5 | 5.2x | 124.0 | 2416 | yes |
| pub-polygons-zg | 1034.3 | 282.5 | 3.7x | 340.9 | 5264 | yes |
| pub-lines-zg | 367.9 | 112.2 | 3.3x | 127.6 | 2907 | yes |
