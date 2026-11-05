//! Projection and spatial index (tippecanoe `projection.cpp`).

use crate::num::cvt_i64;
use std::f64::consts::PI;

/// `lonlat2tile` (EPSG:4326 to tile coordinates at `zoom`). With
/// `zoom = 32` the result is in world coordinates.
///
/// The y formula is `round(n * (1 - log(tan(lat) + 1/cos(lat)) / PI) / 2)`:
/// the division by 2 is inside `round`. `ln`, `tan` and `cos` call the
/// system libm (glibc), the same functions that tippecanoe calls.
#[inline]
pub fn lonlat2tile(lon: f64, lat: f64, zoom: u32) -> (i64, i64) {
    let mut lat = lat;
    let mut lon = lon;
    let mut bad_lon = false;

    // Put infinite and NaN coordinates off the edge of the Mercator plane.
    if !lat.is_finite() {
        lat = 89.9;
    }
    if !lon.is_finite() {
        lon = 720.0;
        bad_lon = true;
    }

    // `lat` is finite here, so `clamp` is the same as the two C++ tests.
    lat = lat.clamp(-89.9, 89.9);
    if lon < -360.0 && !bad_lon {
        lon = -360.0;
    }
    if lon > 360.0 && !bad_lon {
        lon = 360.0;
    }

    let lat_rad = lat * PI / 180.0;
    let n = (1u64 << zoom) as f64;

    let x = cvt_i64((n * ((lon + 180.0) / 360.0)).round());
    let y = cvt_i64((n * (1.0 - ((lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / PI)) / 2.0).round());
    (x, y)
}

/// `lonlat2tile(lon, lat, 32)`: EPSG:4326 to world coordinates.
#[inline]
pub fn lonlat_to_world(lon: f64, lat: f64) -> (i64, i64) {
    lonlat2tile(lon, lat, 32)
}

/// `epsg3857totile(ix, iy, 32)`: EPSG:3857 (web Mercator metres) to world
/// coordinates. Infinite and NaN values become 40000000 (off the plane).
#[inline]
pub fn epsg3857_to_world(ix: f64, iy: f64) -> (i64, i64) {
    let ix = if ix.is_finite() { ix } else { 40000000.0 };
    let iy = if iy.is_finite() { iy } else { 40000000.0 };
    let half = (1i64 << 31) as f64;
    let x = cvt_i64((ix * half / 6378137.0 / PI + half).round());
    let y = cvt_i64((((1i64 << 32) - 1) as f64 - (iy * half / 6378137.0 / PI + half)).round());
    (x, y)
}

/// `tile2lonlat`: tile coordinates at `zoom` to (lon, lat).
#[inline]
pub fn tile2lonlat(x: i64, y: i64, zoom: u32) -> (f64, f64) {
    let n = (1u64 << zoom) as f64;
    let lon = 360.0 * x as f64 / n - 180.0;
    let lat = (PI * (1.0 - 2.0 * y as f64 / n)).sinh().atan() * 180.0 / PI;
    (lon, lat)
}

#[inline(always)]
const fn spread_bits(v: u32) -> u64 {
    let mut x = v as u64;
    x = (x | (x << 16)) & 0x0000_FFFF_0000_FFFF;
    x = (x | (x << 8)) & 0x00FF_00FF_00FF_00FF;
    x = (x | (x << 4)) & 0x0F0F_0F0F_0F0F_0F0F;
    x = (x | (x << 2)) & 0x3333_3333_3333_3333;
    x = (x | (x << 1)) & 0x5555_5555_5555_5555;
    x
}

#[inline(always)]
const fn gather_bits(v: u64) -> u32 {
    let mut x = v & 0x5555_5555_5555_5555;
    x = (x | (x >> 1)) & 0x3333_3333_3333_3333;
    x = (x | (x >> 2)) & 0x0F0F_0F0F_0F0F_0F0F;
    x = (x | (x >> 4)) & 0x00FF_00FF_00FF_00FF;
    x = (x | (x >> 8)) & 0x0000_FFFF_0000_FFFF;
    x = (x | (x >> 16)) & 0x0000_0000_FFFF_FFFF;
    x as u32
}

/// `encode_quadkey`: interleave the bits of x and y, top bit first. The x
/// bit is the high bit of each pair.
///
/// The C++ code uses a loop over 32 bits. This version uses bit masks and
/// gives the same value.
#[inline]
pub const fn encode_quadkey(wx: u32, wy: u32) -> u64 {
    (spread_bits(wx) << 1) | spread_bits(wy)
}

/// `decode_quadkey`: the inverse of [`encode_quadkey`].
#[inline]
pub const fn decode_quadkey(index: u64) -> (u32, u32) {
    (gather_bits(index >> 1), gather_bits(index))
}

/// `bit_reverse` (tippecanoe `text.cpp`): reverse the 64 bits.
#[inline]
pub const fn bit_reverse(v: u64) -> u64 {
    v.reverse_bits()
}
