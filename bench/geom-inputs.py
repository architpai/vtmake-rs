#!/usr/bin/env python3
"""Make the inputs of bench/geom-check.sh (the geometry options).

Usage: geom-inputs.py OUT CORPUS PUBLINES

- Subsets of the public corpus (the first lines, read in place).
- Generated edge cases: rings with the wrong winding, self-intersections,
  holes, tiny polygons, long lines, lines across the date line, and
  polygons that are larger than a tile.

The files are in OUT (not in the repository). The output is the same for
each run (fixed random seed).
"""
import json
import math
import os
import random
import sys

out, corpus, publines = sys.argv[1], sys.argv[2], sys.argv[3]
os.makedirs(out, exist_ok=True)


def head(src, dst, n, strip_minzoom=False):
    path = os.path.join(out, dst)
    with open(src, encoding="utf-8") as f, open(path, "w", encoding="utf-8") as g:
        for i, line in enumerate(f):
            if i >= n:
                break
            if strip_minzoom:
                o = json.loads(line)
                o.pop("tippecanoe", None)
                line = json.dumps(o, ensure_ascii=False) + "\n"
            g.write(line)


head(os.path.join(corpus, "polygons.ndjson"), "polygons.ndjson", 4000)
head(os.path.join(corpus, "polygons.ndjson"), "polygons-nominzoom.ndjson", 4000, True)
head(os.path.join(corpus, "points.ndjson"), "points.ndjson", 3000, True)
head(publines, "lines.ndjson", 3000)

rnd = random.Random(4)


def feat(geom, props=None):
    return {"type": "Feature", "properties": props or {}, "geometry": geom}


def ring(cx, cy, r, n, clockwise=False, jitter=0.0):
    pts = []
    for k in range(n):
        a = 2 * math.pi * k / n
        rr = r * (1 + jitter * (rnd.random() - 0.5))
        pts.append([round(cx + rr * math.cos(a), 7), round(cy + rr * math.sin(a), 7)])
    if clockwise:
        pts.reverse()
    pts.append(pts[0])
    return pts


fs = []
# Polygons with both windings, holes, and holes with the wrong winding.
for i in range(300):
    cx, cy = 139.5 + rnd.random() * 0.4, 35.5 + rnd.random() * 0.3
    r = 10 ** rnd.uniform(-5, -2)
    cw = rnd.random() < 0.5
    rings = [ring(cx, cy, r, rnd.randint(3, 40), cw, 0.4)]
    if rnd.random() < 0.4:
        rings.append(ring(cx, cy, r * 0.4, rnd.randint(3, 20), not cw if rnd.random() < 0.7 else cw, 0.2))
    fs.append(feat({"type": "Polygon", "coordinates": rings}, {"i": i}))
# Bow ties and self-intersections.
for i in range(100):
    cx, cy = 139.5 + rnd.random() * 0.4, 35.5 + rnd.random() * 0.3
    r = 10 ** rnd.uniform(-4, -2)
    pts = [[cx - r, cy - r], [cx + r, cy + r], [cx + r, cy - r], [cx - r, cy + r], [cx - r, cy - r]]
    fs.append(feat({"type": "Polygon", "coordinates": [pts]}, {"bowtie": i}))
# Multipolygons of many tiny parts.
for i in range(50):
    cx, cy = 139.5 + rnd.random() * 0.4, 35.5 + rnd.random() * 0.3
    parts = []
    for k in range(rnd.randint(2, 30)):
        parts.append([ring(cx + rnd.random() * 0.01, cy + rnd.random() * 0.01, 10 ** rnd.uniform(-6, -4), 5)])
    fs.append(feat({"type": "MultiPolygon", "coordinates": parts}, {"multi": i}))
# Large polygons and long lines (many tiles).
for i in range(20):
    cx, cy = 139.0 + rnd.random(), 35.0 + rnd.random()
    fs.append(feat({"type": "Polygon", "coordinates": [ring(cx, cy, 0.3, 200, False, 0.3)]}, {"big": i}))
for i in range(60):
    x, y = 139.0 + rnd.random(), 35.0 + rnd.random()
    pts = []
    for k in range(rnd.randint(2, 300)):
        x += (rnd.random() - 0.5) * 0.01
        y += (rnd.random() - 0.5) * 0.01
        pts.append([round(x, 7), round(y, 7)])
    fs.append(feat({"type": "LineString", "coordinates": pts}, {"line": i}))
# Shared borders: a grid of squares with common edges.
for a in range(10):
    for b in range(10):
        x0, y0 = 139.6 + a * 0.003, 35.6 + b * 0.003
        sq = [[x0, y0], [x0 + 0.003, y0], [x0 + 0.003, y0 + 0.0015], [x0 + 0.003, y0 + 0.003], [x0, y0 + 0.003], [x0, y0]]
        fs.append(feat({"type": "Polygon", "coordinates": [sq]}, {"cell": a * 10 + b}))
with open(os.path.join(out, "edge.ndjson"), "w") as g:
    for f in fs:
        g.write(json.dumps(f) + "\n")

# The date line: lines and polygons with longitudes that jump.
fs = []
for i in range(40):
    y = -60 + rnd.random() * 120
    pts = [[170 + rnd.random() * 9, y], [-179 + rnd.random() * 9, y + 1], [175, y + 2], [-175, y + 3]]
    fs.append(feat({"type": "LineString", "coordinates": pts}, {"wrap": i}))
for i in range(20):
    y = -50 + rnd.random() * 100
    pts = [[178, y], [-178, y], [-178, y + 2], [178, y + 2], [178, y]]
    fs.append(feat({"type": "Polygon", "coordinates": [pts]}, {"wrappoly": i}))
fs.append(feat({"type": "LineString", "coordinates": [[-180, 0], [180, 0]]}, {"world": 1}))
with open(os.path.join(out, "wrap.ndjson"), "w") as g:
    for f in fs:
        g.write(json.dumps(f) + "\n")
