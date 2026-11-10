//! JavaScript value rules for the Postgres COPY input (`crate::pgcopy`).
//!
//! A Node.js exporter reads each row with node-pg
//! (`JSON.parse`), builds a feature object, and writes it with
//! `JSON.stringify`. To give the same features without Node, the COPY input
//! applies the same rules:
//!
//! - [`number_text`]: `Number.prototype.toString` (ECMA-262
//!   `Number::toString`): the shortest digits that give the same double.
//! - [`to_number`] and [`string_to_number`]: `Number(x)` (`ToNumber`).
//! - [`Value`] objects keep the JavaScript key order: array index keys
//!   (`"0"` to `"4294967294"`) first, in numeric order, then the other keys in
//!   insertion order (`OrdinaryOwnPropertyKeys`).
//! - [`parse`] reads JSON text as `JSON.parse` does (the last duplicate key
//!   wins, at the position of the first one), optionally with the Postgres
//!   `jsonb` rules first (the last duplicate wins, keys sorted by length,
//!   then by bytes).

use std::fmt::Write as _;

/// A JavaScript value after `JSON.parse`.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    /// Members in JavaScript key order (see [`js_order`]).
    Obj(Vec<(String, Value)>),
}

/// The digits `d1…dk` (no leading or trailing zeros) and `n` of a Ryu text,
/// with value `0.d1…dk × 10^n`.
fn shortest(text: &str) -> (String, i32) {
    let (mant, exp) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().expect("Ryu exponent")),
        None => (text, 0),
    };
    let point = mant.find('.').unwrap_or(mant.len()) as i32;
    let all: String = mant.chars().filter(|&c| c != '.').collect();
    let lead = all.len() - all.trim_start_matches('0').len();
    let digits = all.trim_start_matches('0').trim_end_matches('0').to_string();
    (digits, point - lead as i32 + exp)
}

/// `Number::toString(x)` (radix 10). `-0` gives `"0"`.
pub fn number_text(x: f64, out: &mut String) {
    if x.is_nan() {
        out.push_str("NaN");
        return;
    }
    if x == 0.0 {
        out.push('0');
        return;
    }
    if x.is_infinite() {
        out.push_str(if x < 0.0 { "-Infinity" } else { "Infinity" });
        return;
    }
    if x < 0.0 {
        out.push('-');
    }
    // Ryu gives the shortest digits that read back as the same double, the
    // closest of them, and on a tie the even one (as ECMA-262 asks).
    let mut buf = ryu::Buffer::new();
    let (digits, n) = shortest(buf.format_finite(x.abs()));
    let k = digits.len() as i32;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..n - k {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..-n {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if n - 1 >= 0 { '+' } else { '-' });
        let _ = write!(out, "{}", (n - 1).abs());
    }
}

/// `JSON.stringify` of a number: [`number_text`], or `null` if not finite.
pub fn json_number_text(x: f64, out: &mut String) {
    if x.is_finite() {
        number_text(x, out)
    } else {
        out.push_str("null")
    }
}

/// `WhiteSpace` and `LineTerminator` of ECMA-262 (`StringToNumber` trims them).
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// The value of the digits of `s` in `radix` (2, 8 or 16), correctly rounded
/// (round half to even); `None` if a character is not a digit or `s` is empty.
fn radix_value(s: &str, radix: u32) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    let bits_per = radix.trailing_zeros();
    let mut bits: Vec<bool> = Vec::new();
    for c in s.chars() {
        let d = c.to_digit(radix)?;
        for i in (0..bits_per).rev() {
            let b = (d >> i) & 1 == 1;
            if b || !bits.is_empty() {
                bits.push(b);
            }
        }
    }
    if bits.is_empty() {
        return Some(0.0);
    }
    let len = bits.len();
    if len <= 64 {
        let v = bits.iter().fold(0u64, |a, &b| (a << 1) | b as u64);
        return Some(v as f64);
    }
    // The top 64 bits, with a sticky bit for the rest: `u64 as f64` then
    // rounds as the exact value would (64 > 53 + 2).
    let mut top = bits[..64].iter().fold(0u64, |a, &b| (a << 1) | b as u64);
    if bits[64..].iter().any(|&b| b) {
        top |= 1;
    }
    let scale = (len - 64) as i32;
    Some(if scale > 1100 { f64::INFINITY } else { top as f64 * 2f64.powi(scale) })
}

/// `StringToNumber` (ECMA-262): `Number("…")`.
pub fn string_to_number(s: &str) -> f64 {
    let t = s.trim_matches(is_js_space);
    if t.is_empty() {
        return 0.0;
    }
    let b = t.as_bytes();
    if b.len() > 2 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            return radix_value(&t[2..], radix).unwrap_or(f64::NAN);
        }
    }
    // StrDecimalLiteral: [+-] (Infinity | digits [. [digits]] | . digits) [(e|E) [+-] digits]
    let (neg, rest) = match b[0] {
        b'+' => (false, &t[1..]),
        b'-' => (true, &t[1..]),
        _ => (false, t),
    };
    let v = if rest == "Infinity" {
        f64::INFINITY
    } else {
        let r = rest.as_bytes();
        let mut i = 0;
        while i < r.len() && r[i].is_ascii_digit() {
            i += 1;
        }
        let int_part = &rest[..i];
        let mut frac_part = "";
        if i < r.len() && r[i] == b'.' {
            let s0 = i + 1;
            i = s0;
            while i < r.len() && r[i].is_ascii_digit() {
                i += 1;
            }
            frac_part = &rest[s0..i];
        }
        if int_part.is_empty() && frac_part.is_empty() {
            return f64::NAN;
        }
        let mut exp_part = "";
        if i < r.len() && (r[i] == b'e' || r[i] == b'E') {
            let s0 = i + 1;
            let mut j = s0;
            if j < r.len() && (r[j] == b'+' || r[j] == b'-') {
                j += 1;
            }
            let d0 = j;
            while j < r.len() && r[j].is_ascii_digit() {
                j += 1;
            }
            if j == d0 {
                return f64::NAN;
            }
            exp_part = &rest[s0..j];
            i = j;
        }
        if i != r.len() {
            return f64::NAN;
        }
        let mut text = String::with_capacity(rest.len() + 3);
        text.push_str(if int_part.is_empty() { "0" } else { int_part });
        if !frac_part.is_empty() {
            text.push('.');
            text.push_str(frac_part);
        }
        if !exp_part.is_empty() {
            text.push('e');
            text.push_str(exp_part);
        }
        text.parse::<f64>().unwrap_or(f64::NAN)
    };
    if neg { -v } else { v }
}

/// A JavaScript `TypeError` ("Cannot convert object to primitive value").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeError;

/// An object from `JSON.parse` has an own `"toString"` member. Its value is
/// never a function, so `OrdinaryToPrimitive` skips it, also skips `valueOf`
/// (own and not callable, or `Object.prototype.valueOf`, which returns the
/// object), and throws a `TypeError`. Without it, `Object.prototype.toString`
/// gives `"[object Object]"`.
fn own_to_string(o: &[(String, Value)]) -> bool {
    o.iter().any(|(k, _)| k == "toString")
}

/// `ToString` of an array element in `Array.prototype.join` (null is "").
fn join_text(v: &Value, out: &mut String) -> Result<(), TypeError> {
    match v {
        Value::Null => {}
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => number_text(*n, out),
        Value::Str(s) => out.push_str(s),
        Value::Arr(a) => {
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                join_text(e, out)?;
            }
        }
        Value::Obj(o) => {
            if own_to_string(o) {
                return Err(TypeError);
            }
            out.push_str("[object Object]")
        }
    }
    Ok(())
}

/// `Number(v)` (`ToNumber` with `ToPrimitive`); `None` is `undefined` (NaN).
/// An array becomes `join(",")` of its elements; an object is NaN, or a
/// `TypeError` if it has an own `"toString"` member (see [`own_to_string`]).
pub fn to_number(v: Option<&Value>) -> Result<f64, TypeError> {
    Ok(match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Num(n)) => *n,
        Some(Value::Str(s)) => string_to_number(s),
        Some(a @ Value::Arr(_)) => {
            let mut s = String::new();
            join_text(a, &mut s)?;
            string_to_number(&s)
        }
        Some(Value::Obj(o)) => {
            if own_to_string(o) {
                return Err(TypeError);
            }
            f64::NAN
        }
    })
}

/// The value of an array index key (`CanonicalNumericIndexString` with a
/// value below 2^32 - 1), else `None`.
pub fn array_index(key: &str) -> Option<u32> {
    let b = key.as_bytes();
    if b.is_empty() || b.len() > 10 || (b.len() > 1 && b[0] == b'0') || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v: u64 = key.parse().ok()?;
    (v < u32::MAX as u64).then_some(v as u32)
}

/// Put the members in JavaScript key order: array index keys first, in
/// numeric order, then the other keys in their present order.
pub fn js_order<V>(members: &mut Vec<(String, V)>) {
    if members.iter().any(|(k, _)| array_index(k).is_some()) {
        let mut idx: Vec<(String, V)> = Vec::new();
        let mut rest: Vec<(String, V)> = Vec::new();
        for m in members.drain(..) {
            if array_index(&m.0).is_some() { idx.push(m) } else { rest.push(m) }
        }
        idx.sort_by_key(|m| array_index(&m.0).unwrap());
        members.extend(idx);
        members.extend(rest);
    }
}

/// The key order of Postgres `jsonb`: shorter keys first, then by bytes.
pub fn jsonb_key_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

/// A JSON parse error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseErr {
    /// More nested arrays and objects than the limit (see [`parse_depth`]).
    TooDeep,
    Invalid(String),
}

impl std::fmt::Display for ParseErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseErr::TooDeep => f.write_str("JSON nesting over the limit"),
            ParseErr::Invalid(m) => f.write_str(m),
        }
    }
}

/// The nesting limit of [`parse`]: the callers' recursion is safe on a
/// normal thread stack.
pub const FAST_DEPTH: usize = 256;

/// The largest nesting that the COPY reader accepts (on a thread with a
/// 1 GiB stack, see `pgcopy::DEEP_STACK`). Measured stack use of a deep row
/// in check mode (parse, strip nulls, `Number()`, attribute text, NDJSON line,
/// reference reader): about 2.0 KB per level in a debug build, 0.4 KB in a
/// release build (`pgcopy::tests::deep_stack_probe`). 100,000 levels need
/// about 200 MB (debug): 5x margin. vtmake's NDJSON reader has no limit;
/// Postgres and Node have lower ones (a few thousand levels).
pub const MAX_DEPTH: usize = 100_000;

/// Parse JSON text as `JSON.parse` does: the last duplicate key wins, at the
/// position of the first one; then the JavaScript key order. At most
/// [`FAST_DEPTH`] levels.
pub fn parse(text: &str) -> Result<Value, ParseErr> {
    parse_depth(text, FAST_DEPTH)
}

/// [`parse`] with at most `max_depth` nested arrays and objects. Deeper text
/// is rejected by [`nesting_depth`] before the recursive parse.
pub fn parse_depth(text: &str, max_depth: usize) -> Result<Value, ParseErr> {
    // Check the depth first, without recursion: a value over the limit never
    // reaches the recursive parser (and its stack).
    if nesting_depth(text.as_bytes()) > max_depth {
        return Err(ParseErr::TooDeep);
    }
    let mut p = crate::json::Parser { s: text, i: 0 };
    let v = value(&mut p, 0, max_depth)?;
    p.space();
    if p.i != text.len() {
        return Err(ParseErr::Invalid("trailing JSON input".into()));
    }
    Ok(v)
}

/// The largest nesting of arrays and objects in JSON text, in one pass
/// without recursion. Brackets in strings do not count. (For text that is
/// not valid JSON the number has no meaning; the parser then reports it.)
pub fn nesting_depth(text: &[u8]) -> usize {
    let (mut depth, mut max) = (0usize, 0usize);
    let (mut in_str, mut esc) = (false, false);
    for &b in text {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

fn invalid(e: crate::ParseError) -> ParseErr {
    ParseErr::Invalid(e.message)
}

fn value(p: &mut crate::json::Parser<'_>, depth: usize, max: usize) -> Result<Value, ParseErr> {
    use crate::json::Value as J;
    p.space();
    let open = p.s.as_bytes().get(p.i).copied();
    if !matches!(open, Some(b'[' | b'{')) {
        // A scalar: the json.rs parser does not recurse for it.
        return Ok(match p.value(0).map_err(invalid)? {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(b),
            J::Number(n) => Value::Num(n.parse::<f64>().unwrap_or(f64::NAN)),
            J::String(s) => Value::Str(s.into_owned()),
            J::Array(_) | J::Object(_) => unreachable!("not a scalar"),
        });
    }
    if depth >= max {
        return Err(ParseErr::TooDeep);
    }
    let object = open == Some(b'{');
    let end = if object { b'}' } else { b']' };
    p.i += 1;
    p.space();
    let mut arr = Vec::new();
    let mut obj: Vec<(String, Value)> = Vec::new();
    if p.s.as_bytes().get(p.i) == Some(&end) {
        p.i += 1;
    } else {
        loop {
            if object {
                p.space();
                if p.s.as_bytes().get(p.i) != Some(&b'"') {
                    return Err(ParseErr::Invalid("expected object key".into()));
                }
                let k = p.string().map_err(invalid)?;
                p.space();
                if p.s.as_bytes().get(p.i) != Some(&b':') {
                    return Err(ParseErr::Invalid("expected colon".into()));
                }
                p.i += 1;
                let v = value(p, depth + 1, max)?;
                match obj.iter_mut().find(|m| m.0 == k.as_ref()) {
                    Some(m) => m.1 = v,
                    None => obj.push((k.into_owned(), v)),
                }
            } else {
                arr.push(value(p, depth + 1, max)?);
            }
            p.space();
            match p.s.as_bytes().get(p.i) {
                Some(c) if *c == end => {
                    p.i += 1;
                    break;
                }
                Some(b',') => p.i += 1,
                _ => return Err(ParseErr::Invalid("expected JSON separator".into())),
            }
        }
    }
    Ok(if object {
        js_order(&mut obj);
        Value::Obj(obj)
    } else {
        Value::Arr(arr)
    })
}

/// `jsonb_strip_nulls`: remove the object members with a null value, at all
/// depths (also in objects inside arrays). Array elements stay.
pub fn strip_nulls(v: &mut Value) {
    match v {
        Value::Arr(a) => a.iter_mut().for_each(strip_nulls),
        Value::Obj(o) => {
            o.retain(|(_, v)| !matches!(v, Value::Null));
            o.iter_mut().for_each(|(_, v)| strip_nulls(v));
        }
        _ => {}
    }
}

/// A string as `JSON.stringify` writes it.
pub fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `JSON.stringify(v)`.
pub fn stringify(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => json_number_text(*n, out),
        Value::Str(s) => quote(s, out),
        Value::Arr(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                stringify(e, out);
            }
            out.push(']');
        }
        Value::Obj(o) => {
            out.push('{');
            for (i, (k, e)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                quote(k, out);
                out.push(':');
                stringify(e, out);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nt(x: f64) -> String {
        let mut s = String::new();
        number_text(x, &mut s);
        s
    }

    #[test]
    fn number_text_follows_ecma262() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (9007199254740993.0, "9007199254740992"),
            (1152921504606846976.0, "1152921504606847000"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (139.76712345, "139.76712345"),
            // An exact tie of two shortest texts: the even digit.
            (1924910681147418.25, "1924910681147418.2"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for &(x, want) in cases {
            assert_eq!(nt(x), want, "{x:?}");
        }
    }

    #[test]
    fn string_to_number_follows_ecma262() {
        let cases: &[(&str, f64)] = &[
            ("", 0.0),
            ("  ", 0.0),
            (" 12 ", 12.0),
            ("\u{3000}7\u{feff}", 7.0),
            ("1e1", 10.0),
            (".5", 0.5),
            ("5.", 5.0),
            ("+5", 5.0),
            ("-5", -5.0),
            ("0x10", 16.0),
            ("0X1f", 31.0),
            ("0o17", 15.0),
            ("0b101", 5.0),
            ("Infinity", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("00012", 12.0),
        ];
        for &(s, want) in cases {
            assert_eq!(string_to_number(s), want, "{s:?}");
        }
        for s in ["abc", "1x", "-0x10", "0x", "1e", ".", "+", "1_000", "infinity", "inf", "NaN", "1 2", "\u{85}1", "0x1g"] {
            assert!(string_to_number(s).is_nan(), "{s:?}");
        }
        assert!(string_to_number("-0").is_sign_negative());
        // 2^64 + 1 rounds down, 2^64 + 2^11 + 1 rounds up.
        assert_eq!(string_to_number("0x10000000000000001"), 18446744073709551616.0);
        assert_eq!(string_to_number("0x10000000000000801"), 18446744073709555712.0);
    }

    #[test]
    fn to_number_of_values() {
        let n = |t: &str| to_number(Some(&parse(t).unwrap()));
        assert_eq!(n("null"), Ok(0.0));
        assert!(to_number(None).unwrap().is_nan());
        assert_eq!(n("true"), Ok(1.0));
        assert_eq!(n("[5]"), Ok(5.0));
        assert_eq!(n(r#"["5"]"#), Ok(5.0));
        assert_eq!(n("[]"), Ok(0.0));
        assert_eq!(n("[null]"), Ok(0.0));
        assert!(n("[1,2]").unwrap().is_nan());
        assert!(n("{}").unwrap().is_nan());
        assert_eq!(n(r#"[[" 3 "]]"#), Ok(3.0));
        // ToPrimitive: an own "toString" member is not callable: TypeError.
        assert_eq!(n(r#"{"toString":null}"#), Err(TypeError));
        assert_eq!(n(r#"{"toString":"7"}"#), Err(TypeError));
        assert!(n(r#"{"valueOf":7}"#).unwrap().is_nan());
        assert_eq!(n(r#"[{"toString":1}]"#), Err(TypeError));
        assert!(n(r#"[{"valueOf":1}]"#).unwrap().is_nan());
        assert_eq!(n(r#"[[{"a":{"toString":1}}]]"#).map(f64::is_nan), Ok(true), "a nested member is not converted");
    }

    #[test]
    fn keys_in_javascript_and_jsonb_order() {
        let v = parse(r#"{"b":1,"10":2,"a":3,"2":4,"01":5,"4294967295":6,"4294967294":7,"b":8}"#).unwrap();
        let Value::Obj(o) = v else { panic!() };
        let keys: Vec<&str> = o.iter().map(|m| m.0.as_str()).collect();
        assert_eq!(keys, ["2", "10", "4294967294", "b", "a", "01", "4294967295"]);
        assert_eq!(o[3].1, Value::Num(8.0));
    }

    #[test]
    fn nesting_limits() {
        let deep = |n: usize| format!("{}1{}", "[".repeat(n), "]".repeat(n));
        assert!(parse(&deep(FAST_DEPTH)).is_ok());
        assert_eq!(parse(&deep(FAST_DEPTH + 1)), Err(ParseErr::TooDeep));
        assert!(matches!(parse("[1,"), Err(ParseErr::Invalid(_))));
        assert_eq!(nesting_depth(br#"[{"a":"[[[{","b":[1,{"c":"\\\"]"}]}]"#), 4);
        assert_eq!(nesting_depth(b"1"), 0);
        // Over the limit: rejected by the scan, without recursion (a large
        // depth on a small stack must not overflow).
        let n = 5_000_000;
        let deep = format!("{}{}", "[".repeat(n), "]".repeat(n));
        let r = std::thread::Builder::new().stack_size(64 << 10).spawn(move || parse_depth(&deep, MAX_DEPTH)).unwrap().join().unwrap();
        assert_eq!(r, Err(ParseErr::TooDeep));
    }

    #[test]
    fn stringify_and_strip() {
        let mut v = parse(r#"{"a":null,"b":[null,{"c":null,"d":1e400}],"e":"\u0001\"\\\n"}"#).unwrap();
        strip_nulls(&mut v);
        let mut s = String::new();
        stringify(&v, &mut s);
        assert_eq!(s, r#"{"b":[null,{"d":null}],"e":"\u0001\"\\\n"}"#);
    }
}
