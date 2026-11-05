#!/usr/bin/env python3
"""Check density threshold exhaustion with a time limit."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--work', type=Path, required=True)
p.add_argument('--vtmake', type=Path, default=Path('target/release/vtmake'))
p.add_argument('--felt', default=os.environ.get('FELT'), help='felt tippecanoe binary (default: $FELT)')
p.add_argument('--full', type=Path, metavar='NDJSON', help='Also check this large polygon input (for example one million polygons)')
a = p.parse_args()
if not a.felt:
    p.error('set --felt or FELT to the felt tippecanoe binary')
a.work.mkdir(parents=True, exist_ok=True)
data = a.work / 'polygons.ndjson'
with data.open('w') as out:
    for i in range(100):
        x = i * 0.1
        out.write(json.dumps(dict(type='Feature', properties=dict(i=i), geometry=dict(type='Polygon', coordinates=[[[x, 0], [x+.05, 0], [x+.05, .05], [x, 0]]]))) + '\n')
cases = [('small', data, ['-z4', '-pt', '-M1'], 10)]
if a.full:
    cases.append(('full', a.full, ['-zg'], 1800))
rows = []
for name, source, flags, limit in cases:
    for label, command in [('felt', [a.felt]), ('vtmake', [str(a.vtmake.resolve()), 'build'])]:
        timeout = min(limit, 1100) if label == 'vtmake' else limit
        start = time.monotonic()
        log = a.work / f'{name}-{label}.log'
        with log.open('w') as output:
            try:
                code = subprocess.run(['taskset', '-c', '4,5', *command, '-q', '-f', '-l', 'b',
                                       '--coalesce-densest-as-needed', *flags, '-o',
                                       str(a.work / f'{name}-{label}.mbtiles'), str(source)],
                                      env=dict(os.environ, TIPPECANOE_MAX_THREADS='2'),
                                      stdout=output, stderr=output, timeout=timeout).returncode
            except subprocess.TimeoutExpired:
                code = 124
        row = dict(case=name, tool=label, code=code, seconds=time.monotonic()-start,
                   passed=code == 100 and "Can't increase feature gap threshold further" in log.read_text())
        rows.append(row)
        print(json.dumps(row), flush=True)
(a.work / 'results.json').write_text(json.dumps(rows, indent=2)+'\n')
raise SystemExit(not all(row['passed'] for row in rows))
