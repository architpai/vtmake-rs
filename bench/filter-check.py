#!/usr/bin/env python3
"""Compare felt and vtmake byte for byte on shell-filter commands .

For each case: exit code, stderr lines (sorted), tile blobs, metadata
(without generator and generator_options), and the files that the tee
filters write (the exact filter input text). The filters are in
bench/filter-check/; felt's own filters come from its tests/filter.

Usage: bench/filter-check.py WORK [CASE-TEXT]
  WORK       a new folder for the outputs
  CASE-TEXT  run only the cases whose name contains this text
Env: FELT (felt tippecanoe binary; no default),
     VTMAKE (default target/release/vtmake), S (felt source tree; no default).
Exit: 0 if all cases are equal, 1 if not."""
import json, os, pathlib, shutil, sqlite3, subprocess, sys

HERE = pathlib.Path(__file__).resolve().parent
if not os.environ.get('S'):
    sys.exit('set S to the felt source tree')
if not os.environ.get('FELT'):
    sys.exit('set FELT to the felt tippecanoe binary')
S = pathlib.Path(os.environ['S'])
F = HERE / 'filter-check'
FELT = os.environ['FELT']
VTMAKE = os.environ.get('VTMAKE', str(HERE.parent / 'target/release/vtmake'))
OUT = pathlib.Path(sys.argv[1]).resolve()
ONLY = sys.argv[2] if len(sys.argv) > 2 else ''
PLACES = str(S / 'tests/ne_110m_populated_places/in.json')
COUNTRIES = str(S / 'tests/ne_110m_admin_0_countries/in.json.gz')
ROADS = str(S / 'tests/tl_2018_51685_roads/in.json')
ACC = str(S / 'tests/accumulate/in.json')
T = S / 'tests/filter'

def f(name):
    # The filter arguments z, x, y go to the `sh -c` text.
    return str(F / name) + ' "$@"'

CASES = [
    # felt's own filter commands
    ('cat-pre-places', ['-yNAME', '-Ccat', '-z5', PLACES]),
    ('cat-post-places', ['-yNAME', '-z5', '-ccat', PLACES]),
    ('cat-pre-countries', ['--coalesce', '-z2', '-Ccat', COUNTRIES]),
    ('null-pre', ['-z0', '-C' + str(T / 'null'), PLACES]),
    ('null-post', ['-yNAME', '-z0', '-c' + str(T / 'null'), PLACES]),
    ('remove-pre', ['-yNAME', '-z4', '-C' + str(T / 'remove'), PLACES]),
    ('remove-post', ['-z4', '-yNAME', '-c' + str(T / 'remove'), PLACES]),
    ('rename-pre', ['-yNAME', '-z4', '-C' + str(T / 'rename'), PLACES]),
    ('rename-post', ['-yNAME', '-z5', '-c' + str(T / 'rename'), PLACES]),
    ('rename-both', ['-yNAME', '-z4', '-C' + str(T / 'rename'), '-c' + str(T / 'rename2'), PLACES]),
    # the exact filter input text
    ('tee-pre-places', ['-z5', '-C' + f('tee-pre'), PLACES]),
    ('tee-post-places', ['-z5', '-c' + f('tee-post'), PLACES]),
    ('tee-both-countries', ['-z3', '-C' + f('tee-pre'), '-c' + f('tee-post'), COUNTRIES]),
    ('tee-both-roads', ['-z12', '-Z9', '-C' + f('tee-pre'), '-c' + f('tee-post'), ROADS]),
    ('tee-pre-roads-3857', ['-z10', '-C' + f('tee-pre'), '-c' + f('tee-post'), '-s', 'EPSG:3857', str(S / 'tests/epsg-3857/in.json')]),
    ('tee-pre-ai', ['-z4', '-ai', '-C' + f('tee-pre'), PLACES]),
    ('tee-pre-an', ['-z3', '-an', '-M', '20000', '-C' + f('tee-pre'), COUNTRIES]),
    ('tee-pre-zg', ['-zg', '-C' + f('tee-pre'), COUNTRIES]),
    ('tee-pre-j', ['-z3', '-j', '{"*":[">=","SCALERANK",3]}', '-C' + f('tee-pre'), PLACES]),
    ('tee-pre-minzoom', ['-z5', '-Z2', '-C' + f('tee-pre'), '-c' + f('tee-post'), PLACES]),
    # filters that drop, change, add features and attributes
    ('drop-pre', ['-z4', '-C' + f('drop-half'), PLACES]),
    ('drop-post', ['-z4', '-c' + f('drop-half'), COUNTRIES]),
    ('change-pre', ['-z3', '-C' + f('change-attrs'), PLACES]),
    ('change-post', ['-z3', '-c' + f('change-attrs'), PLACES]),
    ('change-both-roads', ['-z12', '-Z10', '-C' + f('change-attrs'), '-c' + f('change-attrs'), ROADS]),
    ('add-pre', ['-z3', '-C' + f('add-features'), COUNTRIES]),
    ('add-post', ['-z3', '-c' + f('add-features'), COUNTRIES]),
    ('reverse-pre-pi', ['-z4', '-pi', '-C' + f('reverse'), PLACES]),
    ('reverse-pre', ['-z4', '-C' + f('reverse'), PLACES]),
    # failures and empty tiles
    ('nothing-pre', ['-z3', '-C' + f('nothing'), PLACES]),
    ('nothing-post', ['-z3', '-c' + f('nothing'), PLACES]),
    ('fail-pre', ['-z2', '-C' + f('fail'), PLACES]),
    ('fail-post', ['-z2', '-c' + f('fail'), COUNTRIES]),
    ('bad-json-pre', ['-z1', '-C' + f('bad-json'), PLACES]),
    ('bad-json-post', ['-z1', '-c' + f('bad-json'), PLACES]),
    ('no-geometry-pre', ['-z1', '-C' + f('no-geometry'), PLACES]),
    ('no-geometry-post', ['-z1', '-c' + f('no-geometry'), PLACES]),
    ('empty-geom-pre', ['-z2', '-C' + f('empty-geom'), PLACES]),
    ('empty-geom-post', ['-z2', '-c' + f('empty-geom'), PLACES]),
    # filters with other tile options
    ('cat-pre-densest', ['-z5', '-M', '20000', '--drop-densest-as-needed', '-Ccat', COUNTRIES]),
    ('cat-post-densest', ['-z5', '-M', '20000', '--drop-densest-as-needed', '-ccat', COUNTRIES]),
    ('cat-both-fraction', ['-z5', '-M', '2000', '--drop-fraction-as-needed', '-Ccat', '-ccat', PLACES]),
    ('cat-pre-coalesce-densest', ['-z5', '-M', '20000', '--coalesce-densest-as-needed', '-Ccat', COUNTRIES]),
    ('cat-pre-accum', ['-z3', '-r1', '-K100', '-Ethesum:sum', '-Ethemax:max', '-Ccat', ACC]),
    ('cat-pre-reorder', ['-z3', '--coalesce', '--reorder', '-Ccat', COUNTRIES]),
    ('cat-pre-limit', ['-z4', '--limit-tile-feature-count', '5', '-Ccat', '-ccat', PLACES]),
    ('cat-pre-extend', ['-z2', '-M', '8000', '--drop-densest-as-needed', '--extend-zooms-if-still-dropping', '-Ccat', PLACES]),
    ('cat-pre-ad-roads', ['-z13', '-Z11', '-M', '3000', '--drop-fraction-as-needed', '-Ccat', ROADS]),
    ('cat-pre-multiplier', ['-z4', '--retain-points-multiplier', '3', '-Ccat', PLACES]),
    ('cat-post-countries', ['-z2', '-ccat', COUNTRIES]),
    # a filter that changes geometry, IDs, layers and attributes
    ('mangle-pre-countries', ['-z4', '-C' + f('mangle.py'), COUNTRIES]),
    ('mangle-post-countries', ['-z4', '-c' + f('mangle.py'), COUNTRIES]),
    ('mangle-both-countries', ['-z4', '-C' + f('mangle.py'), '-c' + f('mangle.py'), COUNTRIES]),
    ('mangle-pre-places', ['-z5', '-C' + f('mangle.py'), PLACES]),
    ('mangle-post-roads', ['-z13', '-Z11', '-c' + f('mangle.py'), ROADS]),
    ('mangle-pre-roads', ['-z13', '-Z11', '-C' + f('mangle.py'), ROADS]),
    ('mangle-both-coalesce', ['-z4', '--coalesce', '--reorder', '-C' + f('mangle.py'), '-c' + f('mangle.py'), COUNTRIES]),
    ('mangle-pre-accum', ['-z3', '-r1', '-K100', '-Ethesum:sum', '-Ethemean:mean', '-Etheconcat:concat', '-C' + f('mangle.py'), ACC]),
    ('mangle-both-accum', ['-z3', '-r1', '-K100', '-Ethesum:sum', '-Ethemean:mean', '-C' + f('mangle.py'), '-c' + f('mangle.py'), ACC]),
    ('broken-pre-countries', ['-z1', '-C' + f('broken'), COUNTRIES]),
    ('broken-post-countries', ['-z1', '-c' + f('broken'), COUNTRIES]),
    ('two-layers-rename', ['-z3', '-L', 'a:' + PLACES, '-L', 'b:' + COUNTRIES, '-C' + str(T / 'rename'), '-c' + f('drop-half')]),
    ('both-densest-M', ['-z5', '-M', '20000', '--drop-densest-as-needed', '-Ccat', '-ccat', COUNTRIES]),
    ('both-smallest-M', ['-z5', '-M', '20000', '--drop-smallest-as-needed', '-Ccat', '-ccat', COUNTRIES]),
    ('pre-variable-depth', ['-z8', '-Z2', '--generate-variable-depth-tile-pyramid', '-Ccat', PLACES]),
    ('post-variable-depth', ['-z8', '-Z2', '--generate-variable-depth-tile-pyramid', '-ccat', PLACES]),
    ('pre-gamma', ['-z5', '-g', '2', '-C' + f('mangle.py'), PLACES]),
    ('pre-cluster', ['-z5', '-K', '20', '-C' + f('mangle.py'), PLACES]),
    ('pre-pi-mangle', ['-z5', '-pi', '-C' + f('mangle.py'), PLACES]),
    ('pre-extra-detail', ['-z3', '--extra-detail', '14', '-C' + f('mangle.py'), '-c' + f('mangle.py'), COUNTRIES]),
    ('post-detail-z14', ['-z14', '-Z14', '-d', '14', '-c' + f('tee-post'), ROADS]),
    ('pre-feature-limit', ['-z3', '-O', '50', '--drop-densest-as-needed', '-C' + f('mangle.py'), PLACES]),
]

# Known differences that do not come from the filters: vtmake's variable-depth
# tiles also differ from felt without a filter.
KNOWN = {'pre-variable-depth', 'post-variable-depth'}


def run(name, tool, args):
    d = OUT / name / tool
    if d.exists():
        shutil.rmtree(d)
    (d / 'capture').mkdir(parents=True)
    cmd = [FELT] if tool == 'felt' else [VTMAKE, 'build']
    env = dict(os.environ, TIPPECANOE_MAX_THREADS='2', LC_ALL='C', FILTER_DIR=str(d / 'capture'))
    p = subprocess.run(cmd + ['-q', '-f', '-o', 'out.mbtiles'] + args, cwd=d, env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=600)
    tiles, meta = [], {}
    if (d / 'out.mbtiles').exists():
        with sqlite3.connect(d / 'out.mbtiles') as c:
            try:
                tiles = list(c.execute('select zoom_level, tile_column, tile_row, tile_data from tiles order by 1,2,3'))
                meta = dict(c.execute('select name, value from metadata'))
            except sqlite3.Error:
                pass
    for k in ['generator', 'generator_options']:
        meta.pop(k, None)
    cap = {q.name: q.read_bytes() for q in sorted((d / 'capture').iterdir())}
    err = sorted(p.stderr.decode(errors='replace').splitlines())
    return dict(rc=p.returncode, tiles=tiles, meta=meta, cap=cap, err=err)


rows = []
for name, args in CASES:
    if ONLY and ONLY not in name:
        continue
    a, b = run(name, 'felt', args), run(name, 'vtmake', args)
    row = dict(case=name, known=name in KNOWN, equal=a == b, rc=(a['rc'], b['rc']), tiles=len(a['tiles']), tiles_equal=a['tiles'] == b['tiles'],
               meta_equal=a['meta'] == b['meta'], captures=len(a['cap']), captures_equal=a['cap'] == b['cap'], stderr_equal=a['err'] == b['err'])
    if not row['equal']:
        row['diff'] = [k for k in a if a[k] != b[k]]
        if a['err'] != b['err']:
            row['stderr'] = (a['err'][:3], b['err'][:3])
        if a['meta'] != b['meta']:
            row['meta_keys'] = [k for k in set(a['meta']) | set(b['meta']) if a['meta'].get(k) != b['meta'].get(k)]
        if a['cap'] != b['cap']:
            row['cap_diff'] = [k for k in set(a['cap']) | set(b['cap']) if a['cap'].get(k) != b['cap'].get(k)][:5]
    rows.append(row)
    print(json.dumps(row), flush=True)
(OUT / 'results.json').write_text(json.dumps(rows, indent=1))
print(json.dumps({'equal': sum(r['equal'] for r in rows), 'total': len(rows), 'known': sum(r['known'] for r in rows)}))
sys.exit(0 if all(r['equal'] or r['known'] for r in rows) else 1)
