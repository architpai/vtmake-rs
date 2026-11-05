//! One feature at one tile: the clip (`tile.cpp:clip_to_tile`), the child
//! candidates (`tile.cpp:rewrite`), and the steps from the clip result to
//! the MVT geometry (`tile.cpp:write_tile`, `simplification_worker`).
//!
//! # Coordinate frames
//!
//! tippecanoe stores the features of a tile in **absolute** world
//! coordinates (`rewrite` adds the parent tile origin back). When it reads
//! a feature for tile (z, tx, ty), `decode_geometry` subtracts the tile
//! origin `(tx << (32-z), ty << (32-z))` (not at z = 0) and makes the bbox
//! from the **tile-relative** points. All later steps (quick check, clip,
//! tiny polygons, simplification, `to_tile_scale`) use tile-relative
//! coordinates. The children of a tile get the **clipped** geometry, and
//! their candidates come from the bbox of the geometry **before** this
//! clip (the bbox of the parent-clipped geometry).
//!
//! # Order of the steps for one feature at one tile
//!
//! 1. [`clip_to_tile`] (absolute in, tile-relative out). If it returns
//!    `None`, the feature is not in the tile and has no children here.
//! 2. If the clipped geometry is not empty and the feature can appear at
//!    a deeper zoom: write it to the children in [`child_tile_range`].
//! 3. Zoom checks (`-Z`, `tippecanoe.minzoom`/`maxzoom`), then the rate drop
//!    ([`crate::drop`]).
//! 4. [`tile_reduce`]: tiny polygons (with the tile accumulator) and the
//!    "too small" test. Call it in tile feature order.
//! 5. [`tile_simplify`]: simplification and `to_tile_scale`. The polygon
//!    result in tile units goes to polygon cleaning (`coalesce_polygon`,
//!    wagyu; not in this crate).
//! 6. Polygons: [`tile_finish_polygon`] with the cleaned result. Lines:
//!    [`tile_finish_line`]. Points: no step.
//!
//! The functions without `_opts` are the plain path (no geometry option);
//! the `_opts` functions add the options of [`GeomOpts`]. They are separate
//! copies, so that the code of the plain path does not change.

use crate::clip::{
    PolyClipScratch, clip_lines, clip_point, poly_ring_checks, quick_check, simple_clip_poly, simple_clip_poly_nodes,
};
use crate::polygon::geometry_bbox;
use crate::simplify::{
    NodeCtx, SimplifyScratch, close_poly, remove_noop, simplify_feature, simplify_feature_opts, to_tile_scale,
};
use crate::tiny::{
    ReviveInfo, TinyPolyAccum, TinyPolyResult, line_is_too_small, reduce_tiny_poly, revive_polygon,
};
use vtmake_core::{Draw, DrawVec, GeomType, VT_LINETO, VT_MOVETO};

/// Reusable buffers for the functions of this module. Keep one for each
/// thread.
#[derive(Default, Debug)]
pub struct GeomScratch {
    pub(crate) out: DrawVec,
    pub(crate) keep: DrawVec,
    /// With `-pn`: the edge nodes of the last polygon clip
    /// ([`clip_relative_opts`]), tile-relative.
    pub edge_nodes: DrawVec,
    pub(crate) poly: PolyClipScratch,
    pub(crate) simplify: SimplifyScratch,
}

/// The geometry options of tippecanoe that change the per-feature steps
/// (`prevent[]` and `additional[]` flags). [`GeomOpts::PLAIN`] is the
/// plain command line: all false.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GeomOpts {
    /// `-z`: the steps are different at the maximum zoom.
    pub maxzoom: u32,
    /// `-ps`: no simplification of lines and polygons.
    pub no_simplify: bool,
    /// `-pS`: no simplification at the maximum zoom.
    pub no_simplify_maxzoom: bool,
    /// `-pt`: no tiny-polygon reduction.
    pub no_tiny: bool,
    /// `-pT`: no tiny-polygon reduction at the maximum zoom.
    pub no_tiny_maxzoom: bool,
    /// `-aL`: stair-step grid below the maximum zoom (no simplification and
    /// no tiny-polygon reduction there).
    pub grid_low_zooms: bool,
    /// `-av`: Visvalingam instead of Douglas–Peucker.
    pub visvalingam: bool,
    /// `-pc`: no clip to the tile buffer.
    pub no_clipping: bool,
    /// `-pD`: each feature only in the tile of its bbox center.
    pub no_duplication: bool,
    /// `-pn`: keep the shared nodes in the simplification.
    pub shared_nodes: bool,
    /// `-ab`: the polygons are simplified before, by
    /// [`crate::shared_borders::find_common_edges`].
    pub shared_borders: bool,
}

impl GeomOpts {
    /// The plain command line (no geometry option).
    pub const PLAIN: GeomOpts = GeomOpts {
        maxzoom: 0,
        no_simplify: false,
        no_simplify_maxzoom: false,
        no_tiny: false,
        no_tiny_maxzoom: false,
        grid_low_zooms: false,
        visvalingam: false,
        no_clipping: false,
        no_duplication: false,
        shared_nodes: false,
        shared_borders: false,
    };
}

/// `geometry.cpp:point_within_tile`: strictly in the tile proper (no
/// buffer), tile-relative.
#[inline]
pub fn point_within_tile(x: i64, y: i64, z: u32) -> bool {
    let area = 1i64 << (32 - z);
    x >= 0 && y >= 0 && x < area && y < area
}

/// The origin of tile (z, tx, ty) in world coordinates (0 at z = 0).
#[inline(always)]
pub const fn tile_origin(z: u32, tx: u32, ty: u32) -> (i64, i64) {
    if z == 0 {
        (0, 0)
    } else {
        ((tx as i64) << (32 - z), (ty as i64) << (32 - z))
    }
}

/// `decode_geometry`: move absolute world coordinates to tile-relative
/// coordinates of tile (z, tx, ty), in place, and return the bbox of the
/// MOVETO/LINETO points (tile-relative).
#[inline]
pub fn to_tile_relative(geom: &mut [Draw], z: u32, tx: u32, ty: u32) -> [i64; 4] {
    let (ox, oy) = tile_origin(z, tx, ty);
    if ox != 0 || oy != 0 {
        for d in geom.iter_mut() {
            if d.op == VT_MOVETO || d.op == VT_LINETO {
                d.x -= ox;
                d.y -= oy;
            }
        }
    }
    geometry_bbox(geom)
}

/// The clip part of `tile.cpp:clip_to_tile`, for tile-relative input.
///
/// `bbox` is the tile-relative bbox of `geom` (as [`to_tile_relative`]
/// returns it). Returns false if the feature is fully outside the tile
/// plus buffer (`quick_check` = 0). Else `geom` is the clipped geometry
/// (it can be empty) and `bbox` is the bbox for [`child_tile_range`] (it
/// changes only at z = 0).
///
/// The clip runs unless the bbox is strictly inside the tile proper. When
/// the bbox is strictly inside the buffer box, this uses a fast path with
/// the same result as the full clip.
pub fn clip_relative(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    bbox: &mut [i64; 4],
    buffer: i64,
    scratch: &mut GeomScratch,
) -> bool {
    let mut quick = quick_check(bbox, z, buffer);

    if z == 0 {
        let world = 1i64 << 32;
        let edge = world * buffer / 256;
        if bbox[0] <= edge || bbox[2] >= world - edge {
            // Copy the geometry 360 degrees to the east and/or west, and
            // clip all copies.
            let n = geom.len();
            if bbox[0] <= edge {
                for i in 0..n {
                    let d = geom[i];
                    geom.push(Draw::new(d.op, d.x + world, d.y));
                }
            }
            if bbox[2] >= world - edge {
                for i in 0..n {
                    let d = geom[i];
                    geom.push(Draw::new(d.op, d.x - world, d.y));
                }
            }
            bbox[0] = 0;
            bbox[2] = world;
            quick = -1;
        }
    }

    match quick {
        0 => return false,
        1 => return true,
        _ => {}
    }

    let out = &mut scratch.out;
    out.clear();
    match (t, quick) {
        (GeomType::Point, _) => {
            if quick != 3 {
                clip_point(geom, z, buffer);
            }
        }
        (GeomType::Line, 3) => {}
        (GeomType::Line, _) => {
            clip_lines(geom, z, buffer, out);
            std::mem::swap(geom, out);
        }
        (GeomType::Polygon, 3) => {
            poly_ring_checks(geom, out);
            std::mem::swap(geom, out);
        }
        (GeomType::Polygon, _) => {
            simple_clip_poly(geom, z, buffer, out, &mut scratch.poly);
            std::mem::swap(geom, out);
        }
    }
    remove_noop(geom, t, 0);
    true
}

/// [`clip_relative`] with `-pc` (`no_clipping`) and `-pD` (`no_duplication`)
/// in `opts`. Not at z = 0, where tippecanoe always clips (date line):
///
/// - `-pD`: the feature keeps its full geometry in the tile that has the
///   center of its bbox (`point_within_tile`); in all other tiles of the
///   buffer box its geometry is empty.
/// - `-pc`: the feature keeps its full geometry if the clipped geometry is
///   not empty; else its geometry is empty.
#[inline]
pub fn clip_relative_opts(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    bbox: &mut [i64; 4],
    buffer: i64,
    opts: &GeomOpts,
    scratch: &mut GeomScratch,
) -> bool {
    if opts.shared_nodes {
        scratch.edge_nodes.clear();
    }
    if (opts.no_clipping || opts.no_duplication) && z != 0 {
        let quick = quick_check(bbox, z, buffer);
        if quick == 0 {
            return false;
        }
        if quick == 1 && !opts.no_duplication {
            return true;
        }
        // tippecanoe clips, but it keeps the clipped geometry only for its
        // test (and for the side effects of `-pn`: the edge nodes, and the
        // no-ops removed from its input).
        let mut full = std::mem::take(&mut scratch.keep);
        full.clear();
        full.extend_from_slice(geom);
        let inner = GeomOpts { no_clipping: false, no_duplication: false, ..*opts };
        let mut b = *bbox;
        clip_relative_opts(geom, t, z, &mut b, buffer, &inner, scratch);
        let clipped_empty = geom.is_empty();
        std::mem::swap(geom, &mut full);
        scratch.keep = full;
        if opts.shared_nodes && t == GeomType::Polygon {
            remove_noop(geom, t, 0);
        }
        let keep = if opts.no_duplication {
            point_within_tile((bbox[0] + bbox[2]) / 2, (bbox[1] + bbox[3]) / 2, z)
        } else {
            !clipped_empty
        };
        if !keep {
            geom.clear();
        }
        return true;
    }
    let mut quick = quick_check(bbox, z, buffer);

    if z == 0 {
        let world = 1i64 << 32;
        let edge = world * buffer / 256;
        if bbox[0] <= edge || bbox[2] >= world - edge {
            // Copy the geometry 360 degrees to the east and/or west, and
            // clip all copies.
            let n = geom.len();
            if bbox[0] <= edge {
                for i in 0..n {
                    let d = geom[i];
                    geom.push(Draw::new(d.op, d.x + world, d.y));
                }
            }
            if bbox[2] >= world - edge {
                for i in 0..n {
                    let d = geom[i];
                    geom.push(Draw::new(d.op, d.x - world, d.y));
                }
            }
            bbox[0] = 0;
            bbox[2] = world;
            quick = -1;
        }
    }

    match quick {
        0 => return false,
        // With `-pD`, tippecanoe clips also when the bbox is in the tile proper.
        1 if !opts.no_duplication => return true,
        _ => {}
    }

    let out = &mut scratch.out;
    out.clear();
    match (t, quick) {
        (GeomType::Point, _) => {
            if quick != 3 {
                clip_point(geom, z, buffer);
            }
        }
        (GeomType::Line, 3) => {}
        (GeomType::Line, _) => {
            clip_lines(geom, z, buffer, out);
            std::mem::swap(geom, out);
        }
        (GeomType::Polygon, _) if opts.shared_nodes => {
            let area = 1i64 << (32 - z);
            let b = buffer * area / 256;
            simple_clip_poly_nodes(geom, (-b, -b), (area + b, area + b), (0, 0), (area, area), out, &mut scratch.edge_nodes, &mut scratch.poly);
            std::mem::swap(geom, out);
        }
        (GeomType::Polygon, 3) => {
            poly_ring_checks(geom, out);
            std::mem::swap(geom, out);
        }
        (GeomType::Polygon, _) => {
            simple_clip_poly(geom, z, buffer, out, &mut scratch.poly);
            std::mem::swap(geom, out);
        }
    }
    remove_noop(geom, t, 0);
    true
}

/// `tile.cpp:clip_to_tile` for one feature at tile (z, tx, ty).
///
/// Input: `geom` in absolute world coordinates (the parent-clipped
/// geometry, or the original geometry at the first zoom). Output: `geom` is
/// tile-relative and clipped to the tile plus `buffer` (1/256 tile units).
///
/// Returns `None` if the feature is not in the tile. Else returns the bbox
/// for [`child_tile_range`] (tile-relative, from the geometry before the
/// clip). The clipped geometry can be empty; tippecanoe still counts such a
/// feature in the tile (see [`crate::drop::LayerRateDrop`]).
#[inline]
pub fn clip_to_tile(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    tx: u32,
    ty: u32,
    buffer: i64,
    scratch: &mut GeomScratch,
) -> Option<[i64; 4]> {
    let mut bbox = to_tile_relative(geom, z, tx, ty);
    if clip_relative(geom, t, z, &mut bbox, buffer, scratch) {
        Some(bbox)
    } else {
        None
    }
}

/// [`clip_to_tile`] with `-pc` and `-pD` in `opts` (see
/// [`clip_relative_opts`]).
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn clip_to_tile_opts(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    tx: u32,
    ty: u32,
    buffer: i64,
    opts: &GeomOpts,
    scratch: &mut GeomScratch,
) -> Option<[i64; 4]> {
    let mut bbox = to_tile_relative(geom, z, tx, ty);
    if clip_relative_opts(geom, t, z, &mut bbox, buffer, opts, scratch) {
        Some(bbox)
    } else {
        None
    }
}

/// The child tiles at `nextzoom` that `tile.cpp:rewrite` writes a feature
/// to, from the tile-relative `bbox` that [`clip_to_tile`] returns.
/// Returns the inclusive ranges `(x_min, y_min, x_max, y_max)` of child
/// tile numbers (absolute, at `nextzoom`).
///
/// This is the exact candidate set of tippecanoe, also with its clamp to
/// the children of this tile. Requires `z < nextzoom <= 24`.
#[inline]
pub fn child_tile_range(
    bbox: &[i64; 4],
    z: u32,
    nextzoom: u32,
    tx: u32,
    ty: u32,
    buffer: i64,
) -> (u32, u32, u32, u32) {
    let span = 1i64 << (nextzoom - z);
    let div = 1i64 << (32 - nextzoom - 8);
    let mut b = [bbox[0] / div, bbox[1] / div, bbox[2] / div, bbox[3] / div];
    b[0] -= buffer + 1;
    b[1] -= buffer + 1;
    b[2] += buffer;
    b[3] += buffer;
    for v in &mut b {
        if *v < 0 {
            *v = 0;
        }
        if *v >= 256 * span {
            *v = 256 * (span - 1);
        }
        *v /= 256;
    }
    let bx = tx as i64 * span;
    let by = ty as i64 * span;
    (
        (bx + b[0]) as u32,
        (by + b[1]) as u32,
        (bx + b[2]) as u32,
        (by + b[3]) as u32,
    )
}

/// The result of [`tile_reduce`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reduced {
    /// False: drop the feature.
    pub keep: bool,
    /// The polygon became only tiny-polygon squares (`sf.reduced`): do not
    /// simplify it.
    pub reduced: bool,
    /// The polygon had tiny rings. tippecanoe adds 1 to
    /// `strategies[z].tiny_polygons` for it (also when it is dropped).
    pub tiny_polygon: bool,
}

/// Steps 2 and 3 of spec 7 (in `write_tile` feature order): polygons go
/// through [`reduce_tiny_poly`] with the tile accumulator; lines and
/// polygons are dropped if [`line_is_too_small`]. In place.
#[inline]
pub fn tile_reduce(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    detail: u32,
    tiny_polygon_size: f64,
    accum: &mut TinyPolyAccum,
    scratch: &mut GeomScratch,
) -> Reduced {
    let mut r = Reduced {
        keep: true,
        ..Reduced::default()
    };
    if t == GeomType::Polygon {
        let out = &mut scratch.out;
        out.clear();
        let TinyPolyResult {
            still_needs_simplification,
            reduced_away,
        } = reduce_tiny_poly(geom, z, detail, tiny_polygon_size, accum, out);
        std::mem::swap(geom, out);
        r.tiny_polygon = reduced_away;
        r.reduced = !still_needs_simplification;
        if geom.is_empty() {
            r.keep = false;
            return r;
        }
    }
    if (t == GeomType::Polygon || t == GeomType::Line) && line_is_too_small(geom, z, detail) {
        r.keep = false;
    }
    r
}

/// [`tile_reduce`] with `-pt`, `-pT` and `-aL` in `opts`: these options
/// stop the tiny-polygon reduction (`-aL` at all zooms).
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn tile_reduce_opts(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    detail: u32,
    tiny_polygon_size: f64,
    opts: &GeomOpts,
    accum: &mut TinyPolyAccum,
    scratch: &mut GeomScratch,
) -> Reduced {
    let mut r = Reduced {
        keep: true,
        ..Reduced::default()
    };
    let prevent_tiny = opts.no_tiny || (opts.no_tiny_maxzoom && z == opts.maxzoom);
    if t == GeomType::Polygon && !prevent_tiny && !opts.grid_low_zooms {
        let out = &mut scratch.out;
        out.clear();
        let TinyPolyResult {
            still_needs_simplification,
            reduced_away,
        } = reduce_tiny_poly(geom, z, detail, tiny_polygon_size, accum, out);
        std::mem::swap(geom, out);
        r.tiny_polygon = reduced_away;
        r.reduced = !still_needs_simplification;
        if geom.is_empty() {
            r.keep = false;
            return r;
        }
    }
    if (t == GeomType::Polygon || t == GeomType::Line) && line_is_too_small(geom, z, detail) {
        r.keep = false;
    }
    r
}

/// The early simplification of `write_tile` (spec 7.5): when the kept
/// geometry of a tile is more than 10 MB, tippecanoe runs `simplify_feature`
/// on the features so far (in world coordinates, no `to_tile_scale`). Later
/// [`tile_simplify`] runs on them again. In place.
#[inline]
pub fn tile_simplify_early(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    detail: u32,
    simplification: f64,
    reduced: bool,
    scratch: &mut GeomScratch,
) {
    simplify_feature(geom, t, z, detail, simplification, reduced, &mut scratch.simplify);
}

/// [`tile_simplify_early`] with the options in `opts`.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn tile_simplify_early_opts(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    detail: u32,
    simplification: f64,
    reduced: bool,
    opts: &GeomOpts,
    nodes: &NodeCtx,
    scratch: &mut GeomScratch,
) {
    simplify_feature_opts(geom, t, z, detail, simplification, reduced, opts, nodes, &mut scratch.simplify);
}

/// The values that [`tile_finish_polygon`] needs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PolygonPre {
    /// World area before simplification (`get_mp_area`).
    pub area: f64,
    /// Point sums of the tile-unit geometry before cleaning.
    pub revive: ReviveInfo,
}

/// Steps 5 and 6 of spec 7: `simplify_feature` (unless `reduced`), then
/// `to_tile_scale`. In place. After this, `geom` is in tile units. For a
/// polygon, it is the input of polygon cleaning (`coalesce_polygon(geom,
/// true)`), and the return value is for [`tile_finish_polygon`].
#[inline]
pub fn tile_simplify(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    detail: u32,
    simplification: f64,
    reduced: bool,
    scratch: &mut GeomScratch,
) -> PolygonPre {
    let area = simplify_feature(
        geom,
        t,
        z,
        detail,
        simplification,
        reduced,
        &mut scratch.simplify,
    );
    to_tile_scale(geom, z, detail);
    let revive = if t == GeomType::Polygon {
        ReviveInfo::of(geom)
    } else {
        ReviveInfo::default()
    };
    PolygonPre { area, revive }
}

/// [`tile_simplify`] with the options in `opts`. `detail` is the detail of
/// the simplification (`line_detail`); `out_detail` is the detail of the
/// tile units (`--extra-detail` at the maximum zoom, else `detail`).
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn tile_simplify_opts(
    geom: &mut DrawVec,
    t: GeomType,
    z: u32,
    detail: u32,
    out_detail: u32,
    simplification: f64,
    reduced: bool,
    opts: &GeomOpts,
    nodes: &NodeCtx,
    scratch: &mut GeomScratch,
) -> PolygonPre {
    let area = simplify_feature_opts(
        geom,
        t,
        z,
        detail,
        simplification,
        reduced,
        opts,
        nodes,
        &mut scratch.simplify,
    );
    to_tile_scale(geom, z, out_detail);
    let revive = if t == GeomType::Polygon {
        ReviveInfo::of(geom)
    } else {
        ReviveInfo::default()
    };
    PolygonPre { area, revive }
}

/// Steps 7 to 9 of spec 7 for a polygon, after polygon cleaning. `geom` is
/// the cleaned result (closed rings, tile units). If it has fewer than 3
/// draws: revive it if `pre.area > 0`, else clear it. Then `close_poly`
/// and `remove_noop(polygon, 0)`. In place. An empty result means: drop
/// the feature.
#[inline]
pub fn tile_finish_polygon(geom: &mut DrawVec, pre: &PolygonPre, z: u32, detail: u32) {
    if geom.len() < 3 {
        geom.clear();
        if pre.area > 0.0 {
            revive_polygon(&pre.revive, pre.area, z, detail, geom);
        }
    }
    close_poly(geom);
    remove_noop(geom, GeomType::Polygon, 0);
}

/// Step 9 of spec 7 for a line: `remove_noop(line, 0)`. In place. An empty
/// result means: drop the feature.
#[inline]
pub fn tile_finish_line(geom: &mut DrawVec) {
    remove_noop(geom, GeomType::Line, 0);
}

/// The label point step of `write_tile` (`-aP`), for a polygon at tile
/// (z, tx, ty), before the rate drop: drop the polygon if it is too small
/// or has no area, else simplify it and replace it with its label point in
/// this tile ([`crate::label::checkerboard_anchors`]). Returns false if
/// the feature is dropped. After `true`, `geom` is one point (MOVETO).
/// With `-pn`, `nodes` has only the tile nodes so far (no global nodes).
#[allow(clippy::too_many_arguments)]
pub fn tile_label_point(
    geom: &mut DrawVec,
    z: u32,
    tx: u32,
    ty: u32,
    detail: u32,
    simplification: f64,
    label_point: u64,
    opts: &GeomOpts,
    nodes: &NodeCtx,
    scratch: &mut GeomScratch,
) -> bool {
    if line_is_too_small(geom, z, detail) || crate::polygon::get_mp_area(geom) <= 0.0 {
        return false;
    }
    let mark = !(opts.no_clipping || opts.no_duplication);
    crate::simplify::simplify_lines_opts(geom, z, detail, mark, simplification, 4, opts, nodes, &mut scratch.simplify);
    if geom.is_empty() {
        return false;
    }
    let out = &mut scratch.out;
    out.clear();
    crate::label::checkerboard_anchors(geom, tx, ty, z, label_point, out);
    std::mem::swap(geom, out);
    !geom.is_empty()
}
