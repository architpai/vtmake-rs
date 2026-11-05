//! `vtmake overzoom`: a port of `tippecanoe-overzoom` (`overzoom.cpp`) and of
//! the `overzoom()` function of `clip.cpp`, which `tile-join -O` also uses.
//!
//! `overzoom` makes the tile nz/nx/ny from one or more source tiles that
//! contain it: it moves each feature to world coordinates, clips it to the new
//! tile plus the buffer, applies the filter, the tiny polygon reduction and the
//! simplification, scales it to the new extent, cleans polygons (wagyu, scale
//! 1), and writes the tile with the attributes that `-y`/`-x` keep. With `-m`
//! (`demultiply`), the features of a `--retain-points-multiplier` cluster
//! become one feature (and `-E` accumulates their attributes onto it).
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::common::*;
use crate::getopt::{GetoptLong, Got};
use crate::mvtdec::{self, CLOSEPATH, Feature, Geom, Layer, MOVETO, Value};
use crate::usage::{self, Opt};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use vtmake_core::{Draw, DrawVec, GeomType, VT_LINETO, VT_MOVETO};
use vtmake_filter::{Filter, Value as FValue};
use vtmake_geom::num::cvt_i64;
use vtmake_geom::simplify::{SimplifyScratch, close_poly, remove_noop, simplify_lines, to_tile_scale};
use vtmake_geom::tiny::{TinyPolyAccum, reduce_tiny_poly};
use vtmake_geom::{PolyClipScratch, clip_lines, clip_point, simple_clip_poly};
use vtmake_mvt::{MvtFeature, MvtLayer, MvtPoint, MvtTile, MvtValue};

const VT_POINT: i32 = 1;
const VT_LINE: i32 = 2;
const VT_POLYGON: i32 = 3;

/// A decoded source tile (`source_tile`).
pub struct SourceTile {
    pub layers: Vec<Layer>,
    pub z: i32,
    pub x: i32,
    pub y: i32,
}

/// `attribute_op` (`-E`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accum {
    Sum,
    Product,
    Mean,
    Max,
    Min,
    Concat,
    Comma,
    Count,
}

/// The settings of `overzoom()`, other than the tile.
pub struct Params<'a> {
    /// `detail_or_unspecified`: 0 or less takes the detail from the source extent.
    pub detail: i32,
    pub buffer: i32,
    pub keep: BTreeSet<String>,
    pub exclude: BTreeSet<String>,
    pub exclude_prefix: Vec<String>,
    pub compress: bool,
    pub demultiply: bool,
    pub filter: Option<&'a Filter>,
    pub preserve_input_order: bool,
    pub accum: HashMap<String, Accum>,
    pub simplification: f64,
    pub tiny_polygon_size: f64,
    pub feature_limit: usize,
    pub dedup_by_id: bool,
}

impl Params<'_> {
    /// The settings of `tile-join -O` (`tileset_reader::retrieve_overzoom`).
    pub fn join(buffer: i32) -> Params<'static> {
        Params {
            detail: -1,
            buffer,
            keep: BTreeSet::new(),
            exclude: BTreeSet::new(),
            exclude_prefix: Vec::new(),
            compress: false,
            demultiply: false,
            filter: None,
            preserve_input_order: false,
            accum: HashMap::new(),
            simplification: 0.0,
            tiny_polygon_size: 0.0,
            feature_limit: usize::MAX,
            dedup_by_id: false,
        }
    }
}

/// A tile string as UTF-8 (the MVT writer keeps `str`).
fn text(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap_or_else(|_| not_supported("overzoom", "a tile string that is not UTF-8"))
}

/// The value of a filter for a tile feature (`evaluate(mvt_feature, ...)`):
/// `$id`, `$type` and `$zoom` come first, then the first tag with the key.
pub fn feature_value<'v>(layer: &'v Layer, id: Option<u64>, ty: i32, tags: &[u32], z: i32, key: &str) -> FValue<'v> {
    if key == "$id"
        && let Some(id) = id
    {
        return FValue::Number(id as f64);
    }
    if key == "$type" {
        return FValue::String(match ty {
            1 => "Point",
            2 => "LineString",
            3 => "Polygon",
            _ => "",
        });
    }
    if key == "$zoom" {
        return FValue::Number(z as u32 as f64);
    }
    for t in tags.as_chunks::<2>().0 {
        let Some(k) = layer.keys.get(t[0] as usize) else {
            continue;
        };
        if k == key.as_bytes() {
            return match layer.values.get(t[1] as usize) {
                Some(Value::String(s)) => FValue::String(text(s)),
                Some(Value::Float(f)) => FValue::Number(*f as f64),
                Some(Value::Double(f)) => FValue::Number(*f),
                Some(Value::Int(i) | Value::Sint(i)) => FValue::Number(*i as f64),
                Some(Value::Uint(u)) => FValue::Number(*u as f64),
                Some(Value::Bool(b)) => FValue::Bool(*b),
                Some(Value::Null) | None => FValue::Null,
            };
        }
    }
    FValue::NoKey
}

/// `mvt_value_to_double`.
fn to_double(v: &Value) -> f64 {
    match v {
        Value::String(s) => vtmake_parse::pull::atof(cstr(s)),
        Value::Float(f) => *f as f64,
        Value::Double(f) => *f,
        Value::Int(i) | Value::Sint(i) => *i as f64,
        Value::Uint(u) => *u as f64,
        Value::Bool(b) => *b as u8 as f64,
        Value::Null => 0.0,
    }
}

/// `mvt_value_to_long_long`.
fn to_long_long(v: &Value) -> i64 {
    match v {
        Value::String(s) => atoi_ll(cstr(s)),
        Value::Float(f) => cvt_i64(*f as f64),
        Value::Double(f) => cvt_i64(*f),
        Value::Int(i) | Value::Sint(i) => *i,
        Value::Uint(u) => *u as i64,
        Value::Bool(b) => *b as i64,
        Value::Null => 0,
    }
}

/// C `atoll` (glibc `strtoll(s, NULL, 10)`, saturated).
fn atoi_ll(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let neg = i < s.len() && s[i] == b'-';
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        i += 1;
    }
    let mut v: i128 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = (v * 10 + (s[i] - b'0') as i128).min(i64::MAX as i128 + 1);
        i += 1;
    }
    let v = if neg { -v } else { v };
    v.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// `mvt_value::toString` of a double: an integer if it is one.
fn double_text(v: f64) -> String {
    let l = cvt_i64(v);
    if v == l as f64 { l.to_string() } else { vtmake_parse::dtoa::dtoa_milo(v) }
}

/// `mvt_value::get_string_value`: the string, or `toString()` of other types.
fn string_value(v: &Value) -> Vec<u8> {
    match v {
        Value::String(s) => s.clone(),
        Value::Float(f) => double_text(*f as f64).into_bytes(),
        Value::Double(f) => double_text(*f).into_bytes(),
        Value::Int(i) | Value::Sint(i) => i.to_string().into_bytes(),
        Value::Uint(u) => u.to_string().into_bytes(),
        Value::Bool(b) => if *b { "true" } else { "false" }.into(),
        Value::Null => "null".into(),
    }
}

/// An `mvt_value` with its `count` (for `mean` and `count`).
#[derive(Clone)]
struct CountedValue {
    v: Value,
    count: u64,
}

impl CountedValue {
    fn double_count(v: f64, count: u64) -> Self {
        CountedValue { v: Value::Double(v), count }
    }
}

/// `preserve_attribute` for `mvt_value`.
fn preserve_attribute(op: Accum, key: &str, val: &Value, keys: &mut Vec<String>, values: &mut Vec<CountedValue>) {
    if let Some(i) = keys.iter().position(|k| k == key) {
        let f = &mut values[i];
        match op {
            Accum::Sum => *f = CountedValue::double_count(to_double(&f.v) + to_double(val), 0),
            Accum::Product => *f = CountedValue::double_count(to_double(&f.v) * to_double(val), 0),
            Accum::Max => {
                if to_double(val) > to_double(&f.v) {
                    *f = CountedValue { v: val.clone(), count: 0 };
                }
            }
            Accum::Min => {
                if to_double(val) < to_double(&f.v) {
                    *f = CountedValue { v: val.clone(), count: 0 };
                }
            }
            Accum::Mean => {
                let count = f.count;
                if count <= 1 {
                    *f = CountedValue::double_count((to_double(&f.v) + to_double(val)) / 2.0, 2);
                } else {
                    let sum = to_double(&f.v) * count as f64 + to_double(val);
                    *f = CountedValue::double_count(sum / (count + 1) as f64, count + 1);
                }
            }
            Accum::Concat | Accum::Comma => {
                let mut s = string_value(&f.v);
                if op == Accum::Comma {
                    s.push(b',');
                }
                s.extend_from_slice(&string_value(val));
                *f = CountedValue { v: Value::String(s), count: f.count };
            }
            Accum::Count => {
                let count = f.count;
                *f = if count <= 1 { CountedValue::double_count(2.0, 2) } else { CountedValue::double_count((count + 1) as f64, count + 1) };
            }
        }
        return;
    }
    let v = match op {
        Accum::Sum | Accum::Max | Accum::Min => CountedValue { v: val.clone(), count: 0 },
        Accum::Count => CountedValue::double_count(1.0, 1),
        Accum::Mean => CountedValue::double_count(to_double(val), 1),
        _ => {
            eprintln!("can't happen: operation that isn't used by --accumulate-numeric-attributes");
            std::process::exit(EXIT_IMPOSSIBLE)
        }
    };
    keys.push(key.to_string());
    values.push(v);
}

/// An `mvt_value` for the tile writer.
pub fn mvt_value(v: &Value) -> MvtValue {
    match v {
        Value::String(s) => MvtValue::String(text(s).into()),
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

fn should_keep(key: &str, p: &Params) -> bool {
    if p.keep.is_empty() || p.keep.contains(key) {
        if p.exclude.contains(key) {
            return false;
        }
        return !p.exclude_prefix.iter().any(|pre| key.starts_with(pre.as_str()));
    }
    false
}

/// A feature that waits for the end of its multiplier cluster (`tile_feature`).
struct TileFeature<'l> {
    geom: DrawVec,
    t: i32,
    id: Option<u64>,
    tags: Vec<u32>,
    layer: &'l Layer,
    seq: u64,
}

/// An output layer and the `seq` of each of its features.
struct OutLayer {
    layer: MvtLayer,
    seqs: Vec<u64>,
}

/// `feature_out` (with buffer -1 and `include_nonaggregate`): write the first
/// feature of the cluster, with the attributes of the others accumulated.
fn feature_out(features: &[TileFeature], out: &mut OutLayer, p: &Params, dedup: Option<&mut BTreeSet<u64>>) {
    let f0 = &features[0];
    let mut of = MvtFeature { geom_type: f0.t as u8, ..Default::default() };
    of.geometry = f0.geom.iter().map(|g| MvtPoint { op: g.op, x: g.x, y: g.y }).collect();

    if let (Some(id), Some(set)) = (f0.id, dedup)
        && !set.insert(id)
    {
        of.geometry.clear();
    }
    if of.geometry.is_empty() {
        return;
    }
    of.id = f0.id;
    let layer = &mut out.layer;
    let tag = |layer: &mut MvtLayer, of: &mut MvtFeature, l: &Layer, t: &[u32]| {
        let key = text(&l.keys[t[0] as usize]);
        layer.tag(of, key, &mvt_value(&l.values[t[1] as usize]));
    };

    if !p.accum.is_empty() {
        let mut full_keys: Vec<String> = Vec::new();
        let mut full_values: Vec<CountedValue> = Vec::new();
        for t in f0.tags.as_chunks::<2>().0 {
            let key = text(&f0.layer.keys[t[0] as usize]);
            if should_keep(key, p) {
                if p.accum.contains_key(key) {
                    full_keys.push(key.to_string());
                    full_values.push(CountedValue { v: f0.layer.values[t[1] as usize].clone(), count: 0 });
                } else {
                    tag(layer, &mut of, f0.layer, t);
                }
            }
        }
        for f in &features[1..] {
            for t in f.tags.as_chunks::<2>().0 {
                let key = text(&f.layer.keys[t[0] as usize]);
                if should_keep(key, p)
                    && let Some(&op) = p.accum.get(key)
                {
                    preserve_attribute(op, key, &f.layer.values[t[1] as usize], &mut full_keys, &mut full_values);
                }
            }
        }
        for (k, v) in full_keys.iter().zip(&full_values) {
            if should_keep(k, p) {
                layer.tag(&mut of, k, &mvt_value(&v.v));
            }
        }
    } else {
        for t in f0.tags.as_chunks::<2>().0 {
            if should_keep(text(&f0.layer.keys[t[0] as usize]), p) {
                tag(layer, &mut of, f0.layer, t);
            }
        }
    }
    layer.features.push(of);
    out.seqs.push(f0.seq);
}

/// `close_poly`, with its warning for a ring that is not closed.
fn close_poly_warn(geom: &mut DrawVec) {
    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() && geom[j].op == VT_LINETO {
                j += 1;
            }
            if j - 1 > i && (geom[j - 1].x != geom[i].x || geom[j - 1].y != geom[i].y) {
                eprintln!("Internal error: polygon not closed");
            }
            i = j;
        } else {
            i += 1;
        }
    }
    close_poly(geom);
}

/// `remove_noop` for a type code (types other than point and line have the
/// passes of polygons).
fn remove_noop_t(geom: &mut DrawVec, t: i32) {
    let gt = match t {
        VT_POINT => GeomType::Point,
        VT_LINE => GeomType::Line,
        _ => GeomType::Polygon,
    };
    remove_noop(geom, gt, 0);
}

/// `overzoom()`: the tile nz/nx/ny from `tiles`. The result is the encoded
/// tile (gzip if `p.compress`), or empty if the tile has no features. With
/// `next`, it adds the child tiles that have features.
pub fn overzoom(tiles: &[SourceTile], nz: i32, nx: i32, ny: i32, p: &Params, next: Option<&mut Vec<(u32, u32)>>) -> Vec<u8> {
    let mut out: Vec<OutLayer> = Vec::new();
    let mut dedup_ids: BTreeMap<Vec<u8>, BTreeSet<u64>> = BTreeMap::new();
    let mut cleaner = vtmake_wagyu::Cleaner::new();
    let mut clip_scratch = PolyClipScratch::default();
    let mut simp_scratch = SimplifyScratch::default();
    let mut tmp = DrawVec::new();
    const FIRST: &[u8] = b"tippecanoe:retain_points_multiplier_first";
    const SEQUENCE: &[u8] = b"tippecanoe:retain_points_multiplier_sequence";

    for tile in tiles {
        for layer in &tile.layers {
            let det = if p.detail <= 0 { ((layer.extent as f64).ln() / 2f64.ln()).round() as i32 } else { p.detail };
            let name = text(&layer.name);
            let ol = match out.iter().position(|l| l.layer.name == name) {
                Some(i) => i,
                None => {
                    let mut l = MvtLayer::new(name);
                    l.version = layer.version;
                    l.extent = (1i64 << det) as u32;
                    out.push(OutLayer { layer: l, seqs: Vec::new() });
                    out.len() - 1
                }
            };
            let outlayer = &mut out[ol];
            let mut dedup = p.dedup_by_id.then(|| dedup_ids.entry(layer.name.clone()).or_default());

            let mut pending: Vec<TileFeature> = Vec::new();
            let mut accum_area = TinyPolyAccum::default();

            for feature in &layer.features {
                let mut tags = feature.tags.clone();
                let mut seq = 0u64;
                let t = feature.ty;
                let mark = |dedup: &mut Option<&mut BTreeSet<u64>>| {
                    if let (Some(set), Some(id)) = (dedup.as_mut(), feature.id) {
                        set.insert(id);
                    }
                };

                // To world coordinates.
                let tilesize = 1i64 << (32 - tile.z);
                let mut ring_closure = Draw::new(0, 0, 0);
                let sametile = nz == tile.z && nx == tile.x && ny == tile.y && outlayer.layer.extent as i64 >= layer.extent as i64;
                let extent = layer.extent as i64;
                let mut geom: DrawVec = Vec::with_capacity(feature.geometry.len());
                for g in &feature.geometry {
                    if g.op == CLOSEPATH {
                        geom.push(ring_closure);
                    } else {
                        let d = Draw::new(g.op as u8, g.x * tilesize / extent + tile.x as i64 * tilesize, g.y * tilesize / extent + tile.y as i64 * tilesize);
                        geom.push(d);
                        if g.op == MOVETO {
                            ring_closure = Draw::new(VT_LINETO, d.x, d.y);
                        }
                    }
                }

                // To the output tile, at world scale (as tippecanoe's clipping wants).
                let outtilesize = 1i64 << (32 - nz);
                for g in &mut geom {
                    g.x -= nx as i64 * outtilesize;
                    g.y -= ny as i64 * outtilesize;
                }

                let (mut xmin, mut ymin, mut xmax, mut ymax) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
                for g in &geom {
                    xmin = xmin.min(g.x);
                    ymin = ymin.min(g.y);
                    xmax = xmax.max(g.x);
                    ymax = ymax.max(g.y);
                }
                let b = outtilesize * p.buffer as i64 / 256;
                if xmax < -b || ymax < -b || xmin > outtilesize + b || ymin > outtilesize + b {
                    mark(&mut dedup);
                    continue;
                }
                match t {
                    VT_LINE => {
                        tmp.clear();
                        clip_lines(&geom, nz as u32, p.buffer as i64, &mut tmp);
                        std::mem::swap(&mut geom, &mut tmp);
                    }
                    VT_POLYGON => {
                        tmp.clear();
                        simple_clip_poly(&geom, nz as u32, p.buffer as i64, &mut tmp, &mut clip_scratch);
                        std::mem::swap(&mut geom, &mut tmp);
                    }
                    VT_POINT => clip_point(&mut geom, nz as u32, p.buffer as i64),
                    _ => {}
                }
                if geom.is_empty() {
                    mark(&mut dedup);
                    continue;
                }

                let mut flush = false;
                if p.demultiply {
                    let mut i = tags.len() as isize - 2;
                    while i >= 0 {
                        let u = i as usize;
                        let key = layer.keys.get(tags[u] as usize).map(Vec::as_slice);
                        if key == Some(FIRST) {
                            if let Some(Value::Bool(true)) = layer.values.get(tags[u + 1] as usize) {
                                flush = true;
                                tags.drain(u..u + 2);
                            }
                        } else if u < tags.len() && key == Some(SEQUENCE) {
                            seq = layer.values.get(tags[u + 1] as usize).map(to_long_long).unwrap_or(0) as u64;
                            tags.drain(u..u + 2);
                        }
                        i -= 2;
                    }
                } else {
                    flush = true;
                }

                if flush && !pending.is_empty() {
                    feature_out(&pending, outlayer, p, dedup.as_deref_mut());
                    if outlayer.layer.features.len() >= p.feature_limit {
                        // `pending` stays: tippecanoe writes it again after the loop.
                        break;
                    }
                    pending.clear();
                }

                if let Some(f) = p.filter {
                    let mut excluded = Vec::new();
                    let get = |k: &str| feature_value(layer, feature.id, t, &tags, nz, k);
                    if !f.evaluate(name, &get, &mut excluded) {
                        mark(&mut dedup);
                        continue;
                    }
                }

                let mut still_needs_simplification = true;
                if t == VT_POLYGON && p.tiny_polygon_size > 0.0 {
                    tmp.clear();
                    let r = reduce_tiny_poly(&geom, nz as u32, det as u32, p.tiny_polygon_size, &mut accum_area, &mut tmp);
                    std::mem::swap(&mut geom, &mut tmp);
                    still_needs_simplification = r.still_needs_simplification;
                }
                if p.simplification > 0.0 && still_needs_simplification && (t == VT_POLYGON || t == VT_LINE) {
                    let retain = if t == VT_POLYGON { 4 } else { 0 };
                    simplify_lines(&mut geom, nz as u32, det as u32, false, p.simplification, retain, &mut simp_scratch);
                }

                to_tile_scale(&mut geom, nz as u32, det as u32);

                if !sametile {
                    remove_noop_t(&mut geom, t);
                    if t == VT_POLYGON {
                        // `clean_or_clip_poly(geom, 0, 0, false, false)` (the cleaner
                        // does its `remove_noop`).
                        tmp.clear();
                        if cleaner.clean_unscaled(&geom, &mut tmp).is_err() {
                            eprintln!("Internal error: Polygon cleaning failed. Log in /tmp/wagyu.log");
                            std::process::exit(EXIT_IMPOSSIBLE);
                        }
                        std::mem::swap(&mut geom, &mut tmp);
                    }
                }
                if t == VT_POLYGON {
                    close_poly_warn(&mut geom);
                }
                if geom.is_empty() {
                    mark(&mut dedup);
                    continue;
                }

                pending.push(TileFeature { geom, t, id: feature.id, tags, layer, seq });
            }

            if !pending.is_empty() {
                feature_out(&pending, outlayer, p, dedup);
                pending.clear();
                if outlayer.layer.features.len() >= p.feature_limit {
                    // As tippecanoe: this leaves the layer loop only, without the sort.
                    break;
                }
            }

            if p.preserve_input_order {
                // `std::stable_sort` by `seq`.
                let ol = &mut out[ol];
                let mut idx: Vec<usize> = (0..ol.seqs.len()).collect();
                idx.sort_by_key(|&i| ol.seqs[i]);
                let mut feats: Vec<Option<MvtFeature>> = std::mem::take(&mut ol.layer.features).into_iter().map(Some).collect();
                ol.layer.features = idx.iter().map(|&i| feats[i].take().unwrap()).collect();
                ol.seqs = idx.iter().map(|&i| ol.seqs[i]).collect();
            }
        }
    }

    if let Some(next) = next
        && !out.is_empty()
    {
        // Which child tiles have features? Overzoom this tile to each of them,
        // with a feature limit of 1.
        let src = [SourceTile { layers: out.iter().map(|l| to_source(&l.layer)).collect(), z: nz, x: nx, y: ny }];
        let child_params = Params {
            detail: p.detail,
            buffer: p.buffer,
            keep: p.keep.clone(),
            exclude: p.exclude.clone(),
            exclude_prefix: p.exclude_prefix.clone(),
            compress: false,
            demultiply: p.demultiply,
            filter: p.filter,
            preserve_input_order: p.preserve_input_order,
            accum: p.accum.clone(),
            simplification: p.simplification,
            tiny_polygon_size: p.tiny_polygon_size,
            feature_limit: 1,
            dedup_by_id: p.dedup_by_id,
        };
        for x in 0..2 {
            for y in 0..2 {
                let child = overzoom(&src, nz + 1, nx * 2 + x, ny * 2 + y, &child_params, None);
                if !child.is_empty() {
                    next.push(((nx * 2 + x) as u32, (ny * 2 + y) as u32));
                }
            }
        }
    }

    let layers: Vec<MvtLayer> = out.into_iter().map(|l| l.layer).filter(|l| !l.features.is_empty()).collect();
    if layers.is_empty() {
        return Vec::new();
    }
    let mut pbf = Vec::new();
    MvtTile { layers }.encode(&mut pbf);
    if p.compress {
        let mut gz = Vec::with_capacity(pbf.len() / 2 + 64);
        vtmake_mvt::gzip::gzip(&pbf, &mut gz);
        gz
    } else {
        pbf
    }
}

/// An output layer as a source layer (the `mvt_tile` that the child test reads).
fn to_source(l: &MvtLayer) -> Layer {
    let value = |v: &MvtValue| match v {
        MvtValue::String(s) => Value::String(s.as_bytes().to_vec()),
        MvtValue::Float(f) => Value::Float(*f),
        MvtValue::Double(f) => Value::Double(*f),
        MvtValue::Int(i) => Value::Int(*i),
        MvtValue::Uint(u) => Value::Uint(*u),
        MvtValue::Sint(i) => Value::Sint(*i),
        MvtValue::Bool(b) => Value::Bool(*b),
    };
    Layer {
        version: l.version,
        name: l.name.as_bytes().to_vec(),
        features: l
            .features
            .iter()
            .map(|f| Feature {
                tags: f.tags.clone(),
                geometry: f.geometry.iter().map(|g| Geom { op: g.op as u32, x: g.x, y: g.y }).collect(),
                ty: f.geom_type as i32,
                id: f.id,
            })
            .collect(),
        keys: l.keys.iter().map(|k| k.as_bytes().to_vec()).collect(),
        values: l.values.iter().map(value).collect(),
        extent: l.extent,
    }
}

/// Decode the source tiles (`overzoom(std::vector<input_tile>, ...)`).
pub fn decode_sources(inputs: Vec<(Vec<u8>, i32, i32, i32)>) -> Vec<SourceTile> {
    inputs
        .into_iter()
        .map(|(data, z, x, y)| match mvtdec::decode(&data) {
            Ok((layers, _)) => SourceTile { layers, z, x, y },
            Err(mvtdec::Error::Decompress) => Fatal { msg: "Decompression error: data error".into(), code: EXIT_MVT }.exit(),
            Err(mvtdec::Error::Pbf) => Fatal { msg: format!("PBF decoding error in tile {z}/{x}/{y}"), code: EXIT_PROTOBUF }.exit(),
        })
        .collect()
}

/// `set_attribute_accum` for one name and method.
fn set_accum(map: &mut HashMap<String, Accum>, name: &str, ty: &str) {
    let t = match ty {
        "sum" => Accum::Sum,
        "product" => Accum::Product,
        "mean" => Accum::Mean,
        "max" => Accum::Max,
        "min" => Accum::Min,
        "concat" => Accum::Concat,
        "comma" => Accum::Comma,
        "count" => Accum::Count,
        _ => {
            eprintln!("Attribute method ({ty}) must be sum, product, mean, max, min, concat, comma, or count");
            std::process::exit(EXIT_ARGS)
        }
    };
    // `unordered_map::insert`: the first one stays.
    map.entry(name.to_string()).or_insert(t);
}

/// `set_attribute_accum(attribute_accum, arg, argv)`: `name:method` or a JSON object.
pub fn parse_accum(map: &mut HashMap<String, Accum>, arg: &str, argv0: &str) {
    use vtmake_parse::pull::Kind;
    if arg.starts_with('{') {
        let (j, o) = vtmake_parse::pull::parse_tree(arg.as_bytes()).unwrap_or_else(|e| Fatal { msg: format!("{argv0}: -E{arg}: {e}"), code: EXIT_JSON }.exit());
        if j.kind(o) != Kind::Hash {
            Fatal { msg: format!("{argv0}: -E{arg}: not a JSON object"), code: EXIT_JSON }.exit();
        }
        for (i, e) in j.items(o).chunks(2).enumerate() {
            if j.kind(e[0]) != Kind::String {
                Fatal { msg: format!("{argv0}: -E{arg}: key {i} not a string"), code: EXIT_JSON }.exit();
            }
            if j.kind(e[1]) != Kind::String {
                Fatal { msg: format!("{argv0}: -E{arg}: value {i} not a string"), code: EXIT_JSON }.exit();
            }
            let s = |n| String::from_utf8_lossy(j.text(n)).into_owned();
            set_accum(map, &s(e[0]), &s(e[1]));
        }
        return;
    }
    let Some(k) = arg.find(':') else {
        Fatal { msg: format!("-E{arg} option must be in the form -Ename:method"), code: EXIT_ARGS }.exit()
    };
    set_accum(map, &arg[..k], &arg[k + 1..]);
}

/// `read_unidecode`: tippecanoe reads the file, but nothing uses the data in
/// the tools. Only the open error can change the result.
pub fn read_unidecode(fname: &str) {
    if let Err(e) = std::fs::File::open(fname) {
        Fatal { msg: format!("{fname}: {}", strerror(&e)), code: EXIT_OPEN }.exit();
    }
}

/// C `sscanf(s, "%d/%d/%d")`: three integers.
fn scan_zxy(s: &str) -> Option<(i32, i32, i32)> {
    let mut rest = s;
    let mut out = [0i32; 3];
    for (n, slot) in out.iter_mut().enumerate() {
        let (v, used) = scan_int(rest)?;
        *slot = v;
        rest = &rest[used..];
        if n < 2 {
            rest = rest.strip_prefix('/')?;
        }
    }
    Some((out[0], out[1], out[2]))
}

/// `%d`: white space, a sign, digits. The value and the bytes used.
fn scan_int(s: &str) -> Option<(i32, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let neg = i < b.len() && b[i] == b'-';
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let start = i;
    let mut v: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add((b[i] - b'0') as i64);
        i += 1;
    }
    if i == start {
        return None;
    }
    let v = if neg { v.wrapping_neg() } else { v };
    Some((v as i32, i))
}

const OPTIONS: &[Opt] = &[
    Opt::heading("Output tile"),
    Opt::new("output", true, 'o' as u32),
    Opt::new("source-tile", true, 't' as u32),
    Opt::new("no-tile-compression", false, 'd' as u32 & 0x1F),
    Opt::heading("Tile resolution"),
    Opt::new("full-detail", true, 'd' as u32),
    Opt::new("buffer", true, 'b' as u32),
    Opt::heading("Filtering feature attributes"),
    Opt::new("include", true, 'y' as u32),
    Opt::new("exclude", true, 'x' as u32),
    Opt::new("exclude-prefix", true, 'x' as u32 & 0x1F),
    Opt::heading("Modifying feature attributes"),
    Opt::new("accumulate-attribute", true, 'E' as u32),
    Opt::heading("Filtering features"),
    Opt::new("feature-filter", true, 'j' as u32),
    Opt::new("feature-filter-file", true, 'J' as u32),
    Opt::new("filter-points-multiplier", false, 'm' as u32),
    Opt::new("deduplicate-by-id", false, 'i' as u32 & 0x1F),
    Opt::heading("Line and polygon simplification"),
    Opt::new("line-simplification", true, 'S' as u32),
    Opt::new("tiny-polygon-size", true, 's' as u32 & 0x1F),
    Opt::heading("Reordering features within the tile"),
    Opt::new("preserve-input-order", false, 'o' as u32 & 0x1F),
    Opt::heading(""),
    Opt::new("unidecode-data", true, 'u' as u32 & 0x1F),
];

fn print_usage(argv0: &str) -> ! {
    let forms = ["[options] tile.pbf.gz oz/ox/oy nz/nx/ny", "[options] --source-tile=nz/nx/ny tile.pbf.gz oz/ox/oy ..."];
    let mut s = usage::usage_text(argv0, &forms, OPTIONS, &[("output", "newtile.pbf.gz", 0)]);
    s.push_str("\nThe tile nz/nx/ny is created from the tile or tiles oz/ox/oy that contain it.\n");
    s.push_str("In the second form, each source tile is named by a file name and a z/x/y pair.\n");
    eprint!("{s}");
    std::process::exit(1)
}

/// `tippecanoe-overzoom`.
pub fn main(argv: Vec<String>) {
    let argv0 = argv.first().cloned().unwrap_or_else(|| "tippecanoe-overzoom".into());
    let (optstring, longs) = usage::getopt_table(OPTIONS);
    let mut g = GetoptLong::new(&argv, &optstring, &longs);
    let mut p = Params {
        detail: 12,
        buffer: 5,
        keep: BTreeSet::new(),
        exclude: BTreeSet::new(),
        exclude_prefix: Vec::new(),
        compress: true,
        demultiply: false,
        filter: None,
        preserve_input_order: false,
        accum: HashMap::new(),
        simplification: 0.0,
        tiny_polygon_size: 0.0,
        feature_limit: usize::MAX,
        dedup_by_id: false,
    };
    let mut filter = String::new();
    let (mut outtile, mut outfile): (Option<String>, Option<String>) = (None, None);
    while let Some(got) = g.next_opt() {
        let (val, arg) = match got {
            Got::Long(i, v) => (longs[i].ident, v),
            Got::Short(c, v) => (c as u32, v),
            Got::Error => ('?' as u32, None),
        };
        let arg = arg.unwrap_or_default();
        const C: fn(char) -> u32 = |c| c as u32 & 0x1F;
        match val {
            v if v == 'y' as u32 => drop(p.keep.insert(arg)),
            v if v == 'x' as u32 => drop(p.exclude.insert(arg)),
            v if v == C('x') => p.exclude_prefix.push(arg),
            v if v == 'o' as u32 => outfile = Some(arg),
            v if v == 'd' as u32 => p.detail = atoi(&arg),
            v if v == 'b' as u32 => p.buffer = atoi(&arg),
            v if v == 'm' as u32 => p.demultiply = true,
            v if v == 'j' as u32 => filter = arg,
            v if v == 'J' as u32 => {
                filter = match std::fs::read(&arg) {
                    Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                    Err(e) => Fatal { msg: format!("{arg}: {}", strerror(&e)), code: EXIT_OPEN }.exit(),
                }
            }
            v if v == C('o') => p.preserve_input_order = true,
            v if v == 'E' as u32 => parse_accum(&mut p.accum, &arg, &argv0),
            v if v == C('u') => read_unidecode(&arg),
            v if v == 't' as u32 => outtile = Some(arg),
            v if v == C('s') => p.tiny_polygon_size = atof(&arg),
            v if v == 'S' as u32 => p.simplification = atof(&arg),
            v if v == C('d') => p.compress = false,
            v if v == C('i') => p.dedup_by_id = true,
            v => {
                // `fprintf(stderr, "Unrecognized flag -%c\n", i)`.
                let mut m = b"Unrecognized flag -".to_vec();
                m.push(v as u8);
                m.push(b'\n');
                use std::io::Write;
                let _ = std::io::stderr().write_all(&m);
                print_usage(&argv0);
            }
        }
    }
    let operands = std::mem::take(&mut g.operands);

    let Some(outfile) = outfile else {
        eprintln!("{argv0}: must specify -o newtile.pbf.gz");
        print_usage(&argv0)
    };
    let not_zxy = |s: &str| -> ! {
        eprintln!("{s}: not in z/x/y form");
        print_usage(&argv0)
    };
    let mut sources: Vec<(String, i32, i32, i32)> = Vec::new();
    let (nz, nx, ny);
    match &outtile {
        None => {
            if operands.len() != 3 {
                eprintln!("Wrong number of arguments");
                print_usage(&argv0);
            }
            let (oz, ox, oy) = scan_zxy(&operands[1]).unwrap_or_else(|| not_zxy(&operands[1]));
            (nz, nx, ny) = scan_zxy(&operands[2]).unwrap_or_else(|| not_zxy(&operands[2]));
            sources.push((operands[0].clone(), oz, ox, oy));
        }
        Some(t) => {
            if !operands.len().is_multiple_of(2) {
                eprintln!("Unpaired arguments");
                print_usage(&argv0);
            }
            (nz, nx, ny) = scan_zxy(t).unwrap_or_else(|| not_zxy(t));
            for pair in operands.chunks(2) {
                let (oz, ox, oy) = scan_zxy(&pair[1]).unwrap_or_else(|| not_zxy(&pair[1]));
                sources.push((pair[0].clone(), oz, ox, oy));
            }
        }
    }

    let json_filter = (!filter.is_empty()).then(|| match vtmake_filter::parse_filter(&filter) {
        Ok(t) => Filter::new(&t),
        Err((m, c)) => Fatal { msg: m, code: c }.exit(),
    });
    p.filter = json_filter.as_ref();
    let mut inputs = Vec::new();
    for (f, z, x, y) in sources {
        match std::fs::read(&f) {
            Ok(b) => inputs.push((b, z, x, y)),
            Err(e) => Fatal { msg: format!("{f}: {}", strerror(&e)), code: 1 }.exit(),
        }
    }
    let tiles = decode_sources(inputs);
    let out = overzoom(&tiles, nz, nx, ny, &p, None);
    if let Err(e) = std::fs::write(&outfile, &out) {
        Fatal { msg: format!("{outfile}: {}", strerror(&e)), code: 1 }.exit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zxy_scan() {
        assert_eq!(scan_zxy("11/327/791"), Some((11, 327, 791)));
        assert_eq!(scan_zxy(" 1/-2/+3junk"), Some((1, -2, 3)));
        assert_eq!(scan_zxy("1/2"), None);
        assert_eq!(scan_zxy("a/2/3"), None);
    }

    #[test]
    fn accumulate_values() {
        let (mut k, mut v) = (Vec::new(), Vec::new());
        preserve_attribute(Accum::Mean, "a", &Value::Int(2), &mut k, &mut v);
        preserve_attribute(Accum::Mean, "a", &Value::Int(4), &mut k, &mut v);
        preserve_attribute(Accum::Mean, "a", &Value::Int(6), &mut k, &mut v);
        assert!(matches!(v[0].v, Value::Double(d) if d == 4.0));
        assert_eq!(v[0].count, 3);
        assert_eq!(string_value(&Value::Double(2.5)), b"2.5");
        assert_eq!(string_value(&Value::Double(3.0)), b"3");
    }
}
