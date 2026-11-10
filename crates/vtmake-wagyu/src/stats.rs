//! Aggregate call counts. This module stores no coordinates.
use std::sync::Mutex;
use vtmake_core::{Draw, VT_LINETO, VT_MOVETO};

#[derive(Default, Debug)]
struct Counts {
    calls: u64,
    convex_hits: u64,
    single: u64,
    multi: u64,
    empty: u64,
    with_negative_ring: u64,
    convex: u64,
    nonconvex: u64,
    vertices: [u64; 6],
}
static COUNTS: Mutex<Counts> = Mutex::new(Counts {
    calls: 0,
    convex_hits: 0,
    single: 0,
    multi: 0,
    empty: 0,
    with_negative_ring: 0,
    convex: 0,
    nonconvex: 0,
    vertices: [0; 6],
});

#[cold]
#[inline(never)]
pub(super) fn record(geom: &[Draw], convex_hit: bool) {
    let mut counts = COUNTS.lock().unwrap();
    counts.calls += 1;
    if convex_hit {
        counts.convex_hits += 1;
        return;
    }
    let mut rings = 0;
    let mut negative = false;
    let mut single = &[][..];
    for (i, d) in geom.iter().enumerate() {
        if d.op != VT_MOVETO {
            continue;
        }
        let end = geom[i + 1..]
            .iter()
            .position(|p| p.op != VT_LINETO)
            .map_or(geom.len(), |n| i + 1 + n);
        let ring = &geom[i..end];
        if ring.len() >= 4 {
            single = ring;
            rings += 1;
            negative |= super::ring_area(ring) < 0.;
        }
    }
    counts.with_negative_ring += u64::from(negative);
    match rings {
        0 => counts.empty += 1,
        1 => {
            counts.single += 1;
            let ring = if single
                .first()
                .zip(single.last())
                .is_some_and(|(a, b)| (a.x, a.y) == (b.x, b.y))
            {
                &single[..single.len() - 1]
            } else {
                single
            };
            let n = ring.len();
            counts.vertices[match n {
                0..=3 => 0,
                4..=16 => 1,
                17..=32 => 2,
                33..=64 => 3,
                65..=128 => 4,
                _ => 5,
            }] += 1;
            if is_convex(ring) {
                counts.convex += 1;
            } else {
                counts.nonconvex += 1;
            }
        }
        _ => counts.multi += 1,
    }
}

/// Print counts of calls outside the original convex path. No source data is kept.
pub fn print_statistics() {
    eprintln!(
        "wagyu statistics (miss vertex bins: <=3, 4-16, 17-32, 33-64, 65-128, >128): {:?}",
        COUNTS.lock().unwrap()
    );
}

// Each edge must support the whole ring on the same side. Local turns alone
// would also classify some self-intersecting stars as convex.
fn is_convex(ring: &[Draw]) -> bool {
    let mut positive = false;
    let mut negative = false;
    for (i, &a) in ring.iter().enumerate() {
        let b = ring[(i + 1) % ring.len()];
        for &p in ring {
            let cross = (b.x as i128 - a.x as i128) * (p.y as i128 - a.y as i128)
                - (b.y as i128 - a.y as i128) * (p.x as i128 - a.x as i128);
            positive |= cross > 0;
            negative |= cross < 0;
            if positive && negative {
                return false;
            }
        }
    }
    positive || negative
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn convexity_requires_supporting_edges() {
        let points = [(0, 0), (10, 0), (10, 10), (0, 10)].map(|(x, y)| Draw::new(2, x, y));
        assert!(is_convex(&points));
        assert!(!is_convex(&[points[0], points[2], points[1], points[3]]));
        let star =
            [(0, -10), (6, 8), (-10, -3), (10, -3), (-6, 8)].map(|(x, y)| Draw::new(2, x, y));
        assert!(!is_convex(&star));
    }
}
