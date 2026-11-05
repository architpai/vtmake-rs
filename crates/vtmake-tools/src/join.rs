//! `vtmake join`: a port of `tile-join` (`tile-join.cpp`) and the metadata
//! code that it uses (`mbtiles.cpp`: `make_metadata`, `tilestats`,
//! `merge_layermaps`, `add_to_tilestats`). The tiles and the metadata are
//! byte-identical to felt's tool; only `generator` names vtmake.
//!
//! tile-join decodes each tile and encodes it again: the layers of all inputs
//! with the same tile are merged (in the order of the tile bytes), the key and
//! value tables are made again, and the tile gets gzip again. Tiles larger than
//! 500000 bytes are skipped (not with `-pk`). The bounds come from the tile
//! geometry; the center, name, description and attribution come from the input
//! metadata.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::common::*;
use crate::getopt::{self, Long, Short};
use crate::json::Json;
use crate::jsonw::JsonWriter;
use crate::mvtdec::{self, CLOSEPATH, LINETO, MOVETO, Value};
use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use vtmake_mvt::{MvtFeature, MvtLayer, MvtPoint, MvtTile, MvtValue};
use vtmake_out::{DirWriter, MbtilesWriter, TileSink};

// `mvt_value_type` codes, for the tilestats types.
const T_STRING: u8 = 0;
const T_DOUBLE: u8 = 2;
const T_BOOL: u8 = 6;

/// The tilestats limits (`max_tilestats_*`).
#[derive(Clone, Copy)]
struct Limits {
    attributes: usize,
    sample_values: usize,
    values: usize,
}

/// `serial_val`: the value text and its type. The order is text, then type.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SerialVal {
    s: String,
    ty: u8,
}

#[derive(Clone)]
struct TileStat {
    samples: Vec<SerialVal>,
    min: f64,
    max: f64,
}

impl Default for TileStat {
    fn default() -> Self {
        TileStat { samples: Vec::new(), min: f64::INFINITY, max: f64::NEG_INFINITY }
    }
}

/// `layermap_entry`.
#[derive(Clone, Default)]
struct LayerEntry {
    tilestats: BTreeMap<String, TileStat>,
    minzoom: i32,
    maxzoom: i32,
    description: String,
    points: u64,
    lines: u64,
    polygons: u64,
}

type LayerMap = BTreeMap<String, LayerEntry>;

/// `add_to_tilestats`. `num` is the value of a number (the `atof` of its text).
fn add_to_tilestats(ts: &mut BTreeMap<String, TileStat>, attrib: &str, val: SerialVal, num: f64, lim: Limits) {
    let t = ts.entry(attrib.to_string()).or_default();
    if val.ty == T_DOUBLE {
        if num < t.min {
            t.min = num;
        }
        if num > t.max {
            t.max = num;
        }
    }
    let pt = t.samples.partition_point(|v| *v < val);
    if pt == t.samples.len() || t.samples[pt] != val {
        if t.samples.len() >= lim.sample_values {
            if pt != t.samples.len() {
                t.samples.insert(pt, val);
                t.samples.pop();
            }
        } else {
            t.samples.insert(pt, val);
        }
    }
}

/// Merge the layer map `m` into `out` (`merge_layermaps`). With `trunc`, the
/// layer and attribute names are cut to 256 UTF-16 units first.
fn merge_into(out: &mut LayerMap, m: LayerMap, trunc: bool, lim: Limits) {
    for (name, e) in m {
        if e.points + e.lines + e.polygons == 0 {
            continue;
        }
        let name = if trunc { truncate16(&name).to_string() } else { name };
        let o = out.entry(name).or_insert_with(|| LayerEntry { minzoom: e.minzoom, maxzoom: e.maxzoom, description: e.description.clone(), ..Default::default() });
        for (a, ts) in e.tilestats {
            let a = if trunc { truncate16(&a).to_string() } else { a };
            match o.tilestats.get_mut(&a) {
                None => {
                    o.tilestats.insert(a, ts);
                }
                Some(t2) => {
                    for v in ts.samples {
                        let pt = t2.samples.partition_point(|x| *x < v);
                        if pt == t2.samples.len() || t2.samples[pt] != v {
                            t2.samples.insert(pt, v);
                            if t2.samples.len() > lim.sample_values {
                                t2.samples.pop();
                            }
                        }
                    }
                    if ts.min < t2.min {
                        t2.min = ts.min;
                    }
                    if ts.max > t2.max {
                        t2.max = ts.max;
                    }
                }
            }
        }
        o.minzoom = o.minzoom.min(e.minzoom);
        o.maxzoom = o.maxzoom.max(e.maxzoom);
        o.points += e.points;
        o.lines += e.lines;
        o.polygons += e.polygons;
    }
}

/// The bounds in degrees (`arg::minlat` and so on).
#[derive(Clone, Copy)]
struct Bounds {
    minlat: f64,
    minlon: f64,
    maxlat: f64,
    maxlon: f64,
    minlon2: f64,
    maxlon2: f64,
}

impl Bounds {
    fn new() -> Self {
        let (mx, mn) = (i32::MAX as f64, i32::MIN as f64);
        Bounds { minlat: mx, minlon: mx, maxlat: mn, maxlon: mn, minlon2: mx, maxlon2: mn }
    }
    fn merge(&mut self, o: &Bounds) {
        self.minlat = self.minlat.min(o.minlat);
        self.minlon = self.minlon.min(o.minlon);
        self.maxlat = self.maxlat.max(o.maxlat);
        self.maxlon = self.maxlon.max(o.maxlon);
        self.minlon2 = self.minlon2.min(o.minlon2);
        self.maxlon2 = self.maxlon2.max(o.maxlon2);
    }
}

/// What the workers collect.
struct Acc {
    layermap: LayerMap,
    bounds: Bounds,
    outputs: Vec<((i64, i64, i64), Vec<u8>)>,
}

struct Opts {
    pk: bool,
    pc: bool,
    pg: bool,
    quiet: bool,
    minzoom: i32,
    maxzoom: i32,
    renames: BTreeMap<String, String>,
    exclude_all: bool,
    exclude_all_tile_attributes: bool,
    exclude: BTreeSet<String>,
    include: BTreeSet<String>,
    keep_layers: BTreeSet<String>,
    remove_layers: BTreeSet<String>,
    ifmatched: bool,
    empty_null: bool,
    csv_header: Vec<String>,
    csv_rows: BTreeMap<String, Vec<Vec<u8>>>,
    lim: Limits,
    /// `-j`/`-J`.
    filter: Option<vtmake_filter::Filter>,
    exclude_all_tile_geometries: bool,
}

fn csv_number(raw: &[u8]) -> Value {
    let text=String::from_utf8_lossy(raw);
    if !text.is_empty() && text.bytes().all(|c|c.is_ascii_digit()) && let Ok(n)=text.parse::<u64>() {
        return if n<=i64::MAX as u64 {Value::Int(n as i64)} else {Value::Uint(n)};
    }
    if text.starts_with('-') && text[1..].bytes().all(|c|c.is_ascii_digit()) && let Ok(n)=text.parse::<i64>() {return Value::Sint(n);}
    let d=atof(&text); let f=d as f32;
    if f as f64==d {Value::Float(f)} else {Value::Double(d)}
}

fn read_csv(o: &mut Opts, path: &str) {
    use vtmake_parse::csv::{csv_dequote, csv_split};
    let data = std::fs::read(path).unwrap_or_else(|e| Fatal { msg: format!("{path}: {}", strerror(&e)), code: EXIT_OPEN }.exit());
    let mut lines = data.split_inclusive(|&b| b == b'\n');
    o.csv_header = lines.next().map(|l| csv_split(l).iter().map(|v| utf8(csv_dequote(v))).collect()).unwrap_or_default();
    o.csv_rows.clear();
    for line in lines {
        let fields = csv_split(line);
        if let Some(first) = fields.first() {
            o.csv_rows.entry(utf8(csv_dequote(first))).or_insert(fields);
        }
    }
}

fn utf8(b: Vec<u8>) -> String {
    String::from_utf8(b).unwrap_or_else(|_| not_supported("join", "a tile string that is not UTF-8"))
}

/// `mvt_value_to_serial_val`, and the number value for the tilestats.
fn serial(v: &Value) -> (SerialVal, f64) {
    let d = |s: String, n: f64| (SerialVal { s, ty: T_DOUBLE }, n);
    match v {
        Value::String(s) => (SerialVal { s: String::from_utf8_lossy(s).into_owned(), ty: T_STRING }, 0.0),
        Value::Float(f) => d(vtmake_parse::dtoa::dtoa_milo(*f as f64), *f as f64),
        Value::Double(f) => d(vtmake_parse::dtoa::dtoa_milo(*f), *f),
        Value::Int(i) | Value::Sint(i) => d(i.to_string(), *i as f64),
        Value::Uint(u) => d(u.to_string(), *u as f64),
        Value::Bool(b) => (SerialVal { s: if *b { "true" } else { "false" }.into(), ty: T_BOOL }, 0.0),
        Value::Null => (SerialVal { s: "null".into(), ty: 7 }, 0.0),
    }
}

fn mvt_value(v: &Value) -> MvtValue {
    match v {
        Value::String(s) => MvtValue::String(utf8(s.clone()).into()),
        Value::Float(f) => MvtValue::Float(*f),
        Value::Double(f) => MvtValue::Double(*f),
        Value::Int(i) => MvtValue::Int(*i),
        Value::Uint(u) => MvtValue::Uint(*u),
        Value::Sint(i) => MvtValue::Sint(*i),
        Value::Bool(b) => MvtValue::Bool(*b),
        Value::Null => {
            eprintln!("Internal error: trying to write null attribute to tile");
            std::process::exit(EXIT_IMPOSSIBLE)
        }
    }
}

/// `append_tile`: add the layers of one input tile to `out`.
fn append_tile(o: &Opts, message: &[u8], z: i64, x: i64, y: i64, acc: &mut Acc, out: &mut Vec<MvtLayer>) {
    let layers = match mvtdec::decode(message) {
        Ok((l, _)) => l,
        Err(mvtdec::Error::Decompress) => Fatal { msg: "Decompression error: data error".into(), code: EXIT_MVT }.exit(),
        Err(mvtdec::Error::Pbf) => Fatal { msg: format!("PBF decoding error in tile {z}/{x}/{y}"), code: EXIT_MVT }.exit(),
    };

    for layer in layers {
        let mut name = utf8(layer.name.clone());
        if let Some(n) = o.renames.get(&name) {
            name = n.clone();
        }
        if !o.keep_layers.is_empty() && !o.keep_layers.contains(&name) {
            continue;
        }
        if o.remove_layers.contains(&name) {
            continue;
        }

        let ol = match out.iter().position(|l| l.name == name) {
            Some(i) => i,
            None => {
                let mut l = MvtLayer::new(name.clone());
                l.version = layer.version;
                l.extent = layer.extent;
                out.push(l);
                out.len() - 1
            }
        };
        let outlayer = &mut out[ol];

        if layer.extent > outlayer.extent {
            // Scale up the layer that is there; scaling down would need cleaning.
            let (le, oe) = (layer.extent as i64, outlayer.extent as i64);
            for f in &mut outlayer.features {
                for g in &mut f.geometry {
                    g.x = g.x * le / oe;
                    g.y = g.y * le / oe;
                }
            }
            outlayer.extent = layer.extent;
        }

        let (mut minx, mut miny, mut maxx, mut maxy) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        let (mut minx2, mut maxx2) = (i64::MAX, i64::MIN);
        let mut added = false;
        let keys: Vec<String> = layer.keys.iter().map(|k| utf8(k.clone())).collect();

        for feat in &layer.features {
            // `attribute-filter` removes these keys.
            let mut excluded: Vec<Box<str>> = Vec::new();
            if let Some(f) = &o.filter {
                let get = |k: &str| crate::overzoom::feature_value(&layer, feat.id, feat.ty, &feat.tags, z as i32, k);
                if !f.evaluate(&name, &get, &mut excluded) {
                    continue;
                }
            }
            let kept = |key: &str| !o.exclude.contains(key) && !excluded.iter().any(|e| &**e == key);
            if o.ifmatched && !feat.tags.as_chunks::<2>().0.iter().any(|pair| {
                keys.get(pair[0] as usize).is_some_and(|key| o.csv_header.first()==Some(key)) &&
                layer.values.get(pair[1] as usize).is_some_and(|v| o.csv_rows.contains_key(&serial(v).0.s))
            }) { continue; }
            let lm = acc.layermap.entry(name.clone()).or_insert_with(|| LayerEntry { minzoom: z as i32, maxzoom: z as i32, ..Default::default() });
            let mut of = MvtFeature { id: feat.id, ..Default::default() };
            if o.csv_header.is_empty() {
                if o.ifmatched {
                    // No CSV: no feature matches.
                    continue;
                }
                // The attributes in their order. A key that comes again keeps its
                // first value (`std::map::insert`).
                let mut attrs: Vec<(usize, usize)> = Vec::new();
                if !o.exclude_all_tile_attributes {
                    let mut t = 0;
                    while t + 1 < feat.tags.len() {
                        let (k, v) = (feat.tags[t] as usize, feat.tags[t + 1] as usize);
                        t += 2;
                        let (Some(key), Some(_)) = (keys.get(k), layer.values.get(v)) else {
                            Fatal { msg: format!("PBF decoding error in tile {z}/{x}/{y}"), code: EXIT_MVT }.exit()
                        };
                        if o.include.contains(key) || (!o.exclude_all && kept(key)) {
                            if attrs.iter().any(|&(k2, _)| keys[k2] == *key) {
                                continue;
                            }
                            attrs.push((k, v));
                        }
                    }
                }

                for &(k, v) in &attrs {
                    let val = &layer.values[v];
                    outlayer.tag(&mut of, &keys[k], &mvt_value(val));
                    let (sv, num) = serial(val);
                    if sv.ty != 7 {
                        add_to_tilestats(&mut lm.tilestats, &keys[k], sv, num, o.lim);
                    }
                }
            } else {
                let mut attrs: Vec<(String, Value)> = Vec::new();
                let mut matched = false;
                let mut order: Vec<String> = Vec::new();
                for tags in feat.tags.as_chunks::<2>().0 {
                    let (k, v) = (tags[0] as usize, tags[1] as usize);
                    let (Some(key), Some(val)) = (keys.get(k), layer.values.get(v)) else {
                        Fatal { msg: format!("PBF decoding error in tile {z}/{x}/{y}"), code: EXIT_MVT }.exit()
                    };
                    if *val == Value::Null {
                        continue;
                    }
                    if !o.exclude_all_tile_attributes && (o.include.contains(key) || (!o.exclude_all && kept(key))) {
                        if !attrs.iter().any(|(k, _)| k == key) {
                            attrs.push((key.clone(), val.clone()));
                        }
                        order.push(key.clone());
                    }
                    if o.csv_header.first() == Some(key)
                        && let Some(fields) = o.csv_rows.get(&serial(val).0.s)
                    {
                        matched = true;
                        for (i, raw) in fields.iter().enumerate().skip(1) {
                            let Some(key) = o.csv_header.get(i) else {
                                break;
                            };
                            let null = raw.is_empty() && o.empty_null;
                            if o.include.contains(key) || (!o.exclude_all && kept(key) && !null) {
                                let value = if raw.first() == Some(&b'"') {
                                    Value::String(vtmake_parse::csv::csv_dequote(raw))
                                } else if vtmake_parse::csv::is_number(raw) || null {
                                    csv_number(raw)
                                } else {
                                    Value::String(raw.clone())
                                };
                                attrs.retain(|(k, _)| k != key);
                                attrs.push((key.clone(), value));
                                order.push(key.clone());
                            }
                        }
                    }
                }
                if !matched && o.ifmatched {
                    continue;
                }
                for key in order {
                    if let Some(i) = attrs.iter().position(|(k, _)| *k == key) {
                        let (key, val) = attrs.remove(i);
                        outlayer.tag(&mut of, &key, &mvt_value(&val));
                        let (sv, num) = serial(&val);
                        if sv.ty != 7 {
                            add_to_tilestats(&mut lm.tilestats, &key, sv, num, o.lim);
                        }
                    }
                }
            }

            if o.exclude_all_tile_geometries {
                of.geom_type = vtmake_mvt::NO_GEOM_TYPE;
            } else {
                if !(0..=254).contains(&feat.ty) {
                    not_supported("join", "a feature type that is not 0 to 254");
                }
                of.geom_type = feat.ty as u8;
                let (le, oe) = (layer.extent as i64, outlayer.extent as i64);
                of.geometry = feat
                    .geometry
                    .iter()
                    .map(|g| {
                        if g.op != MOVETO && g.op != LINETO && g.op != CLOSEPATH {
                            Fatal { msg: "\nInternal error: corrupted geometry".into(), code: EXIT_IMPOSSIBLE }.exit()
                        }
                        let (mut gx, mut gy) = (g.x, g.y);
                        if le != oe {
                            gx = gx * oe / le;
                            gy = gy * oe / le;
                        }
                        MvtPoint { op: g.op as u8, x: gx, y: gy }
                    })
                    .collect();
                for g in &of.geometry {
                    if g.op == MOVETO as u8 || g.op == LINETO as u8 {
                        // Pin to the tile extent: the bounds are not larger than the earth.
                        let mut gx = g.x.clamp(0, oe);
                        let mut gy = g.y.clamp(0, oe);
                        let shift = 1i64 << (32 - z);
                        gx = gx * shift / oe;
                        gy = gy * shift / oe;
                        gx += shift * x;
                        gy += shift * y;
                        minx = minx.min(gx);
                        miny = miny.min(gy);
                        maxx = maxx.max(gx);
                        maxy = maxy.max(gy);
                        // In the western hemisphere: also try a shift to the east.
                        if gx < 1i64 << 31 {
                            gx += 1i64 << 32;
                        }
                        minx2 = minx2.min(gx);
                        maxx2 = maxx2.max(gx);
                    }
                }
            }

            added = true;
            outlayer.features.push(of);
            let z = z as i32;
            lm.minzoom = lm.minzoom.min(z);
            lm.maxzoom = lm.maxzoom.max(z);
            match feat.ty {
                1 => lm.points += 1,
                2 => lm.lines += 1,
                3 => lm.polygons += 1,
                _ => {}
            }
        }

        if added {
            let b = &mut acc.bounds;
            let (lon1, lat1) = vtmake_geom::tile2lonlat(minx, maxy, 32);
            let (lon2, lat2) = vtmake_geom::tile2lonlat(maxx, miny, 32);
            b.minlat = b.minlat.min(lat1.min(lat2));
            b.minlon = b.minlon.min(lon1.min(lon2));
            b.maxlat = b.maxlat.max(lat1.max(lat2));
            b.maxlon = b.maxlon.max(lon1.max(lon2));
            let (lon1, _) = vtmake_geom::tile2lonlat(minx2, maxy, 32);
            let (lon2, _) = vtmake_geom::tile2lonlat(maxx2, miny, 32);
            b.minlon2 = b.minlon2.min(lon1.min(lon2));
            b.maxlon2 = b.maxlon2.max(lon1.max(lon2));
        }
    }
}

/// The geometry deltas must fit in 32 bits (`mvt_tile::encode`).
fn check_deltas(layers: &[MvtLayer]) {
    for l in layers {
        for f in &l.features {
            let (mut px, mut py) = (0i64, 0i64);
            for g in &f.geometry {
                if g.op == MOVETO as u8 || g.op == LINETO as u8 {
                    let (dx, dy) = (g.x - px, g.y - py);
                    if i32::try_from(dx).is_err() || i32::try_from(dy).is_err() {
                        Fatal { msg: format!("Internal error: Geometry delta is too big: {dx},{dy}"), code: EXIT_IMPOSSIBLE }.exit()
                    }
                    (px, py) = (g.x, g.y);
                }
            }
        }
    }
}

/// `join_worker` for one tile.
fn join_tile(o: &Opts, key: (i64, i64, i64), inputs: Vec<Vec<u8>>, acc: &mut Acc) {
    let (z, x, y) = key;
    let mut layers: Vec<MvtLayer> = Vec::new();
    for m in &inputs {
        append_tile(o, m, z, x, y, acc, &mut layers);
    }
    layers.retain(|l| !l.features.is_empty());
    if layers.is_empty() {
        return;
    }
    check_deltas(&layers);
    let mut pbf = Vec::new();
    MvtTile { layers }.encode(&mut pbf);
    let data = if o.pc {
        pbf
    } else {
        let mut gz = Vec::with_capacity(pbf.len() / 2 + 64);
        vtmake_mvt::gzip::gzip(&pbf, &mut gz);
        gz
    };
    if !o.pk && data.len() > 500000 {
        eprintln!("Tile {z}/{x}/{y} size is {}, >500000. Skipping this tile.", data.len());
    } else {
        acc.outputs.push((key, data));
    }
}

/// An input tileset (`tileset_reader`), in z, x, TMS y order.
///
/// With `-O`, a reader that is done continues with overzoomed tiles: the
/// children of its tiles at its highest zoom, then the children that have
/// features, one zoom at a time. The queue loop stops when all readers are
/// done and no reader has given an overzoomed tile at its current zoom.
struct Reader {
    zoom: i64,
    x: i64,
    y: i64,
    data: Vec<u8>,
    done: bool,
    name: String,
    src: Src,
    /// `-O`: the buffer (`-b`) for the overzoomed tiles.
    overzoom: Option<i32>,
    current_tile_is_overzoomed: bool,
    overzoom_consumed_at_this_zoom: bool,
    maxzoom_so_far: i64,
    tiles_at_maxzoom_so_far: Vec<(u32, u32)>,
    /// The tiles at `zoom` that are left (the next one is the last).
    overzoomed_tiles: Vec<(u32, u32)>,
    /// The tiles at `zoom + 1` that have features.
    next_overzoomed_tiles: Vec<(u32, u32)>,
    cache: TileCache,
    /// A second connection for `get_tile` (MBTiles input).
    tile_db: Option<Connection>,
}

/// The parent tile cache of `-O` (`tile_cache`, 1000 tiles).
#[derive(Default)]
struct TileCache {
    tiles: BTreeMap<(i64, i64, i64), (crate::overzoom::SourceTile, u64)>,
    seq: u64,
}

enum Src {
    /// The rows come from a thread that owns the database connection.
    Mbtiles(std::sync::mpsc::Receiver<(i32, i32, i32, Vec<u8>)>),
    Pmtiles {
        tiles: std::vec::IntoIter<(i64, i64, i64, Vec<u8>)>,
        db: Connection,
    },
    Dir {
        tiles: Vec<Zxy>,
        next: usize,
    },
}

const TILES_SQL: &str = "SELECT zoom_level, tile_column, tile_row, tile_data from tiles order by zoom_level, tile_column, tile_row;";

impl Reader {
    fn open(fname: &str, minzoom: i32, maxzoom: i32, overzoom: Option<i32>) -> Result<Reader, Fatal> {
        let src = if is_dir(fname) {
            Src::Dir { tiles: enumerate_dirtiles(fname, minzoom, maxzoom)?, next: 0 }
        } else if pmtiles_has_suffix(fname) {
            {
                let db = vtmake_out::pmtiles::open(fname).or_else(|e| fatal(format!("{fname}: {}", strerror(&e)), EXIT_OPEN))?;
                let mut st = db.prepare(TILES_SQL).unwrap();
                let tiles = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
                drop(st);
                Src::Pmtiles { tiles: tiles.into_iter(), db }
            }
        } else {
            let db = Connection::open(fname).or_else(|e| fatal(format!("{fname}: {}", sql_msg(&e)), EXIT_SQLITE))?;
            if let Err(e) = db.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)) {
                return fatal(format!("{fname}: integrity_check: {}", sql_msg(&e)), EXIT_SQLITE);
            }
            if let Err(e) = db.prepare(TILES_SQL) {
                return fatal(format!("{fname}: select failed: {}", sql_msg(&e)), EXIT_SQLITE);
            }
            let (tx, rx) = std::sync::mpsc::sync_channel(256);
            std::thread::spawn(move || {
                let Ok(mut st) = db.prepare(TILES_SQL) else {
                    return;
                };
                let Ok(mut rows) = st.query([]) else { return };
                while let Ok(Some(r)) = rows.next() {
                    let get = |i| r.get_ref(i).map(col_int).unwrap_or(0);
                    let data = match r.get_ref(3) {
                        Ok(rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b)) => b.to_vec(),
                        _ => Vec::new(),
                    };
                    if tx.send((get(0), get(1), get(2), data)).is_err() {
                        return;
                    }
                }
            });
            Src::Mbtiles(rx)
        };
        let mut r = Reader {
            zoom: 0,
            x: 0,
            y: 0,
            data: Vec::new(),
            done: false,
            name: fname.to_string(),
            src,
            overzoom,
            current_tile_is_overzoomed: false,
            overzoom_consumed_at_this_zoom: false,
            maxzoom_so_far: -1,
            tiles_at_maxzoom_so_far: Vec::new(),
            overzoomed_tiles: Vec::new(),
            next_overzoomed_tiles: Vec::new(),
            cache: TileCache::default(),
            tile_db: None,
        };
        r.advance();
        Ok(r)
    }

    /// `all_done` for this reader.
    fn finished(&self) -> bool {
        self.done && !self.overzoom_consumed_at_this_zoom
    }

    /// `current`: the reader gives its tile.
    fn take_current(&mut self) {
        if self.current_tile_is_overzoomed {
            self.overzoom_consumed_at_this_zoom = true;
        }
    }

    fn advance(&mut self) {
        if self.done {
            let Some(buffer) = self.overzoom else {
                Fatal { msg: "overzoom advance called without -O".into(), code: EXIT_IMPOSSIBLE }.exit()
            };
            if self.overzoomed_tiles.is_empty() {
                self.next_overzoom();
                self.overzoom_consumed_at_this_zoom = false;
            }
            let Some((x, y)) = self.overzoomed_tiles.pop() else {
                // Nothing to overzoom: give up.
                self.current_tile_is_overzoomed = false;
                self.zoom = 32;
                return;
            };
            self.x = x as i64;
            self.y = y as i64;
            self.data = self.retrieve_overzoom(self.zoom, x as i64, y as i64, buffer);
            self.current_tile_is_overzoomed = true;
            return;
        }
        self.current_tile_is_overzoomed = false;
        match &mut self.src {
            Src::Mbtiles(rx) => match rx.recv() {
                Ok((z, x, tms_y, data)) => {
                    self.zoom = z as i64;
                    self.x = x as i64;
                    self.y = (1i64 << self.zoom) - 1 - tms_y as i64;
                    self.data = data;
                }
                Err(_) => self.done = true,
            },
            Src::Pmtiles { tiles, .. } => match tiles.next() {
                Some((z, x, y, data)) => {
                    self.zoom = z;
                    self.x = x;
                    self.y = (1i64 << z) - 1 - y;
                    self.data = data;
                }
                None => self.done = true,
            },
            Src::Dir { tiles, next } => {
                if *next == tiles.len() {
                    self.done = true;
                } else {
                    let t = &tiles[*next];
                    *next += 1;
                    self.zoom = t.z;
                    self.x = t.x;
                    self.y = t.y;
                    // `dir_read_tile`: a file that does not read gives no data.
                    self.data = std::fs::read(format!("{}/{}", self.name, t.path())).unwrap_or_default();
                }
            }
        }
        if self.done {
            if self.overzoom.is_some() {
                self.next_overzoom();
                self.advance();
            } else {
                self.zoom = 32;
            }
        } else {
            if self.zoom > self.maxzoom_so_far {
                self.maxzoom_so_far = self.zoom;
                self.tiles_at_maxzoom_so_far.clear();
            }
            if self.overzoom.is_some() {
                self.tiles_at_maxzoom_so_far.push((self.x as u32, self.y as u32));
            }
        }
    }

    /// `next_overzoom`: the tiles of the next zoom. At the first overzoomed
    /// zoom, all children of the tiles at the highest zoom; after that, the
    /// children that have features.
    fn next_overzoom(&mut self) {
        self.zoom += 1;
        self.overzoomed_tiles.clear();
        // +1 because maxzoom_so_far is -1 at first.
        let scale = (1i64 << (self.zoom + 1)) / (1i64 << (self.maxzoom_so_far + 1));
        if self.zoom == self.maxzoom_so_far + 1 {
            for &(x, y) in &self.tiles_at_maxzoom_so_far {
                for xx in 0..scale {
                    for yy in 0..scale {
                        self.overzoomed_tiles.push(((x as i64 * scale + xx) as u32, (y as i64 * scale + yy) as u32));
                    }
                }
            }
        } else {
            self.overzoomed_tiles = std::mem::take(&mut self.next_overzoomed_tiles);
        }
        // `tilecmp`, from the end: x up, then TMS y up.
        self.overzoomed_tiles.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.overzoom_consumed_at_this_zoom = false;
    }

    /// `get_tile`: the decoded tile z/x/y of this tileset (no layers if it is not there).
    fn get_tile(&mut self, z: i64, x: i64, y: i64) -> Vec<mvtdec::Layer> {
        const SQL: &str = "SELECT tile_data from tiles where zoom_level = ? and tile_column = ? and tile_row = ?;";
        let query = |db: &Connection, name: &str| -> Vec<u8> {
            let mut st = db.prepare(SQL).unwrap_or_else(|e| Fatal { msg: format!("{name}: select failed: {}", sql_msg(&e)), code: EXIT_SQLITE }.exit());
            let tms = (1i64 << z) - 1 - y;
            let mut rows = st.query(rusqlite::params![z as i32, x as i32, tms as i32]).unwrap();
            match rows.next() {
                Ok(Some(r)) => match r.get_ref(0) {
                    Ok(rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b)) => b.to_vec(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            }
        };
        let source = match &self.src {
            Src::Mbtiles(_) => {
                if self.tile_db.is_none() {
                    let db = Connection::open(&self.name).unwrap_or_else(|e| Fatal { msg: format!("{}: {}", self.name, sql_msg(&e)), code: EXIT_SQLITE }.exit());
                    self.tile_db = Some(db);
                }
                query(self.tile_db.as_ref().unwrap(), &self.name)
            }
            Src::Pmtiles { db, .. } => query(db, &self.name),
            Src::Dir { .. } => std::fs::read(format!("{}/{}", self.name, Zxy { z, x, y, mvt: false }.path())).unwrap_or_default(),
        };
        if source.is_empty() {
            return Vec::new();
        }
        match mvtdec::decode(&source) {
            Ok((l, _)) => l,
            Err(mvtdec::Error::Decompress) => Fatal { msg: "Decompression error: data error".into(), code: EXIT_MVT }.exit(),
            Err(mvtdec::Error::Pbf) => Fatal { msg: format!("PBF decoding error in tile {z}/{x}/{y}"), code: EXIT_PROTOBUF }.exit(),
        }
    }

    /// `retrieve_overzoom`: the tile z/x/y made from its parent at the
    /// highest zoom (not compressed), or empty.
    fn retrieve_overzoom(&mut self, z: i64, x: i64, y: i64, buffer: i32) -> Vec<u8> {
        let (mut pz, mut px, mut py) = (z, x, y);
        while pz > self.maxzoom_so_far {
            pz -= 1;
            px /= 2;
            py /= 2;
        }
        let key = (pz, px, py);
        let seq = self.cache.seq;
        self.cache.seq += 1;
        if let Some(e) = self.cache.tiles.get_mut(&key) {
            e.1 = seq;
        } else {
            if self.cache.tiles.len() >= 1000 {
                // Remove the oldest tile.
                let oldest = self.cache.tiles.iter().min_by_key(|(_, (_, s))| *s).map(|(k, _)| *k).unwrap();
                self.cache.tiles.remove(&oldest);
            }
            let layers = self.get_tile(pz, px, py);
            let t = crate::overzoom::SourceTile { layers, z: pz as i32, x: px as i32, y: py as i32 };
            self.cache.tiles.insert(key, (t, seq));
        }
        let source = &self.cache.tiles[&key].0;
        if source.layers.is_empty() {
            return Vec::new();
        }
        let p = crate::overzoom::Params::join(buffer);
        crate::overzoom::overzoom(std::slice::from_ref(source), z as i32, x as i32, y as i32, &p, Some(&mut self.next_overzoomed_tiles))
    }

    /// `tileset_reader::operator<`.
    fn less(&self, r: &Reader) -> bool {
        if self.zoom != r.zoom {
            return self.zoom < r.zoom;
        }
        if self.x != r.x {
            return self.x < r.x;
        }
        let sorty = ((1i64 << self.zoom) - 1 - self.y) as i32;
        let r_sorty = ((1i64 << r.zoom) - 1 - r.y) as i32;
        if sorty != r_sorty {
            return sorty < r_sorty;
        }
        self.data < r.data
    }

    /// The metadata database: the mbtiles file or a copy of `metadata.json`.
    fn metadata_db(&self) -> Result<Connection, Fatal> {
        match self.src {
            Src::Dir { .. } => dirmeta2tmp(&self.name),
            Src::Pmtiles { .. } => vtmake_out::pmtiles::open(&self.name).or_else(|e| fatal(format!("{}: {}", self.name, strerror(&e)), EXIT_OPEN)),
            Src::Mbtiles(_) => Connection::open(&self.name).or_else(|e| fatal(format!("{}: {}", self.name, sql_msg(&e)), EXIT_SQLITE)),
        }
    }
}

/// Put `r` into the queue before the first reader that it sorts before.
fn enqueue(readers: &mut Vec<Reader>, r: Reader) {
    let p = readers.iter().position(|q| r.less(q)).unwrap_or(readers.len());
    readers.insert(p, r);
}

type Tasks = BTreeMap<(i64, i64, std::cmp::Reverse<i64>), ((i64, i64, i64), Vec<Vec<u8>>)>;

/// `dispatch_tasks`: join the tiles of `tasks` in parallel and write them.
fn dispatch(o: &Opts, tasks: Tasks, sink: &mut dyn TileSink, total: &mut Acc) {
    if let Some((_, (k, _))) = tasks.iter().next()
        && !o.quiet
    {
        eprint!("{}/{}/{}  \r", k.0, k.1, k.2);
    }
    let new_acc = || Acc { layermap: LayerMap::new(), bounds: Bounds::new(), outputs: Vec::new() };
    let acc = tasks
        .into_par_iter()
        .fold(new_acc, |mut acc, (_, (key, inputs))| {
            join_tile(o, key, inputs, &mut acc);
            acc
        })
        .reduce(new_acc, |mut a, b| {
            merge_into(&mut a.layermap, b.layermap, false, o.lim);
            a.bounds.merge(&b.bounds);
            a.outputs.extend(b.outputs);
            a
        });
    merge_into(&mut total.layermap, acc.layermap, false, o.lim);
    total.bounds.merge(&acc.bounds);
    let mut outputs = acc.outputs;
    outputs.sort_by_key(|((z, x, y), _)| (*z, *x, std::cmp::Reverse(*y)));
    for ((z, x, y), data) in outputs {
        if let Err(e) = sink.write_tile(z as u8, x as u32, y as u32, &data) {
            eprintln!("{z}/{x}/{y}: {e}");
            std::process::exit(EXIT_WRITE);
        }
    }
}

/// `strategy` (the counters of `tile.hpp`), in the order of `handle_strategies`.
const STRATEGY_KEYS: [&str; 9] = [
    "dropped_by_rate",
    "dropped_by_gamma",
    "dropped_as_needed",
    "coalesced_as_needed",
    "truncated_zooms",
    "detail_reduced",
    "tiny_polygons",
    "tile_size_desired",
    "feature_count_desired",
];

/// The output order of `stringify_strategies`, as indexes into [`STRATEGY_KEYS`].
const STRATEGY_OUT: [usize; 9] = [0, 1, 2, 3, 5, 6, 7, 8, 4];

fn handle_strategies(s: &[u8], st: &mut Vec<[u64; 9]>) {
    let text = String::from_utf8_lossy(s);
    let Ok(Json::Array(a)) = crate::json::parse(s) else {
        return;
    };
    for (i, h) in a.iter().enumerate() {
        let Json::Hash(es) = h else {
            eprintln!("Element {i} is not a hash: {text}");
            continue;
        };
        for (j, (k, v)) in es.iter().enumerate() {
            let Some(k) = k.str() else {
                eprintln!("Key {j} of {i} is not a string: {text}");
                continue;
            };
            let Some(n) = v.number() else {
                eprintln!("Value {j} of {i} is not a number: {text}");
                continue;
            };
            if i >= st.len() {
                st.resize(i + 1, [0; 9]);
            }
            if let Some(p) = STRATEGY_KEYS.iter().position(|sk| sk.as_bytes() == k) {
                st[i][p] = (st[i][p] as f64 + n) as u64;
            }
        }
    }
}

fn stringify_strategies(st: &[[u64; 9]]) -> String {
    let mut w = JsonWriter::mem();
    let mut any = false;
    w.nospace = true;
    w.write_array();
    for s in st {
        w.nospace = true;
        w.write_hash();
        for &p in &STRATEGY_OUT {
            if s[p] > 0 {
                w.nospace = true;
                w.write_str(STRATEGY_KEYS[p]);
                w.nospace = true;
                w.write_number(s[p] as f64);
                any = true;
            }
        }
        w.nospace = true;
        w.end_hash();
    }
    w.nospace = true;
    w.end_array();
    if any { w.into_string() } else { String::new() }
}

fn handle_vector_layers(vl: Option<&Json>, layermap: &mut LayerMap, descs: &mut BTreeMap<String, String>) {
    let Some(Json::Array(a)) = vl else { return };
    for e in a {
        if !matches!(e, Json::Hash(_)) {
            continue;
        }
        if let (Some(id), Some(desc)) = (e.get("id").and_then(Json::str), e.get("description").and_then(Json::str))
            && !desc.is_empty()
            && let Some(l) = layermap.get_mut(&*String::from_utf8_lossy(id))
        {
            l.description = String::from_utf8_lossy(desc).into_owned();
        }
        if let Some(Json::Hash(fields)) = e.get("fields") {
            for (k, v) in fields {
                if let (Some(k), Some(v)) = (k.str(), v.str())
                    && !matches!(v, b"Number" | b"String" | b"Boolean" | b"Mixed")
                {
                    descs.entry(String::from_utf8_lossy(k).into_owned()).or_insert_with(|| String::from_utf8_lossy(v).into_owned());
                }
            }
        }
    }
}

fn type_bits(samples: &[SerialVal]) -> u32 {
    samples.iter().fold(0, |t, s| t | 1 << s.ty)
}

/// `vector_layers` (`make_metadata`).
fn vector_layers(layermap: &LayerMap, descs: &BTreeMap<String, String>, lim: Limits) -> String {
    let mut w = JsonWriter::mem();
    w.nospace = true;
    w.write_array();
    for (name, e) in layermap {
        w.nospace = true;
        w.write_hash();
        for (k, v) in [("id", name.as_str()), ("description", e.description.as_str())] {
            w.nospace = true;
            w.write_str(k);
            w.nospace = true;
            w.write_str(v);
        }
        for (k, v) in [("minzoom", e.minzoom), ("maxzoom", e.maxzoom)] {
            w.nospace = true;
            w.write_str(k);
            w.nospace = true;
            w.write_signed(v as i64);
        }
        w.nospace = true;
        w.write_str("fields");
        w.nospace = true;
        w.write_hash();
        let mut count = 0;
        for (a, ts) in &e.tilestats {
            w.nospace = true;
            w.write_str(a);
            w.nospace = true;
            match descs.get(a) {
                Some(d) => w.write_str(d),
                None => w.write_str(match type_bits(&ts.samples) {
                    t if t == 1 << T_DOUBLE => "Number",
                    t if t == 1 << T_BOOL => "Boolean",
                    t if t == 1 << T_STRING => "String",
                    _ => "Mixed",
                }),
            }
            count += 1;
            if count >= lim.attributes {
                break;
            }
        }
        w.nospace = true;
        w.end_hash();
        w.nospace = true;
        w.end_hash();
    }
    w.nospace = true;
    w.end_array();
    w.into_string()
}

/// `tilestats` (`mbtiles.cpp`).
fn tilestats(layermap1: &LayerMap, elements: usize, lim: Limits) -> String {
    let mut layermap = LayerMap::new();
    merge_into(&mut layermap, layermap1.clone(), true, lim);
    let mut w = JsonWriter::mem();
    macro_rules! ns {
        ($e:expr) => {{
            w.nospace = true;
            $e;
        }};
    }
    ns!(w.write_hash());
    ns!(w.write_str("layerCount"));
    ns!(w.write_unsigned(layermap.len() as u64));
    ns!(w.write_str("layers"));
    ns!(w.write_array());
    for (name, l) in &layermap {
        ns!(w.write_hash());
        ns!(w.write_str("layer"));
        ns!(w.write_str(name));
        ns!(w.write_str("count"));
        ns!(w.write_unsigned(l.points + l.lines + l.polygons));
        let geom = if l.points >= l.lines && l.points >= l.polygons {
            "Point"
        } else if l.lines >= l.polygons && l.lines >= l.points {
            "LineString"
        } else {
            "Polygon"
        };
        ns!(w.write_str("geometry"));
        ns!(w.write_str(geom));
        ns!(w.write_str("attributeCount"));
        ns!(w.write_unsigned(l.tilestats.len().min(lim.attributes) as u64));
        ns!(w.write_str("attributes"));
        ns!(w.write_array());
        for (attrs, (a, ts)) in l.tilestats.iter().enumerate() {
            if attrs == elements {
                break;
            }
            ns!(w.write_hash());
            ns!(w.write_str("attribute"));
            ns!(w.write_str(a));
            ns!(w.write_str("count"));
            ns!(w.write_unsigned(ts.samples.len().min(lim.sample_values) as u64));
            let t = type_bits(&ts.samples);
            let ty = match t {
                t if t == 1 << T_DOUBLE => "number",
                t if t == 1 << T_BOOL => "boolean",
                t if t == 1 << T_STRING => "string",
                _ => "mixed",
            };
            ns!(w.write_str("type"));
            ns!(w.write_str(ty));
            ns!(w.write_str("values"));
            ns!(w.write_array());
            let mut vals = 0;
            for v in &ts.samples {
                if vals == elements {
                    break;
                }
                if v.ty == T_DOUBLE || v.ty == T_BOOL {
                    vals += 1;
                    ns!(w.write_json(&v.s));
                } else if truncate16(&v.s).len() == v.s.len() {
                    vals += 1;
                    ns!(w.write_str(&v.s));
                }
            }
            ns!(w.end_array());
            if t & (1 << T_DOUBLE) != 0 {
                ns!(w.write_str("min"));
                ns!(w.write_number(ts.min));
                ns!(w.write_str("max"));
                ns!(w.write_number(ts.max));
            }
            ns!(w.end_hash());
        }
        ns!(w.end_array());
        ns!(w.end_hash());
    }
    ns!(w.end_array());
    ns!(w.end_hash());
    w.into_string()
}

fn sixdig(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// sscanf("%lf,%lf"): the values that it reads.
fn scan_center(s: &str, lon: &mut f64, lat: &mut f64) {
    let (v, n) = strtod_prefix(s);
    if n == 0 {
        return;
    }
    *lon = v;
    if let Some(rest) = s[n..].strip_prefix(',') {
        let (v, n) = strtod_prefix(rest);
        if n > 0 {
            *lat = v;
        }
    }
}

fn usage(argv0: &str) -> ! {
    eprintln!("Usage: {argv0} [options] source.mbtiles ...");
    eprintln!("   or: {argv0} [options] --read-from=inputfile.txt");
    for l in [
        "  Output tileset",
        "         (--output=new.mbtiles | --output-to-directory=directory) [--force]",
        "  Tileset description and attribution",
        "         [--name=...] [--attribution=...] [--description=...]",
        "  Input tilesets",
        "         [--read-from=...]",
        "  Zoom levels",
        "         [--maximum-zoom=...] [--minimum-zoom=...] [--overzoom] [--buffer=...]",
        "  Layer names",
        "         [--layer=...] [--exclude-layer=...] [--rename-layer=...]",
        "  Joining with a CSV file",
        "         [--csv=...] [--if-matched] [--empty-csv-columns-are-null]",
        "  Filtering feature attributes",
        "         [--exclude=...] [--include=...] [--exclude-all]",
        "         [--exclude-all-tile-attributes] [--exclude-all-tile-geometries]",
        "  Filtering features by attributes",
        "         [--feature-filter-file=...] [--feature-filter=...]",
        "  Setting or disabling tile size limits",
        "         [--no-tile-size-limit] [--no-tile-compression] [--no-tile-stats]",
        "         [--tile-stats-attributes-limit=...]",
        "         [--tile-stats-sample-values-limit=...] [--tile-stats-values-limit=...]",
        "  Progress indicator",
        "         [--quiet]",
    ] {
        eprintln!("{l}");
    }
    std::process::exit(EXIT_ARGS)
}

fn threads() -> usize {
    let n = std::env::var("TIPPECANOE_MAX_THREADS").ok().map(|v| atoi(&v).max(1) as usize).unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));
    n.max(1)
}

/// `vtmake join` (argv[0] is the program name). `generator` is the text of the
/// `generator` metadata row.
pub fn main(argv: Vec<String>, generator: &str) {
    let argv0 = argv.first().cloned().unwrap_or_else(|| "tile-join".into());
    let s = |ch, arg, key| Short { ch, arg, key };
    let shorts = [
        s('o', true, "o"),
        s('e', true, "e"),
        s('f', false, "f"),
        s('n', true, "n"),
        s('A', true, "A"),
        s('N', true, "N"),
        s('r', true, "r"),
        s('z', true, "z"),
        s('Z', true, "Z"),
        s('O', false, "O"),
        s('b', true, "b"),
        s('l', true, "l"),
        s('L', true, "L"),
        s('R', true, "R"),
        s('c', true, "c"),
        s('i', false, "i"),
        s('x', true, "x"),
        s('y', true, "y"),
        s('X', false, "X"),
        s('J', true, "J"),
        s('j', true, "j"),
        s('q', false, "q"),
        s('p', true, "p"),
    ];
    let l = |name, arg, key| Long { name, arg, key };
    let longs = [
        l("output", true, "o"),
        l("output-to-directory", true, "e"),
        l("force", false, "f"),
        l("name", true, "n"),
        l("attribution", true, "A"),
        l("description", true, "N"),
        l("read-from", true, "r"),
        l("maximum-zoom", true, "z"),
        l("minimum-zoom", true, "Z"),
        l("overzoom", false, "O"),
        l("buffer", true, "b"),
        l("layer", true, "l"),
        l("exclude-layer", true, "L"),
        l("rename-layer", true, "R"),
        l("csv", true, "c"),
        l("if-matched", false, "i"),
        l("empty-csv-columns-are-null", false, "pe"),
        l("exclude", true, "x"),
        l("include", true, "y"),
        l("exclude-all", false, "X"),
        l("exclude-all-tile-attributes", false, "exclude-all-tile-attributes"),
        l("exclude-all-tile-geometries", false, "exclude-all-tile-geometries"),
        l("feature-filter-file", true, "J"),
        l("feature-filter", true, "j"),
        l("no-tile-size-limit", false, "pk"),
        l("no-tile-compression", false, "pC"),
        l("no-tile-stats", false, "pg"),
        l("tile-stats-attributes-limit", true, "tile-stats-attributes-limit"),
        l("tile-stats-sample-values-limit", true, "tile-stats-sample-values-limit"),
        l("tile-stats-values-limit", true, "tile-stats-values-limit"),
        l("quiet", false, "q"),
        l("prevent", true, "p"),
        l("unidecode-data", true, "unidecode-data"),
    ];
    let u = || usage(&argv0);
    let p = getopt::parse(&argv, &shorts, &longs, &u);

    let mut o = Opts {
        pk: false,
        pc: false,
        pg: false,
        quiet: false,
        minzoom: 0,
        maxzoom: 32,
        renames: BTreeMap::new(),
        exclude_all: false,
        exclude_all_tile_attributes: false,
        exclude: BTreeSet::new(),
        include: BTreeSet::new(),
        keep_layers: BTreeSet::new(),
        remove_layers: BTreeSet::new(),
        ifmatched: false,
        empty_null: false,
        csv_header: Vec::new(),
        csv_rows: BTreeMap::new(),
        lim: Limits { attributes: 1000, sample_values: 1000, values: 100 },
        filter: None,
        exclude_all_tile_geometries: false,
    };
    let (mut out_mbtiles, mut out_dir): (Option<String>, Option<String>) = (None, None);
    let mut force = false;
    let mut buffer = 5;
    let (mut set_name, mut set_description, mut set_attribution) = (String::new(), String::new(), String::new());
    let mut readers: Vec<Reader> = Vec::new();
    let mut filearg = false;
    let mut want_overzoom = false;
    let open_reader = |f: &str, o: &Opts, oz: bool, buffer: i32| Reader::open(f, o.minzoom, o.maxzoom, oz.then_some(buffer)).unwrap_or_else(|e| e.exit());

    for (k, v) in p.opts {
        let v = v.unwrap_or_default();
        match k {
            "o" => out_mbtiles = Some(v),
            "e" => out_dir = Some(v),
            "f" => force = true,
            "O" => want_overzoom = true,
            "b" => buffer = atoi(&v),
            "i" => o.ifmatched = true,
            "A" => set_attribution = v,
            "n" => set_name = v,
            "N" => set_description = v,
            "z" => o.maxzoom = atoi(&v),
            "Z" => o.minzoom = atoi(&v),
            "J" => match vtmake_filter::read_filter(&v) {
                Ok(t) => o.filter = Some(vtmake_filter::Filter::new(&t)),
                Err((msg, code)) => Fatal { msg, code }.exit(),
            },
            "j" => match vtmake_filter::parse_filter(&v) {
                Ok(t) => o.filter = Some(vtmake_filter::Filter::new(&t)),
                Err((msg, code)) => Fatal { msg, code }.exit(),
            },
            "p" => match v.as_str() {
                "k" => o.pk = true,
                "C" => o.pc = true,
                "g" => o.pg = true,
                // Only for CSV joins, so without -c it changes nothing.
                "e" => o.empty_null = true,
                _ => {
                    eprintln!("{argv0}: Unknown option for -p{v}");
                    std::process::exit(EXIT_ARGS)
                }
            },
            "pk" => o.pk = true,
            "pC" => o.pc = true,
            "pg" => o.pg = true,
            "pe" => o.empty_null = true,
            "c" => read_csv(&mut o, &v),
            "x" => drop(o.exclude.insert(v)),
            "X" => o.exclude_all = true,
            "y" => {
                o.exclude_all = true;
                o.include.insert(v);
            }
            "l" => drop(o.keep_layers.insert(v)),
            "L" => drop(o.remove_layers.insert(v)),
            "R" => match v.find(':') {
                Some(c) if c > 0 => {
                    o.renames.entry(v[..c].to_string()).or_insert_with(|| v[c + 1..].to_string());
                }
                _ => {
                    eprintln!("{argv0}: -R requires old:new");
                    std::process::exit(EXIT_ARGS)
                }
            },
            "r" => {
                if let Ok(text) = std::fs::read_to_string(&v) {
                    filearg = true;
                    for line in text.split_terminator('\n') {
                        let r = open_reader(line, &o, want_overzoom, buffer);
                        enqueue(&mut readers, r);
                    }
                }
            }
            "q" => o.quiet = true,
            "tile-stats-attributes-limit" => o.lim.attributes = atoi(&v) as usize,
            "tile-stats-sample-values-limit" => o.lim.sample_values = atoi(&v) as usize,
            "tile-stats-values-limit" => o.lim.values = atoi(&v) as usize,
            // tile-join reads the file, but its filter does not use the data.
            "unidecode-data" => crate::overzoom::read_unidecode(&v),
            "exclude-all-tile-attributes" => o.exclude_all_tile_attributes = true,
            "exclude-all-tile-geometries" => o.exclude_all_tile_geometries = true,
            _ => unreachable!(),
        }
    }

    if p.operands.is_empty() && !filearg {
        usage(&argv0);
    }
    if out_mbtiles.is_none() && out_dir.is_none() {
        eprintln!("{argv0}: must specify -o out.mbtiles or -e directory");
        usage(&argv0);
    }
    if out_mbtiles.is_some() && out_dir.is_some() {
        eprintln!("{argv0}: Options -o and -e cannot be used together");
        usage(&argv0);
    }
    if o.minzoom > o.maxzoom {
        eprintln!("{argv0}: Minimum zoom -Z{} cannot be greater than maxzoom -z{}", o.minzoom, o.maxzoom);
        std::process::exit(EXIT_ARGS);
    }
    if buffer < 0 {
        eprintln!("{argv0}: buffer cannot be less than 0");
        std::process::exit(EXIT_ARGS);
    }

    let mut sink: Box<dyn TileSink> = if let Some(out) = &out_mbtiles {
        match MbtilesWriter::create(std::path::Path::new(out), force) {
            Ok(mut w) => {
                w.set_tile_compression(!o.pc);
                Box::new(w)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                eprintln!("{argv0}: Tileset \"{out}\" already exists. You can use --force if you want to delete the old tileset.");
                eprintln!("{argv0}: {e}");
                std::process::exit(EXIT_EXISTS)
            }
            Err(e) => {
                eprintln!("{argv0}: {out}: {e}");
                std::process::exit(EXIT_OPEN)
            }
        }
    } else {
        let out = out_dir.clone().unwrap();
        match DirWriter::create(std::path::Path::new(&out), force) {
            Ok(w) => Box::new(w),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if e.to_string().contains("metadata.json") {
                    eprintln!("{argv0}: Tileset \"{out}\" already exists. You can use --force if you want to delete the old tileset.");
                    eprintln!("{argv0}: {e}");
                } else {
                    eprintln!("{e}");
                }
                std::process::exit(EXIT_EXISTS)
            }
            Err(e) => {
                eprintln!("{out}: {e}");
                std::process::exit(EXIT_UNLINK)
            }
        }
    };

    if !filearg {
        for f in &p.operands {
            let r = open_reader(f, &o, want_overzoom, buffer);
            enqueue(&mut readers, r);
        }
    }

    let cpus = threads();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(cpus).build().expect("thread pool");
    let mut total = Acc { layermap: LayerMap::new(), bounds: Bounds::new(), outputs: Vec::new() };

    // `decode`: take the tiles in z, x, TMS y order from the queue of readers.
    let mut tasks: Tasks = BTreeMap::new();
    pool.install(|| {
        while !readers.is_empty() && !readers.iter().all(Reader::finished) {
            readers[0].take_current();
            let r0 = &readers[0];
            let cur = (r0.zoom, r0.x, r0.y);
            if cur.0 >= o.minzoom as i64 && cur.0 <= o.maxzoom as i64 {
                tasks.entry((cur.0, cur.1, std::cmp::Reverse(cur.2))).or_insert_with(|| (cur, Vec::new())).1.push(r0.data.clone());
            }
            readers[0].advance();
            let r = readers.remove(0);
            if (readers.is_empty() || (readers[0].zoom, readers[0].x, readers[0].y) != cur) && tasks.len() > 100 * cpus {
                dispatch(&o, std::mem::take(&mut tasks), sink.as_mut(), &mut total);
            }
            enqueue(&mut readers, r);
        }
        dispatch(&o, std::mem::take(&mut tasks), sink.as_mut(), &mut total);
    });

    let mut layermap = total.layermap;
    let b = total.bounds;
    // `struct stats` with the tile bounds.
    let (mut minlat, mut minlon, mut maxlat, mut maxlon) = (b.minlat, b.minlon, b.maxlat, b.maxlon);
    let (mut minlat2, mut minlon2, mut maxlat2, mut maxlon2) = (b.minlat, b.minlon2, b.maxlat, b.maxlon2);
    let (mut midlat, mut midlon) = (0.0f64, 0.0f64);
    let mut st_maxzoom = i32::MIN;
    let mut attribution = String::new();
    let mut description = String::new();
    let mut name = String::new();
    let mut generator_options = String::new();
    let mut attribute_descriptions: BTreeMap<String, String> = BTreeMap::new();
    let mut strategies: Vec<[u64; 9]> = Vec::new();

    for r in &readers {
        let db = r.metadata_db().unwrap_or_else(|e| e.exit());
        let text = |n: &str| meta_value(&db, n).flatten().map(|v| String::from_utf8_lossy(&v).into_owned());
        if let Some(maxz) = meta_int(&db, "maxzoom") {
            let maxz = maxz.min(o.maxzoom);
            if !want_overzoom && st_maxzoom >= 0 && maxz != st_maxzoom {
                eprintln!("Warning: mismatched maxzooms: {maxz} in {} vs previous {st_maxzoom}", r.name);
            }
            st_maxzoom = st_maxzoom.max(maxz);
        }
        if let Some(s) = text("center") {
            scan_center(&s, &mut midlon, &mut midlat);
        }
        if let Some(s) = text("attribution") {
            attribution = s;
        }
        if let Some(s) = text("description") {
            description = s;
        }
        if let Some(s) = text("name") {
            if name.is_empty() {
                name = s;
            } else {
                let proposed = format!("{name} + {s}");
                if proposed.len() < 255 {
                    name = proposed;
                }
            }
        }
        if let Some(Some(s)) = meta_value(&db, "json")
            && let Ok(j) = crate::json::parse(&s)
            && matches!(j, Json::Hash(_))
        {
            handle_vector_layers(j.get("vector_layers"), &mut layermap, &mut attribute_descriptions);
        }
        if let Some(s) = text("generator_options") {
            if generator_options.is_empty() {
                generator_options = s;
            } else {
                generator_options.push_str("; ");
                generator_options.push_str(&s);
            }
        }
        if let Some(Some(s)) = meta_value(&db, "strategies") {
            handle_strategies(&s, &mut strategies);
        }
    }

    if !set_attribution.is_empty() {
        attribution = set_attribution;
    }
    if !set_description.is_empty() {
        description = set_description;
    }
    if !set_name.is_empty() {
        name = set_name;
    }
    if !generator_options.is_empty() {
        generator_options.push_str("; ");
    }
    generator_options.push_str(&format_commandline(&argv));

    // Do not trust the source metadata zooms: use the zooms that were written.
    let (mut minzoom, mut maxzoom) = (i32::MAX, i32::MIN);
    for l in layermap.values() {
        minzoom = minzoom.min(l.minzoom);
        maxzoom = maxzoom.max(l.maxzoom);
    }

    if maxlon < minlon {
        (maxlon, minlon, maxlat, minlat, minlon2, maxlon2, minlat2, maxlat2) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    }
    if maxlon - minlon <= maxlon2 - minlon2 {
        minlon2 = minlon;
        maxlon2 = maxlon;
    }

    let f4 = |a: f64, b: f64, c: f64, d: f64| format!("{},{},{},{}", printf_f(sixdig(a)), printf_f(sixdig(b)), printf_f(sixdig(c)), printf_f(sixdig(d)));
    let vl = vector_layers(&layermap, &attribute_descriptions, o.lim);
    let ts = if !o.pg && o.lim.values > 0 { tilestats(&layermap, o.lim.values, o.lim) } else { String::new() };
    let json = if ts.is_empty() { format!("{{\"vector_layers\":{vl}}}") } else { format!("{{\"vector_layers\":{vl},\"tilestats\":{ts}}}") };

    let mut meta: vtmake_out::Metadata = vec![
        ("name".into(), name),
        ("description".into(), description),
        ("version".into(), "2".into()),
        ("minzoom".into(), minzoom.to_string()),
        ("maxzoom".into(), maxzoom.to_string()),
        ("center".into(), format!("{},{},{maxzoom}", printf_f(sixdig(midlon)), printf_f(sixdig(midlat)))),
        ("bounds".into(), f4(minlon, minlat, maxlon, maxlat)),
        ("antimeridian_adjusted_bounds".into(), f4(minlon2, minlat2, maxlon2, maxlat2)),
        ("type".into(), "overlay".into()),
    ];
    if !attribution.is_empty() {
        meta.push(("attribution".into(), attribution));
    }
    meta.push(("format".into(), "pbf".into()));
    let pmtiles = out_mbtiles.as_deref().is_some_and(pmtiles_has_suffix);
    meta.push(("generator".into(), if pmtiles { "tile-join v2.82.0".into() } else { generator.to_string() }));
    meta.push(("generator_options".into(), generator_options));
    let strat = stringify_strategies(&strategies);
    if !strat.is_empty() {
        meta.push(("strategies".into(), strat));
    }
    meta.push(("json".into(), json));

    if let Err(e) = sink.finish(&meta) {
        eprintln!("{argv0}: {e}");
        std::process::exit(EXIT_CLOSE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lim() -> Limits {
        Limits { attributes: 1000, sample_values: 3, values: 100 }
    }

    #[test]
    fn tilestats_keep_the_smallest_values() {
        let mut ts = BTreeMap::new();
        for v in ["d", "b", "e", "a", "c", "b"] {
            add_to_tilestats(&mut ts, "k", SerialVal { s: v.into(), ty: T_STRING }, 0.0, lim());
        }
        let s: Vec<&str> = ts["k"].samples.iter().map(|v| v.s.as_str()).collect();
        assert_eq!(s, ["a", "b", "c"]);
    }

    #[test]
    fn strategies_text() {
        let mut st = Vec::new();
        handle_strategies(br#"[{},{"dropped_by_rate":3,"tiny_polygons":2},{"truncated_zooms":1}]"#, &mut st);
        handle_strategies(br#"[{"dropped_by_rate":1}]"#, &mut st);
        assert_eq!(stringify_strategies(&st), r#"[{"dropped_by_rate":1},{"dropped_by_rate":3,"tiny_polygons":2},{"truncated_zooms":1}]"#);
        assert_eq!(stringify_strategies(&[[0; 9]]), "");
    }

    #[test]
    fn center_scan() {
        let (mut lon, mut lat) = (0.0, 0.0);
        scan_center("139.5,35.25,14", &mut lon, &mut lat);
        assert_eq!((lon, lat), (139.5, 35.25));
        scan_center("7", &mut lon, &mut lat);
        assert_eq!((lon, lat), (7.0, 35.25));
    }
}
