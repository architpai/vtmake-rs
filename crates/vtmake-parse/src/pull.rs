//! A port of tippecanoe's streaming JSON parser (`jsonpull/jsonpull.cpp`).
//!
//! `json_read` gives each value when it is complete, also the values inside
//! containers. The tree keeps the parent links, so the GeoJSON loop
//! (`crate::geojson`) can look at the ancestors of a value, and `free`
//! removes a value from its parent as `json_free` does. The nodes are in an
//! arena with a free list, so a large FeatureCollection does not keep the
//! features that were read before.
//!
//! The input comes in regions (`set_input`). A region can end at any byte. At
//! the end of a region that is not the last one, `read` gives [`Read::More`]
//! and keeps the parser state; a token (a number, a string, a word or the
//! byte-order mark) that can go on in the next region is read again with the
//! next region.
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.

/// No node.
pub const NIL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hash,
    Array,
    Number,
    String,
    True,
    False,
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    Item,
    Comma,
    Key,
    Colon,
    Value,
}

/// The number representation (`json_number::repr`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Repr {
    Double(f64),
    LargeUnsigned(u64),
    LargeSigned(i64),
}

#[derive(Debug)]
pub struct Node {
    pub kind: Kind,
    pub parent: u32,
    /// The bytes of a string.
    pub text: Vec<u8>,
    pub num: Repr,
    /// Array: the items. Hash: key, value, key, value, ... (a value is `NIL`
    /// until the parser reads it).
    pub items: Vec<u32>,
}

/// The result of [`Pull::read`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// A complete value.
    Node(u32),
    /// The region is at its end and it is not the last one.
    More,
    /// The end of the input, or an error (then `error` is set).
    End,
}

#[derive(Default)]
pub struct Pull {
    pub nodes: Vec<Node>,
    free_list: Vec<u32>,
    stack: Vec<(u32, Expect)>,
    /// The most recent top-level value.
    pub root: u32,
    pub error: Option<&'static str>,
    pub line: i32,
    buf: Vec<u8>,
    pos: usize,
    /// The bytes at the start of `buf` that come from the region before
    /// (the start of a token).
    carry: usize,
    last: bool,
    number_buffer: Vec<u8>,
    string_buffer: Vec<u8>,
}

/// C `atof` (glibc `strtod` in the "C" locale).
pub fn atof(s: &[u8]) -> f64 {
    let mut c = Vec::with_capacity(s.len() + 1);
    c.extend_from_slice(s);
    c.push(0);
    unsafe extern "C" {
        fn strtod(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char) -> f64;
    }
    // SAFETY: `c` is a NUL-terminated buffer that lives during the call.
    unsafe { strtod(c.as_ptr().cast(), std::ptr::null_mut()) }
}

/// C `strtoull`/`strtoll` with `errno` and the end check: `None` on overflow or
/// on text after the number.
fn parse_full<T: std::str::FromStr>(s: &[u8]) -> Option<T> {
    let t = std::str::from_utf8(s).ok()?;
    let t = t.strip_prefix('+').unwrap_or(t);
    t.parse().ok()
}

const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;

impl Pull {
    pub fn new() -> Self {
        Pull { root: NIL, line: 1, ..Default::default() }
    }

    /// Give the next region of the input. `last`: no more input after it.
    pub fn set_input(&mut self, data: &[u8], last: bool) {
        self.buf.drain(..self.pos);
        self.carry = self.buf.len();
        self.buf.extend_from_slice(data);
        self.pos = 0;
        self.last = last;
    }

    /// The byte position in the current region (after the end of the value
    /// that `read` gave).
    pub fn position(&self) -> usize {
        self.pos.saturating_sub(self.carry)
    }

    /// The token that starts at `start` (with `line` there) can go on in
    /// the next region: read it again with the next region.
    #[inline]
    fn again(&mut self, start: usize, line: i32) -> Option<Read> {
        if self.last || self.pos < self.buf.len() {
            return None;
        }
        self.pos = start;
        self.line = line;
        Some(Read::More)
    }

    /// The parser has no open container.
    pub fn at_top(&self) -> bool {
        self.stack.is_empty()
    }

    /// The innermost open container is an array that expects an item, and no
    /// open container is the value of a `"properties"` key. A Feature here is
    /// read as a feature.
    pub fn in_feature_list(&self) -> bool {
        match self.stack.last() {
            Some(&(a, Expect::Item)) if self.nodes[a as usize].kind == Kind::Array => !self.under_properties(a),
            _ => false,
        }
    }

    /// After a feature in a feature list: as when its closing brace is read
    /// and the feature is freed. With `comma`, a comma follows.
    pub fn skip_item(&mut self, comma: bool) {
        if let Some(f) = self.stack.last_mut() {
            f.1 = if comma { Expect::Item } else { Expect::Comma };
        }
    }

    /// Some ancestor of `o` (or `o`) is the value of a `"properties"` key.
    pub fn under_properties(&self, mut o: u32) -> bool {
        while o != NIL {
            let p = self.nodes[o as usize].parent;
            if p != NIL && self.nodes[p as usize].kind == Kind::Hash && self.hash_get(p, "properties") == Some(o) {
                return true;
            }
            o = p;
        }
        false
    }

    #[inline]
    pub fn kind(&self, o: u32) -> Kind {
        self.nodes[o as usize].kind
    }

    #[inline]
    pub fn parent(&self, o: u32) -> u32 {
        self.nodes[o as usize].parent
    }

    pub fn text(&self, o: u32) -> &[u8] {
        &self.nodes[o as usize].text
    }

    pub fn items(&self, o: u32) -> &[u32] {
        &self.nodes[o as usize].items
    }

    /// `json_object::number()`.
    pub fn number(&self, o: u32) -> f64 {
        match self.nodes[o as usize].num {
            Repr::Double(d) => d,
            Repr::LargeUnsigned(u) => u as f64,
            Repr::LargeSigned(s) => s as f64,
        }
    }

    pub fn large_unsigned(&self, o: u32) -> u64 {
        match self.nodes[o as usize].num {
            Repr::LargeUnsigned(u) => u,
            _ => 0,
        }
    }

    pub fn large_signed(&self, o: u32) -> i64 {
        match self.nodes[o as usize].num {
            Repr::LargeSigned(s) => s,
            _ => 0,
        }
    }

    /// `json_hash_get`: the value of the first entry with this string key.
    pub fn hash_get(&self, o: u32, key: &str) -> Option<u32> {
        if o == NIL || self.nodes[o as usize].kind != Kind::Hash {
            return None;
        }
        let items = &self.nodes[o as usize].items;
        for e in items.chunks(2) {
            let k = &self.nodes[e[0] as usize];
            if k.kind == Kind::String && k.text == key.as_bytes() {
                return (e[1] != NIL).then_some(e[1]);
            }
        }
        None
    }

    fn alloc(&mut self, kind: Kind, parent: u32) -> u32 {
        if let Some(i) = self.free_list.pop() {
            let n = &mut self.nodes[i as usize];
            n.kind = kind;
            n.parent = parent;
            n.text.clear();
            n.items.clear();
            n.num = Repr::Double(0.0);
            i
        } else {
            self.nodes.push(Node { kind, parent, text: Vec::new(), num: Repr::Double(0.0), items: Vec::new() });
            (self.nodes.len() - 1) as u32
        }
    }

    /// Put `o` and its subtree on the free list.
    fn destroy(&mut self, o: u32) {
        if o == NIL {
            return;
        }
        let mut todo = vec![o];
        while let Some(n) = todo.pop() {
            todo.extend(self.nodes[n as usize].items.iter().copied().filter(|&c| c != NIL));
            self.nodes[n as usize].items.clear();
            self.free_list.push(n);
        }
    }

    /// `json_free`: remove `o` from its owner and destroy it.
    pub fn free(&mut self, o: u32) {
        let parent = self.nodes[o as usize].parent;
        if parent == NIL {
            if self.root == o {
                self.root = NIL;
                self.destroy(o);
            }
            return;
        }
        match self.nodes[parent as usize].kind {
            Kind::Array => {
                if let Some(i) = self.nodes[parent as usize].items.iter().position(|&c| c == o) {
                    self.nodes[parent as usize].items.remove(i);
                    self.destroy(o);
                }
            }
            Kind::Hash => {
                let n = self.nodes[parent as usize].items.len() / 2;
                for i in 0..n {
                    let (k, v) = (self.nodes[parent as usize].items[2 * i], self.nodes[parent as usize].items[2 * i + 1]);
                    if k == o || v == o {
                        self.destroy(o);
                        let null = self.alloc(Kind::Null, parent);
                        let slot = if k == o { 2 * i } else { 2 * i + 1 };
                        self.nodes[parent as usize].items[slot] = null;
                        let other = self.nodes[parent as usize].items[if k == o { 2 * i + 1 } else { 2 * i }];
                        if other != NIL && self.nodes[other as usize].kind == Kind::Null {
                            self.nodes[parent as usize].items.drain(2 * i..2 * i + 2);
                            self.destroy(null);
                            self.destroy(other);
                        }
                        return;
                    }
                }
            }
            _ => {}
        }
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// `read_wrap`.
    #[inline]
    fn next(&mut self) -> Option<u8> {
        let c = self.buf.get(self.pos).copied();
        if let Some(c) = c {
            self.pos += 1;
            if c == b'\n' {
                self.line += 1;
            }
        }
        c
    }

    fn fail(&mut self, msg: &'static str) -> Read {
        self.error = Some(msg);
        Read::End
    }

    /// `add_object`: put a new node in the current container, or make it the root.
    fn add_object(&mut self, kind: Kind) -> Option<u32> {
        let Some(&(c, expect)) = self.stack.last() else {
            let o = self.alloc(kind, NIL);
            let old = std::mem::replace(&mut self.root, o);
            self.destroy(old);
            return Some(o);
        };
        let ck = self.nodes[c as usize].kind;
        let top = self.stack.len() - 1;
        if ck == Kind::Array {
            if expect == Expect::Item {
                let o = self.alloc(kind, c);
                self.nodes[c as usize].items.push(o);
                self.stack[top].1 = Expect::Comma;
                return Some(o);
            }
            self.error = Some("Expected a comma, not a list item");
            return None;
        }
        match expect {
            Expect::Value => {
                let o = self.alloc(kind, c);
                *self.nodes[c as usize].items.last_mut().expect("a key before a value") = o;
                self.stack[top].1 = Expect::Comma;
                Some(o)
            }
            Expect::Key => {
                if kind != Kind::String {
                    self.error = Some("Hash key is not a string");
                    return None;
                }
                let o = self.alloc(kind, c);
                self.nodes[c as usize].items.extend([o, NIL]);
                self.stack[top].1 = Expect::Colon;
                Some(o)
            }
            _ => {
                self.error = Some("Expected a comma or colon");
                None
            }
        }
    }

    fn word(&mut self, rest: &[u8]) -> bool {
        rest.iter().all(|&w| self.next() == Some(w))
    }

    /// `json_read`: the next complete value.
    pub fn read(&mut self) -> Read {
        if self.stack.is_empty() {
            let old = std::mem::replace(&mut self.root, NIL);
            self.destroy(old);
        }
        loop {
            let (start, line) = (self.pos, self.line);
            let Some(c) = self.next() else {
                if !self.last {
                    return Read::More;
                }
                if !self.stack.is_empty() {
                    self.error = Some("Reached EOF without all containers being closed");
                }
                return Read::End;
            };
            match c {
                0xEF => {
                    if self.peek() == Some(0xBB) {
                        self.next();
                        if self.peek() == Some(0xBF) {
                            self.next();
                            continue;
                        }
                    }
                    if let Some(r) = self.again(start, line) {
                        return r;
                    }
                    return self.fail("Corrupt byte-order mark found");
                }
                b' ' | b'\t' | b'\r' | b'\n' | 0x1E => continue,
                b'[' | b'{' => {
                    let (kind, expect) = if c == b'[' { (Kind::Array, Expect::Item) } else { (Kind::Hash, Expect::Key) };
                    let Some(o) = self.add_object(kind) else { return Read::End };
                    self.stack.push((o, expect));
                }
                b']' | b'}' => {
                    let Some(&(cc, expect)) = self.stack.last() else {
                        return self.fail(if c == b']' { "Found ] at top level" } else { "Found } at top level" });
                    };
                    let n = &self.nodes[cc as usize];
                    if c == b']' {
                        if n.kind != Kind::Array {
                            return self.fail("Found ] not in an array");
                        }
                        if expect != Expect::Comma && !(expect == Expect::Item && n.items.is_empty()) {
                            return self.fail("Found ] without final element");
                        }
                    } else {
                        if n.kind != Kind::Hash {
                            return self.fail("Found } not in a hash");
                        }
                        if expect != Expect::Comma && !(expect == Expect::Key && n.items.is_empty()) {
                            return self.fail("Found } without final element");
                        }
                    }
                    self.stack.pop();
                    return Read::Node(cc);
                }
                b'n' => {
                    if !self.word(b"ull") {
                        if let Some(r) = self.again(start, line) {
                            return r;
                        }
                        return self.fail("Found misspelling of null");
                    }
                    return self.add_object(Kind::Null).map_or(Read::End, Read::Node);
                }
                b'N' => {
                    if !self.word(b"aN") {
                        if let Some(r) = self.again(start, line) {
                            return r;
                        }
                        return self.fail("Found misspelling of NaN");
                    }
                    return self.fail("JSON does not allow NaN");
                }
                b'I' => {
                    if !self.word(b"nfinity") {
                        if let Some(r) = self.again(start, line) {
                            return r;
                        }
                        return self.fail("Found misspelling of Infinity");
                    }
                    return self.fail("JSON does not allow Infinity");
                }
                b't' => {
                    if !self.word(b"rue") {
                        if let Some(r) = self.again(start, line) {
                            return r;
                        }
                        return self.fail("Found misspelling of true");
                    }
                    return self.add_object(Kind::True).map_or(Read::End, Read::Node);
                }
                b'f' => {
                    if !self.word(b"alse") {
                        if let Some(r) = self.again(start, line) {
                            return r;
                        }
                        return self.fail("Found misspelling of false");
                    }
                    return self.add_object(Kind::False).map_or(Read::End, Read::Node);
                }
                b',' => {
                    if let Some(&(cc, expect)) = self.stack.last() {
                        if expect != Expect::Comma {
                            return self.fail("Found unexpected comma");
                        }
                        let e = if self.nodes[cc as usize].kind == Kind::Hash { Expect::Key } else { Expect::Item };
                        self.stack.last_mut().unwrap().1 = e;
                    }
                }
                b':' => {
                    let Some(&(_, expect)) = self.stack.last() else {
                        return self.fail("Found colon at top level");
                    };
                    if expect != Expect::Colon {
                        return self.fail("Found unexpected colon");
                    }
                    self.stack.last_mut().unwrap().1 = Expect::Value;
                }
                b'-' | b'0'..=b'9' => return self.read_number(c, start, line),
                b'"' => return self.string(start, line),
                _ => return self.fail("Found unexpected character"),
            }
        }
    }

    fn read_number(&mut self, first: u8, start: usize, line: i32) -> Read {
        let mut nb = std::mem::take(&mut self.number_buffer);
        nb.clear();
        let mut decimal = false;
        let mut c = Some(first);
        if first == b'-' {
            nb.push(b'-');
            c = self.next();
        }
        match c {
            Some(b'0') => nb.push(b'0'),
            Some(d @ b'1'..=b'9') => {
                nb.push(d);
                while let Some(d @ b'0'..=b'9') = self.peek() {
                    self.next();
                    nb.push(d);
                }
            }
            _ => {}
        }
        let digits = |p: &mut Pull, nb: &mut Vec<u8>| {
            while let Some(d @ b'0'..=b'9') = p.peek() {
                p.next();
                nb.push(d);
            }
        };
        if self.peek() == Some(b'.') {
            self.next();
            nb.push(b'.');
            decimal = true;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                self.number_buffer = nb;
                if let Some(r) = self.again(start, line) {
                    return r;
                }
                return self.fail("Decimal point without digits");
            }
            digits(self, &mut nb);
        }
        if let Some(e @ (b'e' | b'E')) = self.peek() {
            self.next();
            nb.push(e);
            decimal = true;
            if let Some(s @ (b'+' | b'-')) = self.peek() {
                self.next();
                nb.push(s);
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                self.number_buffer = nb;
                if let Some(r) = self.again(start, line) {
                    return r;
                }
                return self.fail("Exponent without digits");
            }
            digits(self, &mut nb);
        }
        // More digits can come in the next region.
        if let Some(r) = self.again(start, line) {
            self.number_buffer = nb;
            return r;
        }
        let r = match self.add_object(Kind::Number) {
            Some(n) => {
                let d = atof(&nb);
                let mut repr = Repr::Double(d);
                if !decimal && d > MAX_SAFE_INTEGER
                    && let Some(u) = parse_full::<u64>(&nb)
                {
                    repr = Repr::LargeUnsigned(u);
                }
                if !decimal && d < -MAX_SAFE_INTEGER
                    && let Some(s) = parse_full::<i64>(&nb)
                {
                    repr = Repr::LargeSigned(s);
                }
                self.nodes[n as usize].num = repr;
                Read::Node(n)
            }
            None => Read::End,
        };
        self.number_buffer = nb;
        r
    }

    fn string(&mut self, start: usize, line: i32) -> Read {
        let mut val = std::mem::take(&mut self.string_buffer);
        val.clear();
        let r = self.string_inner(&mut val);
        if r.is_err()
            && let Some(m) = self.again(start, line)
        {
            self.string_buffer = val;
            return m;
        }
        let r = match r {
            Ok(()) => match self.add_object(Kind::String) {
                Some(s) => {
                    self.nodes[s as usize].text.extend_from_slice(&val);
                    Read::Node(s)
                }
                None => Read::End,
            },
            Err(e) => self.fail(e),
        };
        self.string_buffer = val;
        r
    }

    fn string_inner(&mut self, val: &mut Vec<u8>) -> Result<(), &'static str> {
        fn push3(val: &mut Vec<u8>, s: u32) {
            val.extend([0xE0 | (s >> 12) as u8, 0x80 | ((s >> 6) & 0x3F) as u8, 0x80 | (s & 0x3F) as u8]);
        }
        let mut surrogate: i64 = -1;
        loop {
            let Some(c) = self.next() else {
                return Err("String without closing quote mark");
            };
            if c == b'"' {
                if surrogate >= 0 {
                    push3(val, surrogate as u32);
                }
                return Ok(());
            } else if c == b'\\' {
                let c = self.next();
                if c == Some(b'u') {
                    let mut ch = 0u32;
                    for _ in 0..4 {
                        let h = self.next().ok_or("Invalid \\u hex character")?;
                        let d = (h as char).to_digit(16).ok_or("Invalid \\u hex character")?;
                        ch = ch * 16 + d;
                    }
                    if (0xd800..=0xdbff).contains(&ch) {
                        if surrogate >= 0 {
                            push3(val, surrogate as u32);
                        }
                        surrogate = ch as i64;
                        continue;
                    } else if (0xdc00..=0xdfff).contains(&ch) && surrogate >= 0 {
                        let c1 = surrogate as u32 - 0xd800;
                        let c2 = ch - 0xdc00;
                        ch = ((c1 << 10) | c2) + 0x10000;
                        surrogate = -1;
                    }
                    if surrogate >= 0 {
                        push3(val, surrogate as u32);
                        surrogate = -1;
                    }
                    if ch <= 0x7F {
                        val.push(ch as u8);
                    } else if ch <= 0x7FF {
                        val.extend([0xC0 | (ch >> 6) as u8, 0x80 | (ch & 0x3F) as u8]);
                    } else if ch <= 0xFFFF {
                        push3(val, ch);
                    } else {
                        val.extend([0xF0 | (ch >> 18) as u8, 0x80 | ((ch >> 12) & 0x3F) as u8, 0x80 | ((ch >> 6) & 0x3F) as u8, 0x80 | (ch & 0x3F) as u8]);
                    }
                } else {
                    if surrogate >= 0 {
                        push3(val, surrogate as u32);
                        surrogate = -1;
                    }
                    val.push(match c {
                        Some(b'"') => b'"',
                        Some(b'\\') => b'\\',
                        Some(b'/') => b'/',
                        Some(b'b') => 8,
                        Some(b'f') => 12,
                        Some(b'n') => b'\n',
                        Some(b'r') => b'\r',
                        Some(b't') => b'\t',
                        _ => return Err("Found backslash followed by unknown character"),
                    });
                }
            } else if c < b' ' {
                return Err("Found control character in string");
            } else {
                if surrogate >= 0 {
                    push3(val, surrogate as u32);
                    surrogate = -1;
                }
                val.push(c);
            }
        }
    }

    /// `json_stringify`.
    pub fn stringify(&self, o: u32) -> Vec<u8> {
        let mut out = Vec::new();
        self.print(&mut out, o);
        out
    }

    fn print(&self, out: &mut Vec<u8>, o: u32) {
        if o == NIL {
            out.extend_from_slice(b"...");
            return;
        }
        let n = &self.nodes[o as usize];
        match n.kind {
            Kind::Hash => {
                out.push(b'{');
                let k = n.items.len() / 2;
                for i in 0..k {
                    self.print(out, n.items[2 * i]);
                    out.push(b':');
                    self.print(out, n.items[2 * i + 1]);
                    if i + 1 < k {
                        out.push(b',');
                    }
                }
                out.push(b'}');
            }
            Kind::Array => {
                out.push(b'[');
                for (i, &c) in n.items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    self.print(out, c);
                }
                out.push(b']');
            }
            Kind::String => {
                out.push(b'"');
                for &c in &n.text {
                    if c == b'\\' || c == b'"' {
                        out.extend([b'\\', c]);
                    } else if c < b' ' {
                        out.extend_from_slice(format!("\\u{c:04x}").as_bytes());
                    } else {
                        out.push(c);
                    }
                }
                out.push(b'"');
            }
            Kind::Number => match n.num {
                Repr::LargeSigned(s) if s != 0 => out.extend_from_slice(s.to_string().as_bytes()),
                Repr::LargeUnsigned(u) if u != 0 => out.extend_from_slice(u.to_string().as_bytes()),
                _ => out.extend_from_slice(crate::dtoa::dtoa_milo(self.number(o)).as_bytes()),
            },
            Kind::Null => out.extend_from_slice(b"null"),
            Kind::True => out.extend_from_slice(b"true"),
            Kind::False => out.extend_from_slice(b"false"),
        }
    }

    /// `json_read_tree`: the first complete top-level value, detached.
    pub fn read_tree(&mut self) -> Option<u32> {
        loop {
            match self.read() {
                Read::Node(j) if self.parent(j) == NIL => {
                    self.root = NIL;
                    return Some(j);
                }
                Read::Node(_) => {}
                _ => return None,
            }
        }
    }
}

/// Parse one JSON value from a string as `json_begin_string` + `json_read_tree`.
/// `Err` has jsonpull's message.
pub fn parse_tree(s: &[u8]) -> Result<(Pull, u32), &'static str> {
    // `read_string` stops at a NUL.
    let s = &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())];
    let mut p = Pull::new();
    p.set_input(s, true);
    match p.read_tree() {
        Some(t) => Ok((p, t)),
        None => Err(p.error.unwrap_or("")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(s: &str) -> (Vec<String>, Option<&'static str>) {
        let mut p = Pull::new();
        p.set_input(s.as_bytes(), true);
        let mut out = Vec::new();
        while let Read::Node(n) = p.read() {
            out.push(String::from_utf8_lossy(&p.stringify(n)).into_owned());
        }
        (out, p.error)
    }

    #[test]
    fn values_in_completion_order() {
        let (v, e) = all(r#"{"a":[1,2],"b":{"c":null}} 7 "x""#);
        assert_eq!(v, ["\"a\"", "1", "2", "[1,2]", "\"b\"", "\"c\"", "null", "{\"c\":null}", "{\"a\":[1,2],\"b\":{\"c\":null}}", "7", "\"x\""]);
        assert_eq!(e, None);
    }

    #[test]
    fn errors() {
        assert_eq!(all("[1 2]").1, Some("Expected a comma, not a list item"));
        assert_eq!(all("{\"a\" 1}").1, Some("Expected a comma or colon"));
        assert_eq!(all("[1,]").1, Some("Found ] without final element"));
        assert_eq!(all("[1.]").1, Some("Decimal point without digits"));
        assert_eq!(all("NaN").1, Some("JSON does not allow NaN"));
        assert_eq!(all("{").1, Some("Reached EOF without all containers being closed"));
        assert_eq!(all("\u{c}").1, Some("Found unexpected character"));
        // A comma at the top level is not an error.
        assert_eq!(all("1,2"), (vec!["1".into(), "2".into()], None));
    }

    #[test]
    fn numbers_and_strings() {
        let (v, _) = all(r#"[18446744073709551615, -9007199254740993, 1e2, "😀é", "\ud800x"]"#);
        assert_eq!(v[0], "18446744073709551615");
        assert_eq!(v[1], "-9007199254740993");
        assert_eq!(v[2], "100");
        let mut p = Pull::new();
        p.set_input(br#""\ud800x""#, true);
        let Read::Node(n) = p.read() else { panic!() };
        assert_eq!(p.text(n), b"\xed\xa0\x80x");
    }

    #[test]
    fn free_keeps_the_entry_as_null() {
        let mut p = Pull::new();
        p.set_input(br#"{"p":{"type":"x"},"q":1}"#, true);
        let mut inner = NIL;
        loop {
            match p.read() {
                Read::Node(n) if p.kind(n) == Kind::Hash && p.parent(n) != NIL => {
                    inner = n;
                    p.free(n);
                }
                Read::Node(n) if p.parent(n) == NIL => {
                    assert_ne!(inner, NIL);
                    assert_eq!(p.stringify(n), br#"{"p":null,"q":1}"#);
                    break;
                }
                Read::Node(_) => {}
                r => panic!("{r:?}"),
            }
        }
    }

    #[test]
    fn regions() {
        let mut p = Pull::new();
        p.set_input(b"[1,\n", false);
        assert!(matches!(p.read(), Read::Node(_)));
        assert_eq!(p.read(), Read::More);
        p.set_input(b"2]\n", true);
        assert!(matches!(p.read(), Read::Node(_)));
        let Read::Node(a) = p.read() else { panic!() };
        assert_eq!(p.stringify(a), b"[1,2]");
        assert_eq!(p.read(), Read::End);
        assert_eq!(p.error, None);
    }

    /// The values and the error of `s` given in regions of `n` bytes.
    fn split(s: &[u8], n: usize) -> (Vec<Vec<u8>>, Option<&'static str>, i32) {
        let mut p = Pull::new();
        let mut out = Vec::new();
        let parts: Vec<&[u8]> = s.chunks(n.max(1)).collect();
        for (i, part) in parts.iter().enumerate() {
            p.set_input(part, i + 1 == parts.len());
            loop {
                match p.read() {
                    Read::Node(n) => out.push(p.stringify(n)),
                    Read::More => break,
                    Read::End => return (out, p.error, p.line),
                }
            }
        }
        if parts.is_empty() {
            p.set_input(&[], true);
            while let Read::Node(n) = p.read() {
                out.push(p.stringify(n));
            }
        }
        (out, p.error, p.line)
    }

    #[test]
    fn regions_can_end_in_a_token() {
        let texts: [&[u8]; 9] = [
            b"\xef\xbb\xbf{\"a\":[12345,-0.5e+10,true,false,null],\"s\":\"x\\u00e9\\ud83d\\ude00\\n\"}\n 77 \"tail\"",
            b"[1.]",
            b"[1e]",
            b"[nul]",
            b"\"abc\ndef\"",
            b"\xef\xbbx",
            b"[1,\n2,\n\"\\q\"]",
            b"-",
            b"123",
        ];
        for t in texts {
            let whole = split(t, t.len());
            for n in 1..t.len() {
                assert_eq!(split(t, n), whole, "{} at {n}", String::from_utf8_lossy(t));
            }
        }
    }
}
