//! Minimal protobuf writer with protozero's layout.

#[inline]
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
fn key(out: &mut Vec<u8>, field: u32, wire: u32) {
    put_varint(out, ((field << 3) | wire) as u64);
}

#[inline]
pub fn add_varint_field(out: &mut Vec<u8>, field: u32, v: u64) {
    key(out, field, 0);
    put_varint(out, v);
}

#[inline]
pub fn add_bytes_field(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    key(out, field, 2);
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

#[inline]
pub fn add_fixed32_field(out: &mut Vec<u8>, field: u32, v: u32) {
    key(out, field, 5);
    out.extend_from_slice(&v.to_le_bytes());
}

#[inline]
pub fn add_fixed64_field(out: &mut Vec<u8>, field: u32, v: u64) {
    key(out, field, 1);
    out.extend_from_slice(&v.to_le_bytes());
}

/// Packed repeated uint32. protozero writes nothing for an empty range.
#[inline]
pub fn add_packed_u32(out: &mut Vec<u8>, field: u32, vals: &[u32], scratch: &mut Vec<u8>) {
    if vals.is_empty() {
        return;
    }
    scratch.clear();
    for &v in vals {
        put_varint(scratch, v as u64);
    }
    add_bytes_field(out, field, scratch);
}
