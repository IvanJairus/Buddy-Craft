//! Tick clock. Wall-clock time never enters `sim`; a room advances by ticks.

use serde::{Deserialize, Serialize};

/// Ticks per second the authoritative simulation runs at (plan §6.2).
pub const TICK_HZ: u32 = 20;
/// Milliseconds per tick.
pub const TICK_MS: u32 = 1000 / TICK_HZ;

/// A simulation tick counter.
///
/// Wrapping is intentional and matches the protocol's `u32` tick field; a room
/// would have to run for 6.8 years at 20 Hz to wrap, so no room will ever see
/// it, but the behaviour is documented rather than accidental.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug, Serialize, Deserialize,
)]
#[repr(transparent)]
pub struct Tick(pub u32);

impl Tick {
    /// The tick at match start.
    pub const ZERO: Tick = Tick(0);

    /// Advance by `n` ticks, wrapping.
    #[inline]
    pub const fn advance(self, n: u32) -> Tick {
        Tick(self.0.wrapping_add(n))
    }

    /// The next tick.
    #[inline]
    pub const fn next(self) -> Tick {
        self.advance(1)
    }

    /// Ticks elapsed from `earlier` to `self`.
    #[inline]
    pub const fn since(self, earlier: Tick) -> u32 {
        self.0.wrapping_sub(earlier.0)
    }

    /// Convert a duration in seconds to a whole number of ticks, rounding down.
    #[inline]
    pub const fn from_secs(secs: u32) -> Tick {
        Tick(secs * TICK_HZ)
    }

    /// Convert a duration in milliseconds to ticks, rounding down.
    #[inline]
    pub const fn from_millis(ms: u32) -> Tick {
        Tick(ms / TICK_MS)
    }

    /// This tick expressed in whole seconds.
    #[inline]
    pub const fn as_secs(self) -> u32 {
        self.0 / TICK_HZ
    }

    /// True if `self` is at or after `other`.
    #[inline]
    pub const fn at_or_after(self, other: Tick) -> bool {
        self.0 >= other.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_constants_match_plan() {
        assert_eq!(TICK_HZ, 20);
        assert_eq!(TICK_MS, 50);
    }

    #[test]
    fn seconds_roundtrip() {
        assert_eq!(Tick::from_secs(20).0, 400);
        assert_eq!(Tick::from_secs(20).as_secs(), 20);
        assert_eq!(Tick::from_millis(500).0, 10);
    }

    #[test]
    fn wrapping_is_defined() {
        let t = Tick(u32::MAX);
        assert_eq!(t.next().0, 0);
        assert_eq!(Tick(5).since(Tick(u32::MAX)), 6);
    }
}
