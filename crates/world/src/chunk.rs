//! Voxel column storage.
//!
//! A column is `16 x 96 x 16` blocks (24,576 cells). Storage is paletted and
//! nibble-packed, so the common case — a column with fewer than 17 distinct
//! blocks, which is every column of generated terrain — costs about 12 KB
//! instead of 49 KB for a plain `u16` per cell. A pad full of crates plus a
//! core plus terrain can exceed 16 palette entries, so the storage promotes
//! itself to a wide `u16` index array at that point rather than failing.
//!
//! This is deliberately **not** a sparse map or an octree. The world is
//! finite and only 96 blocks tall; a flat paletted array gives O(1) access,
//! trivial serialisation, and a memory footprint that a phone can hold for the
//! 169 columns of the render distance (169 x 12 KB ≈ 2 MB).

use crate::block::{BlockId, AIR};

/// Column extent along X.
pub const CHUNK_X: usize = 16;
/// Column extent along Z.
pub const CHUNK_Z: usize = 16;
/// Column extent along Y.
pub const CHUNK_Y: usize = 96;
/// Total cells in a column.
pub const VOLUME: usize = CHUNK_X * CHUNK_Z * CHUNK_Y;
/// Bytes used by nibble-packed indices.
pub const PACKED_BYTES: usize = VOLUME / 2;
/// Maximum palette size while nibble-packed (4 bits per index).
pub const MAX_PACKED_PALETTE: usize = 16;

/// Backing storage for a column's indices.
#[derive(Clone, Debug)]
enum Cells {
    /// Two 4-bit palette indices per byte.
    Packed(Vec<u8>),
    /// One `u16` palette index per cell; used once the palette outgrows 16.
    Wide(Vec<u16>),
}

/// A single 16 x 96 x 16 column of blocks.
#[derive(Clone, Debug)]
pub struct Column {
    palette: Vec<BlockId>,
    cells: Cells,
}

impl Default for Column {
    fn default() -> Self {
        Self::new()
    }
}

impl Column {
    /// An all-air column.
    pub fn new() -> Column {
        Column {
            palette: vec![AIR],
            cells: Cells::Packed(vec![0u8; PACKED_BYTES]),
        }
    }

    /// Linear index of a cell.
    ///
    /// Layout is `x + 16 * (z + 16 * y)`, i.e. X is the fastest-varying axis so
    /// that the mesher's inner loop and the diff encoder both walk contiguous
    /// memory.
    #[inline]
    pub const fn index(x: usize, y: usize, z: usize) -> usize {
        x + CHUNK_X * (z + CHUNK_Z * y)
    }

    /// Inverse of [`Column::index`].
    #[inline]
    pub const fn coords(mut index: usize) -> (usize, usize, usize) {
        let x = index % CHUNK_X;
        index /= CHUNK_X;
        let z = index % CHUNK_Z;
        let y = index / CHUNK_Z;
        (x, y, z)
    }

    /// True when the coordinates are inside the column.
    #[inline]
    pub const fn in_bounds(x: usize, y: usize, z: usize) -> bool {
        x < CHUNK_X && y < CHUNK_Y && z < CHUNK_Z
    }

    /// Number of distinct blocks in the palette.
    #[inline]
    pub fn palette_len(&self) -> usize {
        self.palette.len()
    }

    /// True when storage has promoted to wide indices.
    #[inline]
    pub fn is_wide(&self) -> bool {
        matches!(self.cells, Cells::Wide(_))
    }

    /// Heap bytes held by this column, for the memory-budget test.
    pub fn bytes_allocated(&self) -> usize {
        let cells = match &self.cells {
            Cells::Packed(v) => v.capacity(),
            Cells::Wide(v) => v.capacity() * core::mem::size_of::<u16>(),
        };
        cells + self.palette.capacity() * core::mem::size_of::<BlockId>()
    }

    /// Read a cell. Out-of-bounds reads return [`AIR`] rather than panicking,
    /// because the mesher samples one block outside the column on every face.
    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> BlockId {
        if !Self::in_bounds(x, y, z) {
            return AIR;
        }
        let i = Self::index(x, y, z);
        match &self.cells {
            Cells::Packed(bytes) => {
                let byte = bytes[i / 2];
                let nibble = if i & 1 == 0 { byte & 0x0F } else { byte >> 4 };
                self.palette[nibble as usize]
            }
            Cells::Wide(cells) => self.palette[cells[i] as usize],
        }
    }

    /// Write a cell. Returns `true` when the value actually changed.
    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, id: BlockId) -> bool {
        if !Self::in_bounds(x, y, z) {
            return false;
        }
        if self.get(x, y, z) == id {
            return false;
        }
        let p = self.palette_index(id);
        let i = Self::index(x, y, z);
        match &mut self.cells {
            Cells::Packed(bytes) => {
                let byte = &mut bytes[i / 2];
                if i & 1 == 0 {
                    *byte = (*byte & 0xF0) | (p as u8 & 0x0F);
                } else {
                    *byte = (*byte & 0x0F) | ((p as u8 & 0x0F) << 4);
                }
            }
            Cells::Wide(cells) => cells[i] = p as u16,
        }
        true
    }

    /// Fill the whole column with one block.
    pub fn fill(&mut self, id: BlockId) {
        self.palette.clear();
        self.palette.push(id);
        match &mut self.cells {
            Cells::Packed(bytes) => bytes.fill(0),
            Cells::Wide(cells) => cells.fill(0),
        }
    }

    /// Number of non-air cells.
    pub fn count_non_air(&self) -> usize {
        match &self.cells {
            Cells::Packed(bytes) => {
                // Palette entry 0 is air for a freshly created column, but
                // `fill` rewrites the palette so that entry 0 becomes the fill
                // block. Resolving through a 16-entry table keeps the count
                // correct in both cases without a branch per cell.
                let mut solid = [false; MAX_PACKED_PALETTE];
                for (i, id) in self.palette.iter().enumerate() {
                    solid[i] = *id != AIR;
                }
                bytes
                    .iter()
                    .map(|b| {
                        solid[(b & 0x0F) as usize] as usize + solid[(b >> 4) as usize] as usize
                    })
                    .sum()
            }
            Cells::Wide(cells) => cells
                .iter()
                .filter(|p| self.palette[**p as usize] != AIR)
                .count(),
        }
    }

    /// Highest non-air Y, or 0 for an empty column. Used by the mesher to skip
    /// empty sky slices and by worldgen to place trees.
    pub fn highest_solid_y(&self) -> usize {
        for y in (0..CHUNK_Y).rev() {
            for z in 0..CHUNK_Z {
                for x in 0..CHUNK_X {
                    if self.get(x, y, z) != AIR {
                        return y;
                    }
                }
            }
        }
        0
    }

    /// Palette index for a block, inserting it if there is room.
    fn palette_index(&mut self, id: BlockId) -> usize {
        if let Some(i) = self.palette.iter().position(|b| *b == id) {
            return i;
        }
        if matches!(self.cells, Cells::Packed(_)) && self.palette.len() >= MAX_PACKED_PALETTE {
            self.promote();
        }
        self.palette.push(id);
        self.palette.len() - 1
    }

    /// Convert nibble-packed indices to wide indices, preserving contents.
    fn promote(&mut self) {
        let Cells::Packed(bytes) = &self.cells else {
            return;
        };
        let mut wide = Vec::with_capacity(VOLUME);
        for i in 0..VOLUME {
            let byte = bytes[i / 2];
            let nibble = if i & 1 == 0 { byte & 0x0F } else { byte >> 4 };
            wide.push(nibble as u16);
        }
        self.cells = Cells::Wide(wide);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::registry;

    /// Deterministic pseudo-random driver, so the property-style tests are
    /// reproducible without pulling in a proptest dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn new_column_is_all_air() {
        let c = Column::new();
        assert_eq!(c.palette_len(), 1);
        assert!(!c.is_wide());
        assert_eq!(c.count_non_air(), 0);
        for y in [0, 1, 47, 95] {
            assert_eq!(c.get(0, y, 0), AIR);
            assert_eq!(c.get(15, y, 15), AIR);
        }
    }

    #[test]
    fn index_roundtrips() {
        for y in [0usize, 1, 47, 95] {
            for z in [0usize, 7, 15] {
                for x in [0usize, 3, 15] {
                    let i = Column::index(x, y, z);
                    assert_eq!(Column::coords(i), (x, y, z));
                }
            }
        }
        assert_eq!(Column::index(15, 95, 15), VOLUME - 1);
    }

    #[test]
    fn set_get_roundtrip_every_block() {
        let r = registry();
        let mut c = Column::new();
        for i in 0..r.len() as u8 {
            let id = BlockId(i);
            // One distinct cell per block type, so every write is a real
            // change except the very first (air over air).
            let (x, y, z) = (i as usize % CHUNK_X, 0, i as usize / CHUNK_X);
            let changed = c.set(x, y, z, id);
            assert_eq!(
                changed,
                id != AIR,
                "block {i} reported the wrong change flag"
            );
            assert_eq!(c.get(x, y, z), id, "block {i} did not roundtrip");
        }
        // 20 distinct blocks means we must have promoted past the nibble limit.
        assert!(
            c.is_wide(),
            "20 distinct blocks must promote to wide storage"
        );
        assert_eq!(
            c.count_non_air(),
            r.len() - 1,
            "every block except air is solid"
        );
    }

    #[test]
    fn set_reports_change_only_once() {
        let mut c = Column::new();
        assert!(c.set(1, 2, 3, BlockId(1)));
        assert!(
            !c.set(1, 2, 3, BlockId(1)),
            "writing the same value must report no change"
        );
    }

    #[test]
    fn eight_blocks_stay_under_16kb() {
        let mut c = Column::new();
        for i in 0..8u8 {
            for y in 0..8 {
                c.set(0, y, i as usize, BlockId(i));
            }
        }
        assert!(!c.is_wide());
        let bytes = c.bytes_allocated();
        assert!(
            bytes <= 16 * 1024,
            "column used {bytes} bytes, budget is 16 KB"
        );
    }

    #[test]
    fn promotion_preserves_contents() {
        let mut c = Column::new();
        // Fill with 16 distinct blocks (the packed maximum).
        for i in 0..16u8 {
            c.set(i as usize, 0, 0, BlockId(i));
        }
        assert!(!c.is_wide());
        let before: Vec<BlockId> = (0..16).map(|i| c.get(i, 0, 0)).collect();

        // The 17th distinct value forces promotion.
        c.set(0, 1, 0, BlockId(17));
        assert!(c.is_wide());
        let after: Vec<BlockId> = (0..16).map(|i| c.get(i, 0, 0)).collect();
        assert_eq!(before, after, "promotion must not disturb existing cells");
        assert_eq!(c.get(0, 1, 0), BlockId(17));
    }

    #[test]
    fn fill_and_count() {
        let mut c = Column::new();
        c.fill(BlockId(3));
        assert_eq!(c.count_non_air(), VOLUME);
        assert_eq!(c.palette_len(), 1);
        c.fill(AIR);
        assert_eq!(c.count_non_air(), 0);
    }

    #[test]
    fn out_of_bounds_is_air_not_panic() {
        let c = Column::new();
        assert_eq!(c.get(16, 0, 0), AIR);
        assert_eq!(c.get(0, 96, 0), AIR);
        assert_eq!(c.get(0, 0, 16), AIR);
        let mut c = Column::new();
        assert!(!c.set(99, 99, 99, BlockId(1)));
    }

    #[test]
    fn randomized_roundtrip() {
        // 20,000 random writes followed by a full read-back comparison.
        let mut c = Column::new();
        let mut model = vec![AIR; VOLUME];
        let mut rng = Lcg(12345);
        for _ in 0..20_000 {
            let (x, y, z) = (rng.below(CHUNK_X), rng.below(CHUNK_Y), rng.below(CHUNK_Z));
            let id = BlockId(rng.below(20) as u8);
            c.set(x, y, z, id);
            model[Column::index(x, y, z)] = id;
        }
        for y in 0..CHUNK_Y {
            for z in 0..CHUNK_Z {
                for x in 0..CHUNK_X {
                    assert_eq!(c.get(x, y, z), model[Column::index(x, y, z)]);
                }
            }
        }
    }

    #[test]
    fn highest_solid_y_tracks_the_top_block() {
        let mut c = Column::new();
        assert_eq!(c.highest_solid_y(), 0);
        c.set(4, 42, 4, BlockId(1));
        assert_eq!(c.highest_solid_y(), 42);
        c.set(4, 90, 4, BlockId(1));
        assert_eq!(c.highest_solid_y(), 90);
    }
}
