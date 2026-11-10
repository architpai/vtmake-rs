//! Conservative simple-ring shortcut, adapted from manby crates/wagyu/src/simple.rs.
//! Copyright (c) 2026, Birk Skyum. BSD-2-Clause; see LICENSE-manby.md.
use vtmake_core::{Draw, DrawVec, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};

fn cross(a: Draw, b: Draw, p: Draw) -> i64 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

fn crosses(a: Draw, b: Draw, c: Draw, d: Draw) -> bool {
    // Collinear and touching cases are rejected by the vertex/edge test below.
    let ab_c = cross(a, b, c);
    let ab_d = cross(a, b, d);
    if (ab_c > 0) == (ab_d > 0) {
        return false;
    }
    let cd_a = cross(c, d, a);
    let cd_b = cross(c, d, b);
    (cd_a > 0) != (cd_b > 0)
}

pub fn simple_ring(geom: &[Draw], out: &mut DrawVec) -> bool {
    if !(4..=66).contains(&geom.len()) {
        return false;
    }
    // Check CLOSEPATH coordinates too, as the full wrapper does.
    if geom
        .iter()
        .any(|d| d.x.abs_diff(0) > (1 << 20) || d.y.abs_diff(0) > (1 << 20))
    {
        return false;
    }
    let geom = if geom.last().is_some_and(|d| d.op == VT_CLOSEPATH) {
        &geom[..geom.len() - 1]
    } else {
        geom
    };
    if !(4..=65).contains(&geom.len())
        || geom[0].op != VT_MOVETO
        || geom[1..].iter().any(|d| d.op != VT_LINETO)
        || (geom[0].x, geom[0].y) != (geom[geom.len() - 1].x, geom[geom.len() - 1].y)
    {
        return false;
    }
    let ring = &geom[..geom.len() - 1];
    let n = ring.len();
    let mut area = 0i64;
    let mut start = 0;
    let mut lowest = 1;
    for i in 0..n {
        let (a, b, c) = (ring[i], ring[(i + 1) % n], ring[(i + 2) % n]);
        if cross(a, b, c) == 0 {
            return false;
        }
        area += a.x * b.y - a.y * b.x;
        if i != 0 {
            if a.y < ring[start].y {
                start = i;
                lowest = 1;
            } else if a.y == ring[start].y {
                lowest += 1;
                if a.x > ring[start].x {
                    start = i;
                }
            }
        }
    }
    // Keep ambiguous equal-height maxima on the full path.
    if area == 0 || lowest > 2 {
        return false;
    }

    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let (x0, x1, y0, y1) = (a.x.min(b.x), a.x.max(b.x), a.y.min(b.y), a.y.max(b.y));
        let margin = (b.x - a.x).abs() + (b.y - a.y).abs();
        for (j, &p) in ring.iter().enumerate() {
            if j == i || j == (i + 1) % n {
                continue;
            }
            if p.x < x0 - 1 || p.x > x1 + 1 || p.y < y0 - 1 || p.y > y1 + 1 {
                continue;
            }
            // An edge that meets the square of radius one about p must pass
            // both tests. Reject the whole strip, which is conservative.
            if cross(a, b, p).abs() <= margin {
                return false;
            }
        }
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            let (c, d) = (ring[j], ring[(j + 1) % n]);
            if c.x.min(d.x) > x1 || c.x.max(d.x) < x0 || c.y.min(d.y) > y1 || c.y.max(d.y) < y0 {
                continue;
            }
            if crosses(a, b, c, d) {
                return false;
            }
        }
    }
    out.clear();
    // Positive fill discards an isolated simple clockwise ring.
    if area < 0 {
        return true;
    }
    out.reserve(n + 1);
    for i in 0..=n {
        let p = ring[(start + i) % n];
        out.push(Draw::new(
            if i == 0 { VT_MOVETO } else { VT_LINETO },
            p.x,
            p.y,
        ));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cleaner;

    fn check(points: &[(i64, i64)], hit: bool, empty: bool) {
        let mut cleaner = Cleaner::new();
        for reverse in [false, true] {
            for start in 0..points.len() {
                let n = points.len();
                let mut geom = Vec::new();
                for i in 0..=n {
                    let p = points[(start + if reverse { n - i % n } else { i }) % n];
                    let mut d = Draw::new(if i == 0 { VT_MOVETO } else { VT_LINETO }, p.0, p.1);
                    d.necessary = true;
                    geom.push(d);
                }
                for close in [false, true] {
                    let mut input = geom.clone();
                    if close {
                        input.push(Draw::new(VT_CLOSEPATH, 0, 0));
                    }
                    let mut actual = vec![Draw::new(VT_MOVETO, 999, 999)];
                    let mut expected = Vec::new();
                    assert_eq!(simple_ring(&input, &mut actual), hit, "{input:?}");
                    cleaner.clean_full(&input, &mut expected).unwrap();
                    if hit {
                        assert_eq!(actual, expected, "{input:?}");
                        assert_eq!(actual.is_empty(), empty != reverse);
                        assert!(actual.iter().all(|d| !d.necessary));
                    }
                }
            }
        }
    }

    #[test]
    fn footprints_and_winding() {
        check(
            &[(0, 0), (40, 0), (40, 10), (10, 10), (10, 40), (0, 40)],
            true,
            false,
        );
        check(
            &[
                (0, 0),
                (40, 0),
                (40, 40),
                (30, 40),
                (30, 10),
                (10, 10),
                (10, 40),
                (0, 40),
            ],
            true,
            false,
        );
        // Two separate vertices share the smallest y.
        check(
            &[(0, 0), (10, 10), (20, 0), (30, 20), (-10, 20)],
            true,
            false,
        );
    }

    #[test]
    fn rejects_unsafe_rings() {
        for points in [
            vec![(0, 0), (40, 0), (20, 0), (40, 40), (0, 40)], // spike
            vec![(0, 0), (40, 0), (40, 40), (20, 1), (0, 40)], // near edge
            vec![(0, 0), (40, 0), (40, 40), (20, 0), (0, 40)], // self touch
            vec![(0, 0), (20, 0), (40, 0), (40, 40), (0, 40)], // collinear
            vec![(0, 0), (40, 0), (40, 0), (40, 40), (0, 40)], // duplicate
            vec![(0, 0), (40, 40), (0, 40), (40, 0)],          // crossing
            vec![
                (0, 0),
                (10, 10),
                (20, 0),
                (30, 10),
                (40, 0),
                (50, 30),
                (-10, 30),
            ], // three minima
        ] {
            check(&points, false, false);
        }
    }

    #[test]
    fn limits() {
        for n in [64, 65] {
            let points: Vec<_> = (0..n)
                .map(|i| {
                    let angle = i as f64 * std::f64::consts::TAU / n as f64;
                    (
                        (angle.cos() * 10000.).round() as i64,
                        (angle.sin() * 10000.).round() as i64,
                    )
                })
                .collect();
            check(&points, n == 64, false);
        }
        for offset in [-(1 << 20), (1 << 20) - 40, -(1 << 20) - 1, (1 << 38) - 40] {
            let points: Vec<_> = [(0, 0), (40, 0), (40, 10), (10, 10), (10, 40), (0, 40)]
                .map(|(x, y)| (x + offset, y + offset))
                .into();
            check(
                &points,
                offset >= -(1 << 20) && offset <= (1 << 20) - 40,
                false,
            );
        }
    }
}
