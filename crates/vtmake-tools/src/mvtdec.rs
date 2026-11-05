//! Port of tippecanoe's tile decoder (`mvt.cpp:mvt_tile::decode`, with the
//! protozero rules). It keeps what the felt tools see: a value with no field
//! is null, the layer version and extent are 0 if they are not set, a
//! ClosePath gives one point for each command, and a MoveTo/LineTo stops at
//! the end of the data.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

pub const MOVETO: u32 = 1;
pub const LINETO: u32 = 2;
pub const CLOSEPATH: u32 = 7;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(Vec<u8>),
    Float(f32),
    Double(f64),
    Int(i64),
    Uint(u64),
    Sint(i64),
    Bool(bool),
    Null,
}

#[derive(Clone, Copy, Debug)]
pub struct Geom {
    pub op: u32,
    pub x: i64,
    pub y: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Feature {
    pub tags: Vec<u32>,
    pub geometry: Vec<Geom>,
    pub ty: i32,
    pub id: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub version: u32,
    pub name: Vec<u8>,
    pub features: Vec<Feature>,
    pub keys: Vec<Vec<u8>>,
    pub values: Vec<Value>,
    pub extent: u32,
}

/// Why a tile did not decode.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The gzip or zlib data is bad (tippecanoe: "Decompression error", exit 109).
    Decompress,
    /// The protobuf data is bad (a protozero exception).
    Pbf,
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
    tag: u32,
    wire: u32,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0, tag: 0, wire: 0 }
    }

    fn varint(&mut self) -> Result<u64, Error> {
        let mut v = 0u64;
        for i in 0..10 {
            let c = *self.b.get(self.p).ok_or(Error::Pbf)?;
            self.p += 1;
            v |= ((c & 0x7f) as u64) << (7 * i);
            if c < 0x80 {
                return Ok(v);
            }
        }
        Err(Error::Pbf)
    }

    fn next(&mut self) -> Result<bool, Error> {
        if self.p >= self.b.len() {
            return Ok(false);
        }
        let k = self.varint()? as u32;
        self.tag = k >> 3;
        self.wire = k & 7;
        match self.wire {
            0 | 1 | 2 | 5 => Ok(true),
            _ => Err(Error::Pbf),
        }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let s = self.b.get(self.p..self.p.checked_add(n).ok_or(Error::Pbf)?).ok_or(Error::Pbf)?;
        self.p += n;
        Ok(s)
    }

    fn view(&mut self) -> Result<&'a [u8], Error> {
        if self.wire != 2 {
            return Err(Error::Pbf);
        }
        let n = self.varint()? as u32 as usize;
        self.bytes(n)
    }

    fn skip(&mut self) -> Result<(), Error> {
        match self.wire {
            0 => drop(self.varint()?),
            1 => drop(self.bytes(8)?),
            2 => drop(self.view()?),
            _ => drop(self.bytes(4)?),
        }
        Ok(())
    }

    fn get_varint(&mut self) -> Result<u64, Error> {
        if self.wire != 0 {
            return Err(Error::Pbf);
        }
        self.varint()
    }

    /// protozero `get_bool`: the first byte of the varint is not zero.
    fn get_bool(&mut self) -> Result<bool, Error> {
        if self.wire != 0 {
            return Err(Error::Pbf);
        }
        let b = *self.b.get(self.p).ok_or(Error::Pbf)? != 0;
        self.varint()?;
        Ok(b)
    }

    fn packed(&mut self, out: &mut Vec<u32>) -> Result<(), Error> {
        let mut r = Reader::new(self.view()?);
        while r.p < r.b.len() {
            out.push(r.varint()? as u32);
        }
        Ok(())
    }
}

fn zigzag32(u: u32) -> i32 {
    ((u >> 1) as i32) ^ -((u & 1) as i32)
}

fn value(b: &[u8]) -> Result<Value, Error> {
    let mut r = Reader::new(b);
    let mut v = Value::Null;
    while r.next()? {
        v = match r.tag {
            1 => Value::String(r.view()?.to_vec()),
            2 => {
                if r.wire != 5 {
                    return Err(Error::Pbf);
                }
                Value::Float(f32::from_le_bytes(r.bytes(4)?.try_into().unwrap()))
            }
            3 => {
                if r.wire != 1 {
                    return Err(Error::Pbf);
                }
                Value::Double(f64::from_le_bytes(r.bytes(8)?.try_into().unwrap()))
            }
            4 => Value::Int(r.get_varint()? as i64),
            5 => Value::Uint(r.get_varint()?),
            6 => {
                let u = r.get_varint()?;
                Value::Sint(((u >> 1) as i64) ^ -((u & 1) as i64))
            }
            7 => Value::Bool(r.get_bool()?),
            _ => {
                r.skip()?;
                continue;
            }
        };
    }
    Ok(v)
}

fn feature(b: &[u8]) -> Result<Feature, Error> {
    let mut r = Reader::new(b);
    let mut f = Feature::default();
    let mut geoms = Vec::new();
    while r.next()? {
        match r.tag {
            1 => f.id = Some(r.get_varint()?),
            2 => r.packed(&mut f.tags)?,
            3 => f.ty = r.get_varint()? as i32,
            4 => r.packed(&mut geoms)?,
            _ => r.skip()?,
        }
    }
    let (mut px, mut py) = (0i64, 0i64);
    f.geometry.reserve(geoms.len());
    let mut g = 0;
    while g < geoms.len() {
        let op = geoms[g] & 7;
        let count = geoms[g] >> 3;
        if op == MOVETO || op == LINETO {
            let mut k = 0;
            while k < count && g + 2 < geoms.len() {
                px += zigzag32(geoms[g + 1]) as i64;
                py += zigzag32(geoms[g + 2]) as i64;
                g += 2;
                f.geometry.push(Geom { op, x: px, y: py });
                k += 1;
            }
        } else {
            f.geometry.push(Geom { op, x: 0, y: 0 });
        }
        g += 1;
    }
    Ok(f)
}

fn layer(b: &[u8]) -> Result<Layer, Error> {
    let mut r = Reader::new(b);
    let mut l = Layer::default();
    while r.next()? {
        match r.tag {
            1 => l.name = r.view()?.to_vec(),
            3 => l.keys.push(r.view()?.to_vec()),
            4 => l.values.push(value(r.view()?)?),
            5 => l.extent = r.get_varint()? as u32,
            15 => l.version = r.get_varint()? as u32,
            2 => l.features.push(feature(r.view()?)?),
            _ => r.skip()?,
        }
    }
    Ok(l)
}

/// `is_compressed`: zlib (78 9C) or gzip (1F 8B) magic, and more than 2 bytes.
pub fn is_compressed(d: &[u8]) -> bool {
    d.len() > 2 && ((d[0] == 0x78 && d[1] == 0x9C) || (d[0] == 0x1F && d[1] == 0x8B))
}

/// Decode a tile (gzip, zlib or raw). Returns the layers and whether the
/// tile was compressed.
pub fn decode(message: &[u8]) -> Result<(Vec<Layer>, bool), Error> {
    let mut buf = Vec::new();
    let (src, compressed) = if is_compressed(message) {
        if !vtmake_mvt::gzip::gunzip(message, &mut buf) {
            return Err(Error::Decompress);
        }
        (&buf[..], true)
    } else {
        (message, false)
    };
    let mut r = Reader::new(src);
    let mut layers = Vec::new();
    while r.next()? {
        if r.tag == 3 {
            layers.push(layer(r.view()?)?);
        } else {
            r.skip()?;
        }
    }
    Ok((layers, compressed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_tippecanoe_tile() {
        let mut layer = vtmake_mvt::MvtLayer::new("l");
        let mut f = vtmake_mvt::MvtFeature { geom_type: 3, id: Some(7), ..Default::default() };
        layer.tag(&mut f, "k", &vtmake_mvt::MvtValue::Sint(-3));
        f.geometry = vec![
            vtmake_mvt::MvtPoint { op: 1, x: 1, y: 1 },
            vtmake_mvt::MvtPoint { op: 2, x: 5, y: 1 },
            vtmake_mvt::MvtPoint { op: 2, x: 5, y: 5 },
            vtmake_mvt::MvtPoint { op: 7, x: 0, y: 0 },
        ];
        layer.features.push(f);
        let mut out = Vec::new();
        vtmake_mvt::MvtTile { layers: vec![layer] }.encode(&mut out);
        let (ls, z) = decode(&out).unwrap();
        assert!(!z);
        assert_eq!(ls[0].name, b"l");
        assert_eq!((ls[0].version, ls[0].extent), (2, 4096));
        assert_eq!(ls[0].values, [Value::Sint(-3)]);
        let g: Vec<(u32, i64, i64)> = ls[0].features[0].geometry.iter().map(|g| (g.op, g.x, g.y)).collect();
        assert_eq!(g, [(1, 1, 1), (2, 5, 1), (2, 5, 5), (7, 0, 0)]);
        assert_eq!(ls[0].features[0].id, Some(7));
    }
}
