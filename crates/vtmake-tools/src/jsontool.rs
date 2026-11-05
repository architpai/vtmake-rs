//! `vtmake json-tool`: a port of `tippecanoe-json-tool` (`jsontool.cpp` and
//! the GeoJSON loop of `geojson-loop.cpp`).
//!
//! It writes each GeoJSON feature (or bare geometry) of the input as one line
//! of compact JSON. `-e key` puts a sort key before each feature (for `sort`),
//! `-w` wraps the lines in a FeatureCollection, and `-c file.csv` joins the
//! CSV columns to the features of a sorted input (the CSV must also be in
//! order).
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::common::*;
use crate::getopt::{GetoptLong, Got};
use crate::usage::{self, Opt};
use std::io::{Read as _, Write};
use vtmake_parse::csv::{csv_dequote, csv_split, is_number};
use vtmake_parse::geojson::{check_utf8, context};
use vtmake_parse::pull::{Kind, NIL, Node, Pull, Read, Repr};

const EXIT_CSV: i32 = 103;
const EXIT_UTF8: i32 = 119;

const GEOMETRY_NAMES: [&str; 6] = ["Point", "MultiPoint", "LineString", "MultiLineString", "Polygon", "MultiPolygon"];

/// `decode32`: UTF-8 (up to a NUL) to code points; a bad sequence is U+FFFD.
fn decode32(s: &[u8]) -> Vec<u32> {
    let s = cstr(s);
    let at = |i: usize| s.get(i).copied().unwrap_or(0) as u32;
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let b = at(i);
        i += 1;
        if b < 0x80 {
            out.push(b);
        } else if b & 0xe0 == 0xc0 {
            let b1 = at(i);
            i += 1;
            if b1 & 0xc0 == 0x80 {
                out.push(((b & 0x1f) << 6) | (b1 & 0x3f));
            } else {
                i -= 1;
                out.push(0xfffd);
            }
        } else if b & 0xf0 == 0xe0 {
            let b1 = at(i);
            i += 1;
            if b1 & 0xc0 == 0x80 {
                let b2 = at(i);
                i += 1;
                if b2 & 0xc0 == 0x80 {
                    out.push(((b & 0x0f) << 12) | ((b1 & 0x3f) << 6) | (b2 & 0x3f));
                } else {
                    i -= 2;
                    out.push(0xfffd);
                }
            } else {
                i -= 1;
                out.push(0xfffd);
            }
        } else if b & 0xf8 == 0xf0 {
            let b1 = at(i);
            i += 1;
            if b1 & 0xc0 == 0x80 {
                let b2 = at(i);
                i += 1;
                if b2 & 0xc0 == 0x80 {
                    let b3 = at(i);
                    i += 1;
                    if b3 & 0xc0 == 0x80 {
                        out.push(((b & 0x07) << 18) | ((b1 & 0x3f) << 12) | ((b2 & 0x3f) << 6) | (b3 & 0x3f));
                    } else {
                        i -= 3;
                        out.push(0xfffd);
                    }
                } else {
                    i -= 2;
                    out.push(0xfffd);
                }
            } else {
                i -= 1;
                out.push(0xfffd);
            }
        } else {
            out.push(0xfffd);
        }
    }
    out
}

/// `sort_quote`: a string that sorts in UTF-32 order in spite of quoting.
/// A code point below U+D800 becomes `\u` and its decimal value.
fn sort_quote(s: &[u8]) -> Vec<u8> {
    let mut ret = Vec::new();
    for c in decode32(s) {
        if c < 0xD800 {
            ret.extend_from_slice(format!("\\u{c:04}").as_bytes());
        } else if c <= 0x7f {
            ret.push(c as u8);
        } else if c <= 0x7ff {
            ret.extend([0xc0 | (c >> 6) as u8, 0x80 | (c & 0x3f) as u8]);
        } else if c <= 0xffff {
            ret.extend([0xe0 | (c >> 12) as u8, 0x80 | ((c >> 6) & 0x3f) as u8, 0x80 | (c & 0x3f) as u8]);
        } else {
            ret.extend([0xf0 | (c >> 18) as u8, 0x80 | ((c >> 12) & 0x3f) as u8, 0x80 | ((c >> 6) & 0x3f) as u8, 0x80 | (c & 0x3f) as u8]);
        }
    }
    ret
}

struct Tool {
    out: std::io::BufWriter<std::io::Stdout>,
    wrap: bool,
    extract: Option<String>,
    warned_extract: bool,
    // CSV join.
    csv: Option<(Vec<u8>, usize)>,
    pe: bool,
    header: Vec<Vec<u8>>,
    fields: Vec<Vec<u8>>,
    prev_joinkey: Vec<u8>,
    warned_key: bool,
    // `-w`: 0 nothing yet, 1 a line in `buffered`, 2 the wrapper is written.
    buffered: Vec<u8>,
    buffered_type: i32,
    buffer_state: i32,
}

impl Tool {
    /// Stop as C `exit()` does: the standard output is flushed first.
    fn die(&mut self, msg: &str, code: i32) -> ! {
        let _ = self.out.flush();
        Fatal { msg: msg.to_string(), code }.exit()
    }

    fn put(&mut self, b: &[u8]) {
        if self.out.write_all(b).is_err() {
            std::process::exit(EXIT_WRITE)
        }
    }

    /// `out`.
    fn emit(&mut self, p: &Pull, s: &[u8], ty: i32, properties: u32) {
        if let Some(key) = self.extract.clone() {
            let mut extracted = sort_quote(b"null");
            match p.hash_get(properties, &key) {
                Some(o) => {
                    extracted = if p.kind(o) == Kind::String { sort_quote(p.text(o)) } else { sort_quote(&p.stringify(o)) };
                }
                None => {
                    if !self.warned_extract {
                        let _ = self.out.flush();
                        eprintln!("Warning: extract key \"{key}\" not found in JSON");
                        self.warned_extract = true;
                    }
                }
            }
            let mut line = b"{\"".to_vec();
            line.extend_from_slice(&extracted);
            line.extend_from_slice(b"\":");
            line.extend_from_slice(cstr(s));
            line.extend_from_slice(b"}\n");
            self.put(&line);
            return;
        }
        if !self.wrap {
            self.put(cstr(s));
            self.put(b"\n");
            return;
        }
        if self.buffer_state == 0 {
            self.buffered = s.to_vec();
            self.buffered_type = ty;
            self.buffer_state = 1;
            return;
        }
        if self.buffer_state == 1 {
            if self.buffered_type == 1 {
                self.put(b"{\"type\":\"FeatureCollection\",\"features\":[\n");
            } else {
                self.put(b"{\"type\":\"GeometryCollection\",\"geometries\":[\n");
            }
            let b = std::mem::take(&mut self.buffered);
            self.put(cstr(&b));
            self.put(b"\n");
            self.buffered = b;
            self.buffer_state = 2;
        }
        self.put(b",\n");
        self.put(cstr(s));
        self.put(b"\n");
        if ty != self.buffered_type {
            self.die("Error: mix of bare geometries and features", EXIT_IMPOSSIBLE);
        }
    }

    /// `csv_getline`: the next line with its `\n`, or empty at the end.
    fn csv_line(&mut self) -> Vec<u8> {
        let (data, pos) = self.csv.as_mut().unwrap();
        let rest = &data[*pos..];
        let n = rest.iter().position(|&c| c == b'\n').map_or(rest.len(), |i| i + 1);
        *pos += n;
        rest[..n].to_vec()
    }

    fn check_utf8(&mut self, s: &[u8]) {
        let err = check_utf8(s);
        if !err.is_empty() {
            self.die(&err, EXIT_UTF8);
        }
    }

    /// `join_csv`: add the CSV columns of the feature's key to its properties.
    fn join_csv(&mut self, p: &mut Pull, j: u32) {
        if self.header.is_empty() {
            let s = self.csv_line();
            if s.is_empty() {
                self.die("Couldn't get column header from CSV file", EXIT_CSV);
            }
            self.check_utf8(&s);
            self.header = csv_split(cstr(&s)).iter().map(|h| csv_dequote(h)).collect();
            if self.header.is_empty() {
                let m = format!("No columns in CSV header \"{}\"", String::from_utf8_lossy(cstr(&s)));
                self.die(&m, EXIT_CSV);
            }
        }

        let properties = p.hash_get(j, "properties").unwrap_or(NIL);
        let h0 = String::from_utf8_lossy(cstr(&self.header[0])).into_owned();
        let key = if properties != NIL { p.hash_get(properties, &h0) } else { None };
        let Some(key) = key else {
            if !self.warned_key {
                let _ = self.out.flush();
                eprintln!("Warning: couldn't find CSV key \"{h0}\" in JSON");
                self.warned_key = true;
            }
            return;
        };

        let joinkey: Vec<u8> = match p.kind(key) {
            Kind::String => p.text(key).to_vec(),
            Kind::Number => vtmake_parse::dtoa::dtoa_milo(p.number(key)).into_bytes(),
            _ => p.stringify(key),
        };
        if joinkey < self.prev_joinkey {
            let m = format!(
                "GeoJSON file is out of sort: \"{}\" follows \"{}\"",
                String::from_utf8_lossy(cstr(&joinkey)),
                String::from_utf8_lossy(cstr(&self.prev_joinkey))
            );
            self.die(&m, EXIT_IMPOSSIBLE);
        }
        self.prev_joinkey = joinkey.clone();

        if self.fields.is_empty() || joinkey > self.fields[0] {
            let mut prevkey = self.fields.first().cloned().unwrap_or_default();
            loop {
                let s = self.csv_line();
                if s.is_empty() {
                    self.fields.clear();
                    break;
                }
                self.check_utf8(&s);
                self.fields = csv_split(cstr(&s)).iter().map(|f| csv_dequote(f)).collect();
                if let Some(f0) = self.fields.first() {
                    if *f0 < prevkey {
                        let m = format!("CSV file is out of sort: \"{}\" follows \"{}\"", String::from_utf8_lossy(cstr(f0)), String::from_utf8_lossy(cstr(&prevkey)));
                        self.die(&m, EXIT_CSV);
                    }
                    if *f0 >= joinkey {
                        break;
                    }
                    prevkey = f0.clone();
                }
            }
        }

        if !self.fields.is_empty() && joinkey == self.fields[0] {
            for i in 1..self.fields.len() {
                let Some(k) = self.header.get(i).cloned() else {
                    // `header[i]` past the end: tippecanoe reads out of bounds.
                    not_supported("json-tool", "a CSV line with more columns than the header");
                };
                let mut v = self.fields[i].clone();
                let mut ty = Kind::String;
                if !v.is_empty() {
                    if v[0] == b'"' {
                        v = csv_dequote(&v);
                    } else if is_number(&v) {
                        ty = Kind::Number;
                    }
                } else if self.pe {
                    ty = Kind::Null;
                }
                if ty == Kind::Null {
                    continue;
                }
                let ko = push_node(p, properties, Kind::String, cstr(&k).to_vec(), Repr::Double(0.0));
                let vo = if ty == Kind::String {
                    push_node(p, properties, Kind::String, v, Repr::Double(0.0))
                } else {
                    push_node(p, properties, Kind::Number, Vec::new(), Repr::Double(vtmake_parse::pull::atof(cstr(&v))))
                };
                p.nodes[properties as usize].items.extend([ko, vo]);
            }
        }
    }

    /// `json_join_action::add_feature`.
    fn add_feature(&mut self, p: &mut Pull, geometry: u32, feature: u32) {
        if feature != geometry {
            if self.csv.is_some() {
                self.join_csv(p, feature);
            }
            let s = p.stringify(feature);
            let props = p.hash_get(feature, "properties").unwrap_or(NIL);
            self.emit(p, &s, 1, props);
        } else {
            let s = p.stringify(geometry);
            self.emit(p, &s, 2, NIL);
        }
    }

    /// `parse_json` with this tool's action, for one input.
    fn process(&mut self, data: &[u8], fname: &str) {
        let mut p = Pull::new();
        p.set_input(data, true);
        let (mut found_hashes, mut found_features, mut found_geometries) = (0i64, 0i64, 0i64);
        loop {
            let j = match p.read() {
                Read::Node(j) => j,
                Read::More | Read::End => {
                    if let Some(e) = p.error {
                        let ctx = if p.root != NIL { context(&p, p.root) } else { String::new() };
                        let _ = self.out.flush();
                        eprintln!("{fname}:{}: {e}: {ctx}", p.line);
                    }
                    break;
                }
            };
            if p.kind(j) == Kind::Hash {
                found_hashes += 1;
                if found_hashes == 50 && found_features == 0 && found_geometries == 0 {
                    let _ = self.out.flush();
                    eprintln!("{fname}:{}: Warning: not finding any GeoJSON features or geometries in input yet after 50 objects.", p.line);
                }
            }
            let Some(ty) = p.hash_get(j, "type") else { continue };
            if p.kind(ty) != Kind::String {
                continue;
            }
            let ty = p.text(ty).to_vec();
            if found_features == 0 {
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
                    found_geometries += 1;
                    self.add_feature(&mut p, j, j);
                    p.free(j);
                    continue;
                }
            }
            if ty != b"Feature" {
                if ty == b"FeatureCollection" {
                    p.free(j);
                }
                continue;
            }
            if found_features == 0 && found_geometries != 0 {
                let _ = self.out.flush();
                eprintln!("{fname}:{}: Warning: found a mixture of features and bare geometries", p.line);
            }
            found_features += 1;
            let Some(geometry) = p.hash_get(j, "geometry") else {
                let _ = self.out.flush();
                eprintln!("{fname}:{}: feature with no geometry: {}", p.line, context(&p, j));
                p.free(j);
                continue;
            };
            match p.hash_get(j, "properties") {
                Some(pr) if matches!(p.kind(pr), Kind::Hash | Kind::Null) => {}
                _ => {
                    let _ = self.out.flush();
                    eprintln!("{fname}:{}: feature without properties hash: {}", p.line, context(&p, j));
                    p.free(j);
                    continue;
                }
            }
            if p.under_properties(j) {
                continue;
            }
            let g = match p.hash_get(geometry, "geometries") {
                Some(g) if p.kind(g) == Kind::Array => g,
                _ => geometry,
            };
            self.add_feature(&mut p, g, j);
            p.free(j);
        }
    }
}

/// A new node of `kind` under `parent` (for the CSV columns).
fn push_node(p: &mut Pull, parent: u32, kind: Kind, text: Vec<u8>, num: Repr) -> u32 {
    p.nodes.push(Node { kind, parent, text, num, items: Vec::new() });
    (p.nodes.len() - 1) as u32
}

const OPTIONS: &[Opt] = &[
    Opt::heading("Wrapping the output"),
    Opt::new("wrap", false, 'w' as u32),
    Opt::heading("Sorting and joining"),
    Opt::new("extract", true, 'e' as u32),
    Opt::new("csv", true, 'c' as u32),
    // `{"empty-csv-columns-are-null", no_argument, &pe, 1}`.
    Opt::new("empty-csv-columns-are-null", false, 1),
    Opt::heading(""),
    Opt::new("prevent", true, 'p' as u32),
];

fn print_usage(argv0: &str) -> ! {
    let mut s = usage::usage_text(argv0, &["[options] [file.json ...]"], OPTIONS, &[]);
    s.push_str("\nIf no files are named, the JSON is read from the standard input.\n");
    eprint!("{s}");
    std::process::exit(EXIT_ARGS)
}

/// `tippecanoe-json-tool`.
pub fn main(argv: Vec<String>) {
    let argv0 = argv.first().cloned().unwrap_or_else(|| "tippecanoe-json-tool".into());
    let (optstring, longs) = usage::getopt_table(OPTIONS);
    let mut g = GetoptLong::new(&argv, &optstring, &longs);
    let mut t = Tool {
        out: std::io::BufWriter::with_capacity(1 << 16, std::io::stdout()),
        wrap: false,
        extract: None,
        warned_extract: false,
        csv: None,
        pe: false,
        header: Vec::new(),
        fields: Vec::new(),
        prev_joinkey: Vec::new(),
        warned_key: false,
        buffered: Vec::new(),
        buffered_type: -1,
        buffer_state: 0,
    };
    let mut csv: Option<String> = None;
    while let Some(got) = g.next_opt() {
        let (val, arg) = match got {
            Got::Long(i, v) => (longs[i].ident, v),
            Got::Short(c, v) => (c as u32, v),
            Got::Error => print_usage(&argv0),
        };
        let arg = arg.unwrap_or_default();
        match char::from_u32(val).unwrap_or('\0') {
            '\u{1}' => t.pe = true,
            'w' => t.wrap = true,
            'e' => t.extract = Some(arg),
            'c' => csv = Some(arg),
            'p' => {
                if arg == "e" {
                    t.pe = true;
                } else {
                    eprintln!("{argv0}: Unknown option for -p{arg}");
                    std::process::exit(EXIT_ARGS);
                }
            }
            _ => print_usage(&argv0),
        }
    }
    if t.extract.is_some() && t.wrap {
        eprintln!("{argv0}: --wrap and --extract not supported together");
        std::process::exit(EXIT_ARGS);
    }
    if let Some(c) = &csv {
        match std::fs::read(c) {
            Ok(b) => t.csv = Some((b, 0)),
            Err(e) => Fatal { msg: format!("{c}: {}", strerror(&e)), code: EXIT_OPEN }.exit(),
        }
    }
    let operands = std::mem::take(&mut g.operands);
    if operands.is_empty() {
        let mut data = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut data);
        t.process(&data, "standard input");
    } else {
        for f in &operands {
            match std::fs::read(f) {
                Ok(b) => t.process(&b, f),
                Err(e) => t.die(&format!("{f}: {}", strerror(&e)), EXIT_OPEN),
            }
        }
    }
    if t.buffer_state == 1 {
        let b = std::mem::take(&mut t.buffered);
        t.put(cstr(&b));
        t.put(b"\n");
    } else if t.buffer_state == 2 {
        t.put(b"]}\n");
    }
    if t.out.flush().is_err() {
        std::process::exit(EXIT_WRITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_quote_text() {
        assert_eq!(sort_quote(b"A"), b"\\u0065");
        assert_eq!(sort_quote("é".as_bytes()), b"\\u0233");
        assert_eq!(sort_quote(&[0xc3]), "\u{fffd}".as_bytes());
        assert_eq!(sort_quote("😀".as_bytes()), "😀".as_bytes());
    }
}
