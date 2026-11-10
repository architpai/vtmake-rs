#![cfg(feature = "reference")]
use vtmake_core::Draw;
use vtmake_wagyu::Cleaner;
unsafe extern "C" {
    fn bt_reference(
        input: *const i64,
        n: usize,
        out: *mut i64,
        capacity: usize,
        coalesce: bool,
    ) -> usize;
}
fn compare(cleaner: &mut Cleaner, geom: &[Draw], coalesce: bool) {
    let input: Vec<i64> = geom.iter().flat_map(|d| [d.op as i64, d.x, d.y]).collect();
    let mut expected = vec![0; geom.len() * 30 + 4096];
    let len = unsafe {
        bt_reference(
            input.as_ptr(),
            geom.len(),
            expected.as_mut_ptr(),
            expected.len(),
            coalesce,
        )
    };
    assert!(len <= expected.len());
    expected.truncate(len);
    let mut out = Vec::new();
    if coalesce {
        out.extend_from_slice(geom);
        cleaner.coalesce(&mut out).unwrap();
    } else {
        cleaner.clean(geom, &mut out).unwrap();
    }
    let actual: Vec<i64> = out.iter().flat_map(|d| [d.op as i64, d.x, d.y]).collect();
    assert_eq!(actual, expected, "input {geom:?}");
    assert!(out.iter().all(|d| !d.necessary));
}
#[test]
fn millions_of_polygons() {
    let count = std::env::var("VTMAKE_POLYGON_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);
    let mut state = 123456789u64;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut cleaner = Cleaner::new();
    for _ in 0..count {
        let mut geom = Vec::new();
        for _ in 0..1 + rand() % 4 {
            let n = 3 + rand() % 12;
            let x = (rand() % 400) as i64 - 200;
            let y = (rand() % 400) as i64 - 200;
            for i in 0..n {
                geom.push(Draw::new(
                    if i == 0 { 1 } else { 2 },
                    x + (rand() % 200) as i64,
                    y + (rand() % 200) as i64,
                ));
            }
            let start = geom[geom.len() - n as usize];
            geom.push(Draw::new(2, start.x, start.y));
            if rand() % 3 == 0 {
                geom.push(Draw::new(7, 0, 0));
            }
        }
        compare(&mut cleaner, &geom, false);
    }
    eprintln!("wagyu: {count} polygons, 0 mismatches");
}
#[test]
fn split_and_empty() {
    let mut c = Cleaner::new();
    compare(&mut c, &[], false);
    compare(
        &mut c,
        &[Draw::new(1, 0, 0), Draw::new(7, 0, 0), Draw::new(1, 1, 1)],
        false,
    );
    let mut geom = Vec::new();
    for i in 0..20_001 {
        let x = (i % 200) * 10;
        let y = (i / 200) * 10;
        for (op, a, b) in [(1, 0, 0), (2, 8, 0), (2, 8, 8), (2, 0, 8), (2, 0, 0)] {
            geom.push(Draw::new(op, x + a, y + b));
        }
    }
    compare(&mut c, &geom, true);
}
#[test]
fn simple_ring_adversarial() {
    let mut c = Cleaner::new();
    let mut shapes = vec![
        vec![(0, 0), (40, 0), (40, 10), (10, 10), (10, 40), (0, 40)],
        vec![
            (0, 0),
            (40, 0),
            (40, 40),
            (30, 40),
            (30, 10),
            (10, 10),
            (10, 40),
            (0, 40),
        ],
        vec![(0, 0), (40, 0), (40, 40), (20, 1), (0, 40)],
        vec![(0, 0), (40, 0), (20, 0), (40, 40), (0, 40)],
        vec![(0, 0), (20, 0), (40, 0), (40, 40), (0, 40)],
        vec![(0, 0), (40, 0), (40, 0), (40, 40), (0, 40)],
        vec![(0, 0), (40, 0), (40, 40), (20, 0), (0, 40)],
        vec![(0, 0), (40, 40), (0, 40), (40, 0)],
        vec![(0, 0), (10, 10), (20, 0), (30, 20), (-10, 20)],
        vec![
            (0, 0),
            (10, 10),
            (20, 0),
            (30, 10),
            (40, 0),
            (50, 30),
            (-10, 30),
        ],
    ];
    for n in [64, 65] {
        shapes.push(
            (0..n)
                .map(|i| {
                    let angle = i as f64 * std::f64::consts::TAU / n as f64;
                    let r = if i % 2 == 0 { 10000. } else { 7000. };
                    (
                        (angle.cos() * r).round() as i64,
                        (angle.sin() * r).round() as i64,
                    )
                })
                .collect(),
        );
    }
    for points in shapes {
        for offset in [
            -(1 << 38) + 10000,
            -(1 << 20) - 1,
            -100,
            0,
            (1 << 20) - 10000,
            (1 << 38) - 10000,
        ] {
            for reverse in [false, true] {
                for start in 0..points.len() {
                    let n = points.len();
                    let mut geom = Vec::new();
                    for i in 0..=n {
                        let p = points[(start + if reverse { n - i % n } else { i }) % n];
                        let mut d =
                            Draw::new(if i == 0 { 1 } else { 2 }, p.0 + offset, p.1 + offset);
                        d.necessary = true;
                        geom.push(d);
                    }
                    compare(&mut c, &geom, false);
                    geom.push(Draw::new(7, 0, 0));
                    compare(&mut c, &geom, false);
                }
            }
        }
    }
}
#[test]
#[ignore = "Set VTMAKE_CORPUS to public polygon NDJSON"]
fn corpus_polygons() {
    use std::io::BufRead;
    use vtmake_parse::{ParsedLine, parse_feature};
    let Ok(path) = std::env::var("VTMAKE_CORPUS") else {
        eprintln!("VTMAKE_CORPUS not set: skip");
        return;
    };
    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let limit = std::env::var("VTMAKE_CORPUS_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    let mut cleaner = Cleaner::new();
    let mut parsed = ParsedLine::default();
    let mut count = 0;
    for line in file.split(b'\n').take(limit) {
        parse_feature(&line.unwrap(), &mut parsed).unwrap();
        for f in &parsed.features {
            for zoom in [9, 12, 16, 20] {
                let mut geom = Vec::new();
                let mut rings = Vec::<(bool, Vec<Draw>)>::new();
                let mut outer = true;
                for p in &f.geom {
                    if p.op == 7 {
                        outer = true;
                        continue;
                    }
                    if p.op == 1 {
                        rings.push((outer, Vec::new()));
                        outer = false;
                    }
                    let n = (1u64 << zoom) as f64 * 4096.;
                    let x = ((p.lon + 180.) / 360. * n).round() as i64;
                    let lat = p.lat.to_radians();
                    let y = ((1. - (lat.tan() + 1. / lat.cos()).ln() / std::f64::consts::PI) * n
                        / 2.)
                        .round() as i64;
                    rings.last_mut().unwrap().1.push(Draw::new(p.op, x, y));
                }
                let origin = rings
                    .first()
                    .and_then(|r| r.1.first())
                    .copied()
                    .unwrap_or(Draw::new(1, 0, 0));
                for (outer, mut ring) in rings {
                    for d in &mut ring {
                        d.x -= origin.x;
                        d.y -= origin.y;
                    }
                    let area: i128 = ring
                        .windows(2)
                        .map(|p| p[0].x as i128 * p[1].y as i128 - p[1].x as i128 * p[0].y as i128)
                        .sum();
                    if (outer && area < 0) || (!outer && area > 0) {
                        ring.reverse();
                        for (i, d) in ring.iter_mut().enumerate() {
                            d.op = if i == 0 { 1 } else { 2 };
                        }
                    }
                    geom.extend(ring);
                }
                compare(&mut cleaner, &geom, false);
                count += 1;
            }
        }
    }
    eprintln!("public polygon zoom cases: {count}, 0 mismatches");
}
#[test]
fn independent_threads() {
    std::thread::scope(|scope| {
        for _ in 0..30 {
            scope.spawn(|| {
                let geom = [
                    Draw::new(1, 0, 0),
                    Draw::new(2, 10, 10),
                    Draw::new(2, 0, 10),
                    Draw::new(2, 10, 0),
                    Draw::new(2, 0, 0),
                ];
                let mut cleaner = Cleaner::new();
                for _ in 0..100 {
                    compare(&mut cleaner, &geom, false);
                }
            });
        }
    });
}
