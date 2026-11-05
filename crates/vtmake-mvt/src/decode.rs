//! A small MVT decoder, for tests and tools.

use crate::{MvtPoint, MvtValue};

pub struct DecodedFeature {
    pub geom_type: u8,
    pub id: Option<u64>,
    pub tags: Vec<u32>,
    pub geometry: Vec<MvtPoint>,
}

pub struct DecodedLayer {
    pub name: String,
    pub version: u32,
    pub extent: u32,
    pub keys: Vec<String>,
    pub values: Vec<MvtValue>,
    pub features: Vec<DecodedFeature>,
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let c = *self.b.get(self.p)?;
            self.p += 1;
            v |= ((c & 0x7f) as u64) << shift;
            if c < 0x80 {
                return Some(v);
            }
        }
        None
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.varint()? as usize;
        let s = self.b.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn fixed(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn skip(&mut self, wire: u64) -> Option<()> {
        match wire {
            0 => drop(self.varint()?),
            1 => drop(self.fixed(8)?),
            2 => drop(self.bytes()?),
            5 => drop(self.fixed(4)?),
            _ => return None,
        }
        Some(())
    }
    fn done(&self) -> bool {
        self.p >= self.b.len()
    }
}

fn packed(b: &[u8]) -> Option<Vec<u32>> {
    let mut r = Reader { b, p: 0 };
    let mut v = Vec::new();
    while !r.done() {
        v.push(r.varint()? as u32);
    }
    Some(v)
}

fn value(b: &[u8]) -> Option<MvtValue> {
    let mut r = Reader { b, p: 0 };
    let mut out = None;
    while !r.done() {
        let k = r.varint()?;
        out = Some(match (k >> 3, k & 7) {
            (1, 2) => MvtValue::String(std::str::from_utf8(r.bytes()?).ok()?.into()),
            (2, 5) => MvtValue::Float(f32::from_le_bytes(r.fixed(4)?.try_into().ok()?)),
            (3, 1) => MvtValue::Double(f64::from_le_bytes(r.fixed(8)?.try_into().ok()?)),
            (4, 0) => MvtValue::Int(r.varint()? as i64),
            (5, 0) => MvtValue::Uint(r.varint()?),
            (6, 0) => {
                let u = r.varint()?;
                MvtValue::Sint(((u >> 1) as i64) ^ -((u & 1) as i64))
            }
            (7, 0) => MvtValue::Bool(r.varint()? != 0),
            (_, w) => {
                r.skip(w)?;
                continue;
            }
        });
    }
    out
}

fn geometry(cmds: &[u32]) -> Option<Vec<MvtPoint>> {
    let mut out = Vec::new();
    let (mut x, mut y) = (0i64, 0i64);
    let mut i = 0;
    while i < cmds.len() {
        let op = (cmds[i] & 7) as u8;
        let count = cmds[i] >> 3;
        i += 1;
        for _ in 0..count {
            if op == 7 {
                out.push(MvtPoint { op, x: 0, y: 0 });
            } else {
                let dz = |u: u32| ((u >> 1) as i32 ^ -((u & 1) as i32)) as i64;
                x += dz(*cmds.get(i)?);
                y += dz(*cmds.get(i + 1)?);
                i += 2;
                out.push(MvtPoint { op, x, y });
            }
        }
    }
    Some(out)
}

/// Decode an uncompressed tile.
pub fn decode_tile(b: &[u8]) -> Option<Vec<DecodedLayer>> {
    let mut r = Reader { b, p: 0 };
    let mut layers = Vec::new();
    while !r.done() {
        let k = r.varint()?;
        if k != (3 << 3 | 2) {
            r.skip(k & 7)?;
            continue;
        }
        let mut lr = Reader { b: r.bytes()?, p: 0 };
        let mut l = DecodedLayer { name: String::new(), version: 1, extent: 4096, keys: vec![], values: vec![], features: vec![] };
        while !lr.done() {
            let k = lr.varint()?;
            match (k >> 3, k & 7) {
                (15, 0) => l.version = lr.varint()? as u32,
                (1, 2) => l.name = String::from_utf8(lr.bytes()?.to_vec()).ok()?,
                (5, 0) => l.extent = lr.varint()? as u32,
                (3, 2) => l.keys.push(String::from_utf8(lr.bytes()?.to_vec()).ok()?),
                (4, 2) => l.values.push(value(lr.bytes()?)?),
                (2, 2) => {
                    let mut fr = Reader { b: lr.bytes()?, p: 0 };
                    let mut f = DecodedFeature { geom_type: 0, id: None, tags: vec![], geometry: vec![] };
                    while !fr.done() {
                        let k = fr.varint()?;
                        match (k >> 3, k & 7) {
                            (1, 0) => f.id = Some(fr.varint()?),
                            (2, 2) => f.tags = packed(fr.bytes()?)?,
                            (3, 0) => f.geom_type = fr.varint()? as u8,
                            (4, 2) => f.geometry = geometry(&packed(fr.bytes()?)?)?,
                            (_, w) => fr.skip(w)?,
                        }
                    }
                    l.features.push(f);
                }
                (_, w) => lr.skip(w)?,
            }
        }
        layers.push(l);
    }
    Some(layers)
}
