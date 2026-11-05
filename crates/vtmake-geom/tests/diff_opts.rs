//! Differential tests of the geometry options with random
//! input: each Rust function against the real tippecanoe C++ function with
//! the same `prevent[]` and `additional[]` flags. Run with
//! `cargo test -p vtmake-geom --features cpp-ref --release -- --nocapture`.
//! `VTMAKE_DIFF_SCALE` multiplies the case counts.

#![cfg(feature = "cpp-ref")]

mod common;

use common::*;
use rayon::prelude::*;
use vtmake_core::GeomType;
use vtmake_geom::cpp_ref as cpp;
use vtmake_geom::*;

const BUFFER: i64 = 5;

fn cases<F>(seed: u64, n: u64, f: F)
where
    F: Fn(&mut Rng) + Sync,
{
    let chunk = 2000u64;
    (0..n.div_ceil(chunk)).into_par_iter().for_each(|c| {
        let mut rng = Rng::new(seed.wrapping_mul(1_000_003).wrapping_add(c));
        for _ in 0..chunk.min(n - c * chunk) {
            f(&mut rng);
        }
    });
}

/// Random geometry options.
fn rand_opts(rng: &mut Rng, maxzoom: u32) -> GeomOpts {
    GeomOpts {
        maxzoom,
        no_simplify: rng.chance(0.15),
        no_simplify_maxzoom: rng.chance(0.3),
        no_tiny: false,
        no_tiny_maxzoom: false,
        grid_low_zooms: rng.chance(0.3),
        visvalingam: rng.chance(0.4),
        no_clipping: rng.chance(0.3),
        no_duplication: rng.chance(0.3),
        shared_nodes: rng.chance(0.3),
        shared_borders: rng.chance(0.2),
    }
}

#[test]
fn visvalingam_random() {
    let t = Tally::new("visvalingam");
    cases(41, scale(300_000), |rng| {
        let z = rng.below(21) as u32;
        let l = if rng.chance(0.5) { line(rng, z, BUFFER) } else { polygon(rng, z, BUFFER) };
        let j = polygon::part_end(&l, 0);
        if j < 2 {
            return;
        }
        let mut g = l.clone();
        for d in &mut g {
            d.necessary = rng.chance(0.05);
        }
        let threshold = if rng.chance(0.1) {
            0.0
        } else {
            let res = (1i64 << (32 - 12 - z)) as f64;
            res * res * rng.f64() * 16.0
        };
        let retain = if rng.chance(0.5) { 0 } else { rng.below(6) as usize };
        let c = cpp::visvalingam(&g, 0, j, threshold, retain);
        visvalingam(&mut g, 0, j, threshold, retain);
        t.check(&g, &c, &(&l, threshold, retain));
    });
    t.report();
}

#[test]
fn stairstep_random() {
    let t = Tally::new("stairstep");
    cases(42, scale(200_000), |rng| {
        let z = rng.below(19) as u32;
        let detail = 5 + rng.below(8) as u32;
        let g = if rng.chance(0.5) { line(rng, z, BUFFER) } else { polygon(rng, z, BUFFER) };
        if g.first().is_some_and(|d| d.op != vtmake_core::VT_MOVETO) {
            return;
        }
        // Keep the steps few: the points within 64 tile units.
        let unit = 1i64 << (32 - detail - z);
        let mut g = g;
        for d in &mut g {
            d.x = d.x.rem_euclid(64 * unit);
            d.y = d.y.rem_euclid(64 * unit);
        }
        let c = cpp::stairstep(&g, z, detail);
        let mut r = g.clone();
        let mut out = Vec::new();
        stairstep(&mut r, z, detail, &mut out);
        t.check(&r, &c, &(&g, z, detail));
    });
    t.report();
}

#[test]
fn check_polygon_random() {
    let tp = Tally::new("pnpoly");
    let tc = Tally::new("check_polygon geometry");
    cases(43, scale(200_000), |rng| {
        let z = rng.below(21) as u32;
        let g = polygon(rng, z, BUFFER);
        let j = polygon::part_end(&g, 0);
        let (x, y) = (coord(rng, z, BUFFER), coord(rng, z, BUFFER));
        tp.check(&polygon::pnpoly(&g, 0, j, x, y), &cpp::pnpoly(&g, 0, j, x, y), &(&g, x, y));
        // Tile units, as after the polygon cleaning.
        let mut t: Vec<_> = g.clone();
        to_tile_scale(&mut t, z, 12);
        let c = cpp::check_polygon(&t);
        let mut r = t.clone();
        remove_noop(&mut r, GeomType::Polygon, 0);
        tc.check(&r, &c, &t);
    });
    tp.report();
    tc.report();
}

#[test]
fn simplify_opts_random() {
    let tl = Tally::new("simplify_lines opts");
    let tf = Tally::new("simplify_feature opts");
    cases(44, scale(400_000), |rng| {
        let z = rng.below(21) as u32;
        let maxzoom = (z + rng.below(3) as u32).min(20);
        let o = rand_opts(rng, maxzoom);
        let detail = if rng.chance(0.7) { 12 } else { 6 + rng.below(7) as u32 };
        let detail = detail.min(32 - z);
        let t = rand_type(rng);
        let g = geom_of(rng, t, z, BUFFER);
        let simp = if rng.chance(0.6) { 1.0 } else { rng.f64() * 8.0 };
        let retain = if t == GeomType::Polygon { 4 } else { 0 };
        let mark = rng.chance(0.8);
        let mut sc = SimplifyScratch::default();
        let mut r = g.clone();
        simplify_lines_opts(&mut r, z, detail, mark, simp, retain, &o, &NodeCtx::EMPTY, &mut sc);
        tl.check(&r, &cpp::simplify_lines_opts(&g, z, detail, mark, simp, retain, &o), &(&g, z, detail, &o));

        let reduced = rng.chance(0.2);
        let mut r = g.clone();
        let a = simplify_feature_opts(&mut r, t, z, detail, simp, reduced, &o, &NodeCtx::EMPTY, &mut sc);
        let (cg, ca) = cpp::simplify_feature_opts(&g, t, z, detail, simp, reduced, &o);
        tf.check(&(r, a.to_bits()), &(cg, ca.to_bits()), &(&g, t, z, detail, reduced, &o));
    });
    tl.report();
    tf.report();
}

#[test]
fn clip_opts_random() {
    let t = Tally::new("clip_to_tile opts");
    cases(45, scale(600_000), |rng| {
        let z = if rng.chance(0.1) { 0 } else { rng.below(21) as u32 };
        let n = 1u64 << z;
        let tx = rng.below(n) as u32;
        let ty = rng.below(n) as u32;
        let gt = rand_type(rng);
        let o = rand_opts(rng, 20);
        let (ox, oy) = tile_origin(z, tx, ty);
        let mut g = geom_of(rng, gt, z, BUFFER);
        for d in &mut g {
            d.x += ox;
            d.y += oy;
        }
        let c = cpp::clip_to_tile_opts(&g, gt, z, tx, ty, BUFFER, &o);
        let mut rg = g.clone();
        let mut sc = GeomScratch::default();
        let r = clip_to_tile_opts(&mut rg, gt, z, tx, ty, BUFFER, &o, &mut sc).map(|b| (rg, b, if o.shared_nodes { sc.edge_nodes.clone() } else { Vec::new() }));
        t.check(&r, &c, &(&g, gt, z, tx, ty, &o));
    });
    t.report();
}

#[test]
fn label_points_random() {
    let tg = Tally::new("label_goodness");
    let ta = Tally::new("polygon_to_anchor");
    let tc = Tally::new("checkerboard_anchors");
    cases(46, scale(100_000), |rng| {
        let z = rng.below(21) as u32;
        let n = 1u64 << z;
        let (tx, ty) = (rng.below(n) as u32, rng.below(n) as u32);
        // World coordinates of a polygon after fix_polygon (as at read time).
        let (ox, oy) = tile_origin(z, tx, ty);
        let mut raw = if rng.chance(0.8) { polygon(rng, z, BUFFER) } else { parsed_polygon(rng, z, BUFFER) };
        for d in &mut raw {
            if d.op != vtmake_core::VT_CLOSEPATH {
                d.x += ox;
                d.y += oy;
            }
        }
        let mut g = Vec::new();
        fix_polygon(&raw, &mut g);
        if g.is_empty() {
            return;
        }
        let (x, y) = (coord(rng, z, BUFFER) + ox, coord(rng, z, BUFFER) + oy);
        tg.check(&label::label_goodness(&g, x, y).to_bits(), &cpp::label_goodness(&g, x, y).to_bits(), &(&g, x, y));
        let r = label::polygon_to_anchor(&g).map(|(x, y)| vec![vtmake_core::Draw::new(vtmake_core::VT_MOVETO, x, y)]).unwrap_or_default();
        let c = cpp::polygon_to_anchor(&g);
        ta.check(&r, &c, &g);
        // The tile step: tile-relative geometry and the anchor quadkey.
        let Some(a) = r.first() else { return };
        let lp = encode_quadkey((a.x & 0xFFFF_FFFF) as u32, (a.y & 0xFFFF_FFFF) as u32);
        let mut rel = g.clone();
        to_tile_relative(&mut rel, z, tx, ty);
        let mut out = Vec::new();
        label::checkerboard_anchors(&rel, tx, ty, z, lp, &mut out);
        tc.check(&out, &cpp::checkerboard_anchors(&rel, tx, ty, z, lp), &(&rel, tx, ty, z, lp));
    });
    tg.report();
    ta.report();
    tc.report();
}

#[test]
fn shared_nodes_random() {
    let t = Tally::new("simplify_lines nodes");
    cases(47, scale(200_000), |rng| {
        let z = rng.below(21) as u32;
        let n = 1u64 << z;
        let (tx, ty) = (rng.below(n) as u32, rng.below(n) as u32);
        let mut o = rand_opts(rng, 20);
        o.shared_nodes = true;
        let gt = if rng.chance(0.5) { GeomType::Line } else { GeomType::Polygon };
        let g = geom_of(rng, gt, z, BUFFER);
        // Tile nodes: some points of the geometry and some random points;
        // global nodes: the keys of some other points.
        let pick = |rng: &mut Rng| {
            let d = g[rng.below(g.len().max(1) as u64) as usize];
            vtmake_core::Draw::new(vtmake_core::VT_MOVETO, d.x, d.y)
        };
        let mut tile: Vec<_> = Vec::new();
        let mut global: Vec<u64> = Vec::new();
        if !g.is_empty() {
            for _ in 0..rng.below(6) {
                tile.push(pick(rng));
            }
            let (ox, oy) = tile_origin(z, tx, ty);
            for _ in 0..rng.below(6) {
                let d = pick(rng);
                global.push(encode_vertex((d.x + ox) as u32, (d.y + oy) as u32));
            }
        }
        if rng.chance(0.7) {
            tile.sort_by_key(|d| (d.y, d.x));
        }
        global.sort_unstable();
        global.dedup();
        let detail = 12u32.min(32 - z);
        let simp = if rng.chance(0.6) { 1.0 } else { rng.f64() * 8.0 };
        let retain = if gt == GeomType::Polygon { 4 } else { 0 };
        let mark = rng.chance(0.8);
        let nodes = NodeCtx { tile: &tile, global: &global, tx, ty };
        let mut r = g.clone();
        let mut sc = SimplifyScratch::default();
        simplify_lines_opts(&mut r, z, detail, mark, simp, retain, &o, &nodes, &mut sc);
        let c = cpp::simplify_lines_nodes(&g, z, tx, ty, detail, mark, simp, retain, &tile, &global, &o);
        t.check(&r, &c, &(&g, z, tx, ty, &tile, &global, &o));
    });
    t.report();
}

/// A grid corner (column, row).
type Corner = (usize, usize);

#[test]
fn shared_borders_random() {
    let t = Tally::new("find_common_edges");
    cases(48, scale(20_000), |rng| {
        let z = rng.below(21) as u32;
        let area = 1i64 << (32 - z);
        let detail = 12u32.min(32 - z);
        let mz = z + rng.below(2) as u32;
        let o = rand_opts(rng, mz);
        // A jittered grid of cells: neighbours share their edges (and the
        // points on them).
        let (nx, ny) = (1 + rng.below(4) as usize, 1 + rng.below(4) as usize);
        let step = (area / (1 + rng.below(8) as i64)).max(16);
        let jit = (step / 4).max(1);
        let mut pts = vec![vec![(0i64, 0i64); ny + 1]; nx + 1];
        for (i, col) in pts.iter_mut().enumerate() {
            for (j, p) in col.iter_mut().enumerate() {
                *p = (i as i64 * step + rng.range(-jit, jit), j as i64 * step + rng.range(-jit, jit));
            }
        }
        // Extra points on the edges (the same for both cells of an edge).
        let mut mids: std::collections::HashMap<(Corner, Corner), Vec<(i64, i64)>> = Default::default();
        let mut edge_pts = |a: (usize, usize), b: (usize, usize), rng: &mut Rng| -> Vec<(i64, i64)> {
            let (k, rev) = if a < b { ((a, b), false) } else { ((b, a), true) };
            let v = mids
                .entry(k)
                .or_insert_with(|| {
                    let (p, q) = (pts[k.0.0][k.0.1], pts[k.1.0][k.1.1]);
                    let m = rng.below(4);
                    (1..=m).map(|s| (p.0 + (q.0 - p.0) * s as i64 / (m as i64 + 1) + rng.range(-2, 2), p.1 + (q.1 - p.1) * s as i64 / (m as i64 + 1) + rng.range(-2, 2))).collect()
                })
                .clone();
            if rev { v.into_iter().rev().collect() } else { v }
        };
        let mut polys: Vec<vtmake_core::DrawVec> = Vec::new();
        for i in 0..nx {
            for j in 0..ny {
                if rng.chance(0.15) {
                    continue;
                }
                let corners = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                let mut ring: Vec<(i64, i64)> = Vec::new();
                for c in 0..4 {
                    ring.push(pts[corners[c].0][corners[c].1]);
                    ring.extend(edge_pts(corners[c], corners[(c + 1) % 4], rng));
                }
                if rng.chance(0.3) {
                    ring.reverse();
                }
                let rot = rng.below(ring.len() as u64) as usize;
                ring.rotate_left(rot);
                if rng.chance(0.2) {
                    // A repeated point.
                    let k = rng.below(ring.len() as u64) as usize;
                    let p = ring[k];
                    ring.insert(k, p);
                }
                ring.push(ring[0]);
                let mut g = Vec::new();
                push_ring(&mut g, &ring);
                if rng.chance(0.3) && !polys.is_empty() {
                    polys.last_mut().unwrap().extend(g);
                } else {
                    polys.push(g);
                }
            }
        }
        if polys.is_empty() {
            return;
        }
        let simp = if rng.chance(0.6) { 1.0 } else { rng.f64() * 8.0 };
        let c = cpp::find_common_edges(&polys, z, detail, simp, &o);
        let mut r = polys.clone();
        let mut sc = SimplifyScratch::default();
        shared_borders::find_common_edges(&mut r, z, detail, simp, &o, &mut sc);
        t.check(&r, &c, &(&polys, z, simp, &o));
    });
    t.report();
}
