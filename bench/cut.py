# Cut a one-tile subset from an NDJSON corpus: keep the features whose first
# coordinate is in tile z/x/y. (The first coordinate is a close, cheap estimate
# of "the bbox touches the tile".)
# Usage: python3 -I bench/cut.py <in.ndjson> <out.ndjson> <z/x/y>
import math, re, sys
src, dst, zxy = sys.argv[1:4]
z, x, y = map(int, zxy.split('/'))
n = 2 ** z
def lon(x): return x / n * 360 - 180
def lat(y): return math.degrees(math.atan(math.sinh(math.pi * (1 - 2 * y / n))))
w, e, s, nn = lon(x), lon(x + 1), lat(y + 1), lat(y)
pat = re.compile(rb'"coordinates":\[+(-?[0-9.]+),(-?[0-9.]+)')
kept = total = 0
with open(src, 'rb') as f, open(dst, 'wb') as o:
    for line in f:
        total += 1
        m = pat.search(line)
        if m and w <= float(m.group(1)) < e and s <= float(m.group(2)) < nn:
            o.write(line); kept += 1
print(f'{dst}: {kept} of {total} features in {zxy}')
