//! The stacking pad: the only place crates can exist.
//!
//! # Why this is 256 bytes and not a chunk
//!
//! Decision D-01 plus the anti-abuse rule "crates can only be placed inside
//! your own pad" (§4.5.4) means the *entire mutable world* is an 8 x 4 x 8 box
//! per team. Nothing else in the world ever changes, because terrain is
//! indestructible in v1.
//!
//! That has three consequences worth stating plainly:
//!
//! * `sim` does not need a voxel storage crate at all. The authoritative state
//!   is `[bool; 256]` per team, which hashes trivially, serialises trivially,
//!   and cannot desync.
//! * The chunk-diff budget from T-008 is not just met, it is irrelevant: the
//!   server can afford to resend a whole pad every tick if it wants to.
//! * The recipe matcher's search space is 256 cells, not 65,536, which is why
//!   it is cheap enough to run on every deposit.
//!
//! # Coordinates
//!
//! Pad-local `(x, y, z)` with `x, z` in `0..8` and `y` in `0..4`, where `y = 0`
//! is the layer resting on the pad floor. World position is
//! `(pad_origin_x + x, pad_origin_y + y, pad_origin_z + z)`.
//!
//! # Two masks, not one
//!
//! Each pad holds a *contents* mask (crates) and a *blocked* mask (the core's
//! protected volume, §4.5.1 rule 5). Keeping them separate means the blocked
//! mask never enters the stability hash: it is static geometry stamped during
//! base layout, so it cannot make a stable pad look like it is still moving.

use serde::{Deserialize, Serialize};

use crate::ids::TeamId;
use crate::time::Tick;

/// Pad edge length in blocks. Must equal `tuning.pad.size`.
pub const PAD_SIZE: i32 = 8;
/// Maximum stack height in blocks. Must equal `tuning.pad.max_height`.
pub const PAD_HEIGHT: i32 = 4;
/// Cells in a pad.
pub const PAD_CELLS: usize = (PAD_SIZE * PAD_HEIGHT * PAD_SIZE) as usize;
/// Cells in one pad layer.
pub const PAD_LAYER: usize = (PAD_SIZE * PAD_SIZE) as usize;

/// Ticks a pad's contents must be unchanged before a recipe fires (§4.5.1).
///
/// 10 ticks is 500 ms: long enough that a player depositing crates one per
/// tick never triggers mid-deposit, short enough that the feedback still feels
/// immediate.
pub const STABILITY_TICKS: u32 = 10;

/// Why a crate placement was refused (§4.5.4).
///
/// Every variant is a rule the server enforces, not a client-side courtesy.
/// The strings exist so the host can log a refusal without guessing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaceError {
    /// Not inside any team's pad. Terrain is not buildable (§4.4.3).
    Outside,
    /// Inside a pad that belongs to another team.
    NotYours,
    /// Inside the core's protected volume (§4.5.1 rule 5).
    Protected,
    /// Horizontally inside a pad but above its build height.
    TooHigh,
    /// The pad's team has been eliminated (D-02). The volume is dead ground:
    /// crates are gone, nothing can be built there again, and the losing
    /// players are on a defeat screen rather than placing blocks.
    Eliminated,
}

impl core::fmt::Display for PlaceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            PlaceError::Outside => "not inside a stacking pad",
            PlaceError::NotYours => "inside another team's pad",
            PlaceError::Protected => "inside the core's protected volume",
            PlaceError::TooHigh => "above the pad's build height",
            PlaceError::Eliminated => "that team has been eliminated",
        };
        f.write_str(s)
    }
}

/// A team's 8 x 4 x 8 stacking pad.
#[derive(Clone, Copy, Debug)]
pub struct Pad {
    cells: [bool; PAD_CELLS],
    blocked: [bool; PAD_CELLS],
    stable_since: Tick,
    last_hash: u64,
    /// Set once the team is eliminated (D-02). Irreversible.
    sealed: bool,
    /// World position of pad-local `(0, 0, 0)`.
    origin: [i32; 3],
}

impl Default for Pad {
    fn default() -> Self {
        Pad::new([0, 0, 0])
    }
}

impl Pad {
    /// An empty pad whose local origin sits at `origin`.
    ///
    /// The stability clock starts primed with the empty pad's own hash, so a
    /// freshly created pad is "unchanged since tick 0" rather than reporting a
    /// spurious change on its first tick.
    pub fn new(origin: [i32; 3]) -> Pad {
        let mut pad = Pad {
            cells: [false; PAD_CELLS],
            blocked: [false; PAD_CELLS],
            stable_since: Tick::ZERO,
            last_hash: 0,
            sealed: false,
            origin,
        };
        pad.last_hash = pad.hash();
        pad
    }

    /// World position of pad-local `(0, 0, 0)`.
    #[inline]
    pub fn origin(&self) -> [i32; 3] {
        self.origin
    }

    /// Move the pad. Only valid before the match starts; used when the base
    /// layout is resolved from the seed.
    pub fn set_origin(&mut self, origin: [i32; 3]) {
        self.origin = origin;
    }

    /// Pad-local to world coordinates.
    #[inline]
    pub fn to_world(&self, local: [i32; 3]) -> [i32; 3] {
        [
            self.origin[0] + local[0],
            self.origin[1] + local[1],
            self.origin[2] + local[2],
        ]
    }

    /// World to pad-local coordinates. The inverse of [`Pad::to_world`].
    #[inline]
    pub fn to_local(&self, world: [i32; 3]) -> [i32; 3] {
        [
            world[0] - self.origin[0],
            world[1] - self.origin[1],
            world[2] - self.origin[2],
        ]
    }

    /// True when a pad-local coordinate is inside the volume.
    #[inline]
    pub const fn in_bounds(local: [i32; 3]) -> bool {
        local[0] >= 0
            && local[1] >= 0
            && local[2] >= 0
            && local[0] < PAD_SIZE
            && local[1] < PAD_HEIGHT
            && local[2] < PAD_SIZE
    }

    /// Flat index of a pad-local coordinate, or `None` if out of bounds.
    ///
    /// Public because the recipe matcher needs the same index space for its
    /// visited mask; two different index conventions in one crate is exactly
    /// the kind of thing that produces a silent double-craft.
    #[inline]
    pub fn index_of(local: [i32; 3]) -> Option<usize> {
        if !Pad::in_bounds(local) {
            return None;
        }
        Some((local[0] + PAD_SIZE * (local[2] + PAD_SIZE * local[1])) as usize)
    }

    /// Pad-local coordinate of a flat index.
    #[inline]
    pub fn local_of(index: usize) -> [i32; 3] {
        let x = (index % PAD_LAYER) as i32 % PAD_SIZE;
        let z = ((index % PAD_LAYER) as i32) / PAD_SIZE;
        let y = (index / PAD_LAYER) as i32;
        [x, y, z]
    }

    /// True when the cell holds a crate.
    #[inline]
    pub fn get(&self, local: [i32; 3]) -> bool {
        Pad::index_of(local).map(|i| self.cells[i]).unwrap_or(false)
    }

    /// Place or remove a crate. Returns `true` when the contents changed.
    ///
    /// Out-of-bounds writes are rejected rather than clamped: a crate outside
    /// the pad is an exploit, not a rounding error. Blocked cells reject
    /// placement for the same reason.
    pub fn set(&mut self, local: [i32; 3], occupied: bool) -> bool {
        let Some(i) = Pad::index_of(local) else {
            return false;
        };
        if self.sealed {
            return false;
        }
        if self.cells[i] == occupied {
            return false;
        }
        if occupied && self.blocked[i] {
            return false;
        }
        self.cells[i] = occupied;
        true
    }

    /// Mark a pad cell as unusable, or clear the mark.
    ///
    /// Used for the core's protected volume (§4.5.1 rule 5). Stamping a cell
    /// clears any crate already there, because the two states contradict.
    /// Returns `true` when anything changed.
    pub fn block(&mut self, local: [i32; 3], blocked: bool) -> bool {
        let Some(i) = Pad::index_of(local) else {
            return false;
        };
        let changed = self.blocked[i] != blocked || (blocked && self.cells[i]);
        self.blocked[i] = blocked;
        if blocked {
            self.cells[i] = false;
        }
        changed
    }

    /// True when the cell is inside the protected volume.
    #[inline]
    pub fn is_blocked(&self, local: [i32; 3]) -> bool {
        Pad::index_of(local)
            .map(|i| self.blocked[i])
            .unwrap_or(false)
    }

    /// Number of crates on the pad.
    pub fn count(&self) -> usize {
        self.cells.iter().filter(|c| **c).count()
    }

    /// True when the pad holds nothing.
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    /// Remove every crate. Used when a base is destroyed (D-02).
    ///
    /// The blocked mask survives: the core's volume is still a core's volume
    /// after the pad has been swept.
    pub fn clear(&mut self) {
        self.cells = [false; PAD_CELLS];
    }

    /// Destroy the pad's contents and close it for good (D-02).
    ///
    /// Irreversible and idempotent. An eliminated team's pad is dead ground:
    /// sealing is a flag rather than "block all 256 cells" because the blocked
    /// mask means something specific — the core's protected volume — and
    /// overloading it would make a swept pad indistinguishable from a core that
    /// grew to fill its own pad.
    pub fn seal(&mut self) {
        self.cells = [false; PAD_CELLS];
        self.sealed = true;
    }

    /// True once the pad has been sealed by an elimination.
    #[inline]
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Remove a set of crates. Returns how many were actually present.
    ///
    /// The recipe matcher calls this after a match, so consumption is one
    /// operation rather than a loop the caller can interrupt half way.
    pub fn consume(&mut self, blocks: &[[i32; 3]]) -> usize {
        let mut n = 0;
        for b in blocks {
            if self.set(*b, false) {
                n += 1;
            }
        }
        n
    }

    /// FNV-1a over the occupancy bitmap.
    ///
    /// Used only to detect "did anything change this tick", so a fast
    /// non-cryptographic hash is the right tool. It is deterministic and has no
    /// dependence on iteration order because it walks the fixed array.
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for c in &self.cells {
            h ^= u64::from(*c);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Advance the stability clock. Returns `true` when the pad has been
    /// unchanged for [`STABILITY_TICKS`].
    pub fn tick_stability(&mut self, now: Tick) -> bool {
        let h = self.hash();
        if h != self.last_hash {
            self.last_hash = h;
            self.stable_since = now;
            return false;
        }
        now.since(self.stable_since) >= STABILITY_TICKS
    }

    /// Force the stability clock back to `now`, e.g. after a craft consumed
    /// blocks so the next shape gets its own full window.
    pub fn reset_stability(&mut self, now: Tick) {
        self.last_hash = self.hash();
        self.stable_since = now;
    }

    /// Iterate occupied cells in a fixed order: `y` slowest, then `z`, then `x`.
    ///
    /// The order is part of the contract, not an implementation detail: it is
    /// what makes "which cluster crafts first when two match" deterministic.
    pub fn occupied(&self) -> impl Iterator<Item = [i32; 3]> + '_ {
        (0..PAD_HEIGHT).flat_map(move |y| {
            (0..PAD_SIZE).flat_map(move |z| {
                (0..PAD_SIZE).filter_map(move |x| {
                    let p = [x, y, z];
                    self.get(p).then_some(p)
                })
            })
        })
    }
}

/// Every pad in a match, addressed by team.
///
/// This exists so that "crates may be placed only inside your own pad"
/// (§4.5.4) is enforced by the type system rather than by remembering to check
/// it: there is no way to write to a pad except through [`Pads::place`], and
/// that method rejects the two interesting failure modes by name.
#[derive(Clone, Debug, Default)]
pub struct Pads {
    pads: Vec<Pad>,
}

impl Pads {
    /// No pads. Pads are registered as teams are created.
    pub fn new() -> Pads {
        Pads { pads: Vec::new() }
    }

    /// Register a pad for the next team id and return that id.
    pub fn push(&mut self, pad: Pad) -> TeamId {
        let id = TeamId(self.pads.len() as u8);
        self.pads.push(pad);
        id
    }

    /// Number of registered pads.
    pub fn len(&self) -> usize {
        self.pads.len()
    }

    /// True when no pad is registered.
    pub fn is_empty(&self) -> bool {
        self.pads.is_empty()
    }

    /// The pad belonging to `team`.
    pub fn get(&self, team: TeamId) -> Option<&Pad> {
        self.pads.get(team.0 as usize)
    }

    /// Mutable access to `team`'s pad. Only the match loop and the matcher use
    /// this; players go through [`Pads::place`].
    pub fn get_mut(&mut self, team: TeamId) -> Option<&mut Pad> {
        self.pads.get_mut(team.0 as usize)
    }

    /// The team whose pad covers `world` horizontally, if any.
    ///
    /// Height is deliberately **not** part of this test. "You are above your
    /// pad" and "that is not a pad" are different mistakes and deserve
    /// different messages, so [`Pads::place`] reports the height limit itself.
    ///
    /// Linear over at most six pads, and returns the lowest team id if pads
    /// ever overlap. Base layout (T-017) is responsible for them not
    /// overlapping; this function documents what happens if that invariant
    /// breaks rather than panicking.
    pub fn owner_of(&self, world: [i32; 3]) -> Option<TeamId> {
        (0..self.pads.len()).find_map(|i| {
            let l = self.pads[i].to_local(world);
            ((0..PAD_SIZE).contains(&l[0]) && (0..PAD_SIZE).contains(&l[2]))
                .then_some(TeamId(i as u8))
        })
    }

    /// Place (`occupied = true`) or remove a crate on `team`'s pad.
    ///
    /// Returns `Ok(true)` when the pad changed, `Ok(false)` when it was
    /// already in that state, and `Err(_)` when the position is not writable
    /// by this team at all.
    pub fn place(
        &mut self,
        team: TeamId,
        world: [i32; 3],
        occupied: bool,
    ) -> Result<bool, PlaceError> {
        match self.owner_of(world) {
            None => return Err(PlaceError::Outside),
            Some(owner) if owner != team => return Err(PlaceError::NotYours),
            Some(_) => {}
        }
        let Some(pad) = self.pads.get_mut(team.0 as usize) else {
            return Err(PlaceError::Outside);
        };
        if pad.is_sealed() {
            return Err(PlaceError::Eliminated);
        }
        let local = pad.to_local(world);
        if local[1] < 0 {
            return Err(PlaceError::Outside);
        }
        if local[1] >= PAD_HEIGHT {
            return Err(PlaceError::TooHigh);
        }
        if occupied && pad.is_blocked(local) {
            return Err(PlaceError::Protected);
        }
        Ok(pad.set(local, occupied))
    }
}

/// A crate placement, as it would appear in a chunk diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PadCell {
    /// Pad-local coordinate.
    pub local: [i32; 3],
    /// Whether the cell now holds a crate.
    pub occupied: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_match_tuning() {
        let t = crate::tuning::embedded();
        assert_eq!(PAD_SIZE as u32, t.pad.size);
        assert_eq!(PAD_HEIGHT as u32, t.pad.max_height);
        assert_eq!(PAD_CELLS, 256);
        assert_eq!(PAD_LAYER, 64);
    }

    #[test]
    fn new_pad_is_empty() {
        let p = Pad::new([10, 41, 20]);
        assert!(p.is_empty());
        assert_eq!(p.count(), 0);
        assert_eq!(p.to_world([0, 0, 0]), [10, 41, 20]);
        assert_eq!(p.to_world([7, 3, 7]), [17, 44, 27]);
        assert_eq!(p.to_local([17, 44, 27]), [7, 3, 7]);
    }

    #[test]
    fn index_roundtrips_every_cell() {
        let mut seen = [false; PAD_CELLS];
        for y in 0..PAD_HEIGHT {
            for z in 0..PAD_SIZE {
                for x in 0..PAD_SIZE {
                    let local = [x, y, z];
                    let i = Pad::index_of(local).expect("in bounds");
                    assert!(!seen[i], "index {i} produced twice");
                    seen[i] = true;
                    assert_eq!(Pad::local_of(i), local);
                }
            }
        }
        assert!(seen.iter().all(|s| *s), "every cell must be reachable");
    }

    #[test]
    fn set_get_roundtrips_and_reports_change() {
        let mut p = Pad::new([0, 0, 0]);
        assert!(p.set([1, 2, 3], true));
        assert!(p.get([1, 2, 3]));
        assert!(
            !p.set([1, 2, 3], true),
            "re-setting the same value is not a change"
        );
        assert!(p.set([1, 2, 3], false));
        assert!(!p.get([1, 2, 3]));
    }

    #[test]
    fn out_of_bounds_is_rejected() {
        let mut p = Pad::new([0, 0, 0]);
        for bad in [
            [-1, 0, 0],
            [0, -1, 0],
            [0, 0, -1],
            [PAD_SIZE, 0, 0],
            [0, PAD_HEIGHT, 0],
            [0, 0, PAD_SIZE],
        ] {
            assert!(!p.set(bad, true), "{bad:?} should be rejected");
            assert!(!p.get(bad));
        }
        assert_eq!(p.count(), 0);
    }

    #[test]
    fn occupied_iterates_in_a_fixed_order() {
        let mut p = Pad::new([0, 0, 0]);
        p.set([2, 1, 0], true);
        p.set([0, 0, 0], true);
        p.set([1, 0, 3], true);
        let order: Vec<[i32; 3]> = p.occupied().collect();
        assert_eq!(order, vec![[0, 0, 0], [1, 0, 3], [2, 1, 0]]);
    }

    #[test]
    fn stability_needs_ten_quiet_ticks() {
        let mut p = Pad::new([0, 0, 0]);
        // Establish the baseline.
        assert!(!p.tick_stability(Tick(0)));

        // A deposit resets the window.
        p.set([0, 0, 0], true);
        assert!(!p.tick_stability(Tick(1)));
        for t in 2..10 {
            assert!(!p.tick_stability(Tick(t)), "fired early at tick {t}");
        }
        assert!(
            p.tick_stability(Tick(11)),
            "should be stable 10 ticks after the change"
        );
    }

    #[test]
    fn stability_resets_when_the_pad_changes() {
        let mut p = Pad::new([0, 0, 0]);
        p.tick_stability(Tick(0));
        for t in 1..12 {
            p.tick_stability(Tick(t));
        }
        assert!(p.tick_stability(Tick(12)));
        // A late deposit must restart the whole window.
        p.set([5, 0, 5], true);
        assert!(!p.tick_stability(Tick(13)));
        assert!(!p.tick_stability(Tick(20)));
        assert!(p.tick_stability(Tick(23)));
    }

    #[test]
    fn hash_changes_with_contents() {
        let mut a = Pad::new([0, 0, 0]);
        let b = Pad::new([0, 0, 0]);
        assert_eq!(a.hash(), b.hash());
        a.set([3, 1, 3], true);
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn blocked_cells_reject_crates() {
        let mut p = Pad::new([0, 0, 0]);
        assert!(p.block([1, 0, 1], true));
        assert!(p.is_blocked([1, 0, 1]));
        assert!(!p.set([1, 0, 1], true), "blocked cell must refuse a crate");
        assert!(!p.get([1, 0, 1]));
        // Clearing the block makes it writable again.
        assert!(p.block([1, 0, 1], false));
        assert!(p.set([1, 0, 1], true));
    }

    #[test]
    fn blocking_clears_an_existing_crate() {
        let mut p = Pad::new([0, 0, 0]);
        p.set([2, 0, 2], true);
        assert!(p.block([2, 0, 2], true));
        assert!(!p.get([2, 0, 2]));
        assert_eq!(p.count(), 0);
    }

    #[test]
    fn the_blocked_mask_is_not_part_of_the_stability_hash() {
        // Base layout stamps the mask before the match; if it entered the hash
        // it would be indistinguishable from a late deposit.
        let mut p = Pad::new([0, 0, 0]);
        p.set([0, 0, 0], true);
        p.tick_stability(Tick(0));
        for t in 1..12 {
            p.tick_stability(Tick(t));
        }
        assert!(p.tick_stability(Tick(12)));
        p.block([7, 3, 7], true);
        assert!(
            p.tick_stability(Tick(13)),
            "static geometry must not restart the window"
        );
    }

    #[test]
    fn consume_removes_only_present_crates() {
        let mut p = Pad::new([0, 0, 0]);
        p.set([0, 0, 0], true);
        p.set([1, 0, 0], true);
        let removed = p.consume(&[[0, 0, 0], [1, 0, 0], [2, 0, 0]]);
        assert_eq!(removed, 2, "the absent cell is not a removal");
        assert_eq!(p.count(), 0);
    }

    #[test]
    fn pads_place_only_in_the_owners_pad() {
        let mut pads = Pads::new();
        let a = pads.push(Pad::new([0, 0, 0]));
        let b = pads.push(Pad::new([100, 0, 0]));
        assert_eq!(a, TeamId(0));
        assert_eq!(b, TeamId(1));

        assert_eq!(pads.place(a, [2, 0, 2], true), Ok(true));
        assert!(pads.get(a).unwrap().get([2, 0, 2]));

        // §4.5.4: terrain is not buildable.
        assert_eq!(pads.place(a, [50, 0, 50], true), Err(PlaceError::Outside));
        // §4.5.4: an enemy pad rejects placement.
        assert_eq!(pads.place(a, [102, 0, 2], true), Err(PlaceError::NotYours));
        assert!(pads.get(b).unwrap().is_empty());
        // The owner can.
        assert_eq!(pads.place(b, [102, 0, 2], true), Ok(true));
    }

    #[test]
    fn pads_report_height_separately_from_outside() {
        let mut pads = Pads::new();
        let a = pads.push(Pad::new([0, 0, 0]));
        assert_eq!(pads.place(a, [1, 4, 1], true), Err(PlaceError::TooHigh));
        assert_eq!(pads.place(a, [1, 3, 1], true), Ok(true));
        assert_eq!(pads.place(a, [1, -1, 1], true), Err(PlaceError::Outside));
    }

    #[test]
    fn pads_refuse_the_protected_volume() {
        let mut pads = Pads::new();
        let a = pads.push(Pad::new([0, 0, 0]));
        pads.get_mut(a).unwrap().block([3, 0, 3], true);
        assert_eq!(pads.place(a, [3, 0, 3], true), Err(PlaceError::Protected));
    }

    #[test]
    fn owner_of_finds_the_containing_pad() {
        let mut pads = Pads::new();
        pads.push(Pad::new([0, 0, 0]));
        pads.push(Pad::new([100, 0, 0]));
        assert_eq!(pads.owner_of([7, 3, 7]), Some(TeamId(0)));
        assert_eq!(pads.owner_of([100, 0, 0]), Some(TeamId(1)));
        assert_eq!(pads.owner_of([8, 0, 0]), None, "one past the edge");
    }
}
