//! Tile gzip.
//!
//! [`Mode::Exact`] (the default) gives the same bytes as tippecanoe
//! `mvt.cpp:compress`: classic zlib 1.3.1, level 6, windowBits 31 (gzip
//! header), memLevel 8, default strategy. It uses the vendored zlib in `vtmake-zlib`
//! (faster, same output bytes).
//!
//! [`Mode::Fast`] uses libdeflate. The gzip bytes are different, but the tiles
//! decode to the same MVT bytes. Use it only when one full re-upload of all
//! tiles is acceptable.

use std::cell::RefCell;

/// libdeflate level for [`Mode::Fast`]. Level 6 gives tiles approximately 1.3%
/// smaller than zlib level 6 on tippecanoe tiles, at approximately 2 times the
/// speed of `Mode::Exact`.
pub const FAST_LEVEL: i32 = 6;

/// How to gzip a tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The same bytes as tippecanoe (zlib 1.3.1 level 6).
    #[default]
    Exact,
    /// libdeflate at [`FAST_LEVEL`]: other gzip bytes, the same decoded tile.
    Fast,
    /// No compression (`-pC`, `--no-tile-compression`): the tile as it is.
    None,
}

impl Mode {
    /// Parse a mode name: `exact` or `fast`.
    pub fn from_name(name: &str) -> Option<Mode> {
        match name {
            "exact" => Some(Mode::Exact),
            "fast" => Some(Mode::Fast),
            _ => None,
        }
    }

    /// gzip `input` and append the result to `out`.
    pub fn compress(self, input: &[u8], out: &mut Vec<u8>) {
        match self {
            Mode::Exact => ZLIB.with(|z| z.borrow_mut().compress(input, out)),
            Mode::Fast => LIBDEFLATE.with(|c| {
                let mut c = c.borrow_mut();
                let start = out.len();
                out.resize(start + c.gzip_compress_bound(input.len()), 0);
                let n = c.gzip_compress(input, &mut out[start..]).expect("libdeflate gzip failed");
                out.truncate(start + n);
            }),
            Mode::None => out.extend_from_slice(input),
        }
    }
}

thread_local! {
    // One stream for each thread, used again for each tile.
    static ZLIB: RefCell<vtmake_zlib::GzipEncoder> = RefCell::new(vtmake_zlib::GzipEncoder::new());
    static LIBDEFLATE: RefCell<libdeflater::Compressor> = RefCell::new(libdeflater::Compressor::new(
        libdeflater::CompressionLvl::new(FAST_LEVEL).expect("libdeflate level"),
    ));
}

/// gzip `input` with [`Mode::Exact`] and append it to `out`.
pub fn gzip(input: &[u8], out: &mut Vec<u8>) {
    Mode::Exact.compress(input, out)
}

/// Decode gzip data and append it to `out`. Returns false if the data is not
/// valid.
pub fn gunzip(input: &[u8], out: &mut Vec<u8>) -> bool {
    vtmake_zlib::gunzip(input, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_modes_decode_to_the_input() {
        let input: Vec<u8> = (0..20000u32).map(|i| (i * i % 251) as u8).collect();
        for mode in [Mode::Exact, Mode::Fast] {
            let mut gz = b"x".to_vec();
            mode.compress(&input, &mut gz);
            assert_eq!(&gz[1..4], &[0x1f, 0x8b, 8]);
            let mut back = Vec::new();
            assert!(gunzip(&gz[1..], &mut back));
            assert_eq!(back, input);
        }
        assert_eq!(Mode::from_name("fast"), Some(Mode::Fast));
        assert_eq!(Mode::from_name("exact"), Some(Mode::Exact));
        assert_eq!(Mode::from_name("other"), None);
    }
}
