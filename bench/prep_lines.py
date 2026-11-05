# Make the public lines inputs from an ogr2ogr GeoJSONSeq export of OSM roads.
# pub-lines.ndjson: per-feature minzoom from the road class (as in a layered map).
# pub-lines-plain.ndjson: no per-feature zoom (for -zg).
# Usage: python3 -I bench/prep_lines.py <roads.geojsonl> <outdir>
import json, sys
src, out = sys.argv[1:3]
mz = {'motorway': 9, 'trunk': 10, 'primary': 11, 'secondary': 12, 'tertiary': 13,
      'motorway_link': 12, 'trunk_link': 12, 'primary_link': 13, 'secondary_link': 13,
      'residential': 14, 'unclassified': 14}
n = 0
with open(src) as f, open(f'{out}/pub-lines.ndjson', 'w') as a, open(f'{out}/pub-lines-plain.ndjson', 'w') as b:
    for line in f:
        ft = json.loads(line)
        p = {k: v for k, v in ft['properties'].items() if v is not None}
        g = ft['geometry']
        b.write(json.dumps({'type': 'Feature', 'geometry': g, 'properties': p}, separators=(',', ':')) + '\n')
        m = mz.get(p.get('highway'), 16)
        a.write(json.dumps({'type': 'Feature', 'geometry': g, 'properties': p,
                            'tippecanoe': {'layer': 'lines', 'minzoom': m, 'maxzoom': 18}}, separators=(',', ':')) + '\n')
        n += 1
print(n, 'features')
