//! Q16.16 fixed-point arithmetic.
//!
//! Every value that participates in authoritative game state is a [`Fixed`].
//! Floating point is permitted in this module **only** for the `from_f32` /
//! `to_f32` conversion helpers, which exist for authoring data and for test
//! oracles. The CI guard in `tests/no_float.rs` greps the rest of `sim` for
//! `f32`/`f64` and fails the build, so a stray float cannot creep into
//! movement or combat without someone deleting that test.
//!
//! # Representation
//!
//! `Fixed(i32)` where `1.0 == 65536`. Range is `[-32768.0, 32767.99998]` with a
//! resolution of `1/65536 ≈ 1.5e-5`.
//!
//! # Rounding
//!
//! Multiplication and division use an `i64` intermediate and an arithmetic
//! right shift, which **floors** toward negative infinity. This is deliberate:
//! `wasm32` and `x86_64`/`aarch64` all implement arithmetic shift identically,
//! whereas rounding-half-away-from-zero would need an explicit bias whose sign
//! handling is the usual source of cross-target drift.
//!
//! Overflow saturates rather than panicking, because a physics edge case must
//! not take down a room with six players in it.

use serde::{Deserialize, Serialize};

use crate::sin_table::{SIN_TABLE, SIN_TABLE_LEN};

/// Number of fractional bits.
pub const FRAC_BITS: u32 = 16;
/// `1.0` in Q16.16.
pub const ONE_RAW: i32 = 1 << FRAC_BITS;
/// `1.0` as a `Fixed`.
pub const ONE: Fixed = Fixed(ONE_RAW);
/// `0.0`.
pub const ZERO: Fixed = Fixed(0);
/// `0.5`.
pub const HALF: Fixed = Fixed(ONE_RAW / 2);
/// `pi`.
pub const PI: Fixed = Fixed(205_887);
/// `2 * pi`.
pub const TAU: Fixed = Fixed(411_775);

/// A Q16.16 signed fixed-point number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Fixed(pub i32);

impl Fixed {
    /// Raw constructor. `Fixed::from_raw(65536)` is `1.0`.
    #[inline]
    pub const fn from_raw(raw: i32) -> Self {
        Fixed(raw)
    }

    /// The underlying Q16.16 bits.
    #[inline]
    pub const fn to_raw(self) -> i32 {
        self.0
    }

    /// Integer to fixed. Saturates instead of overflowing.
    #[inline]
    pub const fn from_int(v: i32) -> Self {
        match v.checked_mul(ONE_RAW) {
            Some(r) => Fixed(r),
            None => Fixed(if v > 0 { i32::MAX } else { i32::MIN }),
        }
    }

    /// Truncating conversion toward zero.
    #[inline]
    pub const fn to_int(self) -> i32 {
        // `>>` on i32 is arithmetic and therefore floors; `to_int` truncates,
        // so negative values need the fraction added back before shifting.
        if self.0 >= 0 {
            self.0 >> FRAC_BITS
        } else {
            -((-self.0) >> FRAC_BITS)
        }
    }

    /// Floor to an integer (toward negative infinity).
    ///
    /// `>>` on `i32` is an arithmetic shift, which is exactly floor division by
    /// 2^16 — so this is both the fastest and the only correct way to get a
    /// floor out of Q16.16. [`Fixed::to_int`] truncates instead, which is the
    /// wrong operation for lattice lookups at negative coordinates.
    #[inline]
    pub const fn floor_int(self) -> i32 {
        self.0 >> FRAC_BITS
    }

    /// Round to the nearest integer, halves away from zero.
    #[inline]
    pub const fn round_int(self) -> i32 {
        if self.0 >= 0 {
            (self.0 + ONE_RAW / 2) >> FRAC_BITS
        } else {
            -(((-self.0) + ONE_RAW / 2) >> FRAC_BITS)
        }
    }

    /// Saturating add.
    #[inline]
    pub const fn saturating_add(self, rhs: Fixed) -> Fixed {
        Fixed(self.0.saturating_add(rhs.0))
    }

    /// Saturating subtract.
    #[inline]
    pub const fn saturating_sub(self, rhs: Fixed) -> Fixed {
        Fixed(self.0.saturating_sub(rhs.0))
    }

    /// Saturating multiply. `Fixed::from_int(3) * HALF == 1.5`.
    #[inline]
    pub const fn saturating_mul(self, rhs: Fixed) -> Fixed {
        let p = (self.0 as i64) * (rhs.0 as i64);
        let r = p >> FRAC_BITS;
        if r > i32::MAX as i64 {
            Fixed(i32::MAX)
        } else if r < i32::MIN as i64 {
            Fixed(i32::MIN)
        } else {
            Fixed(r as i32)
        }
    }

    /// Saturating divide. Division by zero saturates to the sign of the
    /// numerator rather than panicking.
    #[inline]
    pub const fn saturating_div(self, rhs: Fixed) -> Fixed {
        if rhs.0 == 0 {
            return Fixed(if self.0 >= 0 { i32::MAX } else { i32::MIN });
        }
        let q = ((self.0 as i64) << FRAC_BITS) / (rhs.0 as i64);
        if q > i32::MAX as i64 {
            Fixed(i32::MAX)
        } else if q < i32::MIN as i64 {
            Fixed(i32::MIN)
        } else {
            Fixed(q as i32)
        }
    }

    /// Saturating negation.
    #[inline]
    pub const fn saturating_neg(self) -> Fixed {
        Fixed(self.0.saturating_neg())
    }

    /// Absolute value, saturating at `i32::MAX`.
    #[inline]
    pub const fn abs(self) -> Fixed {
        Fixed(self.0.saturating_abs())
    }

    /// Clamp between two bounds.
    #[inline]
    pub fn clamp(self, lo: Fixed, hi: Fixed) -> Fixed {
        if self < lo {
            lo
        } else if self > hi {
            hi
        } else {
            self
        }
    }

    /// Smaller of two values.
    #[inline]
    pub fn min(self, other: Fixed) -> Fixed {
        if self < other {
            self
        } else {
            other
        }
    }

    /// Larger of two values.
    #[inline]
    pub fn max(self, other: Fixed) -> Fixed {
        if self > other {
            self
        } else {
            other
        }
    }

    /// Square root. Negative input clamps to zero.
    ///
    /// Computed as `isqrt(raw << 16)`, which is exact to within one unit of
    /// the fixed-point resolution.
    #[inline]
    pub const fn sqrt(self) -> Fixed {
        if self.0 <= 0 {
            return ZERO;
        }
        let n = (self.0 as u64) << FRAC_BITS;
        Fixed(isqrt_u64(n) as i32)
    }

    /// Linear interpolation: `self` at `t == 0`, `other` at `t == 1`.
    #[inline]
    pub const fn lerp(self, other: Fixed, t: Fixed) -> Fixed {
        self.saturating_add(other.saturating_sub(self).saturating_mul(t))
    }

    /// `self * self`, saturating.
    #[inline]
    pub const fn square(self) -> Fixed {
        self.saturating_mul(self)
    }

    /// Conversion from `f32`. **Authoring and tests only** — see module docs.
    #[inline]
    pub fn from_f32(v: f32) -> Fixed {
        Fixed((v * ONE_RAW as f32).round() as i32)
    }

    /// Conversion to `f32`. **Presentation and tests only.**
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.0 as f32 / ONE_RAW as f32
    }

    /// Conversion from `f64`, for higher-precision test oracles.
    #[inline]
    pub fn from_f64(v: f64) -> Fixed {
        Fixed((v * ONE_RAW as f64).round() as i32)
    }

    /// Conversion to `f64`, for higher-precision test oracles.
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / ONE_RAW as f64
    }
}

/// Integer square root of a `u64`, by bit-by-bit restoration.
///
/// Chosen over Newton iteration because it is branch-predictable, exact, and
/// needs no seed — which matters for determinism across targets.
const fn isqrt_u64(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut bit: u64 = 1 << 62;
    let mut rem = n;
    let mut root: u64 = 0;
    while bit > rem {
        bit >>= 2;
    }
    while bit != 0 {
        if rem >= root + bit {
            rem -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root
}

// ---------------------------------------------------------------------------
// Operators. All saturating; see module docs.
// ---------------------------------------------------------------------------

impl core::ops::Add for Fixed {
    type Output = Fixed;
    #[inline]
    fn add(self, rhs: Fixed) -> Fixed {
        self.saturating_add(rhs)
    }
}

impl core::ops::Sub for Fixed {
    type Output = Fixed;
    #[inline]
    fn sub(self, rhs: Fixed) -> Fixed {
        self.saturating_sub(rhs)
    }
}

impl core::ops::Mul for Fixed {
    type Output = Fixed;
    #[inline]
    fn mul(self, rhs: Fixed) -> Fixed {
        self.saturating_mul(rhs)
    }
}

impl core::ops::Div for Fixed {
    type Output = Fixed;
    #[inline]
    fn div(self, rhs: Fixed) -> Fixed {
        self.saturating_div(rhs)
    }
}

impl core::ops::Neg for Fixed {
    type Output = Fixed;
    #[inline]
    fn neg(self) -> Fixed {
        self.saturating_neg()
    }
}

impl core::ops::AddAssign for Fixed {
    #[inline]
    fn add_assign(&mut self, rhs: Fixed) {
        *self = self.saturating_add(rhs);
    }
}

impl core::ops::SubAssign for Fixed {
    #[inline]
    fn sub_assign(&mut self, rhs: Fixed) {
        *self = self.saturating_sub(rhs);
    }
}

impl core::fmt::Debug for Fixed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:.5}", self.to_f64())
    }
}

// ---------------------------------------------------------------------------
// Trigonometry
// ---------------------------------------------------------------------------

/// Sine of an angle in radians.
///
/// Table lookup over 512 entries with linear interpolation between them.
pub fn sin(rad: Fixed) -> Fixed {
    let idx = turn_index(rad);
    let (i0, i1, t) = idx;
    let a = SIN_TABLE[i0];
    let b = SIN_TABLE[i1];
    let a = Fixed(a);
    let b = Fixed(b);
    a.lerp(b, t)
}

/// Cosine of an angle in radians.
pub fn cos(rad: Fixed) -> Fixed {
    sin(rad.saturating_add(Fixed::from_raw(TAU.0 / 4)))
}

/// Sine of a binary angle: `yaw` is 1/256 of a full turn.
///
/// This is the hot path for movement and buddy aiming — the protocol carries
/// yaw as a `u8`, so this lookup is exact with no interpolation at all.
#[inline]
pub fn sin_yaw(yaw: u8) -> Fixed {
    // 256 yaw steps map exactly onto every other entry of the 512-entry table.
    Fixed(SIN_TABLE[(yaw as usize) * 2])
}

/// Cosine of a binary angle; see [`sin_yaw`].
#[inline]
pub fn cos_yaw(yaw: u8) -> Fixed {
    // cos(t) = sin(t + pi/2), and a quarter turn is len/4 entries.
    Fixed(SIN_TABLE[((yaw as usize) * 2 + SIN_TABLE_LEN / 4) % SIN_TABLE_LEN])
}

/// Convert a yaw byte to radians.
#[inline]
pub fn yaw_to_rad(yaw: u8) -> Fixed {
    Fixed::from_raw(((yaw as i32) * TAU.0) / 256)
}

/// Map radians onto `(i0, i1, t)` where `i0`/`i1` index the sine table and `t`
/// is the interpolation weight between them.
fn turn_index(rad: Fixed) -> (usize, usize, Fixed) {
    let len = SIN_TABLE_LEN as i64;
    let tau = TAU.0 as i64;
    // `scaled` is the table position in units of `tau`, i.e. idx * tau.
    // Dividing by tau once gives the integer index; the remainder over tau is
    // the interpolation weight. Dividing twice was the original bug.
    let scaled = (rad.0 as i64) * len;
    let idx = scaled.div_euclid(tau);
    let rem = scaled.rem_euclid(tau);
    let i0 = idx.rem_euclid(len) as usize;
    let i1 = (i0 + 1) % SIN_TABLE_LEN;
    let t = Fixed(((rem * ONE_RAW as i64) / tau) as i32);
    (i0, i1, t)
}

/// Two-argument arctangent, in radians.
///
/// Uses the classic cubic approximation on the reduced argument
/// `z = min/max ∈ [0, 1]`:
///
/// ```text
/// atan(z) ≈ (pi/4)z - z(|z| - 1)(0.2447 + 0.0663|z|)
/// ```
///
/// Maximum absolute error is about 0.0015 rad (0.086 degrees), which is far
/// inside the 1/256 rad bound this module promises for aiming.
pub fn atan2(y: Fixed, x: Fixed) -> Fixed {
    if x.0 == 0 && y.0 == 0 {
        return ZERO;
    }

    let ax = x.abs();
    let ay = y.abs();
    let (num, den) = if ay <= ax { (ay, ax) } else { (ax, ay) };

    // atan of the reduced argument.
    let z = num.saturating_div(den);
    let pio4 = Fixed::from_raw(51_472); // pi/4
    let c1 = Fixed::from_raw(16_037); // 0.2447
    let c2 = Fixed::from_raw(4_345); // 0.0663
    let mut a = pio4.saturating_mul(z).saturating_sub(
        z.saturating_mul(z.abs().saturating_sub(ONE))
            .saturating_mul(c1.saturating_add(c2.saturating_mul(z.abs()))),
    );

    if ay > ax {
        a = Fixed::from_raw(102_944).saturating_sub(a); // pi/2 - a
    }
    if x.0 < 0 {
        a = PI.saturating_sub(a);
    }
    if y.0 < 0 {
        a = a.saturating_neg();
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tolerance for the accuracy assertions: 1e-3 in fixed-point terms.
    const TOL: f64 = 1.0e-3;

    #[test]
    fn one_is_65536() {
        assert_eq!(ONE.to_raw(), 65_536);
        assert_eq!(ONE.to_int(), 1);
        assert_eq!(ZERO.to_int(), 0);
    }

    #[test]
    fn from_and_to_int_roundtrip() {
        for v in [-1000, -1, 0, 1, 1000, 32767] {
            assert_eq!(Fixed::from_int(v).to_int(), v, "v = {v}");
        }
    }

    #[test]
    fn mul_and_div_are_consistent() {
        let a = Fixed::from_int(3);
        let b = Fixed::from_int(2);
        assert_eq!(a * b, Fixed::from_int(6));
        assert_eq!(a / b, Fixed::from_f32(1.5));
        assert_eq!(a * HALF, Fixed::from_f32(1.5));
    }

    #[test]
    fn negative_division_floors() {
        // -3 / 2 = -1.5 exactly in Q16.16.
        let q = Fixed::from_int(-3) / Fixed::from_int(2);
        assert_eq!(q.to_raw(), -(3 * ONE_RAW) / 2);
        assert!((q.to_f64() - (-1.5)).abs() < 1e-6);
    }

    #[test]
    fn to_int_truncates_toward_zero() {
        assert_eq!(Fixed::from_f32(-1.75).to_int(), -1);
        assert_eq!(Fixed::from_f32(1.75).to_int(), 1);
        assert_eq!(Fixed::from_f32(-1.75).round_int(), -2);
    }

    #[test]
    fn floor_int_floors_negatives() {
        assert_eq!(Fixed::from_f32(-1.75).floor_int(), -2);
        assert_eq!(Fixed::from_f32(-0.25).floor_int(), -1);
        assert_eq!(Fixed::from_f32(1.75).floor_int(), 1);
        assert_eq!(Fixed::from_int(-3).floor_int(), -3);
    }

    #[test]
    fn saturating_ops_do_not_panic() {
        let max = Fixed::from_raw(i32::MAX);
        assert_eq!(max + max, Fixed(i32::MAX));
        assert_eq!(max * max, Fixed(i32::MAX));
        assert_eq!(max / ZERO, Fixed(i32::MAX));
        assert_eq!(Fixed::from_int(-5) / ZERO, Fixed(i32::MIN));
    }

    #[test]
    fn sqrt_matches_reference() {
        for v in [0.0_f64, 0.25, 1.0, 2.0, 9.0, 100.0, 1234.5] {
            let got = Fixed::from_f64(v).sqrt().to_f64();
            assert!((got - v.sqrt()).abs() < TOL, "sqrt({v}) = {got}");
        }
        assert_eq!(Fixed::from_int(-4).sqrt(), ZERO);
    }

    #[test]
    fn sin_and_cos_are_accurate() {
        let mut max_err: f64 = 0.0;
        // Sweep the full circle in 1000 steps, including negative angles.
        for k in -500..500 {
            let rad = (k as f64) * 0.012_566_370_614_359; // 2*pi/500
            let f = Fixed::from_f64(rad);
            max_err = max_err.max((sin(f).to_f64() - rad.sin()).abs());
            max_err = max_err.max((cos(f).to_f64() - rad.cos()).abs());
        }
        assert!(max_err < TOL, "max trig error {max_err}");
    }

    #[test]
    fn yaw_table_is_exact_at_cardinals() {
        // yaw 0 = +X, 64 = +Z, 128 = -X, 192 = -Z in our convention.
        assert_eq!(sin_yaw(0), ZERO);
        assert_eq!(cos_yaw(0), ONE);
        assert!((sin_yaw(64).to_f64() - 1.0).abs() < 1e-3);
        assert!((cos_yaw(64).to_f64()).abs() < 1e-3);
        assert!((cos_yaw(128).to_f64() + 1.0).abs() < 1e-3);
    }

    #[test]
    fn atan2_quadrants() {
        let cases = [
            (0.0, 1.0, 0.0),
            (1.0, 0.0, core::f64::consts::FRAC_PI_2),
            (0.0, -1.0, core::f64::consts::PI),
            (-1.0, 0.0, -core::f64::consts::FRAC_PI_2),
            (1.0, 1.0, core::f64::consts::FRAC_PI_4),
            (-1.0, -1.0, -3.0 * core::f64::consts::FRAC_PI_4),
        ];
        for (y, x, want) in cases {
            let got = atan2(Fixed::from_f64(y), Fixed::from_f64(x)).to_f64();
            assert!(
                (got - want).abs() < 0.02,
                "atan2({y},{x}) = {got}, want {want}"
            );
        }
    }

    #[test]
    fn lerp_hits_endpoints() {
        let a = Fixed::from_int(2);
        let b = Fixed::from_int(10);
        assert_eq!(a.lerp(b, ZERO), a);
        assert_eq!(a.lerp(b, ONE), b);
        assert_eq!(a.lerp(b, HALF), Fixed::from_int(6));
    }
}
