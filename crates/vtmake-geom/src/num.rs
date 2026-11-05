//! Number conversions that copy the behaviour of tippecanoe's C++ code on
//! x86-64 Linux (gcc).

/// C `(long long) d` on x86-64 (`cvttsd2si`): truncate toward zero.
/// NaN and values out of range give `i64::MIN` (the "integer indefinite"
/// value). Rust `as` saturates, so it is not the same for these values.
#[inline(always)]
pub fn cvt_i64(d: f64) -> i64 {
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&d) {
        d as i64
    } else {
        i64::MIN
    }
}

/// C `(int) d` on x86-64 (32-bit `cvttsd2si`): truncate toward zero. NaN
/// and values out of range give `i32::MIN`.
#[inline(always)]
pub fn cvt_i32(d: f64) -> i32 {
    if d > -2_147_483_649.0 && d < 2_147_483_648.0 {
        d as i32
    } else {
        i32::MIN
    }
}

/// C `(unsigned long long) d` as gcc makes it on x86-64 without AVX-512.
/// A value below 2^63 (also a negative value and NaN) goes through
/// `cvttsd2si` and is then read as unsigned.
#[inline(always)]
pub fn cvt_u64(d: f64) -> u64 {
    const TWO63: f64 = 9_223_372_036_854_775_808.0;
    if d >= TWO63 {
        (cvt_i64(d - TWO63) as u64) ^ (1u64 << 63)
    } else {
        cvt_i64(d) as u64
    }
}

/// Store a value in tippecanoe's `draw` bit field (`long long x : 40`):
/// keep the low 40 bits and extend the sign.
#[inline(always)]
pub const fn wrap40(v: i64) -> i64 {
    (v << 24) >> 24
}

/// `std::round((double) x / (1LL << shift))` in integer arithmetic.
///
/// The result is the same as the C++ expression when `|x| < 2^53` (all
/// tippecanoe coordinates are 40-bit values). Division by a power of two is
/// exact in `double`, and `round` rounds half away from zero.
#[inline(always)]
pub const fn round_shift(x: i64, shift: u32) -> i64 {
    if shift == 0 {
        x
    } else {
        let half = 1i64 << (shift - 1);
        if x >= 0 {
            (x + half) >> shift
        } else {
            -((half - x) >> shift)
        }
    }
}

/// The same value as [`round_shift`], made with `f64` like the C++ code.
/// Tests use it to check [`round_shift`].
#[inline]
pub fn round_shift_f64(x: i64, shift: u32) -> i64 {
    cvt_i64((x as f64 / (1i64 << shift) as f64).round())
}
