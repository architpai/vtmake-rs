#!/usr/bin/env python3
"""Compare forced dynamic drops with felt. Keep all data outside the checkout."""
import argparse
import itertools
import json
import os
from pathlib import Path
import sqlite3
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--work', type=Path, required=True)
p.add_argument('--vtmake', type=Path, default=Path('target/release/vtmake'))
p.add_argument('--felt', default=os.environ.get('FELT'), help='felt tippecanoe binary (default: $FELT)')
p.add_argument('--lines', type=int, default=10000)
p.add_argument('--cpus', default='2-31')
p.add_argument('--public', type=Path, default=os.environ.get('P'),
               help='public corpus folder with points-plain.ndjson and polygons-plain.ndjson (default: $P)')
p.add_argument('--roads', type=Path, default=os.environ.get('PL'),
               help='pub-lines.ndjson from bench/prep_lines.py (default: $PL)')
a = p.parse_args()
if not a.felt:
    p.error('set --felt or FELT to the felt tippecanoe binary')
if not a.public:
    p.error('set --public or P to the public corpus folder')
if not a.roads:
    p.error('set --roads or PL to pub-lines.ndjson from bench/prep_lines.py')
a.work.mkdir(parents=True, exist_ok=True)
a.vtmake = a.vtmake.resolve()
pub = Path(a.public)
sources = {'points': pub / 'points-plain.ndjson', 'polygons': pub / 'polygons-plain.ndjson',
           'lines': Path(a.roads)}
strategies = [['-as'], ['-ad'], ['-an'], ['-aG'], ['-pd'],
              ['--drop-by-attribute-as-needed=priority'],
              ['--drop-by-attribute-as-needed=priority', '--drop-by-attribute-order=desc']]
results = []
for kind, source in sources.items():
    data = a.work / (kind + '.ndjson')
    # Use the public geometry. Add a numeric value to test attribute thresholds.
    with source.open() as src, data.open('w') as dst:
        for i, line in enumerate(itertools.islice(src, a.lines)):
            f = json.loads(line)
            f.pop('tippecanoe', None)
            f.setdefault('properties', {})['priority'] = i
            dst.write(json.dumps(f, ensure_ascii=False, separators=(',', ':')) + '\n')
    cases = [(f'{i}-{limit}', flags + [limit, '-z2'])
             for i, flags in enumerate(strategies) for limit in ['-O100', '-M1000']]
    cases += [('extend', ['-as', '-M1000', '-z0', '--extend-zooms-if-still-dropping-maximum=2']),
              ('hilbert', ['-ah', '-as', '-O100', '-z2']),
              ('order', ['-z2', '--order-descending-by=priority']),
              ('input-order', ['-pi', '-z2']),
              ('truncate', ['--limit-tile-feature-count=100', '-z2'])]
    if kind != 'points':
        cases = [(name, [x for x in flags if not x.startswith('-z')] + ['-Z10', '-z12']) for name, flags in cases if name != 'extend']
    for name, flags in cases:
        name = kind + '-' + name
        outputs = []
        for label, cmd in [('felt', [a.felt]), ('vtmake', [str(a.vtmake), 'build'])]:
            db = a.work / (name + '-' + label + '.mbtiles')
            with (a.work / (name + '-' + label + '.log')).open('w') as log:
                rc = subprocess.run(['taskset', '-c', a.cpus, *cmd, '-q', '-f', '-l', 'test', *flags,
                                     '-o', str(db), str(data)], env=dict(os.environ, TIPPECANOE_MAX_THREADS='2'),
                                    stdout=log, stderr=subprocess.STDOUT).returncode
            with sqlite3.connect(db) as conn:
                tiles = list(conn.execute('select * from tiles order by 1,2,3'))
                meta = dict(conn.execute('select * from metadata'))
            outputs.append((rc, tiles, meta))
        f, v = outputs
        equal = f[0] == v[0] and f[1] == v[1]
        row = dict(name=name, flags=flags, felt_exit=f[0], vtmake_exit=v[0],
                   felt_tiles=len(f[1]), vtmake_tiles=len(v[1]),
                   equal_tiles=sum(x == y for x, y in zip(f[1], v[1])), equal=equal,
                   strategies_equal=f[2].get('strategies') == v[2].get('strategies'))
        results.append(row)
        print(json.dumps(row), flush=True)
(a.work / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(dict(passed=sum(r['equal'] for r in results), total=len(results),
                      tiles=sum(r['equal_tiles'] for r in results))), flush=True)
raise SystemExit(any(not r['equal'] for r in results))
