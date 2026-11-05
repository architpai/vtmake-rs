# Make a Markdown table from bench/results/runs.jsonl: median per (task, profile, tool).
# Usage: python3 -I bench/report.py [runs.jsonl]
import json, statistics, sys
from collections import defaultdict
path = sys.argv[1] if len(sys.argv) > 1 else 'bench/results/runs.jsonl'
runs = [json.loads(l) for l in open(path) if l.strip()]
g = defaultdict(list)
tools, keys = [], []
for r in runs:
    if r['rc'] != 0: continue
    k = (r['task'], r['profile'])
    if k not in keys: keys.append(k)
    if r['tool'] not in tools: tools.append(r['tool'])
    g[(k, r['tool'])].append(r)
base = tools[0]
print('| task | profile | ' + ' | '.join(f'{t} wall s (CPU s, RSS MB)' for t in tools) + ' | tiles | same tiles |')
print('|---|---|' + '---:|' * len(tools) + '---:|---|')
for k in keys:
    cells, hashes, tiles = [], set(), ''
    b = g.get((k, base))
    bw = statistics.median(r['wall_s'] for r in b) if b else None
    for t in tools:
        rs = g.get((k, t))
        if not rs: cells.append('–'); continue
        w = statistics.median(r['wall_s'] for r in rs)
        c = statistics.median((r['user_s'] or 0) + (r['sys_s'] or 0) for r in rs)
        m = max(r['max_rss_mb'] for r in rs)
        rel = f' **{(w / bw - 1) * 100:+.0f}%**' if bw and t != base else ''
        cells.append(f'{w:.1f}{rel} ({c:.0f}, {m})')
        hashes |= {r['tiles_sha'] for r in rs}
        tiles = rs[0]['tiles']
    print(f'| {k[0]} | {k[1]} | ' + ' | '.join(cells) + f' | {tiles} | {"yes" if len(hashes) == 1 else "no"} |')
