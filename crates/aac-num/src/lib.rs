//! One numeric layer for AI Account Center (Rust rewrite L3).
//!
//! Every number or date the Rust side writes in place of a JavaScript, `Intl` or Python reference goes through
//! this crate, so one rule is proved once instead of copied per caller. Each operation names its reference and the
//! domain it was proven on, and answers `None` outside that domain: the caller then declines and the reference
//! answers instead. No domain widens without a proof and a corpus generated from the reference runtime
//! (`tests/spec/num/`, checked by `crates/aac-num/tests/corpus.rs`).
//!
//! | Module | Reference |
//! |---|---|
//! | [`digits`] | the exact decimal value of a binary64 and its shortest round-trip digits (closest, ties even) |
//! | [`ecma`] | `Number#toString`, `JSON.stringify`, `Math.round`, `toFixed`, `toPrecision`, `reduce` sums, ISO dates |
//! | [`intl`] | `Intl.NumberFormat('en-US')` as AAC uses it, zone clock and day words, ASCII collation |
//! | [`py`] | `round(x, n)`, `repr` and `json.dumps` float text, `datetime.fromtimestamp` and `fromisoformat` |
//! | [`view`] | the bounded domain the M3 views (Status, Scoreboard, Home) were proven on |
//! | [`civil`] | proleptic Gregorian day arithmetic shared by every date form |
//!
//! `core` and `alloc` only: the crate builds for the server and for the Leptos page (wasm32) alike.
#![no_std]
extern crate alloc;
#[cfg(test)]
extern crate std;

mod big;
pub mod civil;
pub mod digits;
pub mod ecma;
pub mod intl;
pub mod py;
pub mod view;

/// `f64::trunc` without `std`: the integer part, keeping the sign (`-0.5` gives `-0`).
pub fn trunc(x: f64) -> f64 {
    let bits = x.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    if exponent < 0 {
        return f64::from_bits(bits & (1 << 63));
    }
    if exponent >= 52 {
        // Already integral, or NaN/infinite.
        return x;
    }
    f64::from_bits(bits & !((1u64 << (52 - exponent)) - 1))
}

/// `f64::floor` without `std` (`Math.floor`): exact, `-0` stays `-0`.
pub fn floor(x: f64) -> f64 {
    let t = trunc(x);
    // |t| < 2^52 whenever t != x, so t - 1 is exact.
    if t > x { t - 1.0 } else { t }
}

/// `|x|` without `std`.
pub fn abs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1 << 63))
}

/// Whether a finite `x` has no fraction.
pub fn is_integral(x: f64) -> bool {
    x.is_finite() && trunc(x) == x
}

#[cfg(test)]
mod tests {
    // Exact binary ties and neighbours are written out in full on purpose.
    #![allow(clippy::excessive_precision)]
    use super::*;

    #[test]
    fn trunc_and_floor_are_exact_and_keep_signed_zero() {
        for (x, t, f) in [
            (0.5, 0.0, 0.0),
            (-0.5, -0.0, -1.0),
            (1.5, 1.0, 1.0),
            (-1.5, -1.0, -2.0),
            (4503599627370495.5, 4503599627370495.0, 4503599627370495.0),
            (
                -4503599627370495.5,
                -4503599627370495.0,
                -4503599627370496.0,
            ),
            (9007199254740993.0, 9007199254740992.0, 9007199254740992.0),
            (-0.0, -0.0, -0.0),
            (5e-324, 0.0, 0.0),
            (-5e-324, -0.0, -1.0),
        ] {
            assert_eq!(trunc(x).to_bits(), f64::to_bits(t), "trunc {x}");
            assert_eq!(floor(x).to_bits(), f64::to_bits(f), "floor {x}");
        }
        assert!(trunc(f64::NAN).is_nan() && floor(f64::NAN).is_nan());
        assert_eq!(floor(f64::NEG_INFINITY), f64::NEG_INFINITY);
        assert!(is_integral(-0.0) && !is_integral(0.5) && !is_integral(f64::INFINITY));
    }
}
