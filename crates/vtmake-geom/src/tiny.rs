//! Tiny polygon reduction, the "too small" test, and polygon revival
//! (tippecanoe `clip.cpp`, `tile.cpp:revive_polygon`).

use crate::num::{cvt_i32, cvt_i64, round_shift};
use crate::polygon::{get_area, part_end};
use vtmake_core::{Draw, DrawVec, VT_LINETO, VT_MOVETO};

/// The tiny polygon accumulator of one tile (`accum_area` in
/// `tile.cpp:write_tile`). One value is shared by all polygon features of
/// the tile (all layers), in feature order. Make a new one for each tile.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TinyPolyAccum {
    pub area: f64,
}

/// The flags that [`reduce_tiny_poly`] returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TinyPolyResult {
    /// At least one ring was copied, so the feature must be simplified.
    pub still_needs_simplification: bool,
    /// At least one ring was added to the accumulator
    /// (`strategies[z].tiny_polygons` counts these features).
    pub reduced_away: bool,
}

/// `reduce_tiny_poly`: replace rings smaller than `tiny_polygon_size`
/// tile units squared by accumulated squares. Coordinates are tile-relative
/// world units. The output is appended to `out`.
///
/// For each ring with area A (from `get_area`), with
/// `pixel = 2^(32-detail-z) * tiny_polygon_size`:
/// - A == 0: drop the ring.
/// - (A > 0 and A <= pixel²) or (A < 0 and the last outer ring was not
///   kept): add A to the accumulator. If A > 0 and the accumulator is more
///   than pixel², emit a `pixel` square at the first point of the ring and
///   subtract pixel².
/// - Else: copy the ring.
pub fn reduce_tiny_poly(
    geom: &[Draw],
    z: u32,
    detail: u32,
    tiny_polygon_size: f64,
    accum: &mut TinyPolyAccum,
    out: &mut DrawVec,
) -> TinyPolyResult {
    let pixel = (1i64 << (32 - detail - z)) as f64 * tiny_polygon_size;
    let pixel2 = pixel * pixel;
    let half = pixel / 2.0;

    let mut res = TinyPolyResult::default();
    let mut included_last_outer = false;

    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let j = part_end(geom, i);
            let area = get_area(geom, i, j);

            if area != 0.0 {
                if (area > 0.0 && area <= pixel2) || (area < 0.0 && !included_last_outer) {
                    accum.area += area;
                    res.reduced_away = true;

                    if area > 0.0 && accum.area > pixel2 {
                        let x = geom[i].x as f64;
                        let y = geom[i].y as f64;
                        let c = |v: f64| cvt_i64(v);
                        out.push(Draw::new(VT_MOVETO, c(x - half), c(y - half)));
                        out.push(Draw::new(VT_LINETO, c(x - half + pixel), c(y - half)));
                        out.push(Draw::new(
                            VT_LINETO,
                            c(x - half + pixel),
                            c(y - half + pixel),
                        ));
                        out.push(Draw::new(VT_LINETO, c(x - half), c(y - half + pixel)));
                        out.push(Draw::new(VT_LINETO, c(x - half), c(y - half)));
                        accum.area -= pixel2;
                    }

                    if area > 0.0 {
                        included_last_outer = false;
                    }
                } else {
                    out.extend_from_slice(&geom[i..j]);
                    res.still_needs_simplification = true;
                    if area > 0.0 {
                        included_last_outer = true;
                    }
                }
            }
            i = j;
        } else {
            // tippecanoe prints "how did we get here" and copies the draw.
            out.push(geom[i]);
            i += 1;
        }
    }
    res
}

/// `line_is_too_small`: true if the geometry is empty, or if every point
/// rounds (to tile units) to the MOVETO point of its part.
pub fn line_is_too_small(geom: &[Draw], z: u32, detail: u32) -> bool {
    if geom.is_empty() {
        return true;
    }
    let s = 32 - detail - z;
    let (mut x, mut y) = (0i64, 0i64);
    for g in geom {
        let gx = round_shift(g.x, s);
        let gy = round_shift(g.y, s);
        if g.op == VT_MOVETO {
            x = gx;
            y = gy;
        } else if gx != x || gy != y {
            return false;
        }
    }
    true
}

/// The point sums that [`revive_polygon`] needs from the geometry before
/// polygon cleaning. Make it before cleaning, so no copy of the geometry
/// is necessary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReviveInfo {
    pub sx: i64,
    pub sy: i64,
    pub n: i64,
}

impl ReviveInfo {
    /// Sum the MOVETO/LINETO points of `geom` (tile units).
    pub fn of(geom: &[Draw]) -> Self {
        let mut r = ReviveInfo::default();
        for d in geom {
            if d.op == VT_MOVETO || d.op == VT_LINETO {
                r.sx = r.sx.wrapping_add(d.x);
                r.sy = r.sy.wrapping_add(d.y);
                r.n += 1;
            }
        }
        r
    }
}

/// `revive_polygon`: if cleaning removed a polygon with world area
/// `area > 0`, make a rectangle with about the same area (tile units) at the
/// mean point of the geometry before cleaning. The output is appended to
/// `out` (nothing if the area in tile units is 0 or there are no points).
pub fn revive_polygon(info: &ReviveInfo, area: f64, z: u32, detail: u32, out: &mut DrawVec) {
    let divisor = 1i64 << (32 - detail - z);
    let area = area / divisor.wrapping_mul(divisor) as f64;
    if area == 0.0 {
        return;
    }

    let height = cvt_i32(area.sqrt().ceil());
    let mut width = cvt_i32((area / height as f64).round());
    if width == 0 {
        width = 1;
    }

    if info.n > 0 {
        let sx = info.sx / info.n;
        let sy = info.sy / info.n;
        let (w, h) = (width as i64, height as i64);
        let (hw, hh) = ((width / 2) as i64, (height / 2) as i64);
        out.push(Draw::new(VT_MOVETO, sx - hw, sy - hh));
        out.push(Draw::new(VT_LINETO, sx - hw + w, sy - hh));
        out.push(Draw::new(VT_LINETO, sx - hw + w, sy - hh + h));
        out.push(Draw::new(VT_LINETO, sx - hw, sy - hh + h));
        out.push(Draw::new(VT_LINETO, sx - hw, sy - hh));
    }
}
