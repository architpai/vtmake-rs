# felt test baseline

Reference: felt `4f2621186acfec33b63ddf636f665623c0fef2dd` (v2.82.0).

Each JSON output file is one case. Each other Makefile target is one case.
A target stops at its first failed command, as it does in Make.
All six public tools come from the selected tool set.
The C++ `unit` case needs a separate `unit` binary. vtmake has no such binary;
this case is an ERROR in the vtmake count.

This is the baseline from the start of the port. The current result is 200 of
205 (see the README).

## Results

| Target | felt, 1 thread | felt, 2 threads | vtmake, 2 threads |
|---|---:|---:|---:|
| allow-existing-test | 1/1 | 1/1 | 0/1 |
| csv-test | 1/1 | 1/1 | 0/1 |
| decode-pmtiles-test | 1/1 | 1/1 | 0/1 |
| decode-test | 1/1 | 1/1 | 0/1 |
| enumerate-test | 1/1 | 1/1 | 0/1 |
| flatgeobuf-test | 1/1 | 1/1 | 0/1 |
| join-filter-test | 1/1 | 1/1 | 0/1 |
| join-test | 1/1 | 1/1 | 0/1 |
| json-tool-test | 1/1 | 1/1 | 0/1 |
| json.check | 187/188 | 187/188 | 0/188 |
| layer-json-test | 1/1 | 1/1 | 0/1 |
| overzoom-test | 1/1 | 1/1 | 0/1 |
| parallel-test | 1/1 | 1/1 | 0/1 |
| pbf-test | 1/1 | 1/1 | 1/1 |
| pmtiles-test | 1/1 | 1/1 | 0/1 |
| radix-sort-test | 1/1 | 1/1 | 0/1 |
| raw-tiles-test | 1/1 | 1/1 | 0/1 |
| unit | 1/1 | 1/1 | 0/1 |
| **Total** | **204/205** | **204/205** | **1/205** |

## Time and run records

- felt, 1 thread: 33.28 seconds.
- felt, 2 threads: 28.69 seconds.
- vtmake, 2 threads: 5.15 seconds.

The runs use 30 workers on CPUs 2–31. Build time is not in these times.
Each case has a 180-second limit. Each run has tool hashes and the source revision
in `metadata.json`. `results.tsv` and `results.jsonl` give each command and result.

## felt failures

- `tests/ne_110m_admin_0_countries/out/-z2_--convert-polygons-to-label-points.json.check`: exit 1; stored JSON differs.

`allow-existing-test` passes at both thread counts in this run.
The failure reported by an earlier project run did not occur here.
An independent run with the full original Makefile (`make -k -j30 test`,
with built tools marked up to date and `LC_ALL=C`) found only the same
label point failure.

## Options in failed vtmake cases

Counts show cases with an option, not the cause of a failure. A failed input read
can prevent the tool from reaching an option. Common test controls `-q`, `-f`,
and `-o` are excluded. Short option values are grouped under the option.
Special targets include only commands that Make reached.

| Option | Failed cases | Cases with option |
|---|---:|---:|
| `-a@` | 188 | 188 |
| `-z` | 187 | 187 |
| `-y` | 43 | 43 |
| `-Z` | 35 | 35 |
| `-M` | 23 | 23 |
| `-r` | 23 | 23 |
| `-B` | 10 | 10 |
| `-l` | 10 | 10 |
| `--retain-points-multiplier` | 8 | 8 |
| `-c` | 8 | 8 |
| `--drop-smallest-as-needed` | 7 | 7 |
| `--generate-variable-depth-tile-pyramid` | 6 | 6 |
| `--smallest-maximum-zoom-guess` | 6 | 6 |
| `-C` | 6 | 6 |
| `-b` | 6 | 6 |
| `-d` | 6 | 6 |
| `--accumulate-attribute` | 5 | 5 |
| `--coalesce` | 5 | 5 |
| `--use-attribute-for-id` | 5 | 5 |
| `-D` | 5 | 5 |
| `-K` | 5 | 5 |
| `-P` | 5 | 5 |
| `-x` | 5 | 6 |
| `--coalesce-densest-as-needed` | 4 | 4 |
| `--drop-densest-as-needed` | 4 | 4 |
| `--limit-tile-feature-count` | 4 | 4 |
| `--set-attribute` | 4 | 4 |
| `-L` | 4 | 4 |
| `-O` | 4 | 4 |
| `-ae` | 4 | 4 |
| `-e` | 4 | 4 |
| `-p` | 4 | 4 |
| `-pp` | 4 | 4 |
| `--clip-bounding-box` | 3 | 3 |
| `--detect-longitude-wraparound` | 3 | 3 |
| `--grid-low-zooms` | 3 | 3 |
| `--no-simplification-of-shared-nodes` | 3 | 3 |
| `--order-by` | 3 | 3 |
| `--reorder` | 3 | 3 |
| `--tiny-polygon-size` | 3 | 3 |
| `-J` | 3 | 3 |
| `-S` | 3 | 3 |
| `-ac` | 3 | 3 |
| `-pC` | 3 | 3 |
| `--cluster-densest-as-needed` | 2 | 2 |
| `--coalesce-smallest-as-needed` | 2 | 2 |
| `--drop-by-attribute-as-needed` | 2 | 2 |
| `--drop-fraction-as-needed` | 2 | 2 |
| `--extra-detail` | 2 | 2 |
| `--fraction` | 2 | 2 |
| `--integer` | 2 | 2 |
| `--order-largest-first` | 2 | 2 |
| `--preserve-input-order` | 2 | 2 |
| `--stats` | 2 | 2 |
| `-E` | 2 | 2 |
| `-X` | 2 | 2 |
| `-Y` | 2 | 2 |
| `-aI` | 2 | 2 |
| `-g` | 2 | 2 |
| `-k` | 2 | 2 |
| `-n` | 2 | 2 |
| `--calculate-feature-density` | 1 | 1 |
| `--coalesce-fraction-as-needed` | 1 | 1 |
| `--convert-polygons-to-label-points` | 1 | 1 |
| `--detect-shared-borders` | 1 | 1 |
| `--drop-by-attribute-order` | 1 | 1 |
| `--drop-denser` | 1 | 1 |
| `--drop-polygons` | 1 | 1 |
| `--extend-zooms-if-still-dropping` | 1 | 1 |
| `--extend-zooms-if-still-dropping-maximum` | 1 | 1 |
| `--force-feature-limit` | 1 | 1 |
| `--hilbert` | 1 | 1 |
| `--limit-tile-feature-count-at-maximum-zoom` | 1 | 1 |
| `--maximum-string-attribute-length` | 1 | 1 |
| `--maximum-tile-features` | 1 | 1 |
| `--no-clipping` | 1 | 1 |
| `--no-duplication` | 1 | 1 |
| `--no-tile-stats` | 1 | 1 |
| `--no-tiny-polygon-reduction` | 1 | 1 |
| `--no-tiny-polygon-reduction-at-maximum-zoom` | 1 | 1 |
| `--order-descending-by` | 1 | 1 |
| `--order-smallest-first` | 1 | 1 |
| `--prefer-radix-sort` | 1 | 1 |
| `--preserve-multiplier-density-threshold` | 1 | 1 |
| `--preserve-point-density-threshold` | 1 | 1 |
| `--reverse-source-polygon-winding` | 1 | 1 |
| `--simplification` | 1 | 1 |
| `--simplification-at-maximum-zoom` | 1 | 1 |
| `--tile-stats-attributes-limit` | 1 | 1 |
| `--tile-stats-sample-values-limit` | 1 | 1 |
| `--tile-stats-values-limit` | 1 | 1 |
| `--use-source-polygon-winding` | 1 | 1 |
| `--visvalingam` | 1 | 1 |
| `-A` | 1 | 1 |
| `-N` | 1 | 1 |
| `-R` | 1 | 1 |
| `-T` | 1 | 1 |
| `-aG` | 1 | 1 |
| `-aR` | 1 | 1 |
| `-ad` | 1 | 1 |
| `-ai` | 1 | 1 |
| `-ao` | 1 | 1 |
| `-as` | 1 | 1 |
| `-pD` | 1 | 1 |
| `-pN` | 1 | 1 |
| `-pS` | 1 | 1 |
| `-pb` | 1 | 1 |
| `-pc` | 1 | 1 |
| `-pd` | 1 | 1 |
| `-pf` | 1 | 1 |
| `-pi` | 1 | 1 |
| `-pk` | 1 | 1 |
| `-ps` | 1 | 1 |
| `-pt` | 1 | 1 |
| `-s` | 1 | 2 |

188 cases stop because vtmake rejects `-a@` (exit 101).
Add this option first, then run the suite again to measure the other gaps.
The other option counts show test coverage that this failure blocks.

## Thread count

No test recipe fixes `TIPPECANOE_MAX_THREADS` in this source revision.
The runner set it to 1 and 2 for all felt cases. The reference for vtmake is 2.
The hashes below cover the bytes passed to `cmp`, before Make removes output.
They detect output changes even when both runs fail against the stored JSON.

| Case with changed output | 1 thread | 2 threads |
|---|---|---|
| `tests/ne_110m_admin_0_countries/out/-z2_--convert-polygons-to-label-points.json.check` | FAIL | FAIL |

`parallel-test` uses random input with no fixed seed. Its hashes cannot test
thread dependence. Its comparisons pass within each felt run.

These observations do not prove that all other runs are independent of thread count.
The label point case also changes between repeated runs at the same thread count;
its observed change cannot be assigned to the thread count alone.

## Scope

This checkout has 188 JSON cases and 17 other targets.
The count comes from the source files and Makefile, rather than the older plan count.
No test data or generated tiles are stored in this repository.
