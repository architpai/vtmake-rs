//! zlib 1.3.1, vendored, with changes that make deflate faster but keep the
//! output bytes the same (see `PATCHES.md`). tippecanoe links classic zlib
//! 1.3.1, so this crate gives the same tile bytes as tippecanoe.
//!
//! All C symbols have the prefix `vtmake_z_`. Another zlib in the same binary
//! (for example the system zlib) does not clash with this copy.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use std::mem::MaybeUninit;

/// `z_stream` of zlib 1.3.1. `zalloc`, `zfree` and `opaque` are zero (`Z_NULL`),
/// so zlib uses `malloc` and `free`.
#[repr(C)]
struct ZStream {
    next_in: *const u8,
    avail_in: c_uint,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: c_uint,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut c_void,
    zalloc: Option<unsafe extern "C" fn(*mut c_void, c_uint, c_uint) -> *mut c_void>,
    zfree: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    opaque: *mut c_void,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;
const Z_NO_FLUSH: c_int = 0;
const Z_FINISH: c_int = 4;
const Z_DEFLATED: c_int = 8;
const Z_DEFAULT_STRATEGY: c_int = 0;

unsafe extern "C" {
    fn vtmake_z_zlibVersion() -> *const c_char;
    fn vtmake_z_deflateInit2_(
        s: *mut ZStream,
        level: c_int,
        method: c_int,
        window_bits: c_int,
        mem_level: c_int,
        strategy: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn vtmake_z_deflateReset(s: *mut ZStream) -> c_int;
    fn vtmake_z_deflateBound(s: *mut ZStream, len: c_ulong) -> c_ulong;
    fn vtmake_z_deflate(s: *mut ZStream, flush: c_int) -> c_int;
    fn vtmake_z_deflateEnd(s: *mut ZStream) -> c_int;
    fn vtmake_z_inflateInit2_(s: *mut ZStream, window_bits: c_int, version: *const c_char, stream_size: c_int) -> c_int;
    fn vtmake_z_inflate(s: *mut ZStream, flush: c_int) -> c_int;
    fn vtmake_z_inflateEnd(s: *mut ZStream) -> c_int;
}

/// The zlib version string of the vendored source ("1.3.1").
pub fn version() -> &'static str {
    unsafe { std::ffi::CStr::from_ptr(vtmake_z_zlibVersion()) }.to_str().unwrap()
}

/// A deflate stream with a gzip wrapper and the tippecanoe settings
/// (`mvt.cpp:compress`): level 6, windowBits 31, memLevel 8, default strategy.
/// Use one encoder for many inputs: `deflateReset` gives the same bytes as a new
/// stream, and the encoder does not allocate again.
pub struct GzipEncoder {
    s: Box<MaybeUninit<ZStream>>,
}

// The stream has no references to thread-local data.
unsafe impl Send for GzipEncoder {}

impl GzipEncoder {
    pub fn new() -> GzipEncoder {
        let mut s = Box::new(MaybeUninit::<ZStream>::zeroed());
        let rc = unsafe {
            vtmake_z_deflateInit2_(
                s.as_mut_ptr(),
                6,
                Z_DEFLATED,
                31,
                8,
                Z_DEFAULT_STRATEGY,
                vtmake_z_zlibVersion(),
                std::mem::size_of::<ZStream>() as c_int,
            )
        };
        assert_eq!(rc, Z_OK, "deflateInit2 failed");
        GzipEncoder { s }
    }

    /// gzip `input` and append the result to `out`.
    pub fn compress(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let len = c_uint::try_from(input.len()).expect("input is too large for one deflate call");
        let s = self.s.as_mut_ptr();
        unsafe {
            assert_eq!(vtmake_z_deflateReset(s), Z_OK);
            let bound = vtmake_z_deflateBound(s, len as c_ulong) as usize;
            let start = out.len();
            out.reserve(bound);
            (*s).next_in = input.as_ptr();
            (*s).avail_in = len;
            (*s).next_out = out.as_mut_ptr().add(start);
            (*s).avail_out = bound as c_uint;
            let rc = vtmake_z_deflate(s, Z_FINISH);
            assert_eq!(rc, Z_STREAM_END, "deflate did not finish");
            out.set_len(start + (*s).total_out as usize);
        }
    }
}

impl Default for GzipEncoder {
    fn default() -> Self {
        GzipEncoder::new()
    }
}

impl Drop for GzipEncoder {
    fn drop(&mut self) {
        unsafe {
            vtmake_z_deflateEnd(self.s.as_mut_ptr());
        }
    }
}

/// Decode gzip or zlib data (windowBits 47: automatic header detection) and
/// append it to `out`. Returns false if the data is not valid.
pub fn gunzip(input: &[u8], out: &mut Vec<u8>) -> bool {
    let Ok(len) = c_uint::try_from(input.len()) else {
        return false;
    };
    let mut m = MaybeUninit::<ZStream>::zeroed();
    let s = m.as_mut_ptr();
    unsafe {
        if vtmake_z_inflateInit2_(s, 47, vtmake_z_zlibVersion(), std::mem::size_of::<ZStream>() as c_int) != Z_OK {
            return false;
        }
        (*s).next_in = input.as_ptr();
        (*s).avail_in = len;
        let ok = loop {
            out.reserve(65536);
            let start = out.len();
            let room = (out.capacity() - start).min(c_uint::MAX as usize);
            (*s).next_out = out.as_mut_ptr().add(start);
            (*s).avail_out = room as c_uint;
            let rc = vtmake_z_inflate(s, Z_NO_FLUSH);
            out.set_len(start + room - (*s).avail_out as usize);
            if rc == Z_STREAM_END {
                break true;
            }
            if rc != Z_OK {
                break false;
            }
        };
        vtmake_z_inflateEnd(s);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64, the same as `R` in the golden script.
    struct R(u64);
    impl R {
        fn next(&mut self) -> u32 {
            let mut s = self.0;
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            self.0 = s;
            (s >> 32) as u32
        }
    }

    fn make_input(r: &mut R, n: usize, kind: u32) -> Vec<u8> {
        let mut b = vec![0u8; n];
        match kind {
            0 => b.iter_mut().for_each(|x| *x = r.next() as u8),
            1 => b.iter_mut().for_each(|x| *x = (r.next() % 5) as u8),
            2 => {
                let v = r.next() as u8;
                b.iter_mut().for_each(|x| *x = v);
            }
            _ => {
                let mut i = 0;
                while i < n {
                    if i < 16 || r.next().is_multiple_of(3) {
                        b[i] = (r.next() % 32) as u8;
                        i += 1;
                        continue;
                    }
                    let d = 1 + (r.next() as usize) % i.min(40000);
                    let mut l = 3 + r.next() % 300;
                    while l > 0 && i < n {
                        b[i] = b[i - d];
                        i += 1;
                        l -= 1;
                    }
                }
            }
        }
        b
    }

    const SIZES: [usize; 30] = [
        0, 1, 2, 3, 4, 5, 17, 100, 258, 259, 1000, 4000, 8189, 8190, 8191, 8192, 8193, 9000, 20000, 32505,
        32768, 40000, 65536, 70000, 140000, 3, 1000, 50, 8000, 2,
    ];

    /// The golden value comes from stock zlib 1.3.1 (Python `zlib.compressobj(6,
    /// DEFLATED, 31, 8, 0)`) on the same inputs. The inputs use one encoder in
    /// sequence, so small streams after large streams (window slide) are tested.
    #[test]
    fn same_bytes_as_zlib_1_3_1() {
        let mut r = R(0x9E3779B97F4A7C15);
        let mut enc = GzipEncoder::new();
        let mut h: u64 = 0xcbf29ce484222325;
        let mut total = 0;
        for _ in 0..3 {
            for n in SIZES {
                let kind = r.next() % 4;
                let data = make_input(&mut r, n, kind);
                let mut out = Vec::new();
                enc.compress(&data, &mut out);
                for &x in &out {
                    h ^= x as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                total += out.len();
                let mut back = Vec::new();
                assert!(gunzip(&out, &mut back));
                assert_eq!(back, data);
            }
        }
        assert_eq!((h, total), (0xddcc02adb2059772, 387918));
    }

    #[test]
    fn appends_and_rejects_bad_data() {
        let mut enc = GzipEncoder::new();
        let mut out = b"xy".to_vec();
        enc.compress(b"hello hello hello", &mut out);
        assert_eq!(&out[..2], b"xy");
        assert_eq!(&out[2..12], &[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3]);
        let mut back = Vec::new();
        assert!(gunzip(&out[2..], &mut back));
        assert_eq!(back, b"hello hello hello");
        assert!(!gunzip(b"not gzip data", &mut Vec::new()));
        assert_eq!(version(), "1.3.1");
    }
}
