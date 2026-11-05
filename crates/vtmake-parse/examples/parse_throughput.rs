use vtmake_parse::{ParsedLine, parse_feature};
use rayon::prelude::*;
use std::time::Instant;
#[cfg(feature = "reference")]
unsafe extern "C" {
    fn bt_json_reference(input: *const std::ffi::c_char) -> usize;
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let file = std::fs::File::open(&args[1]).unwrap();
    let map = unsafe { memmap2::Mmap::map(&file) }.unwrap();
    let limit = args
        .get(2)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(map.len())
        .min(map.len());
    let limit = memchr::memrchr(b'\n', &map[..limit]).map_or(limit, |n| n + 1);
    let data = &map[..limit];
    for threads in [1, 30] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let mut starts = vec![0];
        for i in 1..threads {
            let at = data.len() / threads * i;
            if let Some(j) = memchr::memchr(b'\n', &data[at..]) {
                starts.push(at + j + 1);
            }
        }
        starts.push(data.len());
        let start = Instant::now();
        let count: usize = pool.install(|| {
            starts
                .par_windows(2)
                .map(|r| {
                    let mut out = ParsedLine::default();
                    let mut count = 0;
                    for line in data[r[0]..r[1]].split(|&b| b == b'\n') {
                        parse_feature(line, &mut out).unwrap();
                        count += out.features.len();
                        std::hint::black_box(&out);
                    }
                    count
                })
                .sum()
        });
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "rust threads={threads} bytes={} features={count} seconds={elapsed:.4} MB/s={:.2}",
            data.len(),
            data.len() as f64 / elapsed / 1e6
        );
    }
    #[cfg(feature = "reference")]
    {
        let start = Instant::now();
        let mut count = 0;
        for line in data.split(|&b| b == b'\n').filter(|v| !v.is_empty()) {
            let input = std::ffi::CString::new(line).unwrap();
            std::hint::black_box(unsafe { bt_json_reference(input.as_ptr()) });
            count += 1;
        }
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "jsonpull tree-only threads=1 bytes={} features={count} seconds={elapsed:.4} MB/s={:.2}",
            data.len(),
            data.len() as f64 / elapsed / 1e6
        );
    }
}
