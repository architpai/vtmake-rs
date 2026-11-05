import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import Database from "better-sqlite3";
import { PNG } from "pngjs";

export function openTileset(filename) {
  const directory = fs.statSync(filename).isDirectory();
  let metadata,
    tiles = [],
    db;
  if (directory) {
    metadata = fs.existsSync(path.join(filename, "metadata.json"))
      ? JSON.parse(fs.readFileSync(path.join(filename, "metadata.json")))
      : {};
    for (const z of fs.readdirSync(filename).filter((v) => /^\d+$/.test(v)))
      for (const x of fs
        .readdirSync(path.join(filename, z))
        .filter((v) => /^\d+$/.test(v)))
        for (const y of fs
          .readdirSync(path.join(filename, z, x))
          .filter((v) => /^\d+\.pbf$/.test(v)))
          tiles.push({ z: +z, x: +x, y: +y.slice(0, -4) });
  } else {
    db = new Database(filename, { readonly: true, fileMustExist: true });
    metadata = Object.fromEntries(
      db.prepare("SELECT name,value FROM metadata").raw().all(),
    );
    tiles = db
      .prepare("SELECT zoom_level z,tile_column x,tile_row y FROM tiles")
      .all()
      .map((t) => ({ ...t, y: 2 ** t.z - 1 - t.y }));
  }
  const extra =
    typeof metadata.json === "string"
      ? JSON.parse(metadata.json)
      : metadata.json || {};
  const layers = metadata.vector_layers || extra.vector_layers;
  if (!Array.isArray(layers))
    throw new Error(`${filename}: vector_layers metadata is required`);
  const minzoom = tiles.length
    ? Math.min(...new Set(tiles.map((t) => t.z)))
    : 0;
  const maxzoom = tiles.length
    ? Math.max(...new Set(tiles.map((t) => t.z)))
    : 0;
  const query = db?.prepare(
    "SELECT tile_data FROM tiles WHERE zoom_level=? AND tile_column=? AND tile_row=?",
  );
  return {
    metadata,
    layers,
    tiles,
    minzoom,
    maxzoom,
    read(z, x, y) {
      let data;
      if (directory) {
        const file = path.join(filename, `${z}/${x}/${y}.pbf`);
        if (!fs.existsSync(file)) return null;
        data = fs.readFileSync(file);
      } else data = query.get(z, x, 2 ** z - 1 - y)?.tile_data;
      return data && data[0] === 31 && data[1] === 139
        ? zlib.gunzipSync(data)
        : data;
    },
    close() {
      db?.close();
    },
  };
}
// Styles for the layer names of the public benchmark inputs. Other layers get
// all three geometry types in the fallback color.
const configs = {
  polygons: ["fill", "#2563eb"],
  bldg: ["fill", "#2563eb"],
  lines: ["line", "#dc2626", 2],
  points: ["circle", "#059669"],
  pois: ["circle", "#d97706"],
};
export function styleFor(set, url, debug = false) {
  const layers = [
    {
      id: "background",
      type: "background",
      paint: { "background-color": "#ffffff" },
    },
  ];
  for (const { id } of set.layers) {
    const config = configs[id];
    const types = debug || !config ? ["fill", "line", "circle"] : [config[0]];
    for (const type of types) {
      const color = config?.[1] || "#2563eb";
      let paint;
      if (type === "fill")
        paint = {
          "fill-color": color,
          "fill-opacity": debug ? 0.08 : 0.25,
          ...(!debug ? { "fill-outline-color": color } : {}),
        };
      if (type === "line")
        paint = {
          "line-color": color,
          "line-width": debug ? 0.5 : config?.[2] || 1.5,
        };
      if (type === "circle")
        paint = {
          "circle-color": color,
          "circle-radius": debug ? 1 : 4,
          "circle-stroke-color": "#ffffff",
          "circle-stroke-width": debug ? 0 : 1,
        };
      // Debug outlines include polygon boundaries. Viewer fallback also handles all geometry types.
      const filter =
        type === "fill"
          ? ["==", "$type", "Polygon"]
          : type === "circle"
            ? ["==", "$type", "Point"]
            : ["in", "$type", "LineString", "Polygon"];
      layers.push({
        id: `${id}/${type}`,
        type,
        source: "tiles",
        "source-layer": id,
        filter,
        paint,
      });
    }
  }
  return {
    version: 8,
    sources: {
      tiles: {
        type: "vector",
        tiles: [`${url}/{z}/{x}/{y}.pbf`],
        minzoom: set.minzoom,
        maxzoom: set.maxzoom,
      },
    },
    layers,
  };
}
export const key = (t) => `${t.z}/${t.x}/${t.y}`;
export function selectTiles(
  a,
  b,
  { samples = 3, seed = 1, worst = 3, report } = {},
) {
  let state = seed >>> 0;
  const rand = () => {
    state = (Math.imul(1664525, state) + 1013904223) >>> 0;
    return state / 2 ** 32;
  };
  const union = new Map([...a, ...b].map((t) => [key(t), t]));
  const groups = new Map();
  for (const t of [...union.values()].sort(
    (a, b) => a.z - b.z || a.x - b.x || a.y - b.y,
  )) {
    if (!groups.has(t.z)) groups.set(t.z, []);
    groups.get(t.z).push(t);
  }
  const selected = new Map();
  for (const tiles of groups.values()) {
    for (let i = tiles.length - 1; i > 0; i--) {
      const j = Math.floor(rand() * (i + 1));
      [tiles[i], tiles[j]] = [tiles[j], tiles[i]];
    }
    for (const t of tiles.slice(0, samples)) selected.set(key(t), t);
  }
  if (report) {
    const ranked = [...report.tiles].sort(
      (a, b) =>
        b.totals.max_distance - a.totals.max_distance ||
        b.totals.unmatched_a +
          b.totals.unmatched_b -
          (a.totals.unmatched_a + a.totals.unmatched_b) ||
        a.tile.z - b.tile.z ||
        a.tile.x - b.tile.x ||
        a.tile.y - b.tile.y,
    );
    for (const r of ranked.slice(0, worst))
      if (union.has(key(r.tile))) selected.set(key(r.tile), r.tile);
    for (const t of [
      ...report.tile_sets.only_a,
      ...report.tile_sets.only_b,
    ].slice(0, worst))
      if (union.has(key(t))) selected.set(key(t), t);
  }
  return [...selected.values()];
}
export function pixelDiff(a, b, threshold = 3) {
  if (a.width !== b.width || a.height !== b.height)
    throw new Error("Image dimensions differ");
  const diff = new PNG({ width: a.width, height: a.height });
  let count = 0;
  for (let i = 0; i < a.data.length; i += 4) {
    const d = Math.max(
      ...[0, 1, 2, 3].map((c) => Math.abs(a.data[i + c] - b.data[i + c])),
    );
    if (d > threshold) {
      count++;
      diff.data.set([255, 0, 128, 255], i);
    } else {
      const gray = Math.round((a.data[i] + a.data[i + 1] + a.data[i + 2]) / 3);
      diff.data.set([gray, gray, gray, 255], i);
    }
  }
  return { count, diff };
}

// Read large Rust reports one tile at a time; retain only ranking data.
export async function readRanking(filename, worst, a, b) {
  const [{ default: parserStream }, { pick }, { streamArray }, { pipeline }] =
    await Promise.all([
      import("stream-json"),
      import("stream-json/filters/pick.js"),
      import("stream-json/streamers/stream-array.js"),
      import("node:stream/promises"),
    ]);
  const ranked = [];
  const order = (a, b) =>
    b.totals.max_distance - a.totals.max_distance ||
    b.totals.unmatched_a +
      b.totals.unmatched_b -
      (a.totals.unmatched_a + a.totals.unmatched_b) ||
    a.tile.z - b.tile.z ||
    a.tile.x - b.tile.x ||
    a.tile.y - b.tile.y;
  await pipeline(
    fs.createReadStream(filename),
    parserStream(),
    pick.asStream({ filter: "tiles" }),
    streamArray.asStream(),
    async (source) => {
      for await (const { value } of source) {
        ranked.push({ tile: value.tile, totals: value.totals });
        ranked.sort(order);
        if (ranked.length > worst) ranked.pop();
      }
    },
  );
  const aa = new Set(a.map(key)),
    bb = new Set(b.map(key));
  return {
    tiles: ranked,
    tile_sets: {
      only_a: a.filter((t) => !bb.has(key(t))),
      only_b: b.filter((t) => !aa.has(key(t))),
    },
  };
}
