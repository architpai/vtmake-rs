//! CSV input: a port of tippecanoe's `csv.cpp` (`csv_getline`, `csv_split`,
//! `csv_dequote`, `is_number`) and `geocsv.cpp:parse_geocsv`.
//!
//! Each row is a point. The header names the columns; the last column named
//! `y`, `lat` or `*latitude*` (any case) is the latitude, the last one named
//! `x`, `lon`, `lng`, `long` or `*longitude*` is the longitude. The other
//! columns become attributes: numbers in JSON syntax are numbers, the others
//! are strings (or null if empty, with `--empty-csv-columns-are-null`).
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use crate::geojson::{check_utf8, utf8, utf8_or};
use crate::{ParsedFeature, Position};
use vtmake_core::{Attr, AttrType, GeomType, VT_MOVETO};

/// `csv_getline`: the next line with its `\n`, or an empty slice at the end.
fn getline<'a>(data: &'a [u8], pos: &mut usize) -> &'a [u8] {
    let start = *pos;
    let end = memchr::memchr(b'\n', &data[start..]).map_or(data.len(), |i| start + i + 1);
    *pos = end;
    &data[start..end]
}

/// C `isspace` in the "C" locale.
fn isspace(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// `csv_split` (the line is a C string: it ends at a NUL).
pub fn csv_split(s: &[u8]) -> Vec<Vec<u8>> {
    let s = &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())];
    let end = |i: usize| i >= s.len() || s[i] == b'\n' || s[i] == b'\r';
    let mut ret = Vec::new();
    let mut i = 0;
    while !end(i) {
        let start = i;
        let mut within = false;
        while !end(i) {
            if s[i] == b'"' {
                within = !within;
            }
            if s[i] == b',' && !within {
                break;
            }
            i += 1;
        }
        ret.push(s[start..i].to_vec());
        if i < s.len() && s[i] == b',' {
            i += 1;
            while i < s.len() && isspace(s[i]) {
                i += 1;
            }
            if end(i) {
                ret.push(Vec::new());
                break;
            }
        }
    }
    ret
}

/// `csv_dequote`: remove the quotes; `""` becomes `"`.
pub fn csv_dequote(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for i in 0..s.len() {
        if s[i] == b'"' {
            if i + 1 < s.len() && s[i + 1] == b'"' {
                out.push(b'"');
            }
        } else {
            out.push(s[i]);
        }
    }
    out
}

/// `is_number`: JSON number syntax, on a C string (bytes after the end read as NUL).
pub fn is_number(s: &[u8]) -> bool {
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    let mut cp = 0;
    let mut c = at(cp);
    cp += 1;
    if !(c == b'-' || c.is_ascii_digit()) {
        return false;
    }
    if c == b'-' {
        c = at(cp);
        cp += 1;
    }
    if c == b'0' {
    } else if (b'1'..=b'9').contains(&c) {
        while at(cp).is_ascii_digit() {
            cp += 1;
        }
    }
    if at(cp) == b'.' {
        cp += 1;
        if !at(cp).is_ascii_digit() {
            return false;
        }
        while at(cp).is_ascii_digit() {
            cp += 1;
        }
    }
    if at(cp) == b'e' || at(cp) == b'E' {
        cp += 1;
        if at(cp) == b'+' || at(cp) == b'-' {
            cp += 1;
        }
        if !at(cp).is_ascii_digit() {
            return false;
        }
        while at(cp).is_ascii_digit() {
            cp += 1;
        }
    }
    at(cp) == 0
}

/// `parse_geocsv`. `fname` is the name in the messages ("" for standard input).
pub fn read_csv(data: &[u8], fname: &str, empty_is_null: bool, out: &mut Vec<ParsedFeature>) {
    let mut r = CsvReader::new(fname, empty_is_null);
    let mut pos = r.header(data, true).unwrap_or(data.len());
    loop {
        let s = getline(data, &mut pos);
        if s.is_empty() {
            break;
        }
        let seq = r.next_row();
        match r.row(s, seq) {
            Ok(Some(f)) => out.push(f),
            Ok(None) => r.null_geometry(seq),
            Err(e) => e.exit(),
        }
    }
}

/// `parse_geocsv` in parts: [`CsvReader::header`] reads the header, then
/// [`CsvReader::row`] reads each line (also in parallel). The caller writes
/// the messages in the order of the lines.
pub struct CsvReader {
    fname: String,
    empty_is_null: bool,
    columns: usize,
    latcol: usize,
    loncol: usize,
    keys: Vec<Box<str>>,
    /// The number of lines after the header.
    pub seq: usize,
    warned: bool,
}

/// The lines of `data` with their `\n`; with `eof`, also the last line without `\n`.
pub fn lines(data: &[u8], eof: bool) -> impl Iterator<Item = &[u8]> {
    let mut pos = 0;
    std::iter::from_fn(move || {
        let s = getline(data, &mut pos);
        (!s.is_empty() && (eof || s.ends_with(b"\n"))).then_some(s)
    })
}

impl CsvReader {
    pub fn new(fname: &str, empty_is_null: bool) -> Self {
        CsvReader { fname: fname.into(), empty_is_null, columns: 0, latcol: 0, loncol: 0, keys: Vec::new(), seq: 0, warned: false }
    }

    /// Read the header line at the start of `data`. Returns its length, or
    /// `None` if the line is not complete (without `eof`).
    pub fn header(&mut self, data: &[u8], eof: bool) -> Option<usize> {
        let fname = self.fname.as_str();
        let mut pos = 0;
        let s = getline(data, &mut pos);
        if !eof && !s.ends_with(b"\n") {
            return None;
        }
        let mut header: Vec<Vec<u8>> = Vec::new();
        let (mut latcol, mut loncol) = (None, None);
        if !s.is_empty() {
            let err = check_utf8(s);
            if !err.is_empty() {
                eprintln!("{fname}: {err}");
                std::process::exit(119)
            }
            header = csv_split(s).iter().map(|h| csv_dequote(h)).collect();
            for (i, h) in header.iter().enumerate() {
                let lower = h.to_ascii_lowercase();
                let has = |w: &[u8]| lower.windows(w.len()).any(|x| x == w);
                if lower == b"y" || lower == b"lat" || has(b"latitude") {
                    latcol = Some(i);
                }
                if lower == b"x" || lower == b"lon" || lower == b"lng" || lower == b"long" || has(b"longitude") {
                    loncol = Some(i);
                }
            }
        }
        let (Some(latcol), Some(loncol)) = (latcol, loncol) else {
            eprintln!("{fname}: Can't find \"lat\" and \"lon\" columns");
            std::process::exit(103)
        };
        self.keys = header.iter().map(|h| utf8(h, fname, 1)).collect();
        (self.columns, self.latcol, self.loncol) = (header.len(), latcol, loncol);
        Some(pos)
    }

    /// The number of the next line.
    pub fn next_row(&mut self) -> usize {
        self.seq += 1;
        self.seq
    }

    /// The warning for a row without a geometry (once).
    pub fn null_geometry(&mut self, seq: usize) {
        if !self.warned {
            eprintln!("{}:{}: null geometry (additional not reported)", self.fname, seq + 1);
            self.warned = true;
        }
    }

    /// Line `seq` (1 for the first line after the header): the feature, or
    /// `None` for a row without a geometry.
    pub fn row(&self, s: &[u8], seq: usize) -> Result<Option<ParsedFeature>, crate::ParseError> {
        let fname = self.fname.as_str();
        let (latcol, loncol) = (self.latcol, self.loncol);
        let err = check_utf8(s);
        if !err.is_empty() {
            return Err(crate::ParseError { exit_code: 119, message: format!("{fname}: {err}") });
        }
        let line = csv_split(s);
        if line.len() != self.columns {
            return Err(crate::ParseError {
                exit_code: 103,
                message: format!("{fname}:{}: Mismatched column count: {} in line, {} in header", seq + 1, line.len(), self.columns),
            });
        }
        if line[loncol].is_empty() || line[latcol].is_empty() {
            return Ok(None);
        }
        let lon = crate::pull::atof(&line[loncol]);
        let lat = crate::pull::atof(&line[latcol]);
        let mut attrs = Vec::with_capacity(line.len());
        for (i, v) in line.iter().enumerate() {
            if i == latcol || i == loncol {
                continue;
            }
            let v = csv_dequote(v);
            let (ty, text) = if is_number(&v) {
                (AttrType::Number, utf8_or(&v, fname, seq as i32 + 1)?)
            } else if v.is_empty() && self.empty_is_null {
                (AttrType::Null, "null".into())
            } else {
                (AttrType::String, utf8_or(&v, fname, seq as i32 + 1)?)
            };
            attrs.push(Attr { key: self.keys[i].clone(), ty, value: text });
        }
        Ok(Some(ParsedFeature {
            geom_type: GeomType::Point,
            geom: vec![Position { op: VT_MOVETO, lon, lat }],
            attrs,
            minzoom: -1,
            maxzoom: -1,
            layer: None,
            id: None,
            seq: 0,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(s: &str) -> Vec<String> {
        csv_split(s.as_bytes()).into_iter().map(|v| String::from_utf8(v).unwrap()).collect()
    }

    #[test]
    fn split_rules() {
        assert_eq!(split("a,\"b,c\", d\r\n"), ["a", "\"b,c\"", "d"]);
        assert_eq!(split("a,b,\n"), ["a", "b", ""]);
        assert_eq!(split("\n"), Vec::<String>::new());
        assert_eq!(csv_dequote(b"\"x\"\"y\""), b"x\"y");
    }

    #[test]
    fn number_rules() {
        for (s, n) in [("1", true), ("-1.5e3", true), ("007", false), ("1.", false), ("-a", true), ("-ab", false), ("", false), ("1e", false)] {
            assert_eq!(is_number(s.as_bytes()), n, "{s}");
        }
    }

    #[test]
    fn rows() {
        let mut out = Vec::new();
        read_csv(b"name,LAT,lon,pop,e\n\"a, b\",1.5,2,10,\nc,,3,x,\n", "t.csv", true, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].geom[0].lon, out[0].geom[0].lat), (2.0, 1.5));
        let a: Vec<(&str, AttrType, &str)> = out[0].attrs.iter().map(|a| (a.key.as_ref(), a.ty, a.value.as_ref())).collect();
        assert_eq!(a, [("name", AttrType::String, "a, b"), ("pop", AttrType::Number, "10"), ("e", AttrType::Null, "null")]);
    }

    #[test]
    fn lines_of_pieces() {
        let text = b"a\nbb\n\nc";
        assert_eq!(lines(text, false).collect::<Vec<_>>(), [&b"a\n"[..], b"bb\n", b"\n"]);
        assert_eq!(lines(text, true).count(), 4);
    }
}
