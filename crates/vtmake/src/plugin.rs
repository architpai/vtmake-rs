//! Shell filters: `-C/--prefilter` and `-c/--postfilter` of `vtmake build`.
//! A port of felt's `plugin.cpp`, with the GeoJSON text of
//! `write_json.cpp:layer_to_geojson` and the readers of
//! `plugin.cpp:parse_feature` and `read_json.cpp:parse_layers`.
//!
//! For each tile at `-Z` or above, and for each detail that the tile tries,
//! tippecanoe starts `sh -c FILTER sh z x y`. A second thread writes the
//! features to the standard input of the filter: one GeoJSON `Feature` for
//! each line, with a newline between features and no newline at the end.
//! The tiling thread reads features from the standard output of the filter
//! until the end. Then it waits for the filter to stop. The exit status of
//! the filter has no effect.
//!
//! - Prefilter: the input is the features of the tile after the clip, the
//!   zoom checks, the `-j` filter and the rate drop marks, in world
//!   coordinates (`"tippecanoe"` has `layer`, `dropped`, `index`,
//!   `sequence` and `extent`). The output features replace the features of
//!   the tile; the other tile steps use them ([`prefilter_detail`]).
//! - Postfilter: the input is the layers of the finished tile, in tile
//!   coordinates; the output layers replace them before the size checks
//!   ([`postfilter`]). The feature-count check uses the count before the
//!   filter.
//!
//! With a filter, the layer statistics of the metadata come from the filter
//! output, not from the input: the postfilter output if there is a
//! postfilter, else the prefilter output ([`layer_stats`]).
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::cli::{Options, Projection};
use crate::drop::Retry;
use crate::store::{Chunk, FeatMeta, Sorted, encode_geom, put_varint};
use crate::tile::{Cand, Ctx};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use vtmake_core::{AttrType, Draw, DrawVec, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use vtmake_mvt::{MvtFeature, MvtLayer, MvtPoint, MvtValue, stringified_to_mvt_value};
use vtmake_parse::geojson::{context, parse_coordinates, stringify_value};
use vtmake_parse::pull::{Kind, NIL, Pull};
use vtmake_tools::jsonw::JsonWriter;

/// tippecanoe exit codes (`errors.hpp`).
const EXIT_CLOSE: i32 = 102;
const EXIT_IMPOSSIBLE: i32 = 106;
const EXIT_JSON: i32 = 107;

const GEOMETRY_NAMES: [&str; 6] = ["Point", "MultiPoint", "LineString", "MultiLineString", "Polygon", "MultiPolygon"];
const MB_GEOMETRY: [GeomType; 6] = [GeomType::Point, GeomType::Point, GeomType::Line, GeomType::Line, GeomType::Polygon, GeomType::Polygon];

/// The prefilter tiles use these options: no prefilter (the filter output
/// does not go to the filter again) and no `-j` (it ran before the filter).
static SUB_OPTS: OnceLock<Options> = OnceLock::new();

/// Set up the filters of the command line. Call once, before the tiling.
pub fn init(o: &Options) {
    if o.prefilter.is_some() {
        let mut sub = o.clone();
        sub.prefilter = None;
        sub.filter = None;
        let _ = SUB_OPTS.set(sub);
    }
}

/// `ctx` makes a tile of the prefilter output.
pub fn is_sub(ctx: &Ctx) -> bool {
    SUB_OPTS.get().is_some_and(|o| std::ptr::eq(o, ctx.opts))
}

/// The attributes of a kept feature of the prefilter output go to the layer
/// statistics (`write_tile`: `add_tilestats` of the full keys).
pub fn kept_stats(layer: &str, attrs: &[(Box<str>, crate::attrs::SerialVal)]) {
    let mut st = TileStats::default();
    for (k, v) in attrs {
        let s = v.s.find('\0').map_or(&*v.s, |i| &v.s[..i]);
        st.value(layer, k, v.ty, s);
    }
    st.finish();
}

/// The command line has a prefilter or a postfilter (`sst->filters`).
pub fn active(o: &Options) -> bool {
    o.prefilter.is_some() || o.postfilter.is_some()
}

// ---------------------------------------------------------------------------
// The layer statistics of the filter output.

#[derive(Default)]
struct AttrAcc {
    /// The smallest distinct values (`add_to_tilestats`).
    samples: BTreeSet<(Box<str>, AttrType)>,
    min: f64,
    max: f64,
    numbers: bool,
}

#[derive(Default)]
struct LayerAcc {
    counts: [u64; 3],
    attrs: BTreeMap<Box<str>, AttrAcc>,
}

static STATS: Mutex<BTreeMap<Box<str>, LayerAcc>> = Mutex::new(BTreeMap::new());

/// The statistics of one filter run. They go to [`STATS`] at the end.
#[derive(Default)]
struct TileStats {
    layers: BTreeMap<Box<str>, LayerAcc>,
}

impl TileStats {
    fn layer(&mut self, name: &str) -> &mut LayerAcc {
        if !self.layers.contains_key(name) {
            self.layers.insert(name.into(), LayerAcc::default());
        }
        self.layers.get_mut(name).unwrap()
    }

    fn count(&mut self, layer: &str, t: GeomType) {
        self.layer(layer).counts[t as usize - 1] += 1;
    }

    /// `add_to_tilestats` for a value that is not null.
    fn value(&mut self, layer: &str, key: &str, ty: AttrType, text: &str) {
        if ty == AttrType::Null {
            return;
        }
        let l = self.layer(layer);
        if !l.attrs.contains_key(key) {
            l.attrs.insert(key.into(), AttrAcc { samples: BTreeSet::new(), min: f64::INFINITY, max: f64::NEG_INFINITY, numbers: false });
        }
        let a = l.attrs.get_mut(key).unwrap();
        if ty == AttrType::Number {
            let d = vtmake_parse::pull::atof(text.as_bytes());
            a.min = a.min.min(d);
            a.max = a.max.max(d);
            a.numbers = true;
        }
        add_sample(a, (text.into(), ty));
    }

    fn finish(self) {
        if self.layers.is_empty() {
            return;
        }
        let mut all = STATS.lock().unwrap();
        for (name, l) in self.layers {
            let t = all.entry(name).or_default();
            for i in 0..3 {
                t.counts[i] += l.counts[i];
            }
            for (k, a) in l.attrs {
                match t.attrs.get_mut(&k) {
                    None => {
                        t.attrs.insert(k, a);
                    }
                    Some(b) => {
                        b.min = b.min.min(a.min);
                        b.max = b.max.max(a.max);
                        b.numbers |= a.numbers;
                        for s in a.samples {
                            add_sample(b, s);
                        }
                    }
                }
            }
        }
    }
}

fn add_sample(a: &mut AttrAcc, s: (Box<str>, AttrType)) {
    if a.samples.insert(s) && a.samples.len() > crate::meta::sample_limit() {
        a.samples.pop_last();
    }
}

/// Put the layer statistics of the filter output in place of the input
/// statistics (`merge_layermaps`: a layer without features is not in the
/// metadata). The bounds stay.
pub fn layer_stats(st: &mut crate::meta::Stats) {
    let all = std::mem::take(&mut *STATS.lock().unwrap());
    st.layers.clear();
    for (name, l) in all {
        if l.counts.iter().sum::<u64>() == 0 {
            continue;
        }
        let mut out = crate::meta::LayerStats { points: l.counts[0], lines: l.counts[1], polygons: l.counts[2], attrs: BTreeMap::new() };
        for (k, a) in l.attrs {
            let mut s = crate::meta::AttrStats { samples: a.samples.into_iter().collect(), ..Default::default() };
            if a.numbers {
                (s.min, s.max) = (a.min, a.max);
            }
            out.attrs.insert(k, s);
        }
        st.layers.insert(name, out);
    }
}

// ---------------------------------------------------------------------------
// The filter process.

/// Start `sh -c FILTER sh z x y` (`setup_filter`). The pipes are close-on-exec
/// in vtmake, so that the filters of other threads do not get them.
fn spawn(cmd: &str, z: u8, x: u32, y: u32) -> Option<Child> {
    let r = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .arg("sh")
        .arg(z.to_string())
        .arg(x.to_string())
        .arg(y.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    match r {
        Ok(c) => Some(c),
        Err(e) => {
            // tippecanoe's child writes this and stops; the tile then has
            // no filter output.
            eprintln!("exec: {}", crate::os_error(&e));
            None
        }
    }
}

/// Run the filter: `write` gives its input from a second thread, `read`
/// takes its output. Then wait for the filter (the exit status has no
/// effect). `what` names the filter in the broken-pipe warning.
fn run<R>(cmd: &str, (z, x, y): (u8, u32, u32), what: &str, warned: &AtomicBool, write: impl FnOnce(&mut dyn FnMut(&[u8]) -> bool) + Send, read: impl FnOnce(&mut dyn Read) -> R) -> R {
    let Some(mut child) = spawn(cmd, z, x, y) else {
        return read(&mut std::io::empty());
    };
    let mut stdin = child.stdin.take().expect("filter stdin");
    let mut stdout = child.stdout.take().expect("filter stdout");
    std::thread::scope(|s| {
        let writer = s.spawn(move || {
            let mut broken = false;
            let mut sink = |b: &[u8]| -> bool {
                if broken {
                    return false;
                }
                if let Err(e) = stdin.write_all(b) {
                    if e.kind() == std::io::ErrorKind::BrokenPipe {
                        broken = true;
                        return false;
                    }
                    eprintln!("fclose output to {what}: {}", crate::os_error(&e));
                    std::process::exit(EXIT_CLOSE);
                }
                true
            };
            write(&mut sink);
            drop(stdin);
            if broken && !warned.swap(true, Ordering::Relaxed) {
                eprintln!("Warning: broken pipe in {what}");
            }
        });
        let r = read(&mut stdout);
        drop(stdout);
        let _ = child.wait();
        writer.join().expect("filter writer");
        r
    })
}

/// Read JSON values from `r` (`json_read` on the filter output) and give
/// each complete value to `f`. A JSON error stops the program.
fn read_values(r: &mut dyn Read, mut f: impl FnMut(&mut Pull, u32)) {
    let mut p = Pull::new();
    let mut buf = vec![0u8; 1 << 16];
    p.set_input(&[], false);
    loop {
        match p.read() {
            vtmake_parse::pull::Read::Node(j) => f(&mut p, j),
            vtmake_parse::pull::Read::More => {
                let n = loop {
                    match r.read(&mut buf) {
                        Ok(n) => break n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break 0,
                    }
                };
                p.set_input(&buf[..n], n == 0);
            }
            vtmake_parse::pull::Read::End => {
                if let Some(e) = p.error {
                    let c = if p.root != NIL { context(&p, p.root) } else { String::new() };
                    eprintln!("Filter output:{}: {e}: {c}", p.line);
                    std::process::exit(EXIT_JSON);
                }
                return;
            }
        }
    }
}

/// `json_context` and exit, for a bad feature of the filter output.
fn bad_feature(p: &Pull, j: u32, what: &str) -> ! {
    eprintln!("Filter output:{}: {what}: {}", p.line, context(p, j));
    std::process::exit(EXIT_JSON)
}

const TWO_63: f64 = 9_223_372_036_854_775_808.0;

/// `(long long) d` on x86-64 (`cvttsd2si`): a value out of range, or NaN,
/// gives `LLONG_MIN`. For example, `"extent": 9223372036854775807` becomes
/// `LLONG_MIN` in tippecanoe.
fn to_i64(d: f64) -> i64 {
    if (-TWO_63..TWO_63).contains(&d) { d as i64 } else { i64::MIN }
}

/// `(unsigned long long) d` as GCC makes it on x86-64.
fn to_u64(d: f64) -> u64 {
    if d.is_nan() || d < TWO_63 { to_i64(d) as u64 } else { (to_i64(d - TWO_63) as u64) ^ (1 << 63) }
}

/// The geometry of a filter feature in world coordinates
/// (`parse_coordinates`, then `fix_polygon` for polygons). Returns the
/// geometry type and the geometry.
fn parse_geometry(p: &Pull, j: u32, geometry: u32, proj: Projection, coordinates_msg: &str) -> (GeomType, DrawVec) {
    let Some(gt) = p.hash_get(geometry, "type") else { bad_feature(p, j, "null geometry (additional not reported)") };
    if p.kind(gt) != Kind::String {
        bad_feature(p, j, "geometry type is not a string");
    }
    let coordinates = match p.hash_get(geometry, "coordinates") {
        Some(c) if p.kind(c) == Kind::Array => c,
        _ => bad_feature(p, j, coordinates_msg),
    };
    let Some(t) = GEOMETRY_NAMES.iter().position(|n| n.as_bytes() == p.text(gt)) else {
        bad_feature(p, j, &format!("Can't handle geometry type {}", String::from_utf8_lossy(p.text(gt))))
    };
    let mut pos = Vec::new();
    parse_coordinates(p, t, coordinates, &mut pos, VT_MOVETO, "Filter output", p.line, j);
    let dv: DrawVec = pos
        .iter()
        .map(|q| {
            if q.op == VT_CLOSEPATH {
                Draw::new(VT_CLOSEPATH, 0, 0)
            } else {
                let (wx, wy) = match proj {
                    Projection::Epsg4326 => vtmake_geom::lonlat_to_world(q.lon, q.lat),
                    Projection::Epsg3857 => vtmake_geom::epsg3857_to_world(q.lon, q.lat),
                };
                Draw::new(q.op, wx, wy)
            }
        })
        .collect();
    let t = MB_GEOMETRY[t];
    if t == GeomType::Polygon {
        let mut out = Vec::new();
        vtmake_geom::fix_polygon(&dv, &mut out);
        return (t, out);
    }
    (t, dv)
}

/// The `"properties"` of a filter feature: (key, type, text), without nulls.
fn parse_properties(p: &Pull, j: u32, properties: u32) -> Vec<(Box<str>, AttrType, Box<str>)> {
    let mut out = Vec::new();
    if p.kind(properties) == Kind::Hash {
        for e in p.items(properties).chunks(2) {
            if e[1] == NIL {
                continue;
            }
            let (ty, text) = stringify_value(p, e[1], "Filter output", p.line, j);
            if ty != AttrType::Null {
                out.push((vtmake_parse::geojson::utf8(p.text(e[0]), "Filter output", p.line), ty, text));
            }
        }
    }
    out
}

/// The feature ID of a filter feature, if it has a number ID.
fn parse_id(p: &Pull, j: u32) -> Option<u64> {
    let id = p.hash_get(j, "id")?;
    if p.kind(id) != Kind::Number {
        return None;
    }
    let u = p.large_unsigned(id);
    Some(if u > 0 { u } else { to_u64(p.number(id)) })
}

// ---------------------------------------------------------------------------
// The GeoJSON text (`layer_to_geojson`).

#[derive(Clone, Copy)]
struct LonLat {
    op: u8,
    lon: f64,
    lat: f64,
    x: i64,
    y: i64,
}

fn unproject(proj: Projection, wx: i64, wy: i64) -> (f64, f64) {
    match proj {
        Projection::Epsg4326 => vtmake_geom::tile2lonlat(wx, wy, 32),
        Projection::Epsg3857 => {
            // `tiletoepsg3857` at zoom 32.
            let ox = (wx.wrapping_sub(1 << 31)) as f64 * std::f64::consts::PI * 6378137.0 / (1i64 << 31) as f64;
            let oy = ((1i64 << 32) - 1 - wy - (1i64 << 31)) as f64 * std::f64::consts::PI * 6378137.0 / (1i64 << 31) as f64;
            (ox, oy)
        }
    }
}

/// The `"tippecanoe"` fields of a feature.
struct Tip<'a> {
    layer: &'a str,
    dropped: bool,
    index: u64,
    sequence: i64,
    extent: i64,
}

static WARNED_RING: AtomicBool = AtomicBool::new(false);
static WARNED_INNER: AtomicBool = AtomicBool::new(false);

fn write_coords(w: &mut JsonWriter, ll: &LonLat) {
    w.write_float(ll.lon);
    w.write_float(ll.lat);
}

/// `layer_to_geojson` for one feature with `name` and `write_dropped`, and
/// `complain`: a ring without a closepath stops the program.
fn write_feature(w: &mut JsonWriter, tip: &Tip, id: Option<u64>, t: GeomType, props: &[(&str, MvtValue)], ops: &[LonLat]) {
    w.write_hash();
    w.write_str("type");
    w.write_str("Feature");
    if let Some(id) = id {
        w.write_str("id");
        w.write_unsigned(id);
    }
    w.write_str("tippecanoe");
    w.write_hash();
    w.write_str("layer");
    w.write_str(tip.layer);
    w.write_str("dropped");
    w.write_bool(tip.dropped);
    if tip.index != 0 {
        w.write_str("index");
        w.write_unsigned(tip.index);
    }
    if tip.sequence != 0 {
        w.write_str("sequence");
        w.write_signed(tip.sequence);
    }
    if tip.extent != 0 {
        w.write_str("extent");
        w.write_signed(tip.extent);
    }
    w.end_hash();

    w.write_str("properties");
    w.write_hash();
    for (k, v) in props {
        // The key is a C string.
        let k = k.as_bytes();
        w.write_string(&k[..k.iter().position(|&c| c == 0).unwrap_or(k.len())]);
        match v {
            MvtValue::String(s) => w.write_str(s),
            MvtValue::Int(i) | MvtValue::Sint(i) => w.write_signed(*i),
            MvtValue::Double(d) => w.write_number(*d),
            MvtValue::Float(f) => w.write_number(*f as f64),
            MvtValue::Uint(u) => w.write_unsigned(*u),
            MvtValue::Bool(b) => w.write_bool(*b),
        }
    }
    w.end_hash();

    w.write_str("geometry");
    w.write_hash();
    match t {
        GeomType::Point => {
            w.write_str("type");
            if ops.len() == 1 {
                w.write_str("Point");
                w.write_str("coordinates");
                w.write_array();
                write_coords(w, &ops[0]);
                w.end_array();
            } else {
                w.write_str("MultiPoint");
                w.write_str("coordinates");
                w.write_array();
                for o in ops {
                    w.write_array();
                    write_coords(w, o);
                    w.end_array();
                }
                w.end_array();
            }
        }
        GeomType::Line => {
            w.write_str("type");
            if ops.iter().filter(|o| o.op == VT_MOVETO).count() < 2 {
                w.write_str("LineString");
                w.write_str("coordinates");
                w.write_array();
                for o in ops {
                    w.write_array();
                    write_coords(w, o);
                    w.end_array();
                }
                w.end_array();
            } else {
                w.write_str("MultiLineString");
                w.write_str("coordinates");
                w.write_array();
                w.write_array();
                let mut sstate = 0;
                for o in ops {
                    if o.op == VT_MOVETO && sstate != 0 {
                        w.end_array();
                        w.write_array();
                    }
                    if o.op == VT_MOVETO {
                        sstate = 1;
                    }
                    w.write_array();
                    write_coords(w, o);
                    w.end_array();
                }
                w.end_array();
                w.end_array();
            }
        }
        GeomType::Polygon => write_polygon(w, ops),
    }
    w.end_hash();
    w.end_hash();
}

fn write_polygon(w: &mut JsonWriter, ops: &[LonLat]) {
    let mut rings: Vec<Vec<LonLat>> = Vec::new();
    for (i, o) in ops.iter().enumerate() {
        if o.op == VT_MOVETO {
            rings.push(Vec::new());
        }
        if let Some(r) = rings.last_mut() {
            if o.op == VT_CLOSEPATH {
                let first = r[0];
                r.push(first);
            } else {
                r.push(*o);
            }
        }
        if (i + 1 >= ops.len() || ops[i + 1].op == VT_MOVETO) && o.op != VT_CLOSEPATH && !WARNED_RING.swap(true, Ordering::Relaxed) {
            eprintln!("Ring does not end with closepath (ends with {})", o.op);
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }
    let mut outer = 0;
    let mut areas = Vec::with_capacity(rings.len());
    for (i, r) in rings.iter().enumerate() {
        let mut area = 0.0f64;
        for k in 0..r.len() {
            if r[k].op != VT_CLOSEPATH {
                let n = &r[(k + 1) % r.len()];
                area += r[k].x as f64 * n.y as f64;
                area -= r[k].y as f64 * n.x as f64;
            }
        }
        area /= 2.0;
        areas.push(area);
        if area >= 0.0 || i == 0 {
            outer += 1;
        }
    }
    w.write_str("type");
    if outer > 1 {
        w.write_str("MultiPolygon");
        w.write_str("coordinates");
        w.write_array();
        w.write_array();
        w.write_array();
    } else {
        w.write_str("Polygon");
        w.write_str("coordinates");
        w.write_array();
        w.write_array();
    }
    let mut sstate = 0;
    for (i, r) in rings.iter().enumerate() {
        if i == 0 && areas[i] < 0.0 && !WARNED_INNER.swap(true, Ordering::Relaxed) {
            eprintln!("Polygon begins with an inner ring");
            std::process::exit(EXIT_IMPOSSIBLE);
        }
        if areas[i] >= 0.0 {
            if sstate != 0 {
                w.end_array();
                w.end_array();
                w.write_array();
                w.write_array();
            }
            sstate = 1;
        }
        if sstate == 2 {
            w.end_array();
            w.write_array();
        }
        for q in r {
            w.write_array();
            write_coords(w, q);
            w.end_array();
        }
        sstate = 2;
    }
    w.end_array();
    w.end_array();
    if outer > 1 {
        w.end_array();
    }
}

/// Give the text to the filter when it is large. False: the pipe is closed.
fn flush(w: &mut JsonWriter, sink: &mut dyn FnMut(&[u8]) -> bool, min: usize) -> bool {
    if w.buf.len() < min {
        return true;
    }
    let ok = sink(&w.buf);
    w.buf.clear();
    ok
}

// ---------------------------------------------------------------------------
// The prefilter.

static WARNED_PREFILTER: AtomicBool = AtomicBool::new(false);
static WARNED_POSTFILTER: AtomicBool = AtomicBool::new(false);

/// `close_poly`, with its message for a ring that is not closed.
fn close_poly_warn(g: &mut DrawVec) {
    let mut i = 0;
    while i < g.len() {
        if g[i].op == VT_MOVETO {
            let j = vtmake_geom::polygon::part_end(g, i);
            if j - 1 > i && (g[j - 1].x != g[i].x || g[j - 1].y != g[i].y) {
                eprintln!("Internal error: polygon not closed");
            }
            i = j;
        } else {
            i += 1;
        }
    }
    vtmake_geom::close_poly(g);
}

/// The area of a feature (`serial.cpp:serialize_feature`, `extent`): only
/// with `-an`, `-aN`, `--order-by-size` or `-zg`, else 0.
fn feature_extent(ctx: &Ctx, pos: u32) -> i64 {
    let o = ctx.opts;
    if !(o.additional[b'n' as usize] || o.additional[b'N' as usize] || o.order_by_size || o.guess_maxzoom) {
        return 0;
    }
    let (c, m) = ctx.meta(pos);
    let mut g = Vec::new();
    c.decode_geom(m, &mut g);
    let mut extent = 0.0;
    if m.geom_type == GeomType::Polygon {
        let mut i = 0;
        while i < g.len() {
            if g[i].op == VT_MOVETO {
                let j = vtmake_geom::polygon::part_end(&g, i);
                extent += vtmake_geom::polygon::get_area(&g, i, j);
                i = j;
            } else {
                i += 1;
            }
        }
    } else if m.geom_type == GeomType::Line {
        let mut dist = 0.0;
        for w in g.windows(2) {
            if w[1].op == VT_LINETO {
                let dx = (w[1].x - w[0].x) as f64;
                let dy = (w[1].y - w[0].y) as f64;
                dist += (dx * dx + dy * dy).sqrt();
            }
        }
        extent = std::f64::consts::PI * (dist / 2.0) * (dist / 2.0);
    }
    if extent.is_nan() { i64::MAX } else { extent as i64 }
}

/// tippecanoe keeps the sort index of a feature (`sf.index`) only with these
/// options (`serial.cpp:serialize_feature`); else the index is 0.
fn keeps_index(o: &Options) -> bool {
    let a = |c: u8| o.additional[c as usize];
    a(b's') || a(b'S') || a(b'C') || a(b'g') || a(b'n') || a(b'N') || a(b'd') || a(b'D') || o.prevent[b'd' as usize] || a(b'G') || a(b'P')
        || o.gamma != 0.0 || o.retain_points_multiplier > 1 || o.preserve_multiplier_density_threshold > 0 || o.cluster_distance != 0
}

/// Write the prefilter input: the features of the tile (`run_prefilter`).
/// The rate drop marks are those of `next_feature`.
fn write_prefilter_input(ctx: &Ctx, z: u8, x: u32, y: u32, cands: &[Cand], rel: &[Draw], sink: &mut dyn FnMut(&[u8]) -> bool) {
    let zu = z as u32;
    let mut ta = crate::attrs::TileState::new(ctx, zu);
    let mut rate: BTreeMap<&str, vtmake_geom::LayerRateDrop> = BTreeMap::new();
    let rpm = ctx.opts.retain_points_multiplier;
    let extra_zooms = if rpm > 1 { (rpm as f64).ln() / ctx.opts.droprate.ln() } else { 0.0 };
    let mut multiplier_state = BTreeMap::<&str, u64>::new();
    let mut multiplier_previous = 0u64;
    let (ox, oy) = vtmake_geom::tile::tile_origin(zu, x, y);
    let mut w = JsonWriter::mem();
    let mut g: DrawVec = Vec::new();
    let mut ops: Vec<LonLat> = Vec::new();
    let mut props: Vec<(&str, MvtValue)> = Vec::new();
    let index = keeps_index(ctx.opts);
    for c in cands {
        if let Some(ta) = ta.as_mut()
            && !ta.admit(c.pos)
        {
            continue;
        }
        let (chunk, m) = ctx.meta(c.pos);
        let lname = ctx.layer_name(c.pos);
        let mut kept = true;
        if m.minzoom < 0 {
            kept = rate.entry(lname).or_default().mark(zu, m.drop_minzoom as i32, m.index);
            if rpm > 1 {
                let state = multiplier_state.entry(lname).or_default();
                if kept {
                    *state = 0;
                } else if zu as f64 + extra_zooms >= m.drop_minzoom as f64 - ((m.index >> 2).reverse_bits() as f64 / 2f64.powi(64)) && *state + 1 < rpm as u64 {
                    *state += 1;
                    kept = true;
                }
                let threshold = ctx.opts.preserve_multiplier_density_threshold;
                if !kept && threshold > 0 {
                    let distance = (1u64 << (32 - zu)) / threshold as u64;
                    if m.index.wrapping_sub(multiplier_previous) > distance.wrapping_mul(distance) {
                        kept = true;
                    }
                }
                if kept {
                    multiplier_previous = m.index;
                }
            }
        }
        let excluded = ta.as_ref().map_or(&[][..], |ta| ta.excluded(c.pos));
        props.clear();
        for (k, v) in chunk.attrs(m) {
            let v = &chunk.vals[v as usize];
            let key = &chunk.keys[k as usize];
            if v.ty == AttrType::Null || excluded.contains(key) {
                continue;
            }
            if let Some(mv) = stringified_to_mvt_value(v.ty, chunk.val_text(v)) {
                props.push((key, mv));
            }
        }
        g.clear();
        g.extend_from_slice(&rel[c.start as usize..(c.start + c.len) as usize]);
        if m.geom_type == GeomType::Polygon {
            close_poly_warn(&mut g);
        }
        ops.clear();
        ops.extend(g.iter().map(|d| {
            if d.op == VT_MOVETO || d.op == VT_LINETO {
                let (px, py) = (d.x + ox, d.y + oy);
                let (lon, lat) = unproject(ctx.opts.projection, px, py);
                LonLat { op: d.op, lon, lat, x: px, y: py }
            } else {
                LonLat { op: d.op, lon: 0.0, lat: 0.0, x: 0, y: 0 }
            }
        }));
        let s = ctx.order[c.pos as usize];
        let sequence = chunk.felt_seqs.get(s.local as usize).map_or(0, |&q| q as i64);
        let tip = Tip { layer: lname, dropped: !kept, index: if index { m.index } else { 0 }, sequence, extent: feature_extent(ctx, c.pos) };
        write_feature(&mut w, &tip, chunk.id(m), m.geom_type, &props, &ops);
        if !flush(&mut w, sink, 1 << 16) {
            return;
        }
    }
    flush(&mut w, sink, 0);
}

/// A feature of the prefilter output (`parse_feature`).
struct Parsed {
    t: GeomType,
    /// Tile-relative world coordinates.
    geom: DrawVec,
    layer: Box<str>,
    index: u64,
    seq: i64,
    extent: i64,
    dropped: bool,
    id: Option<u64>,
    attrs: Vec<(Box<str>, AttrType, Box<str>)>,
}

/// Read the prefilter output. The features count in the layer statistics
/// only if there is no postfilter.
fn read_prefilter_output(r: &mut dyn Read, ctx: &Ctx, z: u8, x: u32, y: u32, stats: &mut TileStats) -> Vec<Parsed> {
    let count = ctx.opts.postfilter.is_none();
    let (ox, oy) = vtmake_geom::tile::tile_origin(z as u32, x, y);
    let shift = ctx.opts.geometry_scale;
    let scale = (1u64 << shift) as f64;
    let mut out = Vec::new();
    read_values(r, |p, j| {
        let Some(ty) = p.hash_get(j, "type") else { return };
        if p.kind(ty) != Kind::String || p.text(ty) != b"Feature" {
            return;
        }
        let Some(geometry) = p.hash_get(j, "geometry") else { bad_feature(p, j, "filtered feature with no geometry") };
        let properties = match p.hash_get(j, "properties") {
            Some(pr) if matches!(p.kind(pr), Kind::Hash | Kind::Null) => pr,
            _ => bad_feature(p, j, "feature without properties hash"),
        };
        let (t, mut geom) = parse_geometry(p, j, geometry, ctx.opts.projection, "feature without coordinates array");
        for d in &mut geom {
            d.x = ((d.x as f64 / scale).round() * scale - ox as f64) as i64;
            d.y = ((d.y as f64 / scale).round() * scale - oy as f64) as i64;
        }
        if !geom.is_empty() {
            let mut f = Parsed { t, geom, layer: "unknown".into(), index: 0, seq: 0, extent: 0, dropped: true, id: None, attrs: Vec::new() };
            if let Some(tip) = p.hash_get(j, "tippecanoe") {
                if let Some(l) = p.hash_get(tip, "layer")
                    && p.kind(l) == Kind::String
                {
                    f.layer = vtmake_parse::geojson::utf8(p.text(l), "Filter output", p.line);
                }
                let num = |key: &str| p.hash_get(tip, key).filter(|&n| p.kind(n) == Kind::Number).map(|n| p.number(n));
                if let Some(v) = num("index") {
                    f.index = to_u64(v);
                }
                if let Some(v) = num("sequence") {
                    f.seq = to_i64(v);
                }
                if let Some(v) = num("extent") {
                    f.extent = to_i64(v);
                }
                f.dropped = p.hash_get(tip, "dropped").is_some_and(|d| p.kind(d) == Kind::True);
            }
            f.id = parse_id(p, j);
            if count {
                stats.count(&f.layer, t);
            }
            f.attrs = parse_properties(p, j, properties);
            if count {
                for (k, ty, text) in &f.attrs {
                    stats.value(&f.layer, k, *ty, text);
                }
            }
            out.push(f);
        }
        p.free(j);
    });
    out
}

/// The features of one prefilter run, as a feature store for the tile steps.
struct Store {
    chunk: Chunk,
    order: Vec<Sorted>,
    extents: Vec<u64>,
    cands: Vec<Cand>,
    rel: DrawVec,
}

fn store(parsed: Vec<Parsed>, ox: i64, oy: i64, extents: bool) -> Store {
    let mut c = Chunk::default();
    let mut keys: HashMap<Box<str>, u32> = HashMap::new();
    let mut layers: HashMap<Box<str>, u16> = HashMap::new();
    let mut s = Store { chunk: Chunk::default(), order: Vec::new(), extents: Vec::new(), cands: Vec::new(), rel: Vec::new() };
    let mut world = Vec::new();
    for (i, f) in parsed.into_iter().enumerate() {
        let layer = *layers.entry(f.layer.clone()).or_insert_with(|| {
            c.layers.push(f.layer.clone());
            (c.layers.len() - 1) as u16
        });
        let mut bbox = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
        world.clear();
        world.extend(f.geom.iter().map(|d| {
            if d.op == VT_MOVETO || d.op == VT_LINETO {
                bbox = [bbox[0].min(d.x + ox), bbox[1].min(d.y + oy), bbox[2].max(d.x + ox), bbox[3].max(d.y + oy)];
                Draw { x: d.x + ox, y: d.y + oy, ..*d }
            } else {
                *d
            }
        }));
        let geom_off = c.geom.len() as u32;
        encode_geom(&world, &mut c.geom);
        let attr_off = c.attrs.len() as u32;
        for (k, ty, text) in &f.attrs {
            let ki = *keys.entry(k.clone()).or_insert_with(|| c.push_key(k.clone()));
            let vi = c.push_val(*ty, text.as_bytes());
            put_varint(&mut c.attrs, ki as u64);
            put_varint(&mut c.attrs, vi as u64);
        }
        let id = match f.id {
            Some(v) => {
                c.ids.push(v);
                (c.ids.len() - 1) as u32
            }
            None => u32::MAX,
        };
        c.metas.push(FeatMeta {
            index: f.index,
            bbox,
            id,
            geom_off,
            attr_off,
            ndraws: world.len() as u32,
            nattrs: f.attrs.len() as u16,
            layer,
            geom_type: f.t,
            // No `tippecanoe.minzoom`: the rate drop mark decides. A kept
            // feature passes the rate at every zoom.
            minzoom: -1,
            maxzoom: -1,
            drop_minzoom: if f.dropped { i8::MAX } else { 0 },
        });
        // `-pi` and the multiplier sort by `sequence`.
        c.seqs.push((f.seq as u64) ^ (1 << 63));
        if extents {
            s.extents.push(f.extent.max(0) as u64);
        }
        s.order.push(Sorted { chunk: 0, local: i as u32 });
        s.cands.push(Cand { gap: 0, pos: i as u32, start: s.rel.len() as u32, len: f.geom.len() as u32 });
        s.rel.extend_from_slice(&f.geom);
    }
    s.chunk = c;
    s
}

/// One detail of a tile with a prefilter: run the filter on the
/// candidates of the tile, then make the tile from the filter output with
/// the usual steps (`write_tile_detail` on a feature store of the output).
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn prefilter_detail<const P: bool>(ctx: &Ctx, z: u8, x: u32, y: u32, cands: &[Cand], rel: &[Draw], d: u32, retry: &mut Retry) -> u8 {
    let cmd = ctx.opts.prefilter.as_deref().expect("prefilter");
    let mut stats = TileStats::default();
    let parsed = run(
        cmd,
        (z, x, y),
        "prefilter",
        &WARNED_PREFILTER,
        |sink| write_prefilter_input(ctx, z, x, y, cands, rel, sink),
        |r| read_prefilter_output(r, ctx, z, x, y, &mut stats),
    );
    stats.finish();
    let (ox, oy) = vtmake_geom::tile::tile_origin(z as u32, x, y);
    let s = store(parsed, ox, oy, !ctx.extents.is_empty());
    let chunks = [s.chunk];
    let default_layers = ["unknown"];
    let tmpdir = &ctx.opts.tmpdir;
    let sub = Ctx {
        selection: ctx.selection,
        skip_tiles: Default::default(),
        trial_blocked: Default::default(),
        drop_state: Mutex::new(*ctx.drop_state.lock().unwrap()),
        staged: Mutex::new(crate::tile::Spool::new(tmpdir)),
        still_dropping: AtomicBool::new(false),
        gaps: Vec::new(),
        extents: s.extents,
        opts: SUB_OPTS.get().expect("plugin::init"),
        pending: ctx.pending.as_ref().map(|_| Mutex::new(crate::tile::Spool::new(tmpdir))),
        failed: AtomicUsize::new(usize::MAX),
        incomplete: AtomicBool::new(false),
        chunks: &chunks,
        order: &s.order,
        minzoom: ctx.minzoom,
        maxzoom: AtomicUsize::new(ctx.maxzoom.load(Ordering::Relaxed)),
        buffer: ctx.buffer,
        default_layers: &default_layers,
        gzip: ctx.gzip,
        out: ctx.out.clone(),
        max_tiled: AtomicUsize::new(0),
        starts_at_minzoom: ctx.starts_at_minzoom,
        first_zoom: ctx.first_zoom,
        tiles: AtomicU64::new(0),
        parallel_bytes: AtomicUsize::new(0),
        parallel_tasks: AtomicUsize::new(0),
        parallel_limit: 0,
        parallel_budget: 0,
        center: Mutex::new((0, u64::MAX)),
        strategies: (0..ctx.strategies.len()).map(|_| Default::default()).collect(),
        geo: crate::tile::Geo {
            opts: ctx.geo.opts,
            extra_detail: ctx.geo.extra_detail,
            simplification: ctx.geo.simplification,
            maxzoom_simplification: ctx.geo.maxzoom_simplification,
            tiny_polygon_size: ctx.geo.tiny_polygon_size,
            check_polygons: ctx.geo.check_polygons,
            label_points: ctx.geo.label_points,
            nodes: ctx.geo.nodes.clone(),
            plain: ctx.geo.plain,
        },
    };
    let nodes = vec![(0u32, 0u32); s.cands.len()];
    let edge = (&[][..], &nodes[..]);
    let code = if ctx.opts.retain_points_multiplier > 1 {
        crate::tile::write_tile_detail::<P, true>(&sub, z, x, y, &s.cands, &s.rel, edge, d, retry)
    } else {
        crate::tile::write_tile_detail::<P, false>(&sub, z, x, y, &s.cands, &s.rel, edge, d, retry)
    };
    // The results of the tile go to the tiling state.
    for (a, b) in ctx.strategies.iter().zip(&sub.strategies) {
        for (i, (a, b)) in a.iter().zip(b).enumerate() {
            let v = b.load(Ordering::Relaxed);
            if v != 0 {
                if i == 4 || i == 5 { a.fetch_max(v, Ordering::Relaxed) } else { a.fetch_add(v, Ordering::Relaxed) };
            }
        }
    }
    let failed = sub.failed.load(Ordering::Relaxed);
    if failed != usize::MAX {
        ctx.failed.fetch_min(failed, Ordering::Relaxed);
    }
    if sub.incomplete.load(Ordering::Relaxed) {
        ctx.incomplete.store(true, Ordering::Relaxed);
    }
    let c = *sub.center.lock().unwrap();
    if c.1 != u64::MAX {
        let mut r = ctx.center.lock().unwrap();
        if c.0 > r.0 || (c.0 == r.0 && c.1 < r.1) {
            *r = c;
        }
    }
    ctx.tiles.fetch_add(sub.tiles.load(Ordering::Relaxed), Ordering::Relaxed);
    let mut staged = sub.staged.lock().unwrap();
    if staged.count > 0 {
        let mut reader = staged.reader();
        let mut to = ctx.staged.lock().unwrap();
        for _ in 0..staged.count {
            to.push_tile(crate::tile::Spool::read_tile(&mut reader));
        }
    }
    code
}

// ---------------------------------------------------------------------------
// The postfilter.

thread_local! {
    static CLEANER: RefCell<vtmake_wagyu::Cleaner> = RefCell::new(vtmake_wagyu::Cleaner::new());
}

/// Layer names of the postfilter output, for the layer table of the tile
/// (each distinct name is kept once).
static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

fn intern(name: &str) -> &'static str {
    let mut names = NAMES.lock().unwrap();
    let set = names.get_or_insert_with(HashSet::new);
    if let Some(&n) = set.get(name) {
        return n;
    }
    let n: &'static str = Box::leak(name.into());
    set.insert(n);
    n
}

/// Write the postfilter input: the layers of the tile (`run_writer`).
fn write_postfilter_input(layers: &BTreeMap<&str, MvtLayer>, z: u8, x: u32, y: u32, proj: Projection, sink: &mut dyn FnMut(&[u8]) -> bool) {
    let mut w = JsonWriter::mem();
    let mut ops: Vec<LonLat> = Vec::new();
    let wscale = 1i64.wrapping_shl(32 - z as u32);
    for layer in layers.values() {
        let tip = Tip { layer: &layer.name, dropped: false, index: 0, sequence: 0, extent: 0 };
        for f in &layer.features {
            let props: Vec<(&str, MvtValue)> = f.tags.chunks(2).filter(|t| t.len() == 2).map(|t| (&*layer.keys[t[0] as usize], layer.values[t[1] as usize].clone())).collect();
            ops.clear();
            ops.extend(f.geometry.iter().map(|g| {
                if g.op == VT_MOVETO || g.op == VT_LINETO {
                    let ext = layer.extent as i64;
                    let wx = wscale.wrapping_mul(x as i64).wrapping_add((wscale / ext).wrapping_mul(g.x));
                    let wy = wscale.wrapping_mul(y as i64).wrapping_add((wscale / ext).wrapping_mul(g.y));
                    let (lon, lat) = unproject(proj, wx, wy);
                    LonLat { op: g.op, lon, lat, x: g.x, y: g.y }
                } else {
                    LonLat { op: g.op, lon: 0.0, lat: 0.0, x: 0, y: 0 }
                }
            }));
            let t = match f.geom_type {
                1 => GeomType::Point,
                2 => GeomType::Line,
                _ => GeomType::Polygon,
            };
            write_feature(&mut w, &tip, f.id, t, &props, &ops);
            if !flush(&mut w, sink, 1 << 16) {
                return;
            }
        }
    }
    flush(&mut w, sink, 0);
}

/// Read the postfilter output (`parse_layers`): the layers of the tile, in
/// name order. A layer can be empty.
fn read_postfilter_output(r: &mut dyn Read, z: u8, x: u32, y: u32, extent: u32, proj: Projection) -> BTreeMap<String, MvtLayer> {
    let mut ret: BTreeMap<String, MvtLayer> = BTreeMap::new();
    let scale = 1i64 << (32 - z as u32);
    read_values(r, |p, j| {
        let Some(ty) = p.hash_get(j, "type") else { return };
        if p.kind(ty) != Kind::String || p.text(ty) != b"Feature" {
            return;
        }
        let properties = match p.hash_get(j, "properties") {
            Some(pr) if matches!(p.kind(pr), Kind::Hash | Kind::Null) => pr,
            _ => bad_feature(p, j, "feature without properties hash"),
        };
        let mut layername: Box<str> = "unknown".into();
        if let Some(tip) = p.hash_get(j, "tippecanoe")
            && let Some(l) = p.hash_get(tip, "layer")
            && p.kind(l) == Kind::String
        {
            layername = vtmake_parse::geojson::utf8(p.text(l), "Filter output", p.line);
        }
        if !ret.contains_key(&*layername) {
            let mut l = MvtLayer::new(&*layername);
            l.extent = extent;
            ret.insert(layername.to_string(), l);
        }
        let Some(geometry) = p.hash_get(j, "geometry") else { bad_feature(p, j, "filtered feature with no geometry") };
        let (t, mut dv) = parse_geometry(p, j, geometry, proj, "geometry without coordinates array");
        for d in &mut dv {
            d.x -= scale.wrapping_mul(x as i64);
            d.y -= scale.wrapping_mul(y as i64);
            d.x = (d.x as f64 * (extent as f64 / scale as f64)).round() as i64;
            d.y = (d.y as f64 * (extent as f64 / scale as f64)).round() as i64;
        }
        if t == GeomType::Polygon {
            let mut out = Vec::new();
            vtmake_geom::remove_noop(&mut dv, GeomType::Polygon, 0);
            if CLEANER.with_borrow_mut(|c| c.clean_clipped(&dv, &mut out, z as u32, 256)).is_err() {
                eprintln!("Internal error: Polygon cleaning failed. Log in /tmp/wagyu.log");
                std::process::exit(EXIT_IMPOSSIBLE);
            }
            dv = out;
            if dv.len() < 3 {
                dv.clear();
            }
        }
        vtmake_geom::remove_noop(&mut dv, t, 0);
        if t == GeomType::Polygon {
            close_poly_warn(&mut dv);
        }
        if !dv.is_empty() {
            let layer = ret.get_mut(&*layername).unwrap();
            let mut f = MvtFeature { geom_type: t as u8, id: parse_id(p, j), ..Default::default() };
            for (k, ty, text) in parse_properties(p, j, properties) {
                match ty {
                    AttrType::String => layer.tag_str(&mut f, &k, &text),
                    _ => {
                        if let Some(v) = stringified_to_mvt_value(ty, &text) {
                            layer.tag(&mut f, &k, &v);
                        }
                    }
                }
            }
            f.geometry = dv.iter().map(|d| MvtPoint { op: d.op, x: d.x, y: d.y }).collect();
            layer.features.push(f);
        }
        p.free(j);
    });
    ret
}

/// The text of an MVT value for the statistics (`mvt_value_to_serial_val`).
fn value_stat(v: &MvtValue) -> (AttrType, String) {
    match v {
        MvtValue::String(s) => (AttrType::String, s.to_string()),
        MvtValue::Float(f) => (AttrType::Number, vtmake_parse::dtoa::dtoa_milo(*f as f64)),
        MvtValue::Double(d) => (AttrType::Number, vtmake_parse::dtoa::dtoa_milo(*d)),
        MvtValue::Int(i) | MvtValue::Sint(i) => (AttrType::Number, i.to_string()),
        MvtValue::Uint(u) => (AttrType::Number, u.to_string()),
        MvtValue::Bool(b) => (AttrType::Bool, b.to_string()),
    }
}

/// A tile that the postfilter emptied is a candidate for the metadata center
/// (tippecanoe uses the vertex count before the filter).
#[cold]
pub(crate) fn center(ctx: &Ctx, z: u8, x: u32, y: u32, vertices: u64) {
    if vertices > 0 && z as usize == ctx.maxzoom.load(Ordering::Relaxed) {
        let key = ((x as u64) << 32) | y as u64;
        let mut c = ctx.center.lock().unwrap();
        if vertices > c.0 || (vertices == c.0 && key < c.1) {
            *c = (vertices, key);
        }
    }
}

/// The steps after the layers of a tile try are made: the postfilter (see
/// [`postfilter`], `Some` if it runs) and the end of the try for `-u`.
#[cold]
#[inline(never)]
pub(crate) fn after_layers(ctx: &Ctx, z: u8, x: u32, y: u32, layers: &mut BTreeMap<&str, MvtLayer>, extent: u32, n: usize) -> Option<(usize, usize)> {
    let filtered = ctx.opts.postfilter.is_some().then(|| postfilter(ctx, z, x, y, layers, extent));
    if ctx.opts.json_progress {
        crate::progress::tile(ctx, z, x, y, n, true);
    }
    filtered
}

/// Run the postfilter on the layers of tile (z, x, y) (`filter_layers`).
/// `extent`: the extent of the tile layers. Returns the feature count and
/// the layer count before the filter.
fn postfilter(ctx: &Ctx, z: u8, x: u32, y: u32, layers: &mut BTreeMap<&str, MvtLayer>, extent: u32) -> (usize, usize) {
    let before = (layers.values().map(|l| l.features.len()).sum(), layers.len());
    // tippecanoe gives only layers with features to the filter.
    layers.retain(|_, l| !l.features.is_empty());
    let cmd = ctx.opts.postfilter.as_deref().expect("postfilter");
    let proj = ctx.opts.projection;
    let input = &*layers;
    let out = run(cmd, (z, x, y), "postfilter", &WARNED_POSTFILTER, |sink| write_postfilter_input(input, z, x, y, proj, sink), |r| read_postfilter_output(r, z, x, y, extent, proj));
    let mut stats = TileStats::default();
    for (name, l) in &out {
        for f in &l.features {
            match f.geom_type {
                1 => stats.count(name, GeomType::Point),
                2 => stats.count(name, GeomType::Line),
                _ => stats.count(name, GeomType::Polygon),
            }
            for t in f.tags.chunks(2).filter(|t| t.len() == 2) {
                let (ty, text) = value_stat(&l.values[t[1] as usize]);
                stats.value(name, &l.keys[t[0] as usize], ty, &text);
            }
        }
    }
    stats.finish();
    layers.clear();
    for (name, l) in out {
        layers.insert(intern(&name), l);
    }
    before
}
