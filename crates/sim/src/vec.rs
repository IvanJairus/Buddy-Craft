//! Fixed-point 3-vectors.
//!
//! Every position, velocity and offset in authoritative state is a [`Vec3`] of
//! [`Fixed`] components. There is no `glam` or `nalgebra` here for one reason:
//! those libraries are built around `f32`, and a single `f32` in a position
//! makes client prediction and server authority disagree on some future tick
//! (plan A-08).
//!
//! The operations are deliberately minimal — no matrices, no quaternions, no
//! cross product. The simulation needs distances, direction and accumulation,
//! and every extra operation is another place for a rounding rule to diverge.

use serde::{Deserialize, Serialize};

use crate::fixed::{Fixed, ONE, ZERO};

/// A vector of three `Fixed` components.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
pub struct Vec3 {
    /// X component.
    pub x: Fixed,
    /// Y component.
    pub y: Fixed,
    /// Z component.
    pub z: Fixed,
}

impl Vec3 {
    /// The zero vector.
    pub const ZERO: Vec3 = Vec3 {
        x: ZERO,
        y: ZERO,
        z: ZERO,
    };
    /// `(1, 0, 0)`.
    pub const X: Vec3 = Vec3 {
        x: ONE,
        y: ZERO,
        z: ZERO,
    };
    /// `(0, 1, 0)`.
    pub const Y: Vec3 = Vec3 {
        x: ZERO,
        y: ONE,
        z: ZERO,
    };
    /// `(0, 0, 1)`.
    pub const Z: Vec3 = Vec3 {
        x: ZERO,
        y: ZERO,
        z: ONE,
    };

    /// Build from components.
    #[inline]
    pub const fn new(x: Fixed, y: Fixed, z: Fixed) -> Vec3 {
        Vec3 { x, y, z }
    }

    /// Build from integer components.
    #[inline]
    pub fn from_ints(x: i32, y: i32, z: i32) -> Vec3 {
        Vec3 {
            x: Fixed::from_int(x),
            y: Fixed::from_int(y),
            z: Fixed::from_int(z),
        }
    }

    /// Component by axis index: 0 = X, 1 = Y, 2 = Z.
    ///
    /// Panics on `axis > 2`; this is used by the collision sweep, which only
    /// ever passes 0, 1 or 2.
    #[inline]
    pub fn axis(self, axis: usize) -> Fixed {
        match axis {
            0 => self.x,
            1 => self.y,
            2 => self.z,
            _ => panic!("axis index {axis} out of range"),
        }
    }

    /// Write one axis.
    #[inline]
    pub fn set_axis(&mut self, axis: usize, value: Fixed) {
        match axis {
            0 => self.x = value,
            1 => self.y = value,
            2 => self.z = value,
            _ => panic!("axis index {axis} out of range"),
        }
    }

    /// Scale by a scalar.
    #[inline]
    pub fn scale(self, k: Fixed) -> Vec3 {
        Vec3 {
            x: self.x * k,
            y: self.y * k,
            z: self.z * k,
        }
    }

    /// Dot product.
    #[inline]
    pub fn dot(self, rhs: Vec3) -> Fixed {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    /// Squared length. Prefer this to [`Vec3::length`] when comparing
    /// distances: it avoids the square root and its rounding entirely.
    #[inline]
    pub fn length_sq(self) -> Fixed {
        self.dot(self)
    }

    /// Squared length of the horizontal (X, Z) component.
    #[inline]
    pub fn horizontal_length_sq(self) -> Fixed {
        self.x * self.x + self.z * self.z
    }

    /// Euclidean length, via the integer square root in [`Fixed::sqrt`].
    #[inline]
    pub fn length(self) -> Fixed {
        self.length_sq().sqrt()
    }

    /// Horizontal (X, Z) length.
    #[inline]
    pub fn horizontal_length(self) -> Fixed {
        self.horizontal_length_sq().sqrt()
    }

    /// Distance to another point.
    #[inline]
    pub fn distance(self, other: Vec3) -> Fixed {
        (self - other).length()
    }

    /// Squared distance to another point.
    #[inline]
    pub fn distance_sq(self, other: Vec3) -> Fixed {
        (self - other).length_sq()
    }

    /// Horizontal distance to another point, ignoring Y.
    #[inline]
    pub fn horizontal_distance(self, other: Vec3) -> Fixed {
        let d = self - other;
        (d.x * d.x + d.z * d.z).sqrt()
    }

    /// Unit vector in the same direction, or [`Vec3::ZERO`] for a zero vector.
    ///
    /// A zero input returns zero rather than `NaN` — that case is reachable
    /// (an entity standing exactly on its target) and must not produce a
    /// non-finite value in authoritative state.
    #[inline]
    pub fn normalize(self) -> Vec3 {
        let len = self.length();
        if len.to_raw() <= 0 {
            return Vec3::ZERO;
        }
        self.scale(ONE.saturating_div(len))
    }

    /// Clamp each component into `[-limit, limit]`.
    #[inline]
    pub fn clamp_length(self, limit: Fixed) -> Vec3 {
        let len_sq = self.length_sq();
        if len_sq <= limit * limit {
            return self;
        }
        let len = len_sq.sqrt();
        if len.to_raw() <= 0 {
            return Vec3::ZERO;
        }
        self.scale(limit.saturating_div(len))
    }

    /// The point `t` of the way from `self` to `other`.
    #[inline]
    pub fn lerp(self, other: Vec3, t: Fixed) -> Vec3 {
        Vec3 {
            x: self.x.lerp(other.x, t),
            y: self.y.lerp(other.y, t),
            z: self.z.lerp(other.z, t),
        }
    }

    /// Floor each component to an integer triple.
    #[inline]
    pub fn floor_to_ints(self) -> [i32; 3] {
        [self.x.floor_int(), self.y.floor_int(), self.z.floor_int()]
    }
}

impl core::ops::Add for Vec3 {
    type Output = Vec3;
    #[inline]
    fn add(self, rhs: Vec3) -> Vec3 {
        Vec3 {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
            z: self.z + rhs.z,
        }
    }
}

impl core::ops::Sub for Vec3 {
    type Output = Vec3;
    #[inline]
    fn sub(self, rhs: Vec3) -> Vec3 {
        Vec3 {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
            z: self.z - rhs.z,
        }
    }
}

impl core::ops::Neg for Vec3 {
    type Output = Vec3;
    #[inline]
    fn neg(self) -> Vec3 {
        Vec3 {
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }
}

impl core::ops::AddAssign for Vec3 {
    #[inline]
    fn add_assign(&mut self, rhs: Vec3) {
        *self = *self + rhs;
    }
}

impl core::ops::SubAssign for Vec3 {
    #[inline]
    fn sub_assign(&mut self, rhs: Vec3) {
        *self = *self - rhs;
    }
}

/// `vector * scalar`. Same as [`Vec3::scale`], for readability at call sites
/// like `dir * travel` where the scalar reads as a duration or a fraction.
impl core::ops::Mul<Fixed> for Vec3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, rhs: Fixed) -> Vec3 {
        self.scale(rhs)
    }
}

/// `scalar * vector`, the mirrored form.
impl core::ops::Mul<Vec3> for Fixed {
    type Output = Vec3;
    #[inline]
    fn mul(self, rhs: Vec3) -> Vec3 {
        rhs.scale(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::HALF;

    #[test]
    fn arithmetic_is_componentwise() {
        let a = Vec3::from_ints(1, 2, 3);
        let b = Vec3::from_ints(4, 5, 6);
        assert_eq!(a + b, Vec3::from_ints(5, 7, 9));
        assert_eq!(b - a, Vec3::from_ints(3, 3, 3));
        assert_eq!(a.scale(Fixed::from_int(2)), Vec3::from_ints(2, 4, 6));
        assert_eq!(-a, Vec3::from_ints(-1, -2, -3));
        assert_eq!(a.dot(b), Fixed::from_int(32));
    }

    #[test]
    fn length_matches_pythagoras() {
        let v = Vec3::from_ints(3, 4, 0);
        assert_eq!(v.length(), Fixed::from_int(5));
        assert_eq!(v.length_sq(), Fixed::from_int(25));
        let h = Vec3::from_ints(3, 99, 4);
        assert_eq!(h.horizontal_length(), Fixed::from_int(5));
    }

    #[test]
    fn normalize_gives_unit_length() {
        for v in [
            Vec3::from_ints(1, 0, 0),
            Vec3::from_ints(0, -1, 0),
            Vec3::from_ints(3, 4, 12),
            Vec3::from_ints(-7, 2, 1),
        ] {
            let n = v.normalize();
            let len = n.length();
            assert!(
                (len - ONE).abs().to_f64() < 1e-3,
                "normalized length {len:?} is not 1"
            );
        }
    }

    #[test]
    fn normalize_of_zero_is_zero_not_nan() {
        assert_eq!(Vec3::ZERO.normalize(), Vec3::ZERO);
    }

    #[test]
    fn clamp_length_caps_the_magnitude() {
        let v = Vec3::from_ints(30, 40, 0);
        let c = v.clamp_length(Fixed::from_int(10));
        assert!((c.length() - Fixed::from_int(10)).abs().to_f64() < 1e-3);
        // A vector already inside the limit is returned unchanged.
        let small = Vec3::from_ints(1, 0, 0);
        assert_eq!(small.clamp_length(Fixed::from_int(10)), small);
    }

    #[test]
    fn axis_access_roundtrips() {
        let mut v = Vec3::ZERO;
        for axis in 0..3 {
            v.set_axis(axis, Fixed::from_int(axis as i32 + 1));
        }
        for axis in 0..3 {
            assert_eq!(v.axis(axis), Fixed::from_int(axis as i32 + 1));
        }
    }

    #[test]
    fn lerp_hits_the_midpoint() {
        let a = Vec3::from_ints(0, 0, 0);
        let b = Vec3::from_ints(10, 20, 30);
        assert_eq!(a.lerp(b, HALF), Vec3::from_ints(5, 10, 15));
    }

    #[test]
    fn floor_handles_negatives() {
        let v = Vec3::new(
            Fixed::from_raw(-1),
            Fixed::from_int(2),
            Fixed::from_raw(-65537),
        );
        assert_eq!(v.floor_to_ints(), [-1, 2, -2]);
    }
}
