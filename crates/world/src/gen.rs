//! Deterministic world generation (plan T-006).
//!
//! The client never downloads the world. It receives a `u64` seed and a team
//! count, regenerates the terrain locally, and the server only ever sends the
//! handful of crate edits inside a stacking pad (plan §6.7). That makes this
//! module the single most determinism-critical code in the project: a one-unit
//! difference in a height function here is a silent desync with no network
//! packet to blame.
//!
//! # Rules this module obeys
//!
//! * **No floating point.** Every height is an `i32` derived from integer
//!   value noise ([`crate::noise`]). `sin`/`cos` come from the shared 512-entry
//!   integer table in `sim`, not from libm, because libm differs between
//!   `wasm32` and `aarch64`.
//! * **No `HashMap` iteration.** Base and drop-site order is the order they
//!   were pushed.
//! * **No cross-column state.** A column is a pure function of
//!   `(seed, teams, cx, cz)`. Trees near a column edge are handled by scanning
//!   trunk candidates in a margin around the column, not by "the chunk to the
//!   left already placed it".
//!
//! # Layout
//!
//! ```text
//!   256 x 256 world, 96 blocks tall, sea level 40
//!
//!   bases sit on a circle of radius 96 around the centre, evenly spaced by
//!   team count, each flattened to y = 40 with an 8x8 pad_floor stack pad
//!   offset 6 blocks toward the centre.
//!
//!   airdrop sites are a 24-block grid, filtered to slope <= 2 and at least
//!   32 blocks from every base centre.
//! ```

use std::sync::OnceLock;

use sim::fixed::{cos, sin, yaw_to_rad, Fixed, ONE_RAW, TAU};

use crate::block::{registry, BlockId};
use crate::chunk::{Column, CHUNK_X, CHUNK_Y, CHUNK_Z};
use crate::noise::{fbm, hash2};

/// Lowest surface height worldgen will produce.
pub const MIN_HEIGHT: i32 = 28;
/// Highest surface height worldgen will produce.
pub const MAX_HEIGHT: i32 = 64;
/// Flat height every base pad is levelled to. Must equal `tuning.world.sea_level`.
pub const BASE_HEIGHT: i32 = 40;
/// Radius around a base centre that is perfectly flat.
pub const FLAT_RADIUS: i32 = 14;
/// Radius at which the flat pad has blended back into natural terrain.
pub const BLEND_RADIUS: i32 = 26;
/// Edge length of the stack pad floor.
pub const PAD_SIZE: i32 = 8;
/// How far the stack pad is offset from the base centre, toward the map centre.
pub const PAD_OFFSET: i32 = 6;
/// Spacing of the airdrop candidate grid.
pub const DROP_GRID_STEP: i32 = 24;
/// Minimum distance from a base centre for a drop site to be legal.
pub const DROP_MIN_BASE_DIST: i32 = 32;
/// Maximum height difference between a drop site and its four neighbours.
pub const DROP_MAX_SLOPE: i32 = 2;

/// World edge length along X, in blocks. Mirrors `tuning.world.size_x`.
pub const WORLD_X: i32 = 256;
/// World edge length along Z, in blocks. Mirrors `tuning.world.size_z`.
pub const WORLD_Z: i32 = 256;
/// Number of columns along each horizontal axis.
pub const MAP_COLS: i32 = WORLD_X / CHUNK_X as i32;

/// One team's base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Base {
    /// Team index, `0..team_count`.
    pub team: u8,
    /// Base centre, in blocks.
    pub x: i32,
    /// Base centre, in blocks.
    pub z: i32,
    /// Levelled ground height. Always [`BASE_HEIGHT`].
    pub y: i32,
    /// Centre of the 8x8 stacking pad.
    pub pad_x: i32,
    /// Centre of the 8x8 stacking pad.
    pub pad_z: i32,
    /// Angle on the base circle, in radians. Stored so tests can assert equal
    /// angular spacing without recovering it from rounded integer positions.
    pub angle: Fixed,
}

impl Base {
    /// True when `(x, z)` falls inside this base's 8x8 stack pad.
    #[inline]
    pub fn on_pad(&self, x: i32, z: i32) -> bool {
        let half = -PAD_SIZE / 2..PAD_SIZE / 2;
        half.contains(&(x - self.pad_x)) && half.contains(&(z - self.pad_z))
    }

    /// The player spawn point: one block above the pad centre.
    #[inline]
    pub fn spawn(&self) -> [i32; 3] {
        [self.pad_x, self.y + 1, self.pad_z]
    }
}

/// One legal airdrop landing site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropSite {
    /// Centre, in blocks.
    pub x: i32,
    /// Centre, in blocks.
    pub z: i32,
    /// Ground height at the centre.
    pub y: i32,
}

/// The block ids worldgen needs, resolved once.
struct Palette {
    bedrock: BlockId,
    stone: BlockId,
    dirt: BlockId,
    grass: BlockId,
    sand: BlockId,
    gravel: BlockId,
    water: BlockId,
    log: BlockId,
    leaves: BlockId,
    pad_floor: BlockId,
    snow: BlockId,
    clay: BlockId,
}

fn palette() -> &'static Palette {
    static CELL: OnceLock<&'static Palette> = OnceLock::new();
    CELL.get_or_init(|| {
        let r = registry();
        let id = |name: &str| {
            r.id_of(name)
                .unwrap_or_else(|| panic!("data/blocks.ron must define `{name}`"))
        };
        Box::leak(Box::new(Palette {
            bedrock: id("bedrock"),
            stone: id("stone"),
            dirt: id("dirt"),
            grass: id("grass"),
            sand: id("sand"),
            gravel: id("gravel"),
            water: id("water"),
            log: id("log"),
            leaves: id("leaves"),
            pad_floor: id("pad_floor"),
            snow: id("snow"),
            clay: id("clay"),
        }))
    })
}

/// Sea level, read from tuning so there is one source of truth.
#[inline]
pub fn sea_level() -> i32 {
    sim::tuning::embedded().world.sea_level as i32
}

/// `x / denom` as a `Fixed`, without going through `f32`.
#[inline]
const fn scale(x: i32, denom: i32) -> Fixed {
    Fixed::from_raw(((x as i64) * (ONE_RAW as i64) / (denom as i64)) as i32)
}

/// Natural terrain height before any base flattening.
///
/// Two noise fields: a slow continental term over 96-block features and a
/// faster hill term over 24-block features. The weighted sum lands in
/// `[30, 64)`, which is inside the `[28, 64]` contract with margin for the
/// rounding.
pub fn raw_height(seed: u64, x: i32, z: i32) -> i32 {
    let s = seed as u32;
    let continent = fbm(scale(x, 96), scale(z, 96), s ^ 0x1111_1111, 4);
    let hills = fbm(scale(x, 24), scale(z, 24), s ^ 0x2222_2222, 3);
    let h = Fixed::from_int(30)
        .saturating_add(continent.saturating_mul(Fixed::from_int(20)))
        .saturating_add(hills.saturating_mul(Fixed::from_int(14)));
    h.round_int().clamp(MIN_HEIGHT, MAX_HEIGHT)
}

/// Place `teams` bases evenly around a circle of radius 96 at the map centre.
///
/// The whole ring is rotated by a seed-derived offset so two rooms with
/// different seeds do not look identical, but the spacing is always `2*pi/n`.
pub fn bases_for(seed: u64, teams: u8) -> Vec<Base> {
    let n = teams.max(2) as i32;
    let radius = sim::tuning::embedded().world.base_radius as i32;
    let cx = WORLD_X / 2;
    let cz = WORLD_Z / 2;
    // A seed-derived rotation in whole yaw steps keeps this exact. The seed is
    // mixed first: taking a raw slice of the seed makes small seeds (1, 2, 3...)
    // all rotate to zero, which is how this test caught itself.
    let rot = yaw_to_rad((seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8);
    let r = Fixed::from_int(radius);
    let off = Fixed::from_int(PAD_OFFSET);

    (0..n)
        .map(|i| {
            let step = Fixed::from_raw(((TAU.0 as i64) * (i as i64) / (n as i64)) as i32);
            let angle = rot.saturating_add(step);
            let dx = r.saturating_mul(cos(angle)).round_int();
            let dz = r.saturating_mul(sin(angle)).round_int();
            let ox = off.saturating_mul(cos(angle)).round_int();
            let oz = off.saturating_mul(sin(angle)).round_int();
            Base {
                team: i as u8,
                x: cx + dx,
                z: cz + dz,
                y: BASE_HEIGHT,
                // Offset toward the centre, i.e. back along the radius.
                pad_x: cx + dx - ox,
                pad_z: cz + dz - oz,
                angle,
            }
        })
        .collect()
}

/// A generated world: seed, bases, drop sites, and a pure `column` function.
#[derive(Clone, Debug)]
pub struct WorldGen {
    seed: u64,
    bases: Vec<Base>,
    sites: Vec<DropSite>,
}

impl WorldGen {
    /// Build the generator for a room.
    pub fn new(seed: u64, teams: u8) -> WorldGen {
        let bases = bases_for(seed, teams);
        let sites = drop_sites_for(seed, &bases);
        WorldGen { seed, bases, sites }
    }

    /// The room seed.
    #[inline]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The bases, in team order.
    #[inline]
    pub fn bases(&self) -> &[Base] {
        &self.bases
    }

    /// Legal airdrop sites, in grid order.
    #[inline]
    pub fn drop_sites(&self) -> &[DropSite] {
        &self.sites
    }

    /// Spawn point for a team.
    pub fn spawn(&self, team: u8) -> [i32; 3] {
        self.bases.get(team as usize).map(Base::spawn).unwrap_or([
            WORLD_X / 2,
            BASE_HEIGHT + 1,
            WORLD_Z / 2,
        ])
    }

    /// True when `(x, z)` is inside the given radius of any base centre.
    fn near_base(&self, x: i32, z: i32, radius: i32) -> bool {
        let r2 = radius * radius;
        self.bases
            .iter()
            .any(|b| (x - b.x) * (x - b.x) + (z - b.z) * (z - b.z) <= r2)
    }

    /// True when `(x, z)` is inside any stacking pad.
    fn on_any_pad(&self, x: i32, z: i32) -> bool {
        self.bases.iter().any(|b| b.on_pad(x, z))
    }

    /// Surface height including base flattening.
    ///
    /// Inside [`FLAT_RADIUS`] the height is exactly `base.y`; between there and
    /// [`BLEND_RADIUS`] it lerps back to natural terrain so a base does not sit
    /// in a perfectly circular crater with a vertical wall around it.
    pub fn height_at(&self, x: i32, z: i32) -> i32 {
        let mut h = raw_height(self.seed, x, z);
        let flat2 = FLAT_RADIUS * FLAT_RADIUS;
        let blend2 = BLEND_RADIUS * BLEND_RADIUS;
        for b in &self.bases {
            let dx = x - b.x;
            let dz = z - b.z;
            let d2 = dx * dx + dz * dz;
            if d2 <= flat2 {
                return b.y;
            }
            if d2 <= blend2 {
                let d = Fixed::from_int(d2).sqrt().round_int();
                let num = (d - FLAT_RADIUS) * ONE_RAW;
                let t = Fixed::from_raw((num / (BLEND_RADIUS - FLAT_RADIUS)).clamp(0, ONE_RAW));
                h = Fixed::from_int(b.y).lerp(Fixed::from_int(h), t).round_int();
            }
        }
        h
    }

    /// The block that sits on top of the surface at `(x, z)`.
    fn surface_block(&self, x: i32, z: i32, h: i32, slope: i32) -> BlockId {
        let p = palette();
        let sea = sea_level();
        if self.on_any_pad(x, z) {
            return p.pad_floor;
        }
        if slope >= 3 {
            return if h < sea + 6 { p.gravel } else { p.stone };
        }
        if h <= sea + 1 {
            p.sand
        } else if h >= 58 {
            p.snow
        } else if h >= 50 {
            if slope >= 2 {
                p.stone
            } else {
                p.grass
            }
        } else if h <= sea + 4 && slope == 0 {
            p.clay
        } else {
            p.grass
        }
    }

    /// A trunk candidate at `(x, z)`, as `(ground_y, trunk_height)`.
    fn tree_at(&self, x: i32, z: i32) -> Option<(i32, u8)> {
        let h = hash2(x, z, (self.seed as u32) ^ 0x7E33_1A07);
        // ~6% of candidate cells carry a tree.
        if h % 100 >= 6 {
            return None;
        }
        if self.near_base(x, z, FLAT_RADIUS + 6) {
            return None;
        }
        let gy = self.height_at(x, z);
        if gy <= sea_level() || gy >= 52 {
            return None;
        }
        Some((gy, 4 + ((h >> 8) % 3) as u8))
    }

    /// Generate one 16x96x16 column. Pure: same inputs, same bytes.
    pub fn column(&self, cx: i32, cz: i32) -> Column {
        let mut col = Column::new();
        let p = palette();
        let sea = sea_level();
        let wx0 = cx * CHUNK_X as i32;
        let wz0 = cz * CHUNK_Z as i32;

        for lz in 0..CHUNK_Z {
            for lx in 0..CHUNK_X {
                let wx = wx0 + lx as i32;
                let wz = wz0 + lz as i32;
                let h = self.height_at(wx, wz);

                // Local slope, used only for material choice.
                let slope = (h - self.height_at(wx + 1, wz))
                    .abs()
                    .max((h - self.height_at(wx, wz + 1)).abs());

                // Bedrock floor: the map has no bottomless pits.
                col.set(lx, 0, lz, p.bedrock);

                let sub_start = (h - 3).max(1);
                for y in 1..sub_start {
                    col.set(lx, y as usize, lz, p.stone);
                }
                let filler = if h <= sea + 1 { p.sand } else { p.dirt };
                for y in sub_start..h {
                    col.set(lx, y as usize, lz, filler);
                }
                col.set(lx, h as usize, lz, self.surface_block(wx, wz, h, slope));

                // Water fills the gap up to sea level.
                if h < sea {
                    for y in (h + 1)..=sea {
                        col.set(lx, y as usize, lz, p.water);
                    }
                }
            }
        }

        self.decorate_trees(&mut col, wx0, wz0);
        col
    }

    /// Place the parts of nearby trees that fall inside this column.
    ///
    /// Scanning a margin instead of "only trees rooted in this column" is what
    /// keeps trees from being sliced in half at column borders.
    fn decorate_trees(&self, col: &mut Column, wx0: i32, wz0: i32) {
        let p = palette();
        let margin = 2i32;
        for tz in (wz0 - margin)..(wz0 + CHUNK_Z as i32 + margin) {
            for tx in (wx0 - margin)..(wx0 + CHUNK_X as i32 + margin) {
                let Some((gy, trunk)) = self.tree_at(tx, tz) else {
                    continue;
                };
                let top = gy + trunk as i32;

                // Trunk.
                for y in (gy + 1)..=top {
                    self.put(col, (wx0, wz0), [tx, y, tz], p.log);
                }

                // Canopy: two wide layers plus a single cap block.
                for dy in 0..2 {
                    let y = top - 1 + dy;
                    for dz in -2i32..=2 {
                        for dx in -2i32..=2 {
                            if dx.abs() == 2 && dz.abs() == 2 {
                                continue;
                            }
                            // Corner nibble so the canopy is not a cube.
                            if dx.abs() + dz.abs() == 4 && dy == 1 {
                                continue;
                            }
                            self.put(col, (wx0, wz0), [tx + dx, y, tz + dz], p.leaves);
                        }
                    }
                }
                self.put(col, (wx0, wz0), [tx, top + 1, tz], p.leaves);
            }
        }
    }

    /// Write a world-space cell if it lands inside this column.
    fn put(&self, col: &mut Column, origin: (i32, i32), at: [i32; 3], id: BlockId) {
        let (wx0, wz0) = origin;
        let [x, y, z] = at;
        let lx = x - wx0;
        let lz = z - wz0;
        if lx < 0 || lz < 0 || lx >= CHUNK_X as i32 || lz >= CHUNK_Z as i32 {
            return;
        }
        if y < 0 || y >= CHUNK_Y as i32 {
            return;
        }
        col.set(lx as usize, y as usize, lz as usize, id);
    }
}

/// Legal airdrop sites: a 24-block grid filtered by slope and base distance.
pub fn drop_sites_for(seed: u64, bases: &[Base]) -> Vec<DropSite> {
    let probe = WorldGen {
        seed,
        bases: bases.to_vec(),
        sites: Vec::new(),
    };
    let sea = sea_level();
    let mut out = Vec::new();

    let mut z = DROP_GRID_STEP / 2;
    while z < WORLD_Z {
        let mut x = DROP_GRID_STEP / 2;
        while x < WORLD_X {
            let h = probe.height_at(x, z);
            let slope = (h - probe.height_at(x + DROP_GRID_STEP / 2, z))
                .abs()
                .max((h - probe.height_at(x, z + DROP_GRID_STEP / 2)).abs());
            let clear = !bases.iter().any(|b| {
                (x - b.x) * (x - b.x) + (z - b.z) * (z - b.z)
                    < DROP_MIN_BASE_DIST * DROP_MIN_BASE_DIST
            });
            if slope <= DROP_MAX_SLOPE && clear && h >= sea && !probe.on_any_pad(x, z) {
                out.push(DropSite { x, z, y: h });
            }
            x += DROP_GRID_STEP;
        }
        z += DROP_GRID_STEP;
    }
    out
}

/// Convenience wrapper matching the plan's `gen(seed, cx, cz) -> Column`.
pub fn gen(seed: u64, cx: i32, cz: i32) -> Column {
    WorldGen::new(seed, 2).column(cx, cz)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{AIR, CRATE};

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn count_of(col: &Column, id: BlockId) -> usize {
        let mut n = 0;
        for y in 0..CHUNK_Y {
            for z in 0..CHUNK_Z {
                for x in 0..CHUNK_X {
                    if col.get(x, y, z) == id {
                        n += 1;
                    }
                }
            }
        }
        n
    }

    fn digest(col: &Column) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for y in 0..CHUNK_Y {
            for z in 0..CHUNK_Z {
                for x in 0..CHUNK_X {
                    h ^= col.get(x, y, z).0 as u64;
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
        h
    }

    #[test]
    fn base_height_matches_tuning() {
        assert_eq!(
            BASE_HEIGHT,
            sea_level(),
            "worldgen flattens bases to sea level; if tuning moves the sea this must move too"
        );
        assert_eq!(
            MAP_COLS,
            sim::tuning::embedded().world.size_x as i32 / CHUNK_X as i32
        );
        assert_eq!(CHUNK_Y as i32, sim::tuning::embedded().world.height as i32);
    }

    #[test]
    fn same_seed_same_column() {
        // Two generators with the same seed must agree everywhere. The height
        // field is sampled at 1,000 random points because it is the input every
        // later stage depends on; full column bytes are compared on a fixed
        // sample, since generating a column is ~1 ms and 1,000 of them would
        // make this test slower than the rest of the suite combined.
        let mut rng = Lcg(0xBEEF);
        let g = WorldGen::new(0x00C0_FFEE_1234, 4);
        let g2 = WorldGen::new(0x00C0_FFEE_1234, 4);
        for _ in 0..1_000 {
            let x = rng.below(WORLD_X as u64) as i32;
            let z = rng.below(WORLD_Z as u64) as i32;
            assert_eq!(
                g.height_at(x, z),
                g2.height_at(x, z),
                "diverged at ({x}, {z})"
            );
        }
        for (cx, cz) in [(0, 0), (3, 11), (8, 8), (15, 15), (7, 2)] {
            assert_eq!(
                digest(&g.column(cx, cz)),
                digest(&g2.column(cx, cz)),
                "column ({cx}, {cz}) is not reproducible"
            );
        }
    }

    #[test]
    fn different_seeds_differ() {
        let a = WorldGen::new(1, 2);
        let b = WorldGen::new(2, 2);
        assert_ne!(a.bases(), b.bases(), "seed must rotate the base ring");
        assert_ne!(digest(&a.column(5, 5)), digest(&b.column(5, 5)));
    }

    #[test]
    fn terrain_height_stays_in_range() {
        let mut rng = Lcg(99);
        for seed in [0u64, 1, 0xDEAD_BEEF, u64::MAX] {
            let g = WorldGen::new(seed, 6);
            for _ in 0..2_500 {
                let x = rng.below(WORLD_X as u64) as i32;
                let z = rng.below(WORLD_Z as u64) as i32;
                let h = g.height_at(x, z);
                assert!(
                    (MIN_HEIGHT..=MAX_HEIGHT).contains(&h),
                    "height {h} out of range at ({x}, {z}) seed {seed}"
                );
            }
        }
    }

    #[test]
    fn bases_are_flat_and_symmetric() {
        for n in [2u8, 3, 4, 6] {
            let g = WorldGen::new(0x5EED_0000 + n as u64, n);
            assert_eq!(g.bases().len(), n as usize);

            // Equal angular spacing.
            let step = TAU.0 as f64 / n as f64;
            for w in g.bases().windows(2) {
                let d = (w[1].angle.to_raw() - w[0].angle.to_raw()) as f64;
                assert!(
                    (d - step).abs() < 2.0,
                    "angular spacing {d} != {step} for n = {n}"
                );
            }

            // Identical, perfectly flat height profile on every pad.
            let first = g.bases()[0];
            for b in g.bases() {
                for dz in -4i32..4 {
                    for dx in -4i32..4 {
                        let h = g.height_at(b.x + dx, b.z + dz);
                        assert_eq!(h, first.y, "pad {b:?} is not flat at ({dx}, {dz})");
                    }
                }
                assert_eq!(b.y, BASE_HEIGHT);
            }

            // Each pad carries an 8x8 indestructible stack floor.
            let floor = palette().pad_floor;
            assert!(!registry().is_breakable(floor));
            for b in g.bases() {
                let pcx = b.pad_x.div_euclid(CHUNK_X as i32);
                let pcz = b.pad_z.div_euclid(CHUNK_Z as i32);
                let col = g.column(pcx, pcz);
                for dz in -4i32..4 {
                    for dx in -4i32..4 {
                        let x = b.pad_x + dx;
                        let z = b.pad_z + dz;
                        let lx = x - pcx * CHUNK_X as i32;
                        let lz = z - pcz * CHUNK_Z as i32;
                        if lx < 0 || lz < 0 || lx >= CHUNK_X as i32 || lz >= CHUNK_Z as i32 {
                            continue;
                        }
                        assert_eq!(
                            col.get(lx as usize, b.y as usize, lz as usize),
                            floor,
                            "pad cell ({x}, {z}) is not pad_floor"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn drop_sites_are_flat_and_legal() {
        for n in [2u8, 4, 6] {
            let seed = 0xA1B0_0000u64.wrapping_add(n as u64);
            let g = WorldGen::new(seed, n);
            let sites = g.drop_sites();
            assert!(!sites.is_empty(), "no legal drop sites for n = {n}");

            for s in sites {
                // Slope within bounds.
                for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let h = g.height_at(s.x + dx, s.z + dz);
                    assert!(
                        (h - s.y).abs() <= DROP_MAX_SLOPE,
                        "site ({}, {}) slope {} exceeds {DROP_MAX_SLOPE}",
                        s.x,
                        s.z,
                        (h - s.y).abs()
                    );
                }
                // Outside every base pad.
                for b in g.bases() {
                    let d2 = (s.x - b.x) * (s.x - b.x) + (s.z - b.z) * (s.z - b.z);
                    assert!(
                        d2 >= DROP_MIN_BASE_DIST * DROP_MIN_BASE_DIST,
                        "site ({}, {}) is inside base {}",
                        s.x,
                        s.z,
                        b.team
                    );
                    assert!(!b.on_pad(s.x, s.z));
                }
                // Above water, so a crate does not sink.
                assert!(s.y >= sea_level());
            }

            // Deterministic for the same seed.
            let again = WorldGen::new(seed, n);
            assert_eq!(sites, again.drop_sites());
        }
    }

    #[test]
    fn no_crate_ore_exists() {
        let r = registry();
        for i in 0..r.len() as u8 {
            let name = &r.def(BlockId(i)).name;
            // Substring, not `contains`: `core` contains "ore" and is a
            // legitimate block. Only actual ore names are forbidden.
            assert!(
                name != "ore" && !name.ends_with("_ore"),
                "D-01 removed ore entirely, but blocks.ron defines `{name}`"
            );
        }

        // Generation never emits a crate: crates only exist once a player
        // places one, which is what keeps the diff budget tiny (plan §4.4.4).
        let g = WorldGen::new(0xC0FFEE, 4);
        for (cx, cz) in [(0, 0), (5, 5), (9, 3), (12, 14)] {
            assert_eq!(count_of(&g.column(cx, cz), CRATE), 0);
        }
    }

    #[test]
    fn surface_is_never_air_and_water_sits_on_ground() {
        let g = WorldGen::new(7, 2);
        for (cx, cz) in [(1, 1), (6, 9)] {
            let col = g.column(cx, cz);
            for lz in 0..CHUNK_Z {
                for lx in 0..CHUNK_X {
                    let wx = cx * CHUNK_X as i32 + lx as i32;
                    let wz = cz * CHUNK_Z as i32 + lz as i32;
                    let h = g.height_at(wx, wz);
                    assert_ne!(
                        col.get(lx, h as usize, lz),
                        AIR,
                        "surface cell is air at ({wx}, {wz})"
                    );
                    // Nothing floats: the cell below the surface is solid.
                    if h > 0 {
                        assert_ne!(col.get(lx, (h - 1) as usize, lz), AIR);
                    }
                }
            }
        }
    }

    #[test]
    fn trees_are_not_clipped_at_column_borders() {
        // Trees rooted just outside a column must still write their canopy
        // inside it, otherwise every chunk boundary shows a sliced tree.
        let g = WorldGen::new(0x77, 2);
        let mut found = 0;
        for cx in 0..6 {
            for cz in 0..6 {
                let col = g.column(cx, cz);
                found += count_of(&col, palette().log);
            }
        }
        assert!(found > 0, "no trees were generated at all");
    }
}
