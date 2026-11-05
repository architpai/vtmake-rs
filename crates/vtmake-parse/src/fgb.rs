//! FlatGeobuf input: a port of tippecanoe's `flatgeobuf.cpp` with a small
//! FlatBuffers table reader (the FlatGeobuf schema: header.fbs, feature.fbs).
//!
//! As tippecanoe: the reader does not check the magic bytes; an indexed file
//! (`index_node_size` > 0, the default 16) gives each feature its sequence
//! number as the ID; Point and MultiPoint give points, LineString,
//! MultiLineString and Polygon give rings or lines at the `ends`, MultiPolygon
//! gives its parts. Other geometry types and Binary columns stop with exit 106.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.
//! `packed_rtree_size` is from FlatGeobuf (BSD 2-Clause, Copyright (c) 2018,
//! Björn Harrtell), as tippecanoe copies it.

use crate::geojson::{utf8, utf8_or};
use crate::{ParsedFeature, Position};
use vtmake_core::{Attr, AttrType, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};

const EXIT_IMPOSSIBLE: i32 = 106;

fn die(msg: &str) -> ! {
    fatal(msg).exit()
}

fn fatal(msg: &str) -> crate::ParseError {
    crate::ParseError { exit_code: EXIT_IMPOSSIBLE, message: msg.into() }
}

/// Keep the first error in `err`; gives `None`.
fn fail<T>(err: &mut Option<crate::ParseError>, e: crate::ParseError) -> Option<T> {
    err.get_or_insert(e);
    None
}

/// A FlatBuffers table in `b` at `pos`.
#[derive(Clone, Copy)]
struct Table<'a> {
    b: &'a [u8],
    pos: usize,
}

fn u16_at(b: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(p..p + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(p..p + 4)?.try_into().ok()?))
}

impl<'a> Table<'a> {
    /// The root table of a buffer `b` (its first 4 bytes are the offset).
    fn root(b: &'a [u8]) -> Option<Self> {
        let pos = u32_at(b, 0)? as usize;
        let t = Table { b, pos };
        t.field(0)?; // checks the vtable
        Some(t)
    }

    /// The position of field `i`, or None if it is absent. `Err` on a bad table.
    fn field(&self, i: usize) -> Option<Option<usize>> {
        let soff = i32::from_le_bytes(self.b.get(self.pos..self.pos + 4)?.try_into().ok()?);
        let vt = (self.pos as i64 - soff as i64).try_into().ok()?;
        let vsize = u16_at(self.b, vt)? as usize;
        let tsize = u16_at(self.b, vt + 2)? as usize;
        if vsize < 4 || self.pos + tsize > self.b.len() {
            return None;
        }
        if 4 + 2 * i + 2 > vsize {
            return Some(None);
        }
        let off = u16_at(self.b, vt + 4 + 2 * i)? as usize;
        Some((off != 0).then_some(self.pos + off))
    }

    fn scalar<const N: usize>(&self, i: usize) -> Option<Option<[u8; N]>> {
        match self.field(i)? {
            None => Some(None),
            Some(p) => Some(Some(self.b.get(p..p + N)?.try_into().ok()?)),
        }
    }

    fn u8_or(&self, i: usize, d: u8) -> Option<u8> {
        Some(self.scalar::<1>(i)?.map_or(d, |v| v[0]))
    }
    fn u16_or(&self, i: usize, d: u16) -> Option<u16> {
        Some(self.scalar::<2>(i)?.map_or(d, u16::from_le_bytes))
    }
    fn u64_or(&self, i: usize, d: u64) -> Option<u64> {
        Some(self.scalar::<8>(i)?.map_or(d, u64::from_le_bytes))
    }

    /// The target of an offset field.
    fn target(&self, i: usize) -> Option<Option<usize>> {
        match self.field(i)? {
            None => Some(None),
            Some(p) => Some(Some(p + u32_at(self.b, p)? as usize)),
        }
    }

    /// A vector of `size`-byte elements: its data.
    fn vector(&self, i: usize, size: usize) -> Option<Option<&'a [u8]>> {
        match self.target(i)? {
            None => Some(None),
            Some(v) => {
                let n = u32_at(self.b, v)? as usize;
                Some(Some(self.b.get(v + 4..v + 4 + n.checked_mul(size)?)?))
            }
        }
    }

    fn string(&self, i: usize) -> Option<Option<&'a [u8]>> {
        self.vector(i, 1)
    }

    fn table(&self, i: usize) -> Option<Option<Table<'a>>> {
        match self.target(i)? {
            None => Some(None),
            Some(p) => {
                let t = Table { b: self.b, pos: p };
                t.field(0)?;
                Some(Some(t))
            }
        }
    }

    /// A vector of tables.
    fn tables(&self, i: usize) -> Option<Option<Vec<Table<'a>>>> {
        let Some(v) = self.target(i)? else { return Some(None) };
        let n = u32_at(self.b, v)? as usize;
        let mut out = Vec::with_capacity(n.min(1 << 16));
        for k in 0..n {
            let e = v + 4 + 4 * k;
            let t = Table { b: self.b, pos: e + u32_at(self.b, e)? as usize };
            t.field(0)?;
            out.push(t);
        }
        Some(Some(out))
    }
}

/// `PackedRTreeSize`.
fn packed_rtree_size(num_items: u64, node_size: u16) -> u64 {
    if node_size < 2 {
        die("terminate called after throwing an instance of 'std::invalid_argument'\n  what():  Node size must be at least 2");
    }
    if num_items == 0 {
        die("terminate called after throwing an instance of 'std::invalid_argument'\n  what():  Number of items must be greater than 0");
    }
    let node_size = node_size as u64;
    let mut n = num_items;
    let mut num_nodes = n;
    loop {
        n = n.div_ceil(node_size);
        num_nodes += n;
        if n == 1 {
            break;
        }
    }
    num_nodes * 40
}

fn f64s(b: &[u8]) -> impl Iterator<Item = f64> + '_ {
    b.as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c))
}

/// `readPoints`.
fn read_points(g: Table<'_>, out: &mut Vec<Position>) -> Option<()> {
    let xy: Vec<f64> = g.vector(1, 8)?.map(|v| f64s(v).collect()).unwrap_or_default();
    for p in xy.chunks(2) {
        out.push(Position { op: VT_MOVETO, lon: p[0], lat: *p.get(1)? });
    }
    Some(())
}

/// `readLinePart`.
fn read_line_part(g: Table<'_>, out: &mut Vec<Position>) -> Option<()> {
    let xy: Vec<f64> = g.vector(1, 8)?.map(|v| f64s(v).collect()).unwrap_or_default();
    let ends: Option<Vec<u32>> = g.vector(0, 4)?.map(|v| v.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect());
    let mut current_end = 0;
    let mut i = 0;
    while i < xy.len() {
        let (x, y) = (xy[i], *xy.get(i + 1)?);
        let new_part = i == 0 || ends.as_ref().is_some_and(|e| current_end < e.len() && i as u64 == e[current_end] as u64 * 2);
        if new_part {
            out.push(Position { op: VT_MOVETO, lon: x, lat: y });
            if i > 0 {
                current_end += 1;
            }
        } else {
            out.push(Position { op: VT_LINETO, lon: x, lat: y });
        }
        i += 2;
    }
    Some(())
}

/// `readGeometry`: the drawing and the geometry type.
fn read_geometry(g: Table<'_>, header_type: u8, out: &mut Vec<Position>, err: &mut Option<crate::ParseError>) -> Option<GeomType> {
    let t = if header_type == 0 { g.u8_or(6, 0)? } else { header_type };
    match t {
        1 | 4 => {
            read_points(g, out)?;
            Some(GeomType::Point)
        }
        2 | 5 => {
            read_line_part(g, out)?;
            Some(GeomType::Line)
        }
        3 => {
            read_line_part(g, out)?;
            Some(GeomType::Polygon)
        }
        6 => {
            let Some(parts) = g.tables(7)? else { return fail(err, fatal("flatgeobuf MultiPolygon without parts")) };
            for part in parts {
                read_line_part(part, out)?;
                out.push(Position { op: VT_CLOSEPATH, lon: 0.0, lat: 0.0 });
            }
            Some(GeomType::Polygon)
        }
        _ => fail(err, fatal(&format!("flatgeobuf has unsupported geometry type {header_type}"))),
    }
}

/// One feature (`readFeature`).
/// The first error (other than a bad buffer) goes into `err`.
fn read_feature(f: Table<'_>, id: Option<u64>, header_type: u8, names: &[Box<str>], types: &[u8], fname: &str, err: &mut Option<crate::ParseError>) -> Option<ParsedFeature> {
    let Some(g) = f.table(0)? else { return fail(err, fatal("flatgeobuf feature without geometry")) };
    let mut geom = Vec::new();
    let geom_type = read_geometry(g, header_type, &mut geom, err)?;
    let mut attrs = Vec::new();
    if let Some(p) = f.vector(1, 1)? {
        let mut pos = 0;
        while pos < p.len() {
            let col = u16_at(p, pos)? as usize;
            let v = pos + 2;
            let ty = *types.get(col)?;
            let num = |s: String| (AttrType::Number, s.into_boxed_str());
            let get = |n: usize| p.get(v..v + n);
            let (value, size) = match ty {
                0 => (num((get(1)?[0] as i8).to_string()), 1),
                1 => (num(get(1)?[0].to_string()), 1),
                2 => ((AttrType::Bool, if get(1)?[0] != 0 { "true" } else { "false" }.into()), 1),
                3 => (num(i16::from_le_bytes(get(2)?.try_into().ok()?).to_string()), 2),
                4 => (num(u16::from_le_bytes(get(2)?.try_into().ok()?).to_string()), 2),
                5 => (num(i32::from_le_bytes(get(4)?.try_into().ok()?).to_string()), 4),
                6 => (num(u32::from_le_bytes(get(4)?.try_into().ok()?).to_string()), 4),
                7 => (num(i64::from_le_bytes(get(8)?.try_into().ok()?).to_string()), 8),
                8 => (num(u64::from_le_bytes(get(8)?.try_into().ok()?).to_string()), 8),
                9 => (num(crate::dtoa::dtoa_milo(f32::from_le_bytes(get(4)?.try_into().ok()?) as f64)), 4),
                10 => (num(crate::dtoa::dtoa_milo(f64::from_le_bytes(get(8)?.try_into().ok()?))), 8),
                11..=13 => {
                    let n = u32_at(p, v)? as usize;
                    let s = p.get(v + 4..v + 4 + n)?;
                    match utf8_or(s, fname, 0) {
                        Ok(t) => ((AttrType::String, t), 4 + n),
                        Err(e) => return fail(err, e),
                    }
                }
                _ => return fail(err, fatal(&format!("flatgeobuf has unsupported column type {ty}"))),
            };
            attrs.push(Attr { key: names[col].clone(), ty: value.0, value: value.1 });
            pos = v + size;
        }
    }
    Some(ParsedFeature { geom_type, geom, attrs, minzoom: -1, maxzoom: -1, layer: None, id, seq: 0 })
}

/// `parse_flatgeobuf`: the features of a FlatGeobuf file in their order.
pub fn read_fgb(src: &[u8], fname: &str, quiet: bool, out: &mut Vec<ParsedFeature>) {
    let need = 12 + header_size(src);
    let r = FgbReader::new(&src[..need.min(src.len())], fname, quiet);
    let mut start = r.first as usize;
    let mut index = 0;
    while start < src.len() {
        let Some(n) = FgbReader::feature_size(&src[start..]).filter(|&n| start + n <= src.len()) else {
            die("flatgeobuf feature buffer verification failed")
        };
        out.push(r.feature(&src[start..start + n], index).unwrap_or_else(|e| e.exit()));
        index += 1;
        start += n;
    }
}

/// The size of the header table: the `u32` at byte 8. `head`: the first 12
/// bytes of the file (or all if the file is shorter).
pub fn header_size(head: &[u8]) -> usize {
    u32_at(head, 8).unwrap_or_else(|| die("flatgeobuf header verification failed")) as usize
}

/// `parse_flatgeobuf` in parts: [`FgbReader::new`] reads the header,
/// [`FgbReader::feature`] each feature (also in parallel). The caller writes
/// the errors in the order of the features.
pub struct FgbReader {
    fname: String,
    geometry_type: u8,
    names: Vec<Box<str>>,
    types: Vec<u8>,
    /// An indexed file: the feature IDs are the sequence numbers.
    indexed: bool,
    /// The file position of the first feature.
    pub first: u64,
}

impl FgbReader {
    /// `head`: the first `12 + header_size(head)` bytes of the file (or all if
    /// the file is shorter).
    pub fn new(head: &[u8], fname: &str, quiet: bool) -> Self {
        let header_size = header_size(head);
        let header = head
            .get(12..12 + header_size)
            .and_then(Table::root)
            .unwrap_or_else(|| die("flatgeobuf header verification failed"));
        /// features_count, index_node_size, geometry_type, column names and types.
        type Header = (u64, u16, u8, Vec<Box<str>>, Vec<u8>);
        let read_header = || -> Option<Header> {
            let features_count = header.u64_or(8, 0)?;
            let node_size = header.u16_or(9, 16)?;
            let geometry_type = header.u8_or(2, 0)?;
            let mut names = Vec::new();
            let mut types = Vec::new();
            if let Some(cols) = header.tables(7)? {
                for c in cols {
                    names.push(utf8(c.string(0)?.unwrap_or_default(), fname, 0));
                    types.push(c.u8_or(1, 0)?);
                }
            }
            Some((features_count, node_size, geometry_type, names, types))
        };
        let (features_count, node_size, geometry_type, names, types) = read_header().unwrap_or_else(|| die("flatgeobuf header verification failed"));
        let mut index_size = 0;
        if node_size > 0 {
            if !quiet {
                eprintln!("detected indexed FlatGeobuf: assigning feature IDs by sequence");
            }
            index_size = packed_rtree_size(features_count, node_size);
        }
        FgbReader { fname: fname.into(), geometry_type, names, types, indexed: node_size > 0, first: 12u64 + header_size as u64 + index_size }
    }

    /// The size (with the 4-byte size field) of the feature at the start of
    /// `buf`, if `buf` has the size field.
    pub fn feature_size(buf: &[u8]) -> Option<usize> {
        u32_at(buf, 0).map(|n| 4 + n as usize)
    }

    /// The feature in `b` (with its size field); `index`: its number in the
    /// file.
    pub fn feature(&self, b: &[u8], index: u64) -> Result<ParsedFeature, crate::ParseError> {
        let mut err = None;
        let id = self.indexed.then_some(index);
        let f = Table::root(&b[4..]).and_then(|t| read_feature(t, id, self.geometry_type, &self.names, &self.types, &self.fname, &mut err));
        match (f, err) {
            (Some(f), _) => Ok(f),
            (None, Some(e)) => Err(e),
            (None, None) => Err(fatal("flatgeobuf feature buffer verification failed")),
        }
    }
}
