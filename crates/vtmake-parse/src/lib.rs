//! Read NDJSON features. Coordinates stay in longitude and latitude units.
pub mod dtoa;
mod json;
pub mod stream;
pub mod pull;
pub mod geojson;
pub mod csv;
pub mod fgb;
pub mod js;
pub mod pgcopy;
use vtmake_core::{Attr, AttrType, Draw, Feature, GeomType, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use json::Value;
use rayon::prelude::*;
use std::path::Path;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub exit_code: i32,
    pub message: String,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ParseError {}
impl ParseError {
    /// Write the message and stop with the exit code.
    pub fn exit(&self) -> ! {
        eprintln!("{}", self.message);
        std::process::exit(self.exit_code)
    }
}
fn error(exit_code: i32, message: &str) -> ParseError {
    ParseError {
        exit_code,
        message: message.into(),
    }
}
/// Warning kinds. Use `once` to select warnings that occur once per stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Warning {
    MissingProperties,
    MissingGeometry,
    MissingType,
    InvalidType,
    UnknownGeometry,
    MissingCoordinates,
    ExpectedArray,
    ExtraDimensions,
    NegativeId,
    FractionalId,
    LargeId,
    NonNumericId,
    Crs,
}
impl Warning {
    pub fn once(self) -> bool {
        matches!(
            self,
            Self::MissingType
                | Self::ExtraDimensions
                | Self::NegativeId
                | Self::FractionalId
                | Self::LargeId
                | Self::NonNumericId
        )
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingProperties => "feature without properties hash",
            Self::MissingGeometry => "feature with no geometry",
            Self::MissingType => "null geometry (additional not reported)",
            Self::InvalidType => "geometry type is not a string",
            Self::UnknownGeometry => "Can't handle geometry type",
            Self::MissingCoordinates => "feature without coordinates array",
            Self::ExpectedArray => "expected array for geometry",
            Self::ExtraDimensions => "ignoring dimensions beyond two",
            Self::NegativeId => "Can't represent negative feature ID",
            Self::FractionalId => "Can't represent non-integer feature ID",
            Self::LargeId => "Can't represent too-large feature ID",
            Self::NonNumericId => "Can't represent non-numeric feature ID",
            Self::Crs => "GeoJSON specified an unexpected projection",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub op: u8,
    pub lon: f64,
    pub lat: f64,
}
/// A polygon CLOSEPATH marks the end of a whole Polygon, as in tippecanoe.
/// A MOVETO marks each ring. Call fix_polygon after projection.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFeature {
    pub geom_type: GeomType,
    pub geom: Vec<Position>,
    pub attrs: Vec<Attr>,
    pub minzoom: i8,
    pub maxzoom: i8,
    pub layer: Option<Box<str>>,
    pub id: Option<u64>,
    pub seq: u64,
}
impl ParsedFeature {
    /// Project the draws. The caller must fix polygon winding and set the index.
    pub fn project(&self, mut project: impl FnMut(f64, f64) -> (i64, i64)) -> Feature {
        let mut bbox = [i64::MAX, i64::MAX, i64::MIN, i64::MIN];
        let geom = self
            .geom
            .iter()
            .map(|p| {
                if p.op == VT_CLOSEPATH {
                    return Draw::new(p.op, 0, 0);
                }
                let (x, y) = project(p.lon, p.lat);
                bbox[0] = bbox[0].min(x);
                bbox[1] = bbox[1].min(y);
                bbox[2] = bbox[2].max(x);
                bbox[3] = bbox[3].max(y);
                Draw::new(p.op, x, y)
            })
            .collect();
        Feature {
            geom_type: self.geom_type,
            geom,
            bbox,
            minzoom: self.minzoom,
            maxzoom: self.maxzoom,
            layer: self.layer.clone(),
            id: self.id,
            attrs: self.attrs.clone(),
            index: 0,
            seq: self.seq,
        }
    }
}
/// Output storage for one line. A collection can produce more than one feature.
#[derive(Debug, Default)]
pub struct ParsedLine {
    pub features: Vec<ParsedFeature>,
    pub warnings: Vec<Warning>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Features(usize),
    Skipped,
}
fn warn(warnings: &mut Vec<Warning>, w: Warning) {
    if !w.once() || !warnings.contains(&w) {
        warnings.push(w);
    }
}
/// Parse one line. This function does not write warnings to stderr.
pub fn parse_feature(line: &[u8], out: &mut ParsedLine) -> Result<Outcome, ParseError> {
    out.features.clear();
    out.warnings.clear();
    let s = std::str::from_utf8(line).map_err(|_| error(119, "Invalid UTF-8"))?;
    if s.trim().is_empty() {
        return Ok(Outcome::Skipped);
    }
    let root = json::parse(s)?;
    parse_root(&root, out)?;
    Ok(if out.features.is_empty() {
        Outcome::Skipped
    } else {
        Outcome::Features(out.features.len())
    })
}
fn parse_root(root: &Value<'_>, out: &mut ParsedLine) -> Result<(), ParseError> {
    match root.get("type").and_then(Value::string) {
        Some("FeatureCollection") => {
            if root
                .get("crs")
                .and_then(|c| c.get("properties"))
                .and_then(|p| p.get("name"))
                .and_then(Value::string)
                .is_some_and(|s| s != "urn:ogc:def:crs:OGC:1.3:CRS84")
            {
                warn(&mut out.warnings, Warning::Crs);
            }
            if let Some(Value::Array(features)) = root.get("features") {
                for f in features {
                    parse_root(f, out)?;
                }
            }
        }
        Some("Feature") => {
            let Some(g) = root.get("geometry") else {
                warn(&mut out.warnings, Warning::MissingGeometry);
                return Ok(());
            };
            let props = match root.get("properties") {
                Some(Value::Object(v)) => Some(v),
                Some(Value::Null) => None,
                _ => {
                    warn(&mut out.warnings, Warning::MissingProperties);
                    return Ok(());
                }
            };
            let mut template = ParsedFeature {
                geom_type: GeomType::Point,
                geom: Vec::new(),
                attrs: Vec::new(),
                minzoom: -1,
                maxzoom: -1,
                layer: None,
                id: None,
                seq: 0,
            };
            if !has_supported_geometry(g) {
                geometry(g, &template, out)?;
                return Ok(());
            }
            if let Some(props) = props {
                for (key, value) in props {
                    let (ty, s) = match value {
                        Value::String(s) => (AttrType::String, s.to_string()),
                        Value::Number(n) => (AttrType::Number, json::number(n)),
                        Value::Bool(v) => (AttrType::Bool, v.to_string()),
                        Value::Null => (AttrType::Null, "null".into()),
                        v => {
                            let mut s = String::new();
                            json::stringify(v, &mut s);
                            (AttrType::String, s)
                        }
                    };
                    template.attrs.push(Attr {
                        key: key.as_ref().into(),
                        ty,
                        value: s.into(),
                    });
                }
            }
            if let Some(tc) = root.get("tippecanoe") {
                for (key, field) in [
                    ("minzoom", &mut template.minzoom),
                    ("maxzoom", &mut template.maxzoom),
                ] {
                    if let Some(n) = tc.get(key).and_then(Value::number) {
                        let n = dtoa::dtoa_milo(n).parse::<f64>().unwrap_or(f64::NAN);
                        if !n.is_finite() || n.fract() != 0. || !(0. ..=32.).contains(&n) {
                            return Err(error(107, "Zoom must be an integer from 0 through 32"));
                        }
                        *field = n as i8;
                    }
                }
                template.layer = tc.get("layer").and_then(Value::string).map(Into::into);
            }
            if let Some(id) = root.get("id") {
                if let Value::Number(raw) = id {
                    let n = id.number().unwrap_or(f64::NAN);
                    let text = json::number(raw);
                    if n < 0. {
                        warn(&mut out.warnings, Warning::NegativeId);
                    } else if let Ok(id) = text.parse::<u64>() {
                        template.id = Some(id);
                    } else if !text.bytes().all(|b| b.is_ascii_digit()) {
                        warn(&mut out.warnings, Warning::FractionalId);
                    } else {
                        warn(&mut out.warnings, Warning::LargeId);
                    }
                } else {
                    warn(&mut out.warnings, Warning::NonNumericId);
                }
            }
            geometry(g, &template, out)?;
        }
        _ => {}
    }
    Ok(())
}
fn has_supported_geometry(g: &Value<'_>) -> bool {
    fn single(g: &Value<'_>) -> bool {
        matches!(
            g.get("type").and_then(Value::string),
            Some(
                "Point"
                    | "MultiPoint"
                    | "LineString"
                    | "MultiLineString"
                    | "Polygon"
                    | "MultiPolygon"
            )
        ) && matches!(g.get("coordinates"), Some(Value::Array(_)))
    }
    if let Some(Value::Array(v)) = g.get("geometries") {
        v.iter().any(single)
    } else {
        single(g)
    }
}
fn geometry(
    g: &Value<'_>,
    template: &ParsedFeature,
    out: &mut ParsedLine,
) -> Result<(), ParseError> {
    if let Some(Value::Array(parts)) = g.get("geometries") {
        for part in parts {
            single_geometry(part, template, out)?;
        }
        return Ok(());
    }
    single_geometry(g, template, out)
}
fn single_geometry(
    g: &Value<'_>,
    template: &ParsedFeature,
    out: &mut ParsedLine,
) -> Result<(), ParseError> {
    let Some(geometry_type) = g.get("type") else {
        warn(&mut out.warnings, Warning::MissingType);
        return Ok(());
    };
    let Some(ty) = geometry_type.string() else {
        warn(&mut out.warnings, Warning::InvalidType);
        return Ok(());
    };
    let Some(c @ Value::Array(_)) = g.get("coordinates") else {
        warn(&mut out.warnings, Warning::MissingCoordinates);
        return Ok(());
    };
    let (kind, depth) = match ty {
        "Point" => (GeomType::Point, 0),
        "MultiPoint" => (GeomType::Point, 1),
        "LineString" => (GeomType::Line, 1),
        "MultiLineString" => (GeomType::Line, 2),
        "Polygon" => (GeomType::Polygon, 2),
        "MultiPolygon" => (GeomType::Polygon, 3),
        _ => {
            warn(&mut out.warnings, Warning::UnknownGeometry);
            return Ok(());
        }
    };
    let mut feature = template.clone();
    feature.geom_type = kind;
    coordinates(
        c,
        depth,
        kind,
        VT_MOVETO,
        &mut feature.geom,
        &mut out.warnings,
    )?;
    out.features.push(feature);
    Ok(())
}
fn coordinates(
    c: &Value<'_>,
    depth: usize,
    kind: GeomType,
    op: u8,
    out: &mut Vec<Position>,
    warnings: &mut Vec<Warning>,
) -> Result<(), ParseError> {
    let Value::Array(a) = c else {
        warn(warnings, Warning::ExpectedArray);
        return Ok(());
    };
    if depth == 0 {
        let (Some(lon), Some(lat)) = (
            a.first().and_then(Value::number),
            a.get(1).and_then(Value::number),
        ) else {
            return Err(error(107, "malformed point"));
        };
        if a.len() > 2 {
            warn(warnings, Warning::ExtraDimensions);
        }
        out.push(Position { op, lon, lat });
    } else {
        for (i, c) in a.iter().enumerate() {
            let op = if depth == 1 && i > 0 && kind != GeomType::Point {
                VT_LINETO
            } else {
                VT_MOVETO
            };
            coordinates(c, depth - 1, kind, op, out, warnings)?;
        }
        if depth == 2 && kind == GeomType::Polygon {
            out.push(Position {
                op: VT_CLOSEPATH,
                lon: 0.,
                lat: 0.,
            });
        }
    }
    Ok(())
}
/// Map a file and parse N chunks. seq is the byte offset of the source line.
/// Features from a GeometryCollection share seq and keep collection order.
/// The mapped file must not change until this call returns.
pub fn read_parallel(path: &Path, threads: usize) -> Result<ParsedLine, ParseError> {
    let file = std::fs::File::open(path).map_err(|e| error(107, &e.to_string()))?;
    if file
        .metadata()
        .map_err(|e| error(107, &e.to_string()))?
        .len()
        == 0
    {
        return Err(error(110, "Did not read any valid geometries"));
    }
    // SAFETY: callers must keep the file unchanged while it is mapped.
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| error(107, &e.to_string()))?;
    parse_parallel(&map, threads)
}
/// Parse an immutable input buffer. Results keep source order for all thread counts.
pub fn parse_parallel(data: &[u8], threads: usize) -> Result<ParsedLine, ParseError> {
    let n = threads.max(1);
    let mut starts = vec![0];
    for i in 1..n {
        let at = data.len() / n * i;
        if let Some(j) = memchr::memchr(b'\n', &data[at..]) {
            let end = at + j + 1;
            if end > *starts.last().unwrap() {
                starts.push(end);
            }
        }
    }
    if *starts.last().unwrap() != data.len() {
        starts.push(data.len());
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .build()
        .map_err(|e| error(107, &e.to_string()))?;
    let parts: Vec<Result<ParsedLine, ParseError>> = pool.install(|| {
        starts
            .par_windows(2)
            .map(|range| {
                let mut result = ParsedLine::default();
                let mut line = ParsedLine::default();
                let mut offset = range[0];
                for raw in data[range[0]..range[1]].split(|&c| c == b'\n') {
                    parse_feature(raw, &mut line)?;
                    for f in &mut line.features {
                        f.seq = offset as u64;
                    }
                    result.features.append(&mut line.features);
                    for w in line.warnings.drain(..) {
                        warn(&mut result.warnings, w);
                    }
                    offset += raw.len() + 1;
                }
                Ok(result)
            })
            .collect()
    });
    let mut result = ParsedLine::default();
    for part in parts {
        let mut part = part?;
        result.features.append(&mut part.features);
        for w in part.warnings {
            warn(&mut result.warnings, w);
        }
    }
    if result
        .features
        .iter()
        .all(|f| !f.geom.iter().any(|d| d.op != VT_CLOSEPATH))
    {
        return Err(error(110, "Did not read any valid geometries"));
    }
    Ok(result)
}
