//! MVT values (`mvt_value`) and tippecanoe's type choice (`stringified_to_mvt_value`).

use crate::pbf;
use vtmake_core::AttrType;

#[derive(Clone, Debug, PartialEq)]
pub enum MvtValue {
    String(Box<str>),
    Float(f32),
    Double(f64),
    Int(i64),
    Uint(u64),
    Sint(i64),
    Bool(bool),
}

impl MvtValue {
    /// `std::hash<mvt_value>`: fnv1a over the C string plus a 0 byte, or over the raw
    /// little-endian bytes of the number.
    pub fn tippecanoe_hash(&self) -> u64 {
        match self {
            MvtValue::String(s) => {
                let b = s.as_bytes();
                let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                crate::fnv1a_cstr(&b[..end], 0)
            }
            MvtValue::Float(f) => crate::fnv1a(&f.to_le_bytes()),
            MvtValue::Double(d) => crate::fnv1a(&d.to_le_bytes()),
            MvtValue::Int(i) | MvtValue::Sint(i) => crate::fnv1a(&i.to_le_bytes()),
            MvtValue::Uint(u) => crate::fnv1a(&u.to_le_bytes()),
            MvtValue::Bool(b) => crate::fnv1a(&[*b as u8]),
        }
    }

    /// `mvt_value::operator==` (same type and value; NaN is not equal to NaN).
    pub fn same(&self, o: &MvtValue) -> bool {
        match (self, o) {
            (MvtValue::String(a), MvtValue::String(b)) => a == b,
            (MvtValue::Float(a), MvtValue::Float(b)) => a == b,
            (MvtValue::Double(a), MvtValue::Double(b)) => a == b,
            (MvtValue::Int(a), MvtValue::Int(b)) => a == b,
            (MvtValue::Uint(a), MvtValue::Uint(b)) => a == b,
            (MvtValue::Sint(a), MvtValue::Sint(b)) => a == b,
            (MvtValue::Bool(a), MvtValue::Bool(b)) => a == b,
            _ => false,
        }
    }

    /// Encode as an MVT `Value` message body.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            MvtValue::String(s) => pbf::add_bytes_field(out, 1, s.as_bytes()),
            MvtValue::Float(f) => pbf::add_fixed32_field(out, 2, f.to_bits()),
            MvtValue::Double(d) => pbf::add_fixed64_field(out, 3, d.to_bits()),
            MvtValue::Int(i) => pbf::add_varint_field(out, 4, *i as u64),
            MvtValue::Uint(u) => pbf::add_varint_field(out, 5, *u),
            MvtValue::Sint(i) => pbf::add_varint_field(out, 6, ((*i << 1) ^ (*i >> 63)) as u64),
            MvtValue::Bool(b) => pbf::add_varint_field(out, 7, *b as u64),
        }
    }
}

/// The digits after an optional sign, then an optional `.000…` tail: tippecanoe's
/// `is_integer` / `is_unsigned_integer` accept these. Returns the digit part.
/// tippecanoe uses `strtoll`/`strtoull` with base 0. The stringified numbers come
/// from `dtoa_milo` or are exact JSON integers, so there are no hex or octal forms.
fn integer_digits(s: &str) -> Option<(bool, &str)> {
    let (neg, rest) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let n = rest.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 {
        return None;
    }
    let tail = &rest[n..];
    if !tail.is_empty() {
        let t = tail.strip_prefix('.')?;
        if !t.bytes().all(|c| c == b'0') {
            return None;
        }
    }
    Some((neg, &rest[..n]))
}

/// tippecanoe `stringified_to_mvt_value`. Returns None for null (nulls are not written).
pub fn stringified_to_mvt_value(ty: AttrType, s: &str) -> Option<MvtValue> {
    Some(match ty {
        AttrType::Null => return None,
        AttrType::Bool => MvtValue::Bool(s.as_bytes().first() == Some(&b't')),
        AttrType::String => MvtValue::String(s.into()),
        AttrType::Number => {
            if let Some((neg, digits)) = integer_digits(s) {
                if digits.len() > 1 && digits.starts_with('0') {
                    // Base 0: octal, or not an integer.
                    return Some(c_number(s));
                }
                if !neg {
                    // strtoull: an overflow is ERANGE, then strtoll also fails (ERANGE).
                    if let Ok(u) = digits.parse::<u64>() {
                        return Some(if u <= i64::MAX as u64 { MvtValue::Int(u as i64) } else { MvtValue::Uint(u) });
                    }
                } else if let Ok(v) = format!("-{digits}").parse::<i64>() {
                    return Some(MvtValue::Sint(v));
                }
            }
            float_or_double(s)
        }
    })
}

unsafe extern "C" {
    fn strtoll(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char, base: i32) -> i64;
    fn strtoull(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char, base: i32) -> u64;
    fn strtof(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char) -> f32;
    fn strtod(s: *const std::ffi::c_char, end: *mut *mut std::ffi::c_char) -> f64;
    fn __errno_location() -> *mut i32;
}

const ERANGE: i32 = 34;
const EINVAL: i32 = 22;

/// tippecanoe's `stringified_to_mvt_value` for a number text where the Rust
/// parsers can give another result (from `--set-attribute` or `-T`, for
/// example `08`, `0x1F`, `9x` or ` 5`): the C functions `strtoull`/`strtoll`
/// (base 0), `strtof`, `strtod`.
fn c_number(s: &str) -> MvtValue {
    let end_at = s.find('\0').unwrap_or(s.len());
    let mut c = Vec::with_capacity(end_at + 1);
    c.extend_from_slice(&s.as_bytes()[..end_at]);
    c.push(0);
    let p = c.as_ptr().cast::<std::ffi::c_char>();
    // `*endptr` is '\0', or ".000…": an integer.
    let integer_end = |e: *mut std::ffi::c_char| {
        // SAFETY: `e` points into `c`, which ends with a NUL byte.
        let rest = unsafe { std::ffi::CStr::from_ptr(e) }.to_bytes();
        rest.is_empty() || (rest[0] == b'.' && rest[1..].iter().all(|&ch| ch == b'0'))
    };
    // SAFETY: `c` is a NUL-terminated buffer that lives during the calls; errno
    // is the thread's own variable.
    unsafe {
        let errno = __errno_location();
        // is_unsigned_integer
        let mut q = p;
        while matches!(*q as u8, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') {
            q = q.add(1);
        }
        if *q as u8 != b'-' {
            *errno = 0;
            let mut e = std::ptr::null_mut();
            let v = strtoull(q, &mut e, 0);
            let err = *errno;
            if !(v == 0 && err != 0) && !(v == u64::MAX && (err == ERANGE || err == EINVAL)) && integer_end(e) {
                return if v <= i64::MAX as u64 { MvtValue::Int(v as i64) } else { MvtValue::Uint(v) };
            }
        }
        // is_integer
        *errno = 0;
        let mut e = std::ptr::null_mut();
        let v = strtoll(p, &mut e, 0);
        let err = *errno;
        if !(v == 0 && err != 0) && !((v == i64::MIN || v == i64::MAX) && (err == ERANGE || err == EINVAL)) && integer_end(e) {
            return MvtValue::Sint(v);
        }
        *errno = 0;
        let mut e = std::ptr::null_mut();
        let f = strtof(p, &mut e);
        if e.cast_const() == p || (f.is_infinite() && *errno == ERANGE) {
            *errno = 0;
            let d = strtod(p, &mut e);
            if e.cast_const() == p || (d.is_infinite() && *errno == ERANGE) {
                eprintln!("Warning: numeric value {} could not be represented", &s[..end_at]);
            }
            return MvtValue::Double(d);
        }
        let d = strtod(p, std::ptr::null_mut());
        if f as f64 == d { MvtValue::Float(f) } else { MvtValue::Double(d) }
    }
}

fn float_or_double(s: &str) -> MvtValue {
    let Ok(d) = s.parse::<f64>() else {
        // Not a number for Rust (for example `0x1F`, `9x`, ` 5`).
        return c_number(s);
    };
    match s.parse::<f32>() {
        // strtof overflow (HUGE_VALF + ERANGE): use strtod.
        Ok(f) if f.is_infinite() && !d.is_infinite() => MvtValue::Double(d),
        Ok(f) if f as f64 == d => MvtValue::Float(f),
        _ => MvtValue::Double(d),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn n(s: &str) -> MvtValue {
        stringified_to_mvt_value(AttrType::Number, s).unwrap()
    }
    #[test]
    fn number_types_match_the_spec() {
        assert_eq!(n("5"), MvtValue::Int(5));
        assert_eq!(n("-3"), MvtValue::Sint(-3));
        assert_eq!(n("0.5"), MvtValue::Float(0.5));
        assert_eq!(n("139.68762665"), MvtValue::Double(139.68762665));
        assert_eq!(n("1e+21"), MvtValue::Double(1e21));
        assert_eq!(n("1e-7"), MvtValue::Double(1e-7));
        assert_eq!(n("3.0"), MvtValue::Int(3));
        assert_eq!(n("0"), MvtValue::Int(0));
        assert_eq!(n("12345678901234567890"), MvtValue::Uint(12345678901234567890));
        // Other texts: the C functions (base 0).
        assert_eq!(n("08"), MvtValue::Float(8.0));
        assert_eq!(n("010"), MvtValue::Int(8));
        assert_eq!(n("0x1F"), MvtValue::Int(31));
        assert_eq!(n("9x"), MvtValue::Float(9.0));
        assert_eq!(n(" -5"), MvtValue::Sint(-5));
        assert_eq!(n(""), MvtValue::Int(0));
        assert_eq!(n("5."), MvtValue::Int(5));
        assert_eq!(n("abc"), MvtValue::Double(0.0));
    }
}
