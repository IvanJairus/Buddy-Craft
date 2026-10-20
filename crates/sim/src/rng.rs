//! Deterministic pseudo-random numbers.
//!
//! `xoshiro256**` seeded through `splitmix64`. Both are pure integer
//! algorithms with no platform-dependent behaviour, which is what makes them
//! safe to use in authoritative state: the same seed produces the same stream
//! on `wasm32`, `x86_64` and `aarch64`.
//!
//! Randomness is split into **independent streams** ([`RngStreams`]) rather than
//! one global generator. If combat consumed draws from the same stream as
//! worldgen, adding one arrow would change the terrain generated ten seconds
//! later, and every golden hash in the determinism suite would churn for a
//! reason nobody could explain.

use crate::fixed::Fixed;

/// A `xoshiro256**` generator.
#[derive(Clone, Debug)]
pub struct Xoshiro256 {
    s: [u64; 4],
}

#[inline]
const fn rotl(x: u64, k: u32) -> u64 {
    (x << k) | (x >> (64 - k))
}

/// One `splitmix64` step, used both for seeding and as a standalone generator.
#[inline]
pub const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Xoshiro256 {
    /// Seed the generator from a single `u64` via `splitmix64`.
    ///
    /// Seeding with all-zero state is the one input that makes xoshiro produce
    /// all zeros forever; `splitmix64` never returns zero for the first draw in
    /// practice, and the assertion below makes the failure mode explicit rather
    /// than silent.
    pub fn seed_from_u64(seed: u64) -> Self {
        let mut sm = seed;
        let s = [
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
            splitmix64(&mut sm),
        ];
        debug_assert!(
            s != [0, 0, 0, 0],
            "xoshiro must not be seeded with all zeros"
        );
        Xoshiro256 { s }
    }

    /// Seed from two words, for callers that already have a 64-bit room seed
    /// plus a stream tag.
    pub fn seed_from_parts(seed: u64, tag: u64) -> Self {
        Self::seed_from_u64(seed ^ tag.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    /// The next 64 random bits.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let result = rotl(self.s[1].wrapping_mul(5), 7).wrapping_mul(9);
        let t = self.s[1] << 17;

        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = rotl(self.s[3], 45);

        result
    }

    /// The next 32 random bits.
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform in `[0, n)`. Returns 0 when `n == 0`.
    ///
    /// Lemire's multiply-shift with rejection: unbiased, branch-light, and
    /// integer-only so it is identical on every target.
    pub fn next_below(&mut self, n: u32) -> u32 {
        if n <= 1 {
            return 0;
        }
        let mut x = self.next_u64();
        let mut m = (x as u128) * (n as u128);
        let mut low = m as u64;
        if low < n as u64 {
            let threshold = (n as u64).wrapping_neg() % (n as u64);
            while low < threshold {
                x = self.next_u64();
                m = (x as u128) * (n as u128);
                low = m as u64;
            }
        }
        (m >> 64) as u32
    }

    /// Uniform in `[lo, hi]` inclusive.
    pub fn next_range_i32(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        let span = (hi as i64 - lo as i64 + 1) as u64;
        let r = self.next_u64() % span;
        (lo as i64 + r as i64) as i32
    }

    /// Uniform in `[0.0, 1.0)` as a `Fixed`.
    #[inline]
    pub fn next_fixed(&mut self) -> Fixed {
        Fixed::from_raw((self.next_u32() >> 16) as i32)
    }

    /// Uniform in `[lo, hi)` as a `Fixed`.
    #[inline]
    pub fn next_fixed_range(&mut self, lo: Fixed, hi: Fixed) -> Fixed {
        lo.saturating_add(hi.saturating_sub(lo).saturating_mul(self.next_fixed()))
    }

    /// True with probability `numerator / denominator`.
    pub fn chance(&mut self, numerator: u32, denominator: u32) -> bool {
        if denominator == 0 {
            return false;
        }
        self.next_below(denominator) < numerator
    }
}

/// Independent random streams for the subsystems that need randomness.
///
/// Each is seeded from the room seed and a distinct tag, so drawing from one
/// never perturbs another (plan §5.5).
#[derive(Clone, Debug)]
pub struct RngStreams {
    /// Terrain and decorative variation.
    pub world: Xoshiro256,
    /// Hit rolls, crits, knockback jitter.
    pub combat: Xoshiro256,
    /// Crate scatter on death and on landing.
    pub loot: Xoshiro256,
    /// Airdrop site selection and drop composition (plan §4.4.2).
    pub airdrop: Xoshiro256,
}

impl RngStreams {
    /// Build all four streams from a room seed.
    pub fn new(seed: u64) -> Self {
        Self {
            world: Xoshiro256::seed_from_parts(seed, 0x01),
            combat: Xoshiro256::seed_from_parts(seed, 0x02),
            loot: Xoshiro256::seed_from_parts(seed, 0x03),
            airdrop: Xoshiro256::seed_from_parts(seed, 0x04),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression lock. If this value changes, every golden state hash in the
    /// determinism suite (T-015) is invalidated — which is a reviewable event,
    /// not a harmless refactor.
    const FIRST_DRAW_SEED_C0FFEE: u64 = 1_301_146_284_323_022_160;

    #[test]
    fn sequence_is_stable() {
        let mut r = Xoshiro256::seed_from_u64(0xC0FFEE);
        assert_eq!(r.next_u64(), FIRST_DRAW_SEED_C0FFEE);
    }

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Xoshiro256::seed_from_u64(42);
        let mut b = Xoshiro256::seed_from_u64(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Xoshiro256::seed_from_u64(1);
        let mut b = Xoshiro256::seed_from_u64(2);
        let mut same = 0;
        for _ in 0..100 {
            if a.next_u64() == b.next_u64() {
                same += 1;
            }
        }
        assert_eq!(same, 0, "two seeds produced identical draws");
    }

    #[test]
    fn streams_are_independent() {
        // Draining the combat stream must not change the airdrop stream.
        let mut a = RngStreams::new(7);
        let mut b = RngStreams::new(7);
        for _ in 0..500 {
            let _ = a.combat.next_u64();
        }
        assert_eq!(a.airdrop.next_u64(), b.airdrop.next_u64());
        assert_eq!(a.world.next_u64(), b.world.next_u64());
    }

    #[test]
    fn next_below_stays_in_range() {
        let mut r = Xoshiro256::seed_from_u64(99);
        for n in [1u32, 2, 3, 6, 7, 100, 1_000_000] {
            for _ in 0..2000 {
                assert!(r.next_below(n) < n, "next_below({n}) out of range");
            }
        }
        assert_eq!(r.next_below(0), 0);
    }

    #[test]
    fn next_below_is_roughly_uniform() {
        let mut r = Xoshiro256::seed_from_u64(1234);
        let mut buckets = [0u32; 6];
        for _ in 0..60_000 {
            buckets[r.next_below(6) as usize] += 1;
        }
        // 10_000 expected each; allow 8% drift before failing.
        for (i, count) in buckets.iter().enumerate() {
            assert!(
                (*count as i64 - 10_000).abs() < 800,
                "bucket {i} has {count}, expected ~10000"
            );
        }
    }

    #[test]
    fn fixed_is_in_unit_interval() {
        let mut r = Xoshiro256::seed_from_u64(5);
        for _ in 0..10_000 {
            let f = r.next_fixed();
            assert!(f.to_raw() >= 0 && f.to_raw() < 65_536);
        }
    }

    #[test]
    fn fixed_range_respects_bounds() {
        let mut r = Xoshiro256::seed_from_u64(6);
        let lo = Fixed::from_int(-5);
        let hi = Fixed::from_int(5);
        for _ in 0..10_000 {
            let f = r.next_fixed_range(lo, hi);
            assert!(f >= lo && f < hi);
        }
    }

    #[test]
    fn chance_extremes() {
        let mut r = Xoshiro256::seed_from_u64(8);
        for _ in 0..100 {
            assert!(!r.chance(0, 10));
            assert!(r.chance(10, 10));
            assert!(!r.chance(1, 0));
        }
    }

    #[test]
    fn splitmix64_matches_reference_definition() {
        // Recomputing the first draw from the published constants in the
        // algorithm, independently of the function under test.
        let mut state = 0u64;
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let want = z ^ (z >> 31);

        let mut s = 0u64;
        assert_eq!(splitmix64(&mut s), want);
    }
}
