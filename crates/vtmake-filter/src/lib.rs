//! Feature filters `-j` and `-J` (`evaluator.cpp` of felt's tippecanoe).
//!
//! The filter is a JSON object: a key is a layer name or `*`, a value is a
//! Mapbox GL style filter expression (`has`, `!has`, `==`, `!=`, `<`, `<=`,
//! `>`, `>=`, `all`, `any`, `none`, `in`, `!in`, `attribute-filter`). A
//! feature stays in a tile if the expression of its layer and the expression
//! of `*` are true. tippecanoe tests each feature in each tile that it writes
//! (`tile.cpp:next_feature`), after the zoom checks of the feature and before
//! the dot dropping, so `$zoom` can change the result. The feature still goes
//! to the child tiles. Besides the attributes (nulls too), a filter can use
//! `$id`, `$type` (`Point`, `LineString`, `Polygon`) and `$zoom`; an attribute
//! with the same name comes first.
//!
//! `attribute-filter` removes an attribute from the feature in this tile if
//! its expression is false.
//!
//! `vtmake build` uses the global filter ([`init`], [`get`]). `vtmake join`
//! and `vtmake overzoom` make their own [`Filter`] ([`Filter::new`]) and give
//! the values of tile features (`evaluate(mvt_feature, ...)`).
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use vtmake_parse::pull::{Kind, Pull};

/// tippecanoe exit codes (`errors.hpp`).
const EXIT_FILTER: i32 = 105;
const EXIT_JSON: i32 = 107;
const EXIT_OPEN: i32 = 111;

/// `read_filter` (`-J` of tile-join): the filter in the file `fname`, as JSON
/// text. `Err`: the message and the exit code of tippecanoe.
pub fn read_filter(fname: &str) -> Result<String, (String, i32)> {
    let text = std::fs::read(fname).map_err(|e| {
        let m = e.to_string();
        (format!("{fname}: {}", m.split(" (os error").next().unwrap_or(&m)), EXIT_OPEN)
    })?;
    // `json_begin_file` + `json_read_tree` (jsonpull also reads NUL bytes in a file).
    let mut j = vtmake_parse::pull::Pull::new();
    j.set_input(&text, true);
    match j.read_tree() {
        Some(t) => Ok(String::from_utf8_lossy(&j.stringify(t)).into_owned()),
        None => Err((format!("{fname}: {}", j.error.unwrap_or("")), EXIT_JSON)),
    }
}

/// `parse_filter` (`-j`): the filter from a command line value, as JSON text.
pub fn parse_filter(s: &str) -> Result<String, (String, i32)> {
    match vtmake_parse::pull::parse_tree(s.as_bytes()) {
        Ok((j, t)) => Ok(String::from_utf8_lossy(&j.stringify(t)).into_owned()),
        Err(e) => Err((format!("Could not parse filter {s}\n{e}"), EXIT_JSON)),
    }
}

/// One value for a filter (`mvt_value`). `NoKey`: the feature does not have
/// the attribute (`mvt_no_such_key`).
#[derive(Clone, Copy, Debug)]
pub enum Value<'a> {
    NoKey,
    String(&'a str),
    Number(f64),
    Bool(bool),
    Null,
}

pub struct Filter {
    tree: Pull,
    root: u32,
    /// The filter has `attribute-filter`: it can remove attributes.
    pub removes_attributes: bool,
}

static FILTER: OnceLock<Option<Filter>> = OnceLock::new();

/// Set the filter from the command line (its JSON text). Call once.
pub fn init(text: Option<&str>) {
    let _ = FILTER.set(text.map(Filter::new));
}

/// The filter, or `None`.
#[inline]
pub fn get() -> Option<&'static Filter> {
    FILTER.get().and_then(Option::as_ref)
}

fn tree_has_attribute_filter(t: &Pull, o: u32) -> bool {
    match t.kind(o) {
        Kind::String => t.text(o) == b"attribute-filter",
        Kind::Array | Kind::Hash => t.items(o).iter().any(|&i| tree_has_attribute_filter(t, i)),
        _ => false,
    }
}

/// The text before a NUL byte (C strings).
fn cbytes(b: &[u8]) -> &[u8] {
    b.split(|&c| c == 0).next().unwrap_or(b)
}

fn warn_once(flag: &AtomicBool, msg: impl FnOnce() -> String) {
    if !flag.swap(true, Ordering::Relaxed) {
        eprintln!("{}", msg());
    }
}

impl Filter {
    /// A filter from JSON text that parses (the caller checked it).
    pub fn new(text: &str) -> Filter {
        let (tree, root) = vtmake_parse::pull::parse_tree(text.as_bytes()).expect("the filter was checked");
        let removes_attributes = tree_has_attribute_filter(&tree, root);
        Filter { tree, root, removes_attributes }
    }

    fn json(&self, o: u32) -> String {
        String::from_utf8_lossy(&self.tree.stringify(o)).into_owned()
    }

    fn fail(&self, what: &str, o: u32) -> ! {
        eprintln!("{what}: {}", self.json(o));
        std::process::exit(EXIT_FILTER)
    }

    /// `compare`: the order of `one` and the JSON value `two`, or `None` if the
    /// types do not match.
    fn compare(&self, one: Value, two: u32) -> Option<i32> {
        let t = &self.tree;
        match one {
            Value::String(s) => {
                if t.kind(two) != Kind::String {
                    return None;
                }
                Some(match cbytes(s.as_bytes()).cmp(cbytes(t.text(two))) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                })
            }
            Value::Number(v) => {
                if t.kind(two) != Kind::Number {
                    return None;
                }
                let n = t.number(two);
                Some(if v < n {
                    -1
                } else if v > n {
                    1
                } else {
                    0
                })
            }
            Value::Bool(b) => {
                let k = t.kind(two);
                if k != Kind::True && k != Kind::False {
                    return None;
                }
                Some((b && k == Kind::False) as i32)
            }
            Value::Null => (t.kind(two) == Kind::Null).then_some(0),
            Value::NoKey => {
                eprintln!("Internal error: bad mvt type 8");
                std::process::exit(106)
            }
        }
    }

    /// `eval`: 0 (false) or 1 (true).
    fn eval<'v>(&self, get: &dyn Fn(&str) -> Value<'v>, f: u32, excluded: &mut Vec<Box<str>>) -> i32 {
        static WARN_KEY: AtomicBool = AtomicBool::new(false);
        static WARN_TYPE: AtomicBool = AtomicBool::new(false);
        static WARN_KEY_IN: AtomicBool = AtomicBool::new(false);
        static WARN_TYPE_IN: AtomicBool = AtomicBool::new(false);
        let t = &self.tree;
        match t.kind(f) {
            Kind::True | Kind::Null => return 1,
            Kind::False => return 0,
            Kind::Number => return (t.number(f) != 0.0) as i32,
            Kind::String => return (!t.text(f).is_empty()) as i32,
            Kind::Hash => self.fail("Filter is not an array", f),
            Kind::Array => {}
        }
        let a = t.items(f);
        if a.is_empty() {
            self.fail("Array too small in filter", f);
        }
        if t.kind(a[0]) != Kind::String {
            self.fail("Filter operation is not a string", f);
        }
        let key = |i: usize| String::from_utf8_lossy(t.text(a[i])).into_owned();
        let op = t.text(a[0]);
        match op {
            b"has" | b"!has" => {
                if a.len() != 2 {
                    self.fail("Wrong number of array elements in filter", f);
                }
                if t.kind(a[1]) != Kind::String {
                    self.fail(if op == b"has" { "\"has\" key is not a string" } else { "\"!has\" key is not a string" }, f);
                }
                let found = !matches!(get(&key(1)), Value::NoKey);
                (found == (op == b"has")) as i32
            }
            b"==" | b"!=" | b">" | b">=" | b"<" | b"<=" => {
                if a.len() != 3 {
                    self.fail("Wrong number of array elements in filter", f);
                }
                if t.kind(a[1]) != Kind::String {
                    self.fail("comparison key is not a string", f);
                }
                let ff = get(&key(1));
                if matches!(ff, Value::NoKey) {
                    warn_once(&WARN_KEY, || format!("Warning: attribute not found for comparison: {}", self.json(f)));
                    return (op == b"!=") as i32;
                }
                let Some(cmp) = self.compare(ff, a[2]) else {
                    warn_once(&WARN_TYPE, || format!("Warning: mismatched type in comparison: {}", self.json(f)));
                    return (op == b"!=") as i32;
                };
                (match op {
                    b"==" => cmp == 0,
                    b"!=" => cmp != 0,
                    b">" => cmp > 0,
                    b">=" => cmp >= 0,
                    b"<" => cmp < 0,
                    _ => cmp <= 0,
                }) as i32
            }
            b"all" | b"any" | b"none" => {
                let all = op == b"all";
                let mut v = all;
                for &e in &a[1..] {
                    let out = self.eval(get, e, excluded);
                    if all {
                        v = v && out != 0;
                        if !v {
                            break;
                        }
                    } else {
                        v = v || out != 0;
                        if v {
                            break;
                        }
                    }
                }
                (if op == b"none" { !v } else { v }) as i32
            }
            b"in" | b"!in" => {
                if a.len() < 2 {
                    self.fail("Array too small in filter", f);
                }
                if t.kind(a[1]) != Kind::String {
                    self.fail("\"!in\" key is not a string", f);
                }
                let ff = get(&key(1));
                if matches!(ff, Value::NoKey) {
                    warn_once(&WARN_KEY_IN, || format!("Warning: attribute not found for comparison: {}", self.json(f)));
                    return (op == b"!in") as i32;
                }
                let mut found = false;
                for &e in &a[2..] {
                    let cmp = self.compare(ff, e).unwrap_or_else(|| {
                        warn_once(&WARN_TYPE_IN, || format!("Warning: mismatched type in comparison: {}", self.json(f)));
                        1
                    });
                    if cmp == 0 {
                        found = true;
                        break;
                    }
                }
                (found == (op == b"in")) as i32
            }
            b"attribute-filter" => {
                if a.len() != 3 {
                    self.fail("Wrong number of array elements in filter", f);
                }
                if t.kind(a[1]) != Kind::String {
                    self.fail("\"attribute-filter\" key is not a string", f);
                }
                if self.eval(get, a[2], excluded) <= 0 {
                    let k: Box<str> = key(1).into();
                    if !excluded.contains(&k) {
                        excluded.push(k);
                    }
                }
                1
            }
            _ => {
                eprintln!("Unknown filter {}", self.json(f));
                std::process::exit(EXIT_FILTER)
            }
        }
    }

    /// `evaluate`: true if the feature stays. `get` gives the value of an
    /// attribute (or of `$id`, `$type`, `$zoom`). `excluded` gets the
    /// attributes that `attribute-filter` removes.
    pub fn evaluate<'v>(&self, layer: &str, get: &dyn Fn(&str) -> Value<'v>, excluded: &mut Vec<Box<str>>) -> bool {
        let t = &self.tree;
        if t.kind(self.root) != Kind::Hash {
            eprintln!("Error: filter is not a hash: {}", self.json(self.root));
            std::process::exit(EXIT_JSON)
        }
        let mut ok = true;
        if let Some(f) = t.hash_get(self.root, layer) {
            ok = self.eval(get, f, excluded) > 0;
        }
        if ok && let Some(f) = t.hash_get(self.root, "*") {
            ok = self.eval(get, f, excluded) > 0;
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(text: &str) -> Filter {
        Filter::new(text)
    }

    fn feature(k: &str) -> Value<'static> {
        match k {
            "s" => Value::String("abc"),
            "n" => Value::Number(5.0),
            "t" => Value::Bool(true),
            "f" => Value::Bool(false),
            "z" => Value::Null,
            _ => Value::NoKey,
        }
    }

    fn eval(text: &str) -> bool {
        let mut ex = Vec::new();
        filter(text).evaluate("l", &feature, &mut ex)
    }

    #[test]
    fn operators() {
        assert!(eval(r#"{"*":["==","s","abc"]}"#));
        assert!(!eval(r#"{"*":["==","s",5]}"#));
        assert!(eval(r#"{"*":["!=","s",5]}"#));
        assert!(eval(r#"{"l":[">","n",4],"x":false}"#));
        assert!(!eval(r#"{"l":[">","n",4],"*":["<","n",5]}"#));
        assert!(eval(r#"{"*":["in","n",1,"5",5]}"#));
        assert!(eval(r#"{"*":["!in","q",1]}"#));
        assert!(eval(r#"{"*":["all",["has","z"],["!has","q"],["==","z",null]]}"#));
        assert!(eval(r#"{"*":["none",["has","q"]]}"#));
        assert!(eval(r#"{"*":["any",false,0,"",["has","s"]]}"#));
        // tippecanoe's bool order: false is not below true.
        assert!(eval(r#"{"*":["==","f",true]}"#));
        assert!(!eval(r#"{"*":["==","t",false]}"#));
    }

    #[test]
    fn attribute_filter_removes_keys() {
        let f = filter(r#"{"*":["all",["attribute-filter","s",["==","n",4]],["attribute-filter","n",true]]}"#);
        assert!(f.removes_attributes);
        let mut ex = Vec::new();
        assert!(f.evaluate("l", &feature, &mut ex));
        assert_eq!(ex, vec![Box::<str>::from("s")]);
    }
}
