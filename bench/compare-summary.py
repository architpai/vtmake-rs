#!/usr/bin/env python3
"""Read the final tile-set summary without loading the per-feature report.

vtmake-compare writes per-tile results first and the summary fields last.
Use a file mapping to find that final field with constant working memory.
"""
import json
import mmap
import sys

with open(sys.argv[1], 'rb') as source, mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_READ) as report:
    field = b'"tile_sets":'
    start = report.rfind(field)
    if start < 0:
        raise ValueError('Missing tile_sets in comparison report')
    summary, _ = json.JSONDecoder().raw_decode(report[start + len(field):].decode().lstrip())
    print(summary['a'], summary['b'], summary['byte_identical'], len(summary['only_a']), len(summary['only_b']))
