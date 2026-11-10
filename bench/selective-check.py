#!/usr/bin/env python3
"""Byte identity tests. Data and outputs stay outside the repository."""
import argparse
import gzip
import json
import os
from pathlib import Path
import random
import sqlite3
import subprocess
import tempfile

p = argparse.ArgumentParser()
p.add_argument("--vtmake", required=True)
p.add_argument("--input", required=True)
p.add_argument("--minzoom", default="12")
p.add_argument("--maxzoom", default="18")
p.add_argument("--baseline")
a = p.parse_args()
flags = ["build", "-q", "-Z" + a.minzoom, "-z" + a.maxzoom, "--read-parallel",
         "--buffer=5", "--force", "--drop-densest-as-needed",
         "--extend-zooms-if-still-dropping", "--no-feature-limit",
         "--no-tile-size-limit", "--layer", "test"]

def read_tiles(path, mode):
    if mode == "-e":
        return {tuple(map(int, p.relative_to(path).with_suffix("").parts)): p.read_bytes()
                for p in path.glob("*/*/*.pbf")}
    with sqlite3.connect(path) as db:
        return {(z, x, (1 << z)-1-y): b for z, x, y, b in db.execute(
            "select zoom_level,tile_column,tile_row,tile_data from tiles")}

with tempfile.TemporaryDirectory(prefix="vtmake-selective-") as tmp:
    root = Path(tmp)
    def run(name, mode, threads, extra=(), binary=a.vtmake):
        out = root / name
        subprocess.run([binary, *flags, *extra, mode, str(out), a.input], check=True,
                       env={**os.environ, "TIPPECANOE_MAX_THREADS": str(threads)})
        return read_tiles(out, mode)

    full_hash = root / "full.hashes"
    full = run("full", "-o", 2, ["--hashes-out", str(full_hash)])
    assert full, "empty test input"
    assert len(full_hash.read_text().splitlines()) == len(full) + 1
    if a.baseline:
        assert run("baseline", "-o", 2, binary=a.baseline) == full
    keys = sorted(full)
    rng = random.Random(3917)
    full_rows = {tuple(map(int, r.split()[0].split("/"))): r for r in full_hash.read_text().splitlines()[1:]}
    def restricted(keyset):
        # The exact manifest that a selection of these keys must publish.
        return "\n".join([rows_header] + [full_rows[k] for k in sorted(keyset) if k in full_rows]) + "\n"
    rows_header = full_hash.read_text().splitlines()[0]
    # Keys that the full render does not produce: children below the deepest
    # tile, an ancestor below -Z, and a far neighbour. An extra tile fails.
    z0, x0, y0 = keys[len(keys) // 2]
    absent = {(z0 + 1, 2 * x0, 2 * y0), (z0 + 1, 2 * x0 + 1, 2 * y0 + 1),
              (int(a.minzoom) - 1, x0 >> (z0 - int(a.minzoom) + 1), y0 >> (z0 - int(a.minzoom) + 1)),
              (z0, (x0 + 4000) % (1 << z0), y0)}
    absent = {k for k in absent if 0 <= k[0] <= 24 and k not in full}
    known = root / "known"
    rows = full_hash.read_text().splitlines()
    known.write_text("\n".join(rows[:1] + rows[1::2]) + "\n")
    known_keys = {tuple(map(int, row.split()[0].split("/"))) for row in rows[1::2]}
    bad = root / "bad"
    bad.write_text(rows[0] + "\n" + rows[1].split()[0] + " " + "0"*64 + "\n")
    for mode in ["-o", "-e"]:
        for threads in [1, 4]:
            for case, targets in enumerate([set(rng.sample(keys, min(13, len(keys)))) | absent,
                                            set(), set(keys[::7]) | absent]):
                target_file = root / "targets"
                target_file.write_text("".join(f"{z}/{x}/{y}\n" for z,x,y in sorted(targets)))
                expected = {k: v for k, v in full.items() if k in targets}
                manifest = root / "selected.hashes"
                assert run(f"selected-{mode}-{threads}-{case}", mode, threads,
                           ["--only-tiles", str(target_file), "--hashes-out", str(manifest)]) == expected
                assert manifest.read_text() == restricted(targets)
            for label, known_file, expected in [
                    ("all", full_hash, {}),
                    ("partial", known, {k:v for k,v in full.items() if k not in known_keys}),
                    ("wrong", bad, full)]:
                manifest = root / "new.hashes"
                assert run(f"{label}-{mode}-{threads}", mode, threads,
                           ["--known-hashes", str(known_file), "--hashes-out", str(manifest)]) == expected
                assert manifest.read_bytes() == full_hash.read_bytes()
            target_file.write_text("".join(f"{z}/{x}/{y}\n" for z,x,y in keys[:13]))
            manifest = root / "combined.hashes"
            assert run(f"combined-{mode}-{threads}", mode, threads,
                       ["--only-tiles", str(target_file), "--known-hashes", str(full_hash),
                        "--hashes-out", str(manifest)]) == {}
            assert manifest.read_text() == restricted(set(keys[:13]))
    for malformed in ["25/0/0\n", "1/2/0\n", "foo\n"]:
        target_file = root / "invalid"
        target_file.write_text(malformed)
        result = subprocess.run([a.vtmake, *flags, "--only-tiles", str(target_file),
                                 "-o", str(root/"invalid-output"), a.input], capture_output=True)
        assert result.returncode == 101
        assert not (root/"invalid-output").exists()
    for extra in [
            ["--known-hashes", str(bad), "--hashes-out", str(root/"collision"), "-o", str(root/"collision")],
            ["--only-tiles", str(root/"invalid"), "--allow-existing", "-o", str(root/"allow")],
            ["--hashes-out", str(root/"size"), "--maximum-tile-bytes", "100", "-o", str(root/"limits")]]:
        # A size flag alone does not re-enable --no-tile-size-limit; test
        # the default limits separately below.
        if "--maximum-tile-bytes" in extra:
            command_flags = [f for f in flags if f not in ["--no-feature-limit", "--no-tile-size-limit"]]
        else:
            command_flags = flags
        result = subprocess.run([a.vtmake, *command_flags, *extra, a.input], capture_output=True)
        assert result.returncode == 101, result.stderr
    print(json.dumps({"result": "PASS", "input": a.input, "full_tiles": len(full),
                      "cases": "selection, empty, combined, all/partial/wrong hashes; dir/MBTiles; threads 1/4"}))
