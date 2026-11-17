//! Exact dyadic accumulation for experiment_run.py's statistics.mean/stdev.
//! Each finite binary64 input is an integer multiple of 2^-1074. Keep that
//! integer through summation and variance, and round only the final result.
use std::cmp::Ordering;

#[derive(Clone, Default, Eq, PartialEq)]
struct Natural(Vec<u64>);
impl Natural {
    fn new(value: u128) -> Self {
        let mut n = Self(vec![value as u64, (value >> 64) as u64]);
        n.trim();
        n
    }
    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.iter().rev().cmp(other.0.iter().rev()))
    }
    fn add(&mut self, other: &Self) {
        self.0.resize(self.0.len().max(other.0.len()), 0);
        let mut carry = 0u128;
        for (i, word) in self.0.iter_mut().enumerate() {
            let sum = *word as u128 + other.0.get(i).copied().unwrap_or(0) as u128 + carry;
            *word = sum as u64;
            carry = sum >> 64;
        }
        if carry != 0 {
            self.0.push(carry as u64);
        }
    }
    fn sub(&mut self, other: &Self) {
        debug_assert!(self.cmp(other) != Ordering::Less);
        let mut borrow = false;
        for (i, word) in self.0.iter_mut().enumerate() {
            let (a, b) = word.overflowing_sub(other.0.get(i).copied().unwrap_or(0));
            let (a, c) = a.overflowing_sub(u64::from(borrow));
            *word = a;
            borrow = b || c;
        }
        debug_assert!(!borrow);
        self.trim();
    }
    fn shift(&self, bits: usize) -> Self {
        if self.0.is_empty() {
            return Self::default();
        }
        let words = bits / 64;
        let bits = bits % 64;
        let mut result = vec![0; self.0.len() + words + usize::from(bits != 0)];
        for (i, word) in self.0.iter().enumerate() {
            result[i + words] |= word << bits;
            if bits != 0 {
                result[i + words + 1] |= word >> (64 - bits);
            }
        }
        let mut result = Self(result);
        result.trim();
        result
    }
    fn multiply(&self, other: &Self) -> Self {
        let mut result = vec![0u64; self.0.len() + other.0.len()];
        for (i, a) in self.0.iter().enumerate() {
            let mut carry = 0u128;
            for (j, b) in other.0.iter().enumerate() {
                let product = *a as u128 * *b as u128 + result[i + j] as u128 + carry;
                result[i + j] = product as u64;
                carry = product >> 64;
            }
            result[i + other.0.len()] = carry as u64;
        }
        let mut result = Self(result);
        result.trim();
        result
    }
    fn times(&self, n: usize) -> Self {
        self.multiply(&Self::new(n as u128))
    }
}

fn parts(value: f64) -> (u64, usize) {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as usize;
    let fraction = bits & ((1u64 << 52) - 1);
    if exponent == 0 {
        (fraction, 0)
    } else {
        (fraction | (1 << 52), exponent - 1)
    }
}
fn integer(bits: u64) -> Natural {
    // The infinity sentinel represents the next binade, for overflow rounding.
    if bits == f64::INFINITY.to_bits() {
        Natural::new(1).shift(2098)
    } else {
        let (mantissa, shift) = parts(f64::from_bits(bits));
        Natural::new(mantissa as u128).shift(shift)
    }
}
struct Sum {
    magnitude: Natural,
    squares: Natural,
    negative: bool,
}
fn sum(values: &[f64], squares: bool) -> Option<Sum> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mut positive = Natural::default();
    let mut negative = Natural::default();
    let mut ss = Natural::default();
    for value in values {
        let (mantissa, shift) = parts(*value);
        let term = Natural::new(mantissa as u128).shift(shift);
        if value.is_sign_negative() {
            negative.add(&term);
        } else {
            positive.add(&term);
        }
        if squares {
            ss.add(&Natural::new(mantissa as u128 * mantissa as u128).shift(2 * shift));
        }
    }
    let sign = positive.cmp(&negative) == Ordering::Less;
    let magnitude = if sign {
        negative.sub(&positive);
        negative
    } else {
        positive.sub(&negative);
        positive
    };
    Some(Sum {
        magnitude,
        squares: ss,
        negative: sign,
    })
}

// Compare exact values, then the exact midpoint. This also covers subnormal
// ties and overflow without intermediate floating-point under/overflow.
fn rounded(compare: impl Fn(&Natural) -> Ordering, midpoint: impl Fn(&Natural) -> Ordering) -> f64 {
    let mut low = 0u64;
    let mut high = f64::INFINITY.to_bits();
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        match compare(&integer(middle)) {
            Ordering::Less => high = middle,
            Ordering::Equal => return f64::from_bits(middle),
            Ordering::Greater => low = middle,
        }
    }
    let mut boundary = integer(low);
    boundary.add(&integer(high));
    match midpoint(&boundary) {
        Ordering::Less => f64::from_bits(low),
        Ordering::Greater => f64::from_bits(high),
        Ordering::Equal => f64::from_bits(if low & 1 == 0 { low } else { high }),
    }
}
pub fn mean(values: &[f64]) -> Option<f64> {
    let sum = sum(values, false)?;
    let twice = sum.magnitude.shift(1);
    let value = rounded(
        |candidate| sum.magnitude.cmp(&candidate.times(values.len())),
        |boundary| twice.cmp(&boundary.times(values.len())),
    );
    Some(if sum.negative { -value } else { value })
}
pub fn stdev(values: &[f64]) -> Option<f64> {
    let sum = sum(values, true)?;
    if values.len() == 1 {
        return Some(0.0);
    }
    // (n * sum(x^2) - sum(x)^2) / (n * (n-1)), in units of 2^-2148.
    let mut numerator = sum.squares.times(values.len());
    numerator.sub(&sum.magnitude.multiply(&sum.magnitude));
    let four = numerator.shift(2);
    let square = |v: &Natural| v.multiply(v).times(values.len()).times(values.len() - 1);
    Some(rounded(
        |candidate| numerator.cmp(&square(candidate)),
        |boundary| four.cmp(&square(boundary)),
    ))
}
