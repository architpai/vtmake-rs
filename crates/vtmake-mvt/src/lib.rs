//! MVT encoding with the same bytes as tippecanoe (`mvt.cpp`).
//!
//! Spec: `docs/spec/tippecanoe-plain-path.md`, sections 3.4 (value types),
//! 8.3 to 8.5 (layers, key/value tables, protobuf layout) and 9.1 (gzip).

pub mod gzip;
pub mod pbf;
pub mod value;

pub use value::{MvtValue, stringified_fast, stringified_to_mvt_value};

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

/// The dedup slot of a key (`fnv1a(key) % 65536` in `mvt_layer::tag`).
#[inline]
pub fn key_slot(key: &str) -> u16 {
    fnv1a(key.as_bytes()) as u16
}

/// The dedup slot of a string value (`std::hash<mvt_value>`: the C string,
/// so the bytes up to the first NUL, plus a 0 byte).
#[inline]
pub fn str_slot(value: &[u8]) -> u16 {
    let end = value.iter().position(|&c| c == 0).unwrap_or(value.len());
    fnv1a_cstr(&value[..end], 0) as u16
}

/// A key or a value from a feature store: a source id and the precomputed
/// dedup slot (`key_slot`, or `MvtValue::slot` of the value).
///
/// The caller makes sure that, in one layer, one `id` always means the same
/// key text, or the same value (type and text). The id must not be 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Src {
    pub id: u64,
    pub slot: u16,
}

/// A 65536-slot dedup table (`key_dedup` / `value_dedup`). A slot holds
/// index + 1 of the last key or value with that slot, or 0. When the layer is
/// dropped, it clears only the slots that it set, and gives the table back to
/// a thread-local pool. Thus a small tile does not clear 65536 slots.
struct Dedup {
    slots: Box<[u32]>,
}

impl Dedup {
    fn take() -> Dedup {
        POOL.with_borrow_mut(|p| p.pop()).unwrap_or_else(|| Dedup { slots: vec![0; DEDUP].into() })
    }
    #[inline]
    fn get(&self, h: u16) -> Option<usize> {
        let s = self.slots[h as usize];
        (s != 0).then(|| (s - 1) as usize)
    }
    #[inline]
    fn set(&mut self, h: u16, i: usize) {
        self.slots[h as usize] = i as u32 + 1;
    }
    fn give_back(mut self, used: &[u16]) {
        for &h in used {
            self.slots[h as usize] = 0;
        }
        POOL.with_borrow_mut(|p| p.push(self));
    }
}

thread_local! {
    static POOL: std::cell::RefCell<Vec<Dedup>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A layer under construction (`mvt_layer`).
///
/// Do not change `keys` and `values` directly: the dedup tables refer to them.
pub struct MvtLayer {
    pub name: String,
    pub version: u32,
    pub extent: u32,
    pub features: Vec<MvtFeature>,
    pub keys: Vec<Box<str>>,
    pub values: Vec<MvtValue>,
    /// The slot and the source id (0: none) of each key and value.
    key_slot: Vec<u16>,
    key_src: Vec<u64>,
    value_slot: Vec<u16>,
    value_src: Vec<u64>,
    key_dedup: Option<Dedup>,
    value_dedup: Option<Dedup>,
}

impl Drop for MvtLayer {
    fn drop(&mut self) {
        if let Some(d) = self.key_dedup.take() {
            d.give_back(&self.key_slot);
        }
        if let Some(d) = self.value_dedup.take() {
            d.give_back(&self.value_slot);
        }
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
            key_slot: Vec::new(),
            key_src: Vec::new(),
            value_slot: Vec::new(),
            value_src: Vec::new(),
            key_dedup: Some(Dedup::take()),
            value_dedup: Some(Dedup::take()),
        }
    }

    /// Add a key with its slot and source id; returns its index. A collision
    /// overwrites the slot, as tippecanoe does.
    #[inline]
    fn push_key(&mut self, key: &str, slot: u16, src: u64) -> u32 {
        let n = self.keys.len();
        self.key_dedup.as_mut().unwrap().set(slot, n);
        self.keys.push(key.into());
        self.key_slot.push(slot);
        self.key_src.push(src);
        n as u32
    }

    #[inline]
    fn push_value(&mut self, value: MvtValue, slot: u16, src: u64) -> u32 {
        let n = self.values.len();
        self.value_dedup.as_mut().unwrap().set(slot, n);
        // A NaN is not equal to itself: no source id, so that it is never the same.
        let src = if value.is_nan() { 0 } else { src };
        self.values.push(value);
        self.value_slot.push(slot);
        self.value_src.push(src);
        n as u32
    }

    /// `tag` for a string value, without a temporary `MvtValue`.
    pub fn tag_str(&mut self, feature: &mut MvtFeature, key: &str, value: &str) {
        let ki = self.key_index(key);
        feature.tags.push(ki);
        let slot = str_slot(value.as_bytes());
        let vi = match self.value_dedup.as_ref().unwrap().get(slot) {
            Some(i) if matches!(&self.values[i], MvtValue::String(s) if &**s == value) => i as u32,
            _ => self.push_value(MvtValue::String(value.into()), slot, 0),
        };
        feature.tags.push(vi);
    }

    fn key_index(&mut self, key: &str) -> u32 {
        let slot = key_slot(key);
        match self.key_dedup.as_ref().unwrap().get(slot) {
            Some(i) if &*self.keys[i] == key => i as u32,
            _ => self.push_key(key, slot, 0),
        }
    }

    /// `key_index` for a key with a known slot and source id.
    #[inline]
    fn key_index_src(&mut self, key: &str, ks: Src) -> u32 {
        match self.key_dedup.as_ref().unwrap().get(ks.slot) {
            Some(i) if self.key_src[i] == ks.id || &*self.keys[i] == key => i as u32,
            _ => self.push_key(key, ks.slot, ks.id),
        }
    }

    /// Add a key/value pair to `feature` (`mvt_layer::tag`). A hash collision with a
    /// different key adds the key again, as tippecanoe does.
    pub fn tag(&mut self, feature: &mut MvtFeature, key: &str, value: &MvtValue) {
        let ki = self.key_index(key);
        feature.tags.push(ki);
        let slot = value.slot();
        let vi = match self.value_dedup.as_ref().unwrap().get(slot) {
            Some(i) if self.values[i].same(value) => i as u32,
            _ => self.push_value(value.clone(), slot, 0),
        };
        feature.tags.push(vi);
    }

    /// `tag_str` for a key and a string value from a feature store. The same
    /// result as `tag_str`, but with the precomputed slots, and a same source
    /// id is a match without a text comparison.
    #[inline]
    pub fn tag_str_src(&mut self, feature: &mut MvtFeature, key: &str, ks: Src, value: &str, vs: Src) {
        let ki = self.key_index_src(key, ks);
        feature.tags.push(ki);
        let vi = match self.value_dedup.as_ref().unwrap().get(vs.slot) {
            Some(i) if self.value_src[i] == vs.id || matches!(&self.values[i], MvtValue::String(s) if &**s == value) => i as u32,
            _ => self.push_value(MvtValue::String(value.into()), vs.slot, vs.id),
        };
        feature.tags.push(vi);
    }

    /// `tag` for a key and a value from a feature store. `value` makes the
    /// value; it is called only when the source id does not match.
    #[inline]
    pub fn tag_src(&mut self, feature: &mut MvtFeature, key: &str, ks: Src, vs: Src, value: impl FnOnce() -> MvtValue) {
        let ki = self.key_index_src(key, ks);
        feature.tags.push(ki);
        let found = self.value_dedup.as_ref().unwrap().get(vs.slot);
        let vi = match found {
            Some(i) if self.value_src[i] == vs.id => i as u32,
            _ => {
                let v = value();
                match found {
                    Some(i) if self.values[i].same(&v) => i as u32,
                    _ => self.push_value(v, vs.slot, vs.id),
                }
            }
        };
        feature.tags.push(vi);
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
        // The encoded values, (offset, length) in `enc`, and (sort prefix, index).
        let mut enc: Vec<u8> = Vec::new();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut sorted: Vec<((u64, u64), u32)> = Vec::new();
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

            // std::stable_sort of the values by their encoded bytes: an unstable
            // sort with the index as the last key gives the same order.
            enc.clear();
            spans.clear();
            sorted.clear();
            for (i, v) in layer.values.iter().enumerate() {
                let off = enc.len();
                v.encode(&mut enc);
                spans.push((off, enc.len() - off));
                sorted.push((prefix(&enc[off..]), i as u32));
            }
            let bytes = |i: u32| {
                let (o, l) = spans[i as usize];
                &enc[o..o + l]
            };
            sorted.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| bytes(a.1).cmp(bytes(b.1))));
            mapping.clear();
            mapping.resize(sorted.len(), 0);
            let mut vi = 0u32;
            let mut v = 0;
            while v < sorted.len() {
                mapping[sorted[v].1 as usize] = vi;
                pbf::add_bytes_field(&mut layer_buf, 4, bytes(sorted[v].1));
                while v + 1 < sorted.len() && sorted[v].0 == sorted[v + 1].0 && bytes(sorted[v].1) == bytes(sorted[v + 1].1) {
                    mapping[sorted[v + 1].1 as usize] = vi;
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

/// The first 16 bytes of `b` as two big-endian numbers, with 0 bytes after
/// the end. If two prefixes are not equal, they have the same order as the
/// full byte strings.
#[inline]
fn prefix(b: &[u8]) -> (u64, u64) {
    let mut p = [0u8; 16];
    let n = b.len().min(16);
    p[..n].copy_from_slice(&b[..n]);
    let (hi, lo) = p.split_at(8);
    (u64::from_be_bytes(hi.try_into().unwrap()), u64::from_be_bytes(lo.try_into().unwrap()))
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
