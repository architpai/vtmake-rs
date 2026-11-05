use crate::{ParseError, error};
use std::borrow::Cow;
#[derive(Debug)]
pub(crate) enum Value<'a> {
    Object(Vec<(Cow<'a, str>, Value<'a>)>),
    Array(Vec<Value<'a>>),
    String(Cow<'a, str>),
    Number(&'a str),
    Bool(bool),
    Null,
}
impl<'a> Value<'a> {
    pub fn get(&self, key: &str) -> Option<&Self> {
        if let Self::Object(v) = self {
            v.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        } else {
            None
        }
    }
    pub fn string(&self) -> Option<&str> {
        if let Self::String(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn number(&self) -> Option<f64> {
        if let Self::Number(v) = self {
            v.parse().ok()
        } else {
            None
        }
    }
}
pub(crate) fn parse(s: &str) -> Result<Value<'_>, ParseError> {
    let mut p = Parser { s, i: 0 };
    let v = p.value(0)?;
    p.space();
    if p.i != s.len() {
        return Err(error(107, "Trailing JSON input"));
    }
    Ok(v)
}
pub(crate) struct Parser<'a> {
    pub(crate) s: &'a str,
    pub(crate) i: usize,
}
impl<'a> Parser<'a> {
    /// Skip jsonpull's white space: space, tab, CR, LF and RS (0x1E).
    pub(crate) fn space(&mut self) {
        while self
            .s
            .as_bytes()
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n' | 0x1E))
        {
            self.i += 1;
        }
    }
    pub(crate) fn string(&mut self) -> Result<Cow<'a, str>, ParseError> {
        let start = self.i;
        self.i += 1;
        let rest = &self.s.as_bytes()[self.i..];
        if let Some(n) = memchr::memchr2(b'"', b'\\', rest)
            && rest[n] == b'"'
            && !rest[..n].iter().any(|&c| c < 32)
        {
            self.i += n + 1;
            return Ok(Cow::Borrowed(&self.s[start + 1..self.i - 1]));
        }
        let mut escaped = false;
        while let Some(&b) = self.s.as_bytes().get(self.i) {
            self.i += 1;
            if b == b'"' && !escaped {
                return serde_json::from_str::<String>(&self.s[start..self.i])
                    .map(Cow::Owned)
                    .map_err(|_| error(107, "Invalid JSON string"));
            }
            if b == b'\\' {
                escaped = !escaped;
            } else {
                escaped = false;
            }
        }
        Err(error(107, "Unterminated JSON string"))
    }
    pub(crate) fn value(&mut self, depth: usize) -> Result<Value<'a>, ParseError> {
        if depth > 128 {
            return Err(error(107, "JSON nesting limit"));
        }
        self.space();
        let b = *self
            .s
            .as_bytes()
            .get(self.i)
            .ok_or_else(|| error(107, "Missing JSON value"))?;
        match b {
            b'"' => self.string().map(Value::String),
            b'{' | b'[' => {
                self.i += 1;
                self.space();
                let end = if b == b'{' { b'}' } else { b']' };
                let mut object = Vec::new();
                let mut array = Vec::new();
                if self.s.as_bytes().get(self.i) == Some(&end) {
                    self.i += 1;
                } else {
                    loop {
                        self.space();
                        if b == b'{' {
                            if self.s.as_bytes().get(self.i) != Some(&b'"') {
                                return Err(error(107, "Expected object key"));
                            }
                            let key = self.string()?;
                            self.space();
                            if self.s.as_bytes().get(self.i) != Some(&b':') {
                                return Err(error(107, "Expected colon"));
                            }
                            self.i += 1;
                            object.push((key, self.value(depth + 1)?));
                        } else {
                            array.push(self.value(depth + 1)?);
                        }
                        self.space();
                        match self.s.as_bytes().get(self.i) {
                            Some(c) if *c == end => {
                                self.i += 1;
                                break;
                            }
                            Some(b',') => {
                                self.i += 1;
                            }
                            _ => return Err(error(107, "Expected JSON separator")),
                        }
                    }
                }
                Ok(if b == b'{' {
                    Value::Object(object)
                } else {
                    Value::Array(array)
                })
            }
            b't' | b'f' | b'n' => {
                let (word, v) = match b {
                    b't' => ("true", Value::Bool(true)),
                    b'f' => ("false", Value::Bool(false)),
                    _ => ("null", Value::Null),
                };
                if !self.s[self.i..].starts_with(word) {
                    return Err(error(107, "Invalid JSON literal"));
                }
                self.i += word.len();
                Ok(v)
            }
            b'-' | b'0'..=b'9' => {
                let start = self.i;
                if b == b'-' {
                    self.i += 1;
                }
                match self.s.as_bytes().get(self.i) {
                    Some(b'0') => self.i += 1,
                    Some(b'1'..=b'9') => {
                        while self
                            .s
                            .as_bytes()
                            .get(self.i)
                            .is_some_and(u8::is_ascii_digit)
                        {
                            self.i += 1;
                        }
                    }
                    _ => return Err(error(107, "Invalid JSON number")),
                }
                if self.s.as_bytes().get(self.i) == Some(&b'.') {
                    self.i += 1;
                    self.digits()?;
                }
                if self
                    .s
                    .as_bytes()
                    .get(self.i)
                    .is_some_and(|c| *c == b'e' || *c == b'E')
                {
                    self.i += 1;
                    if self
                        .s
                        .as_bytes()
                        .get(self.i)
                        .is_some_and(|c| *c == b'+' || *c == b'-')
                    {
                        self.i += 1;
                    }
                    self.digits()?;
                }
                Ok(Value::Number(&self.s[start..self.i]))
            }
            _ => Err(error(107, "Invalid JSON value")),
        }
    }
    fn digits(&mut self) -> Result<(), ParseError> {
        let start = self.i;
        while self
            .s
            .as_bytes()
            .get(self.i)
            .is_some_and(u8::is_ascii_digit)
        {
            self.i += 1;
        }
        if self.i == start {
            Err(error(107, "Missing decimal digits"))
        } else {
            Ok(())
        }
    }
}
pub(crate) fn number(raw: &str) -> String {
    let v = raw.parse::<f64>().unwrap_or(f64::NAN);
    if v.abs() > 9007199254740991. && !raw.contains(['.', 'e', 'E']) {
        if let Ok(n) = raw.parse::<u64>() {
            return n.to_string();
        }
        if let Ok(n) = raw.parse::<i64>() {
            return n.to_string();
        }
    }
    crate::dtoa::dtoa_milo(v)
}
fn quote(s: &str, out: &mut String) {
    out.push('"');
    const HEX: &[u8] = b"0123456789abcdef";
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 32 => {
                out.push_str("\\u00");
                out.push(HEX[c as usize / 16] as char);
                out.push(HEX[c as usize % 16] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
pub(crate) fn stringify(v: &Value<'_>, out: &mut String) {
    match v {
        Value::String(s) => quote(s, out),
        Value::Number(s) => out.push_str(&number(s)),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Null => out.push_str("null"),
        Value::Array(v) => {
            out.push('[');
            for (i, v) in v.iter().enumerate() {
                if i != 0 {
                    out.push(',');
                }
                stringify(v, out);
            }
            out.push(']');
        }
        Value::Object(v) => {
            out.push('{');
            for (i, (k, v)) in v.iter().enumerate() {
                if i != 0 {
                    out.push(',');
                }
                quote(k, out);
                out.push(':');
                stringify(v, out);
            }
            out.push('}');
        }
    }
}
