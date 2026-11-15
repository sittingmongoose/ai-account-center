//! The bounded number domain the M3 views were proven on: Status, Scoreboard and Home (their goldens, the
//! 120,992 forms of `tests/spec/read-numbers.json`, the Home spec table and fuzz).
//!
//! The [`crate::ecma`] and [`crate::intl`] operations are exact well beyond it (the L3 corpus covers all of
//! binary64), but every view's decline counts were recorded against this domain, so it is the admitted one.
//! Widening it is a separate change: it serves values that decline today and moves those counts.
use crate::{abs, ecma, is_integral};
use alloc::string::String;

/// The smallest nonzero magnitude admitted.
pub const LOW: f64 = 1e-22;
/// The largest magnitude admitted.
pub const HIGH: f64 = 1e12;

/// Zero, or a magnitude in `[1e-22, 1e12]`.
pub fn in_domain(x: f64) -> bool {
    x == 0.0 || (LOW..=HIGH).contains(&abs(x))
}

/// Whether [`string`] admits `x`: in the domain, and an integer or the nearest double to its own 10-significant-digit
/// rounding (`Number(x.toPrecision(10)) === x`).
pub fn string_admitted(x: f64) -> bool {
    in_domain(x) && (is_integral(x) || precision(x, 10) == Some(x))
}

/// `String(x)` on the admitted domain ([`string_admitted`]).
pub fn string(x: f64) -> Option<String> {
    string_admitted(x).then(|| ecma::to_string(x))
}

/// `Number(x.toPrecision(digits))` for `digits` 1 to 10 on the domain; zero (either sign) is `+0` for any `digits`.
pub fn precision(x: f64, digits: u32) -> Option<f64> {
    if x == 0.0 {
        return Some(0.0);
    }
    if !in_domain(x) || !(1..=10).contains(&digits) {
        return None;
    }
    ecma::precision_value(x, digits)
}

/// `x.toFixed(places)` for `places` 0 to 6 on the domain.
pub fn fixed(x: f64, places: u32) -> Option<String> {
    if !in_domain(x) || places > 6 {
        return None;
    }
    ecma::to_fixed(x, places)
}

/// `String(Math.round(x))` while the rounded value's magnitude is at most 1e12.
pub fn round_word(x: f64) -> Option<String> {
    let rounded = ecma::math_round(x);
    (rounded.is_finite() && abs(rounded) <= HIGH).then(|| ecma::to_string(rounded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_domain_is_the_one_the_views_were_proven_on() {
        for x in [
            1e21,
            -1e21,
            2251799813685247.2,
            0.12345678901234567,
            f64::NAN,
            f64::INFINITY,
            1e-23,
            33.333333333333336,
            1e12 + 0.5,
        ] {
            assert_eq!(string(x), None, "{x}");
        }
        for (x, text) in [
            (-0.0, "0"),
            (1e12, "1000000000000"),
            (0.25, "0.25"),
            (1e-7, "1e-7"),
            (1e-22, "1e-22"),
        ] {
            assert_eq!(string(x).as_deref(), Some(text), "{x}");
        }
        assert_eq!(precision(-0.0, 0), Some(0.0));
        assert_eq!(precision(5.0, 11), None);
        assert_eq!(fixed(1.25, 7), None);
        assert_eq!(fixed(-0.0, 1).as_deref(), Some("0.0"));
        assert_eq!(round_word(-0.49999999999999994).as_deref(), Some("0"));
        assert_eq!(round_word(1e30), None);
    }
}
