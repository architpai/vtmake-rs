//! Shared borders (`-ab`, `--detect-shared-borders`): a port of
//! `shared_borders.cpp:find_common_edges` of tippecanoe.
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

use crate::polygon::draw_lt;
use crate::simplify::{NodeCtx, SimplifyScratch, simplify_lines_opts};
use crate::tile::GeomOpts;
use std::collections::{HashMap, HashSet};
use vtmake_core::{Draw, DrawVec, VT_LINETO, VT_MOVETO};

/// An edge key: (y1, x1, y2, x2) as `unsigned`, the ends in `draw` order.
type EdgeKey = (u32, u32, u32, u32);

#[inline]
fn edge_key(a: &Draw, b: &Draw) -> EdgeKey {
    let (p, q) = if draw_lt(a, b) { (a, b) } else { (b, a) };
    (p.y as u32, p.x as u32, q.y as u32, q.x as u32)
}

/// `std::equal_range` of `key` in the sorted `edges`.
#[inline]
fn range(edges: &[(EdgeKey, u32)], key: EdgeKey) -> std::ops::Range<usize> {
    let lo = edges.partition_point(|e| e.0 < key);
    let hi = lo + edges[lo..].partition_point(|e| e.0 <= key);
    lo..hi
}

/// `find_common_edges(features, z, line_detail, simplification, maxzoom, 1)`
/// for the polygons of one layer of a tile, in the order of the layer
/// (tile-relative world coordinates). It simplifies the polygons: each
/// "arc" between points where the set of rings on the two sides changes is
/// simplified once, so that neighbours keep a common border. With the merge
/// fraction 1 of tippecanoe, no polygons are merged.
///
/// tippecanoe exits (106) if a polygon edge is not found (a ring that is
/// not closed); this function does the same.
pub fn find_common_edges(polys: &mut [DrawVec], z: u32, detail: u32, simplification: f64, opts: &GeomOpts, scratch: &mut SimplifyScratch) {
    // Remove LINETOs that repeat the point before them.
    for g in polys.iter_mut() {
        let mut out: DrawVec = Vec::with_capacity(g.len());
        for k in 0..g.len() {
            if !(g[k].op == VT_LINETO && k > 0 && g[k - 1].x == g[k].x && g[k - 1].y == g[k].y) {
                out.push(g[k]);
            }
        }
        *g = out;
    }

    // All edges, with the ring numbers (across the polygons).
    let mut edges: Vec<(EdgeKey, u32)> = Vec::new();
    let mut ring = 0u32;
    for g in polys.iter() {
        for k in 0..g.len().saturating_sub(1) {
            if g[k].op == VT_MOVETO {
                ring = ring.wrapping_add(1);
            }
            if g[k + 1].op == VT_LINETO {
                edges.push((edge_key(&g[k], &g[k + 1]), ring));
            }
        }
    }
    edges.sort_unstable();

    // Mark the points where the rings on the two sides change.
    let mut necessaries: HashSet<(i64, i64)> = HashSet::new();
    for g in polys.iter_mut() {
        for d in g.iter_mut() {
            d.necessary = false;
        }
        let mut a = 0;
        while a < g.len() {
            if g[a].op != VT_MOVETO {
                a += 1;
                continue;
            }
            let b = crate::polygon::part_end(g, a);
            let s = b - a - 1;
            if s > 0 {
                let ordered = |p: Draw, q: Draw| if draw_lt(&p, &q) { (p, q) } else { (q, p) };
                let left = ordered(g[a + (s - 1) % s], g[a]);
                let mut e1 = range(&edges, edge_key(&left.0, &left.1));
                for k in 0..s {
                    let right = ordered(g[a + k], g[a + k + 1]);
                    let e2 = range(&edges, edge_key(&right.0, &right.1));
                    if e1.is_empty() || e2.is_empty() {
                        let (l, r) = (left, right);
                        eprintln!(
                            "Internal error: polygon edge lookup failed for {},{} to {},{} or {},{} to {},{}",
                            l.0.x, l.0.y, l.1.x, l.1.y, r.0.x, r.0.y, r.1.x, r.1.y
                        );
                        std::process::exit(crate::EXIT_IMPOSSIBLE);
                    }
                    let same = e1.len() == e2.len() && edges[e1.clone()].iter().zip(&edges[e2.clone()]).all(|(p, q)| p.1 == q.1);
                    if !same {
                        g[a + k].necessary = true;
                        necessaries.insert((g[a + k].x, g[a + k].y));
                    }
                    e1 = e2;
                }
            }
            a = b;
        }
    }
    drop(edges);

    // Roll each ring to start at a necessary point, and cut it into arcs.
    let mut arcs: HashMap<Vec<(i64, i64)>, usize> = HashMap::new();
    let mut arc_list: Vec<DrawVec> = vec![Vec::new()];
    let mut arc_polygons: Vec<Vec<isize>> = Vec::with_capacity(polys.len());
    let mut tmp: DrawVec = Vec::new();
    for g in polys.iter_mut() {
        let mut arc_polygon: Vec<isize> = Vec::new();
        for d in g.iter_mut() {
            if necessaries.contains(&(d.x, d.y)) {
                d.necessary = true;
            }
        }
        let mut k = 0;
        while k < g.len() {
            if g[k].op != VT_MOVETO {
                k += 1;
                continue;
            }
            let mut necessary: isize = -1;
            let mut lowest = k;
            let mut l = k + 1;
            while l < g.len() && g[l].op == VT_LINETO {
                if g[l].necessary {
                    necessary = l as isize;
                }
                if draw_lt(&g[l], &g[lowest]) {
                    lowest = l;
                }
                l += 1;
            }
            if necessary < 0 {
                necessary = lowest as isize;
                g[lowest].necessary = true;
            }
            let nec = necessary as usize;
            tmp.clear();
            tmp.extend_from_slice(&g[nec..l - 1]);
            tmp.extend_from_slice(&g[k..nec]);
            tmp.push(g[nec]);
            if tmp.len() != l - k {
                eprintln!("internal error shifting ring");
                std::process::exit(crate::EXIT_IMPOSSIBLE);
            }
            for (m, d) in tmp.iter_mut().enumerate() {
                d.op = if m == 0 { VT_MOVETO } else { VT_LINETO };
            }
            g[k..l].copy_from_slice(&tmp);

            // Each part from one necessary point to the next is an arc.
            let mut m = k;
            while m < l {
                if !g[m].necessary {
                    eprintln!("internal error in arc building");
                    std::process::exit(crate::EXIT_IMPOSSIBLE);
                }
                let mut n = m;
                let mut arc: Vec<(i64, i64)> = Vec::new();
                while n < l {
                    arc.push((g[n].x, g[n].y));
                    if n > m && g[n].necessary {
                        break;
                    }
                    n += 1;
                }
                // `n` is the index of the last point of the arc, or `l`.
                let end = n.min(l - 1);
                if let Some(&id) = arcs.get(&arc) {
                    arc_polygon.push(id as isize);
                } else {
                    let rev: Vec<(i64, i64)> = arc.iter().rev().copied().collect();
                    if let Some(&id) = arcs.get(&rev) {
                        arc_polygon.push(-(id as isize));
                    } else {
                        let id = arcs.len() + 1;
                        arc_list.push(g[m..=end].to_vec());
                        arcs.insert(arc, id);
                        arc_polygon.push(id as isize);
                    }
                }
                m = n;
            }
            arc_polygon.push(0);
            k = l;
        }
        arc_polygons.push(arc_polygon);
    }

    // Simplify each arc.
    let simplify = !(opts.no_simplify
        || (z == opts.maxzoom && opts.no_simplify_maxzoom)
        || (z < opts.maxzoom && opts.grid_low_zooms));
    let mark = !(opts.no_clipping || opts.no_duplication);
    for dv in arc_list.iter_mut().skip(1) {
        for (i, d) in dv.iter_mut().enumerate() {
            d.op = if i == 0 { VT_MOVETO } else { VT_LINETO };
        }
        if simplify {
            simplify_lines_opts(dv, z, detail, mark, simplification, 4, opts, &NodeCtx::EMPTY, scratch);
        }
    }

    // Make the polygons again from their arcs.
    for (g, ap) in polys.iter_mut().zip(&arc_polygons) {
        g.clear();
        let mut at_start = true;
        let mut first: Option<Draw> = None;
        for &p in ap {
            if p == 0 {
                if let Some(f) = first.take() {
                    g.push(f);
                }
                at_start = true;
                continue;
            }
            let arc = &arc_list[p.unsigned_abs()];
            let pts: Box<dyn Iterator<Item = &Draw>> = if p > 0 {
                Box::new(arc[..arc.len().saturating_sub(1)].iter())
            } else {
                Box::new(arc[1.min(arc.len())..].iter().rev())
            };
            for d in pts {
                if at_start {
                    g.push(Draw::new(VT_MOVETO, d.x, d.y));
                    first = Some(Draw::new(VT_LINETO, d.x, d.y));
                } else {
                    g.push(Draw::new(VT_LINETO, d.x, d.y));
                }
                at_start = false;
            }
        }
    }
}
