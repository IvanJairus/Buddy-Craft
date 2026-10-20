//! Entity, player and team identifiers.
//!
//! [`EntityId`] packs a 20-bit slot index and a 12-bit generation. Reusing a
//! slot bumps the generation, so a stale id from a despawned entity can never
//! silently address the new occupant — the classic source of "the buddy I
//! killed is still shooting me" bugs.

use serde::{Deserialize, Serialize};

/// Bits reserved for the slot index.
pub const INDEX_BITS: u32 = 20;
/// Bits reserved for the generation counter.
pub const GENERATION_BITS: u32 = 12;
/// Largest addressable slot index.
pub const MAX_INDEX: u32 = (1 << INDEX_BITS) - 1;
/// Largest generation before a slot is retired.
pub const MAX_GENERATION: u16 = ((1 << GENERATION_BITS) - 1) as u16;

/// A generational handle to a simulated entity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct EntityId(u32);

impl EntityId {
    /// A handle that never refers to a live entity.
    pub const NONE: EntityId = EntityId(u32::MAX);

    /// Build a handle from a slot index and generation.
    #[inline]
    pub const fn new(index: u32, generation: u16) -> EntityId {
        debug_assert!(index <= MAX_INDEX);
        EntityId((index & MAX_INDEX) | ((generation as u32) << INDEX_BITS))
    }

    /// The slot index.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0 & MAX_INDEX
    }

    /// The generation of the slot when this handle was issued.
    #[inline]
    pub const fn generation(self) -> u16 {
        (self.0 >> INDEX_BITS) as u16
    }

    /// True for [`EntityId::NONE`].
    #[inline]
    pub const fn is_none(self) -> bool {
        self.0 == u32::MAX
    }
}

/// A player slot within a room. At most six (plan §4.7).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PlayerId(pub u8);

/// A team within a room. At most six.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[repr(transparent)]
pub struct TeamId(pub u8);

/// Maximum players in a room.
pub const MAX_PLAYERS: usize = 6;
/// Maximum teams in a room.
pub const MAX_TEAMS: usize = 6;

/// Allocates and recycles entity slots.
///
/// Deterministic: the free list is a `Vec` used as a stack, never a hash map.
#[derive(Clone, Debug, Default)]
pub struct EntityAllocator {
    generations: Vec<u16>,
    free: Vec<u32>,
    next_index: u32,
}

impl EntityAllocator {
    /// An allocator with no live entities.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live entities.
    pub fn live_count(&self) -> usize {
        self.generations.len() - self.free.len()
    }

    /// Highest slot index ever handed out, plus one.
    pub fn capacity(&self) -> u32 {
        self.next_index
    }

    /// Allocate a slot, or `None` if the index space is exhausted.
    pub fn alloc(&mut self) -> Option<EntityId> {
        if let Some(index) = self.free.pop() {
            let generation = self.generations[index as usize];
            return Some(EntityId::new(index, generation));
        }
        if self.next_index > MAX_INDEX {
            return None;
        }
        let index = self.next_index;
        self.next_index += 1;
        self.generations.push(0);
        Some(EntityId::new(index, 0))
    }

    /// Release a handle. Returns `false` if the handle is stale or foreign,
    /// in which case nothing changes — releasing twice is a no-op, not a panic.
    pub fn free(&mut self, id: EntityId) -> bool {
        if id.is_none() {
            return false;
        }
        let index = id.index();
        if index as usize >= self.generations.len() {
            return false;
        }
        if self.generations[index as usize] != id.generation() {
            return false;
        }
        let g = self.generations[index as usize];
        if g >= MAX_GENERATION {
            // Retire the slot rather than wrapping the generation, so an
            // ancient handle can never alias a new entity.
            return false;
        }
        self.generations[index as usize] = g + 1;
        self.free.push(index);
        true
    }

    /// True if the handle currently refers to a live entity.
    pub fn is_live(&self, id: EntityId) -> bool {
        if id.is_none() || id.index() as usize >= self.generations.len() {
            return false;
        }
        self.generations[id.index() as usize] == id.generation() && !self.free.contains(&id.index())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_packing_roundtrips() {
        let id = EntityId::new(123_456, 7);
        assert_eq!(id.index(), 123_456);
        assert_eq!(id.generation(), 7);
        assert!(!id.is_none());
        assert!(EntityId::NONE.is_none());
    }

    #[test]
    fn alloc_and_free_cycles() {
        let mut a = EntityAllocator::new();
        let e0 = a.alloc().unwrap();
        let e1 = a.alloc().unwrap();
        assert_ne!(e0, e1);
        assert_eq!(a.live_count(), 2);

        assert!(a.free(e0));
        assert_eq!(a.live_count(), 1);

        let e2 = a.alloc().unwrap();
        assert_eq!(e2.index(), e0.index(), "slot must be reused");
        assert_ne!(e2.generation(), e0.generation(), "generation must bump");
    }

    #[test]
    fn recycled_id_is_not_equal() {
        let mut a = EntityAllocator::new();
        let old = a.alloc().unwrap();
        a.free(old);
        let new = a.alloc().unwrap();
        assert_ne!(old, new, "a stale handle must not equal the new occupant");
        assert!(!a.is_live(old), "stale handle must not report live");
        assert!(a.is_live(new));
    }

    #[test]
    fn double_free_is_a_noop() {
        let mut a = EntityAllocator::new();
        let e = a.alloc().unwrap();
        assert!(a.free(e));
        assert!(!a.free(e), "second free must be rejected");
        assert_eq!(a.live_count(), 0);
    }

    #[test]
    fn foreign_handle_is_rejected() {
        let mut a = EntityAllocator::new();
        a.alloc().unwrap();
        let foreign = EntityId::new(999, 3);
        assert!(!a.free(foreign));
    }
}
