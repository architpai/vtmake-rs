//! No-op removal, tile-edge vertices, Douglas–Peucker simplification,
//! scaling to tile units, and ring closing (tippecanoe `clip.cpp`,
//! `geometry.cpp`, `tile.cpp:simplify_feature`).
//!
//! The functions without `_opts` are the plain path (no geometry option).
//! The `_opts` functions have the same steps and the options of
//! [`GeomOpts`]. They are separate copies, so that the code of the plain
//! path does not change; the differential tests check both.

use crate::clip::clip;
use crate::num::{cvt_i64, round_shift};
use crate::polygon::{draw_lt, get_mp_area, part_end};
use crate::tile::GeomOpts;
use crate::visvalingam::visvalingam;
use vtmake_core::{Draw, DrawVec, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};

/// `remove_noop(geom, type, shift)`. In place.
///
/// 1. Remove a LINETO whose rounded point (`round(x / 2^shift)`) is the
///    same as the last kept MOVETO/LINETO (the start value is (0, 0)).
/// 2. Lines and polygons: remove a MOVETO that is last, or that is followed
///    by a MOVETO, or that is followed by a CLOSEPATH (both are removed).
/// 3. Lines only: remove a MOVETO (index > 1) at the same rounded point as
///    the LINETO before it.
pub fn remove_noop(geom: &mut DrawVec, t: GeomType, shift: u32) {
    // Pass 1: remove empty LINETOs.
    let (mut ox, mut oy) = (0i64, 0i64);
    let mut w = 0;
    for i in 0..geom.len() {
        let d = geom[i];
        let nx = round_shift(d.x, shift);
        let ny = round_shift(d.y, shift);
        if d.op == VT_LINETO && nx == ox && ny == oy {
            continue;
        }
        if d.op != VT_CLOSEPATH {
            ox = nx;
            oy = ny;
        }
        geom[w] = d;
        w += 1;
    }
    geom.truncate(w);

    if t == GeomType::Point {
        return;
    }

    // Pass 2: remove MOVETOs that draw nothing.
    let n = geom.len();
    let mut w = 0;
    let mut i = 0;
    while i < n {
        let d = geom[i];
        if d.op == VT_MOVETO {
            if i + 1 >= n {
                i += 1;
                continue;
            }
            let next = geom[i + 1].op;
            if next == VT_MOVETO {
                i += 1;
                continue;
            }
            if next == VT_CLOSEPATH {
                i += 2;
                continue;
            }
        }
        geom[w] = d;
        w += 1;
        i += 1;
    }
    geom.truncate(w);

    if t != GeomType::Line {
        return;
    }

    // Pass 3: remove MOVETOs that continue at the end of the previous part.
    let n = geom.len();
    let mut w = 0;
    let mut prev = Draw::new(0, 0, 0);
    for i in 0..n {
        let d = geom[i];
        let skip = i > 1
            && d.op == VT_MOVETO
            && prev.op == VT_LINETO
            && round_shift(prev.x, shift) == round_shift(d.x, shift)
            && round_shift(prev.y, shift) == round_shift(d.y, shift);
        prev = d;
        if !skip {
            geom[w] = d;
            w += 1;
        }
    }
    geom.truncate(w);
}

/// `impose_tile_boundaries(geom, extent)`: where a segment crosses the edge
/// of `[0, extent]`, add the clipped end points (if they are new) as
/// necessary LINETOs before the segment end point. The output is appended
/// to `out`.
pub fn impose_tile_boundaries(geom: &[Draw], extent: i64, out: &mut DrawVec) {
    out.reserve(geom.len());
    for i in 0..geom.len() {
        let cur = geom[i];
        if i > 0
            && cur.op == VT_LINETO
            && (geom[i - 1].op == VT_MOVETO || geom[i - 1].op == VT_LINETO)
        {
            let (mut x1, mut y1) = (geom[i - 1].x, geom[i - 1].y);
            let (mut x2, mut y2) = (cur.x, cur.y);
            let c = clip(&mut x1, &mut y1, &mut x2, &mut y2, 0, 0, extent, extent);
            if c > 1 {
                if x1 != geom[i - 1].x || y1 != geom[i - 1].y {
                    out.push(Draw {
                        op: VT_LINETO,
                        x: x1,
                        y: y1,
                        necessary: true,
                    });
                }
                if x2 != cur.x || y2 != cur.y {
                    out.push(Draw {
                        op: VT_LINETO,
                        x: x2,
                        y: y2,
                        necessary: true,
                    });
                }
            }
        }
        out.push(cur);
    }
}

/// `distance_from_line`: distance from P to the segment A–B. The products
/// are made in integers (after division by 4 and 8) as in the C++ code,
/// and the result is rounded to 1/16.
#[inline]
pub fn distance_from_line(px: i64, py: i64, ax: i64, ay: i64, bx: i64, by: i64) -> f64 {
    let p2x = bx - ax;
    let p2y = by - ay;

    let something = ((p2x / 4)
        .wrapping_mul(p2x / 8)
        .wrapping_add((p2y / 4).wrapping_mul(p2y / 8))) as f64
        * 32.0;
    let mut u = if something == 0.0 {
        0.0
    } else {
        (((px - ax) / 4)
            .wrapping_mul(p2x / 8)
            .wrapping_add(((py - ay) / 4).wrapping_mul(p2y / 8))) as f64
            * 32.0
            / something
    };

    #[allow(clippy::manual_clamp)] // The C++ tests, also for -0.0 and NaN.
    if u >= 1.0 {
        u = 1.0;
    } else if u <= 0.0 {
        u = 0.0;
    }

    let x = ax as f64 + u * p2x as f64;
    let y = ay as f64 + u * p2y as f64;
    let dx = x - px as f64;
    let dy = y - py as f64;

    ((dx * dx + dy * dy).sqrt() * 16.0).round() / 16.0
}

/// Reusable buffers for [`simplify_lines`] and [`douglas_peucker`].
#[derive(Default, Debug)]
pub struct SimplifyScratch {
    stack: Vec<(u32, u32)>,
    tmp: DrawVec,
}

/// `douglas_peucker(geom, start, n, e, kept, retain, false)`: mark the
/// points of `geom[start..start + n]` to keep (`necessary`). The first and
/// last point must be marked already.
///
/// This copies the tippecanoe variant exactly: a LIFO stack of segments,
/// the scan direction from the (y, x) order of the end points, ties to the
/// smaller point, and `retain` (keep at least `retain` points).
pub fn douglas_peucker(
    geom: &mut [Draw],
    start: usize,
    n: usize,
    e: f64,
    mut kept: usize,
    retain: usize,
    stack: &mut Vec<(u32, u32)>,
) {
    let g = &mut geom[start..start + n];
    assert!(
        g[0].necessary && g[n - 1].necessary,
        "endpoints not marked necessary"
    );

    stack.clear();
    let mut prev = 0u32;
    for (here, d) in g.iter().enumerate().skip(1) {
        if d.necessary {
            stack.push((prev, here as u32));
            prev = here as u32;
        }
    }

    while let Some((first, second)) = stack.pop() {
        let (first, second) = (first as usize, second as usize);
        let mut max_distance = -1.0f64;
        let mut farthest;

        let forward = draw_lt(&g[first], &g[second]);
        if forward {
            farthest = first;
            let (a, b) = (g[first], g[second]);
            for i in first + 1..second {
                let distance = distance_from_line(g[i].x, g[i].y, a.x, a.y, b.x, b.y).abs();
                if (distance > e || kept < retain)
                    && (distance > max_distance
                        || (distance == max_distance && draw_lt(&g[i], &g[farthest])))
                {
                    farthest = i;
                    max_distance = distance;
                }
            }
        } else {
            farthest = second;
            let (a, b) = (g[second], g[first]);
            for i in (first + 1..second).rev() {
                let distance = distance_from_line(g[i].x, g[i].y, a.x, a.y, b.x, b.y).abs();
                if (distance > e || kept < retain)
                    && (distance > max_distance
                        || (distance == max_distance && draw_lt(&g[i], &g[farthest])))
                {
                    farthest = i;
                    max_distance = distance;
                }
            }
        }

        if max_distance >= 0.0 {
            g[farthest].necessary = true;
            kept += 1;

            if forward {
                if farthest - first > 1 {
                    stack.push((first as u32, farthest as u32));
                }
                if second - farthest > 1 {
                    stack.push((farthest as u32, second as u32));
                }
            } else {
                if second - farthest > 1 {
                    stack.push((farthest as u32, second as u32));
                }
                if farthest - first > 1 {
                    stack.push((first as u32, farthest as u32));
                }
            }
        }
    }
}

/// [`douglas_peucker`] with `prevent_simplify_shared_nodes` (`-pn`): each
/// segment between points that are marked already lowers `retain` by 1.
#[allow(clippy::too_many_arguments)]
pub fn douglas_peucker_opts(
    geom: &mut [Draw],
    start: usize,
    n: usize,
    e: f64,
    mut kept: usize,
    mut retain: usize,
    shared_nodes: bool,
    stack: &mut Vec<(u32, u32)>,
) {
    let g = &mut geom[start..start + n];
    assert!(
        g[0].necessary && g[n - 1].necessary,
        "endpoints not marked necessary"
    );

    stack.clear();
    let mut prev = 0u32;
    for (here, d) in g.iter().enumerate().skip(1) {
        if d.necessary {
            stack.push((prev, here as u32));
            prev = here as u32;
            if shared_nodes && retain > 0 {
                retain -= 1;
            }
        }
    }

    while let Some((first, second)) = stack.pop() {
        let (first, second) = (first as usize, second as usize);
        let mut max_distance = -1.0f64;
        let mut farthest;

        let forward = draw_lt(&g[first], &g[second]);
        if forward {
            farthest = first;
            let (a, b) = (g[first], g[second]);
            for i in first + 1..second {
                let distance = distance_from_line(g[i].x, g[i].y, a.x, a.y, b.x, b.y).abs();
                if (distance > e || kept < retain)
                    && (distance > max_distance
                        || (distance == max_distance && draw_lt(&g[i], &g[farthest])))
                {
                    farthest = i;
                    max_distance = distance;
                }
            }
        } else {
            farthest = second;
            let (a, b) = (g[second], g[first]);
            for i in (first + 1..second).rev() {
                let distance = distance_from_line(g[i].x, g[i].y, a.x, a.y, b.x, b.y).abs();
                if (distance > e || kept < retain)
                    && (distance > max_distance
                        || (distance == max_distance && draw_lt(&g[i], &g[farthest])))
                {
                    farthest = i;
                    max_distance = distance;
                }
            }
        }

        if max_distance >= 0.0 {
            g[farthest].necessary = true;
            kept += 1;

            if forward {
                if farthest - first > 1 {
                    stack.push((first as u32, farthest as u32));
                }
                if second - farthest > 1 {
                    stack.push((farthest as u32, second as u32));
                }
            } else {
                if second - farthest > 1 {
                    stack.push((farthest as u32, second as u32));
                }
                if farthest - first > 1 {
                    stack.push((first as u32, farthest as u32));
                }
            }
        }
    }
}

/// The shared nodes for the simplification with `-pn`: the edge nodes of the
/// tile (tile-relative, in the order of tippecanoe's vector: the lookup is
/// its `std::lower_bound`) and the global nodes (`encode_vertex` keys,
/// sorted). `tx`, `ty` is the tile.
#[derive(Clone, Copy, Debug)]
pub struct NodeCtx<'a> {
    pub tile: &'a [Draw],
    pub global: &'a [u64],
    pub tx: u32,
    pub ty: u32,
}

impl NodeCtx<'_> {
    pub const EMPTY: NodeCtx<'static> = NodeCtx { tile: &[], global: &[], tx: 0, ty: 0 };

    /// Is (x, y) (tile-relative at zoom z) a shared node?
    fn contains(&self, x: i64, y: i64, z: u32) -> bool {
        // libstdc++ `std::lower_bound` with `draw::operator<` (y, then x).
        let (mut first, mut len) = (0usize, self.tile.len());
        while len > 0 {
            let half = len >> 1;
            let m = &self.tile[first + half];
            if m.y < y || (m.y == y && m.x < x) {
                first += half + 1;
                len -= half + 1;
            } else {
                len = half;
            }
        }
        if first < self.tile.len() && self.tile[first].x == x && self.tile[first].y == y {
            return true;
        }
        if !self.global.is_empty() {
            let (mut gx, mut gy) = (x, y);
            if z != 0 {
                gx += (self.tx as i64) << (32 - z);
                gy += (self.ty as i64) << (32 - z);
            }
            let key = encode_vertex(gx as u32, gy as u32);
            return self.global.binary_search(&key).is_ok();
        }
        false
    }
}

/// `projection.cpp:encode_vertex`.
#[inline]
pub const fn encode_vertex(wx: u32, wy: u32) -> u64 {
    ((wx as u64) << 32) | wy as u64
}

/// `simplify_lines` without shared nodes and without Visvalingam. In place.
///
/// 1. Mark MOVETO (and any op that is not LINETO) as necessary.
/// 2. If `mark_tile_bounds`: [`impose_tile_boundaries`] with
///    `extent = 2^(32-z)`.
/// 3. For each part: mark both ends, then [`douglas_peucker`] with
///    tolerance `2^(32-detail-z) * simplification`.
/// 4. Keep only the necessary points.
pub fn simplify_lines(
    geom: &mut DrawVec,
    z: u32,
    detail: u32,
    mark_tile_bounds: bool,
    simplification: f64,
    retain: usize,
    scratch: &mut SimplifyScratch,
) {
    let res = 1i32 << (32 - detail - z);
    let area = 1i64 << (32 - z);

    for d in geom.iter_mut() {
        d.necessary = d.op != VT_LINETO;
    }

    // If all points are inside [0, area] (inclusive), no segment crosses
    // the tile edge and `impose_tile_boundaries` changes nothing. This test
    // skips the copy; the result is the same.
    if mark_tile_bounds
        && geom
            .iter()
            .any(|d| d.x < 0 || d.y < 0 || d.x > area || d.y > area)
    {
        scratch.tmp.clear();
        impose_tile_boundaries(geom, area, &mut scratch.tmp);
        std::mem::swap(geom, &mut scratch.tmp);
    }

    let e = res as f64 * simplification;
    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let j = part_end(geom, i);
            geom[i].necessary = true;
            geom[j - 1].necessary = true;
            if j - i > 1 {
                douglas_peucker(geom, i, j - i, e, 2, retain, &mut scratch.stack);
            }
            i = j;
        } else {
            i += 1;
        }
    }

    geom.retain(|d| d.necessary);
}

/// [`simplify_lines`] with the options in `opts`: Visvalingam (`-av`)
/// instead of Douglas–Peucker, and with `-pn` the shared nodes in `nodes`
/// stay. In place.
#[allow(clippy::too_many_arguments)]
pub fn simplify_lines_opts(
    geom: &mut DrawVec,
    z: u32,
    detail: u32,
    mark_tile_bounds: bool,
    simplification: f64,
    retain: usize,
    opts: &GeomOpts,
    nodes: &NodeCtx,
    scratch: &mut SimplifyScratch,
) {
    let res = 1i32 << (32 - detail - z);
    let area = 1i64 << (32 - z);

    for d in geom.iter_mut() {
        d.necessary = d.op != VT_LINETO;
    }
    if opts.shared_nodes {
        for d in geom.iter_mut() {
            if nodes.contains(d.x, d.y, z) {
                d.necessary = true;
            }
        }
    }

    // If all points are inside [0, area] (inclusive), no segment crosses
    // the tile edge and `impose_tile_boundaries` changes nothing. This test
    // skips the copy; the result is the same.
    if mark_tile_bounds
        && geom
            .iter()
            .any(|d| d.x < 0 || d.y < 0 || d.x > area || d.y > area)
    {
        scratch.tmp.clear();
        impose_tile_boundaries(geom, area, &mut scratch.tmp);
        std::mem::swap(geom, &mut scratch.tmp);
    }

    let e = res as f64 * simplification;
    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let j = part_end(geom, i);
            geom[i].necessary = true;
            geom[j - 1].necessary = true;
            if j - i > 1 {
                if opts.visvalingam {
                    // The empirical map from Douglas–Peucker tolerances to
                    // Visvalingam areas of tippecanoe.
                    let sim = simplification * (0.1596 * z as f64 + 0.878);
                    let scale = (res as f64 * sim) * (res as f64 * sim);
                    let scale = (1.002 * scale.ln() + 0.3043).exp();
                    visvalingam(geom, i, j, scale, retain);
                } else {
                    douglas_peucker_opts(geom, i, j - i, e, 2, retain, opts.shared_nodes, &mut scratch.stack);
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }

    geom.retain(|d| d.necessary);
}

/// `geometry.cpp:stairstep` (`-aL`): round to tile units, replace each
/// segment by horizontal and vertical steps (Bresenham), and scale back to
/// world units. In place.
pub fn stairstep(geom: &mut DrawVec, z: u32, detail: u32, out: &mut DrawVec) {
    let scale = (1i32 << (32 - detail - z)) as f64;
    for d in geom.iter_mut() {
        d.x = cvt_i64((d.x as f64 / scale).round());
        d.y = cvt_i64((d.y as f64 / scale).round());
    }
    out.clear();
    let mut tmp: DrawVec = Vec::new();
    for g in geom.iter() {
        if g.op == VT_MOVETO {
            out.push(*g);
        } else if let Some(last) = out.last() {
            let (mut x0, mut y0) = (last.x, last.y);
            let (mut x1, mut y1) = (g.x, g.y);
            let swap = y0 < y1;
            if swap {
                std::mem::swap(&mut x0, &mut x1);
                std::mem::swap(&mut y0, &mut y1);
            }
            let (mut xx, mut yy) = (x0, y0);
            let dx = (x1 - x0).abs();
            let sx = if x0 < x1 { 1 } else { -1 };
            let dy = (y1 - y0).abs();
            let sy = if y0 < y1 { 1 } else { -1 };
            let mut err = (if dx > dy { dx } else { -dy }) / 2;
            let mut last = -1;
            tmp.clear();
            tmp.push(Draw::new(VT_LINETO, xx, yy));
            while xx != x1 || yy != y1 {
                let e2 = err;
                if e2 > -dx {
                    err -= dy;
                    xx += sx;
                    let d = Draw::new(VT_LINETO, xx, yy);
                    if last == 1 { *tmp.last_mut().unwrap() = d } else { tmp.push(d) }
                    last = 1;
                }
                if e2 < dy {
                    err += dx;
                    yy += sy;
                    let d = Draw::new(VT_LINETO, xx, yy);
                    if last == 2 { *tmp.last_mut().unwrap() = d } else { tmp.push(d) }
                    last = 2;
                }
            }
            if swap {
                out.extend(tmp.iter().rev());
            } else {
                out.extend_from_slice(&tmp);
            }
        } else {
            eprintln!("Can't happen: stairstepping lineto with no moveto");
            std::process::exit(crate::EXIT_IMPOSSIBLE);
        }
    }
    let m = (1i32 << (32 - detail - z)) as i64;
    for d in out.iter_mut() {
        d.x = d.x.wrapping_mul(m);
        d.y = d.y.wrapping_mul(m);
    }
    std::mem::swap(geom, out);
}

/// `tile.cpp:simplify_feature` for the flags of the spec (no `-pS`, no
/// shared borders, no coalescing, no Visvalingam, no line reversal).
/// In place, world units (tile-relative).
///
/// Returns the polygon area before simplification (`get_mp_area`), or 0 for
/// lines and points. `revive_polygon` needs this value.
///
/// Note: the C++ code has a test "polygon with fewer than 3 points: keep
/// the unsimplified geometry". That test has no effect, because
/// `simplify_lines` changes its argument in place. This port copies the
/// real behaviour (the simplified result is kept).
pub fn simplify_feature(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    detail: u32,
    simplification: f64,
    reduced: bool,
    scratch: &mut SimplifyScratch,
) -> f64 {
    let area = if t == GeomType::Polygon {
        get_mp_area(geom)
    } else {
        0.0
    };

    if (t == GeomType::Line || t == GeomType::Polygon) && !reduced {
        if t == GeomType::Line {
            remove_noop(geom, t, 32 - z - detail);
        }
        let retain = if t == GeomType::Polygon { 4 } else { 0 };
        simplify_lines(geom, z, detail, true, simplification, retain, scratch);
    }

    area
}

/// [`simplify_feature`] with the geometry options in `opts` (`-ps`, `-pS`,
/// `-aL`, `-av`, `-pc`, `-pD`, `-pn`). In place.
#[allow(clippy::too_many_arguments)]
pub fn simplify_feature_opts(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    detail: u32,
    simplification: f64,
    reduced: bool,
    opts: &GeomOpts,
    nodes: &NodeCtx,
    scratch: &mut SimplifyScratch,
) -> f64 {
    let low = z < opts.maxzoom;
    if opts.grid_low_zooms && low {
        stairstep(geom, z, detail, &mut scratch.tmp);
    }

    let area = if t == GeomType::Polygon {
        get_mp_area(geom)
    } else {
        0.0
    };

    let simplify = !(opts.no_simplify || (!low && opts.no_simplify_maxzoom) || (low && opts.grid_low_zooms));
    if (t == GeomType::Line || t == GeomType::Polygon) && simplify && !reduced {
        if t == GeomType::Line && !opts.shared_nodes {
            remove_noop(geom, t, 32 - z - detail);
        }
        // With `-ab`, `find_common_edges` simplified the polygons before.
        if !(t == GeomType::Polygon && opts.shared_borders) {
            let retain = if t == GeomType::Polygon { 4 } else { 0 };
            let mark = !(opts.no_clipping || opts.no_duplication);
            simplify_lines_opts(geom, z, detail, mark, simplification, retain, opts, nodes, scratch);
        }
    }

    area
}

/// `to_tile_scale(geom, z, detail)`: world units to tile units,
/// `round(x / 2^(32-detail-z))`. In place.
pub fn to_tile_scale(geom: &mut [Draw], z: u32, detail: u32) {
    let s = 32 - detail as i32 - z as i32;
    if s < 0 {
        let m = (1i64 << -s) as f64;
        for d in geom {
            d.x = cvt_i64((d.x as f64 * m).round());
            d.y = cvt_i64((d.y as f64 * m).round());
        }
    } else {
        let s = s as u32;
        for d in geom {
            d.x = round_shift(d.x, s);
            d.y = round_shift(d.y, s);
        }
    }
}

/// `close_poly`: each ring becomes MOVETO, the LINETOs without the closing
/// point, and CLOSEPATH (0, 0). Ops outside a ring are removed. In place.
pub fn close_poly(geom: &mut DrawVec) {
    let n = geom.len();
    let mut w = 0;
    let mut i = 0;
    while i < n {
        if geom[i].op == VT_MOVETO {
            let j = part_end(geom, i);
            // The output is never longer than the input, so `w <= i`.
            geom.copy_within(i..j - 1, w);
            w += j - 1 - i;
            geom[w] = Draw::new(VT_CLOSEPATH, 0, 0);
            w += 1;
            i = j;
        } else {
            i += 1;
        }
    }
    geom.truncate(w);
}
