//! Label points (`-aP`, `--convert-polygons-to-label-points`): a port of
//! `geometry.cpp:label_goodness`, `polygon_to_anchor` and
//! `checkerboard_anchors` of tippecanoe.
//!
//! tippecanoe is BSD-2-Clause:
//!
//! Copyright (c) 2022, Protomaps LLC
//! Copyright (c) 2014, Mapbox Inc.
//! All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions are
//! met:
//!
//! - Redistributions of source code must retain the above copyright
//!   notice, this list of conditions and the following disclaimer.
//! - Redistributions in binary form must reproduce the above copyright
//!   notice, this list of conditions and the following disclaimer in the
//!   documentation and/or other materials provided with the distribution.
//!
//! THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS
//! IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
//! THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
//! PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
//! CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
//! EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
//! PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
//! PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
//! LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
//! NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
//! SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use crate::num::{cvt_u64, wrap40};
use crate::polygon::{center_of_mass, get_area, part_end, pnpoly};
use crate::projection::{decode_quadkey, tile2lonlat};
use crate::simplify::distance_from_line;
use std::sync::atomic::{AtomicI64, Ordering};
use vtmake_core::{Draw, DrawVec, VT_LINETO, VT_MOVETO};

/// `label_goodness`: 0 if (x, y) is not in the polygon (ring nesting below
/// 1), else the distance to the nearest vertex or edge.
pub fn label_goodness(dv: &[Draw], x: i64, y: i64) -> f64 {
    let mut nesting = 0i32;
    let mut i = 0;
    while i < dv.len() {
        if dv[i].op == VT_MOVETO {
            let j = part_end(dv, i);
            if pnpoly(dv, i, j - i, x, y) {
                if get_area(dv, i, j) >= 0.0 {
                    nesting += 1;
                } else {
                    nesting -= 1;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    if nesting < 1 {
        return 0.0;
    }
    let mut closest = f64::INFINITY;
    for i in 0..dv.len() {
        let dx = (dv[i].x - x) as f64;
        let dy = (dv[i].y - y) as f64;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < closest {
            closest = dist;
        }
        if i > 0 && dv[i].op == VT_LINETO {
            let dist = distance_from_line(x, y, dv[i - 1].x, dv[i - 1].y, dv[i].x, dv[i].y);
            if dist < closest {
                closest = dist;
            }
        }
    }
    closest
}

/// `sorty_sorter(kind)`: the values (xa, ya) of a vertex. The sort is by
/// ya, then by xa.
#[inline]
fn sorty_key(kind: usize, p: (i64, i64)) -> (i64, i64) {
    let (x, y) = p;
    match kind {
        0 => (x, y),
        1 => (y, x),
        2 => (x.wrapping_add(y), x.wrapping_sub(y)),
        _ => (x.wrapping_sub(y), x.wrapping_add(y)),
    }
}

static WARNED: AtomicI64 = AtomicI64::new(0);

/// `polygon_to_anchor`: the label point of a polygon (world coordinates),
/// or `None` if it has no outer ring. It can write tippecanoe's warning
/// "could not find good label point" (not more than 10 times).
pub fn polygon_to_anchor(geom: &[Draw]) -> Option<(i64, i64)> {
    let (mut start, mut end) = (0usize, 0usize);
    // `size_t best_area`: the area is truncated to an integer.
    let mut best_area: u64 = 0;
    let mut points: Vec<(i64, i64)> = Vec::new();
    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let j = part_end(geom, i);
            points.extend(geom[i + 1..j].iter().map(|d| (d.x, d.y)));
            let area = get_area(geom, i, j);
            if area > best_area as f64 {
                start = i;
                end = j;
                best_area = cvt_u64(area);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    if best_area == 0 {
        return None;
    }

    let (mut xsum, mut ysum, mut count) = (0i64, 0i64, 0u64);
    let (mut xmin, mut ymin, mut xmax, mut ymax) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for d in &geom[start + 1..end] {
        xsum = xsum.wrapping_add(d.x);
        ysum = ysum.wrapping_add(d.y);
        count += 1;
        xmin = xmin.min(d.x);
        ymin = ymin.min(d.y);
        xmax = xmax.max(d.x);
        ymax = ymax.max(d.y);
    }
    if count == 0 {
        return None;
    }

    let radius = (best_area as f64 / std::f64::consts::PI).sqrt();
    let goodness_threshold = radius / 5.0;
    // `long long / size_t` is an unsigned division in C++. A `draw` keeps
    // 40 bits of each coordinate (`wrap40`).
    let centroid = (wrap40(((xsum as u64) / count) as i64), wrap40(((ysum as u64) / count) as i64));
    let mut d = center_of_mass(geom, start, end, centroid);
    let mut goodness = label_goodness(geom, d.0, d.1);
    let mut kind = "mass";

    if goodness < goodness_threshold {
        let mut candidates: Vec<(i64, i64, f64)> = Vec::new();
        for pass in 0..4 {
            points.sort_by(|a, b| {
                let (xa, ya) = sorty_key(pass, *a);
                let (xb, yb) = sorty_key(pass, *b);
                (ya, xa).cmp(&(yb, xb))
            });
            for w in points.windows(2) {
                let dx = (w[1].0 - w[0].0) as f64;
                let dy = (w[1].1 - w[0].1) as f64;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > 2.0 * goodness_threshold {
                    candidates.push(((w[1].0 + w[0].0) / 2, (w[1].1 + w[0].1) / 2, dist));
                }
            }
        }
        // Largest distance first; stable for equal distances.
        candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        for c in candidates.iter().take(50) {
            let maybe = label_goodness(geom, c.0, c.1);
            if maybe > goodness {
                d = (wrap40(c.0), wrap40(c.1));
                goodness = maybe;
                kind = "diagonal";
                if goodness > goodness_threshold {
                    break;
                }
            }
        }
    }

    if goodness < goodness_threshold {
        let mut sub = 2i64;
        while sub < 32 && (xmax - xmin) > 2 * sub && (ymax - ymin) > 2 * sub {
            for x in 1..sub {
                for y in 1..sub {
                    let maybe = (wrap40(xmin + x * (xmax - xmin) / sub), wrap40(ymin + y * (ymax - ymin) / sub));
                    let g = label_goodness(geom, maybe.0, maybe.1);
                    if g > goodness {
                        d = maybe;
                        goodness = g;
                        kind = "grid";
                    }
                }
            }
            if goodness > goodness_threshold {
                break;
            }
            sub *= 2;
        }
        let maybe = label_goodness(geom, centroid.0, centroid.1);
        if maybe > goodness {
            d = centroid;
            goodness = maybe;
            kind = "centroid";
        }
        if goodness <= 0.0 {
            let (lon, lat) = tile2lonlat(d.0, d.1, 32);
            if WARNED.fetch_add(1, Ordering::Relaxed) < 10 {
                eprintln!("could not find good label point: {kind} {lat:.6},{lon:.6}");
            }
        }
    }
    Some(d)
}

/// `checkerboard_anchors`: the label points of a polygon in tile (z, tx,
/// ty). `geom` is tile-relative and simplified; `label_point` is the
/// quadkey of the anchor of the feature. The output (0 or 1 MOVETO,
/// tile-relative) is appended to `out`.
pub fn checkerboard_anchors(geom: &[Draw], tx: u32, ty: u32, z: u32, label_point: u64, out: &mut DrawVec) {
    let (wx, wy) = decode_quadkey(label_point);
    let (wx, wy) = (wx as i64, wy as i64);
    let (mut tx1, mut ty1, mut tx2) = (0i64, 0i64, 1i64 << 32);
    if z != 0 {
        tx1 = (tx as i64) << (32 - z);
        ty1 = (ty as i64) << (32 - z);
        tx2 = (tx as i64 + 1) << (32 - z);
    }
    let (mut bx1, mut by1, mut bx2, mut by2) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for g in geom {
        bx1 = bx1.min(g.x + tx1);
        by1 = by1.min(g.y + ty1);
        bx2 = bx2.max(g.x + tx1);
        by2 = by2.max(g.y + ty1);
    }
    if bx1 > bx2 || by1 > by2 {
        return;
    }

    let mut spiral_dist = 0.3f64;
    if z > 0 {
        spiral_dist *= ((z as f64).ln() * 1.2).exp();
    }
    let label_spacing = crate::num::cvt_i64(spiral_dist * (tx2 - tx1) as f64);

    // Integer divisions (toward zero), then floor/ceil of the integer.
    let x1 = (bx1 - wx).min(bx2 - wx) / label_spacing;
    let x2 = (bx1 - wx).max(bx2 - wx) / label_spacing;
    let y1 = crate::num::cvt_i64((((by1 - wy).min(by2 - wy) / label_spacing) as f64 - 0.5).floor());
    let y2 = (by1 - wy).max(by2 - wy) / label_spacing;

    for lx in x1..=x2 {
        for ly in y1..=y2 {
            let x = lx * label_spacing + wx;
            let mut y = ly * label_spacing + wy;
            if (lx as u64) & 1 == 1 {
                y += label_spacing / 2;
            }
            if x < bx1 || x > bx2 || y < by1 || y > by2 {
                continue;
            }
            if lx == 0 && ly == 0 {
                out.push(Draw::new(VT_MOVETO, wrap40(x - tx1), wrap40(y - ty1)));
                break;
            }
            let tilesize = (1i64 << (32 - z)) as f64;
            if label_goodness(geom, x - tx1, y - ty1) > tilesize / 100.0 {
                out.push(Draw::new(VT_MOVETO, wrap40(x - tx1), wrap40(y - ty1)));
                break;
            }
        }
    }
}
