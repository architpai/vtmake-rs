//! Attribute options.
//!
//! When it reads a feature (`serial.cpp:serialize_feature`), tippecanoe applies:
//! `--set-attribute`, then for each attribute from the last to the first: `-T`
//! (`coerce_value`), `-pN`, `--use-attribute-for-id`, `-X`/`-y` and `-x`. The
//! tile statistics come from the result. Then `--maximum-string-attribute-length`
//! cuts the strings (the statistics keep the full strings). `--generate-ids`
//! (`-ai`) gives a feature without an ID the ID `seq + 1`, where `seq` is
//! tippecanoe's sequence number of the feature (see [`generate_ids`]).
//!
//! In a tile (`tile.cpp:write_tile`), the filter `-j` (crate::filter) decides
//! if a feature is in the tile, and `-E` adds the attributes of a dropped
//! feature to the last kept feature of the same layer and type
//! (`preserve_attributes`). See [`TileState`].
//!
//! The settings are global ([`init`]): one `vtmake build` per process. Without
//! attribute options, [`rules`] and [`accum`] give `None`, and the readers and
//! the tiler use their usual paths.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::cli::{Accum, Options, TypeOpt};
use crate::filter::{Filter, Value};
use crate::store::{Chunk, FeatMeta};
use crate::tile::Ctx;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use vtmake_core::{AttrType, GeomType};
use vtmake_mvt::{MvtLayer, MvtValue, stringified_to_mvt_value};
use vtmake_parse::dtoa::dtoa_milo;
use vtmake_parse::pull::atof;

/// tippecanoe exit code for an internal error (`errors.hpp`).
const EXIT_IMPOSSIBLE: i32 = 106;

/// One attribute while the rules change it: key, type, text.
pub type Owned = (Box<str>, AttrType, String);

/// The read-time rules.
pub struct Rules {
    /// `--set-attribute`, in key order (C++ `std::map`).
    set: Vec<(Box<str>, AttrType, Box<str>)>,
    types: HashMap<Box<str>, TypeOpt>,
    single_precision: bool,
    id_key: Option<Box<str>>,
    convert_ids: bool,
    exclude_all: bool,
    include: HashSet<Box<str>>,
    exclude: HashSet<Box<str>>,
    /// `--maximum-string-attribute-length`, 0: no limit.
    pub max_len: usize,
    pub generate_ids: bool,
    /// Some rule changes the attributes or the ID ([`Rules::apply`]).
    pub changes_attrs: bool,
}

struct Global {
    rules: Option<Rules>,
    accum: Option<HashMap<Box<str>, Accum>>,
    /// The prefilter writes the sequence numbers ([`generate_ids`]).
    seqs: bool,
}

static GLOBAL: OnceLock<Global> = OnceLock::new();

/// Set the attribute options from the command line. Call once, before reading.
pub fn init(o: &Options) {
    let rules = Rules {
        set: o.set_attributes.iter().map(|(k, v)| (k.as_str().into(), v.ty, v.text.as_str().into())).collect(),
        types: o.attribute_types.iter().map(|(k, &t)| (k.as_str().into(), t)).collect(),
        single_precision: o.prevent[b'N' as usize],
        id_key: o.attribute_for_id.as_deref().map(Into::into),
        convert_ids: o.additional[b'I' as usize],
        exclude_all: o.exclude_all,
        include: o.include.iter().map(|k| k.as_str().into()).collect(),
        exclude: o.exclude.iter().map(|k| k.as_str().into()).collect(),
        max_len: o.maximum_string_attribute_length.max(0) as usize,
        generate_ids: o.additional[b'i' as usize],
        changes_attrs: false,
    };
    let mut rules = rules;
    rules.changes_attrs = !rules.set.is_empty()
        || !rules.types.is_empty()
        || rules.single_precision
        || rules.id_key.is_some()
        || rules.exclude_all
        || !rules.exclude.is_empty()
        || rules.max_len > 0;
    let active = rules.changes_attrs || rules.generate_ids;
    let accum = (!o.attribute_accum.is_empty()).then(|| o.attribute_accum.iter().map(|(k, &a)| (k.as_str().into(), a)).collect());
    let _ = GLOBAL.set(Global { rules: active.then_some(rules), accum, seqs: o.prefilter.is_some() });
}

/// The read-time rules, or `None` if the command line has none.
#[inline]
pub fn rules() -> Option<&'static Rules> {
    GLOBAL.get().and_then(|g| g.rules.as_ref())
}

/// The `-E` methods, or `None`.
#[inline]
pub fn accum() -> Option<&'static HashMap<Box<str>, Accum>> {
    GLOBAL.get().and_then(|g| g.accum.as_ref())
}

/// A warning that tippecanoe writes only once (a `static bool warned`).
fn warn_once(flag: &AtomicBool, msg: impl FnOnce() -> String) {
    if !flag.swap(true, Ordering::Relaxed) {
        eprintln!("{}", msg());
    }
}

/// glibc `strtoull(s, &end, 10)`: the value and the rest of the text after the number.
fn strtoull(s: &str) -> (u64, &str) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut v: u64 = 0;
    let mut overflow = false;
    while i < b.len() && b[i].is_ascii_digit() {
        match v.checked_mul(10).and_then(|x| x.checked_add((b[i] - b'0') as u64)) {
            Some(x) => v = x,
            None => overflow = true,
        }
        i += 1;
    }
    if i == start {
        // No digits: the end is the start of the text.
        return (0, s);
    }
    let v = if overflow {
        u64::MAX
    } else if neg {
        v.wrapping_neg()
    } else {
        v
    };
    (v, &s[i..])
}

/// `strip_zeroes`: remove the leading `0` characters.
fn strip_zeroes(s: &str) -> &str {
    s.trim_start_matches('0')
}

/// C `printf("%f")` (`std::to_string(double)`).
fn to_string_f(v: f64) -> String {
    if v.is_nan() {
        return if v.is_sign_negative() { "-nan" } else { "nan" }.into();
    }
    format!("{v:.6}")
}

/// `coerce_value` (`-T`).
fn coerce(t: TypeOpt, ty: &mut AttrType, val: &mut String) {
    match t {
        TypeOpt::String => *ty = AttrType::String,
        TypeOpt::Float => {
            *ty = AttrType::Number;
            *val = dtoa_milo(atof(val.as_bytes()));
        }
        TypeOpt::Int => {
            *ty = AttrType::Number;
            if val.is_empty() {
                *val = "0".into();
            }
            if !val.bytes().all(|c| c.is_ascii_digit()) {
                *val = to_string_f(atof(val.as_bytes()).round());
            }
        }
        TypeOpt::Bool => {
            let f = val == "false" || val == "0" || val == "null" || val.is_empty() || (*ty == AttrType::Number && atof(val.as_bytes()) == 0.0);
            *ty = AttrType::Bool;
            *val = if f { "false" } else { "true" }.into();
        }
    }
}

impl Rules {
    /// Apply the rules to the attributes and the ID of one feature (in place).
    pub fn apply(&self, attrs: &mut Vec<Owned>, id: &mut Option<u64>) {
        static WARN_NAN: AtomicBool = AtomicBool::new(false);
        static WARN_FRAC: AtomicBool = AtomicBool::new(false);
        static WARN_LARGE: AtomicBool = AtomicBool::new(false);
        for (k, ty, v) in &self.set {
            match attrs.iter_mut().find(|a| a.0 == *k) {
                Some(a) => {
                    a.1 = *ty;
                    a.2 = v.to_string();
                }
                None => attrs.push((k.clone(), *ty, v.to_string())),
            }
        }
        let mut i = attrs.len();
        while i > 0 {
            i -= 1;
            let a = &mut attrs[i];
            if let Some(&t) = self.types.get(&a.0) {
                coerce(t, &mut a.1, &mut a.2);
            }
            // Do not change integers to floats (tippecanoe's comment).
            if self.single_precision && a.1 == AttrType::Number && a.2.contains('.') {
                a.2 = dtoa_milo(atof(a.2.as_bytes()) as f32 as f64);
            }
            if self.id_key.as_deref() == Some(&*a.0) {
                if a.1 != AttrType::Number && !self.convert_ids {
                    warn_once(&WARN_NAN, || format!("Warning: Attribute \"{}\"=\"{}\" as feature ID is not a number", a.0, cstr(&a.2)));
                } else {
                    let (v, rest) = strtoull(&a.2);
                    if !rest.is_empty() {
                        warn_once(&WARN_FRAC, || format!("Warning: Can't represent non-integer feature ID {}", cstr(&a.2)));
                    } else if (v as i64).to_string() != strip_zeroes(&a.2) {
                        warn_once(&WARN_LARGE, || format!("Warning: Can't represent too-large feature ID {}", cstr(&a.2)));
                    } else {
                        *id = Some(v);
                        attrs.remove(i);
                        continue;
                    }
                }
            }
            let a = &attrs[i];
            if if self.exclude_all { !self.include.contains(&a.0) } else { self.exclude.contains(&a.0) } {
                attrs.remove(i);
            }
        }
    }

    /// `truncate_string`: cut a string value longer than `max_len` bytes at the
    /// start of a UTF-8 character at or before `max_len`.
    pub fn truncate<'a>(&self, ty: AttrType, s: &'a str) -> &'a str {
        if self.max_len == 0 || ty != AttrType::String || s.len() <= self.max_len {
            return s;
        }
        let b = s.as_bytes();
        let mut i = self.max_len;
        while i > 0 && b[i] & 0xC0 == 0x80 {
            i -= 1;
        }
        &s[..i]
    }
}

/// The text before a NUL byte (C `%s`).
fn cstr(s: &str) -> &str {
    s.split('\0').next().unwrap_or("")
}

// ---------------------------------------------------------------------------
// --generate-ids: tippecanoe's sequence numbers.

/// The next free sequence number (`overall_offset` in `main.cpp:read_input`).
static OVERALL: AtomicU64 = AtomicU64::new(0);

/// How tippecanoe numbers the features of one source.
pub enum SeqPlan {
    /// One reader: `overall_offset` + the feature number. After the source,
    /// `overall_offset` is the next number.
    Serial,
    /// `--read-parallel` (or RS input): stream `i` starts at the input position
    /// `starts[i]` (vtmake) and tippecanoe numbers its features from
    /// `overall_offset + bases[i]`. After the source, `overall_offset` grows
    /// by `len`.
    Parallel { starts: Vec<u64>, bases: Vec<u64>, len: u64 },
    /// FlatGeobuf (`flatgeobuf.cpp:fgbRunQueue`): batches of `cpus * 500 + 1`
    /// features (`queued` in all), each batch in `cpus` parts with their own
    /// numbers.
    Fgb { queued: u64, cpus: u64 },
}

/// Give each feature of one source without an ID the ID `seq + 1`
/// (`serial.cpp:serialize_feature`). `base` is the position base of the source
/// (vtmake's `seq` is `(base + position) << SEQ_SHIFT` plus a member number).
/// With a prefilter, also keep `seq` in `Chunk::felt_seqs`.
pub fn generate_ids(chunks: &mut [Chunk], base: u64, plan: &SeqPlan) {
    let ids = rules().is_some_and(|r| r.generate_ids);
    let seqs = GLOBAL.get().is_some_and(|g| g.seqs);
    if !ids && !seqs {
        return;
    }
    let shift = crate::read::SEQ_SHIFT;
    // All stored features in input order.
    let mut all: Vec<(u64, u32, u32)> = chunks
        .iter()
        .enumerate()
        .flat_map(|(ci, c)| c.seqs.iter().enumerate().map(move |(li, &s)| (s, ci as u32, li as u32)))
        .collect();
    all.sort_unstable();
    let overall = OVERALL.load(Ordering::Relaxed);
    let mut felt_seq = vec![0u64; all.len()];
    match plan {
        SeqPlan::Serial => {
            for (i, s) in felt_seq.iter_mut().enumerate() {
                *s = overall + i as u64;
            }
            OVERALL.store(overall + all.len() as u64, Ordering::Relaxed);
        }
        SeqPlan::Parallel { starts, bases, len } => {
            let mut stream = usize::MAX;
            let mut n = 0;
            for (i, &(s, _, _)) in all.iter().enumerate() {
                let p = (s >> shift) - base;
                let si = starts.partition_point(|&st| st <= p).saturating_sub(1);
                if si != stream {
                    stream = si;
                    n = 0;
                }
                felt_seq[i] = overall + bases[si] + n;
                n += 1;
            }
            OVERALL.store(overall + len, Ordering::Relaxed);
        }
        SeqPlan::Fgb { queued, cpus } => {
            // The feature number in the source is the position.
            let batch = cpus * 500 + 1;
            let mut seq0 = overall;
            let mut k = 0;
            let mut start = 0u64;
            while start < *queued {
                let size = batch.min(queued - start);
                let mut last_count = 0;
                for part in 0..*cpus {
                    let (a, b) = (start + size * part / cpus, start + size * (part + 1) / cpus);
                    let mut n = 0;
                    while k < all.len() && ((all[k].0 >> shift) - base) < b {
                        debug_assert!((all[k].0 >> shift) - base >= a);
                        felt_seq[k] = seq0 + size * part / cpus + n;
                        n += 1;
                        k += 1;
                    }
                    last_count = n;
                }
                seq0 += size * (cpus - 1) / cpus + last_count;
                start += size;
            }
            OVERALL.store(seq0, Ordering::Relaxed);
        }
    }
    if seqs {
        for c in chunks.iter_mut() {
            c.felt_seqs = vec![0; c.metas.len()];
        }
        for (i, &(_, ci, li)) in all.iter().enumerate() {
            chunks[ci as usize].felt_seqs[li as usize] = felt_seq[i];
        }
    }
    if !ids {
        return;
    }
    for (i, &(_, ci, li)) in all.iter().enumerate() {
        let c = &mut chunks[ci as usize];
        if c.metas[li as usize].id == u32::MAX {
            c.metas[li as usize].id = c.ids.len() as u32;
            c.ids.push(felt_seq[i] + 1);
        }
    }
}

// ---------------------------------------------------------------------------
// -E: accumulation in a tile.

/// `serial_val`: a type and a text. The text of an accumulated `mean` or
/// `count` has a NUL byte and the count after the value.
#[derive(Clone, Debug)]
pub struct SerialVal {
    pub ty: AttrType,
    pub s: String,
}

impl SerialVal {
    fn number(v: f64) -> Self {
        SerialVal { ty: AttrType::Number, s: dtoa_milo(v) }
    }
    fn to_double(&self) -> f64 {
        atof(cstr(&self.s).as_bytes())
    }
    fn count(&self) -> u64 {
        match self.s.find('\0') {
            // `atoll`.
            Some(i) => strtoull(&self.s[i + 1..]).0,
            None => 0,
        }
    }
    fn set_double_count(&mut self, v: f64, c: u64) {
        self.ty = AttrType::Number;
        self.s = format!("{}\0{c}", dtoa_milo(v));
    }
    fn set_string(&mut self, s: String) {
        self.ty = AttrType::String;
        self.s = s;
    }
}

/// The attributes of one kept feature in a tile: `keys` (in input order, from
/// the store) and `full` (the changed ones, in the order of change).
#[derive(Default)]
pub struct Acc {
    pub keys: Vec<(Box<str>, SerialVal)>,
    pub full: Vec<(Box<str>, SerialVal)>,
}

impl Acc {
    /// `promote_attribute`: move `key` from `keys` to the end of `full`.
    fn promote(&mut self, key: &str) {
        if let Some(i) = self.keys.iter().position(|k| &*k.0 == key) {
            let kv = self.keys.remove(i);
            self.full.push(kv);
        }
    }

    /// `preserve_attribute`.
    fn preserve(&mut self, op: Accum, key: &str, val: &SerialVal) {
        if let Some((_, f)) = self.full.iter_mut().find(|f| &*f.0 == key) {
            match op {
                Accum::Sum => *f = SerialVal::number(f.to_double() + val.to_double()),
                Accum::Product => *f = SerialVal::number(f.to_double() * val.to_double()),
                Accum::Max => {
                    if val.to_double() > f.to_double() {
                        *f = val.clone();
                    }
                }
                Accum::Min => {
                    if val.to_double() < f.to_double() {
                        *f = val.clone();
                    }
                }
                Accum::Mean => {
                    let count = f.count();
                    if count <= 1 {
                        let v = (f.to_double() + val.to_double()) / 2.0;
                        f.set_double_count(v, 2);
                    } else {
                        let sum = f.to_double() * count as f64 + val.to_double();
                        f.set_double_count(sum / (count + 1) as f64, count + 1);
                    }
                }
                Accum::Concat => f.set_string(format!("{}{}", f.s, val.s)),
                Accum::Comma => f.set_string(format!("{},{}", f.s, val.s)),
                Accum::Count => {
                    let count = f.count();
                    if count <= 1 {
                        f.set_double_count(2.0, 2);
                    } else {
                        f.set_double_count((count + 1) as f64, count + 1);
                    }
                }
            }
            return;
        }
        let mut v = val.clone();
        match op {
            Accum::Sum | Accum::Max | Accum::Min => {}
            Accum::Count => v.set_double_count(1.0, 1),
            Accum::Mean => v.set_double_count(val.to_double(), 1),
            _ => {
                eprintln!("can't happen: operation that isn't used by --accumulate-numeric-attributes");
                std::process::exit(EXIT_IMPOSSIBLE)
            }
        }
        self.full.push((key.into(), v));
    }
}

/// The attributes of a feature of a chunk, as tippecanoe has them in a tile:
/// without nulls, and without the keys in `excluded` (`attribute-filter`).
pub fn feature_attrs(c: &Chunk, m: &FeatMeta, excluded: &[Box<str>]) -> Vec<(Box<str>, SerialVal)> {
    c.attrs(m)
        .filter_map(|(k, v)| {
            let v = &c.vals[v as usize];
            let key = &c.keys[k as usize];
            (v.ty != AttrType::Null && !excluded.contains(key)).then(|| (key.clone(), SerialVal { ty: v.ty, s: c.val_text(v).to_string() }))
        })
        .collect()
}

/// The filter value of a stored value (`stringified_to_mvt_value`).
fn filter_value<'v>(ty: AttrType, s: &'v str) -> Value<'v> {
    match stringified_to_mvt_value(ty, s) {
        None => Value::Null,
        Some(MvtValue::String(_)) => Value::String(s),
        Some(MvtValue::Bool(b)) => Value::Bool(b),
        Some(MvtValue::Float(f)) => Value::Number(f as f64),
        Some(MvtValue::Double(d)) => Value::Number(d),
        Some(MvtValue::Int(i) | MvtValue::Sint(i)) => Value::Number(i as f64),
        Some(MvtValue::Uint(u)) => Value::Number(u as f64),
    }
}

/// The attribute work in one tile: the filter, `-E`, and the tags of the
/// features when they must wait for the end of the tile.
pub struct TileState<'c, 'a> {
    ctx: &'c Ctx<'a>,
    z: u32,
    filter: Option<&'static Filter>,
    ops: Option<&'static HashMap<Box<str>, Accum>>,
    /// The attributes that `attribute-filter` removes, by sort position.
    excluded: HashMap<u32, Vec<Box<str>>>,
    /// The kept features in tile order: the sort position, and the attributes
    /// after `-E` (made when a dropped feature first adds to it).
    kept: Vec<(u32, Option<Acc>)>,
    /// The last kept feature of each (layer, type): an index into `kept`.
    last: HashMap<(&'c str, GeomType), usize>,
    /// The features to tag at the end: (sort position, layer, index in the layer).
    deferred: Vec<(u32, &'c str, usize)>,
    /// A tile of the prefilter output (crate::plugin): all attributes are
    /// "full keys", and with a postfilter the attributes of each kept
    /// feature go to the layer statistics.
    sub: bool,
    stats: bool,
}

impl<'c, 'a> TileState<'c, 'a> {
    /// The state for tile zoom `z`, or `None` if there is no filter and no `-E`.
    #[inline]
    pub fn new(ctx: &'c Ctx<'a>, z: u32) -> Option<Self> {
        // The tiles of a prefilter have no `-j` (it ran before the filter).
        let (filter, ops) = (crate::filter::get().filter(|_| ctx.opts.filter.is_some()), accum());
        if filter.is_none() && ops.is_none() && ctx.opts.postfilter.is_none() {
            return None;
        }
        let sub = crate::plugin::is_sub(ctx);
        let stats = sub && ctx.opts.postfilter.is_some();
        if filter.is_none() && ops.is_none() && !stats {
            return None;
        }
        Some(TileState { ctx, z, filter, ops, excluded: HashMap::new(), kept: Vec::new(), last: HashMap::new(), deferred: Vec::new(), sub, stats })
    }

    /// The features wait for [`TileState::finish`] for their tags.
    pub fn defers_tags(&self) -> bool {
        self.ops.is_some() || self.filter.is_some_and(|f| f.removes_attributes)
    }

    /// The filter (`next_feature`): false if the feature is not in this tile.
    pub fn admit(&mut self, pos: u32) -> bool {
        let Some(f) = self.filter else { return true };
        let (c, m) = self.ctx.meta(pos);
        let z = self.z;
        let get = |key: &str| -> Value<'_> {
            for (k, v) in c.attrs(m) {
                if &*c.keys[k as usize] == key {
                    let v = &c.vals[v as usize];
                    return filter_value(v.ty, c.val_text(v));
                }
            }
            match key {
                "$id" => c.id(m).map_or(Value::NoKey, |id| Value::Number(id as f64)),
                "$type" => Value::String(match m.geom_type {
                    GeomType::Point => "Point",
                    GeomType::Line => "LineString",
                    GeomType::Polygon => "Polygon",
                }),
                "$zoom" => Value::Number(z as f64),
                _ => Value::NoKey,
            }
        };
        let mut excluded = Vec::new();
        if !f.evaluate(self.ctx.layer_name(pos), &get, &mut excluded) {
            return false;
        }
        if !excluded.is_empty() {
            self.excluded.insert(pos, excluded);
        }
        true
    }

    /// The attributes that the filter removes from the feature at `pos`.
    pub fn excluded(&self, pos: u32) -> &[Box<str>] {
        self.excluded.get(&pos).map_or(&[], Vec::as_slice)
    }

    fn attrs_of(&self, pos: u32) -> Vec<(Box<str>, SerialVal)> {
        let (c, m) = self.ctx.meta(pos);
        feature_attrs(c, m, self.excluded.get(&pos).map_or(&[], Vec::as_slice))
    }

    /// A feature is kept in the tile (`features.push_back`).
    pub fn keep(&mut self, pos: u32) {
        if self.ops.is_none() && !self.stats {
            return;
        }
        let t = self.ctx.meta(pos).1.geom_type;
        self.last.insert((self.ctx.layer_name(pos), t), self.kept.len());
        self.kept.push((pos, None));
    }

    /// A feature is dropped onto the last kept feature of its layer and type
    /// (`find_feature_to_accumulate_onto` and `preserve_attributes`).
    pub fn drop_onto(&mut self, pos: u32) {
        let Some(ops) = self.ops else { return };
        let t = self.ctx.meta(pos).1.geom_type;
        let Some(&i) = self.last.get(&(self.ctx.layer_name(pos), t)) else { return };
        let dropped = self.attrs_of(pos);
        if !dropped.iter().any(|(k, _)| ops.contains_key(k)) {
            return;
        }
        let target = self.kept[i].0;
        if self.kept[i].1.is_none() {
            // `promote_attribute` finds no key of a filter feature: it
            // changes its attributes in place.
            let attrs = self.attrs_of(target);
            self.kept[i].1 = Some(if self.sub { Acc { keys: Vec::new(), full: attrs } } else { Acc { keys: attrs, full: Vec::new() } });
        }
        let acc = self.kept[i].1.as_mut().unwrap();
        for (k, v) in &dropped {
            if let Some(&op) = ops.get(k) {
                acc.promote(k);
                acc.preserve(op, k, v);
            }
        }
    }

    /// Feature `index` of `layer` (sort position `pos`) gets its tags in
    /// [`TileState::finish`].
    pub fn defer(&mut self, pos: u32, layer: &'c str, index: usize) {
        self.deferred.push((pos, layer, index));
    }

    /// Keys moved after the multiplier marker by attribute accumulation.
    pub fn promoted_keys(&self) -> BTreeMap<u32, Vec<Box<str>>> {
        self.kept.iter().filter_map(|(pos, acc)| acc.as_ref().map(|acc| (*pos, acc.full.iter().map(|(key, _)| key.clone()).collect()))).collect()
    }

    /// Finish the tile: remove accumulation state, record changed values,
    /// and add the input attributes before the changed attributes.
    pub fn finish(mut self, layers: &mut BTreeMap<&'c str, MvtLayer>, generated: &[(&str, usize, u64, Option<i64>)]) {
        if self.stats {
            // `add_tilestats` of the full keys of each kept feature.
            for (pos, acc) in &self.kept {
                let attrs = match acc {
                    Some(acc) => acc.keys.iter().chain(&acc.full).cloned().collect(),
                    None => self.attrs_of(*pos),
                };
                crate::plugin::kept_stats(self.ctx.layer_name(*pos), &attrs);
            }
        }
        let mut accs: HashMap<u32, Acc> = HashMap::new();
        let mut stats = Vec::new();
        for (pos, acc) in std::mem::take(&mut self.kept) {
            let Some(mut acc) = acc else { continue };
            for (k, v) in &mut acc.full {
                if let Some(i) = v.s.find('\0') {
                    v.s.truncate(i);
                }
                stats.push((self.ctx.layer_name(pos).into(), k.clone(), v.ty, v.s.as_str().into()));
            }
            accs.insert(pos, acc);
        }
        if !stats.is_empty() {
            TILE_STATS.lock().unwrap().extend(stats);
        }
        let generated: HashMap<_, _> = generated.iter().map(|&(name, index, clustered, density)| ((name, index), (clustered, density))).collect();
        for &(pos, lname, index) in &self.deferred {
            let attrs: Vec<(Box<str>, SerialVal)> = match accs.get(&pos) {
                Some(acc) => acc.keys.iter().chain(&acc.full).cloned().collect(),
                None => self.attrs_of(pos),
            };
            let layer = layers.get_mut(lname).expect("deferred layer");
            let mut f = std::mem::take(&mut layer.features[index]);
            for (k, v) in &attrs {
                match v.ty {
                    AttrType::String => layer.tag_str(&mut f, k, &v.s),
                    AttrType::Null => {}
                    ty => {
                        if let Some(mv) = stringified_to_mvt_value(ty, cstr(&v.s)) {
                            layer.tag(&mut f, k, &mv);
                        }
                    }
                }
            }
            layer.features[index] = f;
            if let Some(&(clustered, density)) = generated.get(&(lname, index)) {
                tag_generated(layer, index, clustered, density);
            }
        }
    }
}

/// One value for the tile statistics: (layer, key, type, text).
type StatValue = (Box<str>, Box<str>, AttrType, Box<str>);

/// The tile statistics that `-E` adds.
static TILE_STATS: Mutex<Vec<StatValue>> = Mutex::new(Vec::new());

/// Add the tile statistics of `-E` to the statistics of the input.
pub fn merge_tile_stats(st: &mut crate::meta::Stats) {
    let mut v = std::mem::take(&mut *TILE_STATS.lock().unwrap());
    if v.is_empty() {
        return;
    }
    v.sort_unstable();
    v.dedup();
    // The values are sorted by layer and key: one group for each.
    let mut i = 0;
    while i < v.len() {
        let j = i + v[i..].iter().take_while(|x| x.0 == v[i].0 && x.1 == v[i].1).count();
        let vals = v[i..j].iter().map(|x| (x.3.clone(), x.2)).collect();
        crate::meta::add_values(st, &v[i].0, &v[i].1, vals);
        i = j;
    }
}

/// Attributes made by clustering and feature density (felt tile.cpp, BSD 2-Clause).
pub fn tag_generated(layer: &mut MvtLayer, index: usize, clustered: u64, density: Option<i64>) {
    let mut feature = std::mem::take(&mut layer.features[index]);
    if clustered > 0 {
        let count = clustered + 1;
        record_generated(&layer.name, "clustered", AttrType::Bool, "true");
        layer.tag(&mut feature, "clustered", &vtmake_mvt::MvtValue::Bool(true));
        for (key, value) in [("point_count", count as f64), ("sqrt_point_count", (100. * (count as f64).sqrt()).round() / 100.)] {
            let text = if key == "point_count" { count.to_string() } else { format!("{value:.6}") };
            record_generated(&layer.name, key, AttrType::Number, &text);
            let value = stringified_to_mvt_value(AttrType::Number, &text).unwrap();
            layer.tag(&mut feature, key, &value);
        }
        let abbrev = if count >= 10000 { format!("{:.0}k", count as f64 / 1000.) } else if count >= 1000 { format!("{:.1}k", count as f64 / 1000.) } else { count.to_string() };
        record_generated(&layer.name, "point_count_abbreviated", AttrType::String, &abbrev);
        layer.tag_str(&mut feature, "point_count_abbreviated", &abbrev);
    }
    if let Some(density) = density { record_generated(&layer.name, "tippecanoe_feature_density", AttrType::Number, &density.to_string()); layer.tag(&mut feature, "tippecanoe_feature_density", &vtmake_mvt::MvtValue::Sint(density)); }
    layer.features[index] = feature;
}

pub(crate) fn record_generated(layer: &str, key: &str, ty: AttrType, text: &str) {
    TILE_STATS.lock().unwrap().push((layer.into(), key.into(), ty, text.into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sv(ty: AttrType, s: &str) -> SerialVal {
        SerialVal { ty, s: s.into() }
    }

    #[test]
    fn coerce_as_tippecanoe() {
        let c = |t, ty, v: &str| {
            let (mut ty, mut v) = (ty, v.to_string());
            coerce(t, &mut ty, &mut v);
            (ty, v)
        };
        use AttrType::*;
        assert_eq!(c(TypeOpt::Int, String, "12"), (Number, "12".into()));
        assert_eq!(c(TypeOpt::Int, Number, "-2.5"), (Number, "-3.000000".into()));
        assert_eq!(c(TypeOpt::Int, String, ""), (Number, "0".into()));
        assert_eq!(c(TypeOpt::Int, String, "abc"), (Number, "0.000000".into()));
        assert_eq!(c(TypeOpt::Float, String, "1.50"), (Number, "1.5".into()));
        assert_eq!(c(TypeOpt::Bool, Number, "0.0"), (Bool, "false".into()));
        assert_eq!(c(TypeOpt::Bool, String, "0.0"), (Bool, "true".into()));
        assert_eq!(c(TypeOpt::Bool, Null, "null"), (Bool, "false".into()));
        assert_eq!(c(TypeOpt::String, Null, "null"), (String, "null".into()));
    }

    #[test]
    fn strtoull_and_ids() {
        assert_eq!(strtoull("007"), (7, ""));
        assert_eq!(strtoull("-5"), (5u64.wrapping_neg(), ""));
        assert_eq!(strtoull("1.5"), (1, ".5"));
        assert_eq!(strtoull("x1"), (0, "x1"));
        assert_eq!(strtoull("99999999999999999999"), (u64::MAX, ""));
        assert_eq!(strip_zeroes("0"), "");
    }

    #[test]
    fn accumulate_as_tippecanoe() {
        let mut a = Acc { keys: vec![("n".into(), sv(AttrType::Number, "2")), ("s".into(), sv(AttrType::String, "a"))], full: Vec::new() };
        a.promote("n");
        a.preserve(Accum::Mean, "n", &sv(AttrType::Number, "4"));
        a.preserve(Accum::Mean, "n", &sv(AttrType::Number, "6"));
        assert_eq!(a.full[0].1.s, "4\u{0}3");
        a.promote("s");
        a.preserve(Accum::Comma, "s", &sv(AttrType::String, "b"));
        assert_eq!(a.full[1].1.s, "a,b");
        a.preserve(Accum::Count, "c", &sv(AttrType::String, "x"));
        a.preserve(Accum::Count, "c", &sv(AttrType::String, "x"));
        assert_eq!(a.full[2].1.s, "2\u{0}2");
        assert!(a.keys.is_empty());
    }
}
