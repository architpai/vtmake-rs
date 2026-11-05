//! Polygon union with the Wagyu headers from tippecanoe.
use vtmake_core::{Draw, DrawVec, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

static MODE: OnceLock<String> = OnceLock::new();
static CALLS: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
/// Return counts from differential checks enabled by VTMAKE_WAGYU_MODE=verify.
pub fn verification_counts() -> (u64, u64) {
    (CALLS.load(Ordering::Relaxed), HITS.load(Ordering::Relaxed))
}
unsafe extern "C" {
    fn vtmake_wagyu_new() -> *mut c_void;
    fn vtmake_wagyu_free(p: *mut c_void);
    fn vtmake_wagyu_union(
        p: *mut c_void,
        input: *const i64,
        n: usize,
        scale: i32,
        output: *mut *const i64,
        count: *mut usize,
    ) -> i32;
    fn vtmake_wagyu_union_clip(
        p: *mut c_void,
        input: *const i64,
        n: usize,
        lo: i64,
        hi: i64,
        output: *mut *const i64,
        count: *mut usize,
    ) -> i32;
    fn vtmake_snap_round_changes(input: *const i64, n: usize) -> i32;
}

/// The snap-round test of tippecanoe `check_polygon` (`-a@`): true if
/// `snap_round` changes the rings of `geom` (tile units, closed rings).
/// tippecanoe then writes "Internal error: self-intersecting polygon".
pub fn snap_round_changes(geom: &[Draw]) -> bool {
    let input: Vec<i64> = geom.iter().flat_map(|d| [d.op as i64, d.x, d.y]).collect();
    // SAFETY: the slice is valid for the call; the shim does not keep it.
    unsafe { vtmake_snap_round_changes(input.as_ptr(), geom.len()) == 1 }
}
/// Wagyu could not clean the polygon, or a coordinate exceeds the safe range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanError;
impl std::fmt::Display for CleanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Polygon cleaning failed")
    }
}
impl std::error::Error for CleanError {}
/// Reuse this buffer within one thread. No state is shared between cleaners.
pub struct Cleaner {
    context: NonNull<c_void>,
    input: Vec<i64>,
    scratch: DrawVec,
}
impl Default for Cleaner {
    fn default() -> Self {
        Self::new()
    }
}
impl Cleaner {
    pub fn new() -> Self {
        // SAFETY: the constructor returns an owned context or null.
        let context = NonNull::new(unsafe { vtmake_wagyu_new() }).expect("Wagyu allocation failed");
        Self {
            context,
            input: Vec::new(),
            scratch: Vec::new(),
        }
    }
    /// Clean tile coordinates. The output has explicit closing LINETO points.
    pub fn clean(&mut self, geom: &[Draw], out: &mut DrawVec) -> Result<(), CleanError> {
        let mode = MODE.get_or_init(|| std::env::var("VTMAKE_WAGYU_MODE").unwrap_or_default());
        if mode != "full" {
            let hit = convex_ring(geom, out);
            if mode == "verify" {
                CALLS.fetch_add(1, Ordering::Relaxed);
                let mut expected = Vec::new();
                self.clean_full(geom, &mut expected)?;
                if hit {
                    HITS.fetch_add(1, Ordering::Relaxed);
                    assert_eq!(*out, expected, "Fast polygon cleaning differs: {geom:?}");
                } else {
                    *out = expected;
                }
                return Ok(());
            }
            if hit {
                return Ok(());
            }
        }
        self.clean_full(geom, out)
    }
    /// Run the full reference path without the Rust fast path.
    pub fn clean_full(&mut self, geom: &[Draw], out: &mut DrawVec) -> Result<(), CleanError> {
        self.clean_scaled(geom, out, 16)
    }
    /// `clean_or_clip_poly(geom, 0, 0, false, false)`: the full path at scale 1
    /// (no scaling up), as tippecanoe-overzoom and tile-join `-O` clean.
    pub fn clean_unscaled(&mut self, geom: &[Draw], out: &mut DrawVec) -> Result<(), CleanError> {
        self.clean_scaled(geom, out, 1)
    }
    /// `clean_or_clip_poly(geom, z, buffer, true, false)`: the full path at
    /// scale 1, with tippecanoe's clip ring of tile `z` and its buffer (the
    /// postfilter of `vtmake build`). The input steps are a COPY of
    /// [`Cleaner::clean_scaled`], so that the usual path stays the same.
    #[cold]
    #[inline(never)]
    pub fn clean_clipped(&mut self, geom: &[Draw], out: &mut DrawVec, z: u32, buffer: i64) -> Result<(), CleanError> {
        let area: i64 = if z == 0 { 0xFFFF_FFFF } else { 1 << (32 - z) };
        let clip_buffer = buffer * area / 256;
        self.scratch.clear();
        let mut last = (0., 0.);
        for &d in geom {
            if d.x.abs_diff(0) > (1 << 38) || d.y.abs_diff(0) > (1 << 38) {
                return Err(CleanError);
            }
            let point = ((d.x as f64).round(), (d.y as f64).round());
            if d.op == VT_LINETO && point == last {
                continue;
            }
            self.scratch.push(d);
            if d.op != VT_CLOSEPATH {
                last = point;
            }
        }
        self.input.clear();
        let mut i = 0;
        while i < self.scratch.len() {
            let d = self.scratch[i];
            if d.op == VT_MOVETO {
                match self.scratch.get(i + 1).map(|d| d.op) {
                    None | Some(VT_MOVETO) => {
                        i += 1;
                        continue;
                    }
                    Some(VT_CLOSEPATH) => {
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            self.input.extend_from_slice(&[d.op as i64, d.x, d.y]);
            i += 1;
        }
        let mut ptr = std::ptr::null();
        let mut len = 0;
        // SAFETY: all slices and context are valid until this call returns.
        let status = unsafe { vtmake_wagyu_union_clip(self.context.as_ptr(), self.input.as_ptr(), self.input.len() / 3, -clip_buffer, area + clip_buffer, &mut ptr, &mut len) };
        if status != 0 {
            return Err(CleanError);
        }
        // SAFETY: the context owns len entries. Do not call the shim while borrowed.
        let data = if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len) } };
        // Each ring ends at its first point (`close_ring`, not called here).
        out.clear();
        let mut start = 0;
        for p in data.as_chunks::<3>().0 {
            let draw = Draw::new(p[0] as u8, p[1], p[2]);
            if draw.op == VT_MOVETO {
                if start < out.len() && (out[start].x, out[start].y) != (out[out.len() - 1].x, out[out.len() - 1].y) {
                    out.push(Draw::new(VT_LINETO, out[start].x, out[start].y));
                }
                start = out.len();
            }
            out.push(draw);
        }
        if start < out.len() && (out[start].x, out[start].y) != (out[out.len() - 1].x, out[out.len() - 1].y) {
            out.push(Draw::new(VT_LINETO, out[start].x, out[start].y));
        }
        Ok(())
    }
    /// The full path with a start scale: 16 (`try_scaling`, then 1 if needed)
    /// or 1 (no scaling, for world coordinates).
    fn clean_scaled(&mut self, geom: &[Draw], out: &mut DrawVec, mut scale: i32) -> Result<(), CleanError> {
        self.scratch.clear();
        let mut last = (0., 0.);
        for &d in geom {
            if d.x.abs_diff(0) > (1 << 38) || d.y.abs_diff(0) > (1 << 38) {
                return Err(CleanError);
            }
            let point = ((d.x as f64).round(), (d.y as f64).round());
            if d.op == VT_LINETO && point == last {
                continue;
            }
            self.scratch.push(d);
            if d.op != VT_CLOSEPATH {
                last = point;
            }
        }
        self.input.clear();
        let mut i = 0;
        while i < self.scratch.len() {
            let d = self.scratch[i];
            if d.op == VT_MOVETO {
                match self.scratch.get(i + 1).map(|d| d.op) {
                    None | Some(VT_MOVETO) => {
                        i += 1;
                        continue;
                    }
                    Some(VT_CLOSEPATH) => {
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            self.input.extend_from_slice(&[d.op as i64, d.x, d.y]);
            i += 1;
        }
        loop {
            let mut ptr = std::ptr::null();
            let mut len = 0;
            // SAFETY: all slices and context are valid until this call returns.
            let status = unsafe {
                vtmake_wagyu_union(
                    self.context.as_ptr(),
                    self.input.as_ptr(),
                    self.input.len() / 3,
                    scale,
                    &mut ptr,
                    &mut len,
                )
            };
            if status != 0 {
                return Err(CleanError);
            }
            // SAFETY: the context owns len entries. Do not call the shim while borrowed.
            let data = if len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(ptr, len) }
            };
            if scale != 1
                && data
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .any(|p| p[1] % 16 != 0 || p[2] % 16 != 0)
            {
                scale = 1;
                continue;
            }
            out.clear();
            let mut start: Option<Draw> = None;
            for p in data.as_chunks::<3>().0 {
                let draw = Draw::new(
                    p[0] as u8,
                    (p[1] as f64 / scale as f64).round() as i64,
                    (p[2] as f64 / scale as f64).round() as i64,
                );
                if draw.op == VT_MOVETO {
                    close_ring(out, start);
                    start = Some(draw);
                }
                out.push(draw);
            }
            close_ring(out, start);
            return Ok(());
        }
    }
    /// Split large inputs at the first outer ring after the midpoint
    /// (`coalesce_polygon(geom, true)`, tile coordinates).
    pub fn coalesce(&mut self, geom: &mut DrawVec) -> Result<(), CleanError> {
        self.coalesce_with(geom, true)
    }
    /// `coalesce_polygon(geom, false)`: world coordinates, no scaling up (the
    /// early clean of spec 7.5).
    pub fn coalesce_unscaled(&mut self, geom: &mut DrawVec) -> Result<(), CleanError> {
        self.coalesce_with(geom, false)
    }
    fn coalesce_with(&mut self, geom: &mut DrawVec, scale_up: bool) -> Result<(), CleanError> {
        if geom.len() >= 100_000 {
            let mut i = geom.len() / 2;
            while i < geom.len() {
                if geom[i].op == VT_MOVETO {
                    let mut j = i + 1;
                    while j < geom.len() && geom[j].op == VT_LINETO {
                        j += 1;
                    }
                    let area = ring_area(&geom[i..j]);
                    if area > 0. {
                        let mut second = geom.split_off(i);
                        self.coalesce_with(geom, scale_up)?;
                        self.coalesce_with(&mut second, scale_up)?;
                        geom.extend(second);
                        break;
                    }
                    i = j;
                } else {
                    i += 1;
                }
            }
        }
        let mut out = Vec::new();
        if scale_up {
            self.clean(geom, &mut out)?;
        } else {
            self.clean_scaled(geom, &mut out, 1)?;
        }
        *geom = out;
        Ok(())
    }
}
impl Drop for Cleaner {
    fn drop(&mut self) {
        unsafe {
            vtmake_wagyu_free(self.context.as_ptr());
        }
    }
}
/// Clean one polygon. Use `Cleaner` to reuse the buffers for many polygons.
pub fn clean_polygon(geom: &[Draw]) -> Result<DrawVec, CleanError> {
    let mut out = Vec::new();
    Cleaner::new().clean(geom, &mut out)?;
    Ok(out)
}

// Follow tippecanoe's double arithmetic and scaled sign check.
fn ring_area(ring: &[Draw]) -> f64 {
    let base = ring[0];
    let mut area = 0.;
    let mut overflow = false;
    let limit = ((1u64 << 53) - 1) as f64;
    for (i, a) in ring.iter().enumerate() {
        let b = ring[(i + 1) % ring.len()];
        area += (a.x - base.x) as f64 * (b.y - base.y) as f64;
        overflow |= area.abs() >= limit;
        area -= (a.y - base.y) as f64 * (b.x - base.x) as f64;
        overflow |= area.abs() >= limit;
    }
    area /= 2.;
    if overflow {
        let mut scale = 2;
        while scale < 1 << 30 {
            let mut scaled: f64 = 0.;
            let mut again = false;
            for (i, a) in ring.iter().enumerate() {
                let b = ring[(i + 1) % ring.len()];
                scaled += ((a.x - base.x) / scale) as f64 * ((b.y - base.y) / scale) as f64;
                if scaled.abs() >= limit {
                    again = true;
                    break;
                }
                scaled -= ((a.y - base.y) / scale) as f64 * ((b.x - base.x) / scale) as f64;
                if scaled.abs() >= limit {
                    again = true;
                    break;
                }
            }
            if !again {
                if (area < 0. && scaled > 0.) || (area > 0. && scaled < 0.) {
                    area = -area;
                }
                break;
            }
            scale *= 2;
        }
    }
    area
}

fn close_ring(out: &mut DrawVec, start: Option<Draw>) {
    if let (Some(first), Some(last)) = (start, out.last())
        && (first.x != last.x || first.y != last.y)
    {
        out.push(Draw::new(VT_LINETO, first.x, first.y));
    }
}

// A strict supporting half-plane test proves that this is one convex outer ring.
// The extra distance excludes hot pixels from all nonincident edges, even at
// scale 1. At scale 16, all output points thus remain multiples of 16.
fn convex_ring(geom: &[Draw], out: &mut DrawVec) -> bool {
    if !(4..=18).contains(&geom.len()) {
        return false;
    }
    // The full wrapper checks CLOSEPATH coordinates too.
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
    if !(4..=17).contains(&geom.len())
        || geom[0].op != VT_MOVETO
        || geom[1..].iter().any(|d| d.op != VT_LINETO)
    {
        return false;
    }
    let first = geom[0];
    let last = geom[geom.len() - 1];
    if first.x != last.x || first.y != last.y {
        return false;
    }
    let ring = &geom[..geom.len() - 1];
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let margin = dx.abs() + dy.abs();
        for (j, p) in ring.iter().enumerate() {
            if j == i || j == (i + 1) % n {
                continue;
            }
            if dx * (p.y - a.y) - dy * (p.x - a.x) <= margin {
                return false;
            }
        }
    }
    // The final left bound reaches the smallest y last. A horizontal top edge
    // ends at its right point. build_result walks prev and closes the ring.
    let start = (0..n).min_by_key(|&i| (ring[i].y, -ring[i].x)).unwrap();
    out.clear();
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
