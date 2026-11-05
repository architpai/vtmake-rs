//! Tiling: depth-first over each subtree, in parallel (rayon work stealing).
//! The per-feature steps are in vtmake-geom (`tile.rs`, see its module docs for the
//! order); polygon cleaning is vtmake-wagyu.
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC;
//! Copyright (c) 2014, Mapbox Inc. BSD 2-Clause: LICENSE-tippecanoe.md.

use crate::attrs::TileState;
use crate::store::{Chunk, FeatMeta, Sorted};
use vtmake_core::{AttrType, Draw, DrawVec, GeomType, VT_LINETO, VT_MOVETO};
use vtmake_geom::shared_borders::find_common_edges;
use vtmake_geom::tile::{
    GeomOpts, GeomScratch, PolygonPre, child_tile_range, clip_to_tile, clip_to_tile_opts, tile_finish_line,
    tile_finish_polygon, tile_label_point, tile_origin, tile_reduce, tile_reduce_opts, tile_simplify,
    tile_simplify_early, tile_simplify_early_opts, tile_simplify_opts,
};
use vtmake_geom::{LayerRateDrop, NodeCtx, TinyPolyAccum};
use vtmake_mvt::{MvtFeature, MvtLayer, MvtPoint, MvtTile, stringified_to_mvt_value};
use crossbeam_channel::Sender;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Detail settings from `main.cpp:main` and `tile.cpp:write_tile`.
fn detail(ctx: &Ctx, z: u32) -> u32 {
    if z as usize == ctx.maxzoom.load(Ordering::Relaxed) { ctx.opts.full_detail as u32 } else { ctx.opts.low_detail as u32 }
}
const TINY_POLYGON_SIZE: f64 = 2.0;
const SIMPLIFICATION: f64 = 1.0;

/// The geometry options of the per-feature steps.
pub struct Geo {
    /// The option flags. `opts.maxzoom` is set for each tile (the maximum
    /// zoom can grow with `--extend-zooms-if-still-dropping`).
    pub opts: GeomOpts,
    /// `--extra-detail`, or -1.
    pub extra_detail: i32,
    /// `-S`, and `--simplification-at-maximum-zoom` (or -1).
    pub simplification: f64,
    pub maxzoom_simplification: f64,
    /// `--tiny-polygon-size`.
    pub tiny_polygon_size: f64,
    /// `-a@`.
    pub check_polygons: bool,
    /// `-aP`.
    pub label_points: bool,
    /// `-pn`: the global shared nodes (`read::shared_nodes`).
    pub nodes: Vec<u64>,
    /// No geometry option and tippecanoe's default values (the plain command
    /// line): the tile steps call the plain functions of vtmake-geom.
    pub plain: bool,
}

impl Geo {
    /// The geometry settings of a command line.
    pub fn new(o: &crate::cli::Options) -> Geo {
        let p = |c: u8| o.prevent[c as usize];
        let a = |c: u8| o.additional[c as usize];
        let mut g = Geo {
            opts: GeomOpts {
                maxzoom: o.maxzoom as u32,
                no_simplify: p(b's'),
                no_simplify_maxzoom: p(b'S'),
                no_tiny: p(b't'),
                no_tiny_maxzoom: p(b'T'),
                grid_low_zooms: a(b'L'),
                visvalingam: a(b'v'),
                no_clipping: p(b'c'),
                no_duplication: p(b'D'),
                shared_nodes: p(b'n'),
                shared_borders: a(b'b'),
            },
            extra_detail: o.extra_detail,
            simplification: o.simplification,
            maxzoom_simplification: o.maxzoom_simplification,
            tiny_polygon_size: o.tiny_polygon_size as f64,
            check_polygons: a(b'@'),
            label_points: a(b'P'),
            nodes: Vec::new(),
            plain: false,
        };
        g.plain = GeomOpts { maxzoom: 0, ..g.opts } == GeomOpts::PLAIN
            && g.extra_detail < 0
            && !g.check_polygons
            && !g.label_points
            && g.simplification == 1.0
            && g.maxzoom_simplification <= 0.0
            && g.tiny_polygon_size == 2.0;
        g
    }
}

/// `clip_to_tile`, with the options only if there are some.
#[inline]
fn clip<const P: bool>(ctx: &Ctx, g: &mut DrawVec, t: GeomType, z: u32, x: u32, y: u32, s: &mut GeomScratch) -> Option<[i64; 4]> {
    if P { clip_to_tile(g, t, z, x, y, ctx.buffer, s) } else { clip_to_tile_opts(g, t, z, x, y, ctx.buffer, &ctx.geo.opts, s) }
}

/// The detail of the tile units (`sf.extra_detail`) for the line detail `d`:
/// `--extra-detail` at the maximum zoom when `d` is the full detail (not
/// more than `30 - z`), else `d`.
#[inline]
fn out_detail(ctx: &Ctx, z: u32, d: u32) -> u32 {
    if z as usize == ctx.maxzoom.load(Ordering::Relaxed) && ctx.geo.extra_detail >= 0 && d == detail(ctx, z) {
        (ctx.geo.extra_detail as u32).min(30 - z)
    } else {
        d
    }
}

/// The simplification at zoom `z`.
#[inline]
fn simplification(ctx: &Ctx, z: u32) -> f64 {
    if z as usize == ctx.maxzoom.load(Ordering::Relaxed) && ctx.geo.maxzoom_simplification > 0.0 {
        ctx.geo.maxzoom_simplification
    } else {
        ctx.geo.simplification
    }
}

/// [`simplification`] and the tiny-polygon size; with `P` (the plain command
/// line) tippecanoe's defaults, as constants.
#[inline]
fn simp<const P: bool>(ctx: &Ctx, z: u32) -> f64 {
    if P { SIMPLIFICATION } else { simplification(ctx, z) }
}
#[inline]
fn tiny_size<const P: bool>(ctx: &Ctx) -> f64 {
    if P { TINY_POLYGON_SIZE } else { ctx.geo.tiny_polygon_size }
}

/// The simplification of the label point step of `write_tile` (`-aP`).
/// tippecanoe reads `sf.simplification` there before it sets it (the field
/// of `serial_feature` is not initialized), so its output changes from run
/// to run, and felt fails its own `-aP` test. vtmake uses the value that
/// tippecanoe sets in this field later: the simplification of the zoom.
#[inline]
fn label_simplification(ctx: &Ctx, z: u32) -> f64 {
    simplification(ctx, z)
}

/// `geometry.cpp:check_polygon` (`-a@`), after the polygon cleaning: remove
/// the no-ops, and write tippecanoe's messages for rings that are not valid.
fn check_polygon(g: &mut DrawVec) {
    vtmake_geom::remove_noop(g, GeomType::Polygon, 0);
    if vtmake_wagyu::snap_round_changes(g) {
        eprintln!("Internal error: self-intersecting polygon");
    }
    vtmake_geom::polygon::check_polygon_rings(g);
}

pub use crate::spool::Spool;

pub struct Ctx<'a> {
    // Variable depth: retain child input so a dropping retry can restore it.
    pub skip_tiles: std::sync::Mutex<std::collections::BTreeSet<(u8, u32, u32)>>,
    // The parent had rate drops or features waiting for a deeper zoom.
    pub trial_blocked: std::sync::Mutex<std::collections::BTreeSet<(u8, u32, u32)>>,
    pub drop_state: std::sync::Mutex<(crate::drop::Threshold, crate::drop::Threshold)>,
    pub staged: std::sync::Mutex<Spool>,
    pub still_dropping: std::sync::atomic::AtomicBool,
    pub gaps: Vec<u64>,
    pub extents: Vec<u64>,
    pub opts: &'a crate::cli::Options,
    pub pending: Option<std::sync::Mutex<Spool>>,
    pub failed: AtomicUsize,
    pub incomplete: std::sync::atomic::AtomicBool,
    pub chunks: &'a [Chunk],
    pub order: &'a [Sorted],
    pub minzoom: u8,
    pub maxzoom: AtomicUsize,
    pub buffer: i64,
    /// The default layer name of each chunk (`--layer` or from the file name).
    pub default_layers: &'a [&'a str],
    /// Tile gzip mode (`VTMAKE_GZIP`).
    pub gzip: vtmake_mvt::gzip::Mode,
    pub out: Sender<(u8, u32, u32, Vec<u8>)>,
    /// 1 + the highest zoom that had input features (metadata maxzoom, spec 5.5),
    /// or 0.
    pub max_tiled: AtomicUsize,
    /// tippecanoe starts at `-Z` (all features are in the tiles at `-Z`). Else it
    /// starts at a lower zoom, and a feature with a maxzoom below `-Z` does not
    /// come to `-Z`.
    pub starts_at_minzoom: bool,
    /// `iz`: the zoom where tippecanoe starts (spec 5.4).
    pub first_zoom: u8,
    pub tiles: AtomicU64,
    pub parallel_bytes: AtomicUsize,
    pub parallel_tasks: AtomicUsize,
    pub parallel_limit: usize,
    pub parallel_budget: usize,
    /// For the metadata center: (vertex count, key) of the best maxzoom tile.
    pub center: std::sync::Mutex<(u64, u64)>,
    /// `strategies[z]`: `dropped_by_rate` and `tiny_polygons`.
    pub strategies: Vec<[AtomicU64; 8]>,
    pub geo: Geo,
}

/// One feature in one tile: global sort position and its geometry (absolute
/// world coordinates, already clipped by the parent tile) in the tile arena.
#[derive(Clone, Copy)]
pub struct TF {
    pub gap: u64,
    pub pos: u32,
    pub start: u32,
    pub len: u32,
}

struct Scratch {
    geom: GeomScratch,
    simplify: vtmake_geom::SimplifyScratch,
    cleaner: vtmake_wagyu::Cleaner,
    g: DrawVec,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch {
        geom: GeomScratch::default(),
        simplify: Default::default(),
        cleaner: vtmake_wagyu::Cleaner::new(),
        g: Vec::new(),
    });
}

impl Ctx<'_> {
    #[inline]
    pub(crate) fn meta(&self, pos: u32) -> (&Chunk, &FeatMeta) {
        let s = self.order[pos as usize];
        let c = &self.chunks[s.chunk as usize];
        (c, &c.metas[s.local as usize])
    }

    /// `-aP`: the label point quadkey of the feature.
    fn label_point(&self, pos: u32) -> u64 {
        let s = self.order[pos as usize];
        self.chunks[s.chunk as usize].extra.get(s.local as usize).map_or(0, |e| e.label_point)
    }

    pub(crate) fn layer_name(&self, pos: u32) -> &str {
        let s = self.order[pos as usize];
        let c = &self.chunks[s.chunk as usize];
        let m = &c.metas[s.local as usize];
        if m.layer == u16::MAX { self.default_layers[s.chunk as usize] } else { &c.layers[m.layer as usize] }
    }
}

/// The clip cascade below `-Z` (spec 5.4, 6.2). tippecanoe starts at `iz` and clips
/// each feature at each zoom from `iz` to `-Z`, and it does not write these tiles.
/// With 1 or 2 CPUs, it clips at every zoom; with more CPUs it skips
/// some zooms (the result can then be different by 1 world unit at a buffer edge).
/// This function clips at every zoom.
///
/// `g` is the original geometry of `m` (absolute world coordinates) for the `-Z`
/// tile (x, y). After the call, `g` is the geometry that comes to (x, y) from its
/// parent, in absolute world coordinates. Returns false if the feature does not
/// come to (x, y).
pub fn clip_ancestors(ctx: &Ctx, m: &FeatMeta, x: u32, y: u32, g: &mut DrawVec) -> bool {
    let z0 = ctx.minzoom as u32;
    let iz = ctx.first_zoom as u32;
    if iz >= z0 {
        return true;
    }
    // A bbox in the tile area of the parent of (x, y) is also in the tile areas of
    // all other ancestors: `quick_check` gives 1 (no clip) at each zoom.
    let (px, py) = tile_origin(z0 - 1, x >> 1, y >> 1);
    let area = 1i64 << (32 - (z0 - 1));
    // Not at z = 0 near the date line: there the clip copies the feature (spec 6.8).
    let b = m.bbox;
    let edge = (1i64 << 32) * ctx.buffer / 256;
    let wraps = iz == 0 && (b[0] <= edge || b[2] >= (1i64 << 32) - edge);
    if !wraps && b[0] > px && b[1] > py && b[2] < px + area && b[3] < py + area {
        return true;
    }
    SCRATCH.with_borrow_mut(|s| {
        for z in iz..z0 {
            let (ax, ay) = (x >> (z0 - z), y >> (z0 - z));
            let c = if ctx.geo.plain { clip::<true>(ctx, g, m.geom_type, z, ax, ay, &mut s.geom) } else { clip::<false>(ctx, g, m.geom_type, z, ax, ay, &mut s.geom) };
            if c.is_none() || g.is_empty() {
                return false;
            }
            // `rewrite`: only to a child zoom at or below the feature maxzoom.
            if m.maxzoom >= 0 && (m.maxzoom as u32) < z + 1 {
                return false;
            }
            let (ox, oy) = tile_origin(z, ax, ay);
            for d in g.iter_mut() {
                if d.op == VT_MOVETO || d.op == VT_LINETO {
                    d.x = stored_coord(ctx, d.x + ox);
                    d.y = stored_coord(ctx, d.y + oy);
                }
            }
        }
        true
    })
}

#[inline]
fn stored_coord(ctx: &Ctx, value: i64) -> i64 {
    let shift = ctx.opts.geometry_scale;
    if shift == 0 { value } else { ((value as f64 / (1u64 << shift) as f64).round() as i64) << shift }
}

type Kid = (Vec<TF>, Vec<Draw>);
pub type TileTask = (u8, u32, u32, Vec<TF>, Vec<Draw>);

/// A feature that passed the clip and the zoom checks, with its tile-relative
/// geometry in `rel`.
pub(crate) struct Cand {
    pub(crate) gap: u64,
    pub(crate) pos: u32,
    pub(crate) start: u32,
    pub(crate) len: u32,
}

/// Process tile (z, x, y), then its children. `feats` holds the candidates of this
/// tile (in global sort order); their geometry is in `draws`.
pub fn process(ctx: &Ctx, z: u8, x: u32, y: u32, feats: Vec<TF>, draws: Vec<Draw>) {
    if ctx.geo.plain { process_with::<true>(ctx, z, x, y, feats, draws) } else { process_with::<false>(ctx, z, x, y, feats, draws) }
}

/// [`process`]; `P`: no geometry option (`Geo::plain`), so that the plain
/// path has no tests of the options.
fn process_with<const P: bool>(ctx: &Ctx, z: u8, x: u32, y: u32, feats: Vec<TF>, draws: Vec<Draw>) {
    if ctx.opts.additional[b't' as usize] && ctx.pending.is_some() {
        let threshold = ctx.drop_state.lock().unwrap().0;
        if threshold.gap == 0 && threshold.extent == 0 && threshold.sequence == 0 && threshold.attribute == 0
            && ctx.skip_tiles.lock().unwrap().contains(&(z, x, y)) {
                if z as usize == ctx.maxzoom.load(Ordering::Relaxed) {
                    let key = ((x as u64) << 32) | y as u64;
                    let mut center = ctx.center.lock().unwrap();
                    if center.0 < 1 || (center.0 == 1 && key < center.1) { *center = (1, key); }
                }
                return;
            }
    }
    if feats.is_empty() {
        return;
    }
    let zu = z as u32;
    let nz = z + 1;
    let deeper = (z as usize) < ctx.maxzoom.load(Ordering::Relaxed) || can_extend(ctx, z);
    // Children: index (cx - 2x) + 2 (cy - 2y).
    let mut kids: [Kid; 4] = Default::default();
    let large_queue = ctx.pending.is_some() && draws.len() * std::mem::size_of::<Draw>() > memory_budget();
    let mut rel: Vec<Draw> = if large_queue { Vec::with_capacity(draws.len()) } else { Vec::new() };
    if large_queue {
        // Reserve each child once. Geometric vector growth can otherwise keep
        // almost twice the required geometry in every child at the same time.
        let mut sizes = [(0usize, 0usize); 4];
        let (ox, oy) = tile_origin(zu, x, y);
        for f in &feats {
            let b = ctx.meta(f.pos).1.bbox;
            let bbox = [b[0] - ox, b[1] - oy, b[2] - ox, b[3] - oy];
            let (x0, y0, x1, y1) = child_tile_range(&bbox, zu, nz as u32, x, y, ctx.buffer);
            for cy in y0..=y1 { for cx in x0..=x1 {
                let k = &mut sizes[((cx - 2*x) + 2*(cy - 2*y)) as usize];
                k.0 += 1;
                k.1 += f.len as usize;
            } }
        }
        for (kid, (n, m)) in kids.iter_mut().zip(sizes) {
            kid.0.reserve_exact(n);
            // Clipping can add vertices at the buffer edge.
            kid.1.reserve_exact(m + m / 20 + 64);
        }
    }
    let mut cands: Vec<Cand> = Vec::new();
    // `-pn`: the edge nodes of each candidate (start, len in `nodes`).
    let mut nodes: Vec<Draw> = Vec::new();
    let mut cand_nodes: Vec<(u32, u32)> = Vec::new();
    let mut any = false;
    let mut gap_prev = 0;
    let (ox, oy) = tile_origin(zu, x, y);
    SCRATCH.with_borrow_mut(|s| {
        for f in &feats {
            let (_, m) = ctx.meta(f.pos);
            let mut gap = f.gap;
            if !ctx.gaps.is_empty() && gap == 0 && m.index != gap_prev {
                let (px, py) = if ctx.opts.additional[b'h' as usize] { crate::defaults::decode_hilbert(gap_prev) } else { vtmake_geom::decode_quadkey(gap_prev) };
                for d in &draws[f.start as usize..(f.start + f.len) as usize] {
                    let dx = px as i64 - d.x;
                    let dy = py as i64 - d.y;
                    gap = gap.max(dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy)) as u64);
                }
            }
            gap_prev = m.index;
            s.g.clear();
            s.g.extend_from_slice(&draws[f.start as usize..(f.start + f.len) as usize]);
            let Some(bbox) = clip::<P>(ctx, &mut s.g, m.geom_type, zu, x, y, &mut s.geom) else {
                continue;
            };
            // `wrote_zoom` in tippecanoe: the tile has features from its parent.
            any |= z > ctx.minzoom || ctx.starts_at_minzoom || m.maxzoom < 0 || m.maxzoom as u8 >= z;
            if deeper && !s.g.is_empty() && (m.maxzoom < 0 || m.maxzoom as u8 >= nz) {
                let (x0, y0, x1, y1) = child_tile_range(&bbox, zu, nz as u32, x, y, ctx.buffer);
                for cy in y0..=y1 {
                    for cx in x0..=x1 {
                        let k = &mut kids[((cx - 2 * x) + 2 * (cy - 2 * y)) as usize];
                        let start = k.1.len() as u32;
                        k.1.extend(s.g.iter().map(|d| {
                            if d.op == VT_MOVETO || d.op == VT_LINETO { Draw { x: stored_coord(ctx, d.x + ox), y: stored_coord(ctx, d.y + oy), ..*d } } else { *d }
                        }));
                        k.0.push(TF { gap, pos: f.pos, start, len: s.g.len() as u32 });
                    }
                }
            }
            if z < ctx.minzoom || (m.minzoom >= 0 && z < m.minzoom as u8) || (m.maxzoom >= 0 && z > m.maxzoom as u8) {
                continue;
            }
            let start = rel.len() as u32;
            rel.extend_from_slice(&s.g);
            cands.push(Cand { gap, pos: f.pos, start, len: s.g.len() as u32 });
            if !P && ctx.geo.opts.shared_nodes {
                cand_nodes.push((nodes.len() as u32, s.geom.edge_nodes.len() as u32));
                nodes.extend_from_slice(&s.geom.edge_nodes);
            }
        }
    });
    // Minzoom features are absent from cands until their first visible zoom.
    if ctx.opts.additional[b't' as usize] && feats.iter().any(|f| ctx.meta(f.pos).1.minzoom > z as i8) {
        let mut set = ctx.trial_blocked.lock().unwrap();
        for (i, kid) in kids.iter().enumerate() { if !kid.0.is_empty() { set.insert((nz, 2*x + (i as u32 & 1), 2*y + (i as u32 >> 1))); } }
    }
    drop(feats);
    drop(draws);
    if any {
        ctx.max_tiled.fetch_max(z as usize + 1, Ordering::Relaxed);
    }
    if ctx.opts.additional[b't' as usize] {
        let blocked = cands.iter().any(|c| { let m = ctx.meta(c.pos).1; m.minzoom > z as i8 + 1 || (m.minzoom < 0 && m.drop_minzoom > z as i8) });
        if blocked {
            let mut set = ctx.trial_blocked.lock().unwrap();
            for (i, kid) in kids.iter().enumerate() {
                if !kid.0.is_empty() { set.insert((nz, 2*x + (i as u32 & 1), 2*y + (i as u32 >> 1))); }
            }
        }
    }
    // Release child geometry before the tile encoder makes its working copy.
    let future = kids.iter().flat_map(|k| &k.0).any(|f| ctx.meta(f.pos).1.minzoom > z as i8);
    let child_keys: Vec<_> = kids.iter().enumerate().filter(|_| ctx.pending.is_some() && ctx.opts.additional[b't' as usize]).filter(|(_, k)| !k.0.is_empty()).map(|(i, _)| (nz, 2*x + (i as u32 & 1), 2*y + (i as u32 >> 1))).collect();
    if let Some(pending) = &ctx.pending {
        let mut tasks = pending.lock().unwrap();
        for (i, (cf, cd)) in std::mem::take(&mut kids).into_iter().enumerate() {
            if !cf.is_empty() { tasks.push_task((nz, 2*x + (i as u32 & 1), 2*y + (i as u32 >> 1), cf, cd)); }
        }
    }
    if large_queue { crate::read::collect_free(); }
    // tippecanoe runs the shell filters also for a tile without candidates.
    let prune = (!cands.is_empty() || (z >= ctx.minzoom && crate::plugin::active(ctx.opts))) && write_tile::<P>(ctx, z, x, y, &cands, &rel, (&nodes, &cand_nodes), future);
    drop(rel);
    drop(cands);
    if !deeper || (prune && ctx.pending.is_none()) { return; }
    if prune {
        ctx.skip_tiles.lock().unwrap().extend(child_keys);
    }
    if ctx.pending.is_some() { return; }
    if kids.iter().map(|k| k.0.len()).sum::<usize>() <= 2000 {
        for (i, (cf, cd)) in kids.into_iter().enumerate() {
            if !cf.is_empty() {
                process_with::<P>(ctx, nz, 2 * x + (i as u32 & 1), 2 * y + (i as u32 >> 1), cf, cd);
            }
        }
        return;
    }
    rayon::scope(|scope| {
        for (i, (cf, cd)) in kids.into_iter().enumerate() {
            if cf.is_empty() { continue; }
            let (cx, cy) = (2 * x + (i as u32 & 1), 2 * y + (i as u32 >> 1));
            // Reserve space for three geometry copies and the feature records.
            let cost = cd.len().saturating_mul(24 * 3).saturating_add(cf.len().saturating_mul(64));
            let task = ctx.parallel_tasks.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < ctx.parallel_limit).then_some(n + 1)).is_ok();
            let memory = task && ctx.parallel_bytes.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(cost).filter(|&n| n <= ctx.parallel_budget)).is_ok();
            if memory {
                scope.spawn(move |_| {
                    process_with::<P>(ctx, nz, cx, cy, cf, cd);
                    ctx.parallel_bytes.fetch_sub(cost, Ordering::Relaxed);
                    ctx.parallel_tasks.fetch_sub(1, Ordering::Relaxed);
                });
            } else {
                if task { ctx.parallel_tasks.fetch_sub(1, Ordering::Relaxed); }
                process_with::<P>(ctx, nz, cx, cy, cf, cd);
            }
        }
    });
}

/// `unsimplified_geometry_size > 10 * 1024 * 1024` in `write_tile`, with
/// `sizeof(draw)` = 16: the early simplification (spec 7.5).
const EARLY_DRAWS: usize = 10 * 1024 * 1024 / 16;

/// The last steps for one kept feature with its reduced geometry in `s.g`:
/// simplify, scale, clean, then add it to its layer.
#[allow(clippy::too_many_arguments)]
fn emit<'a, const P: bool>(ctx: &'a Ctx, s: &mut Scratch, pos: u32, t: GeomType, zoom_detail: (u32, u32), reduced: bool, nodes: (&GeomOpts, &NodeCtx), layers: &mut BTreeMap<&'a str, MvtLayer>, vertices: &mut u64, collapsed_lines: &mut usize, ta: Option<&mut TileState<'a, '_>>) {
    let (zu, line_detail) = zoom_detail;
    let (chunk, m) = ctx.meta(pos);
    let lname = ctx.layer_name(pos);
    let d = if P { line_detail } else { out_detail(ctx, zu, line_detail) };
    let tolerance = if line_detail > detail(ctx, zu) { 0.0 } else { simp::<P>(ctx, zu) };
    let pre: PolygonPre = if P {
        tile_simplify(&mut s.g, t, zu, d, tolerance, reduced, &mut s.geom)
    } else {
        tile_simplify_opts(&mut s.g, t, zu, line_detail, d, tolerance, reduced, nodes.0, nodes.1, &mut s.geom)
    };
    if t == GeomType::Line && ctx.opts.additional[b'r' as usize] {
        reverse_line(&mut s.g, ctx.opts.additional[b'h' as usize]);
    }
    let line_before_finish = t == GeomType::Line && !s.g.is_empty();
    match t {
        GeomType::Polygon => {
            if s.cleaner.coalesce(&mut s.g).is_err() {
                eprintln!("Internal error: Polygon cleaning failed. Log in /tmp/wagyu.log");
                std::process::exit(106);
            }
            if !P && ctx.geo.check_polygons {
                check_polygon(&mut s.g);
            }
            tile_finish_polygon(&mut s.g, &pre, zu, d);
        }
        GeomType::Line => tile_finish_line(&mut s.g),
        GeomType::Point => {}
    }
    if s.g.is_empty() {
        if line_before_finish { *collapsed_lines += 1; }
        return;
    }
    let layer = layers.entry(lname).or_insert_with(|| {
        let mut l = MvtLayer::new(lname);
        l.extent = 1 << d;
        l
    });
    let mut mf = MvtFeature { geom_type: t as u8, id: chunk.id(m), ..Default::default() };
    // With `-E` or `attribute-filter`, the tags come at the end of the tile.
    let deferred = ta.filter(|ta| ta.defers_tags()).map(|ta| ta.defer(pos, lname, layer.features.len())).is_some();
    if !deferred {
        for (k, v) in chunk.attrs(m) {
            let v = &chunk.vals[v as usize];
            let key = &chunk.keys[k as usize];
            match v.ty {
                AttrType::String => layer.tag_str(&mut mf, key, chunk.val_text(v)),
                AttrType::Null => {}
                ty => {
                    if let Some(mv) = stringified_to_mvt_value(ty, chunk.val_text(v)) {
                        layer.tag(&mut mf, key, &mv);
                    }
                }
            }
        }
    }
    mf.geometry.extend(s.g.iter().map(|d| MvtPoint { op: d.op, x: d.x, y: d.y }));
    *vertices += mf.geometry.len() as u64;
    layer.features.push(mf);
}

/// Return true only when a full-detail trial stops the child pyramid.
#[allow(clippy::too_many_arguments)]
fn write_tile<const P: bool>(ctx: &Ctx, z: u8, x: u32, y: u32, cands: &[Cand], rel: &[Draw], edge: (&[Draw], &[(u32, u32)]), minzoom_pending: bool) -> bool {
    let initial = if ctx.pending.is_some() { ctx.drop_state.lock().unwrap().0 } else { crate::drop::Threshold { gap: 0, sequence: 0, extent: 0, attribute: 0, gamma: ctx.opts.gamma } };
    let mut retry = crate::drop::Retry::new(initial);
    let normal = detail(ctx, z as u32);
    let trial = ctx.opts.additional[b't' as usize] && !minzoom_pending && z >= ctx.minzoom
        && initial.gap == 0 && initial.extent == 0 && initial.sequence == 0 && initial.attribute == 0
        && 30 - z as u32 > normal && !ctx.trial_blocked.lock().unwrap().contains(&(z, x, y));
    for d in trial.then_some(30 - z as u32).into_iter().chain((ctx.opts.min_detail as u32..=normal).rev()) {
        loop {
            retry.reset();
            // The shell filters and `-u` (crate::plugin, crate::progress).
            if ctx.opts.json_progress { crate::progress::tile(ctx, z, x, y, cands.len(), false); }
            match if ctx.opts.prefilter.is_some() {
                crate::plugin::prefilter_detail::<P>(ctx, z, x, y, cands, rel, d, &mut retry)
            } else if ctx.opts.retain_points_multiplier > 1 {
                write_tile_detail::<P, true>(ctx, z, x, y, cands, rel, edge, d, &mut retry)
            } else {
                write_tile_detail::<P, false>(ctx, z, x, y, cands, rel, edge, d, &mut retry)
            } {
                0 => break,
                1 => {
                    if ctx.pending.is_some() {
                        let mut threshold = retry.threshold;
                        // P_DYNAMIC_DROP keeps its fraction local to this tile.
                        if ctx.opts.prevent[b'd' as usize] { threshold.sequence = initial.sequence; }
                        ctx.drop_state.lock().unwrap().1.merge(threshold);
                    }
                    return false;
                }
                3 => { ctx.strategies[z as usize][7].fetch_add(1, Ordering::Relaxed); return true; },
                _ => { ctx.still_dropping.store(true, Ordering::Relaxed); continue; },
            }
        }
    }
    eprintln!("could not make tile {z}/{x}/{y} small enough");
    ctx.failed.fetch_min(z as usize, Ordering::Relaxed);
    false
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn write_tile_detail<const P: bool, const MULTIPLIER: bool>(ctx: &Ctx, z: u8, x: u32, y: u32, cands: &[Cand], rel: &[Draw], edge: (&[Draw], &[(u32, u32)]), d: u32, retry: &mut crate::drop::Retry) -> u8 {
    let zu = z as u32;
    let trial = d > detail(ctx, zu);
    let mut can_stop = true;
    let (nodes, cand_nodes) = edge;
    // The flags with the maximum zoom of now.
    let gopts = GeomOpts { maxzoom: ctx.maxzoom.load(Ordering::Relaxed) as u32, ..ctx.geo.opts };
    // With `-pn`, the simplification needs the edge nodes of all features of
    // the tile, and `-ab` all features of a layer: all features go to the
    // buffer first.
    let pn = !P && gopts.shared_nodes;
    let ab = !P && gopts.shared_borders;
    // `-pn`: the edge nodes of the kept features (`shared_nodes`).
    let mut tile_nodes: Vec<Draw> = Vec::new();
    let mut layers: BTreeMap<&str, MvtLayer> = BTreeMap::new();
    // Rate drop state for each layer, and the kept geometry types of each layer.
    let mut rate: BTreeMap<&str, (LayerRateDrop, [bool; 4])> = BTreeMap::new();
    let mut extent_previndex = 0u64;
    let mut accum = TinyPolyAccum::default();
    let mut vertices = 0u64;
    // Felt counts lines before the final remove_noop step. A line can still
    // count toward a feature-limit retry when that step removes its geometry.
    let mut collapsed_lines = 0usize;
    let mut tiny = 0u64;
    let mut dropped = 0u64;
    let mut gamma_dropped = 0u64;
    let (mut kept_count, mut skipped_count) = (0usize, 0usize);
    let mut over_limit = false;
    let (mut gamma_prev, mut gamma_gap) = (0u64, 0.0);
    let gamma_scale = 2f64.powi(64 - 2 * (zu as i32 + 8));
    // Only a tile with enough geometry can come to the early simplification.
    // `tile_reduce` can add tiny-polygon squares, but not more than 1/4.
    // Keep multiplier work out of the usual tile path at compile time.
    let multiplier = MULTIPLIER;
    let mut multiplier_state = BTreeMap::<&str, u64>::new();
    let mut multiplier_leads = BTreeMap::<u32, bool>::new();
    let mut multiplier_tags = Vec::new();
    let mut multiplier_previous = 0u64;
    let mut multiplier_density = std::collections::BTreeSet::new();
    let mut multiplier_lead_count = 0usize;
    let mut multiplier_other_count = 0usize;
    let mut multiplier_clusters = BTreeMap::<&str, usize>::new();
    let mut multiplier_drop_rest = false;
    let mut multiplier_sequences = BTreeMap::new();
    let extra_zooms = if multiplier { (ctx.opts.retain_points_multiplier as f64).ln() / ctx.opts.droprate.ln() } else { 0. };
    let clustering = (ctx.opts.cluster_distance != 0 || ctx.opts.additional[b'C' as usize]) && z as i32 <= ctx.opts.cluster_maxzoom;
    let coalescing = b"SND".iter().any(|&k| ctx.opts.additional[k as usize]);
    let buffered = multiplier || ctx.opts.additional[b'o' as usize] || pn || ab || coalescing || clustering || ctx.opts.additional[b'g' as usize] || ctx.opts.prevent[b'i' as usize] || !ctx.opts.order_by.is_empty() || ctx.opts.limit_tile_feature_count > 0 || ctx.opts.limit_tile_feature_count_at_maxzoom > 0 || rel.len() + rel.len() / 4 + 64 > EARLY_DRAWS;
    // The attribute options (filter, -E): crate::attrs.
    let mut ta = TileState::new(ctx, zu);
    let mut generated = Vec::new();
    let mut merged = BTreeMap::<u32, Vec<Draw>>::new();
    let mut coalesced_area = 0u64;
    let mut tile_extents = BTreeMap::new();
    let (mut merge_prev, mut density_prev, mut density_gap, mut spacing) = (0u64, 0u64, 0., 0.);
    let mut cluster_counts = BTreeMap::<u32, u64>::new();
    let mut densities = BTreeMap::<u32, i64>::new();
    SCRATCH.with_borrow_mut(|s| {
        // Buffered mode: the kept features (pos, type, reduced, start, len in `arena`).
        let mut kept: Vec<(u32, GeomType, bool, usize, usize)> = Vec::new();
        let mut arena: Vec<Draw> = Vec::new();
        let mut unsimplified = 0usize;
        let mut through = 0usize;
        for (seq, c) in cands.iter().enumerate() {
            let feature_gap = c.gap;
            if let Some(ta) = ta.as_mut()
                && !ta.admit(c.pos)
            {
                continue;
            }
            let m = ctx.meta(c.pos).1;
            let lname = ctx.layer_name(c.pos);
            let mut t = m.geom_type;
            let extent = if ctx.opts.additional[b'n' as usize] || ctx.opts.additional[b'N' as usize] || ctx.opts.order_by_size {
                if t == GeomType::Point {
                    let value = if extent_previndex >= m.index { 1 } else { (std::f64::consts::PI * (m.index - extent_previndex) as f64 / 16.) as u64 };
                    extent_previndex = m.index;
                    value.max(1)
                } else { ctx.extents[c.pos as usize] }
            } else { 0 };
            if ctx.opts.additional[b'N' as usize] { tile_extents.insert(c.pos, extent); }
            let st = rate.entry(lname).or_default();
            let mut rate_kept = true;
            // `-aP` makes a point of a polygon before the drops, so that they
            // use the point type.
            let label = !P && ctx.geo.label_points && t == GeomType::Polygon;
            if label {
                if m.minzoom < 0 {
                    rate_kept = st.0.mark(zu, m.drop_minzoom as i32, m.index);
                }
                s.g.clear();
                s.g.extend_from_slice(&rel[c.start as usize..(c.start + c.len) as usize]);
                let lp = ctx.label_point(c.pos);
                let nc = NodeCtx { tile: &tile_nodes, global: &[], tx: x, ty: y };
                if !tile_label_point(&mut s.g, zu, x, y, d, label_simplification(ctx, zu), lp, &gopts, &nc, &mut s.geom) {
                    continue;
                }
                t = GeomType::Point;
                if multiplier {
                    // next_feature uses a fractional zoom from the index (felt tile.cpp).
                    let lead = rate_kept;
                    let state = multiplier_state.entry(lname).or_default();
                    if rate_kept { *state = 0; }
                    else if zu as f64 + extra_zooms >= m.drop_minzoom as f64 - ((m.index >> 2).reverse_bits() as f64 / 2f64.powi(64)) && *state + 1 < ctx.opts.retain_points_multiplier as u64 { *state += 1; rate_kept = true; }
                    let threshold = ctx.opts.preserve_multiplier_density_threshold;
                    if !rate_kept && threshold > 0 {
                        let distance = (1u64 << (32 - zu)) / threshold as u64;
                        if m.index.wrapping_sub(multiplier_previous) > distance.wrapping_mul(distance) { rate_kept = true; multiplier_density.insert(c.pos); }
                    }
                    if rate_kept { multiplier_previous = m.index; }
                    multiplier_leads.insert(c.pos, lead);
                }
                if !rate_kept && st.1[t as usize] {
                    if let Some(ta) = ta.as_mut() {
                        ta.drop_onto(c.pos);
                    }
                    dropped += 1;
                    continue;
                }
            } else if m.minzoom < 0 {
                let index = ctx.meta(c.pos).1.index;
                // `find_feature_to_accumulate_onto`: drop only onto an earlier
                // feature of the same type that is in the tile.
                rate_kept = st.0.mark(zu, m.drop_minzoom as i32, index);
                if multiplier {
                    // next_feature uses a fractional zoom from the index (felt tile.cpp).
                    let lead = rate_kept;
                    let state = multiplier_state.entry(lname).or_default();
                    if rate_kept { *state = 0; }
                    else if zu as f64 + extra_zooms >= m.drop_minzoom as f64 - ((m.index >> 2).reverse_bits() as f64 / 2f64.powi(64)) && *state + 1 < ctx.opts.retain_points_multiplier as u64 { *state += 1; rate_kept = true; }
                    let threshold = ctx.opts.preserve_multiplier_density_threshold;
                    if !rate_kept && threshold > 0 {
                        let distance = (1u64 << (32 - zu)) / threshold as u64;
                        if m.index.wrapping_sub(multiplier_previous) > distance.wrapping_mul(distance) { rate_kept = true; multiplier_density.insert(c.pos); }
                    }
                    if rate_kept { multiplier_previous = m.index; }
                    multiplier_leads.insert(c.pos, lead);
                }
                if !rate_kept && st.1[t as usize] {
                    if let Some(ta) = ta.as_mut() {
                        ta.drop_onto(c.pos);
                    }
                    dropped += 1;
                    continue;
                }
            }

            let mut lead = !multiplier || multiplier_leads.get(&c.pos).copied().unwrap_or(true);
            if multiplier {
                let cluster = multiplier_clusters.entry(lname).or_default();
                if lead { multiplier_drop_rest = false; }
                else if (multiplier_drop_rest || (!multiplier_density.contains(&c.pos) && *cluster >= ctx.opts.retain_points_multiplier as usize)) && st.1[t as usize] {
                    if let Some(ta) = ta.as_mut() { ta.drop_onto(c.pos); }
                    dropped += 1;
                    continue;
                }
            }
            if lead && rate_kept && crate::defaults::manage_gap(m.index, &mut gamma_prev, gamma_scale, retry.threshold.gamma, &mut gamma_gap) && st.1[t as usize] {
                if let Some(ta) = ta.as_mut() { ta.drop_onto(c.pos); }
                gamma_dropped += 1;
                continue;
            }
            if rate_kept && clustering && ctx.opts.cluster_distance != 0 {
                let distance = ((1u64 << (32 - zu)) / 256) * ctx.opts.cluster_distance as u64;
                if (m.index < merge_prev || m.index - merge_prev < distance * distance)
                    && merge_cluster(ctx, c, rel, &kept, &mut arena, &mut cluster_counts, ctx.opts.additional[b'a' as usize]) {
                    if let Some(ta) = ta.as_mut() { ta.drop_onto(c.pos); }
                    retry.dropped += 1;
                    continue;
                }
            }
            if rate_kept && clustering && ctx.opts.cluster_distance == 0 {
                let gap = feature_gap;
                retry.sample(gap, seq);
                if gap < retry.threshold.gap && merge_cluster(ctx, c, rel, &kept, &mut arena, &mut cluster_counts, false) {
                    if let Some(ta) = ta.as_mut() { ta.drop_onto(c.pos); }
                    retry.dropped += 1;
                    continue;
                }
            }
            if lead && rate_kept && ctx.pending.is_some() {
                let densest = ctx.opts.additional[b's' as usize] || ctx.opts.additional[b'S' as usize];
                let smallest = ctx.opts.additional[b'n' as usize] || ctx.opts.additional[b'N' as usize];
                let attribute = ctx.opts.additional[b'A' as usize] && !densest && !smallest && !(ctx.opts.additional[b'd' as usize] || ctx.opts.additional[b'D' as usize] || ctx.opts.prevent[b'd' as usize]);
                let fraction = ctx.opts.additional[b'd' as usize] || ctx.opts.additional[b'D' as usize] || ctx.opts.prevent[b'd' as usize];
                let value = if densest { feature_gap } else if smallest { extent } else { crate::drop::sequence(m.index, m.drop_minzoom) };
                let attr_value = if attribute { ctx.opts.drop_by_attribute.as_ref().and_then(|name| {
                    let (chunk, meta) = ctx.meta(c.pos);
                    chunk.attrs(meta).find_map(|(k, v)| {
                        if chunk.keys[k as usize].as_ref() != name { return None; }
                        let v = &chunk.vals[v as usize];
                        if matches!(v.ty, AttrType::String | AttrType::Bool | AttrType::Null) { return None; }
                        let n = vtmake_parse::pull::atof(chunk.val_text(v).as_bytes());
                        n.is_finite().then(|| crate::drop::attribute_key(if ctx.opts.drop_by_attribute_descending { -n } else { n }))
                    })
                }) } else { None };
                let value = attr_value.unwrap_or(value);
                if densest || fraction || smallest || attr_value.is_some() {
                    retry.sample(value, seq);
                    if (if densest { value < retry.threshold.gap } else if smallest { retry.threshold.extent != 0 && value.saturating_add(coalesced_area) <= retry.threshold.extent } else if attribute { value < retry.threshold.attribute } else { retry.threshold.sequence != 0 && value <= retry.threshold.sequence }) && st.1[t as usize] {
                        if coalescing {
                            let target = kept.iter().rev().find(|k| ctx.layer_name(k.0) == lname && ctx.meta(k.0).1.geom_type == t && (!ctx.opts.additional[b'N' as usize] || tile_extents[&k.0] <= retry.threshold.extent));
                            if let Some(k) = target {
                                let geometry = &rel[c.start as usize..(c.start + c.len) as usize];
                                if t == GeomType::Point || !vtmake_geom::line_is_too_small(geometry, zu, d) {
                                    merged.entry(k.0).or_default().extend_from_slice(geometry);
                                }
                                coalesced_area = coalesced_area.saturating_add(extent);
                                if let Some(ta) = ta.as_mut() { ta.drop_onto(c.pos); }
                                retry.dropped += 1;
                                continue;
                            }
                        } else {
                            retry.dropped += 1;
                            if multiplier && *multiplier_clusters.entry(lname).or_default() < ctx.opts.retain_points_multiplier as usize {
                                multiplier_leads.insert(c.pos, false);
                                lead = false;
                            } else {
                                if multiplier { multiplier_drop_rest = true; }
                                if let Some(ta) = ta.as_mut() { ta.drop_onto(c.pos); }
                                continue;
                            }
                        }
                    }
                }
            }
            if ctx.opts.additional[b'g' as usize] {
                let old = density_prev as f64;
                if !crate::defaults::manage_gap(m.index, &mut density_prev, gamma_scale, 1., &mut density_gap) {
                    spacing = (m.index as f64 - old) / gamma_scale;
                }
                densities.insert(c.pos, if spacing > 0. { if 1. / spacing >= 2147483648. { i32::MIN as i64 } else { (1. / spacing) as i64 }.min(255) } else { 255 });
            }
            if !label {
                s.g.clear();
                s.g.extend_from_slice(&rel[c.start as usize..(c.start + c.len) as usize]);
            }
            let r = if P {
                tile_reduce(&mut s.g, t, zu, d, TINY_POLYGON_SIZE, &mut accum, &mut s.geom)
            } else {
                tile_reduce_opts(&mut s.g, t, zu, d, tiny_size::<P>(ctx), &gopts, &mut accum, &mut s.geom)
            };
            if r.tiny_polygon {
                tiny += 1;
            }
            if !r.keep || s.g.is_empty() {
                continue;
            }
            if ctx.pending.is_some() {
                if over_limit || (!ctx.opts.prevent[b'f' as usize] && kept_count > adjusted_limit(ctx.opts.max_tile_features as usize, multiplier_lead_count, multiplier_other_count)) || (!ctx.opts.prevent[b'k' as usize] && kept_count > adjusted_limit(ctx.opts.max_tile_size as usize, multiplier_lead_count, multiplier_other_count)) {
                    over_limit = true;
                    skipped_count += 1;
                    continue;
                }
                kept_count += 1;
            }
            // The feature is in the tile now (`features.push_back`), also if the
            // later steps remove all of its geometry.
            if multiplier {
                let cluster = multiplier_clusters.entry(lname).or_default();
                if !st.1[t as usize] { multiplier_leads.insert(c.pos, true); lead = true; }
                if lead { *cluster = 1; multiplier_lead_count += 1; }
                else { if !multiplier_density.contains(&c.pos) { *cluster += 1; } multiplier_other_count += 1; }
            }
            coalesced_area = 0;
            merge_prev = m.index;
            st.1[t as usize] = true;
            if let Some(ta) = ta.as_mut() {
                ta.keep(c.pos);
            }
            if pn {
                let (a, n) = cand_nodes[seq];
                tile_nodes.extend_from_slice(&nodes[a as usize..(a + n) as usize]);
            }
            if !buffered {
                emit::<P>(ctx, s, c.pos, t, (zu, d), r.reduced, (&gopts, &NodeCtx::EMPTY), &mut layers, &mut vertices, &mut collapsed_lines, ta.as_mut());
                continue;
            }
            // Spec 7.5: more than 10 MB of kept geometry in the tile. Simplify the
            // features so far, and clean the polygons in world coordinates. They
            // go through the normal steps later again.
            unsimplified += s.g.len();
            kept.push((c.pos, t, r.reduced, arena.len(), s.g.len()));
            arena.extend_from_slice(&s.g);
            if unsimplified > EARLY_DRAWS && !ab {
                for k in &mut kept[through..] {
                    let t = k.1;
                    s.g.clear();
                    s.g.extend_from_slice(&arena[k.3..k.3 + k.4]);
                    if P {
                        tile_simplify_early(&mut s.g, t, zu, d, SIMPLIFICATION, k.2, &mut s.geom);
                    } else {
                        let nc = NodeCtx { tile: &tile_nodes, global: &ctx.geo.nodes, tx: x, ty: y };
                        tile_simplify_early_opts(&mut s.g, t, zu, d, simp::<P>(ctx, zu), k.2, &gopts, &nc, &mut s.geom);
                    }
                    if t == GeomType::Polygon && s.cleaner.coalesce_unscaled(&mut s.g).is_err() {
                        eprintln!("Internal error: Polygon cleaning failed. Log in /tmp/wagyu.log");
                        std::process::exit(106);
                    }
                    (k.3, k.4) = (arena.len(), s.g.len());
                    arena.extend_from_slice(&s.g);
                }
                through = kept.len();
                unsimplified = 0;
            }
        }
        if multiplier {
            // Use the input rank within this tile, independent of reader threads.
            let mut by_layer: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
            for k in &kept { by_layer.entry(ctx.layer_name(k.0)).or_default().push(k.0); }
            for ids in by_layer.values_mut() {
                ids.sort_by_key(|&pos| { let id = ctx.order[pos as usize]; ctx.chunks[id.chunk as usize].seqs[id.local as usize] });
                for (seq, &pos) in ids.iter().enumerate() { multiplier_sequences.insert(pos, seq); }
            }
            if ctx.opts.prevent[b'i' as usize] || !ctx.opts.order_by.is_empty() {
                type KeptFeature = (u32, GeomType, bool, usize, usize);
                let mut layer_features: BTreeMap<&str, Vec<KeptFeature>> = BTreeMap::new();
                for k in std::mem::take(&mut kept) { layer_features.entry(ctx.layer_name(k.0)).or_default().push(k); }
                for features in layer_features.into_values() {
                    let mut groups: Vec<Vec<KeptFeature>> = Vec::new();
                    for k in features {
                        if groups.is_empty() || multiplier_leads.get(&k.0).copied().unwrap_or(true) { groups.push(Vec::new()); }
                        groups.last_mut().unwrap().push(k);
                    }
                    if ctx.opts.prevent[b'i' as usize] { groups.sort_by_key(|g| multiplier_sequences[&g[0].0]); }
                    if !ctx.opts.order_by.is_empty() { groups.sort_by(|a,b| feature_cmp(ctx, a[0].0, b[0].0)); }
                    // A sorted group moves its marker to the end of the lead tags.
                    for group in &mut groups { group[1..].sort_by_key(|k| std::cmp::Reverse(crate::drop::sequence(ctx.meta(k.0).1.index, ctx.meta(k.0).1.drop_minzoom))); }
                    kept.extend(groups.into_iter().flatten());
                }
            }
        }
        if !multiplier && ctx.opts.prevent[b'i' as usize] {
            kept.sort_by_key(|k| { let s = ctx.order[k.0 as usize]; ctx.chunks[s.chunk as usize].seqs[s.local as usize] });
        }
        if !multiplier && !ctx.opts.order_by.is_empty() {
            kept.sort_by(|a, b| feature_cmp(ctx, a.0, b.0));
        }
        if ab {
            // `find_common_edges` for the polygons of each layer.
            let mut by_layer: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
            for (i, k) in kept.iter().enumerate() {
                if k.1 == GeomType::Polygon {
                    by_layer.entry(ctx.layer_name(k.0)).or_default().push(i);
                }
            }
            for ids in by_layer.values() {
                let mut polys: Vec<DrawVec> = ids.iter().map(|&i| arena[kept[i].3..kept[i].3 + kept[i].4].to_vec()).collect();
                find_common_edges(&mut polys, zu, d, simplification(ctx, zu), &gopts, &mut s.simplify);
                for (&i, g) in ids.iter().zip(&polys) {
                    (kept[i].3, kept[i].4) = (arena.len(), g.len());
                    arena.extend_from_slice(g);
                }
            }
        }
        // `std::stable_sort(shared_nodes)` (`draw::operator<`: y, then x).
        if pn {
            tile_nodes.sort_by_key(|d| (d.y, d.x));
        }
        let nc = NodeCtx { tile: &tile_nodes, global: &ctx.geo.nodes, tx: x, ty: y };
        let limit = if z as usize == ctx.maxzoom.load(Ordering::Relaxed) && ctx.opts.limit_tile_feature_count_at_maxzoom > 0 { ctx.opts.limit_tile_feature_count_at_maxzoom } else { ctx.opts.limit_tile_feature_count };
        let mut counts = BTreeMap::new();
        if ctx.opts.additional[b'o' as usize] {
            kept.sort_by(|a, b| coal_cmp(ctx, a.0, b.0).then_with(|| ctx.meta(a.0).1.index.cmp(&ctx.meta(b.0).1.index)).then_with(|| {
                arena[a.3..a.3 + a.4].iter().map(|d| (d.y, d.x)).cmp(arena[b.3..b.3 + b.4].iter().map(|d| (d.y, d.x)))
            }));
        }
        for &(pos, t, reduced, start, len) in &kept {
            if limit > 0 {
                let n = counts.entry(ctx.layer_name(pos)).or_insert(0usize);
                if *n >= limit as usize { can_stop = false; skipped_count = 0; continue; }
                *n += 1;
            }
            s.g.clear();
            s.g.extend_from_slice(&arena[start..start + len]);
            if let Some(extra) = merged.get(&pos) {
                s.g.extend_from_slice(extra);
                if !reduced && t == GeomType::Polygon && !gopts.no_simplify && !(zu == gopts.maxzoom && gopts.no_simplify_maxzoom) && !(ab || (zu < gopts.maxzoom && gopts.grid_low_zooms)) && s.cleaner.coalesce_unscaled(&mut s.g).is_err() {
                    eprintln!("Polygon cleaning failed");
                    std::process::exit(1);
                }
            }
            let lname = ctx.layer_name(pos);
            let before = layers.get(lname).map_or(0, |l| l.features.len());
            emit::<P>(ctx, s, pos, t, (zu, d), reduced, (&gopts, &nc), &mut layers, &mut vertices, &mut collapsed_lines, ta.as_mut());
            if layers.get(lname).is_some_and(|l| l.features.len() > before) {
                if multiplier { multiplier_tags.push((lname, before, pos)); }
                let clustered = cluster_counts.get(&pos).copied().unwrap_or(0);
                let density = densities.get(&pos).copied();
                if clustered > 0 || density.is_some() {
                    if ta.as_ref().is_some_and(|ta| ta.defers_tags()) { generated.push((lname, before, clustered, density)); }
                    else { crate::attrs::tag_generated(layers.get_mut(lname).unwrap(), before, clustered, density); }
                }
            }
        }
    });
    let promoted = if multiplier { ta.as_ref().map(TileState::promoted_keys).unwrap_or_default() } else { BTreeMap::new() };
    if let Some(ta) = ta {
        ta.finish(&mut layers, &generated);
    }
    for (lname, index, pos) in multiplier_tags {
        let layer = layers.get_mut(lname).unwrap();
        let mut feature = std::mem::take(&mut layer.features[index]);
        if multiplier_leads.get(&pos).copied().unwrap_or(true) {
            let key = "tippecanoe:retain_points_multiplier_first";
            layer.tag(&mut feature, key, &vtmake_mvt::MvtValue::Bool(true));
            crate::attrs::record_generated(lname, key, AttrType::Bool, "true");
        }
        let key = "tippecanoe:retain_points_multiplier_sequence";
        let seq = multiplier_sequences[&pos];
        layer.tag(&mut feature, key, &vtmake_mvt::MvtValue::Int(seq as i64));
        crate::attrs::record_generated(lname, key, AttrType::Number, &seq.to_string());
        if multiplier_leads.get(&pos).copied().unwrap_or(true) {
            let marker = feature.tags.len() - 4;
            let pair = [feature.tags[marker], feature.tags[marker + 1]];
            if ctx.opts.prevent[b'i' as usize] || !ctx.opts.order_by.is_empty() {
                feature.tags.drain(marker..marker + 2);
                feature.tags.extend(pair);
            } else if !ctx.opts.attribute_accum.is_empty() {
                let insertion = feature.tags[..marker].as_chunks::<2>().0.iter().position(|tag| promoted.get(&pos).is_some_and(|keys| keys.contains(&layer.keys[tag[0] as usize]))).map(|i| i * 2);
                if let Some(insertion) = insertion {
                    feature.tags.drain(marker..marker + 2);
                    feature.tags.splice(insertion..insertion, pair);
                }
            }
        }
        layer.features[index] = feature;
    }
    if ctx.opts.additional[b'c' as usize] {
        for layer in layers.values_mut() { coalesce_layer(ctx, layer, zu); }
        layers.retain(|_, layer| !layer.features.is_empty());
        vertices = layers.values().flat_map(|l| &l.features).map(|f| f.geometry.len() as u64).sum();
    }
    if multiplier || ctx.opts.additional[b'c' as usize] {
        // Felt builds its key and value tables only after final feature order.
        // Rebuild them so generated tags and removed features have the same bytes.
        for layer in layers.values_mut() {
            let mut output = MvtLayer::new(&layer.name);
            output.extent = layer.extent;
            for mut feature in std::mem::take(&mut layer.features) {
                let tags = std::mem::take(&mut feature.tags);
                for &[key, value] in tags.as_chunks::<2>().0 {
                    output.tag(&mut feature, &layer.keys[key as usize], &layer.values[value as usize]);
                }
                output.features.push(feature);
            }
            *layer = output;
        }
    }
    // The postfilter (the feature limit uses the counts before it) and `-u`.
    let filtered = if ctx.opts.postfilter.is_some() || ctx.opts.json_progress { crate::plugin::after_layers(ctx, z, x, y, &mut layers, 1 << if P { d } else { out_detail(ctx, zu, d) }, cands.len()) } else { None };
    if trial && (!can_stop || dropped > 0 || gamma_dropped > 0 || retry.dropped > 0) { return 0; }
    if layers.is_empty() || filtered.is_some_and(|f| f.0 == 0) {
        if filtered.is_some() { crate::plugin::center(ctx, z, x, y, vertices); }
        if retry.dropped > 0 { ctx.strategies[z as usize][6].fetch_add(retry.dropped, Ordering::Relaxed); }
        record_strategy(ctx, z, d, dropped, tiny, gamma_dropped);
        return 1;
    }
    let (mut count, nlayers) = filtered.unwrap_or_else(|| (layers.values().map(|l| l.features.len()).sum::<usize>() + collapsed_lines, layers.len()));
    let multiple = count > nlayers;
    count = (count * (kept_count + skipped_count)).checked_div(kept_count).unwrap_or(count);
    if !ctx.opts.prevent[b'f' as usize] && count > adjusted_limit(ctx.opts.max_tile_features as usize, multiplier_lead_count, multiplier_other_count) {
        ctx.strategies[z as usize][5].fetch_max(count as u64, Ordering::Relaxed);
        if !ctx.opts.quiet { eprintln!("tile {z}/{x}/{y} has {count} (estimated {count}) features, >{}    ", ctx.opts.max_tile_features); }
        if trial { return 0; }
        if retry.tighten(ctx.opts, adjusted_limit(ctx.opts.max_tile_features as usize, multiplier_lead_count, multiplier_other_count) as f64 / count as f64, true, multiple) { return 2; }
        if retry.failed { ctx.incomplete.store(true, Ordering::Relaxed); return 1; }
        eprintln!("Try using --drop-fraction-as-needed or --drop-densest-as-needed.");
        ctx.failed.fetch_min(z as usize, Ordering::Relaxed);
        return 1;
    }
    let tile = MvtTile { layers: layers.into_values().collect() };
    let mut pbf = Vec::with_capacity(4096);
    tile.encode(&mut pbf);
    let mut gz = Vec::with_capacity(pbf.len() / 2 + 64);
    ctx.gzip.compress(&pbf, &mut gz);
    let size = (gz.len() * (kept_count + skipped_count)).checked_div(kept_count).unwrap_or(gz.len());
    if !ctx.opts.prevent[b'k' as usize] && size > adjusted_limit(ctx.opts.max_tile_size as usize, multiplier_lead_count, multiplier_other_count) {
        ctx.strategies[z as usize][4].fetch_max(size as u64, Ordering::Relaxed);
        if !ctx.opts.quiet {
            let estimate = if size != gz.len() { format!(" (probably really {size})") } else { String::new() };
            eprintln!("tile {z}/{x}/{y} size is {}{estimate} with detail {d}, >{}    ", gz.len(), ctx.opts.max_tile_size);
        }
        if trial { return 0; }
        if retry.tighten(ctx.opts, adjusted_limit(ctx.opts.max_tile_size as usize, multiplier_lead_count, multiplier_other_count) as f64 / size as f64, false, multiple) { return 2; }
        if retry.failed { ctx.incomplete.store(true, Ordering::Relaxed); return 1; }
        return 0;
    }
    record_strategy(ctx, z, d, dropped, tiny, gamma_dropped);
    ctx.strategies[z as usize][6].fetch_add(retry.dropped, Ordering::Relaxed);
    if z as usize == ctx.maxzoom.load(Ordering::Relaxed) {
        let key = ((x as u64) << 32) | y as u64;
        let mut c = ctx.center.lock().unwrap();
        if vertices > c.0 || (vertices == c.0 && key < c.1) {
            *c = (vertices, key);
        }
    }
    ctx.tiles.fetch_add(1, Ordering::Relaxed);
    if ctx.pending.is_some() { ctx.staged.lock().unwrap().push_tile((z, x, y, gz)); } else { ctx.out.send((z, x, y, gz)).expect("writer stopped"); }
    if trial { 3 } else { 1 }
}

// Felt's write_tile scales the limit by the multiplier group ratio.
// Keep the zero-lead branch before multiplication, as in the reference.
#[allow(clippy::manual_checked_ops)]
fn adjusted_limit(limit: usize, leads: usize, others: usize) -> usize {
    if leads == 0 { limit } else { limit * (leads + others) / leads }
}

fn record_strategy(ctx: &Ctx, z: u8, d: u32, dropped: u64, tiny: u64, gamma: u64) {
    for (slot, value) in [(0, dropped), (1, tiny), (2, detail(ctx, z as u32).saturating_sub(d) as u64), (3, gamma)] {
        if value > 0 { ctx.strategies[z as usize][slot].fetch_add(value, Ordering::Relaxed); }
    }
}

/// The effective cgroup memory limit, capped at 8 GiB.
pub fn memory_limit() -> u64 {
    let mut limit = 8 * 1024 * 1024 * 1024u64;
    if let Ok(cgroup) = std::fs::read_to_string("/proc/self/cgroup")
        && let Some(path) = cgroup.lines().find_map(|l| l.strip_prefix("0::")) {
            let mut dir = std::path::PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/'));
            loop {
                if let Ok(s) = std::fs::read_to_string(dir.join("memory.max"))
                    && let Ok(n) = s.trim().parse::<u64>() { limit = limit.min(n); }
                if !dir.pop() || dir == std::path::Path::new("/sys/fs") { break; }
            }
    }
    limit
}

/// Reserve most memory for the feature store and the output cache. This is a
/// limit on concurrent group estimates; one large group can exceed the limit.
pub fn group_memory_budget() -> usize {
    let limit = memory_limit();
    let budget = if limit > 4 * 1024 * 1024 * 1024 { limit / 2 } else { limit / 8 };
    budget.clamp(32 * 1024 * 1024, 4 * 1024 * 1024 * 1024) as usize
}

pub fn memory_budget() -> usize {
    (memory_limit().min(4 * 1024 * 1024 * 1024) / 8).clamp(32 * 1024 * 1024, 512 * 1024 * 1024) as usize
}

/// Density is measured before clipping, and remains the same in child tiles.
/// Port of felt's next_feature (BSD 2-Clause; see LICENSE-tippecanoe.md).
pub fn feature_gaps(o: &crate::cli::Options, chunks: &[Chunk], order: &[Sorted]) -> Vec<u64> {
    if !(o.additional[b's' as usize] || o.additional[b'C' as usize] || o.additional[b'S' as usize]) || (o.prevent[b'f' as usize] && o.prevent[b'k' as usize]) { return Vec::new(); }
    let mut prev = 0;
    let mut g = Vec::new();
    order.iter().map(|s| {
        let c = &chunks[s.chunk as usize];
        let m = &c.metas[s.local as usize];
        let (px, py) = if o.additional[b'h' as usize] { crate::defaults::decode_hilbert(prev) } else { vtmake_geom::decode_quadkey(prev) };
        let mut gap = 0;
        if prev != m.index {
            g.clear();
            c.decode_geom(m, &mut g);
            for d in &g {
                if d.op == VT_MOVETO || d.op == VT_LINETO {
                    let dx = px as i64 - d.x;
                    let dy = py as i64 - d.y;
                    gap = gap.max(dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy)) as u64);
                }
            }
        }
        prev = m.index;
        gap
    }).collect()
}

/// Repeat a zoom until every tile uses the same drop thresholds.
pub fn process_zoom(ctx: &Ctx, tasks: &mut Spool) {
    use rayon::prelude::*;
    let initial = crate::drop::Threshold { gap: 0, sequence: 0, extent: 0, attribute: 0, gamma: ctx.opts.gamma };
    *ctx.drop_state.lock().unwrap() = (initial, initial);
    let mut z = 0;
    loop {
        ctx.still_dropping.store(false, Ordering::Relaxed);
        let before = ctx.drop_state.lock().unwrap().0;
        let tile_count = ctx.tiles.load(Ordering::Relaxed);
        let mut reader = tasks.reader();
        let mut remaining = tasks.count;
        while remaining > 0 {
            let mut batch = Vec::new();
            let mut bytes = 0;
            while remaining > 0 && (batch.is_empty() || bytes < memory_budget()) {
                let task = Spool::read_task(&mut reader);
                z = task.0;
                bytes += task.3.len() * 64 + task.4.len() * 72;
                batch.push(task);
                remaining -= 1;
            }
            batch.into_par_iter().for_each(|(z, x, y, feats, draws)| {
                if !ctx.incomplete.load(Ordering::Relaxed) { process(ctx, z, x, y, feats, draws); }
            });
            crate::read::collect_free();
            if ctx.incomplete.load(Ordering::Relaxed) { break; }
        }
        let after = ctx.drop_state.lock().unwrap().1;
        if ctx.still_dropping.load(Ordering::Relaxed) && can_extend(ctx, z) {
            ctx.maxzoom.store(z as usize + 1, Ordering::Relaxed);
            ctx.center.lock().unwrap().0 = 0;
        }
        if before == after || ctx.failed.load(Ordering::Relaxed) != usize::MAX || ctx.incomplete.load(Ordering::Relaxed) { break; }
        ctx.staged.lock().unwrap().clear();
        ctx.pending.as_ref().unwrap().lock().unwrap().clear();
        ctx.tiles.store(tile_count, Ordering::Relaxed);
        if ctx.opts.additional[b't' as usize] {
            ctx.skip_tiles.lock().unwrap().retain(|&(zoom, _, _)| zoom != z + 1);
        }
        for i in [0, 1, 2, 3, 6, 7] { ctx.strategies[z as usize][i].store(0, Ordering::Relaxed); }
        ctx.drop_state.lock().unwrap().0 = after;
    }
    if z as usize == ctx.maxzoom.load(Ordering::Relaxed) { ctx.pending.as_ref().unwrap().lock().unwrap().clear(); }
    let mut staged = ctx.staged.lock().unwrap();
    let mut reader = staged.reader();
    for _ in 0..staged.count { ctx.out.send(Spool::read_tile(&mut reader)).expect("writer stopped"); }
    staged.clear();
}

/// Original feature area from serial.cpp, before tile clipping.
pub fn feature_extents(o: &crate::cli::Options, chunks: &[Chunk], order: &[Sorted]) -> Vec<u64> {
    if !o.additional[b'n' as usize] && !o.additional[b'N' as usize] && !o.order_by_size { return Vec::new(); }
    let mut g = Vec::new();
    order.iter().map(|s| {
        let c = &chunks[s.chunk as usize];
        let m = &c.metas[s.local as usize];
        g.clear();
        c.decode_geom(m, &mut g);
        let mut extent = 0.;
        if m.geom_type == GeomType::Polygon {
            let mut i = 0;
            while i < g.len() {
                if g[i].op == VT_MOVETO {
                    let j = vtmake_geom::polygon::part_end(&g, i);
                    extent += vtmake_geom::polygon::get_area(&g, i, j);
                    i = j;
                } else { i += 1; }
            }
        } else if m.geom_type == GeomType::Line {
            let mut dist = 0.;
            for w in g.windows(2) {
                if w[1].op == VT_LINETO {
                    let dx = (w[1].x - w[0].x) as f64;
                    let dy = (w[1].y - w[0].y) as f64;
                    dist += (dx * dx + dy * dy).sqrt();
                }
            }
            extent = std::f64::consts::PI * (dist / 2.) * (dist / 2.);
        }
        (extent as i64).max(0) as u64
    }).collect()
}

fn can_extend(ctx: &Ctx, z: u8) -> bool {
    ctx.pending.is_some() && z < 24 && z as usize == ctx.maxzoom.load(Ordering::Relaxed)
        && (ctx.opts.additional[b'e' as usize] || ctx.opts.extend_zooms_max > 0)
        && (ctx.opts.extend_zooms_max <= 0 || (z as i32) < ctx.opts.maxzoom + ctx.opts.extend_zooms_max as i32)
}

fn feature_cmp(ctx: &Ctx, a: u32, b: u32) -> std::cmp::Ordering {
    use vtmake_mvt::MvtValue;
    fn value(ctx: &Ctx, pos: u32, name: &str) -> (u8, String, f64) {
        let (c, m) = ctx.meta(pos);
        c.attrs(m).find_map(|(k, v)| {
            if c.keys[k as usize].as_ref() != name { return None; }
            let v = &c.vals[v as usize];
            let v = stringified_to_mvt_value(v.ty, c.val_text(v))?;
            Some(match v {
                MvtValue::String(s) => (1, s.into(), 0.),
                MvtValue::Bool(b) => (7, String::new(), u8::from(b) as f64),
                MvtValue::Float(f) => (3, String::new(), f as f64),
                MvtValue::Double(f) => (3, String::new(), f),
                MvtValue::Int(n) | MvtValue::Sint(n) => (3, String::new(), n as f64),
                MvtValue::Uint(n) => (3, String::new(), n as f64),
            })
        }).unwrap_or((8, String::new(), 0.))
    }
    for (key, descending) in &ctx.opts.order_by {
        if key == "\u{80}size" {
            let cmp = ctx.extents[a as usize].cmp(&ctx.extents[b as usize]);
            if !cmp.is_eq() { return if *descending { cmp.reverse() } else { cmp }; }
            continue;
        }
        let va = value(ctx, a, key);
        let vb = value(ctx, b, key);
        let cmp = va.0.cmp(&vb.0).then_with(|| va.1.cmp(&vb.1)).then_with(|| va.2.partial_cmp(&vb.2).unwrap_or(std::cmp::Ordering::Equal));
        if !cmp.is_eq() { return if *descending { cmp.reverse() } else { cmp }; }
    }
    ctx.meta(a).1.index.cmp(&ctx.meta(b).1.index)
}

/// Fixed and dynamic clustering from felt tile.cpp (BSD 2-Clause).
fn merge_cluster(ctx: &Ctx, c: &Cand, rel: &[Draw], kept: &[(u32, GeomType, bool, usize, usize)], arena: &mut [Draw], counts: &mut BTreeMap<u32, u64>, keep_position: bool) -> bool {
    let m = ctx.meta(c.pos).1;
    let Some(&(pos, _, _, start, len)) = kept.iter().rev().find(|k| ctx.layer_name(k.0) == ctx.layer_name(c.pos) && ctx.meta(k.0).1.geom_type == m.geom_type) else { return false; };
    let count = counts.entry(pos).or_default();
    *count += 1;
    if !keep_position && m.geom_type == GeomType::Point && len == 1 && c.len == 1 {
        let source = rel[c.start as usize];
        arena[start].x = ((arena[start].x as f64 * *count as f64 + source.x as f64) / (*count + 1) as f64) as i64;
        arena[start].y = ((arena[start].y as f64 * *count as f64 + source.y as f64) / (*count + 1) as f64) as i64;
    }
    true
}

/// `geometry.cpp:reorder_lines` (tippecanoe, BSD 2-Clause).
fn reverse_line(g: &mut DrawVec, hilbert: bool) {
    vtmake_geom::remove_noop(g, GeomType::Line, 0);
    if g.is_empty() || g[0].op != VT_MOVETO || g[1..].iter().any(|d| d.op != VT_LINETO) { return; }
    let index = |d: &Draw| if hilbert { crate::defaults::encode_hilbert(d.x as u32, d.y as u32) } else { vtmake_geom::encode_quadkey(d.x as u32, d.y as u32) };
    if index(&g[0]) > index(g.last().unwrap()) {
        g.reverse();
        g[0].op = VT_MOVETO;
        if g.len() > 1 { g.last_mut().unwrap().op = VT_LINETO; }
    }
}

/// `tile.cpp:coalcmp` and `metacmp` (tippecanoe, BSD 2-Clause).
fn coal_cmp(ctx: &Ctx, a: u32, b: u32) -> std::cmp::Ordering {
    let (ac, am) = ctx.meta(a);
    let (bc, bm) = ctx.meta(b);
    let attrs = |c: &Chunk, m: &FeatMeta| c.attrs(m).map(|(k, v)| {
        let v = &c.vals[v as usize];
        (c.keys[k as usize].to_string(), v.ty, c.val_text(v).to_string())
    }).collect::<Vec<_>>();
    let aa = attrs(ac, am);
    let ba = attrs(bc, bm);
    (am.geom_type as u8).cmp(&(bm.geom_type as u8))
        .then_with(|| ac.id(am).cmp(&bc.id(bm)))
        .then_with(|| aa.len().cmp(&ba.len()))
        .then_with(|| aa.cmp(&ba))
}

/// Adjacent feature coalescing from felt tile.cpp (BSD 2-Clause).
fn coalesce_layer(ctx: &Ctx, layer: &mut MvtLayer, z: u32) {
    let mut out: Vec<(MvtFeature, bool)> = Vec::new();
    for feature in std::mem::take(&mut layer.features) {
        if let Some((previous, merged)) = out.last_mut()
            && previous.geom_type == feature.geom_type && previous.id == feature.id && previous.tags == feature.tags {
            previous.geometry.extend(feature.geometry);
            *merged = true;
            continue;
        }
        out.push((feature, false));
    }
    SCRATCH.with_borrow_mut(|s| {
        for (mut feature, merged) in out {
            if merged && feature.geom_type != GeomType::Point as u8 {
                s.g.clear();
                let mut first = (0, 0);
                for p in &feature.geometry {
                    if p.op == VT_MOVETO { first = (p.x, p.y); }
                    if p.op == vtmake_core::VT_CLOSEPATH {
                        s.g.push(Draw::new(VT_LINETO, first.0, first.1));
                    } else { s.g.push(Draw::new(p.op, p.x, p.y)); }
                }
                if feature.geom_type == GeomType::Polygon as u8 {
                    if s.cleaner.coalesce(&mut s.g).is_err() {
                        eprintln!("Polygon cleaning failed");
                        std::process::exit(1);
                    }
                    vtmake_geom::simplify::close_poly(&mut s.g);
                    vtmake_geom::remove_noop(&mut s.g, GeomType::Polygon, 0);
                } else {
                    vtmake_geom::remove_noop(&mut s.g, GeomType::Line, 0);
                    if !ctx.geo.opts.no_simplify && !(z == ctx.geo.opts.maxzoom && ctx.geo.opts.no_simplify_maxzoom) {
                        vtmake_geom::simplify::simplify_lines(&mut s.g, 32, 0, !(ctx.geo.opts.no_clipping || ctx.geo.opts.no_duplication), simp::<false>(ctx, z), 0, &mut s.simplify);
                    }
                }
                if feature.geom_type == GeomType::Line as u8 {
                    vtmake_geom::remove_noop(&mut s.g, GeomType::Line, 0);
                }
                feature.geometry = s.g.iter().map(|d| MvtPoint { op: d.op, x: d.x, y: d.y }).collect();
            }
            if !feature.geometry.is_empty() { layer.features.push(feature); }
        }
    });
}
