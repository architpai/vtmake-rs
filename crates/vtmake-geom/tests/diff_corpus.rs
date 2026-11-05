//! Differential tests with real geometry from the public corpus (the files
//! `points.ndjson`, `polygons.ndjson` and `lines.ndjson` in the directory
//! `VTMAKE_CORPUS_DIR`, and other NDJSON files in `VTMAKE_EXTRA_CORPUS`, a
//! list separated by commas). The files are read in place and are not part
//! of the repository. A test does nothing if its variable is not set or its
//! file is missing.
//!
//! For each feature: project all vertices (Rust and C++), fix polygons,
//! make the index, then go down a chain of tiles from a random start zoom
//! to z18..z20. At each tile: `clip_to_tile`, `child_tile_range`, and all
//! per-tile steps (tiny polygons with a shared accumulator,
//! simplification, scale, revive, close). Polygon cleaning comes from the
//! C++ code. `VTMAKE_CORPUS_LIMIT` sets the number of features for each file
//! (default 20000, 0 = all).

#![cfg(feature = "cpp-ref")]

mod common;

use vtmake_core::{Draw, DrawVec, GeomType};
use vtmake_geom::cpp_ref as cpp;
use vtmake_geom::*;
use common::*;
use std::cell::RefCell;

const BUFFER: i64 = 5;
const DETAIL: u32 = 12;

thread_local! {
    // One tiny polygon accumulator for each zoom (Rust, C++), shared by
    // the features that this thread processes (like the features of a
    // tile).
    static ACCUM: RefCell<([TinyPolyAccum; 33], [f64; 33])> =
        const { RefCell::new(([TinyPolyAccum { area: 0.0 }; 33], [0.0; 33])) };
    static SCRATCH: RefCell<GeomScratch> = RefCell::new(GeomScratch::default());
}

struct Tallies {
    proj: Tally,
    fix: Tally,
    index: Tally,
    clip: Tally,
    range: Tally,
    steps: Tally,
}

impl Tallies {
    const fn new() -> Self {
        Tallies {
            proj: Tally::new("corpus lonlat2tile"),
            fix: Tally::new("corpus fix_polygon"),
            index: Tally::new("corpus feature_index"),
            clip: Tally::new("corpus clip_to_tile"),
            range: Tally::new("corpus child_tile_range"),
            steps: Tally::new("corpus tile steps"),
        }
    }
    fn report(&self) {
        for t in [
            &self.proj,
            &self.fix,
            &self.index,
            &self.clip,
            &self.range,
            &self.steps,
        ] {
            t.report();
        }
    }
}

fn project(f: &LonLatFeature, t: &Tallies) -> DrawVec {
    let mut g = Vec::with_capacity(f.draws.len());
    for &(op, lon, lat) in &f.draws {
        if op == vtmake_core::VT_CLOSEPATH {
            g.push(Draw::new(op, 0, 0));
            continue;
        }
        let r = lonlat_to_world(lon, lat);
        t.proj
            .check(&r, &cpp::lonlat2tile(lon, lat, 32), &(lon, lat));
        g.push(Draw::new(op, r.0, r.1));
    }
    g
}

fn steps(
    clipped: &DrawVec,
    gt: GeomType,
    z: u32,
    t: &Tallies,
    sc: &mut GeomScratch,
    input: &dyn std::fmt::Debug,
) {
    ACCUM.with(|a| {
        let mut a = a.borrow_mut();
        let (ar, ac) = &mut *a;
        let c = cpp::tile_steps(clipped, gt, z, DETAIL, 1.0, 2.0, &mut ac[z as usize]);

        let mut g = clipped.clone();
        let red = tile_reduce(&mut g, gt, z, DETAIL, 2.0, &mut ar[z as usize], sc);
        let mut pre_clean = Vec::new();
        let mut fin = Vec::new();
        if red.keep {
            let pre = tile_simplify(&mut g, gt, z, DETAIL, 1.0, red.reduced, sc);
            pre_clean = g.clone();
            match gt {
                GeomType::Polygon => {
                    let mut cleaned = c.cleaned.clone();
                    tile_finish_polygon(&mut cleaned, &pre, z, DETAIL);
                    fin = cleaned;
                }
                GeomType::Line => {
                    tile_finish_line(&mut g);
                    fin = g;
                }
                GeomType::Point => fin = g,
            }
        }
        t.steps.check(
            &(
                red.keep,
                red.tiny_polygon,
                pre_clean,
                fin,
                ar[z as usize].area.to_bits(),
            ),
            &(
                c.kept,
                c.tiny_counted,
                c.pre_clean,
                c.fin,
                ac[z as usize].to_bits(),
            ),
            input,
        );
    });
}

fn cascade(n: usize, f: &LonLatFeature, t: &Tallies) {
    let mut rng = Rng::new(n as u64);
    let mut g = project(f, t);
    let gt = f.t;
    if gt == GeomType::Polygon {
        let mut fixed = Vec::new();
        fix_polygon(&g, &mut fixed);
        t.fix.check(&fixed, &cpp::fix_polygon(&g), &n);
        g = fixed;
    }
    if g.is_empty() {
        return;
    }
    let bbox = geometry_bbox(&g);
    t.index.check(
        &feature_index(gt, &g, &bbox),
        &cpp::feature_index(gt, &g, &bbox),
        &n,
    );

    // Start zoom and tile: the tile of a random vertex.
    let maxz = 18 + rng.below(3) as u32;
    let mut z = rng.below(15) as u32;
    let v = g[rng.below(g.len() as u64) as usize];
    let (mut tx, mut ty) = if z == 0 {
        (0, 0)
    } else {
        ((v.x >> (32 - z)) as u32, (v.y >> (32 - z)) as u32)
    };

    SCRATCH.with(|s| {
        let mut sc = s.borrow_mut();
        loop {
            let c = cpp::clip_to_tile(&g, gt, z, tx, ty, BUFFER);
            let mut rg = g.clone();
            let r = clip_to_tile(&mut rg, gt, z, tx, ty, BUFFER, &mut sc).map(|b| (rg, b));
            t.clip.check(&r, &c, &(n, z, tx, ty));
            let Some((clipped, cbbox)) = r else { return };
            if clipped.is_empty() {
                return;
            }
            steps(&clipped, gt, z, t, &mut sc, &(n, z, tx, ty));
            if z >= maxz {
                return;
            }

            // The next zoom: one child of the candidate range. Mostly the
            // child under a vertex, sometimes a random candidate (often a
            // buffer-only copy).
            let nz = z + 1;
            let range = child_tile_range(&cbbox, z, nz, tx, ty, BUFFER);
            t.range.check(
                &range,
                &cpp::child_range(&cbbox, z, nz, tx, ty, BUFFER),
                &(n, z),
            );
            let (x0, y0, x1, y1) = range;
            let (ox, oy) = tile_origin(z, tx, ty);
            let (cx, cy) = if rng.chance(0.75) {
                let v = clipped[rng.below(clipped.len() as u64) as usize];
                let ax = v.x + ox;
                let ay = v.y + oy;
                let cx = ((ax.max(0) >> (32 - nz)) as u32).clamp(x0, x1);
                let cy = ((ay.max(0) >> (32 - nz)) as u32).clamp(y0, y1);
                (cx, cy)
            } else {
                (
                    x0 + rng.below((x1 - x0 + 1) as u64) as u32,
                    y0 + rng.below((y1 - y0 + 1) as u64) as u32,
                )
            };

            // tippecanoe writes the clipped geometry to the child in
            // absolute coordinates.
            g = clipped;
            for d in &mut g {
                d.x += ox;
                d.y += oy;
            }
            z = nz;
            tx = cx;
            ty = cy;
        }
    });
}

fn run(path: &str) {
    let t = Tallies::new();
    let limit = corpus_limit();
    match for_each_feature(path, limit, |n, f| cascade(n, f, &t)) {
        None => eprintln!("SKIP {path}: file not found"),
        Some(count) => {
            eprintln!("CORPUS {path}: {count} features");
            t.report();
        }
    }
}

#[test]
fn corpus_public_points() {
    if let Some(path) = pub_corpus(PUB_POINTS) {
        run(&path);
    }
}

#[test]
fn corpus_public_polygons() {
    if let Some(path) = pub_corpus(PUB_POLYGONS) {
        run(&path);
    }
}

#[test]
fn corpus_public_lines() {
    if let Some(path) = pub_corpus(PUB_LINES) {
        run(&path);
    }
}

#[test]
fn corpus_extra() {
    if let Ok(list) = std::env::var("VTMAKE_EXTRA_CORPUS") {
        for p in list.split(',').filter(|p| !p.is_empty()) {
            run(p);
        }
    }
}

/// The libm question of spec 4.1: projection of every point of the public
/// point corpus, Rust against C++. `VTMAKE_CORPUS_LIMIT` applies.
#[test]
fn corpus_projection_points() {
    let Some(path) = pub_corpus(PUB_POINTS) else { return };
    let t = Tally::new("all-points lonlat2tile");
    let limit = corpus_limit();
    let r = for_each_feature(&path, limit, |_, f| {
        for &(op, lon, lat) in &f.draws {
            if op != vtmake_core::VT_CLOSEPATH {
                t.check(
                    &lonlat_to_world(lon, lat),
                    &cpp::lonlat2tile(lon, lat, 32),
                    &(lon, lat),
                );
            }
        }
    });
    if r.is_some() {
        t.report();
    }
}
