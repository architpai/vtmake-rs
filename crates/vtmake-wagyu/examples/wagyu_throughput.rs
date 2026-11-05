use vtmake_core::Draw;
use vtmake_wagyu::Cleaner;
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
    let count = 1_000_000;
    let geom: Vec<_> = [
        (1, 0, 0),
        (2, 100, 0),
        (2, 100, 100),
        (2, 0, 100),
        (2, 0, 0),
    ]
    .into_iter()
    .map(|(op, x, y)| Draw::new(op, x, y))
    .collect();
    let mut cleaner = Cleaner::new();
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    for _ in 0..count {
        cleaner.clean(&geom, &mut out).unwrap();
        std::hint::black_box(&out);
    }
    println!(
        "rust square count={count} us/polygon={:.3}",
        start.elapsed().as_secs_f64() * 1e6 / count as f64
    );
    #[cfg(feature = "reference")]
    {
        let input: Vec<i64> = geom.iter().flat_map(|d| [d.op as i64, d.x, d.y]).collect();
        let mut output = [0; 128];
        let start = std::time::Instant::now();
        for _ in 0..count {
            std::hint::black_box(unsafe {
                bt_reference(
                    input.as_ptr(),
                    geom.len(),
                    output.as_mut_ptr(),
                    output.len(),
                    false,
                )
            });
        }
        println!(
            "cpp square count={count} us/polygon={:.3}",
            start.elapsed().as_secs_f64() * 1e6 / count as f64
        );
    }
}
