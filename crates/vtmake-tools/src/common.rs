//! Shared parts of the tools: C number parsing and formatting, exit codes,
//! the tile directory reader (`dirtiles.cpp`) and metadata access.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use rusqlite::Connection;
use rusqlite::types::{ToSqlOutput, ValueRef};
use std::path::Path;

// tippecanoe exit codes (`errors.hpp`).
pub const EXIT_ARGS: i32 = 101;
pub const EXIT_CLOSE: i32 = 102;
pub const EXIT_EXISTS: i32 = 104;
pub const EXIT_IMPOSSIBLE: i32 = 106;
pub const EXIT_JSON: i32 = 107;
pub const EXIT_MEMORY: i32 = 108;
pub const EXIT_MVT: i32 = 109;
pub const EXIT_OPEN: i32 = 111;
pub const EXIT_PROTOBUF: i32 = 112;
pub const EXIT_SQLITE: i32 = 116;
pub const EXIT_UNLINK: i32 = 118;
pub const EXIT_WRITE: i32 = 120;

/// A fatal error: the message for the standard error and the exit code.
#[derive(Debug)]
pub struct Fatal {
    pub msg: String,
    pub code: i32,
}

pub fn fatal<T>(msg: impl Into<String>, code: i32) -> Result<T, Fatal> {
    Err(Fatal { msg: msg.into(), code })
}

impl Fatal {
    /// Print the message and stop.
    pub fn exit(self) -> ! {
        if !self.msg.is_empty() {
            eprintln!("{}", self.msg);
        }
        std::process::exit(self.code)
    }
}

/// An option that this port does not have: a clear message and exit 101.
pub fn not_supported(tool: &str, what: &str) -> ! {
    eprintln!("vtmake {tool}: {what}: not supported yet");
    std::process::exit(EXIT_ARGS)
}

/// The text of an I/O error without the "(os error N)" part, as `strerror`.
pub fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    s.split(" (os error").next().unwrap_or(&s).to_string()
}

/// C `atoi` (glibc: `(int) strtol(s, NULL, 10)`).
pub fn atoi(s: &str) -> i32 {
    let b = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']).as_bytes();
    let (neg, b) = match b.first() {
        Some(b'-') => (true, &b[1..]),
        Some(b'+') => (false, &b[1..]),
        _ => (false, b),
    };
    let mut v: i64 = 0;
    for &c in b.iter().take_while(|c| c.is_ascii_digit()) {
        v = (v * 10 + (c - b'0') as i64).min(i64::MAX / 100);
    }
    let v = if neg { -v } else { v };
    v as i32
}

/// The longest start of `s` that is a decimal number, and its value (`strtod`).
pub fn strtod_prefix(s: &str) -> (f64, usize) {
    let lead = s.len() - s.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']).len();
    let b = &s.as_bytes()[lead..];
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - ds;
    if i < b.len() && b[i] == b'.' {
        let f = i + 1;
        let mut j = f;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        digits += j - f;
        if digits > 0 {
            i = j;
        }
    }
    if digits == 0 {
        return (0.0, 0);
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let es = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > es {
            i = j;
        }
    }
    let t = std::str::from_utf8(&b[..i]).unwrap_or("0");
    (t.parse().unwrap_or(0.0), lead + i)
}

/// C `atof`.
pub fn atof(s: &str) -> f64 {
    strtod_prefix(s).0
}

/// C `printf("%f")`.
pub fn printf_f(d: f64) -> String {
    if d.is_nan() {
        if d.is_sign_negative() { "-nan".into() } else { "nan".into() }
    } else if d.is_infinite() {
        if d < 0.0 { "-inf".into() } else { "inf".into() }
    } else {
        format!("{d:.6}")
    }
}

/// The bytes before the first NUL (a C string).
pub fn cstr(b: &[u8]) -> &[u8] {
    b.iter().position(|&c| c == 0).map_or(b, |n| &b[..n])
}

/// `format_commandline` (`text.cpp`): argv joined by spaces. An argument with
/// other characters than `[A-Za-z0-9/_@:.%,-]` is put in single quotes; a quote
/// in it becomes `'"'"'`.
pub fn format_commandline(argv: &[String]) -> String {
    // argv[0] can be "<program> join" (the vtmake subcommand): two words.
    let first: Vec<String> = argv.first().map(|a| a.split(' ').map(String::from).collect()).unwrap_or_default();
    let words: Vec<String> = first
        .iter()
        .chain(argv.iter().skip(1))
        .map(|a| {
            if a.bytes().all(|c| c.is_ascii_alphanumeric() || b"/-_@:.%,".contains(&c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\"'\"'"))
            }
        })
        .collect();
    words.join(" ")
}

/// `truncate16(s, 256)` (`text.cpp`): the longest start of `s` with at most
/// 256 UTF-16 units. A NUL ends the string.
pub fn truncate16(s: &str) -> &str {
    let mut len = 0;
    for (i, ch) in s.char_indices() {
        len += ch.len_utf16();
        if ch == '\0' || len > 256 {
            return &s[..i];
        }
    }
    s
}

/// A tile of a tile directory (`zxy`).
#[derive(Clone, Debug)]
pub struct Zxy {
    pub z: i64,
    pub x: i64,
    pub y: i64,
    pub mvt: bool,
}

impl Zxy {
    pub fn path(&self) -> String {
        format!("{}/{}/{}{}", self.z, self.x, self.y, if self.mvt { ".mvt" } else { ".pbf" })
    }
}

fn numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

fn pbfname(s: &str) -> bool {
    let t = s.trim_start_matches(|c: char| c.is_ascii_digit());
    t == ".pbf" || t == ".mvt"
}

fn read_names(dir: &str) -> Result<Vec<String>, std::io::Error> {
    Ok(std::fs::read_dir(dir)?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
}

/// `enumerate_dirtiles`: the tiles of a directory in z, x, TMS y order.
pub fn enumerate_dirtiles(fname: &str, minzoom: i32, maxzoom: i32) -> Result<Vec<Zxy>, Fatal> {
    let mut tiles = Vec::new();
    let Ok(zs) = read_names(fname) else {
        return Ok(tiles);
    };
    for zn in zs {
        if !numeric(&zn) || atoi(&zn) < minzoom || atoi(&zn) > maxzoom {
            continue;
        }
        let zdir = format!("{fname}/{zn}");
        let xs = read_names(&zdir).or_else(|e| fatal(format!("{zdir}: {}", strerror(&e)), EXIT_OPEN))?;
        for xn in xs {
            if !numeric(&xn) {
                continue;
            }
            let xdir = format!("{zdir}/{xn}");
            let ys = read_names(&xdir).or_else(|e| fatal(format!("{xdir}: {}", strerror(&e)), EXIT_OPEN))?;
            for yn in ys {
                if pbfname(&yn) {
                    tiles.push(Zxy { z: atoi(&zn) as i64, x: atoi(&xn) as i64, y: atoi(&yn) as i64, mvt: yn.contains(".mvt") });
                }
            }
        }
    }
    // `zxy::operator<`: z, then x, then y backwards (TMS order). A stable sort.
    tiles.sort_by(|a, b| a.z.cmp(&b.z).then(a.x.cmp(&b.x)).then(b.y.cmp(&a.y)));
    Ok(tiles)
}

/// `dirmeta2tmp`: the entries of `<dir>/metadata.json` in a temporary
/// database table `metadata (name, value)`.
pub fn dirmeta2tmp(fname: &str) -> Result<Connection, Fatal> {
    let db = Connection::open_in_memory().or_else(|e| fatal(format!("Temporary db: {e}"), EXIT_SQLITE))?;
    db.execute_batch("CREATE TABLE metadata (name text, value text);").or_else(|e| fatal(format!("Create metadata table: {e}"), EXIT_SQLITE))?;
    let name = format!("{fname}/metadata.json");
    match std::fs::read(&name) {
        Err(e) => eprintln!("{name}: {}", strerror(&e)),
        Ok(text) => {
            let o = crate::json::parse(&text).or_else(|e| fatal(format!("{name}: metadata parsing error: {e}"), EXIT_JSON))?;
            let crate::json::Json::Hash(entries) = o else {
                return fatal(format!("{name}: bad metadata format"), EXIT_JSON);
            };
            for (k, v) in entries {
                let (Some(k), Some(v)) = (k.str(), v.str()) else {
                    eprintln!("{name}: non-string in metadata");
                    continue;
                };
                let (k, v) = (cstr(k), cstr(v));
                if let Err(e) = db.execute("INSERT INTO metadata (name, value) VALUES (?, ?)", [ToSqlOutput::Borrowed(ValueRef::Text(k)), ToSqlOutput::Borrowed(ValueRef::Text(v))]) {
                    eprintln!("set {} in metadata: {e}", String::from_utf8_lossy(k));
                }
            }
        }
    }
    Ok(db)
}

/// `sqlite3_column_text`: the value as text, or None for NULL.
pub fn col_text(v: ValueRef) -> Option<Vec<u8>> {
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string().into_bytes()),
        ValueRef::Real(f) => Some(format!("{f}").into_bytes()),
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(cstr(b).to_vec()),
    }
}

/// `sqlite3_column_int`.
pub fn col_int(v: ValueRef) -> i32 {
    match v {
        ValueRef::Null => 0,
        ValueRef::Integer(i) => i as i32,
        ValueRef::Real(f) => f as i64 as i32,
        ValueRef::Text(b) | ValueRef::Blob(b) => atoi(&String::from_utf8_lossy(b)),
    }
}

/// The first row of `SELECT value from metadata where name = '<name>'`, as
/// text. None if there is no row or the query fails.
pub fn meta_value(db: &Connection, name: &str) -> Option<Option<Vec<u8>>> {
    let mut st = db.prepare(&format!("SELECT value from metadata where name = '{name}'")).ok()?;
    let mut rows = st.query([]).ok()?;
    let row = rows.next().ok()??;
    Some(row.get_ref(0).ok().and_then(col_text))
}

/// The same as [`meta_value`], with `sqlite3_column_int`.
pub fn meta_int(db: &Connection, name: &str) -> Option<i32> {
    let mut st = db.prepare(&format!("SELECT value from metadata where name = '{name}'")).ok()?;
    let mut rows = st.query([]).ok()?;
    let row = rows.next().ok()??;
    Some(row.get_ref(0).map(col_int).unwrap_or(0))
}

/// The sqlite error text (`sqlite3_errmsg`).
pub fn sql_msg(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(f, Some(m)) => {
            let _ = f;
            m.clone()
        }
        other => other.to_string(),
    }
}

/// True if `p` names a directory (`stat` and `S_IFDIR`).
pub fn is_dir(p: &str) -> bool {
    Path::new(p).is_dir()
}

/// `pmtiles_has_suffix`.
pub fn pmtiles_has_suffix(p: &str) -> bool {
    p.ends_with(".pmtiles")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_numbers() {
        assert_eq!(atoi(" 12abc"), 12);
        assert_eq!(atoi("-3"), -3);
        assert_eq!(atoi("x"), 0);
        assert_eq!(strtod_prefix("139.5,35.25,9"), (139.5, 5));
        assert_eq!(atof("-1e3x"), -1000.0);
        assert_eq!(atof("."), 0.0);
        assert_eq!(printf_f(0.0078125), "0.007812");
        assert_eq!(printf_f(-0.0), "-0.000000");
    }

    #[test]
    fn commandline_quotes() {
        let a: Vec<String> = ["tile-join", "-f", "-o", "a b.mbtiles", "it's", ""].iter().map(|s| s.to_string()).collect();
        assert_eq!(format_commandline(&a), "tile-join -f -o 'a b.mbtiles' 'it'\"'\"'s' ");
    }
}
