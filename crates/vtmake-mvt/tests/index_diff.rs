//! Differential test of the layer key/value index and the value sort in
//! `MvtTile::encode`: the old code (copied below) and the new code must give
//! the same tile bytes. The input has forced slot collisions, NUL bytes,
//! equal text with other types, NaN and -0.0, and values from two sources
//! (like two chunks) with the same text.

use vtmake_core::AttrType;
use vtmake_mvt::{MvtFeature, MvtLayer, MvtPoint, MvtTile, MvtValue, Src, pbf, stringified_fast, stringified_to_mvt_value};

// ---- The old code (vtmake-mvt before the source ids) ----

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

fn old_hash(v: &MvtValue) -> u64 {
    match v {
        MvtValue::String(s) => {
            let b = s.as_bytes();
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            (fnv1a(&b[..end]) ^ 0).wrapping_mul(1099511628211)
        }
        MvtValue::Float(f) => fnv1a(&f.to_le_bytes()),
        MvtValue::Double(d) => fnv1a(&d.to_le_bytes()),
        MvtValue::Int(i) | MvtValue::Sint(i) => fnv1a(&i.to_le_bytes()),
        MvtValue::Uint(u) => fnv1a(&u.to_le_bytes()),
        MvtValue::Bool(b) => fnv1a(&[*b as u8]),
    }
}

struct OldLayer {
    keys: Vec<Box<str>>,
    values: Vec<MvtValue>,
    features: Vec<MvtFeature>,
    key_dedup: Vec<i64>,
    value_dedup: Vec<i64>,
}

impl OldLayer {
    fn new() -> Self {
        OldLayer { keys: vec![], values: vec![], features: vec![], key_dedup: vec![-1; 65536], value_dedup: vec![-1; 65536] }
    }
    fn key_index(&mut self, key: &str) -> u32 {
        let kh = (fnv1a(key.as_bytes()) % 65536) as usize;
        let ki = self.key_dedup[kh];
        if ki >= 0 && &*self.keys[ki as usize] == key {
            ki as u32
        } else {
            let n = self.keys.len() as u32;
            self.key_dedup[kh] = n as i64;
            self.keys.push(key.into());
            n
        }
    }
    fn tag(&mut self, f: &mut MvtFeature, key: &str, value: &MvtValue) {
        let ki = self.key_index(key);
        f.tags.push(ki);
        let vh = (old_hash(value) % 65536) as usize;
        let vi = self.value_dedup[vh];
        let vi = if vi >= 0 && self.values[vi as usize].same(value) {
            vi as u32
        } else {
            let n = self.values.len() as u32;
            self.value_dedup[vh] = n as i64;
            self.values.push(value.clone());
            n
        };
        f.tags.push(vi);
    }
}

fn old_encode(layers: &[(&str, &OldLayer)], out: &mut Vec<u8>) {
    for (name, layer) in layers {
        let mut layer_buf = Vec::new();
        pbf::add_varint_field(&mut layer_buf, 15, 2);
        pbf::add_bytes_field(&mut layer_buf, 1, name.as_bytes());
        pbf::add_varint_field(&mut layer_buf, 5, 4096);
        for k in &layer.keys {
            pbf::add_bytes_field(&mut layer_buf, 3, k.as_bytes());
        }
        let mut encoded_values: Vec<(Vec<u8>, usize)> = Vec::new();
        for (i, v) in layer.values.iter().enumerate() {
            let mut b = Vec::new();
            v.encode(&mut b);
            encoded_values.push((b, i));
        }
        encoded_values.sort_by(|a, b| a.0.cmp(&b.0));
        let mut mapping = vec![0u32; encoded_values.len()];
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
        let mut packed = Vec::new();
        let mut geom = Vec::new();
        for f in &layer.features {
            let mut feat_buf = Vec::new();
            pbf::add_varint_field(&mut feat_buf, 3, f.geom_type as u64);
            let mut tags = f.tags.clone();
            for t in tags.iter_mut().skip(1).step_by(2) {
                *t = mapping[*t as usize];
            }
            pbf::add_packed_u32(&mut feat_buf, 2, &tags, &mut packed);
            vtmake_mvt::encode_geometry(&f.geometry, &mut geom);
            pbf::add_packed_u32(&mut feat_buf, 4, &geom, &mut packed);
            pbf::add_bytes_field(&mut layer_buf, 2, &feat_buf);
        }
        pbf::add_bytes_field(out, 3, &layer_buf);
    }
}

// ---- Input ----

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A store entry: a key or a value text with its type, in one source.
#[derive(Clone)]
struct Entry {
    ty: AttrType,
    text: String,
    src: u64,
}

fn str_slot(s: &str) -> u16 {
    (old_hash(&MvtValue::String(s.into())) % 65536) as u16
}

/// Strings with the same value slot (and keys with the same key slot).
fn colliding(rng: &mut Rng, value: bool, n: usize) -> Vec<String> {
    let mut seen = std::collections::HashMap::<u16, String>::new();
    let mut out = Vec::new();
    while out.len() < n {
        let s = format!("c{:x}", rng.next() & 0xffffff);
        let h = if value { str_slot(&s) } else { (fnv1a(s.as_bytes()) % 65536) as u16 };
        if let Some(o) = seen.get(&h) {
            if *o != s {
                out.push(o.clone());
                out.push(s.clone());
            }
        } else {
            seen.insert(h, s);
        }
    }
    out
}

/// A number text whose value has the same slot as the string `s`.
fn number_like(s: &str) -> String {
    let h = str_slot(s);
    (0i64..).find(|&i| (old_hash(&MvtValue::Int(i)) % 65536) as u16 == h).unwrap().to_string()
}

fn texts(rng: &mut Rng) -> Vec<(AttrType, String)> {
    let mut t: Vec<(AttrType, String)> = Vec::new();
    for s in ["", "a", "b", "1", "1.0", "-0", "0", "-0.0", "0.0", "-0e0", "0e0", "true", "false", "nan", "NaN", "inf", "a\0b", "a\0c", "a", "\0", "1e400", "08", "0x1F",
        "18446744073709551615", "-9223372036854775808", "0.5", "3.000", "x".repeat(200).as_str(), "x".repeat(201).as_str()] {
        for ty in [AttrType::String, AttrType::Number, AttrType::Bool] {
            t.push((ty, s.to_string()));
        }
    }
    let c = colliding(rng, true, 12);
    for s in &c {
        t.push((AttrType::Number, number_like(s)));
        t.push((AttrType::String, s.clone()));
        // The same C string (and slot), another value.
        t.push((AttrType::String, format!("{s}\0tail")));
    }
    for _ in 0..40 {
        t.push((AttrType::String, format!("r{}", rng.below(30))));
        t.push((AttrType::Number, format!("{}", rng.below(30) as f64 / 4.0)));
    }
    t
}

/// The value as the old tile code makes it (`tag_str` for a string).
fn value(e: &Entry) -> Option<MvtValue> {
    stringified_to_mvt_value(e.ty, &e.text)
}

fn run_case(seed: u64) {
    let mut rng = Rng(seed | 1);
    let mut keys: Vec<String> = ["k", "name", "id", "", "k\0x"].iter().map(|s| s.to_string()).collect();
    keys.extend(colliding(&mut rng, false, 6));
    let texts = texts(&mut rng);
    // Two sources ("chunks"): the same texts with other source ids.
    let mut entries = Vec::new();
    let mut key_entries = Vec::new();
    for chunk in 1..=2u64 {
        for (i, (ty, text)) in texts.iter().enumerate() {
            entries.push(Entry { ty: *ty, text: text.clone(), src: chunk << 32 | i as u64 });
        }
        for (i, k) in keys.iter().enumerate() {
            key_entries.push(Entry { ty: AttrType::String, text: k.clone(), src: chunk << 32 | i as u64 });
        }
    }

    let nlayers = 1 + rng.below(2);
    let mut old: Vec<OldLayer> = (0..nlayers).map(|_| OldLayer::new()).collect();
    let mut new: Vec<MvtLayer> = (0..nlayers).map(|i| MvtLayer::new(format!("l{i}"))).collect();
    let nfeatures = 1 + rng.below(60);
    for fi in 0..nfeatures {
        let li = rng.below(nlayers);
        let mut of = MvtFeature { geom_type: 1, ..Default::default() };
        let mut nf = MvtFeature { geom_type: 1, ..Default::default() };
        of.geometry.push(MvtPoint { op: 1, x: fi as i64, y: 0 });
        nf.geometry = of.geometry.clone();
        for _ in 0..rng.below(12) {
            let k = &key_entries[rng.below(key_entries.len())];
            let e = &entries[rng.below(entries.len())];
            let Some(v) = value(e) else { continue };
            old[li].tag(&mut of, &k.text, &v);
            let ks = Src { id: k.src, slot: vtmake_mvt::key_slot(&k.text) };
            let l = &mut new[li];
            // The paths of the tile code, and the plain ones.
            match (rng.below(4), e.ty) {
                (0, _) => l.tag(&mut nf, &k.text, &v),
                (1, AttrType::String) => l.tag_str(&mut nf, &k.text, &e.text),
                (_, AttrType::String) => l.tag_str_src(&mut nf, &k.text, ks, &e.text, Src { id: e.src, slot: vtmake_mvt::str_slot(e.text.as_bytes()) }),
                (_, ty) => match stringified_fast(ty, &e.text) {
                    Some(fast) if !fast.is_nan() => {
                        let vs = Src { id: e.src, slot: fast.slot() };
                        l.tag_src(&mut nf, &k.text, ks, vs, || stringified_fast(ty, &e.text).unwrap())
                    }
                    _ => l.tag(&mut nf, &k.text, &v),
                },
            }
        }
        old[li].features.push(of);
        new[li].features.push(nf);
    }
    let names: Vec<String> = (0..nlayers).map(|i| format!("l{i}")).collect();
    let mut a = Vec::new();
    old_encode(&names.iter().map(String::as_str).zip(old.iter()).collect::<Vec<_>>(), &mut a);
    let mut b = Vec::new();
    MvtTile { layers: new }.encode(&mut b);
    assert!(a == b, "seed {seed}: the tile bytes are different");
}

#[test]
fn index_and_sort_match_the_old_code() {
    for seed in 0..3000u64 {
        run_case(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ 0x1234_5678);
    }
}

/// Many values in one layer, with long common prefixes (the sort prefix is
/// 16 bytes) and duplicates from slot collisions.
#[test]
fn large_layer_sort_matches() {
    let mut rng = Rng(77);
    let c = colliding(&mut rng, true, 200);
    let mut old = OldLayer::new();
    let mut new = MvtLayer::new("l");
    for i in 0..20000 {
        let mut of = MvtFeature { geom_type: 1, ..Default::default() };
        let mut nf = MvtFeature { geom_type: 1, ..Default::default() };
        let texts = [format!("[139.6876266{}, 35.5{}]", rng.below(1000), rng.below(100)), c[rng.below(c.len())].clone(), format!("{}", rng.below(5000))];
        for (j, t) in texts.iter().enumerate() {
            let ty = if j == 2 { AttrType::Number } else { AttrType::String };
            let v = stringified_to_mvt_value(ty, t).unwrap();
            old.tag(&mut of, "k", &v);
            let src = (i as u64 % 3 + 1) << 32 | (i as u64 * 3 + j as u64);
            new.tag_src(&mut nf, "k", Src { id: 1 << 32, slot: vtmake_mvt::key_slot("k") }, Src { id: src, slot: v.slot() }, || v.clone());
        }
        old.features.push(of);
        new.features.push(nf);
    }
    let mut a = Vec::new();
    old_encode(&[("l", &old)], &mut a);
    let mut b = Vec::new();
    MvtTile { layers: vec![new] }.encode(&mut b);
    assert!(a == b);
}
