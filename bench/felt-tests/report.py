#!/usr/bin/env python3
"""Write target counts, option counts, and thread changes from three runs."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re
from urllib.parse import unquote

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--felt-one', type=Path, required=True)
p.add_argument('--felt-two', type=Path, required=True)
p.add_argument('--vtmake', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()

def read(d):
    return {r['id']: r for r in map(json.loads, (d / 'results.jsonl').read_text().splitlines())}

one, two, vt = map(read, (a.felt_one, a.felt_two, a.vtmake))
if one.keys() != two.keys() or two.keys() != vt.keys():
    p.error('The runs must have the same cases')
lines = ['# felt test baseline', '',
         'Reference: felt `4f2621186acfec33b63ddf636f665623c0fef2dd` (v2.82.0).', '',
         'Each JSON output file is one case. Each other Makefile target is one case.',
         'A target stops at its first failed command, as it does in Make.',
         'All six public tools come from the selected tool set.',
         'The C++ `unit` case needs a separate `unit` binary. vtmake has no such binary;',
         'this case is an ERROR in the vtmake count.', '',
         '## Results', '', '| Target | felt, 1 thread | felt, 2 threads | vtmake, 2 threads |',
         '|---|---:|---:|---:|']
for target in sorted({r['target'] for r in two.values()}):
    counts = []
    for results in (one, two, vt):
        rows = [r for r in results.values() if r['target'] == target]
        counts.append(f"{sum(r['status'] == 'PASS' for r in rows)}/{len(rows)}")
    lines.append('| ' + target + ' | ' + ' | '.join(counts) + ' |')
lines += ['| **Total** | ' + ' | '.join(f"**{sum(r['status'] == 'PASS' for r in results.values())}/{len(results)}**" for results in (one, two, vt)) + ' |', '',
          '## Time and run records', '']
for label, d in [('felt, 1 thread', a.felt_one), ('felt, 2 threads', a.felt_two), ('vtmake, 2 threads', a.vtmake)]:
    summary = json.loads((d / 'summary.json').read_text())
    lines.append(f"- {label}: {summary['seconds']} seconds. Records: `{d}`.")
lines += ['', 'The runs use 30 workers on CPUs 2–31. Build time is not in these times.',
          'Each case has a 180-second limit. Each run has tool hashes and the source revision',
          'in `metadata.json`. `results.tsv` and `results.jsonl` give each command and result.', '', '## felt failures', '']
for r in two.values():
    if r['status'] != 'PASS':
        lines.append(f"- `{r['id']}`: exit {r['exit_code']}; stored JSON differs.")
lines += ['', '`allow-existing-test` passes at both thread counts in this run.',
          'An independent run with the full original Makefile (`make -k -j30 test`,',
          'with built tools marked up to date and `LC_ALL=C`) found only the same',
          'label point failure.', '', '## Options in failed vtmake cases', '',
          'Counts show cases with an option, not the cause of a failure. A failed input read',
          'can prevent the tool from reaching an option. Common test controls `-q`, `-f`,',
          'and `-o` are excluded. Short option values are grouped under the option.',
          'Special targets include only commands that Make reached.', '',
          '| Option | Failed cases | Cases with option |', '|---|---:|---:|']
failed, total = Counter(), Counter()
for r in vt.values():
    command = unquote(Path(r['id']).name.removesuffix('.json.check').replace('_', ' ')) if r['target'] == 'json.check' else r['command']
    opts = {'-a@'} if r['target'] == 'json.check' else set()
    for flag in re.findall(r'(?<![\w/])--?[A-Za-z][A-Za-z0-9@-]*', command):
        if flag.startswith('--'):
            flag = flag.split('=')[0]
        elif flag[1] in 'ap' and len(flag) > 2:
            flag = flag[:3]
        else:
            flag = flag[:2]
        if flag not in ('-q', '-f', '-o'):
            opts.add(flag)
    total.update(opts)
    if r['status'] != 'PASS':
        failed.update(opts)
for flag, count in sorted(failed.items(), key=lambda item: (-item[1], item[0])):
    lines.append(f'| `{flag}` | {count} | {total[flag]} |')
blocked = sum('option not supported yet: -a@' in r['reason'] for r in vt.values())
if blocked:
    lines += ['', f'{blocked} cases stop because vtmake rejects `-a@` (exit 101).',
              'Add this option first, then run the suite again to measure the other gaps.',
              'The other option counts show test coverage that this failure blocks.']
lines += ['', '## Thread count', '',
          'No test recipe fixes `TIPPECANOE_MAX_THREADS` in this source revision.',
          'The runner set it to 1 and 2 for all felt cases. The reference for vtmake is 2.',
          'The hashes below cover the bytes passed to `cmp`, before Make removes output.',
          'They detect output changes even when both runs fail against the stored JSON.', '',
          '| Case with changed output | 1 thread | 2 threads |', '|---|---|---|']
changes = []
for key, r in two.items():
    old = one[key]
    # parallel-test creates random data without a seed in the upstream recipe.
    if key == 'parallel-test':
        continue
    if r['checks'] != old['checks'] or r['status'] != old['status']:
        lines.append(f"| `{key}` | {old['status']} | {r['status']} |")
        changes.append(key)
if not changes:
    lines.append('| None observed | | |')
lines += ['', '`parallel-test` uses random input with no fixed seed. Its hashes cannot test',
          'thread dependence. Its comparisons pass within each felt run.', '',
          'These observations do not prove that all other runs are independent of thread count.',
          'The label point case also changes between repeated runs at the same thread count;',
          'its observed change cannot be assigned to the thread count alone.', '',
          '## Scope', '',
          f"This checkout has {sum(r['target'] == 'json.check' for r in two.values())} JSON cases and {sum(r['target'] != 'json.check' for r in two.values())} other targets.",
          'The count comes from the source files and Makefile, rather than the older plan count.',
          'No test data or generated tiles are stored in this repository.', '']
a.output.parent.mkdir(parents=True, exist_ok=True)
a.output.write_text('\n'.join(lines))
print('\n'.join(lines[:35]))
