use vtmake_core::Draw;
use vtmake_parse::{ParsedLine, parse_feature};
use vtmake_wagyu::Cleaner;
use std::{io::BufRead, time::Instant};
#[cfg(feature = "reference")]
unsafe extern "C" {
    fn bt_reference(
        input: *const i64,
        n: usize,
        out: *mut i64,
        capacity: usize,
        coalesce: bool,
    ) -> usize;
}
fn main() {
    let path = std::env::args().nth(1).expect("public polygon file");
    let input = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut parsed = ParsedLine::default();
    let mut polygons = Vec::new();
    for line in input.split(b'\n').take(100_000) {
        parse_feature(&line.unwrap(), &mut parsed).unwrap();
        for f in &parsed.features {
            let mut geom = Vec::new();
            let mut ring: Vec<Draw> = Vec::new();
            let mut is_outer = true;
            let mut origin = None;
            let finish = |ring: &mut Vec<Draw>, geom: &mut Vec<Draw>, outer: bool| {
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
                geom.append(ring);
            };
            for p in &f.geom {
                if p.op == 7 {
                    finish(&mut ring, &mut geom, is_outer);
                    is_outer = true;
                    continue;
                }
                if p.op == 1 && !ring.is_empty() {
                    finish(&mut ring, &mut geom, is_outer);
                    is_outer = false;
                }
                let n = 65536. * 4096.;
                let lat = p.lat.to_radians();
                let x = ((p.lon + 180.) / 360. * n).round() as i64;
                let y = ((1. - (lat.tan() + 1. / lat.cos()).ln() / std::f64::consts::PI) * n / 2.)
                    .round() as i64;
                let (ox, oy) = *origin.get_or_insert((x, y));
                ring.push(Draw::new(p.op, x - ox, y - oy));
            }
            finish(&mut ring, &mut geom, is_outer);
            polygons.push(geom);
        }
    }
    let mut cleaner = Cleaner::new();
    let mut out = Vec::new();
    let start = Instant::now();
    for _ in 0..5 {
        for g in &polygons {
            cleaner.clean(g, &mut out).unwrap();
            std::hint::black_box(&out);
        }
    }
    let count = polygons.len() * 5;
    println!(
        "rust public z16 count={count} us/polygon={:.3}",
        start.elapsed().as_secs_f64() * 1e6 / count as f64
    );
    #[cfg(feature = "reference")]
    {
        let flat: Vec<Vec<i64>> = polygons
            .iter()
            .map(|g| g.iter().flat_map(|d| [d.op as i64, d.x, d.y]).collect())
            .collect();
        let mut out = vec![0; 65536];
        let start = Instant::now();
        for _ in 0..5 {
            for g in &flat {
                let n = unsafe {
                    bt_reference(g.as_ptr(), g.len() / 3, out.as_mut_ptr(), out.len(), false)
                };
                assert!(n <= out.len());
                std::hint::black_box(n);
            }
        }
        println!(
            "cpp public z16 count={count} us/polygon={:.3}",
            start.elapsed().as_secs_f64() * 1e6 / count as f64
        );
    }
}
