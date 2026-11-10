use vtmake_core::Draw;
use vtmake_wagyu::Cleaner;
#[test]
fn random_and_adversarial() {
    let mut c = Cleaner::new();
    let mut seed = 712381u64;
    let mut rand = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut hits = 0;
    for case in 0..1_000_000 {
        let n = 3 + (rand() % if case % 10 == 0 { 63 } else { 14 }) as usize;
        let mut g = Vec::new();
        for i in 0..n {
            let (x, y) = if case % 2 == 0 {
                let angle = i as f64 * std::f64::consts::TAU / n as f64;
                let r = if case % 4 == 0 {
                    1000
                } else {
                    2 + rand() % 1000
                };
                (
                    (angle.cos() * r as f64).round() as i64,
                    (angle.sin() * r as f64).round() as i64,
                )
            } else {
                ((rand() % 40) as i64, (rand() % 40) as i64)
            };
            g.push(Draw::new(if i == 0 { 1 } else { 2 }, x, y));
        }
        g.push(Draw::new(2, g[0].x, g[0].y));
        if case % 3 == 0 {
            g.push(Draw::new(7, 0, 0));
        }
        let mut a = Vec::new();
        let mut b = Vec::new();
        c.clean(&g, &mut a).unwrap();
        c.clean_full(&g, &mut b).unwrap();
        assert_eq!(a, b, "case {case}: {g:?}");
        hits += 1;
    }
    eprintln!(
        "random/adversarial: {hits} cases, 0 mismatches; verification counts {:?}",
        vtmake_wagyu::verification_counts()
    );
}

#[test]
fn fixed_cases() {
    let mut c = Cleaner::new();
    let mut cases = 0;
    for points in [
        vec![(0, 0), (2, 0), (2, 2), (0, 2)],
        vec![(0, 0), (100, 0), (50, 1)],
        vec![(0, 0), (100, 0), (99, 1), (0, 100)],
        vec![(0, 0), (1, 0), (1, 1), (0, 1)],
        vec![(0, 0), (10, 0), (10, 0), (10, 10), (0, 10)],
        vec![(0, 0), (5, 0), (10, 0), (10, 10), (0, 10)],
        vec![(0, 0), (10, 0), (5, 0), (10, 10), (0, 10)],
        vec![(0, 0), (10, 10), (0, 10), (10, 0)],
        vec![(0, 0), (10, 0), (2, 2), (0, 10)],
    ] {
        for offset in [-1_048_577, -1_048_576, -100, 0, 1_048_576] {
            for reverse in [false, true] {
                for start in 0..points.len() {
                    let n = points.len();
                    let mut g = Vec::new();
                    for i in 0..=n {
                        let p = points[(start + if reverse { n - i % n } else { i }) % n];
                        let mut d =
                            Draw::new(if i == 0 { 1 } else { 2 }, p.0 + offset, p.1 + offset);
                        d.necessary = true;
                        g.push(d);
                    }
                    for extra_ring in [false, true] {
                        let mut input = g.clone();
                        if extra_ring {
                            input.extend_from_slice(&g);
                        }
                        let mut a = Vec::new();
                        let mut b = Vec::new();
                        c.clean(&input, &mut a).unwrap();
                        c.clean_full(&input, &mut b).unwrap();
                        assert_eq!(a, b, "{input:?}");
                        cases += 1;
                    }
                }
            }
        }
    }
    eprintln!("fixed adversarial: {cases} cases, 0 mismatches");
}

#[test]
fn invalid_close_coordinate() {
    let mut c = Cleaner::new();
    let g = [
        Draw::new(1, 0, 0),
        Draw::new(2, 10, 0),
        Draw::new(2, 10, 10),
        Draw::new(2, 0, 10),
        Draw::new(2, 0, 0),
        Draw::new(7, 1 << 39, 0),
    ];
    let mut a = vec![Draw::new(1, 123, 456)];
    let mut b = a.clone();
    assert_eq!(c.clean(&g, &mut a), c.clean_full(&g, &mut b));
    assert_eq!(a, b);
}
