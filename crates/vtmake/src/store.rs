//! Compact in-memory feature store, filled by parallel readers.
//!
//! - Geometry: varint deltas, about 5 bytes per vertex (a `Draw` is 24 bytes).
//! - Attributes: (key id, value id) pairs. Each chunk keeps one table of distinct
//!   values, so repeated values (layer names, flags, categories) are stored once.

use vtmake_core::{AttrType, Draw, DrawVec, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};

#[derive(Clone, Copy, Debug)]
pub struct FeatMeta {
    pub index: u64,
    pub bbox: [i64; 4],
    /// Index into the chunk ID table. u32::MAX means no ID.
    pub id: u32,
    pub geom_off: u32,
    pub attr_off: u32,
    pub ndraws: u32,
    pub nattrs: u16,
    /// Index into the chunk's layer names. u16::MAX: the default layer.
    pub layer: u16,
    pub geom_type: GeomType,
    pub minzoom: i8,
    pub maxzoom: i8,
    /// Computed by the drop-rate pass (spec 5.2). Only used for points without minzoom.
    pub drop_minzoom: i8,
}

/// One distinct attribute value of a chunk: its type and its text in `val_text`.
#[derive(Clone, Copy, Debug)]
pub struct Val {
    pub ty: AttrType,
    pub off: u32,
    pub len: u32,
}

/// The features of one input chunk.
#[derive(Default)]
pub struct Chunk {
    pub metas: Vec<FeatMeta>,
    /// Source positions. These are removed after sorting.
    pub seqs: Vec<u64>,
    pub ids: Vec<u64>,
    pub geom: Vec<u8>,
    pub attrs: Vec<u8>,
    pub vals: Vec<Val>,
    pub val_text: Vec<u8>,
    pub keys: Vec<Box<str>>,
    pub layers: Vec<Box<str>>,
    /// With `--maximum-string-attribute-length` only: the attributes before the
    /// strings are cut, for the tile statistics. `stat_offs[i]`: the start of
    /// feature `i` in `stat_attrs` (the same pairs as `attrs`, other value ids).
    pub stat_attrs: Vec<u8>,
    pub stat_offs: Vec<u32>,
    /// With `-aP`, `-aw` or `--clip-bounding-box` (else empty): one entry for
    /// each feature in `metas`.
    pub extra: Vec<FeatExtra>,
    /// With `--clip-bounding-box`: the source position and the input bounds
    /// of each feature that the box removed (they count in the metadata
    /// bounds of tippecanoe).
    pub clipped_away: Vec<(u64, [[i64; 4]; 2])>,
    /// With `-pn`: the source position and the (mid, one end, other end)
    /// points of each vertex (`struct vertex`; the ends in `draw` order).
    pub node_vertices: Vec<(u64, [(i64, i64); 3])>,
    /// With `-pn`: the source position and the `encode_vertex` key of each
    /// node that tippecanoe always keeps (`add_scaled_node`).
    pub node_points: Vec<(u64, u64)>,
    /// With a prefilter: tippecanoe's sequence number of each feature in
    /// `metas` (the `"sequence"` of the filter input).
    pub felt_seqs: Vec<u64>,
}

/// The values of a feature that only some options need.
#[derive(Clone, Copy, Debug)]
pub struct FeatExtra {
    /// `-aP`: the quadkey of the label point (`sf.label_point`), or 0.
    pub label_point: u64,
    /// With `-aw` or `--clip-bounding-box`: the metadata bounds (`bbox1`,
    /// `bbox2` of `meta::Stats`) of the input geometry.
    pub raw: [[i64; 4]; 2],
}

#[inline]
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
fn get_varint(b: &[u8], p: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let c = b[*p];
        *p += 1;
        v |= ((c & 0x7f) as u64) << shift;
        if c < 0x80 {
            return v;
        }
        shift += 7;
    }
}

#[inline]
fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
fn unzz(u: u64) -> i64 {
    ((u >> 1) as i64) ^ -((u & 1) as i64)
}

/// Encode a geometry. Each draw: varint(zigzag(dx) << 2 | op code), then
/// varint(zigzag(dy)). CLOSEPATH has only the op code.
pub fn encode_geom(g: &[Draw], out: &mut Vec<u8>) {
    let (mut px, mut py) = (0i64, 0i64);
    for d in g {
        match d.op {
            VT_CLOSEPATH => put_varint(out, 2),
            op => {
                let code = if op == VT_MOVETO { 0 } else { 1 };
                put_varint(out, (zz(d.x - px) << 2) | code);
                put_varint(out, zz(d.y - py));
                px = d.x;
                py = d.y;
            }
        }
    }
}

impl Chunk {
    pub fn id(&self, m: &FeatMeta) -> Option<u64> {
        (m.id != u32::MAX).then(|| self.ids[m.id as usize])
    }

    /// Decode the geometry of `m` and append it to `out`.
    pub fn decode_geom(&self, m: &FeatMeta, out: &mut DrawVec) {
        let b = &self.geom;
        let mut p = m.geom_off as usize;
        let (mut x, mut y) = (0i64, 0i64);
        out.reserve(m.ndraws as usize);
        for _ in 0..m.ndraws {
            let v = get_varint(b, &mut p);
            match v & 3 {
                2 => out.push(Draw::new(VT_CLOSEPATH, 0, 0)),
                code => {
                    x += unzz(v >> 2);
                    y += unzz(get_varint(b, &mut p));
                    out.push(Draw::new(if code == 0 { VT_MOVETO } else { VT_LINETO }, x, y));
                }
            }
        }
    }

    /// The text of value `v` (valid UTF-8: it comes from a `&str`).
    #[inline]
    pub fn val_text(&self, v: &Val) -> &str {
        let b = &self.val_text[v.off as usize..(v.off + v.len) as usize];
        // SAFETY: only `&str` bytes are pushed, and a value starts and ends there.
        unsafe { std::str::from_utf8_unchecked(b) }
    }

    /// The (key id, value id) pairs of `m`, in input order.
    pub fn attrs<'a>(&'a self, m: &FeatMeta) -> impl Iterator<Item = (u32, u32)> + 'a {
        pairs(&self.attrs, m.attr_off as usize, m.nattrs)
    }

    /// The pairs of feature `i` (`m`) for the tile statistics.
    pub fn stat_attrs<'a>(&'a self, i: usize, m: &FeatMeta) -> impl Iterator<Item = (u32, u32)> + 'a {
        if self.stat_offs.is_empty() {
            pairs(&self.attrs, m.attr_off as usize, m.nattrs)
        } else {
            pairs(&self.stat_attrs, self.stat_offs[i] as usize, m.nattrs)
        }
    }
}

fn pairs(b: &[u8], mut p: usize, n: u16) -> impl Iterator<Item = (u32, u32)> + '_ {
    (0..n).map(move |_| {
        let k = get_varint(b, &mut p) as u32;
        let v = get_varint(b, &mut p) as u32;
        (k, v)
    })
}

/// A feature in global sort order.
#[derive(Clone, Copy, Debug)]
pub struct Sorted {
    pub chunk: u32,
    pub local: u32,
}

// Keep the tile tables within their memory budget.
const _: () = assert!(std::mem::size_of::<FeatMeta>() == 64);
const _: () = assert!(std::mem::size_of::<Sorted>() == 8);
