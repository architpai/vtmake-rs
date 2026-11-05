//! Dynamic tile limits, ported from felt/tippecanoe tile.cpp.
//! Copyright (c) 2022, Protomaps LLC; Copyright (c) 2014, Mapbox Inc.
//! BSD 2-Clause license: LICENSE-tippecanoe.md.

#[derive(Clone, Copy, PartialEq)]
pub struct Threshold {
    pub gap: u64,
    pub sequence: u64,
    pub extent: u64,
    pub attribute: u64,
    pub gamma: f64,
}

impl Threshold {
    pub fn merge(&mut self, other: Self) {
        self.gap = self.gap.max(other.gap);
        self.attribute = self.attribute.max(other.attribute);
        self.extent = self.extent.max(other.extent);
        self.sequence = self.sequence.max(other.sequence);
        self.gamma = self.gamma.max(other.gamma);
    }
}

pub struct Retry {
    pub threshold: Threshold,
    fraction: f64,
    pub samples: Vec<u64>,
    increment: usize,
    pub dropped: u64,
    pub failed: bool,
}

impl Retry {
    pub fn new(threshold: Threshold) -> Self {
        Self {
            threshold,
            fraction: 1.,
            samples: Vec::new(),
            increment: 1,
            dropped: 0,
            failed: false,
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.increment = 1;
        self.dropped = 0;
    }

    pub fn sample(&mut self, value: u64, seq: usize) {
        if seq.is_multiple_of(self.increment) {
            self.samples.push(value);
            if self.samples.len() > 100000 {
                self.samples = self.samples.iter().step_by(2).copied().collect();
                self.increment *= 2;
            }
        }
    }

    pub fn tighten(&mut self, o: &crate::cli::Options, ratio: f64, features: bool, multiple: bool) -> bool {
        if o.additional[b'G' as usize] && self.threshold.gamma < 10. {
            self.threshold.gamma = if self.threshold.gamma < 1. {
                1.
            } else {
                self.threshold.gamma * 1.25
            };
            if !o.quiet { eprintln!("Going to try gamma of {:.3} to make it fit", self.threshold.gamma); }
            return true;
        }
        let densest = o.additional[b's' as usize] || o.additional[b'C' as usize] || o.additional[b'S' as usize];
        let attribute = o.additional[b'A' as usize] && !densest && !(o.additional[b'n' as usize] || o.additional[b'N' as usize]);
        let smallest = o.additional[b'n' as usize] || o.additional[b'N' as usize];
        let fraction = o.additional[b'd' as usize] || o.additional[b'D' as usize] || o.prevent[b'd' as usize];
        if (!densest && !fraction && !smallest && !attribute) || (!densest && !smallest && !attribute && !multiple) {
            return false;
        }
        if self.samples.is_empty() {
            eprintln!("Can't increase {} threshold further", if densest { "feature gap" } else if smallest { "feature area" } else if attribute { "attribute" } else { "feature count" });
            self.failed = true;
            return false;
        }
        let proposed = self.fraction
            * ratio
            * if densest {
                0.80
            } else if smallest || attribute {
                0.75
            } else if features {
                0.95
            } else {
                0.75
            };
        if proposed > 0.80 && !o.quiet {
            eprintln!("Need to drop features, but calculated that we should keep {:.1}% of features", proposed * 100.);
        }
        self.fraction = proposed.min(0.80);
        self.samples.sort_unstable();
        let old = if densest {
            self.threshold.gap
        } else if attribute {
            self.threshold.attribute
        } else if smallest {
            self.threshold.extent
        } else {
            self.threshold.sequence
        };
        let ix = (self.samples.len() - 1) as f64 * (1. - self.fraction);
        let mut ix = if attribute {
            ix.ceil() as usize
        } else {
            ix as usize
        };
        while ix + 1 < self.samples.len() && self.samples[ix] <= old {
            ix += 1;
        }
        if self.samples[ix] <= old {
            eprintln!("Can't increase {} threshold further", if densest { "feature gap" } else if smallest { "feature area" } else if attribute { "attribute" } else { "feature count" });
            self.failed = true;
            return false;
        }
        if densest {
            self.threshold.gap = self.samples[ix];
        } else if attribute {
            self.threshold.attribute = self.samples[ix];
        } else if smallest {
            self.threshold.extent = self.samples[ix];
        } else {
            self.threshold.sequence = self.samples[ix];
        }
        if !o.quiet {
            if attribute {
                let bits = if self.threshold.attribute >> 63 != 0 { self.threshold.attribute ^ (1u64 << 63) } else { !self.threshold.attribute };
                let value = f64::from_bits(bits) * if o.drop_by_attribute_descending { -1. } else { 1. };
                eprintln!("Trying to keep features with '{}' {} {:.6} to make it fit", o.drop_by_attribute.as_deref().unwrap_or(""), if o.drop_by_attribute_descending { "<=" } else { ">=" }, value);
                return true;
            }
            let description = if densest { "the sparsest " } else if smallest { "the biggest " } else { "" };
            eprintln!("Going to try keeping {description}{:.2}% of the features to make it fit", self.fraction * 100.);
        }
        true
    }
}

pub fn sequence(index: u64, minzoom: i8) -> u64 {
    !(((minzoom.clamp(0, 31) as u64) << 59) | (index.reverse_bits() & !(31u64 << 59)))
}

/// An integer key with the same order as the finite numeric value.
pub fn attribute_key(value: f64) -> u64 {
    let bits = value.to_bits();
    if bits >> 63 != 0 {
        !bits
    } else {
        bits ^ (1u64 << 63)
    }
}
