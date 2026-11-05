//! A small JSON tree reader with the rules of tippecanoe's `jsonpull` that the
//! tools need: objects keep all entries in their order (also duplicate keys),
//! and strings decode `\u` escapes as jsonpull does (a lone surrogate becomes
//! three bytes). It reads one value and ignores the text after it.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

#[derive(Debug)]
pub enum Json {
    Hash(Vec<(Json, Json)>),
    Array(Vec<Json>),
    String(Vec<u8>),
    /// The number text.
    Number(String),
    Bool,
    Null,
}

impl Json {
    /// `json_hash_get`: the first entry with this string key.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Hash(es) => es.iter().find(|(k, _)| matches!(k, Json::String(s) if s == key.as_bytes())).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&[u8]> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    /// The number value (`atof` of the text).
    pub fn number(&self) -> Option<f64> {
        match self {
            Json::Number(s) => Some(crate::common::atof(s)),
            _ => None,
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

type R<T> = Result<T, &'static str>;

impl P<'_> {
    fn space(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.b.get(self.i).copied();
        self.i += 1;
        c
    }

    fn lit(&mut self, rest: &[u8], v: Json) -> R<Json> {
        for &c in rest {
            if self.next() != Some(c) {
                return Err("Found misspelling");
            }
        }
        Ok(v)
    }

    fn value(&mut self, depth: usize) -> R<Json> {
        if depth > 1000 {
            return Err("Too deep");
        }
        self.space();
        match self.next() {
            None => Err("Reached EOF without all containers being closed"),
            Some(b'{') => {
                let mut es = Vec::new();
                self.space();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Hash(es));
                }
                loop {
                    let k = self.value(depth + 1)?;
                    self.space();
                    if self.next() != Some(b':') {
                        return Err("Expected a colon");
                    }
                    let v = self.value(depth + 1)?;
                    es.push((k, v));
                    self.space();
                    match self.next() {
                        Some(b',') => continue,
                        Some(b'}') => return Ok(Json::Hash(es)),
                        _ => return Err("Expected a comma or a closing brace"),
                    }
                }
            }
            Some(b'[') => {
                let mut es = Vec::new();
                self.space();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Array(es));
                }
                loop {
                    es.push(self.value(depth + 1)?);
                    self.space();
                    match self.next() {
                        Some(b',') => continue,
                        Some(b']') => return Ok(Json::Array(es)),
                        _ => return Err("Expected a comma or a closing bracket"),
                    }
                }
            }
            Some(b'n') => self.lit(b"ull", Json::Null),
            Some(b't') => self.lit(b"rue", Json::Bool),
            Some(b'f') => self.lit(b"alse", Json::Bool),
            Some(b'"') => self.string().map(Json::String),
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let start = self.i - 1;
                while self.i < self.b.len() && matches!(self.b[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                    self.i += 1;
                }
                Ok(Json::Number(String::from_utf8_lossy(&self.b[start..self.i]).into_owned()))
            }
            Some(_) => Err("Unrecognized character"),
        }
    }

    fn string(&mut self) -> R<Vec<u8>> {
        let mut val = Vec::new();
        let mut surrogate: i64 = -1;
        let push3 = |val: &mut Vec<u8>, s: i64| {
            val.push((0xE0 | (s >> 12)) as u8);
            val.push((0x80 | ((s >> 6) & 0x3F)) as u8);
            val.push((0x80 | (s & 0x3F)) as u8);
        };
        loop {
            let Some(c) = self.next() else {
                return Err("String without closing quote mark");
            };
            if c == b'"' {
                if surrogate >= 0 {
                    push3(&mut val, surrogate);
                }
                return Ok(val);
            } else if c == b'\\' {
                let c = self.next().unwrap_or(0);
                if c == b'u' {
                    let mut ch: i64 = 0;
                    for _ in 0..4 {
                        let h = self.next().unwrap_or(0);
                        let d = (h as char).to_digit(16).ok_or("Invalid \\u hex character")?;
                        ch = ch * 16 + d as i64;
                    }
                    if (0xd800..=0xdbff).contains(&ch) {
                        if surrogate >= 0 {
                            push3(&mut val, surrogate);
                        }
                        surrogate = ch;
                        continue;
                    } else if (0xdc00..=0xdfff).contains(&ch) && surrogate >= 0 {
                        ch = (((surrogate - 0xd800) << 10) | (ch - 0xdc00)) + 0x10000;
                        surrogate = -1;
                    }
                    if surrogate >= 0 {
                        push3(&mut val, surrogate);
                        surrogate = -1;
                    }
                    if ch <= 0x7F {
                        val.push(ch as u8);
                    } else if ch <= 0x7FF {
                        val.push((0xC0 | (ch >> 6)) as u8);
                        val.push((0x80 | (ch & 0x3F)) as u8);
                    } else if ch <= 0xFFFF {
                        push3(&mut val, ch);
                    } else {
                        val.push((0xF0 | (ch >> 18)) as u8);
                        val.push((0x80 | ((ch >> 12) & 0x3F)) as u8);
                        val.push((0x80 | ((ch >> 6) & 0x3F)) as u8);
                        val.push((0x80 | (ch & 0x3F)) as u8);
                    }
                } else {
                    if surrogate >= 0 {
                        push3(&mut val, surrogate);
                        surrogate = -1;
                    }
                    val.push(match c {
                        b'"' => b'"',
                        b'\\' => b'\\',
                        b'/' => b'/',
                        b'b' => 8,
                        b'f' => 12,
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        _ => return Err("Found backslash followed by unknown character"),
                    });
                }
            } else if c < b' ' {
                return Err("Found control character in string");
            } else {
                if surrogate >= 0 {
                    push3(&mut val, surrogate);
                    surrogate = -1;
                }
                val.push(c);
            }
        }
    }
}

/// Read the first JSON value of `b`.
pub fn parse(b: &[u8]) -> Result<Json, &'static str> {
    P { b, i: 0 }.value(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_and_duplicates() {
        let j = parse(br#"{"b": "x\u00e9\ud83d\ude00", "a": [1, -2.5e3, true, null], "b": "y"} trailing"#).unwrap();
        let Json::Hash(es) = &j else { panic!() };
        assert_eq!(es.len(), 3);
        assert_eq!(j.get("b").unwrap().str().unwrap(), "xé😀".as_bytes());
        assert!(parse(b"{\"a\": \"\x01\"}").is_err());
    }
}
