#!/usr/bin/env python3
"""Make the generated inputs of bench/attr-check.sh (the attribute options).

Usage: attr-inputs.py OUTDIR

Files:
  values.ndjson    points, lines and polygons in 3 layers, with all value types
                   (strings with UTF-8, integers, big and negative numbers,
                   floats, exponents, booleans, null, objects, arrays, empty
                   strings), IDs of all kinds, duplicate keys, tippecanoe zooms
  dense.ndjson     many points close together (dot dropping at low zooms), with
                   numbers and strings for -E
  dense.csv        the same points as CSV
  values-rs.json   values.ndjson as a GeoJSON text sequence (RS separators)
  values.ndjson.gz values.ndjson, gzip
  many.fgb         2400 points without an index (more than one tippecanoe batch)
  (and the inputs of bench/input-variants.py, for its FlatGeobuf writer)

Usage: attr-inputs.py TESTS OUTDIR (TESTS: felt's tests folder, read only)
"""
import gzip
import json
import math
import os
import random
import runpy
import sys

tests, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
rnd = random.Random(1234)

STRINGS = ["", "a", "abc", "Zürich", "東京都", "émoji 😀 text", "0", "-1", "1.5", "true", "false", "null",
           "x" * 40, "ä" * 30, "a,b", "with \"quote\"", "line\nbreak", "tab\tx"]
NUMBERS = [0, 1, -1, 2, 7, 42, -17, 3.5, -0.25, 0.1, 1e21, 1.5e-7, 123456789012, -9007199254740993,
           18446744073709551615, 12345678901234567890, 2.5, 1e300, -0.0, 100]


def value(i):
    k = i % 9
    if k == 0:
        return rnd.choice(STRINGS)
    if k == 1:
        return rnd.choice(NUMBERS)
    if k == 2:
        return rnd.choice([True, False])
    if k == 3:
        return None
    if k == 4:
        return {"o": rnd.randint(0, 3), "s": "t"}
    if k == 5:
        return [1, "two", None]
    if k == 6:
        return rnd.randint(-1000, 1000)
    if k == 7:
        return round(rnd.uniform(-100, 100), rnd.randint(0, 6))
    return rnd.choice(STRINGS + ["long " + "ü" * rnd.randint(1, 20)])


def geometry(i):
    lon, lat = rnd.uniform(-20, 40), rnd.uniform(-10, 50)
    t = i % 3
    if t == 0:
        return {"type": "Point", "coordinates": [lon, lat]}
    if t == 1:
        return {"type": "LineString", "coordinates": [[lon, lat], [lon + rnd.uniform(0.1, 3), lat + rnd.uniform(-2, 2)], [lon + 4, lat + 1]]}
    d = rnd.uniform(0.2, 3)
    return {"type": "Polygon", "coordinates": [[[lon, lat], [lon + d, lat], [lon + d, lat + d], [lon, lat + d], [lon, lat]]]}


def ident(i):
    k = i % 11
    return [None, i, str(i), -i, 1.5, "abc", 18446744073709551615, "0012", None, 0, str(10 ** 25)][k]


lines = []
for i in range(1500):
    props = {
        "s": rnd.choice(STRINGS),
        "n": rnd.choice(NUMBERS),
        "b": rnd.choice([True, False]),
        "z": None,
        "mixed": value(i),
        "mixed2": value(i + 4),
        "cat": rnd.choice(["a", "b", "c", "d"]),
        "rank": rnd.randint(0, 10),
        "special": rnd.choice([i, str(i), "x" + str(i), 1.25, -3, "007", 0, "", None, 2 ** 64]),
        "long": "ü" * rnd.randint(0, 25) + "end",
    }
    if i % 13 == 0:
        props["extra"] = "only some"
    f = {"type": "Feature", "properties": props, "geometry": geometry(i)}
    if ident(i) is not None:
        f["id"] = ident(i)
    tc = {}
    if i % 5 == 0:
        tc["layer"] = ["one", "two"][i % 2]
    if i % 17 == 0:
        tc["minzoom"] = rnd.randint(0, 4)
    if i % 19 == 0:
        tc["maxzoom"] = rnd.randint(2, 6)
    if tc:
        f["tippecanoe"] = tc
    text = json.dumps(f, ensure_ascii=False)
    if i % 50 == 7:
        # A duplicate key: tippecanoe keeps both.
        text = text.replace('"cat":', '"cat": "dup", "cat":', 1)
    lines.append(text)
with open(f"{out}/values.ndjson", "w") as fp:
    fp.write("\n".join(lines) + "\n")
with gzip.open(f"{out}/values.ndjson.gz", "wt") as fp:
    fp.write("\n".join(lines) + "\n")
with open(f"{out}/values-rs.json", "w") as fp:
    for t in lines:
        fp.write("\x1e" + t + "\n")

dense = []
rows = ["lon,lat,num,name,flag,count"]
for i in range(4000):
    lon = 10 + rnd.gauss(0, 0.5)
    lat = 45 + rnd.gauss(0, 0.5)
    num = rnd.choice([rnd.randint(-50, 50), round(rnd.uniform(0, 10), 3), 0])
    name = rnd.choice(["Alpha", "Beta", "Gamma", "Δέλτα", "", "x"])
    flag = rnd.choice([True, False])
    props = {"num": num, "name": name, "flag": flag, "count": 1, "label": f"p{i}",
             "mean": rnd.uniform(0, 100), "n2": rnd.randint(1, 5)}
    if i % 7 == 0:
        props["num"] = None
    if i % 11 == 0:
        del props["n2"]
    f = {"type": "Feature", "properties": props, "geometry": {"type": "Point", "coordinates": [lon, lat]}}
    if i % 3 == 0:
        f["id"] = i
    dense.append(json.dumps(f, ensure_ascii=False))
    rows.append(f"{lon},{lat},{num if num is not None else ''},{name},{str(flag).lower()},1")
with open(f"{out}/dense.ndjson", "w") as fp:
    fp.write("\n".join(dense) + "\n")
with open(f"{out}/dense.csv", "w") as fp:
    fp.write("\n".join(rows) + "\n")

# The FlatGeobuf writer of bench/input-variants.py (it also writes its own files).
stamp = os.path.join(out, ".done-v5")
if os.path.exists(stamp):
    os.remove(stamp)
# It reads TESTS and OUTDIR from sys.argv, as this script.
iv = runpy.run_path(os.path.join(os.path.dirname(os.path.abspath(__file__)), "input-variants.py"))
many = []
# (felt stops with a segmentation fault on a feature without geometry.)
for i in range(2400):
    many.append(({"xy": [rnd.uniform(-50, 50), rnd.uniform(-40, 40)]}, iv["props"]((11, "s", f"n{i}"), (5, "i", i))))
iv["write_fgb"]("many.fgb", 1, iv["all_cols"], many, index_node_size=0)
