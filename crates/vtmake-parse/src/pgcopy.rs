//! Postgres binary COPY input (`COPY (…) TO STDOUT (FORMAT binary)`).
//!
//! The input gives one feature per row: a GeoJSON geometry column (text from
//! `ST_AsGeoJSON`) and typed property columns. A [`Schema`] tells the role
//! and the type of each column, and how the attributes are made.
//!
//! The rules copy a Node.js NDJSON exporter (the reference path in
//! `docs/pgcopy.md`): Postgres `to_jsonb(src) - 'geom'` (optionally
//! `jsonb_strip_nulls`), node-pg `JSON.parse`, the feature object
//! `{type, geometry, properties: {layer, ...props, tippecanoe_minzoom,
//! tippecanoe_maxzoom}, tippecanoe: {layer, minzoom, maxzoom}}`, and
//! `JSON.stringify`. The result is the same features, with the same
//! attribute order, types and text, as vtmake's NDJSON reader gives for that
//! text. See `docs/pgcopy.md`.
//!
//! Each row takes the direct path. A row that the direct path cannot do
//! exactly (an unusual geometry, a property object with `"type": "Feature"`,
//! a zoom that is not an integer) takes the reference path: the decoder
//! writes the NDJSON line that Node would write and reads it with the NDJSON
//! parsers ([`crate::stream`], else the jsonpull port). `VTMAKE_PGCOPY_CHECK=1`
//! runs both paths on each row and stops at the first difference.

use crate::js::{self, Value};
use crate::{ParseError, ParsedFeature, error};
use vtmake_core::{AttrType, GeomType, VT_CLOSEPATH};

/// The 11-byte signature of the binary COPY format.
pub const SIGNATURE: &[u8; 11] = b"PGCOPY\n\xff\r\n\0";

/// Exit codes: a schema error (as a bad option), bad input data.
pub const EXIT_SCHEMA: i32 = 101;
pub const EXIT_DATA: i32 = 107;

/// The decoded types. Other Postgres types must be sent as `jsonb`
/// (`to_jsonb(col)`), see `docs/pgcopy.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `text`, `varchar`, `bpchar`, `name`: UTF-8 text.
    Text,
    Uuid,
    Bool,
    Int2,
    Int4,
    Int8,
    Numeric,
    Json,
    Jsonb,
}

impl Kind {
    fn from_name(n: &str) -> Option<Kind> {
        Some(match n {
            "text" | "varchar" | "character varying" | "bpchar" | "character" | "name" => Kind::Text,
            "uuid" => Kind::Uuid,
            "bool" | "boolean" => Kind::Bool,
            "int2" | "smallint" => Kind::Int2,
            "int4" | "integer" | "int" => Kind::Int4,
            "int8" | "bigint" => Kind::Int8,
            "numeric" | "decimal" => Kind::Numeric,
            "json" => Kind::Json,
            "jsonb" => Kind::Jsonb,
            _ => return None,
        })
    }

    /// The built-in type OIDs (`pg_type.dat`).
    fn from_oid(oid: f64) -> Option<Kind> {
        Some(match oid as i64 {
            25 | 1043 | 1042 | 19 => Kind::Text,
            2950 => Kind::Uuid,
            16 => Kind::Bool,
            21 => Kind::Int2,
            23 => Kind::Int4,
            20 => Kind::Int8,
            1700 => Kind::Numeric,
            114 => Kind::Json,
            3802 => Kind::Jsonb,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Geometry,
    Property,
    Ignore,
}

#[derive(Debug)]
struct Column {
    name: String,
    kind: Kind,
    role: Role,
}

/// The feature minzoom rule of the reference exporter (`zoom` in the schema).
#[derive(Debug)]
struct ZoomRule {
    /// `zoomField` then `fallbackZoomFields`: the property column of each
    /// field (`None`: no such property, the value is `undefined`).
    fields: Vec<Option<usize>>,
    /// `Number(defaultMinZoom)` (`resolveZoomValue(defaultMinZoom,
    /// layerMinZoom)` uses it if it is finite, else `layerMinZoom`).
    default: Result<f64, js::TypeError>,
    min: f64,
    max: f64,
}

/// The source of an attribute value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Src {
    /// A constant (index into `Schema::consts`).
    Const(usize),
    /// A property column.
    Col(usize),
    MinZoom,
    MaxZoom,
}

/// The settings of a COPY source (`--pgcopy-schema`).
#[derive(Debug)]
pub struct Schema {
    columns: Vec<Column>,
    layer: Option<Box<str>>,
    strip_nulls: bool,
    /// The constant attributes before the properties: key, value, and the
    /// attribute (type, text).
    consts: Vec<(String, Value, AttrType, String)>,
    /// The property columns in the key order of the properties object.
    props: Vec<usize>,
    minzoom_key: Option<String>,
    maxzoom_key: Option<String>,
    zoom: Option<ZoomRule>,
    /// `tippecanoe.maxzoom` and the maxzoom attribute.
    maxzoom: Option<f64>,
    /// The keys do not collide and none is an array index: the attribute
    /// order is `consts`, the present `props`, minzoom, maxzoom.
    simple: bool,
    /// A constant needs the reference path for each row.
    always_reference: bool,
}

fn schema_error(msg: impl std::fmt::Display) -> ParseError {
    ParseError { exit_code: EXIT_SCHEMA, message: format!("pgcopy schema: {msg}") }
}

fn get<'v>(o: &'v [(String, Value)], k: &str) -> Option<&'v Value> {
    o.iter().find(|m| m.0 == k).map(|m| &m.1)
}

fn opt_str(o: &[(String, Value)], k: &str) -> Result<Option<String>, ParseError> {
    match get(o, k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Str(s)) => Ok(Some(s.clone())),
        Some(_) => Err(schema_error(format!("\"{k}\" must be a string or null"))),
    }
}

fn opt_num(o: &[(String, Value)], k: &str, dflt: f64) -> Result<f64, ParseError> {
    match get(o, k) {
        None | Some(Value::Null) => Ok(dflt),
        Some(Value::Num(n)) if n.is_finite() => Ok(*n),
        Some(_) => Err(schema_error(format!("\"{k}\" must be a number"))),
    }
}

fn opt_bool(o: &[(String, Value)], k: &str, dflt: bool) -> Result<bool, ParseError> {
    match get(o, k) {
        None | Some(Value::Null) => Ok(dflt),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(schema_error(format!("\"{k}\" must be true or false"))),
    }
}

fn object<'v>(v: Option<&'v Value>, what: &str) -> Result<&'v [(String, Value)], ParseError> {
    match v {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Obj(o)) => Ok(o),
        Some(_) => Err(schema_error(format!("\"{what}\" must be an object"))),
    }
}

impl Schema {
    /// Read the schema JSON (`docs/pgcopy.md`). `minzoom` and `maxzoom` (the
    /// `-Z` and `-z` of the build) are the defaults of the zoom range.
    pub fn from_json(text: &str, minzoom: i32, maxzoom: i32) -> Result<Schema, ParseError> {
        let root = js::parse(text).map_err(|e| schema_error(format!("not valid JSON: {e}")))?;
        let Value::Obj(root) = root else { return Err(schema_error("not a JSON object")) };
        for (k, _) in &root {
            if !matches!(k.as_str(), "format" | "columns" | "layer" | "stripNulls" | "keyOrder" | "attributes" | "zoom") {
                return Err(schema_error(format!("unknown member \"{k}\"")));
            }
        }
        match get(&root, "format") {
            Some(Value::Str(f)) if f == "vtmake-pgcopy/1" => {}
            _ => return Err(schema_error("\"format\" must be \"vtmake-pgcopy/1\"")),
        }
        let Some(Value::Arr(cols)) = get(&root, "columns") else { return Err(schema_error("\"columns\" must be an array")) };
        let mut columns = Vec::with_capacity(cols.len());
        for (i, c) in cols.iter().enumerate() {
            let Value::Obj(c) = c else { return Err(schema_error(format!("column {i} must be an object"))) };
            for (k, _) in c {
                if !matches!(k.as_str(), "name" | "type" | "oid" | "role") {
                    return Err(schema_error(format!("column {i}: unknown member \"{k}\"")));
                }
            }
            let Some(name) = opt_str(c, "name")? else { return Err(schema_error(format!("column {i} has no \"name\""))) };
            let kind = match (get(c, "type"), get(c, "oid")) {
                (Some(Value::Str(t)), None) => Kind::from_name(t),
                (None, Some(Value::Num(o))) => Kind::from_oid(*o),
                (Some(Value::Str(t)), Some(Value::Num(o))) => match (Kind::from_name(t), Kind::from_oid(*o)) {
                    (Some(a), Some(b)) if a == b => Some(a),
                    _ => None,
                },
                _ => return Err(schema_error(format!("column \"{name}\" needs \"type\" (a name) or \"oid\" (a number)"))),
            }
            .ok_or_else(|| schema_error(format!("column \"{name}\": the type is not supported; send it as to_jsonb(...) (jsonb)")))?;
            let role = match opt_str(c, "role")?.as_deref() {
                None | Some("property") => Role::Property,
                Some("geometry") => Role::Geometry,
                Some("ignore") => Role::Ignore,
                Some(r) => return Err(schema_error(format!("column \"{name}\": unknown role \"{r}\""))),
            };
            // Postgres validates a `json` value only when it converts it to
            // jsonb (`to_jsonb(src)`: no `\u0000`, numeric range, duplicate
            // keys). The COPY statement must do that conversion.
            if role == Role::Property && kind == Kind::Json {
                return Err(schema_error(format!("column \"{name}\": json properties are not supported; send to_jsonb(col) (jsonb)")));
            }
            columns.push(Column { name, kind, role });
        }
        let geoms: Vec<usize> = (0..columns.len()).filter(|&i| columns[i].role == Role::Geometry).collect();
        let [_] = geoms[..] else { return Err(schema_error("exactly one column must have the role \"geometry\"")) };
        let mut props: Vec<usize> = (0..columns.len()).filter(|&i| columns[i].role == Role::Property).collect();
        for (n, &i) in props.iter().enumerate() {
            if props[..n].iter().any(|&j| columns[j].name == columns[i].name) {
                return Err(schema_error(format!("two property columns are named \"{}\"", columns[i].name)));
            }
        }
        match opt_str(&root, "keyOrder")?.as_deref() {
            None | Some("jsonb") => props.sort_by(|&a, &b| js::jsonb_key_cmp(&columns[a].name, &columns[b].name)),
            Some("columns") => {}
            Some(o) => return Err(schema_error(format!("unknown \"keyOrder\" \"{o}\""))),
        }
        // `JSON.parse` of the properties object: array index keys first.
        let mut keyed: Vec<(String, usize)> = props.iter().map(|&i| (columns[i].name.clone(), i)).collect();
        js::js_order(&mut keyed);
        let props: Vec<usize> = keyed.into_iter().map(|k| k.1).collect();

        let attrs = object(get(&root, "attributes"), "attributes")?;
        for (k, _) in attrs {
            if !matches!(k.as_str(), "before" | "minzoom" | "maxzoom") {
                return Err(schema_error(format!("attributes: unknown member \"{k}\"")));
            }
        }
        let mut consts = Vec::new();
        let mut always_reference = false;
        for (k, v) in object(get(attrs, "before"), "attributes.before")? {
            let mut text = String::new();
            let ty = attr_of_value(v, &mut text);
            always_reference |= needs_reference_value(v) || (k == "type" && is_feature_type(v));
            consts.push((k.clone(), v.clone(), ty, text));
        }
        let minzoom_key = opt_str(attrs, "minzoom")?;
        let maxzoom_key = opt_str(attrs, "maxzoom")?;

        let zoom_obj = match get(&root, "zoom") {
            None | Some(Value::Null) => None,
            Some(Value::Obj(o)) => Some(o.as_slice()),
            Some(_) => return Err(schema_error("\"zoom\" must be an object or null")),
        };
        let (zoom, maxzoom) = match zoom_obj {
            None => (None, None),
            Some(z) => {
                for (k, _) in z {
                    if !matches!(k.as_str(), "zoomField" | "fallbackZoomFields" | "defaultMinZoom" | "minZoom" | "maxZoom") {
                        return Err(schema_error(format!("zoom: unknown member \"{k}\"")));
                    }
                }
                let min = opt_num(z, "minZoom", f64::from(minzoom))?;
                let max = opt_num(z, "maxZoom", f64::from(maxzoom))?;
                let prop_col = |name: &str| props.iter().copied().find(|&i| columns[i].name == name);
                let mut fields = Vec::new();
                // `[zoomField, ...fallbackZoomFields]`; a falsy field gives `undefined`.
                match get(z, "zoomField") {
                    None | Some(Value::Null) => fields.push(None),
                    Some(Value::Str(f)) => fields.push(if f.is_empty() { None } else { prop_col(f) }),
                    Some(_) => return Err(schema_error("zoom.zoomField must be a string or null")),
                }
                match get(z, "fallbackZoomFields") {
                    None | Some(Value::Null) => {}
                    Some(Value::Arr(a)) => {
                        for f in a {
                            match f {
                                Value::Str(f) => fields.push(if f.is_empty() { None } else { prop_col(f) }),
                                Value::Null => fields.push(None),
                                _ => return Err(schema_error("zoom.fallbackZoomFields must hold strings")),
                            }
                        }
                    }
                    Some(_) => return Err(schema_error("zoom.fallbackZoomFields must be an array")),
                }
                // `resolveZoomValue(defaultMinZoom, layerMinZoom)`: an absent value is `undefined`.
                let default = js::to_number(get(z, "defaultMinZoom"));
                (Some(ZoomRule { fields, default, min, max }), Some(max))
            }
        };
        if (minzoom_key.is_some() || maxzoom_key.is_some()) && zoom.is_none() {
            return Err(schema_error("attributes.minzoom and attributes.maxzoom need \"zoom\""));
        }

        // The simple attribute order: no key in two groups, no array index key.
        let mut all: Vec<&str> = consts.iter().map(|c| c.0.as_str()).collect();
        all.extend(props.iter().map(|&i| columns[i].name.as_str()));
        all.extend(minzoom_key.iter().map(String::as_str));
        all.extend(maxzoom_key.iter().map(String::as_str));
        let mut sorted = all.clone();
        sorted.sort_unstable();
        let simple = sorted.windows(2).all(|w| w[0] != w[1]) && all.iter().all(|k| js::array_index(k).is_none());

        Ok(Schema {
            columns,
            layer: opt_str(&root, "layer")?.map(Into::into),
            strip_nulls: opt_bool(&root, "stripNulls", false)?,
            consts,
            props,
            minzoom_key,
            maxzoom_key,
            zoom,
            maxzoom,
            simple,
            always_reference,
        })
    }

    pub fn columns(&self) -> usize {
        self.columns.len()
    }
}

/// `{"type": "Feature"}` or `"FeatureCollection"`: tippecanoe's reader treats
/// such an object inside the properties in its own way.
fn is_feature_type(v: &Value) -> bool {
    matches!(v, Value::Str(s) if s == "Feature" || s == "FeatureCollection")
}

/// The value has an object with a Feature `type` at some depth.
fn needs_reference_value(v: &Value) -> bool {
    match v {
        Value::Arr(a) => a.iter().any(needs_reference_value),
        Value::Obj(o) => o.iter().any(|(k, v)| (k == "type" && is_feature_type(v)) || needs_reference_value(v)),
        _ => false,
    }
}

/// The attribute of a property value as vtmake's NDJSON reader gives it for
/// the `JSON.stringify` text of the value.
fn attr_of_value(v: &Value, out: &mut String) -> AttrType {
    match v {
        Value::Null => {
            out.push_str("null");
            AttrType::Null
        }
        Value::Bool(b) => {
            out.push_str(if *b { "true" } else { "false" });
            AttrType::Bool
        }
        Value::Num(n) => number_attr(*n, out),
        Value::Str(s) => {
            out.push_str(s);
            AttrType::String
        }
        _ => {
            reader_text(v, out);
            AttrType::String
        }
    }
}

/// A number property: `JSON.stringify` gives `null` for a value that is not
/// finite, else the `Number::toString` text, which the reader keeps
/// (`stream::attribute_number`).
fn number_attr(n: f64, out: &mut String) -> AttrType {
    if !n.is_finite() {
        out.push_str("null");
        return AttrType::Null;
    }
    let mut t = String::with_capacity(24);
    js::number_text(n, &mut t);
    out.push_str(&crate::stream::attribute_number(&t));
    AttrType::Number
}

/// The text that the jsonpull port (`Pull::stringify`) gives for the
/// `JSON.stringify` text of a nested value.
fn reader_text(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => {
            if n.is_finite() {
                let mut t = String::with_capacity(24);
                js::number_text(*n, &mut t);
                out.push_str(&crate::json::number(&t));
            } else {
                out.push_str("null");
            }
        }
        Value::Str(s) => reader_quote(s, out),
        Value::Arr(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                reader_text(e, out);
            }
            out.push(']');
        }
        Value::Obj(o) => {
            out.push('{');
            for (i, (k, e)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                reader_quote(k, out);
                out.push(':');
                reader_text(e, out);
            }
            out.push('}');
        }
    }
}

fn reader_quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// What the binary COPY stream has next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// A row of this many bytes.
    Row(usize),
    /// The trailer (2 bytes): the end of the data.
    Trailer,
    /// More bytes are necessary.
    More,
}

fn be16(b: &[u8]) -> i16 {
    i16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> i32 {
    i32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// The length of the file header, or `None` if more bytes are necessary.
pub fn header_len(buf: &[u8]) -> Result<Option<usize>, ParseError> {
    let n = buf.len().min(SIGNATURE.len());
    if buf[..n] != SIGNATURE[..n] {
        return Err(error(EXIT_DATA, "pgcopy: the input is not in the binary COPY format (bad signature)"));
    }
    if buf.len() < 19 {
        return Ok(None);
    }
    let flags = be32(&buf[11..]) as u32;
    if flags & (1 << 16) != 0 {
        return Err(error(EXIT_DATA, "pgcopy: COPY WITH OIDS is not supported"));
    }
    if flags & 0xffff_0000 & !(1 << 16) != 0 {
        return Err(error(EXIT_DATA, "pgcopy: unknown critical flag in the COPY header"));
    }
    let ext = be32(&buf[15..]);
    if ext < 0 {
        return Err(error(EXIT_DATA, "pgcopy: bad header extension length"));
    }
    let len = 19 + ext as usize;
    Ok((buf.len() >= len).then_some(len))
}

/// The next item at the start of `buf` (after the header).
pub fn next(buf: &[u8], ncols: usize) -> Result<Next, ParseError> {
    if buf.len() < 2 {
        return Ok(Next::More);
    }
    let n = be16(buf);
    if n == -1 {
        return Ok(Next::Trailer);
    }
    if n as usize != ncols || n < 0 {
        return Err(error(EXIT_DATA, &format!("pgcopy: a row has {n} columns; the schema has {ncols}")));
    }
    let mut p = 2usize;
    for _ in 0..ncols {
        if buf.len() < p + 4 {
            return Ok(Next::More);
        }
        let len = be32(&buf[p..]);
        p += 4;
        if len >= 0 {
            p += len as usize;
        } else if len != -1 {
            return Err(error(EXIT_DATA, "pgcopy: bad field length"));
        }
    }
    Ok(if buf.len() >= p { Next::Row(p) } else { Next::More })
}

/// One decoded property value.
#[derive(Debug)]
enum Cell {
    /// Not in the properties object (`jsonb_strip_nulls`).
    Absent,
    Null,
    Bool(bool),
    Num(f64),
    /// A string at `[start, end)` of the row text.
    Str(u32, u32),
    /// A string constant.
    Static(&'static str),
    /// A value from `json` or `jsonb` that is an array or an object.
    Tree(Value),
}

/// An attribute key: a schema key or a key of a reference-path feature.
#[derive(Clone, Copy, Debug)]
enum Key {
    Col(u32),
    Const(u32),
    MinZoom,
    MaxZoom,
    Own(u32),
}

/// The features of one row.
#[derive(Default)]
pub struct RowOut {
    /// The features (more than one for a GeometryCollection on the
    /// reference path). `attrs` of a feature is empty: see [`RowOut::attrs`].
    pub features: Vec<ParsedFeature>,
    /// The number of `features` in use.
    pub count: usize,
    /// The row has a geometry that the exporter does not skip (the exporter
    /// counts these rows).
    pub has_geometry: bool,
    attrs: Vec<(Key, AttrType, u32, u32)>,
    /// For each feature: the end of its attributes in `attrs`.
    attr_ends: Vec<usize>,
    /// The texts of the cells and of the attributes.
    text: String,
    /// The keys of reference-path attributes.
    own_keys: Vec<String>,
    cells: Vec<Cell>,
}

impl RowOut {
    /// The attributes of feature `i`: key, type, text.
    pub fn attrs<'a>(&'a self, schema: &'a Schema, i: usize) -> impl ExactSizeIterator<Item = (&'a str, AttrType, &'a str)> + 'a {
        let start = if i == 0 { 0 } else { self.attr_ends[i - 1] };
        self.attrs[start..self.attr_ends[i]].iter().map(move |&(k, ty, s, e)| {
            let key = match k {
                Key::Col(c) => schema.columns[c as usize].name.as_str(),
                Key::Const(c) => schema.consts[c as usize].0.as_str(),
                Key::MinZoom => schema.minzoom_key.as_deref().unwrap_or(""),
                Key::MaxZoom => schema.maxzoom_key.as_deref().unwrap_or(""),
                Key::Own(o) => self.own_keys[o as usize].as_str(),
            };
            (key, ty, &self.text[s as usize..e as usize])
        })
    }

    fn feature(&mut self) -> &mut ParsedFeature {
        if self.features.len() == self.count {
            self.features.push(ParsedFeature {
                geom_type: GeomType::Point,
                geom: Vec::new(),
                attrs: Vec::new(),
                minzoom: -1,
                maxzoom: -1,
                layer: None,
                id: None,
                seq: 0,
            });
        }
        self.count += 1;
        &mut self.features[self.count - 1]
    }
}

/// Decodes rows with a schema. One decoder per thread.
pub struct Decoder<'s> {
    schema: &'s Schema,
    settings: crate::geojson::Settings,
    fname: String,
    check: bool,
    numeric: String,
    order: Vec<(String, Src)>,
    /// The nesting limit for JSON values (see [`Decoder::row`]).
    max_depth: usize,
}

/// An internal exit code: a JSON value has more than `max_depth` levels.
const TOO_DEEP: i32 = -1077;

/// The stack of the thread for rows with deep JSON values (virtual memory;
/// only the used part is resident). See `js::MAX_DEPTH` for the measured use.
const DEEP_STACK: usize = 1 << 30;

/// The result of the direct path for a row.
enum Direct {
    Done,
    /// The reference path must read the row.
    Reference,
}

impl<'s> Decoder<'s> {
    pub fn new(schema: &'s Schema, settings: crate::geojson::Settings, fname: &str) -> Self {
        Decoder {
            schema,
            settings,
            fname: fname.to_string(),
            check: std::env::var_os("VTMAKE_PGCOPY_CHECK").is_some_and(|v| v != "0" && !v.is_empty()),
            numeric: String::new(),
            order: Vec::new(),
            max_depth: js::FAST_DEPTH,
        }
    }

    /// Decode one row (the bytes that [`next`] gave). `out.count` is the
    /// number of features (0: the row has no geometry or no feature).
    ///
    /// A row with a JSON value deeper than [`js::FAST_DEPTH`] levels is
    /// decoded again on a thread with a 1 GiB stack, up to
    /// [`js::MAX_DEPTH`] levels (vtmake's NDJSON reader has no limit). A
    /// deeper value is rejected (exit 107) by a scan without recursion.
    pub fn row(&mut self, row: &[u8], out: &mut RowOut) -> Result<(), ParseError> {
        match self.row_at(row, out) {
            Err(e) if e.exit_code == TOO_DEEP => {
                let mut deep = Decoder {
                    schema: self.schema,
                    settings: self.settings.clone(),
                    fname: self.fname.clone(),
                    check: self.check,
                    numeric: String::new(),
                    order: Vec::new(),
                    max_depth: js::MAX_DEPTH,
                };
                let r = std::thread::scope(|sc| {
                    let h = std::thread::Builder::new()
                        .name("vtmake-pgcopy-deep".into())
                        .stack_size(DEEP_STACK)
                        .spawn_scoped(sc, || {
                            let r = deep.row_at(row, out);
                            // Free the deep values on this stack.
                            out.cells.clear();
                            r
                        })
                        .map_err(|e| error(EXIT_DATA, &format!("pgcopy: cannot start a thread: {e}")))?;
                    h.join().map_err(|_| error(EXIT_DATA, "pgcopy: the thread for a deep row failed"))?
                });
                match r {
                    Err(e) if e.exit_code == TOO_DEEP => Err(error(EXIT_DATA, &format!("pgcopy: a JSON value has more than {} nested levels", js::MAX_DEPTH))),
                    r => r,
                }
            }
            r => r,
        }
    }

    fn row_at(&mut self, row: &[u8], out: &mut RowOut) -> Result<(), ParseError> {
        out.count = 0;
        out.has_geometry = false;
        out.attrs.clear();
        out.attr_ends.clear();
        out.text.clear();
        out.own_keys.clear();
        let geom = self.cells(row, out)?;
        // `if (!geometry) continue`, before all other steps.
        let Some(geom) = geom else { return Ok(()) };
        if is_falsy(&out.text[geom.0 as usize..geom.1 as usize]) {
            return Ok(());
        }
        out.has_geometry = true;
        // The zoom rule runs for each row with a geometry; it can throw a
        // TypeError (then the Node.js export fails).
        let zoom = match &self.schema.zoom {
            None => (None, None),
            Some(z) => (Some(self.minzoom(z, out)?), self.schema.maxzoom),
        };
        let direct = if self.schema.always_reference { Direct::Reference } else { self.direct(geom, out, zoom) };
        match direct {
            Direct::Done if !self.check => Ok(()),
            Direct::Done => {
                let mut r = RowOut::default();
                r.cells = std::mem::take(&mut out.cells);
                r.text = out.text.clone();
                let line = self.line(geom, &r, zoom);
                out.cells = std::mem::take(&mut r.cells);
                let line = line?;
                self.reference(&line, &mut r);
                let a = summary(self.schema, out);
                let b = summary(self.schema, &r);
                if a != b {
                    return Err(error(EXIT_DATA, &format!("pgcopy check: the direct path differs from the NDJSON reader for the line {line}\ndirect: {a:?}\nreference: {b:?}")));
                }
                Ok(())
            }
            Direct::Reference => {
                out.count = 0;
                out.attrs.clear();
                out.attr_ends.clear();
                let line = self.line(geom, out, zoom)?;
                // The cell texts stay at the start of `out.text`.
                self.reference(&line, out);
                Ok(())
            }
        }
    }

    /// Decode the cells into `out.cells` (one per column); the geometry
    /// text as a range of `out.text`, or `None` if it is NULL.
    fn cells(&mut self, row: &[u8], out: &mut RowOut) -> Result<Option<(u32, u32)>, ParseError> {
        let s = self.schema;
        out.cells.clear();
        let mut p = 2usize;
        let mut geom = None;
        for col in &s.columns {
            let len = be32(&row[p..]);
            p += 4;
            let data = if len < 0 {
                None
            } else {
                let d = &row[p..p + len as usize];
                p += len as usize;
                Some(d)
            };
            if col.role == Role::Ignore {
                out.cells.push(Cell::Absent);
                continue;
            }
            if col.role == Role::Geometry {
                if let Some(d) = data {
                    let d = match col.kind {
                        Kind::Jsonb => jsonb_text(d)?,
                        Kind::Text | Kind::Json => d,
                        _ => return Err(error(EXIT_SCHEMA, "pgcopy schema: the geometry column must be text, json or jsonb")),
                    };
                    let t = std::str::from_utf8(d).map_err(|_| error(119, "pgcopy: invalid UTF-8 in the geometry"))?;
                    let start = out.text.len() as u32;
                    out.text.push_str(t);
                    geom = Some((start, out.text.len() as u32));
                }
                out.cells.push(Cell::Absent);
                continue;
            }
            let cell = match data {
                None => Cell::Null,
                Some(d) => self.cell(&col.name, col.kind, d, &mut out.text)?,
            };
            out.cells.push(if s.strip_nulls && matches!(cell, Cell::Null) { Cell::Absent } else { cell });
        }
        if p != row.len() {
            return Err(error(EXIT_DATA, "pgcopy: bad row length"));
        }
        Ok(geom)
    }

    fn cell(&mut self, name: &str, kind: Kind, d: &[u8], text: &mut String) -> Result<Cell, ParseError> {
        let bad = |what: &str| error(EXIT_DATA, &format!("pgcopy: column \"{name}\": bad {what} value"));
        Ok(match kind {
            Kind::Text => {
                let t = std::str::from_utf8(d).map_err(|_| error(119, &format!("pgcopy: column \"{name}\": invalid UTF-8")))?;
                let start = text.len() as u32;
                text.push_str(t);
                Cell::Str(start, text.len() as u32)
            }
            Kind::Uuid => {
                if d.len() != 16 {
                    return Err(bad("uuid"));
                }
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let start = text.len() as u32;
                for (i, b) in d.iter().enumerate() {
                    if matches!(i, 4 | 6 | 8 | 10) {
                        text.push('-');
                    }
                    text.push(HEX[(b >> 4) as usize] as char);
                    text.push(HEX[(b & 15) as usize] as char);
                }
                Cell::Str(start, text.len() as u32)
            }
            Kind::Bool => match d {
                [0] => Cell::Bool(false),
                [1] => Cell::Bool(true),
                _ => return Err(bad("bool")),
            },
            Kind::Int2 => {
                let b: [u8; 2] = d.try_into().map_err(|_| bad("int2"))?;
                Cell::Num(f64::from(i16::from_be_bytes(b)))
            }
            Kind::Int4 => {
                let b: [u8; 4] = d.try_into().map_err(|_| bad("int4"))?;
                Cell::Num(f64::from(i32::from_be_bytes(b)))
            }
            Kind::Int8 => {
                let b: [u8; 8] = d.try_into().map_err(|_| bad("int8"))?;
                // `as` rounds to the nearest double, as `JSON.parse` of the text does.
                Cell::Num(i64::from_be_bytes(b) as f64)
            }
            Kind::Numeric => self.numeric(d).ok_or_else(|| bad("numeric"))?,
            Kind::Json => return Err(error(EXIT_SCHEMA, "pgcopy schema: json properties are not supported")),
            Kind::Jsonb => {
                let t = std::str::from_utf8(jsonb_text(d)?).map_err(|_| error(119, &format!("pgcopy: column \"{name}\": invalid UTF-8")))?;
                let mut v = js::parse_depth(t, self.max_depth).map_err(|e| json_error(&format!("column \"{name}\""), e))?;
                if self.schema.strip_nulls {
                    js::strip_nulls(&mut v);
                }
                match v {
                    Value::Null => Cell::Null,
                    Value::Bool(b) => Cell::Bool(b),
                    Value::Num(n) => Cell::Num(n),
                    Value::Str(s) => {
                        let start = text.len() as u32;
                        text.push_str(&s);
                        Cell::Str(start, text.len() as u32)
                    }
                    v => Cell::Tree(v),
                }
            }
        })
    }

    /// `numeric` (binary): the value as `JSON.parse` reads the `jsonb`
    /// text. NaN and the infinities are strings in `to_jsonb`.
    fn numeric(&mut self, d: &[u8]) -> Option<Cell> {
        if d.len() < 8 {
            return None;
        }
        let ndigits = be16(d) as usize;
        let weight = be16(&d[2..]) as i64;
        let sign = u16::from_be_bytes([d[4], d[5]]);
        if d.len() != 8 + 2 * ndigits {
            return None;
        }
        match sign {
            0x0000 | 0x4000 => {}
            0xC000 => return Some(Cell::Static("NaN")),
            0xD000 => return Some(Cell::Static("Infinity")),
            0xF000 => return Some(Cell::Static("-Infinity")),
            _ => return None,
        }
        if ndigits == 0 {
            return Some(Cell::Num(0.0));
        }
        let t = &mut self.numeric;
        t.clear();
        if sign == 0x4000 {
            t.push('-');
        }
        for i in 0..ndigits {
            let g = be16(&d[8 + 2 * i..]);
            if !(0..10000).contains(&g) {
                return None;
            }
            use std::fmt::Write as _;
            let _ = write!(t, "{g:04}");
        }
        use std::fmt::Write as _;
        let _ = write!(t, "e{}", 4 * (weight - ndigits as i64 + 1));
        t.parse::<f64>().ok().map(Cell::Num)
    }

    /// The zoom rule: `Number()` of every candidate field first
    /// (`.map`, so a TypeError of any candidate throws), then the first finite
    /// one, else the default, then the clamp.
    fn minzoom(&self, z: &ZoomRule, out: &RowOut) -> Result<f64, ParseError> {
        let type_error = |what: &str| error(EXIT_DATA, &format!("pgcopy: TypeError: Cannot convert object to primitive value (zoom rule, {what}); a Node.js export stops here too"));
        let mut resolved = None;
        for f in &z.fields {
            let n = match f {
                None => f64::NAN,
                Some(c) => match &out.cells[*c] {
                    Cell::Absent => f64::NAN,
                    Cell::Null => 0.0,
                    Cell::Bool(b) => f64::from(u8::from(*b)),
                    Cell::Num(n) => *n,
                    Cell::Str(s, e) => js::string_to_number(&out.text[*s as usize..*e as usize]),
                    Cell::Static(s) => js::string_to_number(s),
                    Cell::Tree(v) => js::to_number(Some(v)).map_err(|_| type_error(&format!("field \"{}\"", self.schema.columns[*c].name)))?,
                },
            };
            if resolved.is_none() && n.is_finite() {
                resolved = Some(n);
            }
        }
        let resolved = match resolved {
            Some(v) => v,
            None => match z.default {
                Ok(d) if d.is_finite() => d,
                Ok(_) => z.min,
                Err(_) => return Err(type_error("defaultMinZoom")),
            },
        };
        // `Math.max(layerMinZoom, Math.min(layerMaxZoom, resolved))`.
        Ok(js_max(z.min, js_min(z.max, resolved)))
    }

    /// The direct path: the features and attributes of the row.
    fn direct(&mut self, geom: (u32, u32), out: &mut RowOut, zoom: (Option<f64>, Option<f64>)) -> Direct {
        let s = self.schema;
        // Values that only the reference path does exactly.
        for &c in &s.props {
            match &out.cells[c] {
                Cell::Tree(v) if needs_reference_value(v) => return Direct::Reference,
                Cell::Str(a, b) if s.columns[c].name == "type" && matches!(&out.text[*a as usize..*b as usize], "Feature" | "FeatureCollection") => return Direct::Reference,
                _ => {}
            }
        }
        let (minzoom, maxzoom) = zoom;
        let zoom_i8 = |v: Option<f64>| -> Option<i8> {
            match v {
                None => Some(-1),
                Some(v) if v.is_finite() && v.fract() == 0.0 && (0.0..=32.0).contains(&v) => Some(v as i8),
                Some(_) => None,
            }
        };
        let (Some(fmin), Some(fmax)) = (zoom_i8(minzoom), zoom_i8(maxzoom)) else { return Direct::Reference };

        // The geometry.
        let gtext_range = geom.0 as usize..geom.1 as usize;
        let layer = s.layer.clone();
        {
            let RowOut { features, count, text, .. } = out;
            if features.len() == *count {
                features.push(ParsedFeature { geom_type: GeomType::Point, geom: Vec::new(), attrs: Vec::new(), minzoom: -1, maxzoom: -1, layer: None, id: None, seq: 0 });
            }
            let f = &mut features[*count];
            f.geom.clear();
            if !crate::stream::parse_geometry(&text[gtext_range], f) {
                return Direct::Reference;
            }
            // `JSON.stringify` writes -0 as 0, and a value that is not finite
            // as null (an error of the reference path).
            for p in f.geom.iter_mut() {
                if p.op == VT_CLOSEPATH {
                    continue;
                }
                if !(p.lon.is_finite() && p.lat.is_finite()) {
                    return Direct::Reference;
                }
                p.lon += 0.0;
                p.lat += 0.0;
            }
            f.minzoom = fmin;
            f.maxzoom = fmax;
            if f.layer != layer {
                f.layer = layer;
            }
            f.id = None;
            *count += 1;
        }

        // The attributes.
        if s.simple {
            for (i, c) in s.consts.iter().enumerate() {
                let start = out.text.len() as u32;
                out.text.push_str(&c.3);
                out.attrs.push((Key::Const(i as u32), c.2, start, out.text.len() as u32));
            }
            for &c in &s.props {
                self.push_cell(Key::Col(c as u32), c, out);
            }
        } else {
            self.order.clear();
            let set = |order: &mut Vec<(String, Src)>, k: &str, v: Src| match order.iter_mut().find(|e| e.0 == k) {
                Some(e) => e.1 = v,
                None => order.push((k.to_string(), v)),
            };
            for (i, c) in s.consts.iter().enumerate() {
                set(&mut self.order, &c.0, Src::Const(i));
            }
            for &c in &s.props {
                if !matches!(out.cells[c], Cell::Absent) {
                    set(&mut self.order, &s.columns[c].name, Src::Col(c));
                }
            }
            if let Some(k) = &s.minzoom_key {
                set(&mut self.order, k, Src::MinZoom);
            }
            if let Some(k) = &s.maxzoom_key {
                set(&mut self.order, k, Src::MaxZoom);
            }
            js::js_order(&mut self.order);
            // A key of the order can come from another group than its value.
            let order = std::mem::take(&mut self.order);
            for (k, src) in &order {
                let start_attrs = out.attrs.len();
                match *src {
                    Src::Const(i) => {
                        let c = &s.consts[i];
                        let start = out.text.len() as u32;
                        out.text.push_str(&c.3);
                        out.attrs.push((Key::Const(0), c.2, start, out.text.len() as u32));
                    }
                    Src::Col(c) => self.push_cell(Key::Col(0), c, out),
                    Src::MinZoom => {
                        let start = out.text.len() as u32;
                        let ty = number_attr(minzoom.unwrap(), &mut out.text);
                        out.attrs.push((Key::MinZoom, ty, start, out.text.len() as u32));
                    }
                    Src::MaxZoom => {
                        let start = out.text.len() as u32;
                        let ty = number_attr(maxzoom.unwrap(), &mut out.text);
                        out.attrs.push((Key::MaxZoom, ty, start, out.text.len() as u32));
                    }
                }
                // The key text: an own key (the order key).
                if let Some(a) = out.attrs.get_mut(start_attrs) {
                    a.0 = Key::Own(out.own_keys.len() as u32);
                    out.own_keys.push(k.clone());
                }
            }
            self.order = order;
            out.attr_ends.push(out.attrs.len());
            return Direct::Done;
        }
        if let Some(m) = minzoom
            && s.minzoom_key.is_some()
        {
            let start = out.text.len() as u32;
            let ty = number_attr(m, &mut out.text);
            out.attrs.push((Key::MinZoom, ty, start, out.text.len() as u32));
        }
        if let Some(m) = maxzoom
            && s.maxzoom_key.is_some()
        {
            let start = out.text.len() as u32;
            let ty = number_attr(m, &mut out.text);
            out.attrs.push((Key::MaxZoom, ty, start, out.text.len() as u32));
        }
        out.attr_ends.push(out.attrs.len());
        Direct::Done
    }

    /// Add the attribute of property column `c` (nothing if it is absent).
    fn push_cell(&self, key: Key, c: usize, out: &mut RowOut) {
        let (ty, start, end) = match &out.cells[c] {
            Cell::Absent => return,
            Cell::Null => {
                let s = out.text.len() as u32;
                out.text.push_str("null");
                (AttrType::Null, s, out.text.len() as u32)
            }
            Cell::Bool(b) => {
                let s = out.text.len() as u32;
                out.text.push_str(if *b { "true" } else { "false" });
                (AttrType::Bool, s, out.text.len() as u32)
            }
            Cell::Num(n) => {
                let s = out.text.len() as u32;
                let ty = number_attr(*n, &mut out.text);
                (ty, s, out.text.len() as u32)
            }
            Cell::Str(a, b) => (AttrType::String, *a, *b),
            Cell::Static(t) => {
                let s = out.text.len() as u32;
                out.text.push_str(t);
                (AttrType::String, s, out.text.len() as u32)
            }
            Cell::Tree(v) => {
                let s = out.text.len() as u32;
                reader_text(v, &mut out.text);
                (AttrType::String, s, out.text.len() as u32)
            }
        };
        let key = match key {
            Key::Col(_) => Key::Col(c as u32),
            k => k,
        };
        out.attrs.push((key, ty, start, end));
    }

    /// The JavaScript value of a cell (for the NDJSON line).
    fn cell_value(c: &Cell, text: &str) -> Option<Value> {
        Some(match c {
            Cell::Absent => return None,
            Cell::Null => Value::Null,
            Cell::Bool(b) => Value::Bool(*b),
            Cell::Num(n) => Value::Num(*n),
            Cell::Str(a, b) => Value::Str(text[*a as usize..*b as usize].to_string()),
            Cell::Static(s) => Value::Str((*s).to_string()),
            Cell::Tree(v) => v.clone(),
        })
    }

    /// The NDJSON line that the Node.js exporter writes for the row.
    fn line(&self, geom: (u32, u32), out: &RowOut, zoom: (Option<f64>, Option<f64>)) -> Result<String, ParseError> {
        let s = self.schema;
        let gtext = &out.text[geom.0 as usize..geom.1 as usize];
        let g = js::parse_depth(gtext, self.max_depth).map_err(|e| json_error("the geometry", e))?;
        // `props`: the properties object after `JSON.parse`.
        let mut props: Vec<(String, Value)> = Vec::new();
        for &c in &s.props {
            if let Some(v) = Self::cell_value(&out.cells[c], &out.text) {
                props.push((s.columns[c].name.clone(), v));
            }
        }
        let (minzoom, maxzoom) = zoom;
        // `{layer, ...props, tippecanoe_minzoom, tippecanoe_maxzoom}`.
        let mut obj: Vec<(String, Value)> = Vec::new();
        let set = |obj: &mut Vec<(String, Value)>, k: &str, v: Value| match obj.iter_mut().find(|e| e.0 == k) {
            Some(e) => e.1 = v,
            None => obj.push((k.to_string(), v)),
        };
        for c in &s.consts {
            set(&mut obj, &c.0, c.1.clone());
        }
        for (k, v) in props {
            set(&mut obj, &k, v);
        }
        if let (Some(k), Some(m)) = (&s.minzoom_key, minzoom) {
            set(&mut obj, k, Value::Num(m));
        }
        if let (Some(k), Some(m)) = (&s.maxzoom_key, maxzoom) {
            set(&mut obj, k, Value::Num(m));
        }
        js::js_order(&mut obj);
        let mut feature = vec![("type".to_string(), Value::Str("Feature".into())), ("geometry".to_string(), g), ("properties".to_string(), Value::Obj(obj))];
        let mut tc = Vec::new();
        if let Some(l) = &s.layer {
            tc.push(("layer".to_string(), Value::Str(l.to_string())));
        }
        if let Some(m) = minzoom {
            tc.push(("minzoom".to_string(), Value::Num(m)));
        }
        if let Some(m) = maxzoom {
            tc.push(("maxzoom".to_string(), Value::Num(m)));
        }
        if !tc.is_empty() {
            feature.push(("tippecanoe".to_string(), Value::Obj(tc)));
        }
        let mut line = String::new();
        js::stringify(&Value::Obj(feature), &mut line);
        Ok(line)
    }

    /// The reference path: read the NDJSON line as vtmake's NDJSON reader
    /// does (the fast parser, else the jsonpull port).
    fn reference(&self, line: &str, out: &mut RowOut) {
        out.count = 0;
        out.attrs.clear();
        out.attr_ends.clear();
        out.own_keys.clear();
        let mut fast = crate::stream::StreamParser::default();
        if fast.parse(line.as_bytes()) {
            let f = out.feature();
            f.geom_type = fast.feature.geom_type;
            f.geom.clone_from(&fast.feature.geom);
            f.minzoom = fast.feature.minzoom;
            f.maxzoom = fast.feature.maxzoom;
            f.layer.clone_from(&fast.feature.layer);
            f.id = fast.feature.id;
            for a in &fast.attrs {
                let start = out.text.len() as u32;
                out.text.push_str(&a.value);
                out.attrs.push((Key::Own(out.own_keys.len() as u32), a.ty, start, out.text.len() as u32));
                out.own_keys.push(a.key.to_string());
            }
            out.attr_ends.push(out.attrs.len());
            return;
        }
        let mut p = crate::pull::Pull::new();
        p.set_input(line.as_bytes(), true);
        let mut feats = Vec::new();
        crate::geojson::Loop::default().run(&mut p, &self.settings, &self.fname, &mut feats);
        for (_, pf) in feats {
            let f = out.feature();
            f.geom_type = pf.geom_type;
            f.geom.clone_from(&pf.geom);
            f.minzoom = pf.minzoom;
            f.maxzoom = pf.maxzoom;
            f.layer.clone_from(&pf.layer);
            f.id = pf.id;
            for a in &pf.attrs {
                let start = out.text.len() as u32;
                out.text.push_str(&a.value);
                out.attrs.push((Key::Own(out.own_keys.len() as u32), a.ty, start, out.text.len() as u32));
                out.own_keys.push(a.key.to_string());
            }
            out.attr_ends.push(out.attrs.len());
        }
    }
}

type Summary = Vec<(GeomType, Vec<(u8, u64, u64)>, i8, i8, Option<String>, Option<u64>, Vec<(String, AttrType, String)>)>;

/// The features of a row in a form to compare (the check mode).
fn summary(schema: &Schema, out: &RowOut) -> Summary {
    (0..out.count)
        .map(|i| {
            let f = &out.features[i];
            (
                f.geom_type,
                f.geom.iter().map(|p| (p.op, p.lon.to_bits(), p.lat.to_bits())).collect(),
                f.minzoom,
                f.maxzoom,
                f.layer.as_deref().map(str::to_string),
                f.id,
                out.attrs(schema, i).map(|(k, t, v)| (k.to_string(), t, v.to_string())).collect(),
            )
        })
        .collect()
}

/// The geometry text is a falsy JSON value (`null`, `false`, `0`, `""`):
/// Node skips the row. Objects and arrays are not falsy. Text that is not
/// JSON is not falsy here (the reference path then stops, as `JSON.parse`).
fn is_falsy(text: &str) -> bool {
    let t = text.trim_start_matches([' ', '\t', '\n', '\r']);
    if t.starts_with(['{', '[']) {
        return false;
    }
    match js::parse(text) {
        Ok(Value::Null | Value::Bool(false)) => true,
        Ok(Value::Str(s)) => s.is_empty(),
        Ok(Value::Num(n)) => n == 0.0 || n.is_nan(),
        _ => false,
    }
}

/// A JSON error of `what`: too deep (the caller retries with a larger
/// limit) or not valid.
fn json_error(what: &str, e: js::ParseErr) -> ParseError {
    match e {
        js::ParseErr::TooDeep => error(TOO_DEEP, &format!("pgcopy: {what}: JSON nesting over the limit")),
        js::ParseErr::Invalid(m) => error(EXIT_DATA, &format!("pgcopy: {what}: not valid JSON: {m}")),
    }
}

fn jsonb_text(d: &[u8]) -> Result<&[u8], ParseError> {
    match d.split_first() {
        Some((1, t)) => Ok(t),
        _ => Err(error(EXIT_DATA, "pgcopy: unknown jsonb binary version")),
    }
}

/// `Math.min(a, b)` for numbers that are not NaN.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() { a } else { b }
    } else {
        a.min(b)
    }
}

/// `Math.max(a, b)`.
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_positive() { a } else { b }
    } else {
        a.max(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary COPY stream with the header and the trailer.
    pub(crate) fn stream(rows: &[Vec<Option<Vec<u8>>>]) -> Vec<u8> {
        let mut b = SIGNATURE.to_vec();
        b.extend_from_slice(&0i32.to_be_bytes());
        b.extend_from_slice(&0i32.to_be_bytes());
        for r in rows {
            b.extend_from_slice(&(r.len() as i16).to_be_bytes());
            for f in r {
                match f {
                    None => b.extend_from_slice(&(-1i32).to_be_bytes()),
                    Some(d) => {
                        b.extend_from_slice(&(d.len() as i32).to_be_bytes());
                        b.extend_from_slice(d);
                    }
                }
            }
        }
        b.extend_from_slice(&(-1i16).to_be_bytes());
        b
    }

    fn text(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }
    fn int4(v: i32) -> Option<Vec<u8>> {
        Some(v.to_be_bytes().to_vec())
    }
    fn int8(v: i64) -> Option<Vec<u8>> {
        Some(v.to_be_bytes().to_vec())
    }
    fn jsonb(s: &str) -> Option<Vec<u8>> {
        let mut v = vec![1u8];
        v.extend_from_slice(s.as_bytes());
        Some(v)
    }
    fn numeric(ndigits: &[i16], weight: i16, sign: u16, dscale: i16) -> Option<Vec<u8>> {
        let mut v = Vec::new();
        v.extend_from_slice(&(ndigits.len() as i16).to_be_bytes());
        v.extend_from_slice(&weight.to_be_bytes());
        v.extend_from_slice(&sign.to_be_bytes());
        v.extend_from_slice(&dscale.to_be_bytes());
        for d in ndigits {
            v.extend_from_slice(&d.to_be_bytes());
        }
        Some(v)
    }

    const EXPORT_SCHEMA: &str = r#"{"format":"vtmake-pgcopy/1","layer":"L","stripNulls":STRIP,
        "columns":[{"name":"geom","type":"text","role":"geometry"},{"name":"id","type":"uuid"},{"name":"name","type":"text"},
                   {"name":"zoom","type":"int4"},{"name":"big","type":"int8"},{"name":"num","type":"numeric"},{"name":"props","type":"jsonb"},{"name":"flag","type":"bool"}],
        "attributes":{"before":{"layer":"L"},"minzoom":"tippecanoe_minzoom","maxzoom":"tippecanoe_maxzoom"},
        "zoom":{"zoomField":"zoom","fallbackZoomFields":["num"],"defaultMinZoom":12,"minZoom":12,"maxZoom":18}}"#;

    fn decode(schema: &Schema, rows: &[Vec<Option<Vec<u8>>>], check: bool) -> Vec<Summary> {
        let data = stream(rows);
        let mut p = header_len(&data).unwrap().unwrap();
        let mut d = Decoder::new(schema, crate::geojson::Settings { projection_alias: "urn:ogc:def:crs:OGC:1.3:CRS84", quiet: true, ..Default::default() }, "t");
        d.check = check;
        let mut out = RowOut::default();
        let mut res = Vec::new();
        loop {
            match next(&data[p..], schema.columns()).unwrap() {
                Next::Row(n) => {
                    d.row(&data[p..p + n], &mut out).unwrap();
                    res.push(summary(schema, &out));
                    p += n;
                }
                Next::Trailer => break,
                Next::More => panic!("truncated"),
            }
        }
        res
    }

    fn rows() -> Vec<Vec<Option<Vec<u8>>>> {
        let uuid = Some((0u8..16).collect::<Vec<u8>>());
        vec![
            vec![text(r#"{"type":"Point","coordinates":[139.7,35.6]}"#), uuid.clone(), text("a\"b\\c\u{1}é"), int4(15), int8(9007199254740993), numeric(&[1, 5000], 0, 0, 2), jsonb(r#"{"a": null, "b": [1, null, {"x": null}], "10": "t"}"#), Some(vec![1])],
            vec![text(r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}"#), None, None, None, int8(-5), numeric(&[13], 0, 0, 0), None, Some(vec![0])],
            vec![text(r#"{"type":"LineString","coordinates":[[0,0],[1e-7,-0]]}"#), uuid.clone(), text(""), int4(30), int8(1 << 62), numeric(&[], 0, 0xC000, 0), jsonb(r#""s""#), None],
            vec![None, uuid.clone(), text("x"), int4(15), int8(0), numeric(&[], 0, 0, 0), jsonb("null"), None],
            // A fractional zoom: the reference path (a fatal error there).
            vec![text(r#"{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]},{"type":"Point","coordinates":[3,4]}]}"#), uuid.clone(), text("gc"), int4(13), int8(1), numeric(&[1], 0, 0x4000, 0), jsonb(r#"[{"type": "Feature"}]"#), None],
            vec![text(r#"{"type":"Point","coordinates":[1,2]}"#), uuid, text("Feature"), int4(14), int8(1), numeric(&[1, 2], -3, 0, 20), jsonb(r#"{"type": "FeatureCollection"}"#), None],
        ]
    }

    #[test]
    fn direct_and_reference_paths_agree() {
        for strip in ["true", "false"] {
            let schema = Schema::from_json(&EXPORT_SCHEMA.replace("STRIP", strip), 12, 18).unwrap();
            assert!(schema.simple);
            let got = decode(&schema, &rows(), true);
            assert_eq!(got.len(), 6);
            assert!(got[3].is_empty(), "a NULL geometry is skipped");
            assert_eq!(got[4].len(), 2, "a GeometryCollection gives two features");
            let a = &got[0][0].6;
            let keys: Vec<&str> = a.iter().map(|x| x.0.as_str()).collect();
            if strip == "true" {
                assert_eq!(keys, ["layer", "id", "big", "num", "flag", "name", "zoom", "props", "tippecanoe_minzoom", "tippecanoe_maxzoom"]);
                assert_eq!(a[7].2, r#"{"10":"t","b":[1,null,{}]}"#);
            } else {
                assert_eq!(a[7].2, r#"{"10":"t","a":null,"b":[1,null,{"x":null}]}"#);
            }
            assert_eq!(a[1].2, "00010203-0405-0607-0809-0a0b0c0d0e0f");
            assert_eq!(a[2], ("big".into(), AttrType::Number, "9007199254740992".into()));
            assert_eq!(a[3], ("num".into(), AttrType::Number, "1.5".into()));
            assert_eq!(a[8].2, "15");
            let b = &got[1][0].6;
            // NULL zoom: stripped (undefined, the fallback field), else Number(null) = 0.
            let want = if strip == "true" { "13" } else { "12" };
            assert_eq!(b.iter().find(|x| x.0 == "tippecanoe_minzoom").unwrap().2, want);
            let c = &got[2][0].6;
            assert!(c.contains(&("num".into(), AttrType::String, "NaN".into())));
            assert!(c.contains(&("props".into(), AttrType::String, "s".into())));
            assert!(c.contains(&("big".into(), AttrType::Number, "4611686018427388000".into())));
            assert_eq!(got[2][0].2, 18, "clamped to the maximum");
        }
    }

    #[test]
    fn colliding_keys_follow_object_spread() {
        let s = r#"{"format":"vtmake-pgcopy/1","layer":"L","columns":[{"name":"geom","type":"text","role":"geometry"},
            {"name":"tippecanoe_minzoom","type":"int4"},{"name":"layer","type":"text"},{"name":"7","type":"int4"},{"name":"z","type":"int4"}],
            "attributes":{"before":{"layer":"L","2":true},"minzoom":"tippecanoe_minzoom","maxzoom":"tippecanoe_maxzoom"},
            "zoom":{"zoomField":"z","minZoom":0,"maxZoom":20}}"#;
        let schema = Schema::from_json(s, 0, 20).unwrap();
        assert!(!schema.simple);
        let rows = vec![
            vec![text(r#"{"type":"Point","coordinates":[1,2]}"#), int4(99), text("mine"), int4(1), int4(5)],
            vec![text(r#"{"type":"Point","coordinates":[1,2]}"#), None, None, None, None],
        ];
        let got = decode(&schema, &rows, true);
        let keys: Vec<(&str, &str)> = got[0][0].6.iter().map(|x| (x.0.as_str(), x.2.as_str())).collect();
        assert_eq!(keys, [("2", "true"), ("7", "1"), ("layer", "mine"), ("z", "5"), ("tippecanoe_minzoom", "5"), ("tippecanoe_maxzoom", "20")]);
        let keys: Vec<(&str, &str)> = got[1][0].6.iter().map(|x| (x.0.as_str(), x.2.as_str())).collect();
        assert_eq!(keys, [("2", "true"), ("7", "null"), ("layer", "null"), ("z", "null"), ("tippecanoe_minzoom", "0"), ("tippecanoe_maxzoom", "20")]);
    }

    #[test]
    fn a_falsy_geometry_skips_the_row_before_other_checks() {
        let s = r#"{"format":"vtmake-pgcopy/1","layer":"L","columns":[{"name":"geom","type":"text","role":"geometry"},{"name":"z","type":"text"}],
            "attributes":{"minzoom":"m","maxzoom":"M"},"zoom":{"zoomField":"z","minZoom":0,"maxZoom":20}}"#;
        let schema = Schema::from_json(s, 0, 20).unwrap();
        for g in ["null", " false ", "0", "\"\"", "-0"] {
            // A fractional zoom would stop the run, but Node skips the row first.
            let got = decode(&schema, &[vec![text(g), text("12.5")]], true);
            assert!(got[0].is_empty(), "{g}");
        }
        let data = stream(&[vec![text("null"), text("1")], vec![text(r#"{"type":"Point","coordinates":[1,2]}"#), text("1")]]);
        let mut d = Decoder::new(&schema, crate::geojson::Settings { quiet: true, ..Default::default() }, "t");
        let mut out = RowOut::default();
        let mut p = header_len(&data).unwrap().unwrap();
        let mut seen = Vec::new();
        while let Next::Row(n) = next(&data[p..], 2).unwrap() {
            d.row(&data[p..p + n], &mut out).unwrap();
            seen.push((out.has_geometry, out.count));
            p += n;
        }
        assert_eq!(seen, [(false, 0), (true, 1)]);
    }

    /// Decode rows; the first error, if any.
    fn decode_result(schema: &Schema, rows: &[Vec<Option<Vec<u8>>>], check: bool) -> Result<Vec<Summary>, ParseError> {
        let data = stream(rows);
        let mut p = header_len(&data).unwrap().unwrap();
        let mut d = Decoder::new(schema, crate::geojson::Settings { projection_alias: "urn:ogc:def:crs:OGC:1.3:CRS84", quiet: true, ..Default::default() }, "t");
        d.check = check;
        let mut out = RowOut::default();
        let mut res = Vec::new();
        while let Next::Row(n) = next(&data[p..], schema.columns()).unwrap() {
            d.row(&data[p..p + n], &mut out)?;
            res.push(summary(schema, &out));
            p += n;
        }
        Ok(res)
    }

    const POINT: &str = r#"{"type":"Point","coordinates":[1,2]}"#;

    /// Postgres validates `json` only in its conversion to
    /// jsonb (`\u0000` fails there). The schema refuses json properties; the
    /// caller sends `to_jsonb(col)`.
    #[test]
    fn json_properties_are_refused() {
        let s = r#"{"format":"vtmake-pgcopy/1","columns":[{"name":"g","type":"text","role":"geometry"},{"name":"v","type":"json"}]}"#;
        let e = Schema::from_json(s, 0, 14).unwrap_err();
        assert_eq!(e.exit_code, EXIT_SCHEMA);
        let s = r#"{"format":"vtmake-pgcopy/1","columns":[{"name":"g","oid":114,"role":"geometry"},{"name":"v","oid":3802}]}"#;
        assert!(Schema::from_json(s, 0, 14).is_ok(), "a json geometry column stays possible");
    }

    fn zoom_schema(strip: bool, fields: &str) -> Schema {
        let s = format!(
            r#"{{"format":"vtmake-pgcopy/1","layer":"L","stripNulls":{strip},"columns":[{{"name":"geom","type":"text","role":"geometry"}},{{"name":"a","type":"jsonb"}},{{"name":"b","type":"jsonb"}}],
            "attributes":{{"minzoom":"m","maxzoom":"M"}},"zoom":{{{fields},"defaultMinZoom":3,"minZoom":0,"maxZoom":20}}}}"#
        );
        Schema::from_json(&s, 0, 20).unwrap()
    }

    /// `Number()` of an object with an own "toString"
    /// member throws in Node; every candidate is converted (`.map`) before
    /// the first finite one is taken.
    #[test]
    fn zoom_conversion_throws_where_node_throws() {
        let z = |strip, f: &str, a: &str, b: &str| decode_result(&zoom_schema(strip, f), &[vec![text(POINT), jsonb(a), jsonb(b)]], true);
        let first = r#""zoomField":"a""#;
        let both = r#""zoomField":"a","fallbackZoomFields":["b"]"#;
        assert_eq!(z(false, first, r#"{"toString": null}"#, "1").unwrap_err().exit_code, EXIT_DATA);
        assert_eq!(z(false, first, r#"[{"toString": 1}]"#, "1").unwrap_err().exit_code, EXIT_DATA);
        // The first candidate is finite, the second throws: Node throws.
        assert_eq!(z(false, both, "5", r#"{"toString": "x"}"#).unwrap_err().exit_code, EXIT_DATA);
        // jsonb_strip_nulls removes the member first: no error, the default zoom.
        let ok = z(true, first, r#"{"toString": null}"#, "1").unwrap();
        assert_eq!(ok[0][0].2, 3);
        // valueOf is skipped (not callable), toString of Object.prototype: NaN.
        assert_eq!(z(false, first, r#"{"valueOf": 9}"#, "1").unwrap()[0][0].2, 3);
        // Arrays: Number([5]) = 5, Number(["7"]) = 7, Number([]) = 0, Number([1,2]) = NaN.
        assert_eq!(z(false, first, "[5]", "1").unwrap()[0][0].2, 5);
        assert_eq!(z(false, first, r#"["7"]"#, "1").unwrap()[0][0].2, 7);
        assert_eq!(z(false, first, "[]", "1").unwrap()[0][0].2, 0);
        assert_eq!(z(false, first, "[1, 2]", "1").unwrap()[0][0].2, 3);
        assert_eq!(z(false, first, "[[[6]]]", "1").unwrap()[0][0].2, 6);
        // A falsy geometry skips the row before the zoom rule.
        let skip = decode_result(&zoom_schema(false, first), &[vec![text("null"), jsonb(r#"{"toString": 1}"#), jsonb("1")]], true).unwrap();
        assert!(skip[0].is_empty());
    }

    /// vtmake's NDJSON reader has no nesting limit; the
    /// COPY reader must accept deep values too (and agree in check mode).
    #[test]
    fn deep_json_values_are_accepted() {
        let schema = zoom_schema(false, r#""zoomField":null"#);
        // Up to the limit, on the production path (the deep-row thread), in
        // check mode; this must pass in debug and release builds.
        for n in [141usize, 20_000, js::MAX_DEPTH] {
            let v = format!("{}1{}", "[".repeat(n), "]".repeat(n));
            let got = decode_result(&schema, &[vec![text(POINT), jsonb(&v), jsonb("1")]], true).unwrap();
            let a = &got[0][0].6;
            assert_eq!(a.iter().find(|x| x.0 == "a").unwrap().2, v, "{n}");
        }
        // Over the limit: exit 107 from the scan, without recursion (also far
        // over it).
        for n in [js::MAX_DEPTH + 1, 5_000_000] {
            let v = format!("{}1{}", "[".repeat(n), "]".repeat(n));
            let e = decode_result(&schema, &[vec![text(POINT), jsonb(&v), jsonb("1")]], true).unwrap_err();
            assert_eq!(e.exit_code, EXIT_DATA, "{n}");
        }
    }

    /// Stack probe (not a normal test): decode one row with deep values of
    /// `VTMAKE_PROBE_DEPTH` levels on a thread with `VTMAKE_PROBE_STACK` bytes,
    /// in check mode (direct path, reference line, NDJSON reader), with the
    /// value also used as a zoom field (`Number()` of arrays) and a nested
    /// Feature member (the jsonpull reference path). It aborts if the stack is
    /// too small. Run: cargo test -p vtmake-parse --lib deep_stack_probe -- --ignored
    #[test]
    #[ignore]
    fn deep_stack_probe() {
        let n: usize = std::env::var("VTMAKE_PROBE_DEPTH").map_or(10_000, |v| v.parse().unwrap());
        let stack: usize = std::env::var("VTMAKE_PROBE_STACK").map_or(64 << 20, |v| v.parse().unwrap());
        let r = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                let schema = zoom_schema(true, r#""zoomField":"a""#);
                let arrays = format!("{}5{}", "[".repeat(n), "]".repeat(n));
                let objects = format!("{}{{\"type\": \"Feature\"}}{}", "{\"k\": ".repeat(n - 1), "}".repeat(n - 1));
                let mut d = Decoder::new(&schema, crate::geojson::Settings { quiet: true, ..Default::default() }, "t");
                d.check = true;
                d.max_depth = js::MAX_DEPTH;
                let data = stream(&[vec![text(POINT), jsonb(&arrays), jsonb("1")], vec![text(POINT), jsonb(&objects), jsonb("1")]]);
                let mut p = header_len(&data).unwrap().unwrap();
                let mut out = RowOut::default();
                while let Next::Row(len) = next(&data[p..], 3).unwrap() {
                    d.row_at(&data[p..p + len], &mut out).unwrap();
                    out.cells.clear();
                    p += len;
                }
            })
            .unwrap()
            .join();
        assert!(r.is_ok());
    }

    #[test]
    fn small_integers_have_the_same_text_on_both_reader_paths() {
        for i in (-100_000i64..100_000).chain([999_999_999_999_999, -999_999_999_999_999, 100_000_000_000_000, 123_456_789_012_345]) {
            let t = i.to_string();
            assert_eq!(crate::stream::attribute_number(&t), crate::json::number(&t), "{t}");
        }
    }

    #[test]
    fn stream_errors() {
        let schema = Schema::from_json(&EXPORT_SCHEMA.replace("STRIP", "true"), 12, 18).unwrap();
        let data = stream(&rows());
        assert_eq!(header_len(&data[..10]).unwrap(), None);
        assert!(header_len(b"PGCOPX").is_err());
        let h = header_len(&data).unwrap().unwrap();
        assert_eq!(h, 19);
        let n = match next(&data[h..], 8).unwrap() {
            Next::Row(n) => n,
            _ => panic!(),
        };
        assert_eq!(next(&data[h..h + n - 1], 8).unwrap(), Next::More);
        assert!(next(&data[h..], 7).is_err());
        assert_eq!(next(&data[data.len() - 2..], 8).unwrap(), Next::Trailer);
        let _ = schema;
    }

    #[test]
    fn schema_errors() {
        for bad in [
            r#"{"format":"vtmake-pgcopy/1","columns":[]}"#,
            r#"{"format":"vtmake-pgcopy/2","columns":[{"name":"g","type":"text","role":"geometry"}]}"#,
            r#"{"format":"vtmake-pgcopy/1","columns":[{"name":"g","type":"text","role":"geometry"},{"name":"t","type":"timestamptz"}]}"#,
            r#"{"format":"vtmake-pgcopy/1","columns":[{"name":"g","type":"text","role":"geometry"},{"name":"a","type":"text"},{"name":"a","type":"int4"}]}"#,
            r#"{"format":"vtmake-pgcopy/1","columns":[{"name":"g","type":"text","role":"geometry"}],"extra":1}"#,
            r#"{"format":"vtmake-pgcopy/1","columns":[{"name":"g","type":"text","role":"geometry"}],"attributes":{"minzoom":"m"}}"#,
        ] {
            assert!(Schema::from_json(bad, 0, 14).is_err(), "{bad}");
        }
        let ok = r#"{"format":"vtmake-pgcopy/1","columns":[{"name":"g","oid":25,"role":"geometry"},{"name":"a","oid":3802},{"name":"x","type":"int4","role":"ignore"}]}"#;
        assert!(Schema::from_json(ok, 0, 14).is_ok());
    }
}
