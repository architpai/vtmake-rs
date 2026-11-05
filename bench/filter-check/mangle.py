#!/usr/bin/env python3
"""Test filter: change geometry, IDs, layers and attributes; drop and add features."""
import json, sys
z, x, y = (int(a) for a in sys.argv[1:4])
n = 0
out = []
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    f = json.loads(line)
    n += 1
    tip = f.setdefault('tippecanoe', {})
    if n % 5 == 0:
        continue  # drop
    g = f['geometry']
    def walk(c):
        if not c:
            return c
        if isinstance(c[0], (int, float)):
            return [c[0] + 0.01 * (n % 3), c[1] - 0.01 * (n % 2)]
        return [walk(d) for d in c]
    g['coordinates'] = walk(g['coordinates'])
    if g['type'] in ('Polygon', 'MultiPolygon') and n % 4 == 1:
        # the bounding box as a polygon with a hole
        pts = []
        def flat(c):
            if isinstance(c[0], (int, float)):
                pts.append(c)
            else:
                for d in c:
                    flat(d)
        flat(g['coordinates'])
        if not pts:
            pts = [[0, 0]]
        x0, y0 = min(p[0] for p in pts), min(p[1] for p in pts)
        x1, y1 = max(p[0] for p in pts), max(p[1] for p in pts)
        mx, my = (x0 + x1) / 2, (y0 + y1) / 2
        g['coordinates'] = [[[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]], [[mx, my], [mx, my + 0.1], [mx + 0.1, my + 0.1], [mx + 0.1, my], [mx, my]]]
        g['type'] = 'Polygon'
    p = f.get('properties') or {}
    p['n'] = n
    p['half'] = n / 2
    p['nested'] = {'a': [1, 2, {'b': None}], 's': 'x"y\\zé'}
    p['big'] = 18446744073709551615
    p['negbig'] = -9223372036854775808
    p.pop(next(iter(p)), None)
    f['properties'] = p
    if n % 3 == 0:
        f['id'] = n * 1000
    elif n % 3 == 1:
        f['id'] = -n
    if n % 7 == 0:
        tip['layer'] = 'seven'
    if n % 11 == 0:
        tip['layer'] = 11
    if n == 1 and 'FIRST_DROPPED' in __import__('os').environ:
        tip['dropped'] = True
    if n % 6 == 0:
        tip['index'] = 2 ** 64 - n
        tip['sequence'] = -n
    out.append(f)
for f in out:
    print(json.dumps(f))
if n:
    print(json.dumps({'type': 'FeatureCollection', 'features': [{'type': 'Feature', 'properties': {'added': z * 1000 + x}, 'tippecanoe': {'layer': 'added'}, 'geometry': {'type': 'MultiPoint', 'coordinates': [[0, 0], [1, 1]]}}]}))
