//! Safe wrappers around tippecanoe's real C++ functions (cargo feature
//! `cpp-ref`, test use only). See `cpp-ref/shim.cpp`.

#![allow(missing_docs)]

use vtmake_core::{Draw, DrawVec, GeomType};
use std::sync::Once;

#[repr(C)]
#[derive(Clone, Copy)]
struct CDraw {
    x: i64,
    y: i64,
    op: i32,
    necessary: i32,
}

unsafe extern "C" {
    fn ref_init();
    fn ref_out_len(k: i32) -> usize;
    fn ref_out_ptr(k: i32) -> *const CDraw;

    fn ref_cvt_i64(d: f64) -> i64;
    fn ref_cvt_u64(d: f64) -> u64;
    fn ref_log(x: f64) -> f64;
    fn ref_tan(x: f64) -> f64;
    fn ref_cos(x: f64) -> f64;
    fn ref_sinh(x: f64) -> f64;
    fn ref_atan(x: f64) -> f64;
    fn ref_exp(x: f64) -> f64;

    fn ref_lonlat2tile(lon: f64, lat: f64, zoom: i32, x: *mut i64, y: *mut i64);
    fn ref_tile2lonlat(x: i64, y: i64, zoom: i32, lon: *mut f64, lat: *mut f64);
    fn ref_encode_quadkey(wx: u32, wy: u32) -> u64;
    fn ref_decode_quadkey(ix: u64, wx: *mut u32, wy: *mut u32);
    fn ref_bit_reverse(v: u64) -> u64;

    fn ref_get_area(g: *const CDraw, n: usize, i: usize, j: usize) -> f64;
    fn ref_get_mp_area(g: *const CDraw, n: usize) -> f64;
    fn ref_fix_polygon(g: *const CDraw, n: usize);
    fn ref_center_of_mass_mp(g: *const CDraw, n: usize, x: *mut i64, y: *mut i64);
    fn ref_feature_index(t: i32, g: *const CDraw, n: usize, bbox: *const i64) -> u64;

    fn ref_quick_check(bbox: *const i64, z: i32, buffer: i64) -> i32;
    fn ref_clip(
        x0: *mut i64,
        y0: *mut i64,
        x1: *mut i64,
        y1: *mut i64,
        xmin: i64,
        ymin: i64,
        xmax: i64,
        ymax: i64,
    ) -> i32;
    fn ref_clip_point(g: *const CDraw, n: usize, z: i32, buffer: i64);
    fn ref_clip_lines(g: *const CDraw, n: usize, z: i32, buffer: i64);
    fn ref_simple_clip_poly(g: *const CDraw, n: usize, z: i32, buffer: i32);
    fn ref_clip_to_tile(
        g: *const CDraw,
        n: usize,
        t: i32,
        z: i32,
        tx: u32,
        ty: u32,
        buffer: i64,
        bbox_out: *mut i64,
    ) -> i32;
    fn ref_clip_to_tile_opts(
        g: *const CDraw,
        n: usize,
        t: i32,
        z: i32,
        tx: u32,
        ty: u32,
        buffer: i64,
        bbox_out: *mut i64,
        o: *const RefOpts,
    ) -> i32;
    fn ref_simplify_lines_nodes(
        g: *const CDraw,
        n: usize,
        z: i32,
        tx: i32,
        ty: i32,
        detail: i32,
        mark: i32,
        simplification: f64,
        retain: usize,
        tile: *const CDraw,
        ntile: usize,
        global: *const u64,
        nglobal: usize,
        o: *const RefOpts,
    );
    fn ref_find_common_edges(g: *const CDraw, n: usize, z: i32, detail: i32, simplification: f64, o: *const RefOpts);
    fn ref_visvalingam(g: *const CDraw, n: usize, start: usize, end: usize, threshold: f64, retain: usize);
    fn ref_stairstep(g: *const CDraw, n: usize, z: i32, detail: i32);
    fn ref_pnpoly(g: *const CDraw, n: usize, start: usize, nvert: usize, x: i64, y: i64) -> i32;
    fn ref_check_polygon(g: *const CDraw, n: usize);
    fn ref_label_goodness(g: *const CDraw, n: usize, x: i64, y: i64) -> f64;
    fn ref_polygon_to_anchor(g: *const CDraw, n: usize);
    fn ref_checkerboard_anchors(g: *const CDraw, n: usize, tx: i32, ty: i32, z: i32, label_point: u64);
    fn ref_simplify_lines_opts(
        g: *const CDraw,
        n: usize,
        z: i32,
        detail: i32,
        mark: i32,
        simplification: f64,
        retain: usize,
        o: *const RefOpts,
    );
    fn ref_simplify_feature_opts(
        g: *const CDraw,
        n: usize,
        t: i32,
        z: i32,
        detail: i32,
        simplification: f64,
        reduced: i32,
        o: *const RefOpts,
    ) -> f64;
    fn ref_child_range(
        bbox: *const i64,
        z: i32,
        nextzoom: i32,
        tx: u32,
        ty: u32,
        buffer: i32,
        out4: *mut u32,
    );

    fn ref_remove_noop(g: *const CDraw, n: usize, t: i32, shift: i32);
    fn ref_impose_tile_boundaries(g: *const CDraw, n: usize, extent: i64);
    fn ref_distance_from_line(px: i64, py: i64, ax: i64, ay: i64, bx: i64, by: i64) -> f64;
    fn ref_douglas_peucker(
        g: *const CDraw,
        n: usize,
        start: i32,
        count: i32,
        e: f64,
        kept: usize,
        retain: usize,
    );
    fn ref_simplify_lines(
        g: *const CDraw,
        n: usize,
        z: i32,
        detail: i32,
        mark: i32,
        simplification: f64,
        retain: usize,
    );
    fn ref_simplify_feature(
        g: *const CDraw,
        n: usize,
        t: i32,
        z: i32,
        detail: i32,
        simplification: f64,
        reduced: i32,
    ) -> f64;
    fn ref_to_tile_scale(g: *const CDraw, n: usize, z: i32, detail: i32);
    fn ref_close_poly(g: *const CDraw, n: usize);

    fn ref_reduce_tiny_poly(
        g: *const CDraw,
        n: usize,
        z: i32,
        detail: i32,
        tiny: f64,
        accum: *mut f64,
        still: *mut i32,
        reduced: *mut i32,
    );
    fn ref_line_is_too_small(g: *const CDraw, n: usize, z: i32, detail: i32) -> i32;
    fn ref_revive_polygon(g: *const CDraw, n: usize, area: f64, z: i32, detail: i32);
    fn ref_coalesce_polygon(g: *const CDraw, n: usize, scale_up: i32);
    fn ref_tile_steps(
        g: *const CDraw,
        n: usize,
        t: i32,
        z: i32,
        detail: i32,
        simplification: f64,
        tiny: f64,
        accum: *mut f64,
        tiny_counted: *mut i32,
    ) -> i32;

    fn ref_drop_new(maxzoom: i32, basezoom: i32, droprate: f64) -> *mut std::ffi::c_void;
    fn ref_drop_calc(ds: *mut std::ffi::c_void, t: i32, maxzoom: i32) -> i32;
    fn ref_drop_free(ds: *mut std::ffi::c_void);
}

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| unsafe { ref_init() });
}

fn to_c(g: &[Draw]) -> Vec<CDraw> {
    g.iter()
        .map(|d| CDraw {
            x: d.x,
            y: d.y,
            op: d.op as i32,
            necessary: d.necessary as i32,
        })
        .collect()
}

fn out(k: i32) -> DrawVec {
    unsafe {
        let n = ref_out_len(k);
        let p = ref_out_ptr(k);
        if n == 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(p, n)
            .iter()
            .map(|c| Draw {
                op: c.op as u8,
                x: c.x,
                y: c.y,
                necessary: c.necessary != 0,
            })
            .collect()
    }
}

macro_rules! with_c {
    ($g:expr, |$p:ident, $n:ident| $body:expr) => {{
        init();
        let c = to_c($g);
        let $p = c.as_ptr();
        let $n = c.len();
        unsafe { $body }
    }};
}

pub fn cvt_i64(d: f64) -> i64 {
    unsafe { ref_cvt_i64(d) }
}
pub fn cvt_u64(d: f64) -> u64 {
    unsafe { ref_cvt_u64(d) }
}
pub fn log(x: f64) -> f64 {
    unsafe { ref_log(x) }
}
pub fn tan(x: f64) -> f64 {
    unsafe { ref_tan(x) }
}
pub fn cos(x: f64) -> f64 {
    unsafe { ref_cos(x) }
}
pub fn sinh(x: f64) -> f64 {
    unsafe { ref_sinh(x) }
}
pub fn atan(x: f64) -> f64 {
    unsafe { ref_atan(x) }
}
pub fn exp(x: f64) -> f64 {
    unsafe { ref_exp(x) }
}

pub fn lonlat2tile(lon: f64, lat: f64, zoom: u32) -> (i64, i64) {
    init();
    let (mut x, mut y) = (0, 0);
    unsafe { ref_lonlat2tile(lon, lat, zoom as i32, &mut x, &mut y) };
    (x, y)
}

pub fn tile2lonlat(x: i64, y: i64, zoom: u32) -> (f64, f64) {
    let (mut lon, mut lat) = (0.0, 0.0);
    unsafe { ref_tile2lonlat(x, y, zoom as i32, &mut lon, &mut lat) };
    (lon, lat)
}

pub fn encode_quadkey(wx: u32, wy: u32) -> u64 {
    unsafe { ref_encode_quadkey(wx, wy) }
}

pub fn decode_quadkey(ix: u64) -> (u32, u32) {
    let (mut x, mut y) = (0, 0);
    unsafe { ref_decode_quadkey(ix, &mut x, &mut y) };
    (x, y)
}

pub fn bit_reverse(v: u64) -> u64 {
    unsafe { ref_bit_reverse(v) }
}

pub fn get_area(g: &[Draw], i: usize, j: usize) -> f64 {
    with_c!(g, |p, n| ref_get_area(p, n, i, j))
}

pub fn get_mp_area(g: &[Draw]) -> f64 {
    with_c!(g, |p, n| ref_get_mp_area(p, n))
}

pub fn fix_polygon(g: &[Draw]) -> DrawVec {
    with_c!(g, |p, n| {
        ref_fix_polygon(p, n);
        out(0)
    })
}

pub fn center_of_mass_mp(g: &[Draw]) -> (i64, i64) {
    with_c!(g, |p, n| {
        let (mut x, mut y) = (0, 0);
        ref_center_of_mass_mp(p, n, &mut x, &mut y);
        (x, y)
    })
}

pub fn feature_index(t: GeomType, g: &[Draw], bbox: &[i64; 4]) -> u64 {
    with_c!(g, |p, n| ref_feature_index(t as i32, p, n, bbox.as_ptr()))
}

pub fn quick_check(bbox: &[i64; 4], z: u32, buffer: i64) -> i32 {
    unsafe { ref_quick_check(bbox.as_ptr(), z as i32, buffer) }
}

#[allow(clippy::too_many_arguments)]
pub fn clip(seg: [i64; 4], xmin: i64, ymin: i64, xmax: i64, ymax: i64) -> (i32, [i64; 4]) {
    let [mut x0, mut y0, mut x1, mut y1] = seg;
    let c = unsafe { ref_clip(&mut x0, &mut y0, &mut x1, &mut y1, xmin, ymin, xmax, ymax) };
    (c, [x0, y0, x1, y1])
}

pub fn clip_point(g: &[Draw], z: u32, buffer: i64) -> DrawVec {
    with_c!(g, |p, n| {
        ref_clip_point(p, n, z as i32, buffer);
        out(0)
    })
}

pub fn clip_lines(g: &[Draw], z: u32, buffer: i64) -> DrawVec {
    with_c!(g, |p, n| {
        ref_clip_lines(p, n, z as i32, buffer);
        out(0)
    })
}

pub fn simple_clip_poly(g: &[Draw], z: u32, buffer: i64) -> DrawVec {
    with_c!(g, |p, n| {
        ref_simple_clip_poly(p, n, z as i32, buffer as i32);
        out(0)
    })
}

/// `tile.cpp:clip_to_tile` with the `decode_geometry` step. Input in
/// absolute world coordinates. Returns `None` if outside, else the clipped
/// tile-relative geometry and `sf.bbox`.
pub fn clip_to_tile(
    g: &[Draw],
    t: GeomType,
    z: u32,
    tx: u32,
    ty: u32,
    buffer: i64,
) -> Option<(DrawVec, [i64; 4])> {
    with_c!(g, |p, n| {
        let mut bbox = [0i64; 4];
        let outside = ref_clip_to_tile(p, n, t as i32, z as i32, tx, ty, buffer, bbox.as_mut_ptr());
        if outside != 0 {
            None
        } else {
            Some((out(0), bbox))
        }
    })
}

/// `ref_geom_opts` in `shim.cpp`.
#[repr(C)]
struct RefOpts {
    maxzoom: i32,
    no_simplify: i32,
    no_simplify_maxzoom: i32,
    grid_low_zooms: i32,
    visvalingam: i32,
    no_clipping: i32,
    no_duplication: i32,
    shared_nodes: i32,
    shared_borders: i32,
}

impl RefOpts {
    fn of(o: &crate::tile::GeomOpts) -> RefOpts {
        RefOpts {
            maxzoom: o.maxzoom as i32,
            no_simplify: o.no_simplify as i32,
            no_simplify_maxzoom: o.no_simplify_maxzoom as i32,
            grid_low_zooms: o.grid_low_zooms as i32,
            visvalingam: o.visvalingam as i32,
            no_clipping: o.no_clipping as i32,
            no_duplication: o.no_duplication as i32,
            shared_nodes: o.shared_nodes as i32,
            shared_borders: o.shared_borders as i32,
        }
    }
}

/// `clip_to_tile` with the `-pc`, `-pD` and `-pn` flags of `o`: the
/// geometry, the bbox and the edge nodes.
pub fn clip_to_tile_opts(
    g: &[Draw],
    t: GeomType,
    z: u32,
    tx: u32,
    ty: u32,
    buffer: i64,
    o: &crate::tile::GeomOpts,
) -> Option<(DrawVec, [i64; 4], DrawVec)> {
    let ro = RefOpts::of(o);
    with_c!(g, |p, n| {
        let mut bbox = [0i64; 4];
        let outside = ref_clip_to_tile_opts(p, n, t as i32, z as i32, tx, ty, buffer, bbox.as_mut_ptr(), &ro);
        if outside != 0 { None } else { Some((out(0), bbox, out(1))) }
    })
}

/// `simplify_lines` with the shared nodes of `-pn`.
#[allow(clippy::too_many_arguments)]
pub fn simplify_lines_nodes(
    g: &[Draw],
    z: u32,
    tx: u32,
    ty: u32,
    detail: u32,
    mark: bool,
    simplification: f64,
    retain: usize,
    tile: &[Draw],
    global: &[u64],
    o: &crate::tile::GeomOpts,
) -> DrawVec {
    let ro = RefOpts::of(o);
    let t = to_c(tile);
    with_c!(g, |p, n| {
        ref_simplify_lines_nodes(
            p, n, z as i32, tx as i32, ty as i32, detail as i32, mark as i32, simplification, retain, t.as_ptr(), t.len(),
            global.as_ptr(), global.len(), &ro,
        );
        out(0)
    })
}

/// `find_common_edges` for the polygons `polys` (one layer).
pub fn find_common_edges(polys: &[DrawVec], z: u32, detail: u32, simplification: f64, o: &crate::tile::GeomOpts) -> Vec<DrawVec> {
    let ro = RefOpts::of(o);
    let mut all: DrawVec = Vec::new();
    for g in polys {
        all.extend_from_slice(g);
        all.push(Draw::new(99, 0, 0));
    }
    let flat = with_c!(&all, |p, n| {
        ref_find_common_edges(p, n, z as i32, detail as i32, simplification, &ro);
        out(0)
    });
    flat.split(|d| d.op == 99).take(polys.len()).map(|s| s.to_vec()).collect()
}

pub fn visvalingam(g: &[Draw], start: usize, end: usize, threshold: f64, retain: usize) -> DrawVec {
    with_c!(g, |p, n| {
        ref_visvalingam(p, n, start, end, threshold, retain);
        out(0)
    })
}

pub fn stairstep(g: &[Draw], z: u32, detail: u32) -> DrawVec {
    with_c!(g, |p, n| {
        ref_stairstep(p, n, z as i32, detail as i32);
        out(0)
    })
}

pub fn pnpoly(g: &[Draw], start: usize, nvert: usize, x: i64, y: i64) -> bool {
    with_c!(g, |p, n| ref_pnpoly(p, n, start, nvert, x, y) != 0)
}

pub fn label_goodness(g: &[Draw], x: i64, y: i64) -> f64 {
    with_c!(g, |p, n| ref_label_goodness(p, n, x, y))
}

pub fn polygon_to_anchor(g: &[Draw]) -> DrawVec {
    with_c!(g, |p, n| {
        ref_polygon_to_anchor(p, n);
        out(0)
    })
}

pub fn checkerboard_anchors(g: &[Draw], tx: u32, ty: u32, z: u32, label_point: u64) -> DrawVec {
    with_c!(g, |p, n| {
        ref_checkerboard_anchors(p, n, tx as i32, ty as i32, z as i32, label_point);
        out(0)
    })
}

/// `check_polygon`: the geometry after the call.
pub fn check_polygon(g: &[Draw]) -> DrawVec {
    with_c!(g, |p, n| {
        ref_check_polygon(p, n);
        out(0)
    })
}

#[allow(clippy::too_many_arguments)]
pub fn simplify_lines_opts(
    g: &[Draw],
    z: u32,
    detail: u32,
    mark: bool,
    simplification: f64,
    retain: usize,
    o: &crate::tile::GeomOpts,
) -> DrawVec {
    let ro = RefOpts::of(o);
    with_c!(g, |p, n| {
        ref_simplify_lines_opts(p, n, z as i32, detail as i32, mark as i32, simplification, retain, &ro);
        out(0)
    })
}

#[allow(clippy::too_many_arguments)]
pub fn simplify_feature_opts(
    g: &[Draw],
    t: GeomType,
    z: u32,
    detail: u32,
    simplification: f64,
    reduced: bool,
    o: &crate::tile::GeomOpts,
) -> (DrawVec, f64) {
    let ro = RefOpts::of(o);
    with_c!(g, |p, n| {
        let a = ref_simplify_feature_opts(p, n, t as i32, z as i32, detail as i32, simplification, reduced as i32, &ro);
        (out(0), a)
    })
}

pub fn child_range(
    bbox: &[i64; 4],
    z: u32,
    nextzoom: u32,
    tx: u32,
    ty: u32,
    buffer: i64,
) -> (u32, u32, u32, u32) {
    let mut o = [0u32; 4];
    unsafe {
        ref_child_range(
            bbox.as_ptr(),
            z as i32,
            nextzoom as i32,
            tx,
            ty,
            buffer as i32,
            o.as_mut_ptr(),
        )
    };
    (o[0], o[1], o[2], o[3])
}

pub fn remove_noop(g: &[Draw], t: GeomType, shift: u32) -> DrawVec {
    with_c!(g, |p, n| {
        ref_remove_noop(p, n, t as i32, shift as i32);
        out(0)
    })
}

pub fn impose_tile_boundaries(g: &[Draw], extent: i64) -> DrawVec {
    with_c!(g, |p, n| {
        ref_impose_tile_boundaries(p, n, extent);
        out(0)
    })
}

pub fn distance_from_line(px: i64, py: i64, ax: i64, ay: i64, bx: i64, by: i64) -> f64 {
    unsafe { ref_distance_from_line(px, py, ax, ay, bx, by) }
}

pub fn douglas_peucker(
    g: &[Draw],
    start: usize,
    count: usize,
    e: f64,
    kept: usize,
    retain: usize,
) -> DrawVec {
    with_c!(g, |p, n| {
        ref_douglas_peucker(p, n, start as i32, count as i32, e, kept, retain);
        out(0)
    })
}

pub fn simplify_lines(
    g: &[Draw],
    z: u32,
    detail: u32,
    mark: bool,
    simplification: f64,
    retain: usize,
) -> DrawVec {
    with_c!(g, |p, n| {
        ref_simplify_lines(
            p,
            n,
            z as i32,
            detail as i32,
            mark as i32,
            simplification,
            retain,
        );
        out(0)
    })
}

pub fn simplify_feature(
    g: &[Draw],
    t: GeomType,
    z: u32,
    detail: u32,
    simplification: f64,
    reduced: bool,
) -> (DrawVec, f64) {
    with_c!(g, |p, n| {
        let a = ref_simplify_feature(
            p,
            n,
            t as i32,
            z as i32,
            detail as i32,
            simplification,
            reduced as i32,
        );
        (out(0), a)
    })
}

pub fn to_tile_scale(g: &[Draw], z: u32, detail: u32) -> DrawVec {
    with_c!(g, |p, n| {
        ref_to_tile_scale(p, n, z as i32, detail as i32);
        out(0)
    })
}

pub fn close_poly(g: &[Draw]) -> DrawVec {
    with_c!(g, |p, n| {
        ref_close_poly(p, n);
        out(0)
    })
}

/// Returns (output, still_needs_simplification, reduced_away).
pub fn reduce_tiny_poly(
    g: &[Draw],
    z: u32,
    detail: u32,
    tiny: f64,
    accum: &mut f64,
) -> (DrawVec, bool, bool) {
    with_c!(g, |p, n| {
        let (mut s, mut r) = (0, 0);
        ref_reduce_tiny_poly(p, n, z as i32, detail as i32, tiny, accum, &mut s, &mut r);
        (out(0), s != 0, r != 0)
    })
}

pub fn line_is_too_small(g: &[Draw], z: u32, detail: u32) -> bool {
    with_c!(g, |p, n| ref_line_is_too_small(
        p,
        n,
        z as i32,
        detail as i32
    ) != 0)
}

pub fn revive_polygon(g: &[Draw], area: f64, z: u32, detail: u32) -> DrawVec {
    with_c!(g, |p, n| {
        ref_revive_polygon(p, n, area, z as i32, detail as i32);
        out(0)
    })
}

/// `coalesce_polygon` (wagyu). Only for the tests that need a cleaned
/// polygon as input.
pub fn coalesce_polygon(g: &[Draw], scale_up: bool) -> DrawVec {
    with_c!(g, |p, n| {
        ref_coalesce_polygon(p, n, scale_up as i32);
        out(0)
    })
}

/// The result of [`tile_steps`].
#[derive(Debug, Clone, PartialEq)]
pub struct TileSteps {
    pub kept: bool,
    pub tiny_counted: bool,
    /// Tile-unit geometry before cleaning.
    pub pre_clean: DrawVec,
    /// Cleaned geometry (polygons).
    pub cleaned: DrawVec,
    /// Final geometry.
    pub fin: DrawVec,
}

/// The per-feature steps of `write_tile`/`simplification_worker` in C++.
pub fn tile_steps(
    g: &[Draw],
    t: GeomType,
    z: u32,
    detail: u32,
    simplification: f64,
    tiny: f64,
    accum: &mut f64,
) -> TileSteps {
    with_c!(g, |p, n| {
        let mut tc = 0;
        let kept = ref_tile_steps(
            p,
            n,
            t as i32,
            z as i32,
            detail as i32,
            simplification,
            tiny,
            accum,
            &mut tc,
        ) != 0;
        TileSteps {
            kept,
            tiny_counted: tc != 0,
            pre_clean: out(0),
            cleaned: out(1),
            fin: out(2),
        }
    })
}

/// The C++ drop state (`prep_drop_states` / `calc_feature_minzoom`).
pub struct DropState {
    p: *mut std::ffi::c_void,
    maxzoom: i32,
}

impl DropState {
    pub fn new(maxzoom: u32, basezoom: u32, droprate: f64) -> Self {
        init();
        DropState {
            p: unsafe { ref_drop_new(maxzoom as i32, basezoom as i32, droprate) },
            maxzoom: maxzoom as i32,
        }
    }
    pub fn feature_minzoom(&mut self, t: GeomType) -> i32 {
        unsafe { ref_drop_calc(self.p, t as i32, self.maxzoom) }
    }
}

impl Drop for DropState {
    fn drop(&mut self) {
        unsafe { ref_drop_free(self.p) }
    }
}
