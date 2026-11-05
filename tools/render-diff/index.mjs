import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { chromium } from "playwright";
import { PNG } from "pngjs";
import {
  openTileset,
  readRanking,
  styleFor,
  selectTiles,
  pixelDiff,
  key,
} from "./lib.mjs";

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    out: { type: "string", default: "render-diff" },
    debug: { type: "boolean", default: false },
    samples: { type: "string", default: "3" },
    seed: { type: "string", default: "1" },
    worst: { type: "string", default: "3" },
    compare: { type: "string" },
    "color-threshold": { type: "string", default: "3" },
    "max-diff-pixels": { type: "string", default: "0" },
    help: { type: "boolean" },
  },
});
if (values.help || positionals.length !== 2) {
  console.log(
    "node index.mjs A B --out DIR [--debug] [--samples 3] [--seed 1] [--compare report.json] [--worst 3] [--color-threshold 3] [--max-diff-pixels 0]",
  );
  process.exit(values.help ? 0 : 2);
}
for (const opt of [
  "samples",
  "seed",
  "worst",
  "color-threshold",
  "max-diff-pixels",
])
  if (!Number.isSafeInteger(+values[opt]) || +values[opt] < 0)
    throw new Error(`Invalid --${opt}`);
if (+values.samples === 0) throw new Error("--samples must be positive");
const out = path.resolve(values.out);
fs.mkdirSync(out, { recursive: true });
const sets = [];
let server, browser;
try {
  sets.push(openTileset(positionals[0]));
  sets.push(openTileset(positionals[1]));
  const report = values.compare
    ? await readRanking(
        values.compare,
        +values.worst,
        sets[0].tiles,
        sets[1].tiles,
      )
    : null;
  const selected = selectTiles(sets[0].tiles, sets[1].tiles, {
    samples: +values.samples,
    seed: +values.seed,
    worst: +values.worst,
    report,
  });
  if (!selected.length) throw new Error("No tiles to render");
  const dist = fileURLToPath(
    new URL("./node_modules/maplibre-gl/dist/", import.meta.url),
  );
  server = http.createServer((req, res) => {
    try {
      req.url = new URL(req.url, "http://127.0.0.1").pathname;
      if (/^\/maplibre-gl(?:-shared|-worker)?\.mjs$/.test(req.url)) {
        res.setHeader("Content-Type", "application/javascript");
        res.end(fs.readFileSync(path.join(dist, req.url.slice(1))));
        return;
      }
      if (req.url === "/") {
        res.setHeader("Content-Type", "text/html");
        res.end(
          '<!doctype html><style>html,body{margin:0}#map{width:512px;height:512px}</style><div id="map"></div><script type="module">import * as maplibregl from "/maplibre-gl.mjs";window.maplibregl=maplibregl;</script>',
        );
        return;
      }
      const match = /^\/(a|b)\/(\d+)\/(\d+)\/(\d+)\.pbf$/.exec(req.url);
      if (!match) {
        res.writeHead(404).end();
        return;
      }
      const data = sets[match[1] === "a" ? 0 : 1].read(
        +match[2],
        +match[3],
        +match[4],
      );
      res.setHeader("Content-Type", "application/x-protobuf");
      res.writeHead(data ? 200 : 204);
      res.end(data || undefined);
    } catch (e) {
      console.error(e);
      res.writeHead(500).end();
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({
    headless: true,
    args: [
      "--no-sandbox",
      "--use-angle=swiftshader",
      "--enable-unsafe-swiftshader",
    ],
  });
  const styles = sets.map((s, i) =>
    styleFor(s, `${base}/${i ? "b" : "a"}`, values.debug),
  );
  for (let i = 0; i < 2; i++)
    fs.writeFileSync(
      path.join(out, `style-${i ? "b" : "a"}.json`),
      JSON.stringify(styles[i], null, 2),
    );
  const views = [];
  for (const ratio of [1, 2]) {
    const context = await browser.newContext({
      viewport: { width: 512, height: 512 },
      deviceScaleFactor: ratio,
    });
    const page = await context.newPage();
    await page.goto(base);
    await page.waitForFunction(() => !!window.maplibregl);
    for (const tile of selected)
      for (const offset of [0, 0.5]) {
        const zoom = tile.z + offset;
        const center = [
          ((tile.x + 0.5) / 2 ** tile.z) * 360 - 180,
          (Math.atan(
            Math.sinh(Math.PI * (1 - (2 * (tile.y + 0.5)) / 2 ** tile.z)),
          ) *
            180) /
            Math.PI,
        ];
        const stem = `${tile.z}-${tile.x}-${tile.y}-z${zoom}-r${ratio}`;
        const images = [];
        for (let i = 0; i < 2; i++) {
          const data = await page.evaluate(
            async ({ style, center, zoom }) => {
              if (window.diffMap) {
                window.diffMap.remove();
                window.diffMap = null;
              }
              const errors = [];
              const map = (window.diffMap = new maplibregl.Map({
                container: "map",
                style,
                center,
                zoom,
                interactive: false,
                attributionControl: false,
                fadeDuration: 0,
                renderWorldCopies: false,
                canvasContextAttributes: { preserveDrawingBuffer: true },
                maxZoom: 30,
              }));
              map.on("error", (e) =>
                errors.push(e.error?.message || String(e.error)),
              );
              await new Promise((resolve, reject) => {
                const timer = setTimeout(
                  () =>
                    reject(
                      new Error("Map render timeout: " + errors.join("; ")),
                    ),
                  30000,
                );
                map.once("idle", () => {
                  clearTimeout(timer);
                  resolve();
                });
              });
              if (errors.length) throw new Error(errors.join("; "));
              return map.getCanvas().toDataURL("image/png").split(",")[1];
            },
            { style: styles[i], center, zoom },
          );
          const buf = Buffer.from(data, "base64");
          fs.writeFileSync(path.join(out, `${stem}-${i ? "b" : "a"}.png`), buf);
          images.push(PNG.sync.read(buf));
        }
        const { count, diff } = pixelDiff(
          ...images,
          +values["color-threshold"],
        );
        const passed = count <= +values["max-diff-pixels"];
        const diffFile = passed ? null : `${stem}-diff.png`;
        if (diffFile)
          fs.writeFileSync(path.join(out, diffFile), PNG.sync.write(diff));
        views.push({
          tile,
          zoom,
          pixel_ratio: ratio,
          width: images[0].width,
          height: images[0].height,
          different_pixels: count,
          passed,
          a: `${stem}-a.png`,
          b: `${stem}-b.png`,
          diff: diffFile,
        });
        console.log(
          `${key(tile)} zoom=${zoom} ratio=${ratio}: ${count} pixels ${passed ? "PASS" : "FAIL"}`,
        );
      }
    await context.close();
  }
  const summary = {
    version: 1,
    renderer: "maplibre-gl 6.16.0 / Chromium SwiftShader",
    passed: views.every((v) => v.passed),
    style: values.debug ? "debug" : "viewer",
    seed: +values.seed,
    color_threshold: +values["color-threshold"],
    max_diff_pixels: +values["max-diff-pixels"],
    views,
  };
  fs.writeFileSync(
    path.join(out, "summary.json"),
    JSON.stringify(summary, null, 2),
  );
  fs.writeFileSync(
    path.join(out, "index.html"),
    `<!doctype html><meta charset="utf-8"><title>MapLibre comparison</title><style>body{font:14px sans-serif}section{border-top:1px solid #ccc}figure{display:inline-block;margin:4px;width:31%}img{width:100%}</style><h1>MapLibre comparison: ${summary.passed ? "PASS" : "FAIL"}</h1><p>Style: ${summary.style}. Color threshold: ${summary.color_threshold}. Pixel limit: ${summary.max_diff_pixels}.</p>${views.map((v) => `<section><h2>${key(v.tile)} zoom ${v.zoom}, ratio ${v.pixel_ratio}: ${v.different_pixels} pixels</h2><figure><figcaption>A</figcaption><img src="${v.a}"></figure><figure><figcaption>B</figcaption><img src="${v.b}"></figure><figure><figcaption>Diff</figcaption>${v.diff ? `<img src="${v.diff}">` : "PASS"}</figure></section>`).join("")}`,
  );
  console.log(
    `${views.length} views, ${views.filter((v) => !v.passed).length} failed; ${path.join(out, "index.html")}`,
  );
  process.exitCode = summary.passed ? 0 : 1;
} catch (e) {
  console.error(e);
  process.exitCode = 2;
} finally {
  await browser?.close();
  if (server) await new Promise((resolve) => server.close(resolve));
  sets.forEach((s) => s.close());
}
