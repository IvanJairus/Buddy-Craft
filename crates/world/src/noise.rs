//! Integer value noise.
//!
//! Deliberately contains **no transcendental functions**. `sin`, `exp` and
//! `pow` are implemented differently by different libm builds, so a terrain
//! generator that used them would produce slightly different worlds on
//! `wasm32` and `aarch64` — and since the client regenerates terrain locally
//! instead of downloading it (plan §6.7), that difference would be a desync
//! with no network involved at all.
//!
//! Everything here is `+`, `-`, `*`, `>>` and `^` on integers, which are
//! bit-identical on every target.

use sim::fixed::{Fixed, ONE, ZERO};

/// A 32-bit integer hash of a lattice point.
#[inline]
pub fn hash2(x: i32, z: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343)
        ^ (z as u32).wrapping_mul(0xD816_3841)
        ^ seed.wrapping_mul(0xCB1A_B31F);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

/// Lattice value in `[0, 1)` as a `Fixed`.
#[inline]
fn lattice(x: i32, z: i32, seed: u32) -> Fixed {
    Fixed::from_raw((hash2(x, z, seed) >> 16) as i32)
}

/// Smoothstep `t*t*(3 - 2t)`, which removes the visible grid creases that plain
/// linear interpolation leaves in terrain.
#[inline]
fn smoothstep(t: Fixed) -> Fixed {
    let three = Fixed::from_int(3);
    let two = Fixed::from_int(2);
    t.saturating_mul(t)
        .saturating_mul(three.saturating_sub(two.saturating_mul(t)))
}

/// Bilinearly interpolated value noise in `[0, 1)`.
pub fn value_noise(x: Fixed, z: Fixed, seed: u32) -> Fixed {
    let fx = x.floor_int();
    let fz = z.floor_int();
    let tx = x.saturating_sub(Fixed::from_int(fx));
    let tz = z.saturating_sub(Fixed::from_int(fz));

    let v00 = lattice(fx, fz, seed);
    let v10 = lattice(fx + 1, fz, seed);
    let v01 = lattice(fx, fz + 1, seed);
    let v11 = lattice(fx + 1, fz + 1, seed);

    let sx = smoothstep(tx);
    let sz = smoothstep(tz);

    let a = v00.lerp(v10, sx);
    let b = v01.lerp(v11, sx);
    a.lerp(b, sz)
}

/// Fractal Brownian motion: `octaves` layers of value noise, each at double the
/// frequency and half the amplitude. Result is in `[0, 1)`.
pub fn fbm(x: Fixed, z: Fixed, seed: u32, octaves: u32) -> Fixed {
    let mut sum = ZERO;
    let mut amp = ONE;
    let mut total = ZERO;
    let mut freq = ONE;
    for o in 0..octaves {
        sum += value_noise(
            x.saturating_mul(freq),
            z.saturating_mul(freq),
            seed.wrapping_add(o.wrapping_mul(0x9E37_79B9)),
        )
        .saturating_mul(amp);
        total += amp;
        amp = Fixed::from_raw(amp.to_raw() / 2);
        freq = Fixed::from_raw(freq.to_raw() * 2);
    }
    if total.to_raw() == 0 {
        ZERO
    } else {
        sum.saturating_div(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_avalanches() {
        assert_eq!(hash2(3, 4, 7), hash2(3, 4, 7));
        assert_ne!(hash2(3, 4, 7), hash2(4, 3, 7));
        assert_ne!(hash2(3, 4, 7), hash2(3, 4, 8));

        // Adjacent inputs should not produce adjacent outputs.
        let a = hash2(0, 0, 0);
        let b = hash2(1, 0, 0);
        assert!(a.abs_diff(b) > 1000, "hash has no avalanche");
    }

    #[test]
    fn noise_stays_in_unit_range() {
        for i in -50..50 {
            for j in -50..50 {
                let x = Fixed::from_f32(i as f32 * 0.37);
                let z = Fixed::from_f32(j as f32 * 0.41);
                let n = value_noise(x, z, 11);
                assert!(n >= ZERO && n < ONE, "noise out of range: {n:?}");
            }
        }
    }

    #[test]
    fn noise_is_continuous() {
        // A small step in input must produce a small step in output.
        let seed = 5;
        let mut prev = value_noise(ZERO, ZERO, seed);
        for k in 1..200 {
            let x = Fixed::from_f32(k as f32 * 0.01);
            let cur = value_noise(x, ZERO, seed);
            let delta = (cur - prev).abs().to_f64();
            assert!(delta < 0.05, "noise jumped {delta} at step {k}");
            prev = cur;
        }
    }

    #[test]
    fn noise_hits_lattice_values() {
        // At integer coordinates the interpolation weight is zero, so the
        // result is exactly the lattice value.
        for x in 0..5 {
            for z in 0..5 {
                let n = value_noise(Fixed::from_int(x), Fixed::from_int(z), 3);
                assert_eq!(n, lattice(x, z, 3));
            }
        }
    }

    #[test]
    fn fbm_is_deterministic_and_bounded() {
        let a = fbm(Fixed::from_int(7), Fixed::from_int(9), 42, 4);
        let b = fbm(Fixed::from_int(7), Fixed::from_int(9), 42, 4);
        assert_eq!(a, b);
        assert!(a >= ZERO && a <= ONE);
    }

    #[test]
    fn fbm_varies_across_the_map() {
        let mut min = ONE;
        let mut max = ZERO;
        for k in 0..200 {
            let x = Fixed::from_int(k * 3);
            let v = fbm(x, ZERO, 1, 4);
            min = min.min(v);
            max = max.max(v);
        }
        assert!(
            max.saturating_sub(min).to_f64() > 0.1,
            "terrain is too flat to look like terrain"
        );
    }
}
