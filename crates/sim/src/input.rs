//! Player input, quantised to a wire-safe frame.
//!
//! The input frame is the only thing a client sends per tick, and it is
//! deliberately tiny and integer-only: two `i8` axes, two yaw/pitch bytes, one
//! button bitfield. There are no floats and no absolute positions, which means
//! a client cannot ask the server to teleport it — the server derives movement
//! from the same inputs it would have predicted locally.
//!
//! Quantisation is not a compromise here, it is the point. A `f32` stick axis
//! sent over the wire would deserialise to a slightly different value than the
//! one the client predicted with, and the two would drift. An `i8` round-trips
//! exactly.

use serde::{Deserialize, Serialize};

use crate::fixed::{cos_yaw, sin_yaw, Fixed, ONE};
use crate::time::Tick;
use crate::vec::Vec3;

/// Button bits. Kept as a `u8` so the whole frame fits in 8 bytes.
pub mod buttons {
    /// Jump, or swim upward while submerged.
    pub const JUMP: u8 = 1 << 0;
    /// Attack with the held weapon.
    pub const FIRE: u8 = 1 << 1;
    /// Place a crate (on a pad) or pick one up.
    pub const USE: u8 = 1 << 2;
    /// Cycle the nearest buddy's stance: Follow / Hold / Assault.
    pub const STANCE: u8 = 1 << 3;
    /// Reserved: crouch. Not simulated in v1, but the bit is allocated so the
    /// protocol does not have to change to add it.
    pub const CROUCH: u8 = 1 << 4;
}

/// One tick of player intent.
///
/// `Default` is "no input", which is what a disconnected player contributes:
/// they stand still rather than being removed, so a brief network hiccup does
/// not delete a player from the match.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct InputFrame {
    /// The tick this input applies to.
    pub tick: Tick,
    /// Strafe axis, `-1..=1` (right positive).
    pub move_x: i8,
    /// Forward axis, `-1..=1` (forward positive).
    pub move_z: i8,
    /// Facing, in 1/256 of a turn. 0 faces +Z.
    pub yaw: u8,
    /// Pitch, in 1/256 of a turn. 0 is level, 64 is straight up.
    pub pitch: u8,
    /// Bitfield of [`buttons`].
    pub buttons: u8,
}

impl InputFrame {
    /// True when a button is held.
    #[inline]
    pub fn held(self, button: u8) -> bool {
        self.buttons & button != 0
    }

    /// True when a button is held now and was not held in `previous`.
    ///
    /// Edges are derived from the previous frame rather than sent separately,
    /// because a lost edge packet would silently eat a jump.
    #[inline]
    pub fn pressed(self, button: u8, previous: u8) -> bool {
        self.held(button) && previous & button == 0
    }

    /// True when every field is at rest. Used to skip work for idle players.
    #[inline]
    pub fn is_idle(self) -> bool {
        self.move_x == 0 && self.move_z == 0 && self.buttons == 0
    }
}

/// The unit facing vector for a yaw byte.
///
/// Yaw 0 faces +Z and increases toward +X, matching the `sin_yaw`/`cos_yaw`
/// table in [`crate::fixed`]. Using the shared table rather than `f32::sin`
/// keeps this identical on every target.
#[inline]
pub fn forward_from_yaw(yaw: u8) -> Vec3 {
    Vec3::new(sin_yaw(yaw), Fixed::default(), cos_yaw(yaw))
}

/// The unit right vector for a yaw byte; `cross(up, forward)`.
#[inline]
pub fn right_from_yaw(yaw: u8) -> Vec3 {
    Vec3::new(cos_yaw(yaw), Fixed::default(), -sin_yaw(yaw))
}

/// The unit facing vector for a yaw and pitch pair.
///
/// Pitch is a signed byte's worth of a turn: `0` is level, `64` is straight up,
/// `-64` (i.e. `192`) is straight down. It is read as `i8` and clamped, so a
/// client cannot send a pitch past vertical and get a vector that points
/// backwards — the same reason [`wish_direction`] clamps its axes.
///
/// Reusing the yaw table for elevation keeps combat free of `asin`, and means
/// "straight up" is exactly `(0, 1, 0)` on every target rather than 0.9999998
/// of it.
#[inline]
pub fn look_direction(yaw: u8, pitch: u8) -> Vec3 {
    // Elevation in yaw units: +64 is a quarter turn up, -64 a quarter turn down.
    let elevation = (pitch as i8).clamp(-64, 64) as i32;
    let phase = if elevation >= 0 {
        elevation as u8
    } else {
        (256 + elevation) as u8
    };
    let c = cos_yaw(phase);
    let s = sin_yaw(phase);
    Vec3::new(sin_yaw(yaw) * c, s, cos_yaw(yaw) * c)
}

/// `1/sqrt(2)`, used to normalise a diagonal stick input.
///
/// Hard-coded rather than computed so a diagonal walk is exactly the same
/// length as a cardinal one on every target; the constant is the Q16.16
/// rounding of `0.70710678`.
pub const INV_SQRT2: Fixed = Fixed(46_341);

/// Convert a stick input and yaw into a unit-length horizontal direction.
///
/// Each axis is clamped to `-1..=1`, so a malformed or malicious frame cannot
/// request more than full speed. A diagonal is scaled by [`INV_SQRT2`] so
/// "forward-right" is not 41% faster than "forward".
pub fn wish_direction(move_x: i8, move_z: i8, yaw: u8) -> Vec3 {
    let mx = move_x.clamp(-1, 1);
    let mz = move_z.clamp(-1, 1);
    if mx == 0 && mz == 0 {
        return Vec3::ZERO;
    }

    let f = forward_from_yaw(yaw);
    let r = right_from_yaw(yaw);
    let dir = Vec3::new(
        f.x * Fixed::from_int(mz as i32) + r.x * Fixed::from_int(mx as i32),
        Fixed::default(),
        f.z * Fixed::from_int(mz as i32) + r.z * Fixed::from_int(mx as i32),
    );

    if mx != 0 && mz != 0 {
        dir.scale(INV_SQRT2)
    } else {
        dir
    }
}

/// Guard against a direction whose fixed-point length drifted past 1.0.
///
/// The sin/cos table is integer, so a "unit" vector built from it can be up to
/// about `1e-5` long. That is enough to break a strict `speed <= 4.5` assertion,
/// so callers that care about a hard cap scale by this first.
#[inline]
pub fn clamp_to_unit(v: Vec3) -> Vec3 {
    let len_sq = v.length_sq();
    if len_sq <= ONE * ONE {
        v
    } else {
        v.normalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinal_directions_match_the_yaw_convention() {
        // Yaw 0 faces +Z.
        let f = forward_from_yaw(0);
        assert_eq!(f.z, ONE);
        assert_eq!(f.x, Fixed::default());
        // Yaw 64 is a quarter turn, facing +X.
        let f = forward_from_yaw(64);
        assert!(f.x.to_f64() > 0.99, "yaw 64 should face +X, got {f:?}");
        assert!(f.z.to_f64().abs() < 0.01);
        // Yaw 128 faces -Z.
        let f = forward_from_yaw(128);
        assert!(f.z.to_f64() < -0.99);
    }

    #[test]
    fn right_is_perpendicular_to_forward() {
        for yaw in [0u8, 17, 64, 128, 200, 255] {
            let f = forward_from_yaw(yaw);
            let r = right_from_yaw(yaw);
            assert!(
                f.dot(r).to_f64().abs() < 1e-3,
                "forward and right are not perpendicular at yaw {yaw}"
            );
        }
    }

    #[test]
    fn wish_direction_is_unit_length() {
        for yaw in [0u8, 33, 64, 129, 200] {
            for (mx, mz) in [
                (0i8, 1i8),
                (1, 0),
                (-1, 0),
                (0, -1),
                (1, 1),
                (-1, 1),
                (1, -1),
            ] {
                let d = wish_direction(mx, mz, yaw);
                let len = d.length().to_f64();
                assert!(
                    (len - 1.0).abs() < 1e-3,
                    "direction ({mx}, {mz}) at yaw {yaw} has length {len}"
                );
            }
        }
    }

    #[test]
    fn no_input_is_zero_direction() {
        assert_eq!(wish_direction(0, 0, 42), Vec3::ZERO);
    }

    #[test]
    fn out_of_range_axes_are_clamped() {
        // A malformed frame asking for 5x forward must behave like 1x.
        let a = wish_direction(0, 5, 0);
        let b = wish_direction(0, 1, 0);
        assert_eq!(a, b);
    }

    #[test]
    fn diagonal_is_not_faster_than_cardinal() {
        let card = wish_direction(0, 1, 0).length().to_f64();
        let diag = wish_direction(1, 1, 0).length().to_f64();
        assert!(
            diag <= card + 1e-4,
            "diagonal {diag} exceeds cardinal {card}"
        );
    }

    #[test]
    fn button_edges_are_derived_not_sent() {
        let f = InputFrame {
            buttons: buttons::JUMP,
            ..Default::default()
        };
        assert!(f.held(buttons::JUMP));
        assert!(f.pressed(buttons::JUMP, 0));
        assert!(
            !f.pressed(buttons::JUMP, buttons::JUMP),
            "held is not pressed"
        );
        assert!(!f.held(buttons::FIRE));
    }

    #[test]
    fn idle_frame_is_detected() {
        assert!(InputFrame::default().is_idle());
        let f = InputFrame {
            move_z: 1,
            ..Default::default()
        };
        assert!(!f.is_idle());
    }

    #[test]
    fn look_direction_covers_the_cardinal_axes() {
        // Level, facing +Z.
        let d = look_direction(0, 0);
        assert!((d.z.to_f64() - 1.0).abs() < 1e-3, "{d:?}");
        assert!(d.y.to_f64().abs() < 1e-3);
        // Straight up.
        let d = look_direction(0, 64);
        assert!((d.y.to_f64() - 1.0).abs() < 1e-3, "{d:?}");
        // Straight down, encoded as -64.
        let d = look_direction(0, 192);
        assert!((d.y.to_f64() + 1.0).abs() < 1e-3, "{d:?}");
        // Level, facing +X.
        let d = look_direction(64, 0);
        assert!((d.x.to_f64() - 1.0).abs() < 1e-3, "{d:?}");
    }

    #[test]
    fn look_direction_is_unit_length_and_clamps_past_vertical() {
        for yaw in [0u8, 37, 64, 128, 200] {
            for pitch in [0u8, 32, 64, 96, 128, 160, 192, 255] {
                let d = look_direction(yaw, pitch);
                let len = d.length().to_f64();
                assert!(
                    (len - 1.0).abs() < 1e-3,
                    "yaw {yaw} pitch {pitch} gave length {len}"
                );
            }
        }
        // Anything past +64 is clamped to straight up rather than wrapping.
        assert_eq!(look_direction(0, 100), look_direction(0, 64));
        // 130 reads as -126 as an i8, which clamps to straight down.
        assert_eq!(look_direction(0, 130), look_direction(0, 192));
    }
}
