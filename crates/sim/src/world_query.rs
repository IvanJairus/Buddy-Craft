//! The voxel-query abstraction.
//!
//! `sim` must not depend on `world`. The rules crate is compiled to wasm for
//! client prediction and natively for the server, and pulling in terrain
//! generation, meshing and chunk storage to answer "is there a block at (x, y,
//! z)?" would drag a megabyte of code into a 700 KB budget and couple the rules
//! to a storage format.
//!
//! So `sim` asks a trait, and `world` (which already depends on `sim`)
//! implements it. The dependency arrow points one way, the rules stay testable
//! against a literal `Vec<bool>`, and the server is free to back the query with
//! generated terrain while a unit test backs it with three lines of setup.

/// Something the simulation can ask about the voxel world.
///
/// Coordinates are world blocks, `i32`, and out-of-range queries must return
/// `false` rather than panicking: the collision sweep legitimately probes
/// outside the map at the edges, and a panic there would take down a room.
pub trait VoxelQuery {
    /// True when the cell blocks movement.
    fn solid(&self, x: i32, y: i32, z: i32) -> bool;

    /// True when the cell is water. Swimmable, not walkable.
    ///
    /// Defaults to `false` so a test world only has to implement `solid`.
    fn water(&self, x: i32, y: i32, z: i32) -> bool {
        let _ = (x, y, z);
        false
    }
}

/// A world with nothing in it. Useful for physics tests that only need gravity.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyWorld;

impl VoxelQuery for EmptyWorld {
    #[inline]
    fn solid(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// An axis-aligned box of cells, indexed from `origin`.
///
/// This is the workhorse for tests: a 16³ field of `bool`s is enough to check
/// collision, step-up and swimming without generating a real map. It is also
/// what `tests/recipe_matrix.rs` uses for pad containment.
#[derive(Clone, Debug)]
pub struct VoxelGrid {
    origin: [i32; 3],
    dims: [i32; 3],
    cells: Vec<bool>,
    water: Vec<bool>,
}

impl VoxelGrid {
    /// A grid of `dims` cells starting at `origin`, all empty.
    pub fn new(origin: [i32; 3], dims: [i32; 3]) -> VoxelGrid {
        let n = (dims[0].max(0) * dims[1].max(0) * dims[2].max(0)) as usize;
        VoxelGrid {
            origin,
            dims,
            cells: vec![false; n],
            water: vec![false; n],
        }
    }

    /// A cube of edge `n` starting at the origin.
    pub fn cube(n: i32) -> VoxelGrid {
        VoxelGrid::new([0, 0, 0], [n, n, n])
    }

    /// Grid dimensions.
    pub fn dims(&self) -> [i32; 3] {
        self.dims
    }

    /// Grid origin.
    pub fn origin(&self) -> [i32; 3] {
        self.origin
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let lx = x - self.origin[0];
        let ly = y - self.origin[1];
        let lz = z - self.origin[2];
        if lx < 0
            || ly < 0
            || lz < 0
            || lx >= self.dims[0]
            || ly >= self.dims[1]
            || lz >= self.dims[2]
        {
            return None;
        }
        Some((lx + self.dims[0] * (lz + self.dims[2] * ly)) as usize)
    }

    /// Set a cell solid. Out-of-range writes are ignored.
    pub fn set_solid(&mut self, x: i32, y: i32, z: i32, solid: bool) {
        if let Some(i) = self.index(x, y, z) {
            self.cells[i] = solid;
        }
    }

    /// Set a cell to water. Out-of-range writes are ignored.
    pub fn set_water(&mut self, x: i32, y: i32, z: i32, water: bool) {
        if let Some(i) = self.index(x, y, z) {
            self.water[i] = water;
        }
    }

    /// Fill a horizontal slab at height `y`.
    pub fn fill_layer(&mut self, y: i32, solid: bool) {
        for z in self.origin[2]..self.origin[2] + self.dims[2] {
            for x in self.origin[0]..self.origin[0] + self.dims[0] {
                self.set_solid(x, y, z, solid);
            }
        }
    }

    /// Fill a solid box, inclusive of both corners.
    pub fn fill_box(&mut self, lo: [i32; 3], hi: [i32; 3], solid: bool) {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                for x in lo[0]..=hi[0] {
                    self.set_solid(x, y, z, solid);
                }
            }
        }
    }

    /// Number of solid cells.
    pub fn solid_count(&self) -> usize {
        self.cells.iter().filter(|c| **c).count()
    }
}

impl VoxelQuery for VoxelGrid {
    #[inline]
    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.index(x, y, z).map(|i| self.cells[i]).unwrap_or(false)
    }

    #[inline]
    fn water(&self, x: i32, y: i32, z: i32) -> bool {
        self.index(x, y, z).map(|i| self.water[i]).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_world_is_empty() {
        let w = EmptyWorld;
        assert!(!w.solid(0, 0, 0));
        assert!(!w.water(0, 0, 0));
    }

    #[test]
    fn grid_reads_back_writes() {
        let mut g = VoxelGrid::cube(8);
        assert_eq!(g.solid_count(), 0);
        g.set_solid(3, 4, 5, true);
        assert!(g.solid(3, 4, 5));
        assert!(!g.solid(4, 4, 5));
        assert_eq!(g.solid_count(), 1);
        g.set_solid(3, 4, 5, false);
        assert_eq!(g.solid_count(), 0);
    }

    #[test]
    fn out_of_range_is_empty_not_panic() {
        let mut g = VoxelGrid::new([10, 20, 30], [4, 4, 4]);
        g.set_solid(11, 21, 31, true);
        assert!(g.solid(11, 21, 31));
        assert!(!g.solid(9, 21, 31), "below the grid must be empty");
        assert!(!g.solid(100, 100, 100));
        // Writes outside the grid are dropped, not wrapped onto a neighbour.
        g.set_solid(9, 21, 31, true);
        assert_eq!(g.solid_count(), 1);
    }

    #[test]
    fn fill_helpers_cover_the_right_cells() {
        let mut g = VoxelGrid::cube(4);
        g.fill_layer(1, true);
        assert_eq!(g.solid_count(), 16);
        let mut g = VoxelGrid::cube(4);
        g.fill_box([0, 0, 0], [1, 1, 1], true);
        assert_eq!(g.solid_count(), 8);
    }

    #[test]
    fn water_is_tracked_separately() {
        let mut g = VoxelGrid::cube(4);
        g.set_water(1, 1, 1, true);
        assert!(g.water(1, 1, 1));
        assert!(!g.solid(1, 1, 1), "water must not block movement");
    }
}
