//! Shared types for vtmake.
//!
//! The geometry model copies tippecanoe's `drawvec` (see
//! `docs/spec/tippecanoe-plain-path.md`): a flat list of drawing operations in
//! world coordinates (2^32 x 2^32 plane) or tile-relative world coordinates.

/// Drawing operation codes, the same values as tippecanoe and MVT.
pub const VT_MOVETO: u8 = 1;
pub const VT_LINETO: u8 = 2;
pub const VT_CLOSEPATH: u8 = 7;

/// MVT geometry type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum GeomType {
    Point = 1,
    Line = 2,
    Polygon = 3,
}

/// One drawing operation (tippecanoe `struct draw`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Draw {
    pub op: u8,
    pub x: i64,
    pub y: i64,
    /// Simplification keeps this point (tippecanoe `necessary`).
    pub necessary: bool,
}

impl Draw {
    #[inline]
    pub const fn new(op: u8, x: i64, y: i64) -> Self {
        Draw { op, x, y, necessary: false }
    }
}

pub type DrawVec = Vec<Draw>;

/// Attribute value type before MVT encoding (tippecanoe `mvt_*` stringified types).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AttrType {
    String,
    Number,
    Bool,
    Null,
}

/// One attribute. `value` is tippecanoe's stringified form (spec 3.4).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Attr {
    pub key: Box<str>,
    pub ty: AttrType,
    pub value: Box<str>,
}

/// A feature after parsing and projection.
#[derive(Clone, Debug, PartialEq)]
pub struct Feature {
    pub geom_type: GeomType,
    /// World coordinates. Polygons are already fixed by `fix_polygon` (spec 7.1).
    pub geom: DrawVec,
    /// [min x, min y, max x, max y] in world coordinates.
    pub bbox: [i64; 4],
    /// `tippecanoe.minzoom` / `maxzoom`, -1 if not set.
    pub minzoom: i8,
    pub maxzoom: i8,
    /// `tippecanoe.layer` if set, else None (use `--layer`).
    pub layer: Option<Box<str>>,
    pub id: Option<u64>,
    /// In input order. Nulls are kept here and removed at tile time.
    pub attrs: Vec<Attr>,
    /// Sort key part 1: quadkey of the index point (spec 8.1).
    pub index: u64,
    /// Sort key part 2: input order.
    pub seq: u64,
}
