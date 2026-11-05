//! Helpers for the differential tests: a seeded random generator, random
//! geometry, a small GeoJSON coordinate reader, and case counters.

#![allow(dead_code)]

use vtmake_core::{Draw, DrawVec, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};

/// The number of random cases is `base * VTMAKE_DIFF_SCALE` (default 1).
pub fn scale(base: u64) -> u64 {
    let s: f64 = std::env::var("VTMAKE_DIFF_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    ((base as f64) * s).max(1.0) as u64
}

/// The maximum number of corpus features for each file
/// (`VTMAKE_CORPUS_LIMIT`, default 20000; 0 means all).
pub fn corpus_limit() -> usize {
    std::env::var("VTMAKE_CORPUS_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20000)
}

/// SplitMix64.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[0, n)`.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
    /// Uniform in `[lo, hi]`.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }
}

/// A coordinate near tile edges and buffer edges at zoom `z`, or a random
/// value in `[-2 area, 3 area]` (tile-relative).
pub fn coord(rng: &mut Rng, z: u32, buffer: i64) -> i64 {
    let area = 1i64 << (32 - z);
    let b = buffer * area / 256;
    match rng.below(10) {
        0..=3 => {
            let specials = [0, area, -b, area + b, area / 2];
            specials[rng.below(5) as usize] + rng.range(-3, 3)
        }
        4..=6 => rng.range(-b - b / 2, area + b + b / 2),
        _ => rng.range(-2 * area, 3 * area),
    }
}

/// A random ring (closed with probability 0.8) with points near `center`.
pub fn ring(rng: &mut Rng, z: u32, buffer: i64, max_pts: u64) -> Vec<(i64, i64)> {
    let n = 1 + rng.below(max_pts) as usize;
    let mut pts = Vec::with_capacity(n + 1);
    let mode = rng.below(4);
    let area = 1i64 << (32 - z);
    let cx = coord(rng, z, buffer);
    let cy = coord(rng, z, buffer);
    let r = 1i64 << rng.below(34 - z as u64 + 1).min(33);
    for k in 0..n {
        let p = match mode {
            // Points on a circle (simple ring).
            0 => {
                let ang = k as f64 / n as f64 * std::f64::consts::TAU;
                (
                    cx + (ang.cos() * r as f64) as i64,
                    cy + (ang.sin() * r as f64) as i64,
                )
            }
            // Random points near the center (can self-intersect).
            1 => (cx + rng.range(-r, r), cy + rng.range(-r, r)),
            // Coordinates on special values.
            2 => (coord(rng, z, buffer), coord(rng, z, buffer)),
            // Small: a few world units, with repeats.
            _ => (
                cx + rng.range(-2, 2) * (area >> 12).max(1),
                cy + rng.range(-2, 2) * (area >> 12).max(1),
            ),
        };
        pts.push(p);
        if rng.chance(0.05) {
            pts.push(p);
        }
    }
    if rng.chance(0.8) {
        pts.push(pts[0]);
    }
    pts
}

pub fn push_ring(g: &mut DrawVec, pts: &[(i64, i64)]) {
    for (k, &(x, y)) in pts.iter().enumerate() {
        g.push(Draw::new(if k == 0 { VT_MOVETO } else { VT_LINETO }, x, y));
    }
}

/// A random polygon geometry as `fix_polygon` makes it (no CLOSEPATH).
pub fn polygon(rng: &mut Rng, z: u32, buffer: i64) -> DrawVec {
    let mut g = Vec::new();
    let rings = 1 + rng.below(4);
    for _ in 0..rings {
        let r = ring(rng, z, buffer, 40);
        push_ring(&mut g, &r);
    }
    g
}

/// A random parsed polygon (rings with CLOSEPATH after each polygon).
pub fn parsed_polygon(rng: &mut Rng, z: u32, buffer: i64) -> DrawVec {
    let mut g = Vec::new();
    let polys = 1 + rng.below(3);
    for _ in 0..polys {
        let rings = 1 + rng.below(3);
        for _ in 0..rings {
            let r = ring(rng, z, buffer, 30);
            push_ring(&mut g, &r);
        }
        g.push(Draw::new(VT_CLOSEPATH, 0, 0));
    }
    g
}

/// A random line geometry (one or more parts).
pub fn line(rng: &mut Rng, z: u32, buffer: i64) -> DrawVec {
    let mut g = Vec::new();
    let parts = 1 + rng.below(3);
    for _ in 0..parts {
        let mut r = ring(rng, z, buffer, 40);
        if r.len() > 1 && rng.chance(0.5) {
            r.pop();
        }
        push_ring(&mut g, &r);
    }
    g
}

/// Random points (MultiPoint: all MOVETO).
pub fn points(rng: &mut Rng, z: u32, buffer: i64) -> DrawVec {
    let n = 1 + rng.below(5);
    (0..n)
        .map(|_| Draw::new(VT_MOVETO, coord(rng, z, buffer), coord(rng, z, buffer)))
        .collect()
}

/// A random geometry with random ops (also CLOSEPATH and stray LINETOs)
/// and random `necessary` flags, for the functions that must accept any
/// draw list.
pub fn soup(rng: &mut Rng, z: u32, buffer: i64) -> DrawVec {
    let n = rng.below(30) as usize;
    let mut g = Vec::with_capacity(n);
    let mut last = (coord(rng, z, buffer), coord(rng, z, buffer));
    for _ in 0..n {
        let op = match rng.below(10) {
            0..=2 => VT_MOVETO,
            3..=8 => VT_LINETO,
            _ => VT_CLOSEPATH,
        };
        let p = if rng.chance(0.3) {
            last
        } else {
            (
                last.0 + rng.range(-3000, 3000),
                last.1 + rng.range(-3000, 3000),
            )
        };
        let p = if rng.chance(0.2) {
            (coord(rng, z, buffer), coord(rng, z, buffer))
        } else {
            p
        };
        last = p;
        let (x, y) = if op == VT_CLOSEPATH { (0, 0) } else { p };
        let mut d = Draw::new(op, x, y);
        d.necessary = rng.chance(0.3);
        g.push(d);
    }
    g
}

/// A geometry of type `t`.
pub fn geom_of(rng: &mut Rng, t: GeomType, z: u32, buffer: i64) -> DrawVec {
    match t {
        GeomType::Point => points(rng, z, buffer),
        GeomType::Line => line(rng, z, buffer),
        GeomType::Polygon => polygon(rng, z, buffer),
    }
}

pub fn rand_type(rng: &mut Rng) -> GeomType {
    match rng.below(3) {
        0 => GeomType::Point,
        1 => GeomType::Line,
        _ => GeomType::Polygon,
    }
}

/// Counts cases and mismatches; prints a line at the end of a test.
pub struct Tally {
    pub name: &'static str,
    pub cases: AtomicU64,
    pub mismatches: AtomicU64,
}

impl Tally {
    pub const fn new(name: &'static str) -> Self {
        Tally {
            name,
            cases: AtomicU64::new(0),
            mismatches: AtomicU64::new(0),
        }
    }
    /// Record one case. On a mismatch, print the details (the first 10).
    pub fn check<T: PartialEq + std::fmt::Debug>(
        &self,
        rust: &T,
        cpp: &T,
        input: &dyn std::fmt::Debug,
    ) {
        self.cases.fetch_add(1, Ordering::Relaxed);
        if rust != cpp {
            let m = self.mismatches.fetch_add(1, Ordering::Relaxed);
            if m < 10 {
                eprintln!(
                    "MISMATCH {}: input {:?}\n  rust {:?}\n  cpp  {:?}",
                    self.name, input, rust, cpp
                );
            }
        }
    }
    pub fn report(&self) {
        let c = self.cases.load(Ordering::Relaxed);
        let m = self.mismatches.load(Ordering::Relaxed);
        eprintln!("DIFF {:<28} cases {:>12}  mismatches {}", self.name, c, m);
        assert_eq!(m, 0, "{}: {} mismatches in {} cases", self.name, m, c);
    }
}

// ---- GeoJSON reader ----

/// One parsed feature: the geometry type and the draw list in tippecanoe's
/// parse format (`read_json.cpp:parse_coordinates`), in (lon, lat).
pub struct LonLatFeature {
    pub t: GeomType,
    /// (op, lon, lat); CLOSEPATH has (0, 0).
    pub draws: Vec<(u8, f64, f64)>,
}

#[derive(Debug)]
enum Node {
    Num(f64),
    Arr(Vec<Node>),
}

fn parse_node(s: &[u8], i: &mut usize) -> Option<Node> {
    while *i < s.len() && s[*i].is_ascii_whitespace() {
        *i += 1;
    }
    if *i >= s.len() {
        return None;
    }
    if s[*i] == b'[' {
        *i += 1;
        let mut v = Vec::new();
        loop {
            while *i < s.len() && (s[*i].is_ascii_whitespace() || s[*i] == b',') {
                *i += 1;
            }
            if *i >= s.len() {
                return None;
            }
            if s[*i] == b']' {
                *i += 1;
                return Some(Node::Arr(v));
            }
            v.push(parse_node(s, i)?);
        }
    }
    let start = *i;
    while *i < s.len() && matches!(s[*i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
        *i += 1;
    }
    std::str::from_utf8(&s[start..*i])
        .ok()?
        .parse()
        .ok()
        .map(Node::Num)
}

fn emit(n: &Node, depth: u32, t: &str, out: &mut Vec<(u8, f64, f64)>, op: u8) {
    // depth: 0 = position, 1 = list of positions, ...
    match (n, depth) {
        (Node::Arr(v), 0) => {
            if let (Some(Node::Num(x)), Some(Node::Num(y))) = (v.first(), v.get(1)) {
                out.push((op, *x, *y));
            }
        }
        (Node::Arr(v), 1) => {
            for (k, p) in v.iter().enumerate() {
                let op = if t == "MultiPoint" || k == 0 {
                    VT_MOVETO
                } else {
                    VT_LINETO
                };
                emit(p, 0, t, out, op);
            }
        }
        (Node::Arr(v), d) => {
            for p in v {
                emit(p, d - 1, t, out, op);
            }
            // A Polygon (a list of rings) ends with CLOSEPATH.
            if d == 2 && (t == "Polygon" || t == "MultiPolygon") {
                out.push((VT_CLOSEPATH, 0.0, 0.0));
            }
        }
        _ => {}
    }
}

/// Parse the geometry of one GeoJSON feature line (simple string search).
pub fn parse_feature(line: &str) -> Option<LonLatFeature> {
    let g = line.find("\"geometry\"")?;
    let rest = &line[g..];
    let tpos = rest.find("\"type\"")?;
    let after = &rest[tpos + 6..];
    let q1 = after.find('"')?;
    let q2 = after[q1 + 1..].find('"')?;
    let ty = &after[q1 + 1..q1 + 1 + q2];
    let cpos = rest.find("\"coordinates\"")?;
    let bytes = rest.as_bytes();
    let mut i = cpos + 13;
    while i < bytes.len() && bytes[i] != b'[' {
        i += 1;
    }
    let node = parse_node(bytes, &mut i)?;
    let (t, depth) = match ty {
        "Point" => (GeomType::Point, 0),
        "MultiPoint" => (GeomType::Point, 1),
        "LineString" => (GeomType::Line, 1),
        "MultiLineString" => (GeomType::Line, 2),
        "Polygon" => (GeomType::Polygon, 2),
        "MultiPolygon" => (GeomType::Polygon, 3),
        _ => return None,
    };
    let mut draws = Vec::new();
    if depth == 0 {
        emit(&node, 0, ty, &mut draws, VT_MOVETO);
    } else {
        emit(&node, depth, ty, &mut draws, VT_MOVETO);
    }
    Some(LonLatFeature { t, draws })
}

/// Call `f` for each feature of an NDJSON file, in parallel chunks.
/// `f` gets (feature number, feature). Stops after `limit` features (0 =
/// all). Returns the number of features, or `None` if the file is missing.
pub fn for_each_feature<F>(path: &str, limit: usize, f: F) -> Option<usize>
where
    F: Fn(usize, &LonLatFeature) + Sync,
{
    use rayon::prelude::*;
    let file = std::fs::File::open(path).ok()?;
    let mut rd = BufReader::with_capacity(1 << 22, file);
    let mut count = 0usize;
    let chunk = 20000;
    loop {
        let mut lines = Vec::with_capacity(chunk);
        let mut s = String::new();
        while lines.len() < chunk {
            s.clear();
            if rd.read_line(&mut s).ok()? == 0 {
                break;
            }
            lines.push(std::mem::take(&mut s));
            if limit != 0 && count + lines.len() >= limit {
                break;
            }
        }
        if lines.is_empty() {
            break;
        }
        let base = count;
        count += lines.len();
        lines.par_iter().enumerate().for_each(|(k, l)| {
            if let Some(feat) = parse_feature(l) {
                f(base + k, &feat);
            }
        });
        if limit != 0 && count >= limit {
            break;
        }
    }
    Some(count)
}

/// The public corpus files, in the directory `VTMAKE_CORPUS_DIR`.
pub const PUB_POINTS: &str = "points.ndjson";
pub const PUB_POLYGONS: &str = "polygons.ndjson";
pub const PUB_LINES: &str = "lines.ndjson";

/// The path of the public corpus file `name` in `VTMAKE_CORPUS_DIR`, or
/// `None` (with a message) if the variable is not set.
pub fn pub_corpus(name: &str) -> Option<String> {
    match std::env::var_os("VTMAKE_CORPUS_DIR") {
        Some(dir) => Some(std::path::Path::new(&dir).join(name).to_string_lossy().into_owned()),
        None => {
            eprintln!("SKIP {name}: VTMAKE_CORPUS_DIR is not set");
            None
        }
    }
}
