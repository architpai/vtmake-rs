//! The GeoJSON loop of tippecanoe on the [`Pull`] parser: `geojson-loop.cpp:parse_json`,
//! `geojson.cpp:serialize_geojson_feature` and `check_crs`,
//! `read_json.cpp:parse_coordinates` and `stringify_value`.
//!
//! Each complete hash is examined when the parser gives it: a `Feature` (also a
//! nested one, but not one inside `properties`), a bare geometry (only before
//! the first feature), or a `FeatureCollection` (only its `crs`). Fatal errors
//! (a malformed point, a bad zoom, bad UTF-8) stop the program with
//! tippecanoe's message and exit code, as tippecanoe does.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::pull::{Kind, NIL, Pull, Read};
use crate::{ParsedFeature, Position};
use std::sync::atomic::{AtomicBool, Ordering};
use vtmake_core::{Attr, AttrType, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};

const GEOMETRY_NAMES: [&str; 6] = ["Point", "MultiPoint", "LineString", "MultiLineString", "Polygon", "MultiPolygon"];
/// `geometry_within`: the type of the members (-1: a position).
const WITHIN: [i32; 6] = [-1, 0, 0, 2, 2, 4];
const MB_GEOMETRY: [GeomType; 6] = [GeomType::Point, GeomType::Point, GeomType::Line, GeomType::Line, GeomType::Polygon, GeomType::Polygon];

/// Settings that change the reading.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// `-aI`: `"id"` strings with an integer become the feature ID.
    pub convert_stringified_ids: bool,
    /// `-q`: no CRS warning.
    pub quiet: bool,
    /// The `-s` projection alias (`check_crs` compares with it).
    pub projection_alias: &'static str,
}

/// The state of `parse_json` for one input stream.
#[derive(Default)]
pub struct Loop {
    found_hashes: i64,
    pub found_features: i64,
    found_geometries: i64,
}

/// Exit as tippecanoe does on a fatal input error.
fn die(code: i32) -> ! {
    std::process::exit(code)
}

/// `json_context`: the object as JSON, cut to 500 bytes.
pub fn context(p: &Pull, o: u32) -> String {
    let mut s = p.stringify(o);
    if s.len() >= 500 {
        s.truncate(497);
        s.extend_from_slice(b"...");
    }
    format!("in JSON object {}", String::from_utf8_lossy(&s))
}

/// `check_utf8`: "" or the error message.
pub fn check_utf8(s: &[u8]) -> String {
    let mut i = 0;
    while i < s.len() {
        let mut fail = 0;
        if s[i] & 0x80 == 0x80 {
            let cont = |k: usize| i + k < s.len() && s[i + k] & 0xC0 == 0x80;
            if s[i] & 0xE0 == 0xC0 {
                if cont(1) { i += 1 } else { fail = 2 }
            } else if s[i] & 0xF0 == 0xE0 {
                if cont(1) && cont(2) { i += 2 } else { fail = 3 }
            } else if s[i] & 0xF8 == 0xF0 {
                if cont(1) && cont(2) && cont(3) { i += 3 } else { fail = 4 }
            } else {
                fail = 1;
            }
        }
        if fail != 0 {
            let bytes: Vec<String> = (0..fail).take_while(|j| i + j < s.len()).map(|j| format!("0x{:02X}", s[i + j])).collect();
            return format!("\"{}\" is not valid UTF-8 ({})", String::from_utf8_lossy(s), bytes.join(" "));
        }
        i += 1;
    }
    String::new()
}

/// Text for a Rust string. tippecanoe keeps any bytes; vtmake keeps UTF-8 only.
pub fn utf8(b: &[u8], fname: &str, line: i32) -> Box<str> {
    utf8_or(b, fname, line).unwrap_or_else(|e| e.exit())
}

/// [`utf8`] without the exit: the error has its message and exit code.
pub fn utf8_or(b: &[u8], fname: &str, line: i32) -> Result<Box<str>, crate::ParseError> {
    match std::str::from_utf8(b) {
        Ok(s) => Ok(s.into()),
        Err(_) => Err(crate::ParseError {
            exit_code: 101,
            message: format!("vtmake build: {fname}:{line}: text that is not UTF-8 (\"{}\") is not supported yet", String::from_utf8_lossy(b)),
        }),
    }
}

/// C `strtoull` in base 10: the value (overflow gives `u64::MAX`, a minus sign
/// negates in 64 bits) and the number of bytes used (0 if there are no digits).
pub fn strtoull(s: &[u8]) -> (u64, usize) {
    let lead = s.iter().take_while(|&&c| matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')).count();
    let sign = usize::from(matches!(s.get(lead), Some(b'+' | b'-')));
    let digits = s[lead + sign..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return (0, 0);
    }
    let mut v: u64 = 0;
    let mut overflow = false;
    for &c in &s[lead + sign..lead + sign + digits] {
        match v.checked_mul(10).and_then(|v| v.checked_add((c - b'0') as u64)) {
            Some(n) => v = n,
            None => overflow = true,
        }
    }
    let v = if overflow {
        u64::MAX
    } else if s[lead] == b'-' {
        v.wrapping_neg()
    } else {
        v
    };
    (v, lead + sign + digits)
}

/// `integer_zoom`.
pub fn integer_zoom(fname: &str, text: &str) -> i8 {
    let d = crate::pull::atof(text.as_bytes());
    if !d.is_finite() || d != d.floor() || !(0.0..=32.0).contains(&d) {
        eprintln!("{fname}: Expected integer zoom level in \"tippecanoe\" GeoJSON extension, not {text}");
        die(107)
    }
    d as i8
}

static WARNED_NULL_GEOMETRY: AtomicBool = AtomicBool::new(false);
static WARNED_DIMENSIONS: AtomicBool = AtomicBool::new(false);
static WARNED_FRAC: AtomicBool = AtomicBool::new(false);
static WARNED_LARGE: AtomicBool = AtomicBool::new(false);
static WARNED_NEG: AtomicBool = AtomicBool::new(false);
static WARNED_NAN: AtomicBool = AtomicBool::new(false);
// The string IDs of `-aI` have their own flags (other `static` variables in
// tippecanoe's `serialize_geojson_feature`).
static WARNED_FRAC_STRING: AtomicBool = AtomicBool::new(false);
static WARNED_LARGE_STRING: AtomicBool = AtomicBool::new(false);

fn once(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::Relaxed)
}

/// `stringify_value`: the type and the text of an attribute value.
pub fn stringify_value(p: &Pull, v: u32, fname: &str, line: i32, feature: u32) -> (AttrType, Box<str>) {
    match p.kind(v) {
        Kind::String => {
            let err = check_utf8(p.text(v));
            if !err.is_empty() {
                eprintln!("{fname}:{line}: {err}: {}", context(p, feature));
                die(119)
            }
            (AttrType::String, utf8(p.text(v), fname, line))
        }
        Kind::Number => {
            let s = if p.large_unsigned(v) != 0 {
                p.large_unsigned(v).to_string()
            } else if p.large_signed(v) != 0 {
                p.large_signed(v).to_string()
            } else {
                crate::dtoa::dtoa_milo(p.number(v))
            };
            (AttrType::Number, s.into())
        }
        Kind::True => (AttrType::Bool, "true".into()),
        Kind::False => (AttrType::Bool, "false".into()),
        Kind::Null => (AttrType::Null, "null".into()),
        _ => (AttrType::String, utf8(&p.stringify(v), fname, line)),
    }
}

/// `parse_coordinates`.
#[allow(clippy::too_many_arguments)]
pub fn parse_coordinates(p: &Pull, t: usize, j: u32, out: &mut Vec<Position>, mut op: u8, fname: &str, line: i32, feature: u32) {
    if j == NIL || p.kind(j) != Kind::Array {
        eprintln!("{fname}:{line}: expected array for geometry type {t}: {}", context(p, feature));
        return;
    }
    let within = WITHIN[t];
    if within >= 0 {
        for (i, &c) in p.items(j).iter().enumerate() {
            if within == 0 {
                op = if i == 0 || MB_GEOMETRY[t] == GeomType::Point { VT_MOVETO } else { VT_LINETO };
            }
            parse_coordinates(p, within as usize, c, out, op, fname, line, feature);
        }
    } else {
        let a = p.items(j);
        if a.len() >= 2 && p.kind(a[0]) == Kind::Number && p.kind(a[1]) == Kind::Number {
            if a.len() > 2 && once(&WARNED_DIMENSIONS) {
                eprintln!("{fname}:{line}: ignoring dimensions beyond two: {}", context(p, j));
                eprintln!("{fname}:{line}: ignoring dimensions beyond two: {}", context(p, feature));
            }
            out.push(Position { op, lon: p.number(a[0]), lat: p.number(a[1]) });
        } else {
            eprintln!("{fname}:{line}: malformed point: {}", context(p, j));
            eprintln!("{fname}:{line}: malformed point: {}", context(p, feature));
            die(107)
        }
    }
    if t == 4 {
        out.push(Position { op: VT_CLOSEPATH, lon: 0.0, lat: 0.0 });
    }
}

/// `serialize_geojson_feature` up to the call of `serialize_feature`.
#[allow(clippy::too_many_arguments)]
fn serialize(p: &Pull, s: &Settings, geometry: u32, properties: u32, id: u32, tippecanoe: u32, feature: u32, fname: &str, out: &mut Vec<ParsedFeature>) {
    let line = p.line;
    let Some(gt) = p.hash_get(geometry, "type") else {
        if once(&WARNED_NULL_GEOMETRY) {
            eprintln!("{fname}:{line}: null geometry (additional not reported): {}", context(p, feature));
        }
        return;
    };
    if p.kind(gt) != Kind::String {
        eprintln!("{fname}:{line}: geometry type is not a string: {}", context(p, feature));
        return;
    }
    let coordinates = match p.hash_get(geometry, "coordinates") {
        Some(c) if p.kind(c) == Kind::Array => c,
        _ => {
            eprintln!("{fname}:{line}: feature without coordinates array: {}", context(p, feature));
            return;
        }
    };
    let Some(t) = GEOMETRY_NAMES.iter().position(|n| n.as_bytes() == p.text(gt)) else {
        eprintln!("{fname}:{line}: Can't handle geometry type {}: {}", String::from_utf8_lossy(p.text(gt)), context(p, feature));
        return;
    };

    let mut f = ParsedFeature { geom_type: MB_GEOMETRY[t], geom: Vec::new(), attrs: Vec::new(), minzoom: -1, maxzoom: -1, layer: None, id: None, seq: 0 };
    if tippecanoe != NIL {
        for (key, field) in [("minzoom", &mut f.minzoom), ("maxzoom", &mut f.maxzoom)] {
            if let Some(n) = p.hash_get(tippecanoe, key)
                && p.kind(n) == Kind::Number
            {
                *field = integer_zoom(fname, &crate::dtoa::dtoa_milo(p.number(n)));
            }
        }
        if let Some(l) = p.hash_get(tippecanoe, "layer")
            && p.kind(l) == Kind::String
        {
            f.layer = Some(utf8(p.text(l), fname, line));
        }
    }

    if id != NIL {
        if p.kind(id) == Kind::Number {
            let text = crate::dtoa::dtoa_milo(p.number(id));
            if p.number(id) >= 0.0 {
                let (mut value, used) = strtoull(text.as_bytes());
                if p.large_unsigned(id) != 0 {
                    value = p.large_unsigned(id);
                }
                if used != text.len() {
                    if once(&WARNED_FRAC) {
                        eprintln!("Warning: Can't represent non-integer feature ID {text}");
                    }
                } else if p.large_unsigned(id) == 0 && value.to_string() != text {
                    if once(&WARNED_LARGE) {
                        eprintln!("Warning: Can't represent too-large feature ID {text}");
                    }
                } else {
                    f.id = Some(value);
                }
            } else if once(&WARNED_NEG) {
                eprintln!("Warning: Can't represent negative feature ID {text}");
            }
        } else {
            let mut converted = false;
            if s.convert_stringified_ids && p.kind(id) == Kind::String {
                let t = p.text(id);
                let t = &t[..t.iter().position(|&c| c == 0).unwrap_or(t.len())];
                let text = String::from_utf8_lossy(t);
                let (v, used) = strtoull(t);
                if used != t.len() {
                    if once(&WARNED_FRAC_STRING) {
                        eprintln!("Warning: Can't represent non-integer feature ID {text}");
                    }
                } else if v.to_string().as_bytes() != p.text(id) {
                    if once(&WARNED_LARGE_STRING) {
                        eprintln!("Warning: Can't represent too-large feature ID {text}");
                    }
                } else {
                    f.id = Some(v);
                    converted = true;
                }
            }
            if !converted && once(&WARNED_NAN) {
                eprintln!("Warning: Can't represent non-numeric feature ID {}", String::from_utf8_lossy(&p.stringify(id)));
            }
        }
    }

    if properties != NIL && p.kind(properties) == Kind::Hash {
        for e in p.items(properties).chunks(2) {
            if p.kind(e[0]) == Kind::String {
                let (ty, value) = stringify_value(p, e[1], fname, line, feature);
                f.attrs.push(Attr { key: utf8(p.text(e[0]), fname, line), ty, value });
            }
        }
    }

    parse_coordinates(p, t, coordinates, &mut f.geom, VT_MOVETO, fname, line, feature);
    out.push(f);
}

/// `check_crs`.
fn check_crs(p: &Pull, s: &Settings, j: u32, fname: &str) {
    let Some(crs) = p.hash_get(j, "crs") else { return };
    let Some(props) = p.hash_get(crs, "properties") else { return };
    if let Some(name) = p.hash_get(props, "name")
        && p.kind(name) == Kind::String
        && p.text(name) != s.projection_alias.as_bytes()
        && !s.quiet
    {
        let name = String::from_utf8_lossy(p.text(name));
        let alias = s.projection_alias;
        eprintln!("{fname}: Warning: GeoJSON specified projection \"{name}\", not the expected \"{alias}\".");
        eprintln!("{fname}: If \"{alias}\" is not the expected projection, use -s to specify the right one.");
    }
}

impl Loop {
    /// Read values until the region ends or the input ends. The features go to
    /// `out` with the byte position (in the region) where each one ends.
    /// Returns false at the end of the input or after an error (tippecanoe
    /// then stops reading this stream).
    pub fn run(&mut self, p: &mut Pull, s: &Settings, fname: &str, out: &mut Vec<(usize, ParsedFeature)>) -> bool {
        let mut fs = Vec::new();
        loop {
            let j = match p.read() {
                Read::Node(j) => j,
                Read::More => return true,
                Read::End => {
                    if let Some(e) = p.error {
                        let ctx = if p.root != NIL { context(p, p.root) } else { String::new() };
                        eprintln!("{fname}:{}: {e}: {ctx}", p.line);
                    }
                    return false;
                }
            };
            let pos = p.position();
            fs.clear();
            self.value(p, s, j, fname, &mut fs);
            out.extend(fs.drain(..).map(|f| (pos, f)));
        }
    }

    fn value(&mut self, p: &mut Pull, s: &Settings, j: u32, fname: &str, out: &mut Vec<ParsedFeature>) {
        if p.kind(j) == Kind::Hash {
            self.found_hashes += 1;
            if self.found_hashes == 50 && self.found_features == 0 && self.found_geometries == 0 {
                eprintln!("{fname}:{}: Warning: not finding any GeoJSON features or geometries in input yet after 50 objects.", p.line);
            }
        }
        let Some(ty) = p.hash_get(j, "type") else { return };
        if p.kind(ty) != Kind::String {
            return;
        }
        let ty = p.text(ty).to_vec();
        if self.found_features == 0 {
            let mut is_geometry = GEOMETRY_NAMES.iter().any(|n| n.as_bytes() == ty);
            let par = p.parent(j);
            if is_geometry && par != NIL {
                if p.kind(par) == Kind::Array && p.parent(par) != NIL {
                    let pp = p.parent(par);
                    if p.kind(pp) == Kind::Hash && p.hash_get(pp, "geometries").is_some() {
                        is_geometry = false;
                    }
                } else if p.kind(par) == Kind::Hash && p.hash_get(par, "geometry").is_some() {
                    is_geometry = false;
                }
            }
            if is_geometry && p.under_properties(j) {
                is_geometry = false;
            }
            if is_geometry {
                self.found_geometries += 1;
                serialize(p, s, j, NIL, NIL, NIL, j, fname, out);
                p.free(j);
                return;
            }
        }
        if ty != b"Feature" {
            if ty == b"FeatureCollection" {
                check_crs(p, s, j, fname);
                p.free(j);
            }
            return;
        }
        if self.found_features == 0 && self.found_geometries != 0 {
            eprintln!("{fname}:{}: Warning: found a mixture of features and bare geometries", p.line);
        }
        self.found_features += 1;
        let Some(geometry) = p.hash_get(j, "geometry") else {
            eprintln!("{fname}:{}: feature with no geometry: {}", p.line, context(p, j));
            p.free(j);
            return;
        };
        let properties = match p.hash_get(j, "properties") {
            Some(pr) if matches!(p.kind(pr), Kind::Hash | Kind::Null) => pr,
            _ => {
                eprintln!("{fname}:{}: feature without properties hash: {}", p.line, context(p, j));
                p.free(j);
                return;
            }
        };
        if p.under_properties(j) {
            return;
        }
        let tippecanoe = p.hash_get(j, "tippecanoe").unwrap_or(NIL);
        let id = p.hash_get(j, "id").unwrap_or(NIL);
        match p.hash_get(geometry, "geometries") {
            Some(g) if p.kind(g) == Kind::Array => {
                for &m in p.items(g) {
                    serialize(p, s, m, properties, id, tippecanoe, j, fname, out);
                }
            }
            _ => serialize(p, s, geometry, properties, id, tippecanoe, j, fname, out),
        }
        p.free(j);
    }

    /// A feature that the fast parser read (it is not given to the parser).
    pub fn count_feature(&mut self) {
        self.found_features += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &str) -> Vec<ParsedFeature> {
        let mut p = Pull::new();
        p.set_input(s.as_bytes(), true);
        let mut out = Vec::new();
        Loop::default().run(&mut p, &Settings { projection_alias: "urn:ogc:def:crs:OGC:1.3:CRS84", ..Default::default() }, "t", &mut out);
        out.into_iter().map(|x| x.1).collect()
    }

    #[test]
    fn feature_collection_pretty() {
        let fc = "{\n \"type\": \"FeatureCollection\",\n \"features\": [\n  {\"type\": \"Feature\", \"properties\": {\"a\": 1},\n   \"geometry\": {\"type\": \"Point\", \"coordinates\": [1, 2]}},\n  {\"type\": \"Feature\", \"properties\": null, \"geometry\": {\"type\": \"LineString\", \"coordinates\": [[1, 2], [3, 4]]}}\n ]\n}\n";
        let f = read(fc);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].attrs[0].value.as_ref(), "1");
        assert_eq!(f[1].geom_type, GeomType::Line);
    }

    #[test]
    fn bare_geometries_only_before_features() {
        let f = read(r#"{"type":"Point","coordinates":[1,2]} {"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[3,4]}} {"type":"Point","coordinates":[5,6]}"#);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].geom[0].lon, 1.0);
        assert_eq!(f[1].geom[0].lon, 3.0);
    }

    #[test]
    fn a_feature_type_in_properties_makes_them_null() {
        let f = read(r#"{"type":"Feature","properties":{"type":"Feature","x":1},"geometry":{"type":"Point","coordinates":[1,2]}}"#);
        assert_eq!(f.len(), 1);
        assert!(f[0].attrs.is_empty());
        let f = read(r#"{"type":"Feature","properties":{"p":{"type":"FeatureCollection"}},"geometry":{"type":"Point","coordinates":[1,2]}}"#);
        assert_eq!(f[0].attrs.len(), 1);
        assert_eq!(f[0].attrs[0].ty, AttrType::Null);
    }
}
