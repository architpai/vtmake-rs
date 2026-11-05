//! Port of tippecanoe's `json_writer` (`write_json.cpp`). The spaces, commas
//! and newlines come from the same state machine, so the text is the same.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

use std::io::Write;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tok {
    Hash,
    HashKey,
    HashValue,
    Array,
    ArrayElement,
    Top,
}

/// The JSON writer. The text goes into `buf`; with a sink, [`JsonWriter::flush`]
/// moves it there when it becomes large.
pub struct JsonWriter<'a> {
    state: Vec<Tok>,
    pub nospace: bool,
    pub wantnl: bool,
    pub buf: Vec<u8>,
    sink: Option<&'a mut dyn Write>,
}

impl<'a> JsonWriter<'a> {
    /// A writer into memory (`json_writer(std::string *)`).
    pub fn mem() -> JsonWriter<'static> {
        JsonWriter { state: Vec::new(), nospace: false, wantnl: false, buf: Vec::new(), sink: None }
    }

    /// A writer to a stream (`json_writer(FILE *)`).
    pub fn to(sink: &'a mut dyn Write) -> JsonWriter<'a> {
        JsonWriter { state: Vec::new(), nospace: false, wantnl: false, buf: Vec::with_capacity(1 << 16), sink: Some(sink) }
    }

    /// Move the text to the sink if it is large.
    #[inline]
    pub fn maybe_flush(&mut self) {
        if self.buf.len() >= 1 << 16 {
            self.flush();
        }
    }

    /// Move all text to the sink. A write error (for example a closed pipe)
    /// stops the program, as a C program that gets SIGPIPE.
    pub fn flush(&mut self) {
        if let Some(s) = self.sink.as_mut() {
            if s.write_all(&self.buf).and_then(|_| s.flush()).is_err() {
                std::process::exit(141);
            }
            self.buf.clear();
        }
    }

    /// The text of a memory writer.
    pub fn into_string(self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }

    #[inline]
    fn addc(&mut self, c: u8) {
        self.buf.push(c);
    }

    #[inline]
    fn adds(&mut self, s: &[u8]) {
        self.buf.extend_from_slice(s);
    }

    fn adjust(&mut self) {
        let Some(&last) = self.state.last() else {
            self.state.push(Tok::Top);
            return;
        };
        let n = self.state.len() - 1;
        match last {
            Tok::Top => {
                self.addc(b'\n');
            }
            Tok::Hash => {
                if !self.nospace {
                    self.addc(b' ');
                }
                self.nospace = false;
                self.state[n] = Tok::HashKey;
            }
            Tok::HashKey => {
                self.adds(b":");
                if !self.nospace {
                    self.addc(b' ');
                    self.nospace = false;
                }
                self.state[n] = Tok::HashValue;
            }
            Tok::HashValue | Tok::ArrayElement => {
                if self.wantnl {
                    self.adds(b",\n");
                    self.nospace = false;
                } else if self.nospace {
                    self.addc(b',');
                    self.nospace = false;
                } else {
                    self.adds(b", ");
                }
                self.wantnl = false;
                self.state[n] = if last == Tok::HashValue { Tok::HashKey } else { Tok::ArrayElement };
            }
            Tok::Array => {
                if !self.nospace {
                    self.addc(b' ');
                }
                self.nospace = false;
                self.state[n] = Tok::ArrayElement;
            }
        }
    }

    pub fn write_array(&mut self) {
        self.adjust();
        self.addc(b'[');
        self.state.push(Tok::Array);
    }

    pub fn end_array(&mut self) {
        let tok = self.state.pop();
        assert!(matches!(tok, Some(Tok::Array | Tok::ArrayElement)), "End JSON array with unexpected state");
        if !self.nospace {
            self.addc(b' ');
        }
        self.nospace = false;
        self.addc(b']');
    }

    pub fn write_hash(&mut self) {
        self.adjust();
        self.addc(b'{');
        self.state.push(Tok::Hash);
    }

    pub fn end_hash(&mut self) {
        match self.state.pop() {
            Some(Tok::Hash) => {
                if !self.nospace {
                    // "Preserve accidental extra space from before"
                    self.adds(b"  ");
                }
            }
            Some(Tok::HashValue) => {
                if !self.nospace {
                    self.addc(b' ');
                }
            }
            _ => panic!("End JSON hash with unexpected state"),
        }
        self.nospace = false;
        self.addc(b'}');
    }

    /// A quoted string: `\` and `"` get a backslash, bytes below 0x20 become `\u00xx`.
    pub fn write_string(&mut self, s: &[u8]) {
        self.adjust();
        self.addc(b'"');
        for &c in s {
            if c == b'\\' || c == b'"' {
                self.addc(b'\\');
                self.addc(c);
            } else if c < b' ' {
                self.adds(format!("\\u{c:04x}").as_bytes());
            } else {
                self.addc(c);
            }
        }
        self.addc(b'"');
    }

    pub fn write_str(&mut self, s: &str) {
        self.write_string(s.as_bytes());
    }

    /// Text that is already JSON (`json_write_json`, `json_write_stringified`).
    pub fn write_json(&mut self, s: &str) {
        self.adjust();
        self.adds(s.as_bytes());
    }

    /// A number with `milo::dtoa_milo` (`json_write_number`).
    pub fn write_number(&mut self, d: f64) {
        self.adjust();
        let s = vtmake_parse::dtoa::dtoa_milo(d);
        self.adds(s.as_bytes());
    }

    /// A number with `%f` (`json_write_float`).
    pub fn write_float(&mut self, d: f64) {
        self.adjust();
        self.adds(crate::common::printf_f(d).as_bytes());
    }

    pub fn write_unsigned(&mut self, v: u64) {
        self.adjust();
        self.adds(v.to_string().as_bytes());
    }

    pub fn write_signed(&mut self, v: i64) {
        self.adjust();
        self.adds(v.to_string().as_bytes());
    }

    pub fn write_bool(&mut self, b: bool) {
        self.adjust();
        self.adds(if b { b"true" } else { b"false" });
    }

    pub fn write_null(&mut self) {
        self.adjust();
        self.adds(b"null");
    }

    pub fn write_newline(&mut self) {
        self.addc(b'\n');
        self.nospace = true;
    }

    pub fn comma_newline(&mut self) {
        self.wantnl = true;
    }

    /// `~json_writer`: the JSON must be closed at the end.
    pub fn check_closed(&self) {
        if !self.state.is_empty() && (self.state.len() != 1 || self.state[0] != Tok::Top) {
            eprintln!("JSON not closed at end");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_and_spaced() {
        let mut w = JsonWriter::mem();
        w.nospace = true;
        w.write_hash();
        w.nospace = true;
        w.write_str("a");
        w.nospace = true;
        w.write_signed(1);
        w.nospace = true;
        w.write_str("b");
        w.nospace = true;
        w.write_array();
        w.nospace = true;
        w.end_array();
        w.nospace = true;
        w.end_hash();
        assert_eq!(w.into_string(), "{\"a\":1,\"b\":[]}");

        let mut w = JsonWriter::mem();
        w.write_hash();
        w.write_str("type");
        w.write_str("Point");
        w.write_str("c");
        w.write_array();
        w.write_float(1.5);
        w.write_float(-2.0);
        w.end_array();
        w.end_hash();
        assert_eq!(w.into_string(), "{ \"type\": \"Point\", \"c\": [ 1.500000, -2.000000 ] }");
    }
}
