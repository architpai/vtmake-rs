#![cfg(feature = "reference")]
use vtmake_core::Draw;
use vtmake_wagyu::{Cleaner, simple::simple_ring};
unsafe extern "C" {
    fn bt_reference(
        input: *const i64,
        n: usize,
        out: *mut i64,
        capacity: usize,
        coalesce: bool,
    ) -> usize;
}
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % ((hi - lo + 1) as u64)) as i64
    }
}
type P = (i64, i64);
fn cross(a: P, b: P, c: P) -> i128 {
    (b.0 - a.0) as i128 * (c.1 - a.1) as i128 - (b.1 - a.1) as i128 * (c.0 - a.0) as i128
}
fn proper(a: P, b: P, c: P, d: P) -> bool {
    let s = |v: i128| v.signum();
    s(cross(a, b, c)) * s(cross(a, b, d)) < 0 && s(cross(c, d, a)) * s(cross(c, d, b)) < 0
}
// 2-opt untangle: removes proper crossings (touching may remain; the fast path rejects those).
fn untangle(p: &mut Vec<P>) -> bool {
    let n = p.len();
    for _ in 0..2000 {
        let mut found = false;
        'o: for i in 0..n {
            for j in i + 2..n {
                if i == 0 && j == n - 1 {
                    continue;
                }
                if proper(p[i], p[(i + 1) % n], p[j], p[(j + 1) % n]) {
                    p[i + 1..=j].reverse();
                    found = true;
                    break 'o;
                }
            }
        }
        if !found {
            return true;
        }
    }
    false
}
fn make(r: &mut Rng, kind: u64) -> Vec<P> {
    match kind {
        0 | 1 => {
            let n = r.range(3, 64) as usize;
            let s = [20, 60, 200, 1000, 100000][r.below(5) as usize];
            let levels = if kind == 1 { r.range(2, 8) } else { 0 };
            let mut p: Vec<P> = (0..n)
                .map(|_| {
                    let x = r.range(0, s);
                    let y = if levels > 0 {
                        r.range(0, levels - 1) * (s / levels).max(1)
                    } else {
                        r.range(0, s)
                    };
                    (x, y)
                })
                .collect();
            p.dedup();
            if p.len() < 3 || !untangle(&mut p) {
                return vec![];
            }
            p
        }
        2 => {
            // star polygon with random radii
            let n = r.range(3, 64) as usize;
            let rmax = [10, 50, 300, 5000][r.below(4) as usize] as f64;
            let mut ang: Vec<f64> = (0..n)
                .map(|_| r.below(1 << 20) as f64 / (1u64 << 20) as f64 * std::f64::consts::TAU)
                .collect();
            ang.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let q = r.range(0, 3);
            ang.iter()
                .map(|a| {
                    let rad = rmax * (0.05 + 0.95 * (r.below(1000) as f64 / 1000.));
                    let (mut x, mut y) = (
                        (a.cos() * rad).round() as i64,
                        (a.sin() * rad).round() as i64,
                    );
                    if q > 0 {
                        y = y / (q * 5) * (q * 5);
                        x = x / q * q;
                    }
                    (x, y)
                })
                .collect()
        }
        3 => {
            // histogram: bars hang from y = 0 or stand on y = H
            let k = r.range(1, 30) as usize;
            let w = r.range(1, 40);
            let hmax = r.range(2, 100);
            let hs: Vec<i64> = (0..k).map(|_| r.range(1, hmax)).collect();
            let mut p = vec![(0, hmax + 5)];
            let mut x = 0;
            for &h in &hs {
                let y = hmax + 5 - h - 5;
                if p.last().unwrap().1 != y {
                    p.push((x, y));
                }
                x += w;
                p.push((x, y));
            }
            p.push((x, hmax + 5));
            // remove collinear runs
            let mut q: Vec<P> = vec![];
            for pt in p {
                q.push(pt);
            }
            q.dedup();
            // perturb a bit sometimes
            if r.below(2) == 0 {
                let e = r.range(1, 3);
                for pt in q.iter_mut() {
                    pt.0 += r.range(-e, e);
                    pt.1 += r.range(-e, e);
                }
            }
            if r.below(2) == 0 {
                for pt in q.iter_mut() {
                    pt.1 = -pt.1;
                }
                q.reverse();
            }
            if r.below(2) == 0 {
                for pt in q.iter_mut() {
                    std::mem::swap(&mut pt.0, &mut pt.1);
                }
                q.reverse();
            }
            q
        }
        4 => {
            // spiral band
            let turns = r.range(1, 4) as f64;
            let m = r.range(4, 31) as usize;
            let scale = [5., 30., 200., 3000.][r.below(4) as usize];
            let width = 0.2 + r.below(100) as f64 / 100.;
            let mut outer = vec![];
            let mut inner = vec![];
            for i in 0..m {
                let t = i as f64 / (m - 1) as f64 * turns * std::f64::consts::TAU;
                let rad = 1. + t / std::f64::consts::TAU * 1.5;
                outer.push((
                    (t.cos() * (rad + width) * scale).round() as i64,
                    (t.sin() * (rad + width) * scale).round() as i64,
                ));
                inner.push((
                    (t.cos() * rad * scale).round() as i64,
                    (t.sin() * rad * scale).round() as i64,
                ));
            }
            inner.reverse();
            outer.extend(inner);
            outer
        }
        _ => {
            // comb with pointed teeth, some reaching the same top
            let k = r.range(1, 20);
            let w = r.range(2, 30);
            let base = r.range(5, 60);
            let mut p = vec![(0, base + 20)];
            for i in 0..k {
                let top = if r.below(3) == 0 {
                    0
                } else {
                    r.range(0, base - 1)
                };
                p.push((i * 2 * w, base));
                p.push((i * 2 * w + w + r.range(-w / 2, w / 2), top));
            }
            p.push((k * 2 * w, base));
            p.push((k * 2 * w, base + 20));
            p
        }
    }
}
fn reference(geom: &[Draw]) -> Vec<i64> {
    let input: Vec<i64> = geom.iter().flat_map(|d| [d.op as i64, d.x, d.y]).collect();
    let mut expected = vec![0; geom.len() * 30 + 4096];
    let len = unsafe {
        bt_reference(
            input.as_ptr(),
            geom.len(),
            expected.as_mut_ptr(),
            expected.len(),
            false,
        )
    };
    expected.truncate(len);
    expected
}
#[test]
fn fuzz_simple_ring() {
    let count: u64 = std::env::var("FUZZ_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    let seed: u64 = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x9e3779b97f4a7c15);
    let mut r = Rng(seed);
    let mut c = Cleaner::new();
    let mut hits = [0u64; 6];
    let mut tries = [0u64; 6];
    let mut empty_hits = 0;
    for case in 0..count {
        let kind = case % 6;
        let pts = make(&mut r, kind);
        if pts.len() < 3 {
            continue;
        }
        let n = pts.len();
        let offset = match r.below(6) {
            0 => (1 << 20) - 200_000,
            1 => -(1 << 20),
            2 => r.range(-1000, 1000),
            _ => 0,
        };
        let (ox, oy) = if r.below(2) == 0 {
            (offset, offset)
        } else {
            (offset, -offset)
        };
        let rev = r.below(2) == 0;
        let start = r.below(n as u64) as usize;
        let mut geom = vec![];
        for i in 0..=n {
            let p = pts[(start + if rev { n - i % n } else { i }) % n];
            let mut d = Draw::new(if i == 0 { 1 } else { 2 }, p.0 + ox, p.1 + oy);
            d.necessary = r.below(2) == 0;
            geom.push(d);
        }
        if r.below(2) == 0 {
            geom.push(Draw::new(7, 0, 0));
        }
        tries[kind as usize] += 1;
        let mut fast = vec![Draw::new(1, 7, 7)];
        if !simple_ring(&geom, &mut fast) {
            continue;
        }
        hits[kind as usize] += 1;
        if fast.is_empty() {
            empty_hits += 1;
        }
        let mut full = vec![];
        c.clean_full(&geom, &mut full).unwrap();
        assert_eq!(fast, full, "kind {kind} case {case}: {geom:?}");
        let actual: Vec<i64> = fast.iter().flat_map(|d| [d.op as i64, d.x, d.y]).collect();
        assert_eq!(
            actual,
            reference(&geom),
            "reference kind {kind} case {case}: {geom:?}"
        );
    }
    eprintln!("fuzz: tries {tries:?} hits {hits:?} empty {empty_hits}");
}
