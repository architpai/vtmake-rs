//! Ring area, ring fixes at read time, center of mass, and the feature
//! index point (tippecanoe `clip.cpp`, `geometry.cpp`, `serial.cpp`).

use crate::num::{cvt_i64, cvt_u64, wrap40};
use crate::projection::encode_quadkey;
use vtmake_core::{Draw, DrawVec, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};

const MAX_EXACT_DOUBLE: f64 = ((1i64 << 53) - 1) as f64;

/// Find the end of the part that starts at `i`: the first index after `i`
/// that is not a LINETO.
#[inline(always)]
pub fn part_end(geom: &[Draw], i: usize) -> usize {
    let mut j = i + 1;
    while j < geom.len() && geom[j].op == VT_LINETO {
        j += 1;
    }
    j
}

/// tippecanoe `draw::operator<`: compare `y`, then `x`.
#[inline(always)]
pub fn draw_lt(a: &Draw, b: &Draw) -> bool {
    a.y < b.y || (a.y == b.y && a.x < b.x)
}

#[cold]
fn get_area_scaled(geom: &[Draw], i: usize, j: usize) -> f64 {
    let n = j - i;
    let mut scale: i64 = 2;
    while scale < (1i64 << 30) {
        let bx = geom[i].x;
        let by = geom[i].y;
        let mut again = false;

        let mut area = 0.0f64;
        for k in i..j {
            let nx = &geom[i + (k - i + 1) % n];
            area += ((geom[k].x - bx) / scale) as f64 * ((nx.y - by) / scale) as f64;
            if area.abs() >= MAX_EXACT_DOUBLE {
                again = true;
                break;
            }
            area -= ((geom[k].y - by) / scale) as f64 * ((nx.x - bx) / scale) as f64;
            if area.abs() >= MAX_EXACT_DOUBLE {
                again = true;
                break;
            }
        }

        if !again {
            area /= 2.0;
            return area * scale as f64 * scale as f64;
        }
        scale *= 2;
    }
    // tippecanoe exits here ("get_area_scaled: can't happen").
    panic!("get_area_scaled: can't happen");
}

/// `get_area`: signed shoelace area of the ring `geom[i..j]` (y down, so an
/// outer ring is positive). The ring includes its closing point.
///
/// If a partial sum reaches 2^53, the sign comes from a scaled-down retry.
#[inline]
pub fn get_area(geom: &[Draw], i: usize, j: usize) -> f64 {
    let n = j - i;
    let bx = geom[i].x;
    let by = geom[i].y;

    let mut area = 0.0f64;
    let mut overflow = false;
    for k in i..j {
        let nx = &geom[i + (k - i + 1) % n];
        area += (geom[k].x - bx) as f64 * (nx.y - by) as f64;
        if area.abs() >= MAX_EXACT_DOUBLE {
            overflow = true;
        }
        area -= (geom[k].y - by) as f64 * (nx.x - bx) as f64;
        if area.abs() >= MAX_EXACT_DOUBLE {
            overflow = true;
        }
    }
    area /= 2.0;

    if overflow {
        let scaled = get_area_scaled(geom, i, j);
        if (area < 0.0 && scaled > 0.0) || (area > 0.0 && scaled < 0.0) {
            area = -area;
        }
    }
    area
}

/// `get_mp_area`: the sum of [`get_area`] of all rings.
pub fn get_mp_area(geom: &[Draw]) -> f64 {
    let mut ret = 0.0;
    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let j = part_end(geom, i);
            ret += get_area(geom, i, j);
            i = j;
        } else {
            i += 1;
        }
    }
    ret
}

/// `fix_polygon(geom, false, false)`: fix the rings of a parsed polygon
/// (world coordinates, read time). See spec 7.1.
///
/// Input: rings as MOVETO + LINETOs; a CLOSEPATH ends each GeoJSON Polygon
/// (so the next ring is an outer ring). Output: only MOVETO and LINETO,
/// each ring closed, at least 4 points, outer rings with area > 0 and holes
/// with area <= 0, each ring rotated to start at a far point.
///
/// The output is appended to `out`. tippecanoe exits if a ring starts with
/// an op that is not MOVETO; this function panics.
pub fn fix_polygon(geom: &[Draw], out: &mut DrawVec) {
    fix_polygon_opts(geom, out, false, false);
}

/// `fix_polygon(geom, use_winding, reverse_winding)`: [`fix_polygon`] with
/// `-pw` (`use_winding`: reverse each ring, so that the GeoJSON winding
/// gives the inner and outer rings) or `-pW` (`reverse_winding`: never
/// reverse a ring).
pub fn fix_polygon_opts(geom: &[Draw], out: &mut DrawVec, use_winding: bool, reverse_winding: bool) {
    let mut outer = true;
    let mut i = 0;
    while i < geom.len() {
        let op = geom[i].op;
        if op == VT_CLOSEPATH {
            outer = true;
            i += 1;
            continue;
        }
        assert!(
            op == VT_MOVETO,
            "Internal error: polygon ring begins with {op}, not moveto"
        );

        let j = part_end(geom, i);
        let start = out.len();

        // Copy the ring and close it if it is not closed.
        out.extend_from_slice(&geom[i..j]);
        let first = geom[i];
        let last = geom[j - 1];
        if first.x != last.x || first.y != last.y {
            out.push(first);
        }
        // Pad to at least four points.
        while out.len() - start < 4 {
            out.push(first);
        }

        let n = out.len() - start;
        let reverse = if use_winding {
            true
        } else if reverse_winding {
            false
        } else {
            (get_area(out, start, out.len()) > 0.0) != outer
        };
        if reverse {
            out[start..].reverse();
        }

        let ring = &out[start..];

        // Centroid: integer mean without the closing point.
        let count = (n - 1) as i64;
        let mut xtotal: i64 = 0;
        let mut ytotal: i64 = 0;
        for p in &ring[..n - 1] {
            xtotal = xtotal.wrapping_add(p.x);
            ytotal = ytotal.wrapping_add(p.y);
        }
        xtotal /= count;
        ytotal /= count;

        // The point furthest from the centroid.
        let mut dist2: i64 = 0;
        let mut furthest = 0usize;
        for (a, p) in ring[..n - 1].iter().enumerate() {
            let xd = (p.x - xtotal) / 16;
            let yd = (p.y - ytotal) / 16;
            let d2 = xd.wrapping_mul(xd).wrapping_add(yd.wrapping_mul(yd));
            if d2 > dist2 || (d2 == dist2 && draw_lt(p, &ring[furthest])) {
                dist2 = d2;
                furthest = a;
            }
        }

        // The point furthest from that point.
        let fp = ring[furthest];
        let mut dist2b: i64 = 0;
        let mut furthestb = 0usize;
        for (a, p) in ring[..n - 1].iter().enumerate() {
            let xd = (p.x - fp.x) / 16;
            let yd = (p.y - fp.y) / 16;
            let d2 = xd.wrapping_mul(xd).wrapping_add(yd.wrapping_mul(yd));
            if d2 > dist2b || (d2 == dist2b && draw_lt(p, &ring[furthestb])) {
                dist2b = d2;
                furthestb = a;
            }
        }

        // Rotate so that the ring starts (and ends) at `furthestb`.
        let ring = &mut out[start..];
        ring[..n - 1].rotate_left(furthestb);
        ring[n - 1] = ring[0];
        for (a, d) in ring.iter_mut().enumerate() {
            *d = Draw::new(if a == 0 { VT_MOVETO } else { VT_LINETO }, d.x, d.y);
        }

        outer = false;
        i = j;
    }
}

/// `centerOfMass` (geometry.cpp, from turf): center of mass of the ring
/// `dv[start..end]`, relative to `centre` to reduce rounding errors.
pub(crate) fn center_of_mass(dv: &[Draw], start: usize, end: usize, centre: (i64, i64)) -> (i64, i64) {
    let (tx, ty) = centre;
    let mut sx = 0.0f64;
    let mut sy = 0.0f64;
    let mut s_area = 0.0f64;

    let pts = &dv[start..end];
    for w in pts.windows(2) {
        let xi = wrap40(w[0].x - tx) as f64;
        let yi = wrap40(w[0].y - ty) as f64;
        let xj = wrap40(w[1].x - tx) as f64;
        let yj = wrap40(w[1].y - ty) as f64;

        let a = xi * yj - xj * yi;
        s_area += a;
        sx += (xi + xj) * a;
        sy += (yi + yj) * a;
    }

    if s_area == 0.0 {
        centre
    } else {
        let area = s_area * 0.5;
        let area_factor = 1.0 / (6.0 * area);
        (
            wrap40(cvt_i64(tx as f64 + area_factor * sx)),
            wrap40(cvt_i64(ty as f64 + area_factor * sy)),
        )
    }
}

/// `center_of_mass_mp`: area-weighted center of mass of all rings.
///
/// This copies the C++ arithmetic, also its odd parts: the area sum is a
/// `size_t` (truncated at each add), and a zero total area divides by zero.
/// For that case the x86-64 conversion gives `(0, 0)` (see [`cvt_i64`] and
/// the 40-bit `draw` field).
pub fn center_of_mass_mp(dv: &[Draw]) -> (i64, i64) {
    let mut ringx = 0.0f64;
    let mut ringy = 0.0f64;
    let mut ringcount: u64 = 0;

    let mut i = 0;
    while i < dv.len() {
        if dv[i].op == VT_MOVETO {
            let mut xsum = dv[i].x as f64;
            let mut ysum = dv[i].y as f64;
            let mut count: i64 = 1;
            let mut j = i + 1;
            while j < dv.len() && dv[j].op == VT_LINETO {
                xsum += dv[j].x as f64;
                ysum += dv[j].y as f64;
                count += 1;
                j += 1;
            }

            let area = get_area(dv, i, j);
            // std::llround: half away from zero, like f64::round.
            let centroid = (
                wrap40(cvt_i64((xsum / count as f64).round())),
                wrap40(cvt_i64((ysum / count as f64).round())),
            );

            let center = center_of_mass(dv, i, j, centroid);
            ringx += center.0 as f64 * area;
            ringy += center.1 as f64 * area;
            ringcount = cvt_u64(ringcount as f64 + area);

            i = j;
        } else {
            i += 1;
        }
    }

    (
        wrap40(cvt_i64(ringx / ringcount as f64)),
        wrap40(cvt_i64(ringy / ringcount as f64)),
    )
}

/// The index point of a feature (`serial.cpp:serialize_feature`), for the
/// command line of the spec (`--drop-densest-as-needed`, geometry_scale 0).
///
/// - `geom`: world coordinates, polygons after [`fix_polygon`].
/// - `bbox`: `[min x, min y, max x, max y]` of the MOVETO/LINETO points.
///
/// Points use the bbox center (each half divided first). Polygons use
/// [`center_of_mass_mp`]. Lines use a vertex chosen by a hash of the
/// coordinates. The result is masked to 32 bits.
///
/// Panics if `geom` is empty for a line (C++ reads out of bounds).
pub fn index_point(t: GeomType, geom: &[Draw], bbox: &[i64; 4]) -> (u32, u32) {
    const MASK: i64 = (1i64 << 32) - 1;
    match t {
        GeomType::Point => {
            let midx = (bbox[0] / 2 + bbox[2] / 2) & MASK;
            let midy = (bbox[1] / 2 + bbox[3] / 2) & MASK;
            (midx as u32, midy as u32)
        }
        GeomType::Polygon => {
            let (cx, cy) = center_of_mass_mp(geom);
            ((cx & MASK) as u32, (cy & MASK) as u32)
        }
        GeomType::Line => {
            let mut ix: u64 = 0;
            for d in geom {
                ix = ix.wrapping_add(d.x.wrapping_add(d.y) as u64);
            }
            if !geom.is_empty() {
                ix %= geom.len() as u64;
            }
            let d = &geom[ix as usize];
            ((d.x & MASK) as u32, (d.y & MASK) as u32)
        }
    }
}

/// The sort index of a feature: `encode_quadkey` of [`index_point`].
#[inline]
pub fn feature_index(t: GeomType, geom: &[Draw], bbox: &[i64; 4]) -> u64 {
    let (x, y) = index_point(t, geom, bbox);
    encode_quadkey(x, y)
}

/// The bbox of the MOVETO/LINETO points, as `serial.cpp:scale_geometry`
/// makes it. An empty geometry gives `[i64::MAX, i64::MAX, i64::MIN, i64::MIN]`.
#[inline]
pub fn geometry_bbox(geom: &[Draw]) -> [i64; 4] {
    let mut bbox = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
    for d in geom {
        if d.op == VT_MOVETO || d.op == VT_LINETO {
            bbox[0] = bbox[0].min(d.x);
            bbox[1] = bbox[1].min(d.y);
            bbox[2] = bbox[2].max(d.x);
            bbox[3] = bbox[3].max(d.y);
        }
    }
    bbox
}

/// `clip.cpp:pnpoly`: is (testx, testy) in the ring `vert[start..start +
/// nvert]` (even-odd rule)? The product is in 64-bit integers, as in the
/// C++ code.
pub fn pnpoly(vert: &[Draw], start: usize, nvert: usize, testx: i64, testy: i64) -> bool {
    let mut c = false;
    if nvert == 0 {
        return c;
    }
    let mut j = nvert - 1;
    for i in 0..nvert {
        let (vi, vj) = (vert[i + start], vert[j + start]);
        if (vi.y > testy) != (vj.y > testy)
            && (testx as f64)
                < (vj.x.wrapping_sub(vi.x)).wrapping_mul(testy.wrapping_sub(vi.y)) as f64 / (vj.y - vi.y) as f64
                    + vi.x as f64
        {
            c = !c;
        }
        j = i;
    }
    c
}

/// The ring part of `geometry.cpp:check_polygon` (`-a@`): for each point
/// of an inner ring (area <= 0) that is not in the outer ring before it and
/// not on a line of an outer ring vertex, write tippecanoe's message to
/// standard error. `geom` has had `remove_noop(polygon, 0)`.
pub fn check_polygon_rings(geom: &[Draw]) {
    let mut outer_start: usize = usize::MAX;
    let mut outer_len = 0usize;
    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let j = part_end(geom, i);
            let area = get_area(geom, i, j);
            if area > 0.0 {
                outer_start = i;
                outer_len = j - i;
            } else {
                for k in i..j {
                    if !pnpoly(geom, outer_start, outer_len, geom[k].x, geom[k].y) {
                        let on_edge = (0..outer_len)
                            .any(|l| geom[k].x == geom[outer_start + l].x || geom[k].y == geom[outer_start + l].y);
                        if !on_edge {
                            eprintln!(
                                "{},{} at {} not in outer ring ({} to {})",
                                geom[k].x,
                                geom[k].y,
                                k,
                                outer_start as i64,
                                outer_start.wrapping_add(outer_len) as i64
                            );
                        }
                    }
                }
            }
        }
        i += 1;
    }
}
