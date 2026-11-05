#!/usr/bin/env python3
"""Synthetic inputs for bench/meta-check.sh: attribute types, layers, zoom ranges, edges."""
import json, random, sys, os
out = sys.argv[1]
random.seed(7)
def pt(lon, lat): return {"type": "Point", "coordinates": [lon, lat]}
def sq(lon, lat, d):
    return {"type": "Polygon", "coordinates": [[[lon, lat], [lon + d, lat], [lon + d, lat + d], [lon, lat + d], [lon, lat]]]}
def ln(lon, lat, d): return {"type": "LineString", "coordinates": [[lon, lat], [lon + d, lat + d / 2], [lon + 2 * d, lat]]}
def w(name, feats):
    with open(os.path.join(out, name), "w") as f:
        for x in feats:
            f.write(json.dumps(x, ensure_ascii=False) + "\n")
# 1. types: numbers, booleans, nulls, nested, unicode, long strings, many distinct values, mixed.
fs = []
for i in range(3000):
    lon, lat = 139.6 + random.random() * 0.2, 35.6 + random.random() * 0.2
    p = {
        "int": i, "neg": -i, "float": i / 7.0, "big": 10 ** (i % 25), "small": 1.5e-7 * i,
        "flag": i % 2 == 0, "nul": None, "allnull": None,
        "nested": {"a": i % 5, "b": [1, 2, {"c": "x"}]}, "arr": [i % 3, "y", None, True],
        "uni": random.choice(["東京", "Zürich", "😀", "a\"b", "back\\slash", "tab\tx", "ctl\u0001", "é"]),
        "long": "L" * (250 + i % 10),
        "mixed": [1, "1", True, 2.5, "x", None][i % 6],
        "uuid": "%032x" % random.getrandbits(128),
        "cat": "c%d" % (i % 50),
        "numstr": str(i % 20),
        "exp": float("1e%d" % (i % 40 - 20)),
    }
    if i % 7 == 0: p["sometimes"] = i
    t = i % 3
    g = pt(lon, lat) if t == 0 else (sq(lon, lat, 0.0005) if t == 1 else ln(lon, lat, 0.0005))
    f = {"type": "Feature", "geometry": g, "properties": p, "tippecanoe": {"minzoom": 12}}
    if i % 11 == 0: f["id"] = i
    fs.append(f)
fs.append({"type": "Feature", "geometry": pt(139.7, 35.7), "properties": {"k": 1, "k": 2}})
w("types.ndjson", fs)
# 2. multi-layer, layer order and default layer.
fs = []
for i in range(2000):
    lon, lat = 139.6 + random.random() * 0.1, 35.6 + random.random() * 0.1
    l = ["zeta", "alpha", "Beta", "_x", None, "ünï"][i % 6]
    tc = {"minzoom": 12 + i % 3}
    if l: tc["layer"] = l
    t = i % 3
    g = pt(lon, lat) if t == 0 else (sq(lon, lat, 0.0003) if t == 1 else ln(lon, lat, 0.0003))
    fs.append({"type": "Feature", "geometry": g, "properties": {"n": i, "l": l or "def"}, "tippecanoe": tc})
w("multi.ndjson", fs)
# 3. maxzoom below -z.
fs = [{"type": "Feature", "geometry": sq(139.6 + random.random() * 0.1, 35.6 + random.random() * 0.1, 0.0004),
       "properties": {"i": i}, "tippecanoe": {"minzoom": 12, "maxzoom": 15}} for i in range(1000)]
w("maxz-low.ndjson", fs)
# 4. points without minzoom (rate drop), some with only maxzoom.
fs = []
for i in range(5000):
    f = {"type": "Feature", "geometry": pt(139.5 + random.random() * 0.3, 35.5 + random.random() * 0.3), "properties": {"i": i}}
    if i % 4 == 0: f["tippecanoe"] = {"maxzoom": 17}
    fs.append(f)
w("rate.ndjson", fs)
# 5. multi geometries and GeometryCollection, features with id.
fs = []
for i in range(600):
    lon, lat = 139.6 + random.random() * 0.1, 35.6 + random.random() * 0.1
    k = i % 4
    if k == 0: g = {"type": "MultiPoint", "coordinates": [[lon, lat], [lon + 0.001, lat], [lon, lat + 0.002]]}
    elif k == 1: g = {"type": "MultiLineString", "coordinates": [ln(lon, lat, 0.001)["coordinates"], ln(lon, lat + 0.003, 0.001)["coordinates"]]}
    elif k == 2: g = {"type": "GeometryCollection", "geometries": [pt(lon, lat), ln(lon, lat, 0.0005), sq(lon, lat, 0.0005)]}
    else: g = {"type": "MultiPolygon", "coordinates": [sq(lon, lat, 0.0005)["coordinates"], sq(lon + 0.0003, lat + 0.0003, 0.0005)["coordinates"]]}
    f = {"type": "Feature", "geometry": g, "properties": {"i": i}, "tippecanoe": {"minzoom": 11}}
    if i % 3 == 0: f["id"] = i * 1000
    elif i % 3 == 1: f["id"] = -5
    fs.append(f)
w("multigeom.ndjson", fs)
# 6. tiny bbox crossing a tile edge at -Z 12 (z12 tile edge x: lon of tile 3638 at z12).
import math
def tile2lon(x, z): return x / 2 ** z * 360 - 180
def tile2lat(y, z): return math.degrees(math.atan(math.sinh(math.pi * (1 - 2 * y / 2 ** z))))
e_lon, e_lat = tile2lon(3638, 12), tile2lat(1613, 12)
fs = []
for i in range(300):
    lon, lat = e_lon + (random.random() - 0.5) * 0.002, e_lat + (random.random() - 0.5) * 0.002
    t = i % 3
    g = pt(lon, lat) if t == 0 else (sq(lon, lat, 0.0002) if t == 1 else ln(lon, lat, 0.0002))
    fs.append({"type": "Feature", "geometry": g, "properties": {"i": i}, "tippecanoe": {"minzoom": 12}})
w("edge.ndjson", fs)
# 7. a one-point input.
w("one.ndjson", [{"type": "Feature", "geometry": pt(139.7, 35.7), "properties": {"a": "b"}}])
# 8. only invalid features, and empty input.
with open(os.path.join(out, "invalid.ndjson"), "w") as f:
    f.write('{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]}}\n')
    f.write('{"type":"Feature","properties":{}}\n')
open(os.path.join(out, "empty.ndjson"), "w").close()
# 9. minzoom > maxzoom and maxzoom < -Z.
fs = []
for i in range(500):
    lon, lat = 139.6 + random.random() * 0.1, 35.6 + random.random() * 0.1
    tc = [{"minzoom": 16, "maxzoom": 14}, {"maxzoom": 10}, {"minzoom": 20}, {"minzoom": 13, "maxzoom": 13}, {}][i % 5]
    fs.append({"type": "Feature", "geometry": sq(lon, lat, 0.0004), "properties": {"i": i, "k": i % 5}, "tippecanoe": tc})
w("zoomrange.ndjson", fs)
# 10. antimeridian.
fs = [{"type": "Feature", "geometry": pt(179.9 - 0.01 * i, -16.0), "properties": {"i": i}} for i in range(10)]
fs += [{"type": "Feature", "geometry": pt(-179.9 + 0.01 * i, -16.1), "properties": {"i": i}} for i in range(10)]
w("antimeridian.ndjson", fs)
# 11. degenerate polygon (empty after fix_polygon) far away, changes file bbox.
fs = [{"type": "Feature", "geometry": sq(139.6 + 0.001 * i, 35.6, 0.0004), "properties": {"i": i}, "tippecanoe": {"minzoom": 12}} for i in range(20)]
fs.append({"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [[[140.5, 36.5], [140.5, 36.5]]]}, "properties": {"deg": 1}})
w("degenerate.ndjson", fs)
# 12. many keys (more than 100 and 1000 attributes), long layer and key names.
fs = []
for i in range(60):
    p = {"k%04d" % (i * 20 + j): j for j in range(20)}
    p["L" * 300 + "x"] = i
    p["L" * 300 + "y"] = "v"
    fs.append({"type": "Feature", "geometry": pt(139.7 + 0.001 * i, 35.7), "properties": p,
               "tippecanoe": {"layer": ("Q" * 300) + ("a" if i % 2 else "b"), "minzoom": 12}})
w("wide.ndjson", fs)
# 13. edge numbers.
nums = ["0", "-0", "-0.0", "1e308", "1.5e-300", "12345678901234567890", "-9223372036854775809", "0.1", "1E+2",
        "100000000000000000000000", "3.0", "-1.25e-7", "1e21", "1e20", "123456789012345.6", "5e-324"]
with open(os.path.join(out, "numbers.ndjson"), "w") as f:
    for i, n in enumerate(nums):
        f.write('{"type":"Feature","geometry":{"type":"Point","coordinates":[139.%d,35.7]},"properties":{"n":%s,"s":"%s"}}\n' % (i + 1, n, n))
# 14. all features have maxzoom below -Z.
fs = [{"type": "Feature", "geometry": pt(139.6 + random.random() * 0.1, 35.6 + random.random() * 0.1),
       "properties": {"i": i}, "tippecanoe": {"maxzoom": 10}} for i in range(100)]
w("maxz-below.ndjson", fs)
# 15. long lines and large polygons: they cross tile edges at the zooms below -Z, so
#     the clip cascade from iz changes the bytes (spec 5.4).
fs = []
for i in range(400):
    lon0, lat0 = 138.5 + random.random() * 2, 35.0 + random.random() * 1.5
    ang, L = random.random() * 6.283, 0.05 + random.random() * 1.0
    pts = [[lon0 + t * L * math.cos(ang) / 10, lat0 + t * L * math.sin(ang) / 10] for t in range(11)]
    fs.append({"type": "Feature", "geometry": {"type": "LineString", "coordinates": pts}, "properties": {"i": i}, "tippecanoe": {"minzoom": 12}})
for i in range(150):
    lon0, lat0 = 138.5 + random.random() * 2, 35.0 + random.random() * 1.5
    r, n, ring = 0.02 + random.random() * 0.3, 7 + i % 9, []
    for k in range(n):
        a, rr = 6.283 * k / n + random.random() * 0.3, r * (0.6 + 0.4 * random.random())
        ring.append([lon0 + rr * math.cos(a), lat0 + rr * math.sin(a) * 0.8])
    ring.append(ring[0])
    fs.append({"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [ring]}, "properties": {"p": i}, "tippecanoe": {"minzoom": 12}})
w("cascade.ndjson", fs)
# 16. lines and polygons near and across the date line (lon > 180 too).
fs = []
for i in range(300):
    lon0, lat0 = 177 + random.random() * 6 - (360 if i % 3 == 0 else 0), -20 + random.random() * 6
    t = i % 3
    if t == 0:
        g = pt(lon0, lat0)
    elif t == 1:
        g = {"type": "LineString", "coordinates": [[lon0, lat0], [lon0 + 1.3, lat0 + 0.4], [lon0 + 2.1, lat0 - 0.2]]}
    else:
        g = sq(lon0, lat0, 0.7)
    fs.append({"type": "Feature", "geometry": g, "properties": {"i": i}})
w("dateline.ndjson", fs)
# 17. a -Z tile with more than 10 MB of kept geometry: the early simplification and
#     clean in world coordinates (spec 7.5).
fs = []
for i in range(420):
    lon0, lat0 = 139.70 + random.random() * 0.25, 35.60 + random.random() * 0.2
    r, ring = 0.003 + random.random() * 0.01, []
    for k in range(2000):
        a = 2 * math.pi * k / 2000
        rr = r * (1 + 0.05 * math.sin(7 * a) + 0.01 * random.random())
        ring.append([round(lon0 + rr * math.cos(a), 7), round(lat0 + rr * math.sin(a), 7)])
    ring.append(ring[0])
    fs.append({"type": "Feature", "geometry": {"type": "Polygon", "coordinates": [ring]}, "properties": {"i": i}, "tippecanoe": {"minzoom": 10}})
for i in range(300):
    lon0, lat0 = 139.70 + random.random() * 0.25, 35.60 + random.random() * 0.2
    pts = [[round(lon0 + 0.0001 * k, 7), round(lat0 + 0.00005 * k + 0.00002 * math.sin(k), 7)] for k in range(1500)]
    fs.append({"type": "Feature", "geometry": {"type": "LineString", "coordinates": pts}, "properties": {"l": i}, "tippecanoe": {"minzoom": 10}})
w("early-clean.ndjson", fs)
# 18. near the poles and beyond the plane, and empty geometries (not counted).
fs = [{"type": "Feature", "geometry": pt(-179.99 + 0.5 * i, 89.5 - (i % 3) * 179), "properties": {"i": i}} for i in range(6)]
fs.append({"type": "Feature", "geometry": pt(200.0, 10.0), "properties": {"far": 1}})
fs.append({"type": "Feature", "geometry": {"type": "LineString", "coordinates": []}, "properties": {"empty": "line"}})
fs.append({"type": "Feature", "geometry": {"type": "Polygon", "coordinates": []}, "properties": {"empty": "polygon"}})
fs.append({"type": "Feature", "geometry": {"type": "MultiPoint", "coordinates": []}, "properties": {"empty": "points"}})
w("polar.ndjson", fs)
# 19. rate drop: MultiPoints with members on both sides of a tile and none in it
#     (the clip leaves no geometry), then single points without minzoom.
fs = []
for i in range(400):
    lon0, lat0 = 139.6 + random.random() * 0.2, 35.6 + random.random() * 0.2
    if i % 4 == 0:
        g = {"type": "MultiPoint", "coordinates": [[lon0 - 0.15, lat0], [lon0 + 0.15, lat0 + 0.01]]}
    else:
        g = pt(lon0, lat0)
    fs.append({"type": "Feature", "geometry": g, "properties": {"i": i}})
w("multipoint-rate.ndjson", fs)
