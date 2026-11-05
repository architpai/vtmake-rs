//! MVT encoding with the same bytes as tippecanoe (`mvt.cpp`).
//!
//! Spec: `docs/spec/tippecanoe-plain-path.md`, sections 3.4 (value types),
//! 8.3 to 8.5 (layers, key/value tables, protobuf layout) and 9.1 (gzip).

pub mod gzip;
pub mod pbf;
pub mod value;

pub use value::{MvtValue, stringified_to_mvt_value};

use vtmake_core::{GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};

/// fnv1a 64-bit (`text.cpp:fnv1a`).
#[inline]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// fnv1a over a C string plus one extra byte (`fnv1a(const char *, char)`).
#[inline]
pub(crate) fn fnv1a_cstr(bytes: &[u8], additional: u8) -> u64 {
    let mut h = fnv1a(bytes);
    h ^= additional as u64;
    h.wrapping_mul(1099511628211)
}

/// One point of an output geometry, in tile units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MvtPoint {
    pub op: u8,
    pub x: i64,
    pub y: i64,
}

/// `geom_type` of a feature without a type field (tippecanoe type -1, as
/// `tile-join --exclude-all-tile-geometries` writes).
pub const NO_GEOM_TYPE: u8 = u8::MAX;

#[derive(Clone, Debug, Default)]
pub struct MvtFeature {
    /// Pairs of (key index, value index) into the layer tables.
    pub tags: Vec<u32>,
    pub geometry: Vec<MvtPoint>,
    pub geom_type: u8,
    pub id: Option<u64>,
}

const DEDUP: usize = 65536;

/// A 65536-slot dedup table (`key_dedup` / `value_dedup`). A slot holds
/// (generation << 32 | index). A slot of an older generation is empty, so a
/// table is used again without a clear.
struct Dedup {
    slots: Box<[u64]>,
    generation: u64,
}

impl Dedup {
    fn take() -> Dedup {
        let mut d = POOL.with_borrow_mut(|p| p.pop()).unwrap_or_else(|| Dedup { slots: vec![0; DEDUP].into(), generation: 0 });
        d.generation += 1;
        if d.generation >= u32::MAX as u64 {
            d.slots.fill(0);
            d.generation = 1;
        }
        d
    }
    #[inline]
    fn get(&self, h: usize) -> i64 {
        let s = self.slots[h];
        if s >> 32 == self.generation { (s & 0xFFFF_FFFF) as i64 } else { -1 }
    }
    #[inline]
    fn set(&mut self, h: usize, i: u32) {
        self.slots[h] = (self.generation << 32) | i as u64;
    }
}

thread_local! {
    static POOL: std::cell::RefCell<Vec<Dedup>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A layer under construction (`mvt_layer`).
pub struct MvtLayer {
    pub name: String,
    pub version: u32,
    pub extent: u32,
    pub features: Vec<MvtFeature>,
    pub keys: Vec<Box<str>>,
    pub values: Vec<MvtValue>,
    key_dedup: Option<Dedup>,
    value_dedup: Option<Dedup>,
}

impl Drop for MvtLayer {
    fn drop(&mut self) {
        POOL.with_borrow_mut(|p| {
            p.extend(self.key_dedup.take());
            p.extend(self.value_dedup.take());
        });
    }
}

impl MvtLayer {
    pub fn new(name: impl Into<String>) -> Self {
        MvtLayer {
            name: name.into(),
            version: 2,
            extent: 4096,
            features: Vec::new(),
            keys: Vec::new(),
            values: Vec::new(),
            key_dedup: Some(Dedup::take()),
            value_dedup: Some(Dedup::take()),
        }
    }

    /// `tag` for a string value, without a temporary `MvtValue`.
    pub fn tag_str(&mut self, feature: &mut MvtFeature, key: &str, value: &str) {
        let ki = self.key_index(key);
        feature.tags.push(ki);
        let vd = self.value_dedup.as_mut().unwrap();
        let b = value.as_bytes();
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        let vh = (fnv1a_cstr(&b[..end], 0) % DEDUP as u64) as usize;
        let vi = vd.get(vh);
        let same = vi >= 0 && matches!(&self.values[vi as usize], MvtValue::String(s) if &**s == value);
        let vi = if same {
            vi as u32
        } else {
            let n = self.values.len() as u32;
            vd.set(vh, n);
            self.values.push(MvtValue::String(value.into()));
            n
        };
        feature.tags.push(vi);
    }

    fn key_index(&mut self, key: &str) -> u32 {
        let kd = self.key_dedup.as_mut().unwrap();
        let kh = (fnv1a(key.as_bytes()) % DEDUP as u64) as usize;
        let ki = kd.get(kh);
        if ki >= 0 && &*self.keys[ki as usize] == key {
            ki as u32
        } else {
            let n = self.keys.len() as u32;
            kd.set(kh, n);
            self.keys.push(key.into());
            n
        }
    }

    /// Add a key/value pair to `feature` (`mvt_layer::tag`). A hash collision with a
    /// different key adds the key again, as tippecanoe does.
    pub fn tag(&mut self, feature: &mut MvtFeature, key: &str, value: &MvtValue) {
        let ki = self.key_index(key);
        feature.tags.push(ki);

        let vd = self.value_dedup.as_mut().unwrap();
        let vh = (value.tippecanoe_hash() % DEDUP as u64) as usize;
        let vi = vd.get(vh);
        let vi = if vi >= 0 && self.values[vi as usize].same(value) {
            vi
        } else {
            let n = self.values.len() as u32;
            vd.set(vh, n);
            self.values.push(value.clone());
            n as i64
        };
        feature.tags.push(vi as u32);
    }
}

#[derive(Default)]
pub struct MvtTile {
    pub layers: Vec<MvtLayer>,
}

impl MvtTile {
    /// Encode the tile (`mvt_tile::encode`). Appends to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut layer_buf = Vec::new();
        let mut feat_buf = Vec::new();
        let mut packed = Vec::new();
        let mut geom: Vec<u32> = Vec::new();
        let mut encoded_values: Vec<(Vec<u8>, usize)> = Vec::new();
        let mut mapping: Vec<u32> = Vec::new();
        let mut tags: Vec<u32> = Vec::new();

        for layer in &self.layers {
            layer_buf.clear();
            pbf::add_varint_field(&mut layer_buf, 15, layer.version as u64);
            pbf::add_bytes_field(&mut layer_buf, 1, layer.name.as_bytes());
            pbf::add_varint_field(&mut layer_buf, 5, layer.extent as u64);
            for k in &layer.keys {
                pbf::add_bytes_field(&mut layer_buf, 3, k.as_bytes());
            }

            encoded_values.clear();
            for (i, v) in layer.values.iter().enumerate() {
                let mut b = Vec::with_capacity(16);
                v.encode(&mut b);
                encoded_values.push((b, i));
            }
            // std::stable_sort by the encoded bytes.
            encoded_values.sort_by(|a, b| a.0.cmp(&b.0));
            mapping.clear();
            mapping.resize(encoded_values.len(), 0);
            let mut vi = 0u32;
            let mut v = 0;
            while v < encoded_values.len() {
                mapping[encoded_values[v].1] = vi;
                pbf::add_bytes_field(&mut layer_buf, 4, &encoded_values[v].0);
                while v + 1 < encoded_values.len() && encoded_values[v].0 == encoded_values[v + 1].0 {
                    mapping[encoded_values[v + 1].1] = vi;
                    v += 1;
                }
                vi += 1;
                v += 1;
            }

            for f in &layer.features {
                feat_buf.clear();
                if f.geom_type != NO_GEOM_TYPE {
                    pbf::add_varint_field(&mut feat_buf, 3, f.geom_type as u64);
                }

                tags.clear();
                tags.extend_from_slice(&f.tags);
                for t in tags.iter_mut().skip(1).step_by(2) {
                    *t = mapping[*t as usize];
                }
                pbf::add_packed_u32(&mut feat_buf, 2, &tags, &mut packed);

                if let Some(id) = f.id {
                    pbf::add_varint_field(&mut feat_buf, 1, id);
                }

                encode_geometry(&f.geometry, &mut geom);
                pbf::add_packed_u32(&mut feat_buf, 4, &geom, &mut packed);
                pbf::add_bytes_field(&mut layer_buf, 2, &feat_buf);
            }
            pbf::add_bytes_field(out, 3, &layer_buf);
        }
    }
}

#[inline]
fn zigzag32(v: i64) -> u32 {
    let v = v as i32;
    ((v << 1) ^ (v >> 31)) as u32
}

/// MVT command stream with delta and zigzag coding. Runs of one op share a header.
pub fn encode_geometry(points: &[MvtPoint], out: &mut Vec<u32>) {
    out.clear();
    let (mut px, mut py) = (0i64, 0i64);
    let mut cmd_idx: Option<usize> = None;
    let mut cmd: i32 = -1;
    let mut length: u32 = 0;
    for p in points {
        let op = p.op as i32;
        if op != cmd {
            if let Some(i) = cmd_idx {
                out[i] = (length << 3) | (cmd as u32 & 7);
            }
            cmd = op;
            length = 0;
            cmd_idx = Some(out.len());
            out.push(0);
        }
        if p.op == VT_MOVETO || p.op == VT_LINETO {
            let dx = p.x - px;
            let dy = p.y - py;
            assert!(
                (i32::MIN as i64..=i32::MAX as i64).contains(&dx) && (i32::MIN as i64..=i32::MAX as i64).contains(&dy),
                "geometry delta is too big: {dx},{dy}"
            );
            out.push(zigzag32(dx));
            out.push(zigzag32(dy));
            px = p.x;
            py = p.y;
            length += 1;
        } else if p.op == VT_CLOSEPATH {
            length += 1;
        } else {
            panic!("corrupted geometry: op {}", p.op);
        }
    }
    if let Some(i) = cmd_idx {
        out[i] = (length << 3) | (cmd as u32 & 7);
    }
}

pub fn geom_type_code(t: GeomType) -> u8 {
    t as u8
}

pub mod decode;
