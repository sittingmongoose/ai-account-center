//! The exact decimal value of a binary64, rounding of that value, and its shortest round-trip digits.
//!
//! Every finite binary64 `m × 2^e` is a finite decimal (`m × 5^-e × 10^e` when `e < 0`), so every rounding here works
//! on the exact digits and no intermediate double can round across a tie. Reading a decimal back uses `core`'s
//! correctly rounded parser (round half even), the same reading as `Number(text)` and CPython's `float(text)`.
use crate::big::Big;
use alloc::{format, string::String, vec::Vec};
use core::cmp::Ordering;

/// How an exact tie (a remainder of exactly half a unit) rounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tie {
    /// To the even neighbour: CPython `round`, and the choice between two equally close shortest digit strings.
    Even,
    /// To the larger magnitude: ECMAScript `toFixed` and `toPrecision`, `Intl` `halfExpand`.
    Away,
}

/// `|x| = mantissa × 2^exponent` for a finite `x` (subnormals have exponent -1074).
pub fn parts(x: f64) -> (u64, i32) {
    let bits = x.to_bits() & !(1 << 63);
    let raw = (bits >> 52) as i32;
    let fraction = bits & ((1 << 52) - 1);
    if raw == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), raw - 1075)
    }
}

/// A non-negative decimal `digits × 10^exp`. `digits` are ASCII, most significant first, with no leading or trailing
/// zero, so every value has one spelling; zero has no digits (and `exp` 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decimal {
    /// The significant digits, ASCII `0`-`9`.
    pub digits: Vec<u8>,
    /// The power of ten of the last digit.
    pub exp: i32,
}

impl Decimal {
    /// Zero.
    pub fn zero() -> Decimal {
        Decimal {
            digits: Vec::new(),
            exp: 0,
        }
    }

    /// `digits × 10^exp`, normalized (leading and trailing zeros removed).
    pub fn new(mut digits: Vec<u8>, mut exp: i32) -> Decimal {
        let lead = digits
            .iter()
            .position(|d| *d != b'0')
            .unwrap_or(digits.len());
        digits.drain(..lead);
        while digits.last() == Some(&b'0') {
            digits.pop();
            exp += 1;
        }
        if digits.is_empty() {
            exp = 0;
        }
        Decimal { digits, exp }
    }

    /// The exact value of `|x|`, or `None` for NaN and the infinities.
    pub fn exact(x: f64) -> Option<Decimal> {
        if !x.is_finite() {
            return None;
        }
        let (mantissa, exponent) = parts(x);
        let mut n = Big::from_u64(mantissa);
        let exp = if exponent >= 0 {
            n.shl(exponent as u32);
            0
        } else {
            n.mul_pow5(exponent.unsigned_abs());
            exponent
        };
        Some(Decimal::new(n.decimal(), exp))
    }

    /// Whether this is zero.
    pub fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// The power of ten of the leading digit: the value lies in `[10^lead, 10^(lead+1))`. Zero answers 0.
    pub fn lead(&self) -> i32 {
        if self.is_zero() {
            0
        } else {
            self.digits.len() as i32 - 1 + self.exp
        }
    }

    /// The digits as text (`"0"` for zero), without the exponent.
    pub fn text(&self) -> &str {
        if self.is_zero() {
            "0"
        } else {
            core::str::from_utf8(&self.digits).unwrap_or("0")
        }
    }

    /// The nearest binary64 to this decimal (ties even), as `Number(text)` and `float(text)` read it.
    pub fn to_f64(&self) -> f64 {
        if self.is_zero() {
            return 0.0;
        }
        format!("{}e{}", self.text(), self.exp)
            .parse::<f64>()
            .unwrap_or(f64::NAN)
    }

    /// The integer part of `value / 10^unit` as ASCII digits (possibly empty, possibly with trailing zeros), and how
    /// the remainder compares with half a unit (`None` when the remainder is zero).
    fn split(&self, unit: i32) -> (Vec<u8>, Option<Ordering>) {
        if self.is_zero() {
            return (Vec::new(), None);
        }
        if unit <= self.exp {
            let mut kept = self.digits.clone();
            kept.extend(core::iter::repeat_n(b'0', (self.exp - unit) as usize));
            return (kept, None);
        }
        let cut = (unit - self.exp) as usize;
        let len = self.digits.len();
        if cut > len {
            // The first digit below the unit is a zero: less than half.
            return (Vec::new(), Some(Ordering::Less));
        }
        let (kept, dropped) = self.digits.split_at(len - cut);
        // `dropped` ends in a nonzero digit (no trailing zeros), so after a leading 5 any digit means more than half.
        let half = match dropped[0].cmp(&b'5') {
            Ordering::Equal if dropped.len() > 1 => Ordering::Greater,
            other => other,
        };
        (kept.to_vec(), Some(half))
    }

    /// `value / 10^unit` rounded to an integer, ties by `tie`, as a decimal (that integer `× 10^unit`).
    pub fn round_at(&self, unit: i32, tie: Tie) -> Decimal {
        let (mut kept, half) = self.split(unit);
        let odd = kept.last().is_some_and(|d| (d - b'0') % 2 == 1);
        let up = match half {
            Some(Ordering::Greater) => true,
            Some(Ordering::Equal) => tie == Tie::Away || odd,
            _ => false,
        };
        if up {
            increment(&mut kept);
        }
        Decimal::new(kept, unit)
    }
}

/// Adds one to an ASCII digit string (an empty string is zero).
fn increment(digits: &mut Vec<u8>) {
    for d in digits.iter_mut().rev() {
        if *d == b'9' {
            *d = b'0';
        } else {
            *d += 1;
            return;
        }
    }
    digits.insert(0, b'1');
}

/// Subtracts one from a nonzero ASCII digit string.
fn decrement(digits: &mut [u8]) {
    for d in digits.iter_mut().rev() {
        if *d == b'0' {
            *d = b'9';
        } else {
            *d -= 1;
            return;
        }
    }
}

/// The shortest digits that read back as `|x|`; of several such strings, the one closest to `|x|`; of two equally
/// close, the even one. This is the choice of ECMAScript `Number::toString` (ECMA-262 6.1.6.1.20) and of CPython's
/// `repr` (`_Py_dg_dtoa` mode 0). Zero, NaN and the infinities have no digits (`Decimal::zero()`).
///
/// Rust's own shortest formatter (Grisu with a Dragon4 fallback) finds the minimal length `k`, and the closest
/// `k`-digit string, but breaks an exact tie between two equally close strings upward: 2251799813685247.25 is
/// written `2251799813685247.3`, where JavaScript and Python write `.2`. When a neighbouring `k`-digit string also
/// reads back, the exact value decides.
pub fn shortest(x: f64) -> Decimal {
    if !x.is_finite() || x == 0.0 {
        return Decimal::zero();
    }
    let x = crate::abs(x);
    let text = format!("{x:e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((text.as_str(), "0"));
    let digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    let unit = exponent.parse::<i32>().unwrap_or(0) - (digits.len() as i32 - 1);
    let reads = |d: &Decimal| d.to_f64() == x;
    let mut below = digits.clone();
    decrement(&mut below);
    let mut above = digits.clone();
    increment(&mut above);
    let rust = Decimal::new(digits, unit);
    if !reads(&Decimal::new(below, unit)) && !reads(&Decimal::new(above, unit)) {
        return rust;
    }
    let Some(exact) = Decimal::exact(x) else {
        return rust;
    };
    let (floor, half) = exact.split(unit);
    let Some(half) = half else {
        // |x| itself has no digit below the unit.
        return Decimal::new(floor, unit);
    };
    let odd = floor.last().is_some_and(|d| (d - b'0') % 2 == 1);
    let mut up = floor.clone();
    increment(&mut up);
    let (lo, hi) = (Decimal::new(floor, unit), Decimal::new(up, unit));
    match (reads(&lo), reads(&hi)) {
        (true, true) => match half {
            Ordering::Less => lo,
            Ordering::Greater => hi,
            Ordering::Equal if odd => hi,
            Ordering::Equal => lo,
        },
        (true, false) => lo,
        (false, true) => hi,
        // Unreachable: Rust's string lies at or beyond lo or hi, and every string between it and x reads back.
        (false, false) => rust,
    }
}

/// The exact decimal expansion of `|x|` as text, `"0"` for zero (for tests and diagnostics).
pub fn exact_text(x: f64) -> Option<String> {
    let d = Decimal::exact(x)?;
    if d.is_zero() {
        return Some("0".into());
    }
    Some(format!("{}e{}", d.text(), d.exp))
}

#[cfg(test)]
mod tests {
    // Exact binary ties and neighbours are written out in full on purpose.
    #![allow(clippy::excessive_precision)]
    use super::*;

    #[test]
    fn exact_values_and_rounding() {
        let d = Decimal::exact(0.1).unwrap_or_else(Decimal::zero);
        assert_eq!(
            d.text(),
            "1000000000000000055511151231257827021181583404541015625"
        );
        assert_eq!(d.exp, -55);
        assert_eq!(d.lead(), -1);
        // 2.675 is below the tie; 0.125 is one.
        let two = Decimal::exact(2.675).unwrap_or_else(Decimal::zero);
        assert_eq!(two.round_at(-2, Tie::Away).to_f64(), 2.67);
        let eighth = Decimal::exact(0.125).unwrap_or_else(Decimal::zero);
        assert_eq!(eighth.round_at(-2, Tie::Even).text(), "12");
        assert_eq!(eighth.round_at(-2, Tie::Away).text(), "13");
        assert_eq!(eighth.round_at(0, Tie::Away), Decimal::zero());
        let nines = Decimal::exact(9.995).unwrap_or_else(Decimal::zero);
        assert_eq!(nines.round_at(-2, Tie::Away).to_f64(), 9.99);
        let ninety = Decimal::exact(99.5).unwrap_or_else(Decimal::zero);
        assert_eq!(
            ninety.round_at(0, Tie::Even),
            Decimal::new(b"1".to_vec(), 2)
        );
        assert_eq!(exact_text(5e-324).map(|t| t.len()), Some(757));
    }

    #[test]
    fn shortest_digits_break_ties_to_even() {
        for (x, digits, exp) in [
            (2251799813685247.25, "22517998136852472", -1),
            (140737488355327.125, "14073748835532712", -2),
            (0.1, "1", -1),
            (1e23, "1", 23),
            (5e-324, "5", -324),
            (100.0, "1", 2),
            (33.333333333333336, "33333333333333336", -15),
        ] {
            let d = shortest(x);
            assert_eq!((d.text(), d.exp), (digits, exp), "{x}");
            assert_eq!(d.to_f64(), x);
        }
    }
}
