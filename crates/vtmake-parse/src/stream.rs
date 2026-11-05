//! Read common Feature lines without a JSON tree. The reader gives other lines
//! to the jsonpull port (`pull.rs`, `geojson.rs`). An accepted line gives the
//! same feature as tippecanoe's parser.
use crate::{ParsedFeature, Position, json};
use vtmake_core::{AttrType, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use std::borrow::Cow;

pub struct Attribute<'a> {
    pub key: Cow<'a, str>,
    pub ty: AttrType,
    pub value: Cow<'a, str>,
}

/// Keep the geometry and attribute buffers between lines.
pub struct StreamParser<'a> {
    pub feature: ParsedFeature,
    pub attrs: Vec<Attribute<'a>>,
    /// A comma follows the feature (`{...},`: a line of a FeatureCollection).
    pub comma: bool,
    layer_seen: bool,
}
impl Default for StreamParser<'_> {
    fn default() -> Self {
        Self {
            feature: ParsedFeature {
                geom_type: GeomType::Point,
                geom: Vec::new(),
                attrs: Vec::new(),
                minzoom: -1,
                maxzoom: -1,
                layer: None,
                id: None,
                seq: 0,
            },
            attrs: Vec::new(),
            comma: false,
            layer_seen: false,
        }
    }
}
impl<'a> StreamParser<'a> {
    /// Return false if the reference parser must read this line.
    /// No output is valid when this function returns false.
    pub fn parse(&mut self, line: &'a [u8]) -> bool {
        self.feature.geom.clear();
        self.attrs.clear();
        self.feature.minzoom = -1;
        self.feature.maxzoom = -1;
        self.layer_seen = false;
        self.feature.id = None;
        self.comma = false;
        let accepted = self.parse_inner(line).is_some();
        if !self.layer_seen {
            self.feature.layer = None;
        }
        accepted
    }
    fn parse_inner(&mut self, line: &'a [u8]) -> Option<()> {
        let s = std::str::from_utf8(line).ok()?;
        let mut p = json::Parser { s, i: 0 };
        take(&mut p, b'{')?;
        let mut seen = 0u8;
        loop {
            let key = string(&mut p)?;
            take(&mut p, b':')?;
            let bit = match key.as_ref() {
                "type" => {
                    if string(&mut p)? != "Feature" {
                        return None;
                    }
                    1
                }
                "geometry" => {
                    self.geometry(&mut p)?;
                    2
                }
                "properties" => {
                    self.properties(&mut p)?;
                    4
                }
                "tippecanoe" => {
                    self.options(&mut p)?;
                    8
                }
                "id" => {
                    let json::Value::Number(raw) = p.value(1).ok()? else {
                        return None;
                    };
                    let text = json::number(raw);
                    if raw.parse::<f64>().ok()? < 0. {
                        return None;
                    }
                    self.feature.id = Some(text.parse().ok()?);
                    16
                }
                _ => return None,
            };
            if seen & bit != 0 {
                return None;
            }
            seen |= bit;
            if end(&mut p, b'}')? {
                break;
            }
        }
        p.space();
        if p.s.as_bytes().get(p.i) == Some(&b',') {
            self.comma = true;
            p.i += 1;
            p.space();
        }
        (seen & 7 == 7 && p.i == s.len()).then_some(())
    }
    fn properties(&mut self, p: &mut json::Parser<'a>) -> Option<()> {
        p.space();
        if p.s.as_bytes().get(p.i) == Some(&b'n') {
            return matches!(p.value(1).ok()?, json::Value::Null).then_some(());
        }
        take(p, b'{')?;
        if close(p, b'}') {
            return Some(());
        }
        loop {
            let key = string(p)?;
            take(p, b':')?;
            p.space();
            let (ty, value) = match p.s.as_bytes().get(p.i)? {
                b'"' => {
                    let v = p.string().ok()?;
                    // jsonpull reads the properties as a hash of their own; with
                    // this type tippecanoe frees it (see `geojson.rs`).
                    if key == "type" && (v == "Feature" || v == "FeatureCollection") {
                        return None;
                    }
                    (AttrType::String, v)
                }
                b't' if p.s[p.i..].starts_with("true") => {
                    p.i += 4;
                    (AttrType::Bool, Cow::Borrowed("true"))
                }
                b'f' if p.s[p.i..].starts_with("false") => {
                    p.i += 5;
                    (AttrType::Bool, Cow::Borrowed("false"))
                }
                b'n' if p.s[p.i..].starts_with("null") => {
                    p.i += 4;
                    (AttrType::Null, Cow::Borrowed("null"))
                }
                b'-' | b'0'..=b'9' => {
                    let n = numeric_text(p)?;
                    (AttrType::Number, attribute_number(n))
                }
                _ => return None,
            };
            self.attrs.push(Attribute { key, ty, value });
            if end(p, b'}')? {
                return Some(());
            }
        }
    }
    fn options(&mut self, p: &mut json::Parser<'a>) -> Option<()> {
        take(p, b'{')?;
        if close(p, b'}') {
            return Some(());
        }
        let mut seen = 0;
        loop {
            let key = string(p)?;
            take(p, b':')?;
            let bit = match key.as_ref() {
                "layer" => {
                    let name = string(p)?;
                    if self.feature.layer.as_deref() != Some(name.as_ref()) {
                        self.feature.layer = Some(name.as_ref().into());
                    }
                    self.layer_seen = true;
                    1
                }
                "minzoom" | "maxzoom" => {
                    let n = numeric_text(p)?;
                    let v = if let Ok(v) = n.parse::<u8>() {
                        f64::from(v)
                    } else {
                        crate::dtoa::dtoa_milo(n.parse().ok()?)
                            .parse::<f64>()
                            .ok()?
                    };
                    if !v.is_finite() || v.fract() != 0. || !(0. ..=32.).contains(&v) {
                        return None;
                    }
                    if key == "minzoom" {
                        self.feature.minzoom = v as i8;
                    } else {
                        self.feature.maxzoom = v as i8;
                    }
                    if key == "minzoom" { 2 } else { 4 }
                }
                _ => return None,
            };
            if seen & bit != 0 {
                return None;
            }
            seen |= bit;
            if end(p, b'}')? {
                return Some(());
            }
        }
    }
    fn geometry(&mut self, p: &mut json::Parser<'a>) -> Option<()> {
        take(p, b'{')?;
        if string(p)? != "type" {
            return None;
        }
        take(p, b':')?;
        let (kind, depth) = match string(p)?.as_ref() {
            "Point" => (GeomType::Point, 0),
            "MultiPoint" => (GeomType::Point, 1),
            "LineString" => (GeomType::Line, 1),
            "MultiLineString" => (GeomType::Line, 2),
            "Polygon" => (GeomType::Polygon, 2),
            "MultiPolygon" => (GeomType::Polygon, 3),
            _ => return None,
        };
        take(p, b',')?;
        if string(p)? != "coordinates" {
            return None;
        }
        take(p, b':')?;
        self.feature.geom_type = kind;
        coords(p, depth, kind, VT_MOVETO, &mut self.feature.geom)?;
        take(p, b'}')
    }
}
fn string<'a>(p: &mut json::Parser<'a>) -> Option<Cow<'a, str>> {
    p.space();
    if p.s.as_bytes().get(p.i) != Some(&b'"') {
        return None;
    }
    p.string().ok()
}
fn take(p: &mut json::Parser<'_>, b: u8) -> Option<()> {
    p.space();
    if p.s.as_bytes().get(p.i) != Some(&b) {
        return None;
    }
    p.i += 1;
    Some(())
}
fn close(p: &mut json::Parser<'_>, b: u8) -> bool {
    p.space();
    if p.s.as_bytes().get(p.i) == Some(&b) {
        p.i += 1;
        true
    } else {
        false
    }
}
fn end(p: &mut json::Parser<'_>, b: u8) -> Option<bool> {
    if close(p, b) {
        Some(true)
    } else {
        take(p, b',')?;
        Some(false)
    }
}
fn coords(
    p: &mut json::Parser<'_>,
    depth: usize,
    kind: GeomType,
    op: u8,
    out: &mut Vec<Position>,
) -> Option<()> {
    take(p, b'[')?;
    if depth == 0 {
        let lon = coordinate(p)?;
        take(p, b',')?;
        let lat = coordinate(p)?;
        take(p, b']')?;
        out.push(Position { op, lon, lat });
    } else {
        if !close(p, b']') {
            let mut first = true;
            loop {
                let op = if depth == 1 && !first && kind != GeomType::Point {
                    VT_LINETO
                } else {
                    VT_MOVETO
                };
                coords(p, depth - 1, kind, op, out)?;
                first = false;
                if end(p, b']')? {
                    break;
                }
            }
        }
        if depth == 2 && kind == GeomType::Polygon {
            out.push(Position {
                op: VT_CLOSEPATH,
                lon: 0.,
                lat: 0.,
            });
        }
    }
    Some(())
}

fn attribute_number(raw: &str) -> Cow<'_, str> {
    // Small integers already have the required text. Keep negative zero on the reference path.
    if raw.len() <= 15
        && raw != "-0"
        && raw
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_digit() || (i == 0 && b == b'-'))
    {
        Cow::Borrowed(raw)
    } else {
        Cow::Owned(json::number(raw))
    }
}

fn coordinate(p: &mut json::Parser<'_>) -> Option<f64> {
    p.space();
    let start = p.i;
    let bytes = p.s.as_bytes();
    let negative = bytes.get(p.i) == Some(&b'-');
    p.i += usize::from(negative);
    let digits_start = p.i;
    let mut n = 0u64;
    while let Some(&b @ b'0'..=b'9') = bytes.get(p.i) {
        n = n.wrapping_mul(10).wrapping_add((b - b'0') as u64);
        p.i += 1;
    }
    let integer_end = p.i;
    let mut fraction = 0;
    if bytes.get(p.i) == Some(&b'.') {
        p.i += 1;
        let fraction_start = p.i;
        while let Some(&b @ b'0'..=b'9') = bytes.get(p.i) {
            n = n.wrapping_mul(10).wrapping_add((b - b'0') as u64);
            p.i += 1;
        }
        fraction = p.i - fraction_start;
        if fraction == 0 {
            p.i = start;
            return reference_coordinate(p);
        }
    }
    if integer_end == digits_start
        || (integer_end - digits_start > 1 && bytes[digits_start] == b'0')
        || integer_end - digits_start + fraction > 15
        || bytes.get(p.i).is_some_and(|b| *b == b'e' || *b == b'E')
    {
        p.i = start;
        return reference_coordinate(p);
    }
    const POW10: [f64; 16] = [
        1.,
        10.,
        100.,
        1000.,
        10000.,
        100000.,
        1000000.,
        10000000.,
        100000000.,
        1000000000.,
        10000000000.,
        100000000000.,
        1000000000000.,
        10000000000000.,
        100000000000000.,
        1000000000000000.,
    ];
    // Both operands are exact integers. Division gives the correctly rounded decimal value.
    let v = n as f64 / POW10[fraction];
    Some(if negative { -v } else { v })
}
fn reference_coordinate(p: &mut json::Parser<'_>) -> Option<f64> {
    let json::Value::Number(raw) = p.value(5).ok()? else {
        return None;
    };
    raw.parse().ok()
}

fn numeric_text<'a>(p: &mut json::Parser<'a>) -> Option<&'a str> {
    p.space();
    let start = p.i;
    let bytes = p.s.as_bytes();
    let negative = bytes.get(p.i) == Some(&b'-');
    let mut i = p.i + usize::from(negative);
    let first = i;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if i > first
        && !(i - first > 1 && bytes[first] == b'0')
        && bytes
            .get(i)
            .is_some_and(|b| matches!(b, b',' | b'}') || b.is_ascii_whitespace())
    {
        p.i = i;
        return Some(&p.s[start..i]);
    }
    let json::Value::Number(raw) = p.value(2).ok()? else {
        return None;
    };
    Some(raw)
}
