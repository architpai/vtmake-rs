//! Metadata (spec 9.3): the input statistics and the metadata rows.
//!
//! tippecanoe collects the statistics when it reads each feature
//! (`serial.cpp:serialize_feature`, `mbtiles.cpp:add_to_tilestats`): the feature
//! count and geometry types per layer, the attribute sample values, and the
//! file bounding boxes. Here one parallel pass over the chunks after reading
//! collects the same values. A chunk keeps each distinct value once, so the pass
//! looks at each distinct value of an attribute only once in each chunk.

use crate::cli;
use crate::store::{Chunk, FeatMeta};
use vtmake_core::{AttrType, GeomType};
use vtmake_out::json_string;
use rayon::prelude::*;
use std::collections::BTreeMap;

/// The tilestats limits (`--tile-stats-*-limit`, defaults 1000, 1000, 100) and
/// `-pg` (`--no-tile-stats`).
struct Limits {
    /// `max_tilestats_sample_values`: the number of sample values kept per attribute.
    samples: usize,
    /// `max_tilestats_attributes`: the number of attributes counted per layer.
    attributes: usize,
    /// `max_tilestats_values`: the number of attributes and values written per layer.
    values: usize,
    tilestats: bool,
}

static LIMITS: std::sync::OnceLock<Limits> = std::sync::OnceLock::new();

/// Set the limits from the command line. Call once, before [`collect`].
pub fn init(o: &cli::Options) {
    // `size_t` from `atoi`: a negative value is very large.
    let _ = LIMITS.set(Limits {
        samples: o.tilestats_sample_values_limit as usize,
        attributes: o.tilestats_attributes_limit as usize,
        values: o.tilestats_values_limit as usize,
        tilestats: !o.prevent[b'g' as usize],
    });
}

fn lim() -> &'static Limits {
    LIMITS.get_or_init(|| Limits { samples: 1000, attributes: 1000, values: 100, tilestats: true })
}

/// `max_tilestats_sample_values`.
pub fn sample_limit() -> usize {
    lim().samples
}
/// Names longer than this (in UTF-16 units) are cut in the tilestats (`truncate16`).
const MAX_NAME: usize = 256;

pub struct AttrStats {
    /// The smallest distinct `(text, type)` values, sorted, at most the sample limit.
    pub samples: Vec<(Box<str>, AttrType)>,
    /// Smallest and largest number (of all values, not only the samples).
    pub min: f64,
    pub max: f64,
}

impl Default for AttrStats {
    fn default() -> Self {
        AttrStats { samples: Vec::new(), min: f64::INFINITY, max: f64::NEG_INFINITY }
    }
}

impl AttrStats {
    fn merge(&mut self, o: AttrStats) {
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
        if self.samples.is_empty() {
            self.samples = o.samples;
            return;
        }
        self.samples.extend(o.samples);
        self.samples.sort_unstable();
        self.samples.dedup();
        self.samples.truncate(lim().samples);
    }

    /// The set of sample types: `1 << type` as in `mbtiles.cpp`.
    fn types(&self) -> u8 {
        self.samples.iter().fold(0, |t, s| t | 1 << s.1 as u8)
    }
}

#[derive(Default)]
pub struct LayerStats {
    pub points: u64,
    pub lines: u64,
    pub polygons: u64,
    pub attrs: BTreeMap<Box<str>, AttrStats>,
}

impl LayerStats {
    fn merge(&mut self, o: LayerStats) {
        self.points += o.points;
        self.lines += o.lines;
        self.polygons += o.polygons;
        for (k, a) in o.attrs {
            self.attrs.entry(k).or_default().merge(a);
        }
    }
}

/// The empty value of `FeatExtra::raw`.
pub const RAW_BBOX_EMPTY: [[i64; 4]; 2] = [[0xFFFF_FFFF, 0xFFFF_FFFF, 0, 0], [0x1_FFFF_FFFF, 0xFFFF_FFFF, 0x1_0000_0000, 0]];

/// Add one input vertex to `FeatExtra::raw` (as `Stats::add_vertex`).
pub fn add_raw_vertex(b: &mut [[i64; 4]; 2], x: i64, y: i64) {
    let mut st = Stats { layers: BTreeMap::new(), bbox1: b[0], bbox2: b[1] };
    st.add_vertex(x, y);
    *b = [st.bbox1, st.bbox2];
}

pub struct Stats {
    /// Per layer name, in byte order (C++ `std::map<std::string>`).
    pub layers: BTreeMap<Box<str>, LayerStats>,
    /// `file_bbox1`: vertices with x masked to 32 bits and y clamped (-180 to 180).
    pub bbox1: [i64; 4],
    /// `file_bbox2`: as `bbox1`, but the western half moved 360 degrees east.
    pub bbox2: [i64; 4],
}

impl Stats {
    fn empty() -> Self {
        Stats {
            layers: BTreeMap::new(),
            bbox1: RAW_BBOX_EMPTY[0],
            bbox2: RAW_BBOX_EMPTY[1],
        }
    }

    fn merge(mut self, o: Stats) -> Stats {
        for (l, s) in o.layers {
            self.layers.entry(l).or_default().merge(s);
        }
        for (a, b) in [(&mut self.bbox1, &o.bbox1), (&mut self.bbox2, &o.bbox2)] {
            a[0] = a[0].min(b[0]);
            a[1] = a[1].min(b[1]);
            a[2] = a[2].max(b[2]);
            a[3] = a[3].max(b[3]);
        }
        self
    }

    /// Add one vertex to `bbox1` and `bbox2` (`serial.cpp:serialize_feature`).
    #[inline]
    fn add_vertex(&mut self, x: i64, y: i64) {
        let x = x & 0xFFFF_FFFF;
        let y = y.clamp(0, 0xFFFF_FFFF);
        let b = &mut self.bbox1;
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
        let x = if x < 0x8000_0000 { x + 0x1_0000_0000 } else { x };
        let b = &mut self.bbox2;
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
    }

    fn add_bboxes(&mut self, c: &Chunk, m: &FeatMeta, draws: &mut Vec<vtmake_core::Draw>) {
        let b = m.bbox;
        // The two corners give the same result as all vertices if the feature is
        // inside the plane and does not cross the prime meridian.
        if b[0] >= 0 && b[2] <= 0xFFFF_FFFF && (b[2] < 0x8000_0000 || b[0] >= 0x8000_0000) {
            self.add_vertex(b[0], b[1]);
            self.add_vertex(b[2], b[3]);
        } else {
            draws.clear();
            c.decode_geom(m, draws);
            for d in draws.iter().filter(|d| d.op != vtmake_core::VT_CLOSEPATH) {
                self.add_vertex(d.x, d.y);
            }
        }
    }
}

/// The statistics of one chunk.
fn chunk_stats(c: &Chunk, default_layer: &str) -> Stats {
    let mut st = Stats::empty();
    // With `-aw` or `--clip-bounding-box`, the bounds come from the input
    // geometry (`serial.cpp:serialize_feature`, before `scale_geometry`).
    let raw = crate::read::READ_GEOM.get().is_some_and(|r| r.changes_bounds());
    if raw {
        let raws = c.extra.iter().map(|e| &e.raw).chain(c.clipped_away.iter().map(|(_, r)| r));
        for r in raws {
            st = st.merge(Stats { layers: BTreeMap::new(), bbox1: r[0], bbox2: r[1] });
        }
    }
    let nslots = c.layers.len() + 1;
    let slot = |m: &FeatMeta| if m.layer == u16::MAX { c.layers.len() } else { m.layer as usize };
    let mut counts = vec![[0u64; 3]; nslots];
    // The distinct values of each (layer slot, key) pair: `vals[g]`. `group`
    // gives `g` of a pair (a sparse map: a chunk with many layers and keys
    // has few of all pairs); `last[key]` is the last pair of the key (most
    // chunks have one layer).
    let mut vals: Vec<(u32, u32, Vec<u32>)> = Vec::new();
    let mut group: hashbrown::HashMap<u64, u32, foldhash::fast::FixedState> = Default::default();
    let mut last = vec![(u32::MAX, 0u32); c.keys.len()];
    // `seen[v]`: 1 + the first pair that has value v, or 0. Other pairs with the
    // same value (for example `true`) use `more`.
    let mut seen = vec![0u32; c.vals.len()];
    let mut more: hashbrown::HashSet<u64, foldhash::fast::FixedState> = Default::default();
    let mut draws = Vec::new();
    for (i, m) in c.metas.iter().enumerate() {
        let s = slot(m);
        counts[s][m.geom_type as usize - 1] += 1;
        if !raw {
            st.add_bboxes(c, m, &mut draws);
        }
        for (k, v) in c.stat_attrs(i, m) {
            if c.vals[v as usize].ty == AttrType::Null {
                continue;
            }
            let l = &mut last[k as usize];
            let g = if l.0 == s as u32 {
                l.1
            } else {
                let g = *group.entry(((s as u64) << 32) | k as u64).or_insert_with(|| {
                    vals.push((s as u32, k, Vec::new()));
                    vals.len() as u32 - 1
                });
                *l = (s as u32, g);
                g
            };
            let first = &mut seen[v as usize];
            let new = if *first == 0 {
                *first = g + 1;
                true
            } else if *first == g + 1 {
                false
            } else {
                more.insert(((g as u64) << 32) | v as u64)
            };
            if new {
                vals[g as usize].2.push(v);
            }
        }
    }
    let layer_name = |s: usize| -> &str { if s == c.layers.len() { default_layer } else { &c.layers[s] } };
    for (s, n) in counts.iter().enumerate() {
        if n.iter().sum::<u64>() > 0 {
            let l = st.layers.entry(layer_name(s).into()).or_default();
            l.points += n[GeomType::Point as usize - 1];
            l.lines += n[GeomType::Line as usize - 1];
            l.polygons += n[GeomType::Polygon as usize - 1];
        }
    }
    for (s, k, vs) in vals {
        let (s, k) = (s as usize, k as usize);
        let mut a = AttrStats::default();
        let mut cand: Vec<(&str, AttrType)> = Vec::with_capacity(vs.len());
        for v in vs {
            let val = &c.vals[v as usize];
            let text = c.val_text(val);
            if val.ty == AttrType::Number {
                let d = atof(text);
                a.min = a.min.min(d);
                a.max = a.max.max(d);
            }
            cand.push((text, val.ty));
        }
        let max = lim().samples;
        if cand.len() > max {
            if max == 0 {
                cand.clear();
            } else {
                cand.select_nth_unstable(max - 1);
                cand.truncate(max);
            }
        }
        cand.sort_unstable();
        a.samples = cand.into_iter().map(|(t, ty)| (t.into(), ty)).collect();
        st.layers.entry(layer_name(s).into()).or_default().attrs.entry(c.keys[k].clone()).or_default().merge(a);
    }
    st
}

/// C `atof`. Most texts are JSON numbers: for them the Rust parser gives the
/// same value. Other texts (for example from `--set-attribute`) use `strtod`.
fn atof(text: &str) -> f64 {
    text.parse().unwrap_or_else(|_| vtmake_parse::pull::atof(text.as_bytes()))
}

/// Add distinct values of attribute `key` of `layer` (from the tiles: `-E`).
pub fn add_values(st: &mut Stats, layer: &str, key: &str, vals: Vec<(Box<str>, AttrType)>) {
    let mut a = AttrStats::default();
    for (text, ty) in &vals {
        if *ty == AttrType::Number {
            let d = atof(text);
            a.min = a.min.min(d);
            a.max = a.max.max(d);
        }
    }
    let mut cand = vals;
    cand.sort_unstable();
    cand.dedup();
    cand.truncate(lim().samples);
    a.samples = cand;
    st.layers.entry(layer.into()).or_default().attrs.entry(key.into()).or_default().merge(a);
}

/// Collect the statistics of all chunks (in parallel).
pub fn collect(chunks: &[Chunk], default_layers: &[&str]) -> Stats {
    chunks.par_iter().zip(default_layers).map(|(c, l)| chunk_stats(c, l)).reduce(Stats::empty, Stats::merge)
}

/// `truncate16(s, MAX_NAME)`: the longest start of `s` with at most 256 UTF-16
/// units. A NUL byte ends the string (C string).
fn truncate16(s: &str) -> &str {
    let mut len = 0;
    for (i, ch) in s.char_indices() {
        len += ch.len_utf16();
        if ch == '\0' || len > MAX_NAME {
            return &s[..i];
        }
    }
    s
}

/// `sixdig`: round to 6 decimals.
fn sixdig(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// `format_commandline`: argv joined by spaces. An argument with other characters
/// than `[A-Za-z0-9/_@:.%,-]` is put in single quotes; a quote in it becomes `'"'"'`.
fn format_commandline(argv: &[String]) -> String {
    // argv[0] can be "<program> build" (the vtmake subcommand): two words.
    let first: Vec<String> = argv.first().map(|a| a.split(' ').map(String::from).collect()).unwrap_or_default();
    let words: Vec<String> = first
        .iter()
        .chain(argv.iter().skip(1))
        .map(|a| {
            if a.bytes().all(|c| c.is_ascii_alphanumeric() || b"/-_@:.%,".contains(&c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\"'\"'"))
            }
        })
        .collect();
    words.join(" ")
}

fn type_name(types: u8, names: [&'static str; 4]) -> &'static str {
    match types {
        t if t == 1 << AttrType::Number as u8 => names[0],
        t if t == 1 << AttrType::Bool as u8 => names[1],
        t if t == 1 << AttrType::String as u8 => names[2],
        _ => names[3],
    }
}

/// `vector_layers` (`mbtiles.cpp:make_metadata`).
/// `layer_descriptions`: the `-L` description of each layer (`layermap_entry`);
/// `attribute_descriptions`: `-Y`, written in place of the type.
fn vector_layers(st: &Stats, minzoom: u8, maxzoom: u8, layer_descriptions: &BTreeMap<String, String>, attribute_descriptions: &BTreeMap<String, String>) -> String {
    let mut o = String::from("[");
    for (i, (name, l)) in st.layers.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        let desc = layer_descriptions.get(name.as_ref()).map_or("", String::as_str);
        o += &format!("{{\"id\":{},\"description\":{},\"minzoom\":{minzoom},\"maxzoom\":{maxzoom},\"fields\":{{", json_string(name), json_string(desc));
        // tippecanoe writes a field before it compares the count with the limit.
        for (j, (k, a)) in l.attrs.iter().take(lim().attributes.max(1)).enumerate() {
            if j > 0 {
                o.push(',');
            }
            match attribute_descriptions.get(k.as_ref()) {
                Some(d) => o += &format!("{}:{}", json_string(k), json_string(d)),
                None => {
                    let t = type_name(a.types(), ["Number", "Boolean", "String", "Mixed"]);
                    o += &format!("{}:\"{t}\"", json_string(k));
                }
            }
        }
        o += "}}";
    }
    o.push(']');
    o
}

/// `tilestats` (`mbtiles.cpp:tilestats`). Layer and attribute names are cut to 256
/// UTF-16 units first; names that become equal are merged.
fn tilestats(st: Stats) -> String {
    let cut = st.layers.iter().any(|(l, s)| truncate16(l).len() != l.len() || s.attrs.keys().any(|k| truncate16(k).len() != k.len()));
    let layers = if cut {
        let mut m: BTreeMap<Box<str>, LayerStats> = BTreeMap::new();
        for (l, s) in st.layers {
            let mut t = LayerStats { points: s.points, lines: s.lines, polygons: s.polygons, attrs: BTreeMap::new() };
            for (k, a) in s.attrs {
                t.attrs.entry(truncate16(&k).into()).or_default().merge(a);
            }
            m.entry(truncate16(&l).into()).or_default().merge(t);
        }
        m
    } else {
        st.layers
    };
    let mut o = format!("{{\"layerCount\":{},\"layers\":[", layers.len());
    for (i, (name, l)) in layers.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        let geometry = if l.points >= l.lines && l.points >= l.polygons {
            "Point"
        } else if l.lines >= l.polygons && l.lines >= l.points {
            "LineString"
        } else {
            "Polygon"
        };
        o += &format!(
            "{{\"layer\":{},\"count\":{},\"geometry\":\"{geometry}\",\"attributeCount\":{},\"attributes\":[",
            json_string(name),
            l.points + l.lines + l.polygons,
            l.attrs.len().min(lim().attributes)
        );
        for (j, (k, a)) in l.attrs.iter().take(lim().values).enumerate() {
            if j > 0 {
                o.push(',');
            }
            let types = a.types();
            let t = type_name(types, ["number", "boolean", "string", "mixed"]);
            o += &format!("{{\"attribute\":{},\"count\":{},\"type\":\"{t}\",\"values\":[", json_string(k), a.samples.len());
            let mut n = 0;
            for (text, ty) in &a.samples {
                if n == lim().values {
                    break;
                }
                // Strings longer than 256 UTF-16 units are not written (and not counted).
                if *ty != AttrType::String || truncate16(text).len() == text.len() {
                    if n > 0 {
                        o.push(',');
                    }
                    if *ty == AttrType::String {
                        o += &json_string(text);
                    } else {
                        o += text;
                    }
                    n += 1;
                }
            }
            o.push(']');
            if types & (1 << AttrType::Number as u8) != 0 {
                o += &format!(",\"min\":{},\"max\":{}", vtmake_parse::dtoa::dtoa_milo(a.min), vtmake_parse::dtoa::dtoa_milo(a.max));
            }
            o.push('}');
        }
        o += "]}";
    }
    o += "]}";
    o
}

/// What the tiling found, for the metadata.
pub struct TileResult {
    /// Highest zoom that had input features (metadata maxzoom, spec 5.5).
    pub maxzoom: u8,
    /// `(x << 32) | y` of the `-z` tile with the most output vertices, if any.
    pub center: Option<u64>,
    /// Per zoom 0 to `-z`: `(dropped_by_rate, tiny_polygons)`.
    pub strategies: Vec<[u64; 8]>,
}

/// The metadata rows in the mbtiles order (spec 9.3, 9.4). `generator` is
/// vtmake; all other values are the same as tippecanoe's.
pub fn metadata(opts: &cli::Options, st: Stats, fb: [i64; 4], t: &TileResult, layer_descriptions: &BTreeMap<String, String>) -> vtmake_out::Metadata {
    let name = opts.name.clone().unwrap_or_else(|| match &opts.output {
        cli::Output::Mbtiles(p) | cli::Output::Dir(p) => p.to_string_lossy().into_owned(),
    });
    let description = opts.description.clone().unwrap_or_else(|| name.clone());
    let maxz = t.maxzoom;

    // `main.cpp:read_input`: the center of the center tile, clamped to the bounds.
    let (cx, cy) = t.center.map_or((0, 0), |k| ((k >> 32) as i64, (k & 0xFFFF_FFFF) as i64));
    let (minlon, maxlat) = vtmake_geom::tile2lonlat(cx, cy, maxz as u32);
    let (maxlon, minlat) = vtmake_geom::tile2lonlat(cx + 1, cy + 1, maxz as u32);
    let (mut midlat, mut midlon) = ((maxlat + minlat) / 2.0, (maxlon + minlon) / 2.0);
    let (minlon, maxlat) = vtmake_geom::tile2lonlat(fb[0], fb[1], 32);
    let (maxlon, minlat) = vtmake_geom::tile2lonlat(fb[2], fb[3], 32);
    if midlat < minlat {
        midlat = minlat;
    }
    if midlat > maxlat {
        midlat = maxlat;
    }
    if midlon < minlon {
        midlon = minlon;
    }
    if midlon > maxlon {
        midlon = maxlon;
    }
    let (b1, b2) = (st.bbox1, st.bbox2);
    let ab = if b2[2] - b2[0] < b1[2] - b1[0] { b2 } else { b1 };
    let (minlon2, maxlat2) = vtmake_geom::tile2lonlat(ab[0], ab[1], 32);
    let (maxlon2, minlat2) = vtmake_geom::tile2lonlat(ab[2], ab[3], 32);
    let f4 = |a: f64, b: f64, c: f64, d: f64| format!("{:.6},{:.6},{:.6},{:.6}", sixdig(a), sixdig(b), sixdig(c), sixdig(d));

    // `mbtiles.cpp:stringify_strategies`: one object per zoom, omitted if all are empty.
    let strategies = t.strategies.iter().any(|v| v.iter().any(|&n| n > 0)).then(|| {
        let zs: Vec<String> = t.strategies.iter().map(|&[r, p, d, g, size, features, needed, truncated]| {
            let mut values = Vec::new();
            for (key, value) in [("dropped_by_rate", r), ("dropped_by_gamma", g), (if opts.cluster_distance != 0 || b"CSND".iter().any(|&k| opts.additional[k as usize]) { "coalesced_as_needed" } else { "dropped_as_needed" }, needed), ("detail_reduced", d), ("tiny_polygons", p), ("tile_size_desired", size), ("feature_count_desired", features), ("truncated_zooms", truncated)] {
                if value > 0 { values.push(format!("\"{key}\":{value}")); }
            }
            format!("{{{}}}", values.join(","))
        }).collect();
        format!("[{}]", zs.join(","))
    });

    let vl = vector_layers(&st, opts.minzoom as u8, maxz, layer_descriptions, &opts.attribute_descriptions);
    // No tilestats with `-pg` or `--tile-stats-values-limit=0`.
    let json = if lim().tilestats && lim().values > 0 { format!("{{\"vector_layers\":{vl},\"tilestats\":{}}}", tilestats(st)) } else { format!("{{\"vector_layers\":{vl}}}") };
    let mut meta: vtmake_out::Metadata = vec![
        ("name".into(), name),
        ("description".into(), description),
        ("version".into(), "2".into()),
        ("minzoom".into(), (opts.minzoom as u8).to_string()),
        ("maxzoom".into(), maxz.to_string()),
        ("center".into(), format!("{:.6},{:.6},{maxz}", sixdig(midlon), sixdig(midlat))),
        ("bounds".into(), f4(minlon, minlat, maxlon, maxlat)),
        ("antimeridian_adjusted_bounds".into(), f4(minlon2, minlat2, maxlon2, maxlat2)),
        ("type".into(), "overlay".into()),
    ];
    if let Some(a) = opts.attribution.as_ref().filter(|a| !a.is_empty()) {
        meta.push(("attribution".into(), a.clone()));
    }
    meta.extend([
        ("format".into(), "pbf".into()),
        ("generator".into(), format!("vtmake v{}", env!("CARGO_PKG_VERSION"))),
        ("generator_options".into(), format_commandline(&opts.argv)),
    ]);
    if let Some(s) = strategies {
        meta.push(("strategies".into(), s));
    }
    // `make_metadata`: basezoom is `-z`; it differs from the metadata maxzoom when
    // no feature reaches `-z` (spec 5.5).
    if opts.basezoom != maxz as i32 || opts.droprate != 2.5 || opts.retain_points_multiplier != 1 {
        meta.push(("tippecanoe_decisions".into(), format!("{{\"basezoom\":{},\"droprate\":{},\"retain_points_multiplier\":{}}}", opts.basezoom, opts.droprate, opts.retain_points_multiplier)));
    }
    meta.push(("json".into(), json));
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commandline_quotes() {
        let a: Vec<String> = ["tippecanoe", "-Z9", "--buffer=5", "--layer", "it's", "a b", "", "/x/y.ndjson"].iter().map(|s| s.to_string()).collect();
        assert_eq!(format_commandline(&a), "tippecanoe -Z9 '--buffer=5' --layer 'it'\"'\"'s' 'a b'  /x/y.ndjson");
    }

    #[test]
    fn truncate16_units() {
        let s = "a".repeat(256);
        assert_eq!(truncate16(&s), s);
        assert_eq!(truncate16(&"a".repeat(257)).len(), 256);
        // A character outside the BMP is two UTF-16 units.
        let e = format!("{}😀", "a".repeat(255));
        assert_eq!(truncate16(&e), "a".repeat(255));
        assert_eq!(truncate16("ab\0c"), "ab");
    }

    #[test]
    fn samples_keep_the_smallest() {
        let mut a = AttrStats::default();
        let mk = |r: std::ops::Range<u32>| AttrStats {
            samples: r.map(|i| (format!("{i:05}").into(), AttrType::String)).collect(),
            ..Default::default()
        };
        a.merge(mk(500..1500));
        a.merge(mk(0..600));
        assert_eq!(a.samples.len(), 1000);
        assert_eq!(&*a.samples[0].0, "00000");
        assert_eq!(&*a.samples[999].0, "00999");
    }
}
