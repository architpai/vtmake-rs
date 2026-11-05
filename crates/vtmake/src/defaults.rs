//! Default precision, feature order, and zoom selection.
//! Ported from felt/tippecanoe serial.cpp and main.cpp (BSD 2-Clause).
//! Copyright (c) 2022, Protomaps LLC; Copyright (c) 2014, Mapbox Inc.

use crate::{
    cli::Options,
    store::{self, Chunk, Sorted},
};
use rayon::prelude::*;
use vtmake_core::{GeomType, VT_LINETO, VT_MOVETO};
use vtmake_geom::projection::encode_quadkey;

/// Preserve the plain fast path: it already has full precision and the
/// center-of-mass index. Other modes need the serialized coordinate grid.
pub fn prepare(chunks: &mut [Chunk], o: &Options) -> (f64, usize, f64) {
    if o.geometry_scale == 0 && o.additional[b's' as usize] && !o.guess_maxzoom && !o.additional[b'h' as usize] {
        return (0.0, 0, 0.0);
    }
    chunks
        .par_iter_mut()
        .map(|c| {
            let mut geom = Vec::with_capacity(c.geom.len());
            let mut g = Vec::new();
            let mut locs = Vec::new();
            let (mut sum, mut count, mut area) = (0.0, 0usize, 0.0);
            for i in 0..c.metas.len() {
                let m = c.metas[i];
                g.clear();
                c.decode_geom(&m, &mut g);
                let scale = (1u64 << o.geometry_scale) as f64;
                for d in &mut g {
                    if d.op == VT_MOVETO || d.op == VT_LINETO {
                        d.x = (d.x as f64 / scale).round() as i64;
                        d.y = (d.y as f64 / scale).round() as i64;
                    }
                }
                // The hash includes all draws, including CLOSEPATH at (0, 0).
                let index = if m.geom_type == GeomType::Point {
                    m.index
                } else if (o.additional[b's' as usize] || o.additional[b'S' as usize]) && m.geom_type == GeomType::Polygon {
                    let (x, y) = vtmake_geom::center_of_mass_mp(&g);
                    encode_quadkey(
                        (x << o.geometry_scale) as u32,
                        (y << o.geometry_scale) as u32,
                    )
                } else {
                    let hash = g
                        .iter()
                        .fold(0u64, |h, d| h.wrapping_add(d.x.wrapping_add(d.y) as u64));
                    let d = g[hash as usize % g.len()];
                    encode_quadkey(
                        (d.x << o.geometry_scale) as u32,
                        (d.y << o.geometry_scale) as u32,
                    )
                };
                for d in &mut g {
                    if d.op == VT_MOVETO || d.op == VT_LINETO {
                        d.x <<= o.geometry_scale;
                        d.y <<= o.geometry_scale;
                    }
                }
                if o.guess_maxzoom {
                    locs.clear();
                    locs.extend(
                        g.iter()
                            .filter(|d| d.op == VT_MOVETO || d.op == VT_LINETO)
                            .map(|d| if o.additional[b'h' as usize] { encode_hilbert(d.x as u32, d.y as u32) } else { encode_quadkey(d.x as u32, d.y as u32) }),
                    );
                    locs.sort_unstable();
                    let mut local_sum = 0.0;
                    let mut n = 0;
                    for w in locs.windows(2) {
                        if w[0] != w[1] {
                            local_sum += ((w[1] - w[0]) as f64).ln();
                            n += 1;
                        }
                    }
                    if n > 0 {
                        sum += ((local_sum / n as f64).exp().sqrt() / 33.0).ln() * n as f64;
                        count += n;
                    }
                    if m.geom_type == GeomType::Polygon {
                        area += vtmake_geom::get_mp_area(&g).abs();
                    }
                }
                c.metas[i].index = if o.additional[b'h' as usize] { let (x, y) = vtmake_geom::decode_quadkey(index); encode_hilbert(x, y) } else { index };
                c.metas[i].geom_off = geom.len() as u32;
                store::encode_geom(&g, &mut geom);
            }
            c.geom = geom;
            (sum, count, area)
        })
        .reduce(|| (0.0, 0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2))
}

pub fn guess_zoom(o: &mut Options, chunks: &[Chunk], order: &[Sorted], spacing: (f64, usize, f64)) {
    let (mut mean, mut m2, mut count, mut dupes) = (0.0, 0.0, 0usize, 0usize);
    for w in order.windows(2) {
        let ix = |s: Sorted| chunks[s.chunk as usize].metas[s.local as usize].index;
        let gap = ix(w[1]) - ix(w[0]);
        if gap == 0 {
            dupes += 1;
            continue;
        }
        count += 1;
        let value = (gap as f64).ln();
        let delta = value - mean;
        mean += delta / count as f64;
        m2 += delta * (value - mean);
    }
    if count == 0 && spacing.1 == 0 && o.minimum_maxzoom == 0 {
        eprintln!("Can't guess maxzoom (-zg) without at least two distinct feature locations");
        std::process::exit(110);
    }
    let zoom = |want: f64| {
        ((360.0 / (0.00000274 * want)).ln() / 2f64.ln() - o.full_detail as f64).ceil() as i32
    };
    if count == 0 && spacing.1 == 0 && o.droprate < 0.0 {
        o.droprate = 1.0;
    }
    let stddev = (m2 / count.max(1) as f64).sqrt();
    let mut maxzoom = if count > 0 {
        zoom((mean - 1.5 * stddev).exp().sqrt() / 33.0 / 2.0).max(0)
    } else {
        o.minimum_maxzoom
    };
    if count > 0 {
        while maxzoom < 32 - o.full_detail && maxzoom < 33 - o.low_detail && maxzoom < o.cluster_maxzoom && o.cluster_distance > 0 {
            let distance = ((1u64 << (32 - maxzoom)) / 256) * o.cluster_distance as u64;
            if mean.exp() > (distance * distance) as f64 { break; }
            maxzoom += 1;
        }
    }
    if o.droprate == -3.0 {
        o.droprate = ((-0.7681 * stddev.ln() + 1.582).exp() * 100000.0).round() / 100000.0;
        if dupes > 0 && o.droprate != 0.0 {
            maxzoom +=
                ((((dupes + count) / count.max(1)) as f64).ln() / o.droprate.ln()).round() as i32;
        }
    }
    if spacing.1 > 0 {
        maxzoom = maxzoom.max(zoom((spacing.0 / spacing.1 as f64).exp() / 8.0));
    }
    maxzoom = maxzoom.clamp(0, (32 - o.full_detail).min(33 - o.low_detail));
    let mut tiles = 0.0;
    for z in 1..=maxzoom {
        tiles += (spacing.2 / (1u64 << (32 - z)) as f64 / (1u64 << (32 - z)) as f64).ceil();
        if tiles > (2 * 1024 * 1024) as f64 {
            maxzoom = z - 1;
            break;
        }
    }
    // felt sets explicit -Bg before it applies the minimum maxzoom.
    if o.basezoom == -2 && o.basezoom_marker_width == 1.0 {
        o.basezoom = maxzoom;
    }
    o.maxzoom = maxzoom.max(o.minimum_maxzoom).max(o.minzoom);
    if o.basezoom == -1 {
        o.basezoom = o.maxzoom;
    }
    if !o.quiet {
        eprintln!("Choosing a maxzoom of -z{}", o.maxzoom);
    }
}

/// `main.cpp:calc_feature_minzoom`, including the preserve-density override.
pub fn drop_pass(chunks: &mut [Chunk], order: &[Sorted], o: &Options) {
    let n = o.maxzoom as usize + 1;
    let mut seq = vec![0.0; n];
    let mut prev = vec![0u64; n];
    let interval: Vec<_> = (0..n)
        .map(|i| {
            if (i as i32) < o.basezoom {
                (o.droprate.ln() * (o.basezoom - i as i32) as f64).exp()
            } else {
                0.0
            }
        })
        .collect();
    let mut dense = Vec::new();
    let mut dense_prev = 0u64;
    for s in order {
        let m = &mut chunks[s.chunk as usize].metas[s.local as usize];
        let mut minzoom = 0;
        if o.gamma >= 0.0
            && (m.geom_type == GeomType::Point
                || (m.geom_type == GeomType::Line && o.additional[b'l' as usize])
                || (m.geom_type == GeomType::Polygon && o.additional[b'p' as usize]))
        {
            if o.drop_denser > 0 && m.index % 100 < o.drop_denser as u64 {
                dense.push((m.index.wrapping_sub(dense_prev), *s));
                dense_prev = m.index;
                continue;
            }
            for v in &mut seq {
                *v += 1.0;
            }
            for i in (0..n).rev() {
                if seq[i] < 0.0 {
                    minzoom = i + 1;
                    prev[i + 1..].fill(m.index);
                    break;
                }
                seq[i] -= interval[i];
            }
            if o.preserve_point_density_threshold > 0 {
                for i in 0..minzoom.min(n - 1) {
                    let d = (1u64 << (32 - i)) / o.preserve_point_density_threshold as u64;
                    if m.index.wrapping_sub(prev[i]) > d.wrapping_mul(d) {
                        minzoom = i;
                        prev[i..].fill(m.index);
                        break;
                    }
                }
            }
        }
        m.drop_minzoom = minzoom as i8;
    }
    if o.drop_denser > 0 {
        dense.sort_by_key(|a| std::cmp::Reverse(a.0));
        let mut i = 0;
        for z in 0..=o.basezoom {
            let keep = (dense.len() as f64 / (o.droprate.ln() * (o.basezoom - z) as f64).exp()) as usize;
            while i < keep.min(dense.len()) {
                let s = dense[i].1;
                chunks[s.chunk as usize].metas[s.local as usize].drop_minzoom = z as i8;
                i += 1;
            }
        }
        for &(_, s) in &dense[i..] { chunks[s.chunk as usize].metas[s.local as usize].drop_minzoom = o.basezoom as i8; }
    }
}

/// `main.cpp:read_input`: densest tile counts for automatic base zoom/rate.
/// Index order groups each tile into a contiguous run.
pub fn guess_base(o: &mut Options, chunks: &[Chunk], order: &[Sorted]) {
    if o.basezoom >= 0 && o.droprate >= 0.0 {
        return;
    }
    let mut counts = [0usize; 25];
    for (z, max) in counts.iter_mut().enumerate() {
        let mut previous = 0;
        let mut count = 0;
        let (mut gamma_prev, mut gamma_gap) = (0u64, 0.0);
        for s in order {
            let index = chunks[s.chunk as usize].metas[s.local as usize].index;
            let key = if z == 0 { 0 } else { index >> (64 - 2 * z) };
            if previous != key {
                *max = (*max).max(count);
                count = 0;
                previous = key;
                gamma_prev = 0;
                gamma_gap = 0.0;
            }
            if !manage_gap(
                index,
                &mut gamma_prev,
                2f64.powi(64 - 2 * (z as i32 + 8)),
                o.gamma,
                &mut gamma_gap,
            ) {
                count += 1;
            }
        }
        *max = (*max).max(count);
    }
    let max_features = (50000.0 / (o.basezoom_marker_width * o.basezoom_marker_width)) as usize;
    let automatic = o.basezoom < 0;
    if automatic {
        o.basezoom = (0..=24).find(|&z| counts[z] < max_features).unwrap_or(24) as i32;
    }
    if automatic && o.basezoom > o.maxzoom && o.prevent[b'b' as usize] {
        o.basezoom = o.maxzoom;
    }
    if automatic && o.basezoom > o.maxzoom {
        if o.droprate < 0.0 {
            o.droprate = if o.maxzoom == 0 {
                2.5
            } else {
                (((counts[0] as f64 / counts[o.maxzoom as usize] as f64).ln() / o.maxzoom as f64)
                    .exp()
                    * 100000.0)
                    .round()
                    / 100000.0
            };
        }
        o.basezoom = 0;
        for (z, &count) in counts.iter().enumerate().take(o.maxzoom as usize + 1) {
            o.basezoom = o.basezoom.max(
                ((count as f64 / max_features as f64).ln() / o.droprate.ln() + z as f64).ceil()
                    as i32,
            );
        }
    } else if o.droprate < 0.0 {
        o.droprate = 1.0;
        for z in (0..o.basezoom as usize).rev() {
            let interval = (o.droprate.ln() * (o.basezoom as usize - z) as f64).exp();
            if counts[z] as f64 / interval >= max_features as f64 {
                o.droprate = (((counts[z] as f64 / max_features as f64).ln()
                    / (o.basezoom as usize - z) as f64)
                    .exp()
                    * 100000.0)
                    .round()
                    / 100000.0;
            }
        }
    }
}

/// `tile.cpp:manage_gap`: gamma spacing state for a tile.
pub fn manage_gap(index: u64, prev: &mut u64, scale: f64, gamma: f64, gap: &mut f64) -> bool {
    if gamma <= 0.0 {
        return false;
    }
    if *gap > 0.0 {
        if index == *prev {
            return true;
        }
        if index < *prev || (((index - *prev) as f64 / scale).ln() * gamma).exp() >= *gap {
            *gap = 0.0;
        } else {
            return true;
        }
    } else if index >= *prev {
        *gap = (index - *prev) as f64 / scale;
        if *gap < 1.0 {
            return true;
        }
        *gap = 0.0;
    }
    *prev = index;
    false
}

// Hilbert index, from projection.cpp (same license as this module).
fn hilbert_rot(n: u64, x: &mut u32, y: &mut u32, rx: u64, ry: u64) {
    if ry == 0 {
        if rx == 1 {
            *x = n.wrapping_sub(1).wrapping_sub(*x as u64) as u32;
            *y = n.wrapping_sub(1).wrapping_sub(*y as u64) as u32;
        }
        std::mem::swap(x, y);
    }
}

pub fn encode_hilbert(mut x: u32, mut y: u32) -> u64 {
    let mut d = 0u64;
    let mut s = 1u64 << 31;
    while s > 0 {
        let rx = u64::from(x as u64 & s != 0);
        let ry = u64::from(y as u64 & s != 0);
        d = d.wrapping_add(s * s * ((3 * rx) ^ ry));
        hilbert_rot(s, &mut x, &mut y, rx, ry);
        s /= 2;
    }
    d
}

pub fn decode_hilbert(mut d: u64) -> (u32, u32) {
    let (mut x, mut y) = (0u32, 0u32);
    let mut s = 1u64;
    while s < 1u64 << 32 {
        let rx = 1 & (d / 2);
        let ry = 1 & (d ^ rx);
        hilbert_rot(s, &mut x, &mut y, rx, ry);
        x = x.wrapping_add((s * rx) as u32);
        y = y.wrapping_add((s * ry) as u32);
        d /= 4;
        s *= 2;
    }
    (x, y)
}
