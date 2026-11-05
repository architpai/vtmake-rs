//! Small tests that do not need the C++ reference. The fixed values come
//! from tippecanoe (`cpp-ref` feature).

use vtmake_core::{Draw, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use vtmake_geom::*;

fn ring(pts: &[(i64, i64)]) -> Vec<Draw> {
    pts.iter()
        .enumerate()
        .map(|(k, &(x, y))| Draw::new(if k == 0 { VT_MOVETO } else { VT_LINETO }, x, y))
        .collect()
}

#[test]
fn projection_values() {
    assert_eq!(lonlat_to_world(139.7671, 35.6812), (3814970102, 1691264193));
    assert_eq!(lonlat_to_world(0.0, 0.0), (2147483648, 2147483648));
    assert_eq!(lonlat_to_world(-180.0, 85.0511287798), (0, 0));
    assert_eq!(
        lonlat_to_world(139.5051724, 35.7314295),
        (3811845184, 1690526204)
    );
}

#[test]
fn quadkey() {
    assert_eq!(
        encode_quadkey(0x8000_0001, 0x7fff_ffff),
        10760600709663905111
    );
    for (x, y) in [
        (0u32, 0u32),
        (1, 2),
        (u32::MAX, 0),
        (0x1234_5678, 0x9abc_def0),
    ] {
        assert_eq!(decode_quadkey(encode_quadkey(x, y)), (x, y));
    }
}

#[test]
fn round_shift_matches_f64() {
    for x in [
        -5i64,
        -4,
        -3,
        -2,
        -1,
        0,
        1,
        2,
        3,
        4,
        5,
        1 << 40,
        -(1 << 40) - 1,
    ] {
        for s in 0..21 {
            assert_eq!(
                num::round_shift(x, s),
                num::round_shift_f64(x, s),
                "{x} {s}"
            );
        }
    }
}

#[test]
fn fix_polygon_winding() {
    // A clockwise outer ring (on screen, y down: area < 0) is reversed.
    let mut g = ring(&[(0, 0), (0, 100), (100, 100), (100, 0)]);
    g.push(Draw::new(VT_CLOSEPATH, 0, 0));
    let mut out = Vec::new();
    fix_polygon(&g, &mut out);
    assert_eq!(out.len(), 5);
    assert!(get_area(&out, 0, 5) > 0.0);
    assert_eq!((out[0].x, out[0].y), (out[4].x, out[4].y));
}

#[test]
fn clip_square_to_tile() {
    // z = 20: tile proper [0, 4096], buffer 80.
    let g = ring(&[
        (-1000, -1000),
        (5000, -1000),
        (5000, 5000),
        (-1000, 5000),
        (-1000, -1000),
    ]);
    let mut out = Vec::new();
    let mut sc = PolyClipScratch::default();
    simple_clip_poly(&g, 20, 5, &mut out, &mut sc);
    let pts: Vec<(i64, i64)> = out.iter().map(|d| (d.x, d.y)).collect();
    assert_eq!(
        pts,
        vec![
            (-80, -80),
            (4176, -80),
            (4176, 4176),
            (-80, 4176),
            (-80, -80)
        ]
    );
}

#[test]
fn simplify_straight_line() {
    // z = 10: tolerance 1024 world units. The middle points are on the line.
    let mut g = ring(&[(0, 0), (10000, 0), (20000, 0), (30000, 0)]);
    let mut sc = SimplifyScratch::default();
    simplify_lines(&mut g, 10, 12, true, 1.0, 0, &mut sc);
    let pts: Vec<(i64, i64)> = g.iter().map(|d| (d.x, d.y)).collect();
    assert_eq!(pts, vec![(0, 0), (30000, 0)]);

    // z = 20 (1 world unit = 1 tile unit): the integer distance (division
    // by 4 and 8 before the product) is not exact for short segments, so
    // tippecanoe keeps these points.
    let mut g = ring(&[(0, 0), (10, 0), (20, 0), (30, 0)]);
    simplify_lines(&mut g, 20, 12, true, 1.0, 0, &mut sc);
    assert_eq!(g.len(), 4);
}

#[test]
fn drop_state_rate() {
    // maxzoom 2, rate 2.5: the first point is in z0, then some only at z1/z2.
    let mut ds = DropState::new(2, 2, 2.5);
    let z: Vec<i32> = (0..8)
        .map(|_| ds.feature_minzoom(GeomType::Point))
        .collect();
    assert_eq!(z[0], 0);
    assert!(z.iter().all(|&v| (0..=2).contains(&v)));
    assert_eq!(ds.feature_minzoom(GeomType::Line), 0);
}
