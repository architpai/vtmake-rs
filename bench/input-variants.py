"""Make the GeoJSON and CSV variants for bench/input-check.sh.

Usage: python3 -I bench/input-variants.py <felt tests folder> <output folder>

The features come from felt's tests/ne_110m_populated_places_nulls/in.json
(read in place). The script writes the same features as NDJSON, as a
pretty-printed FeatureCollection, as a FeatureCollection with one feature per
line, on one line, as a GeoJSON text sequence (RS), gzip-compressed, and some
inputs with errors and special cases.
"""
import gzip
import struct
import json
import os
import sys

tests, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
stamp = os.path.join(out, ".done-v5")
if os.path.exists(stamp):
    sys.exit(0)

with open(os.path.join(tests, "ne_110m_populated_places_nulls/in.json"), encoding="utf-8") as f:
    feats = [json.loads(line) for line in f if line.strip()]


def w(name, text, mode="w"):
    with open(os.path.join(out, name), mode, encoding=None if "b" in mode else "utf-8") as f:
        f.write(text)


lines = [json.dumps(x, ensure_ascii=False) for x in feats]
compact = [json.dumps(x, ensure_ascii=False, separators=(",", ":")) for x in feats]
w("places.ndjson", "\n".join(compact) + "\n")
w("places-pretty.json", json.dumps({"type": "FeatureCollection", "features": feats}, indent=2, ensure_ascii=False) + "\n")
w("places-fc-lines.json", '{"type":"FeatureCollection","features":[\n' + ",\n".join(compact) + "\n]}\n")
w("places-fc-oneline.json", json.dumps({"type": "FeatureCollection", "features": feats}, ensure_ascii=False))
w("places-rs.json", "".join("\x1e" + x + "\n" for x in lines))
with gzip.open(os.path.join(out, "places.ndjson.gz"), "wt", encoding="utf-8") as f:
    f.write("\n".join(compact) + "\n")
w("places-bom.ndjson", "﻿" + "\n".join(compact) + "\n")

# A syntax error in the middle: the stream (or the segment) stops there.
bad = compact[:]
bad[100] = '{"type":"Feature","properties":{"a":1},"geometry":{"type":"Point","coordinates":[1,2]} oops}'
w("places-error.ndjson", "\n".join(bad) + "\n")

# A mixture: NDJSON lines, a feature over several lines, a FeatureCollection
# with one feature per line, lines with two features, a bare geometry (after
# features, so not read), blank lines with RS and spaces.
pretty_one = json.dumps(feats[40], indent=1, ensure_ascii=False)
mixed = (
    "\n".join(compact[:30]) + "\n"
    + pretty_one + "\n"
    + '{"type":"FeatureCollection","features":[\n' + ",\n".join(compact[50:80]) + "\n]}\n"
    + compact[90] + " " + compact[91] + "\n"
    + compact[92] + ",\n"
    + '{"type":"Point","coordinates":[10,10]}\n'
    + " \x1e \t\r\n"
    + "\n".join(compact[100:]) + "\n"
)
w("mixed.json", mixed)

# A Feature inside another object (not in properties): jsonpull reads it.
inner = compact[5]
w("nested-feature.json",
  '{"wrapper":' + inner + ',"list":[' + compact[6] + "," + compact[7] + ']}\n'
  + '{"type":"Feature","properties":{"p":' + compact[8] + '},"geometry":{"type":"Point","coordinates":[3,3]}}\n'
  + '{"type":"Feature","geometry":{"type":"Point","coordinates":[4,4]},"tippecanoe":' + compact[9] + ',"properties":{"q":1}}\n')

# Properties with the "type" of a feature or a geometry.
w("props-type.ndjson",
  '{"type":"Feature","properties":{"type":"Feature","x":1},"geometry":{"type":"Point","coordinates":[1,1]}}\n'
  '{"type":"Feature","properties":{"type":"FeatureCollection","x":2},"geometry":{"type":"Point","coordinates":[2,2]}}\n'
  '{"type":"Feature","properties":{"type":"Point","coordinates":[5,5],"x":3},"geometry":{"type":"Point","coordinates":[3,3]}}\n'
  '{"type":"Feature","properties":{"o":{"type":"Feature"},"x":4},"geometry":{"type":"Point","coordinates":[4,4]}}\n'
  '{"type":"Feature","properties":{"type":"building","x":5},"geometry":{"type":"Point","coordinates":[5,5]}}\n')

# A FeatureCollection without its end.
w("unclosed.json", '{"type":"FeatureCollection","features":[\n' + ",\n".join(compact[:20]) + "\n")

# Number forms.
nums = ["0", "-0", "1.0", "0.1", "1e2", "1E-7", "-12.5e+3", "9007199254740993", "-9007199254740993",
        "18446744073709551615", "18446744073709551616", "123456789012345678901234567890", "1.7976931348623157e308", "5e-324"]
rows = []
for i, n in enumerate(nums):
    rows.append('{"type":"Feature","id":%s,"properties":{"n":%s,"i":%d},"geometry":{"type":"Point","coordinates":[%s,%s]}}'
                % (n if not n.startswith("-") else str(i), n, i, "1.%d" % i, "0.123456789012345678%d" % i))
rows.append('{"type":"Feature","id":1.5,"properties":{"n":[1,2.50,{"a":-0}]},"geometry":{"type":"Point","coordinates":[1e1,2E0]}}')
rows.append('{"type":"Feature","id":"7","properties":{"s":"\\u00e9\\ud83d\\ude00\\n\\t\\"q\\""},"geometry":{"type":"Point","coordinates":[-0.0,0.000001]}}')
w("numbers.ndjson", "\n".join(rows) + "\n")

w("empty.json", "")

# CSV forms.
w("quotes.csv", 'name,"Lat",Longitude,"pop, total",note\n"a ""b"" c",1.5,2.5,"1,000",x\nplain, 10 ,20,7,\n"q",-3,4,-0.5,"y"\n,5,6,1e3,z\nmissing,,7,1,w\n')
w("nocols.csv", "a,b\n1,2\n")
w("mismatch.csv", "lat,lon,x\n1,2,3\n4,5\n")



# FlatGeobuf files, with a small FlatBuffers builder (the layout of the official
# builder: objects are prepended, alignment from the end of the buffer).
class FB:
    def __init__(self):
        self.buf = bytearray()
        self.minalign = 1
        self.slots = None

    def off(self):
        return len(self.buf)

    def prep(self, size, additional):
        self.minalign = max(self.minalign, size)
        self.buf[0:0] = bytes((-(len(self.buf) + additional)) % size)

    def put(self, fmt, v):
        self.prep(struct.calcsize(fmt), 0)
        self.buf[0:0] = struct.pack("<" + fmt, v)

    def uoffset(self, o):
        self.prep(4, 0)
        self.buf[0:0] = struct.pack("<I", self.off() + 4 - o)

    def vector(self, fmt, items):
        size = struct.calcsize(fmt)
        self.prep(4, size * len(items))
        self.prep(size, size * len(items))
        for v in reversed(items):
            self.buf[0:0] = struct.pack("<" + fmt, v)
        self.put("I", len(items))
        return self.off()

    def offsets(self, objs):
        self.prep(4, 4 * len(objs))
        for o in reversed(objs):
            self.uoffset(o)
        self.put("I", len(objs))
        return self.off()

    def string(self, s):
        b = s.encode()
        self.prep(4, len(b) + 1)
        self.buf[0:0] = b + b"\0"
        self.put("I", len(b))
        return self.off()

    def start(self, n):
        self.slots = [0] * n
        self.obj_end = self.off()

    def scalar(self, i, fmt, v):
        self.put(fmt, v)
        self.slots[i] = self.off()

    def field(self, i, o):
        self.uoffset(o)
        self.slots[i] = self.off()

    def end(self):
        self.put("i", 0)
        t = self.off()
        n = len(self.slots)
        while n > 0 and self.slots[n - 1] == 0:
            n -= 1
        fields = [t - s if s else 0 for s in self.slots[:n]]
        for f in reversed(fields):
            self.put("H", f)
        self.put("H", t - self.obj_end)
        self.put("H", 4 + 2 * n)
        vt = self.off()
        pos = len(self.buf) - t
        self.buf[pos:pos + 4] = struct.pack("<i", vt - t)
        return t

    def finish(self, root):
        self.prep(max(self.minalign, 4), 8)
        self.uoffset(root)
        self.put("I", len(self.buf))
        return bytes(self.buf)


def geometry(fb, g):
    parts = [geometry(fb, p) for p in g.get("parts", [])]
    pv = fb.offsets(parts) if parts else None
    xy = fb.vector("d", g["xy"]) if "xy" in g else None
    ends = fb.vector("I", g["ends"]) if "ends" in g else None
    fb.start(8)
    if ends is not None:
        fb.field(0, ends)
    if xy is not None:
        fb.field(1, xy)
    if pv is not None:
        fb.field(7, pv)
    if "type" in g:
        fb.scalar(6, "B", g["type"])
    return fb.end()


def write_fgb(name, gtype, columns, features, index_node_size=None):
    """columns: [(name, type)]; features: [(geometry dict, properties bytes)]."""
    fb = FB()
    cols = []
    for cname, ctype in columns:
        n = fb.string(cname)
        fb.start(2)
        fb.field(0, n)
        fb.scalar(1, "B", ctype)
        cols.append(fb.end())
    cv = fb.offsets(cols) if cols else None
    fb.start(10)
    fb.scalar(8, "Q", len(features))
    if cv is not None:
        fb.field(7, cv)
    if index_node_size is not None:
        fb.scalar(9, "H", index_node_size)
    fb.scalar(2, "B", gtype)
    header = fb.finish(fb.end())
    node_size = 16 if index_node_size is None else index_node_size
    index = b""
    if node_size > 0:
        n = num = len(features)
        while True:
            n = -(-n // node_size)
            num += n
            if n == 1:
                break
        index = bytes(num * 40)
    data = b"fgb\x03fgb\x01" + header + index
    for g, props in features:
        fb = FB()
        pv = fb.vector("B", list(props)) if props else None
        gt = geometry(fb, g)
        fb.start(3)
        fb.field(0, gt)
        if pv is not None:
            fb.field(1, pv)
        data += fb.finish(fb.end())
    with open(os.path.join(out, name), "wb") as f:
        f.write(data)


def props(*vals):
    """vals: (column index, struct format or "s", value)."""
    b = b""
    for col, fmt, v in vals:
        b += struct.pack("<H", col)
        if fmt == "s":
            e = v.encode()
            b += struct.pack("<I", len(e)) + e
        else:
            b += struct.pack("<" + fmt, v)
    return b


all_cols = [("byte", 0), ("ubyte", 1), ("bool", 2), ("short", 3), ("ushort", 4), ("int", 5), ("uint", 6), ("long", 7),
            ("ulong", 8), ("float", 9), ("double", 10), ("string", 11), ("json", 12), ("datetime", 13)]
p1 = props((0, "b", -5), (1, "B", 250), (2, "B", 1), (3, "h", -300), (4, "H", 60000), (5, "i", -70000), (6, "I", 4000000000),
           (7, "q", -2**40), (8, "Q", 2**63 + 5), (9, "f", 0.1), (10, "d", 1 / 3), (11, "s", "héllo"), (12, "s", '{"a":1}'), (13, "s", "2024-01-02T03:04:05Z"))
p2 = props((11, "s", "second"), (2, "B", 0), (10, "d", -0.0), (9, "f", 3.5))
pts = [({"xy": [10.5, 20.25]}, p1), ({"xy": [-122.4, 37.8]}, p2), ({"xy": [0, 0]}, b"")]
write_fgb("points.fgb", 1, all_cols, pts)
write_fgb("points-noindex.fgb", 1, all_cols, pts, index_node_size=0)
write_fgb("points-node4.fgb", 1, all_cols, pts * 5, index_node_size=4)
write_fgb("multipoint.fgb", 4, [], [({"xy": [1, 1, 2, 2, 3, 3]}, b"")], index_node_size=0)
write_fgb("lines.fgb", 2, [("n", 5)], [({"xy": [0, 0, 10, 10, 20, 5]}, props((0, "i", 1))), ({"xy": [-5, -5, 5, 5]}, props((0, "i", 2)))])
write_fgb("multilines.fgb", 5, [], [({"xy": [0, 0, 10, 10, 20, 0, 20, 10, 30, 30], "ends": [2, 5]}, b"")], index_node_size=0)
ring = [0, 0, 10, 0, 10, 10, 0, 10, 0, 0]
hole = [2, 2, 2, 8, 8, 8, 8, 2, 2, 2]
write_fgb("polygon.fgb", 3, [], [({"xy": ring + hole, "ends": [5, 10]}, b""), ({"xy": [20, 20, 30, 20, 30, 30, 20, 20]}, b"")])
write_fgb("multipolygon.fgb", 6, [("k", 11)], [({"parts": [{"xy": ring + hole, "ends": [5, 10]}, {"xy": [20, 20, 30, 20, 30, 30, 20, 20]}]}, props((0, "s", "mp")))])
write_fgb("unknown.fgb", 0, [], [({"xy": [1, 2], "type": 1}, b""), ({"xy": [0, 0, 5, 5], "type": 2}, b""), ({"xy": ring, "type": 3}, b""),
                                 ({"xy": [4, 4, 6, 6], "type": 4}, b"")], index_node_size=0)
write_fgb("collection.fgb", 7, [], [({"xy": [1, 2]}, b"")])
write_fgb("binary.fgb", 1, [("b", 14)], [({"xy": [1, 2]}, props((0, "s", "xx")))])
write_fgb("mercator.fgb", 1, [], [({"xy": [1113194.9, 2273030.9]}, b""), ({"xy": [-13627361.0, 4544761.0]}, b"")])

open(stamp, "w").close()
