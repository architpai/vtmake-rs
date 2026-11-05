#!/usr/bin/env python3
"""Compare PMTiles output bytes and enumerate output with felt."""
import os
import pathlib
import subprocess
import sys
import sqlite3
import random

work, felt, vt, cpus = sys.argv[1:]
root = pathlib.Path(work)
same = total = 0

def check(name, command):
    global same, total
    total += 1
    results = []
    for kind in ('felt', 'vt'):
        cwd = root / 'pmtiles' / kind
        cwd.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ, PATH=str(root / ('bin-' + kind)) + ':' + os.environ['PATH'], TIPPECANOE_MAX_THREADS='2')
        p = subprocess.run(['taskset', '-c', cpus, *command], cwd=cwd, env=env, capture_output=True)
        archive = cwd / 'out.pmtiles'
        # felt with --read-parallel can write the same warning one or two times
        # (a race between its reader threads), so join equal adjacent lines.
        err = b''.join(l for i, l in enumerate(p.stderr.splitlines(True)) if i == 0 or l != p.stderr.splitlines(True)[i - 1])
        results.append((p.returncode, p.stdout, err, archive.read_bytes() if archive.exists() else None))
        archive.unlink(missing_ok=True)
    if results[0] == results[1]:
        same += 1
        print('PASS pmtiles-' + name)
    else:
        print('DIFF pmtiles-' + name)
        for kind, result in zip(('felt', 'vt'), results):
            (root / 'pmtiles' / (name + '-' + kind + '.log')).write_text(repr(result[:3]))

# Force both leaf directories and repeated-content runs with small valid MVTs.
def varint(n):
    b = bytearray()
    while n >= 128:
        b.append((n & 127) | 128)
        n >>= 7
    b.append(n)
    return bytes(b)

def field(tag, data):  # a length-delimited protobuf field
    return varint(tag * 8 + 2) + varint(len(data)) + data

def fixture(name, count, zoom, repeated):
    path = root / 'pmtiles' / (name + '.mbtiles')
    path.parent.mkdir(parents=True, exist_ok=True)
    path.unlink(missing_ok=True)
    db = sqlite3.connect(path)
    db.executescript('CREATE TABLE metadata(name text,value text); CREATE TABLE tiles(zoom_level integer,tile_column integer,tile_row integer,tile_data blob);')
    db.executemany('INSERT INTO metadata VALUES (?,?)', [('name', name), ('minzoom', str(zoom)), ('maxzoom', str(zoom)), ('center', '0,0,' + str(zoom))])
    rng = random.Random(19)
    coords = set()
    while len(coords) < count:
        coords.add((rng.randrange(1 << zoom), rng.randrange(1 << zoom)))
    for n, (x, y) in enumerate(sorted(coords)):
        feature = b'\x18\x01\x08' + varint(1 if repeated else n) + field(4, b'\x09\x80\x10\x80\x10')
        layer = field(1, b'fixture') + field(2, feature) + b'\x28\x80\x20\x78\x02'
        db.execute('INSERT INTO tiles VALUES (?,?,?,?)', (zoom, x, y, field(3, layer)))
    db.commit()
    db.close()
    return str(path)

for name, count, zoom, repeated in [('leaves', 18000, 20, False), ('runs', 64, 3, True)]:
    src = fixture(name, count, zoom, repeated)
    check(name, ['tile-join', '-q', '-f', '-o', 'out.pmtiles', src])

for name in ('types', 'multi', 'multigeom', 'numbers', 'antimeridian', 'pub-poly', 'pub-lines', 'pub-points'):
    src = root / 'ts' / (name + '-felt.mbtiles')
    source = root / 'in' / (name + '.ndjson')
    if source.exists():
        check(name + '-build-pmtiles', ['tippecanoe', '-q', '-f', '-Z12', '-z16', '--read-parallel', '--buffer=5', '--drop-densest-as-needed', '--extend-zooms-if-still-dropping', '--no-feature-limit', '--no-tile-size-limit', '-l', 'fixture', '-o', 'out.pmtiles', str(source)])
    for opts in ([], ['-pC'], ['-pg'], ['-Z13', '-z14']):
        check(name + '-pmtiles-' + '-'.join(opts), ['tile-join', '-q', '-f', '-o', 'out.pmtiles', *opts, str(src)])
    check(name + '-enumerate', ['tippecanoe-enumerate', str(src)])
check('enumerate-no-args', ['tippecanoe-enumerate'])
check('enumerate-option', ['tippecanoe-enumerate', '--nothing'])
check('enumerate-missing', ['tippecanoe-enumerate', 'missing.mbtiles'])
check('enumerate-multiple', ['tippecanoe-enumerate', str(root / 'ts/types-felt.mbtiles'), str(root / 'ts/multi-felt.mbtiles')])
corrupt = root / 'pmtiles/corrupt.mbtiles'
corrupt.write_bytes(b'This is not a database')
check('enumerate-corrupt', ['tippecanoe-enumerate', str(corrupt)])
for zoom in (-1, 32):
    path = root / 'pmtiles' / ('zoom-' + str(zoom) + '.mbtiles')
    path.unlink(missing_ok=True)
    db = sqlite3.connect(path)
    db.executescript('CREATE TABLE tiles(zoom_level integer,tile_column integer,tile_row integer);')
    db.execute('INSERT INTO tiles VALUES (?,0,0)', (zoom,))
    db.commit()
    db.close()
    check('enumerate-zoom-' + str(zoom), ['tippecanoe-enumerate', str(path)])

print(f'{total} {same}')
sys.exit(total != same)
