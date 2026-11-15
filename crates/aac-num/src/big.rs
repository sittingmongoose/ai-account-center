//! Unsigned integers of any size, only as much as the exact decimal value of a binary64 needs.
use alloc::vec::Vec;

/// Little-endian base-2^32 limbs with no high zero limb (zero is empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Big(Vec<u32>);

impl Big {
    pub(crate) fn from_u64(n: u64) -> Big {
        let mut big = Big(alloc::vec![n as u32, (n >> 32) as u32]);
        big.trim();
        big
    }

    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    fn mul_small(&mut self, k: u32) {
        if k == 0 {
            self.0.clear();
            return;
        }
        let mut carry = 0u64;
        for limb in &mut self.0 {
            let product = u64::from(*limb) * u64::from(k) + carry;
            *limb = product as u32;
            carry = product >> 32;
        }
        if carry > 0 {
            self.0.push(carry as u32);
        }
    }

    /// Multiplies by 5^n.
    pub(crate) fn mul_pow5(&mut self, mut n: u32) {
        const FIVE_13: u32 = 1_220_703_125;
        while n >= 13 {
            self.mul_small(FIVE_13);
            n -= 13;
        }
        self.mul_small(5u32.pow(n));
    }

    /// Multiplies by 2^bits.
    pub(crate) fn shl(&mut self, bits: u32) {
        if self.is_zero() {
            return;
        }
        let rest = bits % 32;
        if rest > 0 {
            let mut carry = 0u32;
            for limb in &mut self.0 {
                let next = *limb >> (32 - rest);
                *limb = (*limb << rest) | carry;
                carry = next;
            }
            if carry > 0 {
                self.0.push(carry);
            }
        }
        let limbs = (bits / 32) as usize;
        if limbs > 0 {
            self.0.splice(0..0, core::iter::repeat_n(0, limbs));
        }
    }

    /// Divides by `d` in place and answers the remainder.
    fn div_small(&mut self, d: u32) -> u32 {
        let mut rem = 0u64;
        for limb in self.0.iter_mut().rev() {
            let current = (rem << 32) | u64::from(*limb);
            *limb = (current / u64::from(d)) as u32;
            rem = current % u64::from(d);
        }
        self.trim();
        rem as u32
    }

    /// The decimal digits as ASCII, most significant first, without leading zeros (zero is empty).
    pub(crate) fn decimal(mut self) -> Vec<u8> {
        let mut chunks = Vec::new();
        while !self.is_zero() {
            chunks.push(self.div_small(1_000_000_000));
        }
        let mut out = Vec::with_capacity(chunks.len() * 9);
        for chunk in chunks.iter().rev() {
            let mut nine = [b'0'; 9];
            let mut n = *chunk;
            for slot in nine.iter_mut().rev() {
                *slot = b'0' + (n % 10) as u8;
                n /= 10;
            }
            if out.is_empty() {
                let first = nine.iter().position(|d| *d != b'0').unwrap_or(9);
                out.extend_from_slice(&nine[first..]);
            } else {
                out.extend_from_slice(&nine);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Big;

    #[test]
    fn powers_and_shifts_print_exactly() {
        let mut n = Big::from_u64(1);
        n.mul_pow5(27);
        assert_eq!(n.decimal(), b"7450580596923828125".to_vec());
        let mut n = Big::from_u64(3);
        n.shl(100);
        assert_eq!(n.decimal(), b"3802951800684688204490109616128".to_vec());
        let mut n = Big::from_u64(u64::MAX);
        n.shl(37);
        n.mul_pow5(3);
        assert_eq!(n.decimal(), b"316912650057057350356995932160000".to_vec());
        assert!(Big::from_u64(0).decimal().is_empty());
        let mut n = Big::from_u64(1_000_000_000);
        n.mul_pow5(0);
        assert_eq!(n.decimal(), b"1000000000".to_vec());
    }
}
