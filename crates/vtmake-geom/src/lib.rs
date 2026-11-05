//! Exact port of the tippecanoe geometry functions that the plain path
//! uses (spec: `docs/spec/tippecanoe-plain-path.md`). "Exact" means: the
//! same integer results as felt/tippecanoe `4f26211` (x86-64, gcc) for the
//! same input. The tests with the `cpp-ref` feature compare each function
//! with the real C++ code.
//!
//! Modules:
//! - [`projection`]: `lonlat2tile`, `tile2lonlat`, quadkey (spec 4.1, 8.1).
//! - [`polygon`]: `get_area`, `fix_polygon`, center of mass, index point
//!   (spec 7.1, 8.1, 8.2).
//! - [`drop`]: the default point drop rate (spec 5.2).
//! - [`clip`]: quick check and the clips of points, lines, polygons
//!   (spec 6.4 to 6.7).
//! - [`simplify`]: `remove_noop`, Douglas–Peucker, `to_tile_scale`,
//!   `close_poly` (spec 7.4, 7.6, 7.7).
//! - [`tiny`]: tiny polygons, `line_is_too_small`, `revive_polygon`
//!   (spec 7.2, 7.3, 7.5).
//! - [`tile`]: one feature at one tile, in tippecanoe's order (spec 6, 7).
//!
//! Polygon cleaning (wagyu, `coalesce_polygon`) is not in this crate. Its
//! input is the result of [`tile::tile_simplify`] and its output goes to
//! [`tile::tile_finish_polygon`].

pub mod clip;
pub mod label;
pub mod drop;
pub mod num;
pub mod polygon;
pub mod projection;
pub mod shared_borders;
pub mod simplify;
pub mod tile;
pub mod tiny;
pub mod visvalingam;

/// tippecanoe `EXIT_IMPOSSIBLE`.
pub const EXIT_IMPOSSIBLE: i32 = 106;

#[cfg(feature = "cpp-ref")]
pub mod cpp_ref;

pub use clip::{
    PolyClipScratch, buffer_world, clip, clip_lines, clip_lines_box, clip_point, clip_point_box,
    quick_check, simple_clip_poly, simple_clip_poly_box, simple_clip_poly_nodes,
};
pub use drop::{DropState, LayerRateDrop, rate_keeps};
pub use polygon::{
    center_of_mass_mp, feature_index, fix_polygon, fix_polygon_opts, geometry_bbox, get_area, get_mp_area,
    index_point,
};
pub use projection::{decode_quadkey, encode_quadkey, epsg3857_to_world, lonlat_to_world, lonlat2tile, tile2lonlat};
pub use simplify::{
    NodeCtx, SimplifyScratch, close_poly, distance_from_line, douglas_peucker, douglas_peucker_opts,
    encode_vertex, impose_tile_boundaries,
    remove_noop, simplify_feature, simplify_feature_opts, simplify_lines, simplify_lines_opts,
    stairstep, to_tile_scale,
};
pub use tile::{
    GeomOpts, GeomScratch, PolygonPre, Reduced, child_tile_range, clip_relative,
    clip_relative_opts, clip_to_tile, clip_to_tile_opts, point_within_tile, tile_finish_line,
    tile_finish_polygon, tile_origin, tile_reduce, tile_reduce_opts, tile_simplify,
    tile_label_point, tile_simplify_early_opts, tile_simplify_opts, to_tile_relative,
};
pub use visvalingam::visvalingam;
pub use tiny::{
    ReviveInfo, TinyPolyAccum, TinyPolyResult, line_is_too_small, reduce_tiny_poly, revive_polygon,
};
