import { test } from "node:test";
import assert from "node:assert/strict";
import { PNG } from "pngjs";
import { selectTiles, pixelDiff, styleFor } from "./lib.mjs";
test("selection is seeded, includes both sets and worst geometry", () => {
  const a = Array.from({ length: 20 }, (_, x) => ({ z: 5, x, y: 1 }));
  const b = [{ z: 6, x: 1, y: 2 }];
  const opts = {
    samples: 2,
    seed: 42,
    worst: 1,
    report: {
      tiles: [
        {
          tile: a[19],
          totals: { max_distance: 4, unmatched_a: 0, unmatched_b: 0 },
        },
      ],
      tile_sets: { only_a: [], only_b: b },
    },
  };
  const result = selectTiles(a, b, opts);
  assert.deepEqual(result, selectTiles(a, b, opts));
  assert(result.some((t) => t.x === 19));
  assert(result.some((t) => t.z === 6));
});
test("pixel threshold boundary", () => {
  const a = new PNG({ width: 1, height: 1 }),
    b = new PNG({ width: 1, height: 1 });
  b.data[0] = 3;
  assert.equal(pixelDiff(a, b, 3).count, 0);
  assert.equal(pixelDiff(a, b, 2).count, 1);
});
test("viewer colors and debug covers every layer without glyph requests", () => {
  const s = {
    layers: [{ id: "polygons" }, { id: "unknown" }],
    minzoom: 12,
    maxzoom: 13,
  };
  const viewer = styleFor(s, "http://localhost/a");
  assert.equal(viewer.layers[1].paint["fill-color"], "#2563eb");
  assert.equal(viewer.layers[1].paint["fill-opacity"], 0.25);
  assert.equal(styleFor(s, "local", true).layers.length, 7);
  assert(!viewer.glyphs);
});

test("large report ranking streams and keeps only the requested records", async () => {
  const fs = await import("node:fs");
  const os = await import("node:os");
  const path = await import("node:path");
  const { readRanking } = await import("./lib.mjs");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "vtmake-ranking-"));
  const file = path.join(dir, "report.json");
  try {
    fs.writeFileSync(
      file,
      JSON.stringify({
        tiles: [1, 9, 3].map((d, x) => ({
          tile: { z: 2, x, y: 0 },
          totals: { max_distance: d, unmatched_a: 0, unmatched_b: 0 },
          layers: [],
        })),
      }),
    );
    const a = [{ z: 2, x: 0, y: 0 }];
    const r = await readRanking(file, 1, a, []);
    assert.equal(r.tiles.length, 1);
    assert.equal(r.tiles[0].totals.max_distance, 9);
    assert.deepEqual(r.tile_sets.only_a, a);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
