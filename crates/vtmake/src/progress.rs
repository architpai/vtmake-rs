//! `-u/--json-progress`: progress lines `{"progress":12.3}` on standard
//! error while the tiles are made (felt `json_logger.cpp:progress_tile`,
//! called from `tile.cpp:next_feature` and `write_tile`).
//!
//! tippecanoe has one progress value for all threads (`oprogress`). Each
//! try of a tile (each detail) sets it to 0. Then a line is written when
//! the value is 0.1 or more above `oprogress`: at each feature that the try
//! reads, and at the end of the try. vtmake does the same checks at the
//! start of each try (the first feature) and at its end (the other features,
//! then the end), so that the hot feature loop has no progress work.
//!
//! The value is `floor((f + z) / (maxzoom + 1) * 1000) / 10`, as in
//! tippecanoe. In tippecanoe, `f` is the byte position in the compressed
//! temporary file of zoom `z`, divided by the size of that file. vtmake has
//! no such files: its `f` is the position of the tile in quadkey order at
//! zoom `z` (tippecanoe's files are in this order too), and inside the
//! tile, the fraction of its features. Thus the values are near
//! tippecanoe's values, but not the same. vtmake makes each tile and
//! then its child tiles; thus, without tile limits, the zoom of the lines
//! goes up and down. With more than one thread, the order of the lines
//! changes from run to run, in tippecanoe and in vtmake.
//!
//! `-U` (`--progress-interval`) gives the minimum time between two lines
//! (`progress_time`).
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::tile::Ctx;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// `oprogress` (an `f64`).
static OPROGRESS: AtomicU64 = AtomicU64::new(0);
/// `last_progress`: the time of the last line.
static LAST: Mutex<f64> = Mutex::new(0.0);

/// `progress_time`: true if `-U` seconds have passed since the last line.
fn progress_time(interval: f64) -> bool {
    if interval == 0.0 {
        return true;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let mut last = LAST.lock().unwrap();
    if now - *last >= interval {
        *last = now;
        true
    } else {
        false
    }
}

/// The position of tile (x, y) in quadkey order at zoom `z`, from 0 to 1.
fn position(z: u8, x: u32, y: u32) -> f64 {
    let mut q = 0u64;
    for i in (0..z as u32).rev() {
        q = (q << 2) | (((y >> i) & 1) << 1 | ((x >> i) & 1)) as u64;
    }
    q as f64 / 4f64.powi(z as i32)
}

/// The check of one point of a try (`f`: the position in the zoom).
fn check(ctx: &Ctx, z: u8, f: f64) {
    let maxzoom = ctx.maxzoom.load(Ordering::Relaxed) as f64;
    let progress = ((f + z as f64) / (maxzoom + 1.0) * 1000.0).floor() / 10.0;
    if progress >= f64::from_bits(OPROGRESS.load(Ordering::Relaxed)) + 0.1 {
        if progress_time(ctx.opts.progress_interval) {
            eprintln!("{{\"progress\":{progress:3.1}}}");
        }
        OPROGRESS.store(progress.to_bits(), Ordering::Relaxed);
    }
}

/// The start (`end` false) or the end of a try of tile (z, x, y) with `n`
/// features.
#[cold]
#[inline(never)]
pub fn tile(ctx: &Ctx, z: u8, x: u32, y: u32, n: usize, end: bool) {
    let start = position(z, x, y);
    let size = 1.0 / 4f64.powi(z as i32);
    let n = n.max(1);
    if !end {
        OPROGRESS.store(0f64.to_bits(), Ordering::Relaxed);
        check(ctx, z, start + size / n as f64);
        return;
    }
    for i in 2..=n {
        check(ctx, z, start + size * i as f64 / n as f64);
    }
    check(ctx, z, start + size);
}

#[cfg(test)]
mod tests {
    use super::position;

    #[test]
    fn quadkey_position() {
        assert_eq!(position(0, 0, 0), 0.0);
        assert_eq!(position(1, 1, 0), 0.25);
        assert_eq!(position(1, 0, 1), 0.5);
        assert_eq!(position(2, 3, 3), 15.0 / 16.0);
    }
}
