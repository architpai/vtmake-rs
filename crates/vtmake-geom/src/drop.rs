//! The default point drop rate (`-r2.5`, basezoom = maxzoom):
//! `main.cpp:prep_drop_states`, `calc_feature_minzoom`, and the test in
//! `tile.cpp:next_feature`. See spec 5.2.

use crate::projection::bit_reverse;
use vtmake_core::GeomType;

/// The drop state of all zooms (`struct drop_state` array). Feed it every
/// feature in the global sort order `(index, seq)` with
/// [`DropState::feature_minzoom`].
///
/// Only `interval` and `seq` have an effect for this command line
/// (`gamma = 0`, no `--preserve-point-density-threshold`).
#[derive(Clone, Debug)]
pub struct DropState {
    interval: Vec<f64>,
    seq: Vec<f64>,
}

impl DropState {
    /// `prep_drop_states(ds, maxzoom, basezoom, droprate)`.
    pub fn new(maxzoom: u32, basezoom: u32, droprate: f64) -> Self {
        let n = maxzoom as usize + 1;
        let mut interval = vec![0.0; n];
        for (i, v) in interval.iter_mut().enumerate() {
            if (i as i64) < basezoom as i64 {
                *v = (droprate.ln() * (basezoom as i64 - i as i64) as f64).exp();
            }
        }
        DropState {
            interval,
            seq: vec![0.0; n],
        }
    }

    /// `calc_feature_minzoom` with `gamma = 0`: the first zoom where the
    /// rate keeps the feature. Only points change the state. Lines and
    /// polygons give 0 (`-al`/`-ap` are off).
    ///
    /// Call this for every feature (also points with `tippecanoe.minzoom`)
    /// in the global sort order.
    #[inline]
    pub fn feature_minzoom(&mut self, t: GeomType) -> i32 {
        if t != GeomType::Point {
            return 0;
        }
        for s in self.seq.iter_mut().rev() {
            *s += 1.0;
        }
        for i in (0..self.seq.len()).rev() {
            if self.seq[i] < 0.0 {
                return i as i32 + 1;
            }
            self.seq[i] -= self.interval[i];
        }
        0
    }
}

/// The rate test of `next_feature` for a feature without
/// `tippecanoe.minzoom`: `z >= feature_minzoom - bit_reverse(index >> 2) / 2^64`
/// in `double`.
///
/// Usually this is `z >= feature_minzoom`. It is also true at
/// `z = feature_minzoom - 1` if the fraction rounds to 1.0 (bits 2..55 of
/// `index` all set).
#[inline]
pub fn rate_keeps(z: u32, feature_minzoom: i32, index: u64) -> bool {
    let fz = feature_minzoom as f64 - bit_reverse(index >> 2) as f64 / 18_446_744_073_709_551_616.0;
    z as f64 >= fz
}

/// The per-tile, per-layer part of the rate drop (`multiplier_state` and
/// `find_feature_to_accumulate_onto` in `tile.cpp`), for
/// `--retain-points-multiplier 1`. Make one for each layer of each tile.
///
/// A feature without `tippecanoe.minzoom` that fails [`rate_keeps`] is
/// dropped only if both are true:
/// - it is not the first feature without `tippecanoe.minzoom` of the layer
///   in this tile (that one is "kept" by rule), and
/// - the layer already has an earlier kept feature of the same geometry
///   type in this tile.
///
/// The "first" feature counts after the clip and the zoom checks, also
/// when its clipped geometry is empty and it is removed later (the C++
/// marks it in `next_feature`, before the tiny polygon and "too small"
/// steps).
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerRateDrop {
    seen_unset_minzoom: bool,
}

impl LayerRateDrop {
    /// Call for each feature without `tippecanoe.minzoom` of this layer, in
    /// tile order, after the clip and after the `-Z`/minzoom/maxzoom
    /// checks. Returns true if the feature is marked "kept" (it passes the
    /// rate or it is the first one). If it returns false, drop the feature
    /// if the layer already has a kept feature of the same type.
    #[inline]
    pub fn mark(&mut self, z: u32, feature_minzoom: i32, index: u64) -> bool {
        let first = !self.seen_unset_minzoom;
        self.seen_unset_minzoom = true;
        first || rate_keeps(z, feature_minzoom, index)
    }
}
