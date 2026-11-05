#!/usr/bin/env python3
"""Compare edge cases with felt: partial output, zoom extension, and strategies.

--cases is a folder with big-polys.ndjson (large polygons: the feature limit
cannot be met) and rand-points.ndjson (random points with a numeric property
"v"). Keep all output outside Git.
Run this command inside a scope with MemoryMax=16G and MemorySwapMax=0.
"""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--cases', type=Path, required=True)
p.add_argument('--work', type=Path, required=True)
p.add_argument('--vtmake', type=Path, default=Path('target/release/vtmake'))
p.add_argument('--felt', default=os.environ.get('FELT'), help='felt tippecanoe binary (default: $FELT)')
a = p.parse_args()
if not a.felt:
    p.error('set --felt or FELT to the felt tippecanoe binary')
a.work.mkdir(parents=True, exist_ok=True)
rows = []
cases = [
    ('gap-exhausted', 'big-polys.ndjson', ['-z6', '-as', '-M3000'], 100),
    ('area-exhausted', 'big-polys.ndjson', ['-z6', '-an', '-M3000'], 100),
    ('dynamic-extend', 'rand-points.ndjson', ['-z4', '-pd', '-ae', '-M20000'], 0),
    ('densest-attribute', 'rand-points.ndjson', ['-z6', '-as', '-M20000', '--drop-by-attribute-as-needed=v'], 0),
    ('smallest-attribute', 'rand-points.ndjson', ['-z6', '-an', '-M20000', '--drop-by-attribute-as-needed=v'], 0),
    ('failed-center', 'rand-points.ndjson', ['-z4', '-aG', '-ae', '-M20000'], 100),
]
for name, source, flags, expected in cases:
    outputs = []
    for label, cmd in [('felt', [a.felt]), ('vtmake', [str(a.vtmake.resolve()), 'build'])]:
        db = a.work / f'{name}-{label}.mbtiles'
        with (a.work / f'{name}-{label}.log').open('w') as log:
            rc = subprocess.run(['taskset', '-c', '2-31', *cmd, '-f', '-n', 'test', '-N', 'test',
                                 '-l', 'test', *flags, '-t', str(a.work), '-o', str(db),
                                 str(a.cases / source)], env=dict(os.environ, TIPPECANOE_MAX_THREADS='2'),
                                stdout=log, stderr=log, timeout=180).returncode
        # A read/write connection rolls back felt's hot journal after exit 100.
        with sqlite3.connect(db) as c:
            tiles = list(c.execute('select zoom_level,tile_column,tile_row,tile_data from tiles order by 1,2,3'))
            meta = dict(c.execute('select * from metadata'))
        for key in ['generator', 'generator_options']:
            meta.pop(key, None)
        outputs.append((rc, tiles, meta))
    f, v = outputs
    row = dict(case=name, felt_exit=f[0], vtmake_exit=v[0], felt_tiles=len(f[1]), vtmake_tiles=len(v[1]),
               bytes_equal=f[1] == v[1], metadata_equal=f[2] == v[2])
    row['passed'] = f == v and f[0] == expected
    rows.append(row)
    print(json.dumps(row), flush=True)
(a.work / 'results.json').write_text(json.dumps(rows, indent=2) + '\n')
raise SystemExit(not all(r['passed'] for r in rows))
