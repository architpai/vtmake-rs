//! Differential tests with random input: each Rust function against the
//! real tippecanoe C++ function. Run with
//! `cargo test -p vtmake-geom --features cpp-ref --release -- --nocapture`.
//! `VTMAKE_DIFF_SCALE` multiplies the case counts.

#![cfg(feature = "cpp-ref")]

mod common;

use vtmake_core::{Draw, DrawVec, GeomType, VT_LINETO, VT_MOVETO};
use vtmake_geom::cpp_ref as cpp;
use vtmake_geom::*;
use common::*;
use rayon::prelude::*;

const BUFFER: i64 = 5;
const DETAIL: u32 = 12;

/// Run `f` for `n` cases in parallel, with a seeded generator for each
/// chunk of cases.
fn cases<F>(seed: u64, n: u64, f: F)
where
    F: Fn(&mut Rng) + Sync,
{
    let chunk = 2000u64;
    let chunks = n.div_ceil(chunk);
    (0..chunks).into_par_iter().for_each(|c| {
        let mut rng = Rng::new(seed.wrapping_mul(1_000_003).wrapping_add(c));
        let m = chunk.min(n - c * chunk);
        for _ in 0..m {
            f(&mut rng);
        }
    });
}

fn rand_z(rng: &mut Rng) -> u32 {
    rng.below(21) as u32
}

#[test]
fn numbers() {
    let t1 = Tally::new("cvt_i64");
    let t2 = Tally::new("cvt_u64");
    let specials = [
        0.0,
        -0.0,
        0.5,
        -0.5,
        -1.0,
        -5.5,
        1e18,
        9.2e18,
        f64::from_bits(9.223_372_036_854_776e18f64.to_bits() - 1),
        -9.223_372_036_854_776e18,
        -9.3e18,
        1.8e19,
        1.85e19,
        1e300,
        -1e300,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];
    for &d in &specials {
        t1.check(&num::cvt_i64(d), &cpp::cvt_i64(d), &d);
        t2.check(&num::cvt_u64(d), &cpp::cvt_u64(d), &d);
    }
    cases(1, scale(1_000_000), |rng| {
        let d = f64::from_bits(rng.next_u64());
        t1.check(&num::cvt_i64(d), &cpp::cvt_i64(d), &d);
        t2.check(&num::cvt_u64(d), &cpp::cvt_u64(d), &d);
        let d = (rng.f64() - 0.3) * 4e19;
        t1.check(&num::cvt_i64(d), &cpp::cvt_i64(d), &d);
        t2.check(&num::cvt_u64(d), &cpp::cvt_u64(d), &d);
    });
    t1.report();
    t2.report();

    let t3 = Tally::new("round_shift");
    cases(2, scale(2_000_000), |rng| {
        let x = rng.range(-(1i64 << 41), 1i64 << 41);
        let s = rng.below(33) as u32;
        t3.check(
            &num::round_shift(x, s),
            &num::round_shift_f64(x, s),
            &(x, s),
        );
    });
    t3.report();
}

#[test]
fn libm() {
    // Rust f64 methods against the C functions, bit for bit.
    let t = Tally::new("libm ln/tan/cos/sinh/atan");
    cases(3, scale(2_000_000), |rng| {
        let lat = (rng.f64() * 2.0 - 1.0) * 89.9;
        let r = lat * std::f64::consts::PI / 180.0;
        t.check(&r.tan().to_bits(), &cpp::tan(r).to_bits(), &r);
        t.check(&r.cos().to_bits(), &cpp::cos(r).to_bits(), &r);
        let v = r.tan() + 1.0 / r.cos();
        t.check(&v.ln().to_bits(), &cpp::log(v).to_bits(), &v);
        let s = (rng.f64() * 2.0 - 1.0) * 3.2;
        t.check(&s.sinh().to_bits(), &cpp::sinh(s).to_bits(), &s);
        t.check(&s.atan().to_bits(), &cpp::atan(s).to_bits(), &s);
        let e = rng.f64() * 30.0;
        t.check(&e.exp().to_bits(), &cpp::exp(e).to_bits(), &e);
    });
    t.report();
}

#[test]
fn projection() {
    let t = Tally::new("lonlat2tile");
    let specials = [
        (0.0, 0.0),
        (180.0, 85.0511287798),
        (-180.0, -85.0511287798),
        (360.0, 89.9),
        (-360.0, -89.9),
        (400.0, 95.0),
        (-400.0, -95.0),
        (f64::NAN, f64::NAN),
        (f64::INFINITY, f64::NEG_INFINITY),
        (139.7, 35.6),
    ];
    for &(lon, lat) in &specials {
        for z in [0, 1, 12, 20, 31, 32] {
            t.check(
                &lonlat2tile(lon, lat, z),
                &cpp::lonlat2tile(lon, lat, z),
                &(lon, lat, z),
            );
        }
    }
    cases(4, scale(3_000_000), |rng| {
        let lon = (rng.f64() * 2.0 - 1.0) * 200.0;
        let lat = (rng.f64() * 2.0 - 1.0) * 92.0;
        t.check(
            &lonlat2tile(lon, lat, 32),
            &cpp::lonlat2tile(lon, lat, 32),
            &(lon, lat),
        );
        // Longitudes 122 to 154, latitudes 20 to 46, 7 decimals as in OSM data.
        let lon = (122.0 + rng.f64() * 32.0) * 1e7;
        let lat = (20.0 + rng.f64() * 26.0) * 1e7;
        let (lon, lat) = (lon.round() / 1e7, lat.round() / 1e7);
        t.check(
            &lonlat2tile(lon, lat, 32),
            &cpp::lonlat2tile(lon, lat, 32),
            &(lon, lat),
        );
        let z = rng.below(33) as u32;
        t.check(
            &lonlat2tile(lon, lat, z),
            &cpp::lonlat2tile(lon, lat, z),
            &(lon, lat, z),
        );
    });
    t.report();

    let t = Tally::new("tile2lonlat");
    cases(5, scale(1_000_000), |rng| {
        let z = rng.below(33) as u32;
        let n = 1i64 << z;
        let x = rng.range(-1, n + 1);
        let y = rng.range(-1, n + 1);
        let r = tile2lonlat(x, y, z);
        let c = cpp::tile2lonlat(x, y, z);
        t.check(
            &(r.0.to_bits(), r.1.to_bits()),
            &(c.0.to_bits(), c.1.to_bits()),
            &(x, y, z),
        );
    });
    t.report();

    let t = Tally::new("quadkey");
    cases(6, scale(1_000_000), |rng| {
        let x = rng.next_u64() as u32;
        let y = rng.next_u64() as u32;
        t.check(&encode_quadkey(x, y), &cpp::encode_quadkey(x, y), &(x, y));
        let ix = rng.next_u64();
        t.check(&decode_quadkey(ix), &cpp::decode_quadkey(ix), &ix);
        t.check(&projection::bit_reverse(ix), &cpp::bit_reverse(ix), &ix);
    });
    t.report();
}

#[test]
fn area_and_fix_polygon() {
    let ta = Tally::new("get_area");
    let tm = Tally::new("get_mp_area");
    let tf = Tally::new("fix_polygon");
    let tc = Tally::new("center_of_mass_mp");
    let ti = Tally::new("feature_index");
    cases(7, scale(1_000_000), |rng| {
        let z = if rng.chance(0.3) { 0 } else { rand_z(rng) };
        let g = parsed_polygon(rng, z, BUFFER);
        // Some cases far off the plane: big coordinates for the 2^53 retry.
        let g: DrawVec = if rng.chance(0.1) {
            let k = rng.range(1, 4);
            g.iter()
                .map(|d| Draw::new(d.op, d.x * k, d.y * k))
                .collect()
        } else {
            g
        };

        let mut fixed = Vec::new();
        fix_polygon(&g, &mut fixed);
        tf.check(&fixed, &cpp::fix_polygon(&g), &g);

        let mut i = 0;
        while i < fixed.len() {
            let j = polygon::part_end(&fixed, i);
            ta.check(
                &get_area(&fixed, i, j).to_bits(),
                &cpp::get_area(&fixed, i, j).to_bits(),
                &(i, j),
            );
            i = j;
        }
        tm.check(
            &get_mp_area(&fixed).to_bits(),
            &cpp::get_mp_area(&fixed).to_bits(),
            &fixed,
        );
        tc.check(
            &center_of_mass_mp(&fixed),
            &cpp::center_of_mass_mp(&fixed),
            &fixed,
        );
        let bbox = geometry_bbox(&fixed);
        ti.check(
            &feature_index(GeomType::Polygon, &fixed, &bbox),
            &cpp::feature_index(GeomType::Polygon, &fixed, &bbox),
            &fixed,
        );

        // Rings straight from the generator (open, short, degenerate).
        let r = ring(rng, z, BUFFER, 12);
        let mut rg = Vec::new();
        push_ring(&mut rg, &r);
        ta.check(
            &get_area(&rg, 0, rg.len()).to_bits(),
            &cpp::get_area(&rg, 0, rg.len()).to_bits(),
            &rg,
        );
        tc.check(&center_of_mass_mp(&rg), &cpp::center_of_mass_mp(&rg), &rg);

        let l = line(rng, z, BUFFER);
        let bbox = geometry_bbox(&l);
        ti.check(
            &feature_index(GeomType::Line, &l, &bbox),
            &cpp::feature_index(GeomType::Line, &l, &bbox),
            &l,
        );
        let p = points(rng, z, BUFFER);
        let bbox = geometry_bbox(&p);
        ti.check(
            &feature_index(GeomType::Point, &p, &bbox),
            &cpp::feature_index(GeomType::Point, &p, &bbox),
            &p,
        );
    });
    // Degenerate: zero area (center of mass divides by zero).
    for g in [
        vec![(5, 5), (5, 5), (5, 5), (5, 5)],
        vec![(0, 0), (10, 0), (20, 0), (0, 0)],
        vec![(1 << 31, 7), (1 << 31, 9), (1 << 31, 7)],
    ] {
        let mut d = Vec::new();
        push_ring(&mut d, &g);
        tc.check(&center_of_mass_mp(&d), &cpp::center_of_mass_mp(&d), &d);
    }
    // Hole larger than the outer ring: the size_t area sum goes negative.
    {
        let mut d = Vec::new();
        push_ring(&mut d, &[(0, 0), (3, 0), (3, 3), (0, 3), (0, 0)]);
        push_ring(&mut d, &[(0, 0), (0, 300), (300, 300), (300, 0), (0, 0)]);
        tc.check(&center_of_mass_mp(&d), &cpp::center_of_mass_mp(&d), &d);
        let mut d2 = Vec::new();
        push_ring(&mut d2, &[(0, 0), (1, 0), (1, 1), (0, 1), (0, 0)]);
        tc.check(&center_of_mass_mp(&d2), &cpp::center_of_mass_mp(&d2), &d2);
    }
    for t in [&ta, &tm, &tf, &tc, &ti] {
        t.report();
    }
}

#[test]
fn quick_check_and_segment_clip() {
    let tq = Tally::new("quick_check");
    let tc = Tally::new("clip (Cohen-Sutherland)");
    cases(8, scale(2_000_000), |rng| {
        let z = rand_z(rng);
        let mut b = [
            coord(rng, z, BUFFER),
            coord(rng, z, BUFFER),
            coord(rng, z, BUFFER),
            coord(rng, z, BUFFER),
        ];
        if rng.chance(0.8) {
            if b[0] > b[2] {
                b.swap(0, 2);
            }
            if b[1] > b[3] {
                b.swap(1, 3);
            }
        }
        tq.check(
            &quick_check(&b, z, BUFFER),
            &cpp::quick_check(&b, z, BUFFER),
            &(b, z),
        );

        let area = 1i64 << (32 - z);
        let bw = buffer_world(z, BUFFER);
        let (xmin, ymin, xmax, ymax) = if rng.chance(0.5) {
            (-bw, -bw, area + bw, area + bw)
        } else {
            (0, 0, area, area)
        };
        let seg = [
            coord(rng, z, BUFFER),
            coord(rng, z, BUFFER),
            coord(rng, z, BUFFER),
            coord(rng, z, BUFFER),
        ];
        let [mut x0, mut y0, mut x1, mut y1] = seg;
        let c = clip(&mut x0, &mut y0, &mut x1, &mut y1, xmin, ymin, xmax, ymax);
        tc.check(
            &(c, [x0, y0, x1, y1]),
            &cpp::clip(seg, xmin, ymin, xmax, ymax),
            &(seg, z),
        );
    });
    tq.report();
    tc.report();
}

#[test]
fn clip_geometries() {
    let tp = Tally::new("clip_point");
    let tl = Tally::new("clip_lines");
    let ts = Tally::new("simple_clip_poly");
    cases(9, scale(1_000_000), |rng| {
        let z = rand_z(rng);
        let mut p = points(rng, z, BUFFER);
        let c = cpp::clip_point(&p, z, BUFFER);
        clip_point(&mut p, z, BUFFER);
        tp.check(&p, &c, &z);

        let l = if rng.chance(0.2) {
            soup(rng, z, BUFFER)
        } else {
            line(rng, z, BUFFER)
        };
        let mut out = Vec::new();
        clip_lines(&l, z, BUFFER, &mut out);
        tl.check(&out, &cpp::clip_lines(&l, z, BUFFER), &(&l, z));

        let g = polygon(rng, z, BUFFER);
        let mut out = Vec::new();
        let mut sc = PolyClipScratch::default();
        simple_clip_poly(&g, z, BUFFER, &mut out, &mut sc);
        ts.check(&out, &cpp::simple_clip_poly(&g, z, BUFFER), &(&g, z));
    });
    tp.report();
    tl.report();
    ts.report();
}

#[test]
fn clip_to_tile_random() {
    let t = Tally::new("clip_to_tile");
    let tr = Tally::new("child_tile_range");
    cases(10, scale(1_000_000), |rng| {
        let z = if rng.chance(0.1) { 0 } else { rand_z(rng) };
        let n = 1u64 << z;
        let tx = rng.below(n) as u32;
        let ty = rng.below(n) as u32;
        let ty_ = rand_type(rng);
        // Absolute coordinates around the tile.
        let (ox, oy) = tile_origin(z, tx, ty);
        let mut g = geom_of(rng, ty_, z, BUFFER);
        for d in &mut g {
            d.x += ox;
            d.y += oy;
        }
        let c = cpp::clip_to_tile(&g, ty_, z, tx, ty, BUFFER);
        let mut rg = g.clone();
        let mut sc = GeomScratch::default();
        let r = clip_to_tile(&mut rg, ty_, z, tx, ty, BUFFER, &mut sc).map(|b| (rg, b));
        t.check(&r, &c, &(&g, ty_, z, tx, ty));

        if let Some((_, bbox)) = &r
            && z < 20
        {
            let nz = z + 1 + (rng.below(3) as u32).min(19 - z);
            tr.check(
                &child_tile_range(bbox, z, nz, tx, ty, BUFFER),
                &cpp::child_range(bbox, z, nz, tx, ty, BUFFER),
                &(bbox, z, nz),
            );
        }
    });
    t.report();
    tr.report();
}

#[test]
fn simplification() {
    let tn = Tally::new("remove_noop");
    let ti = Tally::new("impose_tile_boundaries");
    let td = Tally::new("distance_from_line");
    let tdp = Tally::new("douglas_peucker");
    let tsl = Tally::new("simplify_lines");
    let tsf = Tally::new("simplify_feature");
    let tts = Tally::new("to_tile_scale");
    let tcp = Tally::new("close_poly");
    cases(11, scale(1_000_000), |rng| {
        let z = rand_z(rng);
        let ty_ = rand_type(rng);
        let g = if rng.chance(0.3) {
            soup(rng, z, BUFFER)
        } else {
            geom_of(rng, ty_, z, BUFFER)
        };

        let shift = if rng.chance(0.5) {
            0
        } else {
            rng.below(21) as u32
        };
        let mut r = g.clone();
        remove_noop(&mut r, ty_, shift);
        tn.check(&r, &cpp::remove_noop(&g, ty_, shift), &(&g, ty_, shift));

        let extent = 1i64 << (32 - z);
        let mut out = Vec::new();
        impose_tile_boundaries(&g, extent, &mut out);
        ti.check(&out, &cpp::impose_tile_boundaries(&g, extent), &(&g, z));

        let q: Vec<i64> = (0..6).map(|_| coord(rng, z, BUFFER)).collect();
        td.check(
            &distance_from_line(q[0], q[1], q[2], q[3], q[4], q[5]).to_bits(),
            &cpp::distance_from_line(q[0], q[1], q[2], q[3], q[4], q[5]).to_bits(),
            &q,
        );

        // Douglas–Peucker on one part with random necessary flags.
        let l = line(rng, z, BUFFER);
        let j = polygon::part_end(&l, 0);
        if j > 1 {
            let mut dl = l.clone();
            for d in &mut dl {
                d.necessary = rng.chance(0.1);
            }
            dl[0].necessary = true;
            dl[j - 1].necessary = true;
            let e = (1i64 << (32 - DETAIL - z)) as f64
                * if rng.chance(0.5) {
                    1.0
                } else {
                    rng.f64() * 8.0
                };
            let retain = if rng.chance(0.5) {
                0
            } else {
                rng.below(8) as usize
            };
            let c = cpp::douglas_peucker(&dl, 0, j, e, 2, retain);
            let mut stack = Vec::new();
            douglas_peucker(&mut dl, 0, j, e, 2, retain, &mut stack);
            tdp.check(&dl, &c, &(&l, e, retain));
        }

        let mark = rng.chance(0.8);
        let simp = if rng.chance(0.7) {
            1.0
        } else {
            rng.f64() * 4.0
        };
        let retain = if ty_ == GeomType::Polygon { 4 } else { 0 };
        let mut sl = g.clone();
        let mut ss = SimplifyScratch::default();
        simplify_lines(&mut sl, z, DETAIL, mark, simp, retain, &mut ss);
        tsl.check(
            &sl,
            &cpp::simplify_lines(&g, z, DETAIL, mark, simp, retain),
            &(&g, z, mark, simp),
        );

        let fg = geom_of(rng, ty_, z, BUFFER);
        let reduced = rng.chance(0.2);
        let mut sf = fg.clone();
        let a = simplify_feature(&mut sf, ty_, z, DETAIL, 1.0, reduced, &mut ss);
        let (cg, ca) = cpp::simplify_feature(&fg, ty_, z, DETAIL, 1.0, reduced);
        tsf.check(
            &(sf, a.to_bits()),
            &(cg, ca.to_bits()),
            &(&fg, ty_, z, reduced),
        );

        let mut s = g.clone();
        to_tile_scale(&mut s, z, DETAIL);
        tts.check(&s, &cpp::to_tile_scale(&g, z, DETAIL), &(&g, z));

        let mut cp = g.clone();
        close_poly(&mut cp);
        tcp.check(&cp, &cpp::close_poly(&g), &g);
    });
    for t in [&tn, &ti, &td, &tdp, &tsl, &tsf, &tts, &tcp] {
        t.report();
    }
}

#[test]
fn tiny_polygons() {
    let tt = Tally::new("reduce_tiny_poly");
    let tl = Tally::new("line_is_too_small");
    let tr = Tally::new("revive_polygon");
    cases(12, scale(200_000), |rng| {
        // One tile: a sequence of polygons that share the accumulator.
        let z = rand_z(rng);
        let mut acc_r = TinyPolyAccum::default();
        let mut acc_c = 0.0f64;
        let tiny = if rng.chance(0.8) {
            2.0
        } else {
            rng.f64() * 6.0
        };
        for _ in 0..1 + rng.below(20) {
            // Small polygons: a few tile units.
            let unit = 1i64 << (32 - DETAIL - z);
            let mut g = Vec::new();
            for _ in 0..1 + rng.below(3) {
                let cx = coord(rng, z, BUFFER);
                let cy = coord(rng, z, BUFFER);
                let k = rng.range(0, 4);
                let pts: Vec<(i64, i64)> = (0..3 + rng.below(5))
                    .map(|_| {
                        (
                            cx + rng.range(-k, k) * unit + rng.range(-3, 3),
                            cy + rng.range(-k, k) * unit,
                        )
                    })
                    .collect();
                let mut pts = pts;
                pts.push(pts[0]);
                push_ring(&mut g, &pts);
            }
            if rng.chance(0.3) {
                g.extend(polygon(rng, z, BUFFER));
            }
            let mut out = Vec::new();
            let res = reduce_tiny_poly(&g, z, DETAIL, tiny, &mut acc_r, &mut out);
            let (cout, cs, cr) = cpp::reduce_tiny_poly(&g, z, DETAIL, tiny, &mut acc_c);
            tt.check(
                &(
                    out,
                    res.still_needs_simplification,
                    res.reduced_away,
                    acc_r.area.to_bits(),
                ),
                &(cout, cs, cr, acc_c.to_bits()),
                &(&g, z),
            );

            tl.check(
                &line_is_too_small(&g, z, DETAIL),
                &cpp::line_is_too_small(&g, z, DETAIL),
                &(&g, z),
            );

            let area = if rng.chance(0.5) {
                get_mp_area(&g)
            } else {
                rng.f64() * (unit * unit * 30) as f64
            };
            let mut tg = g.clone();
            to_tile_scale(&mut tg, z, DETAIL);
            let mut rv = Vec::new();
            revive_polygon(&ReviveInfo::of(&tg), area, z, DETAIL, &mut rv);
            tr.check(
                &rv,
                &cpp::revive_polygon(&tg, area, z, DETAIL),
                &(&tg, area, z),
            );
        }
        let l = line(rng, z, BUFFER);
        tl.check(
            &line_is_too_small(&l, z, DETAIL),
            &cpp::line_is_too_small(&l, z, DETAIL),
            &(&l, z),
        );
    });
    tl.check(
        &line_is_too_small(&[], 5, DETAIL),
        &cpp::line_is_too_small(&[], 5, DETAIL),
        &"empty",
    );
    tt.report();
    tl.report();
    tr.report();
}

#[test]
fn drop_rate() {
    let t = Tally::new("calc_feature_minzoom");
    let maxzooms: Vec<u32> = (0..=24).collect();
    maxzooms.par_iter().for_each(|&maxzoom| {
        let mut rng = Rng::new(100 + maxzoom as u64);
        for basezoom in [maxzoom, maxzoom.saturating_sub(2), maxzoom + 1] {
            for rate in [2.5, 1.0, 3.7, 1.5] {
                let mut r = DropState::new(maxzoom, basezoom, rate);
                let mut c = cpp::DropState::new(maxzoom, basezoom, rate);
                for _ in 0..scale(100_000) {
                    let ty = if rng.chance(0.8) {
                        GeomType::Point
                    } else {
                        rand_type(&mut rng)
                    };
                    t.check(
                        &r.feature_minzoom(ty),
                        &c.feature_minzoom(ty),
                        &(maxzoom, basezoom, rate),
                    );
                }
            }
        }
    });
    t.report();
}

/// The whole per-feature path at one tile, in tile order, with a shared
/// tiny polygon accumulator. Polygon cleaning comes from the C++ code.
#[test]
fn tile_steps_random() {
    let t = Tally::new("tile pipeline (random)");
    cases(13, scale(100_000), |rng| {
        let z = rand_z(rng);
        let n = 1u64 << z;
        let (tx, ty) = (rng.below(n) as u32, rng.below(n) as u32);
        let (ox, oy) = tile_origin(z, tx, ty);
        let mut acc_r = TinyPolyAccum::default();
        let mut acc_c = 0.0f64;
        let mut sc = GeomScratch::default();
        for _ in 0..1 + rng.below(8) {
            let gt = rand_type(rng);
            let mut g = geom_of(rng, gt, z, BUFFER);
            if gt == GeomType::Polygon && rng.chance(0.5) {
                // Make it a valid fixed polygon first.
                let mut p = Vec::new();
                let mut q = g.clone();
                q.push(Draw::new(vtmake_core::VT_CLOSEPATH, 0, 0));
                fix_polygon(&q, &mut p);
                g = p;
            }
            for d in &mut g {
                d.x += ox;
                d.y += oy;
            }
            let mut rg = g.clone();
            if clip_to_tile(&mut rg, gt, z, tx, ty, BUFFER, &mut sc).is_none() {
                continue;
            }
            let c = cpp::tile_steps(&rg, gt, z, DETAIL, 1.0, 2.0, &mut acc_c);

            let red = tile_reduce(&mut rg, gt, z, DETAIL, 2.0, &mut acc_r, &mut sc);
            let mut kept = red.keep;
            let mut pre_clean = Vec::new();
            let mut fin = Vec::new();
            if kept {
                let pre = tile_simplify(&mut rg, gt, z, DETAIL, 1.0, red.reduced, &mut sc);
                pre_clean = rg.clone();
                match gt {
                    GeomType::Polygon => {
                        let mut cleaned = c.cleaned.clone();
                        tile_finish_polygon(&mut cleaned, &pre, z, DETAIL);
                        fin = cleaned;
                    }
                    GeomType::Line => {
                        tile_finish_line(&mut rg);
                        fin = rg;
                    }
                    GeomType::Point => fin = rg,
                }
            } else {
                kept = false;
            }
            t.check(
                &(kept, red.tiny_polygon, pre_clean, fin, acc_r.area.to_bits()),
                &(
                    c.kept,
                    c.tiny_counted,
                    c.pre_clean.clone(),
                    c.fin.clone(),
                    acc_c.to_bits(),
                ),
                &(&g, gt, z, tx, ty),
            );
        }
    });
    t.report();
}

#[test]
fn ops_of_fixed_output() {
    // fix_polygon keeps only MOVETO/LINETO and closes rings.
    let mut rng = Rng::new(99);
    for _ in 0..1000 {
        let g = parsed_polygon(&mut rng, 10, BUFFER);
        let mut f = Vec::new();
        fix_polygon(&g, &mut f);
        assert!(f.iter().all(|d| d.op == VT_MOVETO || d.op == VT_LINETO));
    }
}

/// Two places where the text of the spec does not match the C++ code. This
/// test shows that the difference is real (so the spec must change).
#[test]
fn spec_differences() {
    use std::sync::atomic::{AtomicU64, Ordering};
    // 1. Projection y: the C++ code is round(n * (1 - ...) / 2), not
    //    trunc(round(n * (1 - ...)) / 2) as spec 4.1 says.
    let differ = AtomicU64::new(0);
    cases(20, scale(200_000), |rng| {
        let lat = (rng.f64() * 2.0 - 1.0) * 85.0;
        let r = lat * std::f64::consts::PI / 180.0;
        let n = 4294967296.0f64;
        let spec_y = ((n * (1.0 - ((r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI))).round()
            / 2.0) as i64;
        let c = cpp::lonlat2tile(0.0, lat, 32).1;
        assert_eq!(lonlat2tile(0.0, lat, 32).1, c);
        if spec_y != c {
            differ.fetch_add(1, Ordering::Relaxed);
        }
    });
    let d = differ.load(Ordering::Relaxed);
    eprintln!("SPEC projection y: spec formula differs from C++ in {d} cases");
    assert!(d > 0);

    // 2. simplify_feature: a polygon that simplifies to fewer than 3 draws
    //    keeps the simplified result (the C++ "< 3" test has no effect).
    let small = AtomicU64::new(0);
    cases(21, scale(200_000), |rng| {
        let z = rng.below(21) as u32;
        let g = polygon(rng, z, BUFFER);
        let (c, _) = cpp::simplify_feature(&g, GeomType::Polygon, z, DETAIL, 1.0, false);
        if c.len() < 3 && c != g {
            small.fetch_add(1, Ordering::Relaxed);
        }
    });
    let s = small.load(Ordering::Relaxed);
    eprintln!("SPEC simplify_feature: {s} polygons kept a simplified result with < 3 draws");
    assert!(s > 0);
}
