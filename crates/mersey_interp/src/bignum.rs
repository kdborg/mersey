// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! Arbitrary-precision integers and decimals (spec §3.7).
//!
//! `BigInt`: sign + little-endian u32 limbs. Schoolbook multiplication and
//! shift-subtract division — correctness first; Karatsuba is a Phase 4+
//! concern (see engine.md).
//! `BigDec`: BigInt coefficient + decimal scale (digits right of the
//! point), the java.math.BigDecimal model. `+ - *` are exact; `/` succeeds
//! only when exact within a bounded scale extension (34 digits), otherwise
//! the operation reports inexactness (spec: division needs a rounding
//! context; the `divide(…)` API arrives with the standard library).

use std::cmp::Ordering;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BigInt {
    /// true = negative; zero is always non-negative with empty limbs
    neg: bool,
    /// little-endian base-2^32 limbs, no trailing zeros
    mag: Vec<u32>,
}

impl BigInt {
    pub fn zero() -> BigInt {
        BigInt {
            neg: false,
            mag: vec![],
        }
    }

    pub fn from_i64(v: i64) -> BigInt {
        let neg = v < 0;
        let mut u = v.unsigned_abs();
        let mut mag = vec![];
        while u > 0 {
            mag.push((u & 0xFFFF_FFFF) as u32);
            u >>= 32;
        }
        BigInt {
            neg: neg && !mag.is_empty(),
            mag,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    /// Parse decimal/hex/octal/binary digits (no sign, separators removed).
    pub fn parse(body: &str, radix: u32) -> Option<BigInt> {
        let mut n = BigInt::zero();
        for c in body.chars() {
            let d = c.to_digit(radix)?;
            n = n.mul_small(radix);
            n = n.add_small(d);
        }
        Some(n)
    }

    fn trim(mut self) -> BigInt {
        while self.mag.last() == Some(&0) {
            self.mag.pop();
        }
        if self.mag.is_empty() {
            self.neg = false;
        }
        self
    }

    fn mul_small(&self, m: u32) -> BigInt {
        let mut out = Vec::with_capacity(self.mag.len() + 1);
        let mut carry: u64 = 0;
        for &l in &self.mag {
            let v = l as u64 * m as u64 + carry;
            out.push((v & 0xFFFF_FFFF) as u32);
            carry = v >> 32;
        }
        if carry > 0 {
            out.push(carry as u32);
        }
        BigInt {
            neg: self.neg,
            mag: out,
        }
        .trim()
    }

    fn add_small(&self, a: u32) -> BigInt {
        debug_assert!(!self.neg);
        let mut out = self.mag.clone();
        let mut carry = a as u64;
        let mut i = 0;
        while carry > 0 {
            if i == out.len() {
                out.push(0);
            }
            let v = out[i] as u64 + carry;
            out[i] = (v & 0xFFFF_FFFF) as u32;
            carry = v >> 32;
            i += 1;
        }
        BigInt {
            neg: false,
            mag: out,
        }
    }

    /// Divide by a small divisor, returning (quotient, remainder).
    fn divmod_small(&self, d: u32) -> (BigInt, u32) {
        let mut out = vec![0u32; self.mag.len()];
        let mut rem: u64 = 0;
        for i in (0..self.mag.len()).rev() {
            let cur = (rem << 32) | self.mag[i] as u64;
            out[i] = (cur / d as u64) as u32;
            rem = cur % d as u64;
        }
        (
            BigInt {
                neg: self.neg,
                mag: out,
            }
            .trim(),
            rem as u32,
        )
    }

    fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
        if a.len() != b.len() {
            return a.len().cmp(&b.len());
        }
        for i in (0..a.len()).rev() {
            if a[i] != b[i] {
                return a[i].cmp(&b[i]);
            }
        }
        Ordering::Equal
    }

    fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
        let mut carry: u64 = 0;
        for i in 0..a.len().max(b.len()) {
            let v = *a.get(i).unwrap_or(&0) as u64 + *b.get(i).unwrap_or(&0) as u64 + carry;
            out.push((v & 0xFFFF_FFFF) as u32);
            carry = v >> 32;
        }
        if carry > 0 {
            out.push(carry as u32);
        }
        out
    }

    /// a - b, requires |a| >= |b|
    fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len());
        let mut borrow: i64 = 0;
        for (i, &ai) in a.iter().enumerate() {
            let mut v = ai as i64 - *b.get(i).unwrap_or(&0) as i64 - borrow;
            if v < 0 {
                v += 1 << 32;
                borrow = 1;
            } else {
                borrow = 0;
            }
            out.push(v as u32);
        }
        out
    }

    pub fn add(&self, other: &BigInt) -> BigInt {
        if self.neg == other.neg {
            BigInt {
                neg: self.neg,
                mag: Self::add_mag(&self.mag, &other.mag),
            }
            .trim()
        } else {
            match Self::cmp_mag(&self.mag, &other.mag) {
                Ordering::Equal => BigInt::zero(),
                Ordering::Greater => BigInt {
                    neg: self.neg,
                    mag: Self::sub_mag(&self.mag, &other.mag),
                }
                .trim(),
                Ordering::Less => BigInt {
                    neg: other.neg,
                    mag: Self::sub_mag(&other.mag, &self.mag),
                }
                .trim(),
            }
        }
    }

    pub fn negate(&self) -> BigInt {
        if self.is_zero() {
            self.clone()
        } else {
            BigInt {
                neg: !self.neg,
                mag: self.mag.clone(),
            }
        }
    }

    pub fn sub(&self, other: &BigInt) -> BigInt {
        self.add(&other.negate())
    }

    pub fn mul(&self, other: &BigInt) -> BigInt {
        if self.is_zero() || other.is_zero() {
            return BigInt::zero();
        }
        let mut out = vec![0u32; self.mag.len() + other.mag.len()];
        for (i, &a) in self.mag.iter().enumerate() {
            let mut carry: u64 = 0;
            for (j, &b) in other.mag.iter().enumerate() {
                let v = out[i + j] as u64 + a as u64 * b as u64 + carry;
                out[i + j] = (v & 0xFFFF_FFFF) as u32;
                carry = v >> 32;
            }
            let mut k = i + other.mag.len();
            while carry > 0 {
                let v = out[k] as u64 + carry;
                out[k] = (v & 0xFFFF_FFFF) as u32;
                carry = v >> 32;
                k += 1;
            }
        }
        BigInt {
            neg: self.neg != other.neg,
            mag: out,
        }
        .trim()
    }

    fn shl_bits(&self, bits: usize) -> BigInt {
        if self.is_zero() {
            return BigInt::zero();
        }
        let (limbs, rem) = (bits / 32, bits % 32);
        let mut mag = vec![0u32; limbs];
        if rem == 0 {
            mag.extend_from_slice(&self.mag);
        } else {
            let mut carry = 0u32;
            for &l in &self.mag {
                mag.push((l << rem) | carry);
                carry = l >> (32 - rem);
            }
            if carry > 0 {
                mag.push(carry);
            }
        }
        BigInt { neg: self.neg, mag }.trim()
    }

    fn bit_len(&self) -> usize {
        match self.mag.last() {
            None => 0,
            Some(top) => (self.mag.len() - 1) * 32 + (32 - top.leading_zeros() as usize),
        }
    }

    /// Shift-subtract long division: (quotient, remainder), remainder takes
    /// the dividend's sign (truncated division, matching fixed-size ints).
    pub fn divmod(&self, other: &BigInt) -> Option<(BigInt, BigInt)> {
        if other.is_zero() {
            return None;
        }
        if Self::cmp_mag(&self.mag, &other.mag) == Ordering::Less {
            return Some((BigInt::zero(), self.clone()));
        }
        let shift = self.bit_len() - other.bit_len();
        let mut rem = BigInt {
            neg: false,
            mag: self.mag.clone(),
        };
        let mut quot = BigInt::zero();
        for s in (0..=shift).rev() {
            let d = other.shl_bits(s);
            let da = BigInt {
                neg: false,
                mag: d.mag.clone(),
            };
            if Self::cmp_mag(&rem.mag, &da.mag) != Ordering::Less {
                rem = BigInt {
                    neg: false,
                    mag: Self::sub_mag(&rem.mag, &da.mag),
                }
                .trim();
                quot = quot.add(&BigInt::from_i64(1).shl_bits(s));
            }
        }
        quot.neg = !quot.is_zero() && (self.neg != other.neg);
        rem.neg = !rem.is_zero() && self.neg;
        Some((quot, rem))
    }

    #[allow(clippy::should_implement_trait)]
    pub fn cmp(&self, other: &BigInt) -> Ordering {
        match (self.neg, other.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => Self::cmp_mag(&self.mag, &other.mag),
            (true, true) => Self::cmp_mag(&other.mag, &self.mag),
        }
    }

    pub fn to_decimal(&self) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let mut digits = Vec::new();
        let mut n = BigInt {
            neg: false,
            mag: self.mag.clone(),
        };
        while !n.is_zero() {
            let (q, r) = n.divmod_small(1_000_000_000);
            digits.push(r);
            n = q;
        }
        let mut s = if self.neg {
            "-".to_string()
        } else {
            String::new()
        };
        s.push_str(&digits.pop().unwrap().to_string());
        for d in digits.iter().rev() {
            s.push_str(&format!("{d:09}"));
        }
        s
    }

    fn pow10(k: u32) -> BigInt {
        let mut n = BigInt::from_i64(1);
        for _ in 0..k {
            n = n.mul_small(10);
        }
        n
    }
}

/// Rounding modes (spec §3.7; the java.math / IEEE 754-2019 decimal set).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RoundingMode {
    Up,
    Down,
    Ceiling,
    Floor,
    HalfUp,
    HalfEven,
}

impl RoundingMode {
    pub fn parse(name: &str) -> Option<RoundingMode> {
        Some(match name {
            "UP" => RoundingMode::Up,
            "DOWN" => RoundingMode::Down,
            "CEILING" => RoundingMode::Ceiling,
            "FLOOR" => RoundingMode::Floor,
            "HALF_UP" => RoundingMode::HalfUp,
            "HALF_EVEN" => RoundingMode::HalfEven,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct BigDec {
    pub coef: BigInt,
    /// digits to the right of the decimal point
    pub scale: u32,
}

impl BigDec {
    /// Parse `123`, `1.05`, `1e3`, `2.5e-2` (suffix `m` and `_` removed).
    pub fn parse(text: &str) -> Option<BigDec> {
        let (mant, exp) = match text.find(['e', 'E']) {
            Some(i) => (&text[..i], text[i + 1..].parse::<i32>().ok()?),
            None => (text, 0),
        };
        let (int_part, frac_part) = match mant.find('.') {
            Some(i) => (&mant[..i], &mant[i + 1..]),
            None => (mant, ""),
        };
        let digits: String = format!("{int_part}{frac_part}");
        let coef = BigInt::parse(&digits, 10)?;
        let scale = frac_part.len() as i64 - exp as i64;
        Some(if scale >= 0 {
            BigDec {
                coef,
                scale: u32::try_from(scale).ok()?,
            }
        } else {
            // negative scale: multiply out
            BigDec {
                coef: coef.mul(&BigInt::pow10((-scale) as u32)),
                scale: 0,
            }
        })
    }

    fn align(a: &BigDec, b: &BigDec) -> (BigInt, BigInt, u32) {
        let scale = a.scale.max(b.scale);
        let ac = a.coef.mul(&BigInt::pow10(scale - a.scale));
        let bc = b.coef.mul(&BigInt::pow10(scale - b.scale));
        (ac, bc, scale)
    }

    pub fn add(&self, other: &BigDec) -> BigDec {
        let (a, b, scale) = Self::align(self, other);
        BigDec {
            coef: a.add(&b),
            scale,
        }
    }

    pub fn sub(&self, other: &BigDec) -> BigDec {
        let (a, b, scale) = Self::align(self, other);
        BigDec {
            coef: a.sub(&b),
            scale,
        }
    }

    pub fn mul(&self, other: &BigDec) -> BigDec {
        BigDec {
            coef: self.coef.mul(&other.coef),
            scale: self.scale + other.scale,
        }
    }

    /// Exact division within a bounded scale extension; `None` = inexact.
    pub fn div_exact(&self, other: &BigDec) -> Option<BigDec> {
        if other.coef.is_zero() {
            return None;
        }
        const MAX_EXTRA: u32 = 34;
        let mut num = self.coef.clone();
        let scale = self.scale as i64 - other.scale as i64;
        for extra in 0..=MAX_EXTRA {
            let (q, r) = num.divmod(&other.coef)?;
            if r.is_zero() {
                let final_scale = scale + extra as i64;
                return Some(if final_scale >= 0 {
                    BigDec {
                        coef: q,
                        scale: final_scale as u32,
                    }
                } else {
                    BigDec {
                        coef: q.mul(&BigInt::pow10((-final_scale) as u32)),
                        scale: 0,
                    }
                });
            }
            num = num.mul_small(10);
        }
        None
    }

    /// Divide with an explicit rounding context (spec §3.7): the result has
    /// exactly `scale` digits, rounded by `mode`.
    pub fn divide(&self, other: &BigDec, scale: u32, mode: RoundingMode) -> Option<BigDec> {
        if other.coef.is_zero() {
            return None;
        }
        // Work at scale+1 so we hold the discard digit, then round.
        let shift = scale as i64 + 1 + other.scale as i64 - self.scale as i64;
        let num = if shift >= 0 {
            self.coef.mul(&BigInt::pow10(shift as u32))
        } else {
            self.coef.divmod(&BigInt::pow10((-shift) as u32))?.0
        };
        let (q, r) = num.divmod(&other.coef)?;
        let negative = q.neg || (self.coef.neg != other.coef.neg && !q.is_zero());
        // q currently has scale+1 digits: split off the last one.
        let (mut quotient, discard) = q.divmod_small(10);
        let discard = discard as i64;
        let exact = r.is_zero();
        let magnitude = discard.abs();
        let round_up = match mode {
            RoundingMode::Down => false,
            RoundingMode::Up => magnitude != 0 || !exact,
            RoundingMode::HalfUp => magnitude >= 5,
            RoundingMode::HalfEven => {
                if magnitude > 5 {
                    true
                } else if magnitude < 5 {
                    false
                } else if !exact {
                    true
                } else {
                    // Ties to even: round up only if the kept digit is odd.
                    let (_, last) = quotient.divmod_small(10);
                    last % 2 == 1
                }
            }
            RoundingMode::Ceiling => (magnitude != 0 || !exact) && !negative,
            RoundingMode::Floor => (magnitude != 0 || !exact) && negative,
        };
        if round_up {
            let one = BigInt::from_i64(1);
            let bumped = BigInt {
                neg: false,
                mag: quotient.mag.clone(),
            }
            .add(&one);
            quotient = BigInt {
                neg: quotient.neg,
                mag: bumped.mag,
            };
        }
        quotient.neg = negative && !quotient.is_zero();
        Some(BigDec {
            coef: quotient,
            scale,
        })
    }

    #[allow(clippy::should_implement_trait)]
    pub fn cmp(&self, other: &BigDec) -> Ordering {
        let (a, b, _) = Self::align(self, other);
        a.cmp(&b)
    }

    pub fn to_decimal(&self) -> String {
        let digits = {
            BigInt {
                neg: false,
                mag: self.coef.mag.clone(),
            }
            .to_decimal()
        };
        let neg = if self.coef.neg { "-" } else { "" };
        if self.scale == 0 {
            return format!("{neg}{digits}");
        }
        let scale = self.scale as usize;
        if digits.len() <= scale {
            format!("{neg}0.{}{digits}", "0".repeat(scale - digits.len()))
        } else {
            let split = digits.len() - scale;
            format!("{neg}{}.{}", &digits[..split], &digits[split..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigint_roundtrip_and_ops() {
        let a = BigInt::parse("123456789012345678901234567890", 10).unwrap();
        assert_eq!(a.to_decimal(), "123456789012345678901234567890");
        let b = BigInt::from_i64(-99999);
        assert_eq!(a.add(&b).to_decimal(), "123456789012345678901234467891");
        let sq = a.mul(&a);
        assert_eq!(
            sq.to_decimal(),
            "15241578753238836750495351562536198787501905199875019052100"
        );
        let (q, r) = sq.divmod(&a).unwrap();
        assert_eq!(q.to_decimal(), a.to_decimal());
        assert!(r.is_zero());
        let (q, r) = BigInt::from_i64(-7).divmod(&BigInt::from_i64(2)).unwrap();
        assert_eq!(q.to_decimal(), "-3");
        assert_eq!(r.to_decimal(), "-1");
    }

    #[test]
    fn bigdec_rounding_contexts() {
        let one = BigDec::parse("1").unwrap();
        let three = BigDec::parse("3").unwrap();
        assert_eq!(
            one.divide(&three, 4, RoundingMode::HalfEven)
                .unwrap()
                .to_decimal(),
            "0.3333"
        );
        let two = BigDec::parse("2").unwrap();
        // 1/2 at scale 0: HALF_EVEN ties to even (0), HALF_UP goes to 1.
        assert_eq!(
            one.divide(&two, 0, RoundingMode::HalfEven)
                .unwrap()
                .to_decimal(),
            "0"
        );
        assert_eq!(
            one.divide(&two, 0, RoundingMode::HalfUp)
                .unwrap()
                .to_decimal(),
            "1"
        );
        let three_halves = BigDec::parse("3").unwrap();
        assert_eq!(
            three_halves
                .divide(&two, 0, RoundingMode::HalfEven)
                .unwrap()
                .to_decimal(),
            "2"
        );
        assert_eq!(
            one.divide(&three, 2, RoundingMode::Up)
                .unwrap()
                .to_decimal(),
            "0.34"
        );
        assert_eq!(
            one.divide(&three, 2, RoundingMode::Down)
                .unwrap()
                .to_decimal(),
            "0.33"
        );
        let ten = BigDec::parse("10").unwrap();
        assert_eq!(
            ten.divide(&two, 2, RoundingMode::HalfEven)
                .unwrap()
                .to_decimal(),
            "5.00"
        );
        assert!(one
            .divide(&BigDec::parse("0").unwrap(), 2, RoundingMode::HalfUp)
            .is_none());
    }

    #[test]
    fn bigdec_basics() {
        let a = BigDec::parse("19.99").unwrap();
        let b = BigDec::parse("3").unwrap();
        assert_eq!(a.mul(&b).to_decimal(), "59.97");
        assert_eq!(a.add(&b).to_decimal(), "22.99");
        assert_eq!(a.sub(&b).to_decimal(), "16.99");
        let half = BigDec::parse("1")
            .unwrap()
            .div_exact(&BigDec::parse("8").unwrap())
            .unwrap();
        assert_eq!(half.to_decimal(), "0.125");
        assert!(BigDec::parse("1")
            .unwrap()
            .div_exact(&BigDec::parse("3").unwrap())
            .is_none());
        assert_eq!(BigDec::parse("2.5e-2").unwrap().to_decimal(), "0.025");
        assert_eq!(BigDec::parse("1e3").unwrap().to_decimal(), "1000");
        assert_eq!(a.cmp(&b), Ordering::Greater);
    }
}
