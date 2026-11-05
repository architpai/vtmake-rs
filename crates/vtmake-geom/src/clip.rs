//! Clipping to a tile plus buffer (tippecanoe `clip.cpp`, `geometry.cpp`).
//!
//! All functions here work in tile-relative world coordinates: the tile
//! proper is `[0, 2^(32-z)]` on both axes.

use crate::num::cvt_i64;
use crate::polygon::part_end;
use vtmake_core::{Draw, DrawVec, VT_LINETO, VT_MOVETO};

/// The buffer in world units at zoom `z`: `buffer * 2^(32-z) / 256`.
/// `buffer` is in 1/256 of a tile width (tippecanoe `--buffer`).
#[inline(always)]
pub const fn buffer_world(z: u32, buffer: i64) -> i64 {
    buffer * (1i64 << (32 - z)) / 256
}

/// `quick_check`: compare the bbox with the tile at `z`.
///
/// - 1: the bbox is strictly inside the tile proper (no clip is necessary).
/// - 3: the bbox is strictly inside the tile plus buffer.
/// - 2: the bbox overlaps the edge of the tile plus buffer.
/// - 0: the bbox is fully outside the tile plus buffer.
#[inline]
pub fn quick_check(bbox: &[i64; 4], z: u32, buffer: i64) -> i32 {
    let mut min: i64 = 0;
    let mut area: i64 = 1i64 << (32 - z);

    if bbox[0] > min && bbox[1] > min && bbox[2] < area && bbox[3] < area {
        return 1;
    }

    let b = buffer * area / 256;
    min -= b;
    area += b;

    if bbox[0] > min && bbox[1] > min && bbox[2] < area && bbox[3] < area {
        return 3;
    }
    if bbox[0] > area || bbox[1] > area {
        return 0;
    }
    if bbox[2] < min || bbox[3] < min {
        return 0;
    }
    2
}

/// `clip_point` with explicit bounds: keep the points inside the box
/// (inclusive). In place.
#[inline]
pub fn clip_point_box(geom: &mut DrawVec, minx: i64, miny: i64, maxx: i64, maxy: i64) {
    geom.retain(|d| d.x >= minx && d.y >= miny && d.x <= maxx && d.y <= maxy);
}

/// `clip_point(geom, z, buffer)`. In place.
#[inline]
pub fn clip_point(geom: &mut DrawVec, z: u32, buffer: i64) {
    let area = 1i64 << (32 - z);
    let b = buffer * area / 256;
    clip_point_box(geom, -b, -b, area + b, area + b);
}

const INSIDE: u8 = 0;
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const BOTTOM: u8 = 4;
const TOP: u8 = 8;

#[inline(always)]
fn out_code(x: i64, y: i64, xmin: i64, ymin: i64, xmax: i64, ymax: i64) -> u8 {
    let mut code = INSIDE;
    if x < xmin {
        code |= LEFT;
    } else if x > xmax {
        code |= RIGHT;
    }
    if y < ymin {
        code |= BOTTOM;
    } else if y > ymax {
        code |= TOP;
    }
    code
}

/// `clip`: integer Cohen–Sutherland clip of one segment to a box
/// (inclusive). The intersection uses 64-bit truncating division (wrapping
/// on overflow, like the C++ code on x86-64).
///
/// Returns 0 if the segment is fully outside, 1 if it is unchanged, 2 if
/// an end point moved.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn clip(
    x0: &mut i64,
    y0: &mut i64,
    x1: &mut i64,
    y1: &mut i64,
    xmin: i64,
    ymin: i64,
    xmax: i64,
    ymax: i64,
) -> i32 {
    let mut oc0 = out_code(*x0, *y0, xmin, ymin, xmax, ymax);
    let mut oc1 = out_code(*x1, *y1, xmin, ymin, xmax, ymax);
    let mut changed = 0;

    loop {
        if oc0 | oc1 == 0 {
            return changed + 1;
        } else if oc0 & oc1 != 0 {
            return 0;
        }

        let mut x = *x0;
        let mut y = *y0;
        let out = if oc0 != 0 { oc0 } else { oc1 };

        if out & TOP != 0 {
            x = x0.wrapping_add((*x1 - *x0).wrapping_mul(ymax - *y0) / (*y1 - *y0));
            y = ymax;
        } else if out & BOTTOM != 0 {
            x = x0.wrapping_add((*x1 - *x0).wrapping_mul(ymin - *y0) / (*y1 - *y0));
            y = ymin;
        } else if out & RIGHT != 0 {
            y = y0.wrapping_add((*y1 - *y0).wrapping_mul(xmax - *x0) / (*x1 - *x0));
            x = xmax;
        } else if out & LEFT != 0 {
            y = y0.wrapping_add((*y1 - *y0).wrapping_mul(xmin - *x0) / (*x1 - *x0));
            x = xmin;
        }

        if out == oc0 {
            *x0 = x;
            *y0 = y;
            oc0 = out_code(*x0, *y0, xmin, ymin, xmax, ymax);
        } else {
            *x1 = x;
            *y1 = y;
            oc1 = out_code(*x1, *y1, xmin, ymin, xmax, ymax);
        }
        changed = 1;
    }
}

/// `clip_lines` with explicit bounds. The output is appended to `out`.
///
/// For each segment (from point i-1 to point i): if it is clipped, push
/// `MOVETO p0'`, `LINETO p1'`, `MOVETO p_i`; if it is unchanged, push `p_i`;
/// if it is fully outside, push `MOVETO p_i`. Call [`crate::remove_noop`]
/// after this (tippecanoe does).
pub fn clip_lines_box(
    geom: &[Draw],
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
    out: &mut DrawVec,
) {
    out.reserve(geom.len());
    for i in 0..geom.len() {
        let cur = geom[i];
        if i > 0
            && (geom[i - 1].op == VT_MOVETO || geom[i - 1].op == VT_LINETO)
            && cur.op == VT_LINETO
        {
            let mut x1 = geom[i - 1].x;
            let mut y1 = geom[i - 1].y;
            let mut x2 = cur.x;
            let mut y2 = cur.y;

            let c = clip(&mut x1, &mut y1, &mut x2, &mut y2, minx, miny, maxx, maxy);
            if c > 1 {
                out.push(Draw::new(VT_MOVETO, x1, y1));
                out.push(Draw::new(VT_LINETO, x2, y2));
                out.push(Draw::new(VT_MOVETO, cur.x, cur.y));
            } else if c == 1 {
                out.push(cur);
            } else {
                out.push(Draw::new(VT_MOVETO, cur.x, cur.y));
            }
        } else {
            out.push(cur);
        }
    }
}

/// `clip_lines(geom, z, buffer)`. The output is appended to `out`.
#[inline]
pub fn clip_lines(geom: &[Draw], z: u32, buffer: i64, out: &mut DrawVec) {
    let area = 1i64 << (32 - z);
    let b = buffer * area / 256;
    clip_lines_box(geom, -b, -b, area + b, area + b, out);
}

/// Reusable buffers for [`simple_clip_poly`].
#[derive(Default, Debug)]
pub struct PolyClipScratch {
    a: Vec<(f64, f64)>,
    b: Vec<(f64, f64)>,
}

/// One Sutherland–Hodgman pass for one edge. The tests are strict: a point
/// exactly on the edge is outside.
#[inline(always)]
fn clip_edge<I, X>(input: &[(f64, f64)], out: &mut Vec<(f64, f64)>, inside: I, intersect: X)
where
    I: Fn((f64, f64)) -> bool,
    X: Fn((f64, f64), (f64, f64)) -> (f64, f64),
{
    out.clear();
    let mut s = input[input.len() - 1];
    let mut s_in = inside(s);
    for &e in input {
        let e_in = inside(e);
        if !s_in {
            if e_in {
                out.push(intersect(s, e));
                out.push(e);
            }
        } else if !e_in {
            out.push(intersect(s, e));
        } else {
            out.push(e);
        }
        s = e;
        s_in = e_in;
    }
}

/// `clip_poly1` with `prevent_simplify_shared_nodes = false`: clip one
/// ring (with its closing point) in `double`. The result is in `scratch.a`.
/// It is empty, or closed with at least 3 points.
fn clip_ring(scratch: &mut PolyClipScratch, minx: i64, miny: i64, maxx: i64, maxy: i64) {
    let (minx, miny, maxx, maxy) = (minx as f64, miny as f64, maxx as f64, maxy as f64);
    let PolyClipScratch { a, b } = scratch;

    // Edge 0: top.
    if !a.is_empty() {
        clip_edge(
            a,
            b,
            |d| d.1 > miny,
            |a, b| (a.0 + (b.0 - a.0) * (miny - a.1) / (b.1 - a.1), miny),
        );
    } else {
        b.clear();
    }
    // Edge 1: right.
    if !b.is_empty() {
        clip_edge(
            b,
            a,
            |d| d.0 < maxx,
            |a, b| (maxx, a.1 + (b.1 - a.1) * (maxx - a.0) / (b.0 - a.0)),
        );
    } else {
        a.clear();
    }
    // Edge 2: bottom.
    if !a.is_empty() {
        clip_edge(
            a,
            b,
            |d| d.1 < maxy,
            |a, b| (a.0 + (b.0 - a.0) * (maxy - a.1) / (b.1 - a.1), maxy),
        );
    } else {
        b.clear();
    }
    // Edge 3: left.
    if !b.is_empty() {
        clip_edge(
            b,
            a,
            |d| d.0 > minx,
            |a, b| (minx, a.1 + (b.1 - a.1) * (minx - a.0) / (b.0 - a.0)),
        );
    } else {
        a.clear();
    }

    if !a.is_empty() {
        let first = a[0];
        let last = a[a.len() - 1];
        if first.0 != last.0 || first.1 != last.1 {
            a.push(first);
        }
        if a.len() < 3 {
            a.clear();
        }
    }
}

/// `simple_clip_poly` with explicit bounds and
/// `prevent_simplify_shared_nodes = false`. The output is appended to
/// `out`. Call [`crate::remove_noop`] after this (tippecanoe does).
///
/// Each ring is clipped in `double` with strict tests, then rounded.
/// tippecanoe exits if an op is not MOVETO/LINETO at a ring start; this
/// function panics.
pub fn simple_clip_poly_box(
    geom: &[Draw],
    minx: i64,
    miny: i64,
    maxx: i64,
    maxy: i64,
    out: &mut DrawVec,
    scratch: &mut PolyClipScratch,
) {
    let mut i = 0;
    while i < geom.len() {
        assert!(
            geom[i].op == VT_MOVETO,
            "Unexpected operation in polygon {}",
            geom[i].op
        );
        let j = part_end(geom, i);

        scratch.a.clear();
        scratch
            .a
            .extend(geom[i..j].iter().map(|d| (d.x as f64, d.y as f64)));
        clip_ring(scratch, minx, miny, maxx, maxy);

        for (k, p) in scratch.a.iter().enumerate() {
            out.push(Draw::new(
                if k == 0 { VT_MOVETO } else { VT_LINETO },
                cvt_i64(p.0.round()),
                cvt_i64(p.1.round()),
            ));
        }
        i = j;
    }
}

/// `clip_poly1` with `prevent_simplify_shared_nodes = true` (`-pn`): clip
/// one ring (in `scratch.a`, with its closing point) to the buffer box
/// `min..max`. Where the ring crosses the edge of the tile box `a..b`, it
/// gets one more point on that edge, and the rounded point goes to `nodes`
/// (`edge_nodes`). The result is in `scratch.a`.
#[allow(clippy::too_many_arguments)]
fn clip_ring_nodes(
    scratch: &mut PolyClipScratch,
    min: (i64, i64),
    max: (i64, i64),
    a: (i64, i64),
    b: (i64, i64),
    nodes: &mut DrawVec,
) {
    let (minx, miny, maxx, maxy) = (min.0 as f64, min.1 as f64, max.0 as f64, max.1 as f64);
    let (ax, ay, bx, by) = (a.0 as f64, a.1 as f64, b.0 as f64, b.1 as f64);
    let inside = |d: (f64, f64), edge: u32, minx: f64, miny: f64, maxx: f64, maxy: f64| match edge {
        0 => d.1 > miny,
        1 => d.0 < maxx,
        2 => d.1 < maxy,
        _ => d.0 > minx,
    };
    let intersect = |a: (f64, f64), b: (f64, f64), edge: u32, minx: f64, miny: f64, maxx: f64, maxy: f64| match edge {
        0 => (a.0 + (b.0 - a.0) * (miny - a.1) / (b.1 - a.1), miny),
        1 => (maxx, a.1 + (b.1 - a.1) * (maxx - a.0) / (b.0 - a.0)),
        2 => (a.0 + (b.0 - a.0) * (maxy - a.1) / (b.1 - a.1), maxy),
        _ => (minx, a.1 + (b.1 - a.1) * (minx - a.0) / (b.0 - a.0)),
    };
    let PolyClipScratch { a: out, b: inp } = scratch;
    for edge in 0..4 {
        if out.is_empty() {
            break;
        }
        std::mem::swap(out, inp);
        out.clear();
        let buf_in = |d| inside(d, edge, minx, miny, maxx, maxy);
        let tile_in = |d| inside(d, edge, ax, ay, bx, by);
        let buf_x = |s, e| intersect(s, e, edge, minx, miny, maxx, maxy);
        let tile_x = |s, e| intersect(s, e, edge, ax, ay, bx, by);
        let mut node = |out: &mut Vec<(f64, f64)>, p: (f64, f64)| {
            out.push(p);
            nodes.push(Draw::new(VT_MOVETO, cvt_i64(p.0.round()), cvt_i64(p.1.round())));
        };
        let mut s = inp[inp.len() - 1];
        for &e in inp.iter() {
            if !buf_in(s) {
                if !buf_in(e) {
                } else if !tile_in(e) {
                    out.push(buf_x(s, e));
                    out.push(e);
                } else {
                    out.push(buf_x(s, e));
                    node(out, tile_x(s, e));
                    out.push(e);
                }
            } else if !tile_in(s) {
                if !buf_in(e) {
                    out.push(buf_x(s, e));
                } else if !tile_in(e) {
                    out.push(e);
                } else {
                    node(out, tile_x(s, e));
                    out.push(e);
                }
            } else if !buf_in(e) {
                node(out, tile_x(s, e));
                out.push(buf_x(s, e));
            } else if !tile_in(e) {
                node(out, tile_x(s, e));
                out.push(e);
            } else {
                out.push(e);
            }
            s = e;
        }
    }
    if !out.is_empty() {
        let first = out[0];
        let last = out[out.len() - 1];
        if first.0 != last.0 || first.1 != last.1 {
            out.push(first);
        }
        if out.len() < 3 {
            out.clear();
        }
    }
}

/// `simple_clip_poly(geom, min, max, a, b, edge_nodes, true)` (`-pn`): as
/// [`simple_clip_poly_box`], but first `remove_noop(polygon, 0)` on `geom`
/// (in place, as tippecanoe), and the rings get the tile-edge points of
/// [`clip_ring_nodes`]. The output is appended to `out`, the edge nodes to
/// `nodes`.
#[allow(clippy::too_many_arguments)]
pub fn simple_clip_poly_nodes(
    geom: &mut DrawVec,
    min: (i64, i64),
    max: (i64, i64),
    a: (i64, i64),
    b: (i64, i64),
    out: &mut DrawVec,
    nodes: &mut DrawVec,
    scratch: &mut PolyClipScratch,
) {
    crate::simplify::remove_noop(geom, vtmake_core::GeomType::Polygon, 0);
    let mut i = 0;
    while i < geom.len() {
        assert!(geom[i].op == VT_MOVETO, "Unexpected operation in polygon {}", geom[i].op);
        let j = part_end(geom, i);
        scratch.a.clear();
        scratch.a.extend(geom[i..j].iter().map(|d| (d.x as f64, d.y as f64)));
        clip_ring_nodes(scratch, min, max, a, b, nodes);
        for (k, p) in scratch.a.iter().enumerate() {
            out.push(Draw::new(if k == 0 { VT_MOVETO } else { VT_LINETO }, cvt_i64(p.0.round()), cvt_i64(p.1.round())));
        }
        i = j;
    }
}

/// `simple_clip_poly(geom, z, buffer, ...)`: clip to the tile plus buffer.
/// The output is appended to `out`.
#[inline]
pub fn simple_clip_poly(
    geom: &[Draw],
    z: u32,
    buffer: i64,
    out: &mut DrawVec,
    scratch: &mut PolyClipScratch,
) {
    let area = 1i64 << (32 - z);
    let b = buffer * area / 256;
    simple_clip_poly_box(geom, -b, -b, area + b, area + b, out, scratch);
}

/// The result of [`simple_clip_poly`] for a polygon whose points are all
/// strictly inside the clip box. Sutherland–Hodgman then changes nothing, so
/// only the ring checks are left: close a ring that is open, and remove
/// rings with fewer than 3 points. The output is appended to `out`. This is
/// a fast path with the same result as the full clip.
pub fn poly_ring_checks(geom: &[Draw], out: &mut DrawVec) {
    let mut i = 0;
    while i < geom.len() {
        assert!(
            geom[i].op == VT_MOVETO,
            "Unexpected operation in polygon {}",
            geom[i].op
        );
        let j = part_end(geom, i);
        let first = geom[i];
        let last = geom[j - 1];
        let open = first.x != last.x || first.y != last.y;
        if j - i + open as usize >= 3 {
            out.push(Draw::new(VT_MOVETO, first.x, first.y));
            out.extend(
                geom[i + 1..j]
                    .iter()
                    .map(|d| Draw::new(VT_LINETO, d.x, d.y)),
            );
            if open {
                out.push(Draw::new(VT_LINETO, first.x, first.y));
            }
        }
        i = j;
    }
}
