//! Budgeted A* over the voxel grid.
//!
//! # Why this is bounded three ways
//!
//! A buddy that cannot path is an annoyance. A buddy that paths for 40 ms is a
//! server hang, and a hang is indistinguishable from a crash to six players
//! mid-match. So every search here is bounded by:
//!
//! 1. **A node budget** — [`EXPANSION_BUDGET`] expansions, then give up (§4.6).
//! 2. **A search box** — the union of the start and goal cells plus a margin.
//!    Without it the closed set would be a 256 x 96 x 256 dense array (6.3 M
//!    entries, 25 MB) allocated per search.
//! 3. **A distance cap** — a goal further than [`MAX_SEARCH_EXTENT`] away is
//!    refused outright rather than searched for and failed.
//!
//! When a search fails the caller steers straight at the goal and accepts
//! getting stuck. That is a deliberate trade: a stuck buddy is legible to the
//! player ("it's walking into the wall"), a stalled tick is not.
//!
//! # Determinism
//!
//! The open set is a binary heap ordered by `(f, cell)`. Ties are broken by
//! cell coordinate, not by insertion order, so the same world produces the same
//! path on every target — which matters because a buddy's position feeds the
//! state hash (T-015).

use crate::world_query::VoxelQuery;

/// A voxel coordinate.
pub type Cell = [i32; 3];

/// Node expansions per search before giving up (§4.6).
pub const EXPANSION_BUDGET: u32 = 512;

/// Largest drop a path may include, in blocks (§4.6).
pub const MAX_FALL: i32 = 3;

/// Cells of margin added around the start and goal when sizing the search box.
const MARGIN: i32 = 4;

/// Largest start-to-goal separation on X or Z that will be searched.
///
/// The leash is 24 m (§4.6), so 56 leaves room for a path that has to go
/// around something without ever building a 400 KB closed set.
pub const MAX_SEARCH_EXTENT: i32 = 56;

/// Maximum neighbours a cell can produce.
const MAX_NEIGHBOURS: usize = 24;

/// Tuning for one search.
#[derive(Clone, Copy, Debug)]
pub struct PathLimits {
    /// Node expansions before the search gives up.
    pub budget: u32,
    /// Deepest drop the path may use.
    pub max_fall: i32,
    /// Extra cells above and below the start/goal band that the search may
    /// enter. Four is enough for a buddy following its owner; a bot crossing
    /// hills needs more, because a mountain between two valley endpoints is
    /// otherwise outside the search box even though the path over it exists.
    pub y_margin: i32,
}

impl Default for PathLimits {
    fn default() -> Self {
        PathLimits {
            budget: EXPANSION_BUDGET,
            max_fall: MAX_FALL,
            y_margin: MARGIN,
        }
    }
}

/// Cost of one horizontal step.
const COST_STRAIGHT: i32 = 10;
/// Cost of climbing one block.
const COST_STEP_UP: i32 = 14;
/// Cost of falling one block; deeper falls add [`COST_FALL_PER_BLOCK`] each.
const COST_FALL: i32 = 6;
/// Extra cost per additional block fallen.
const COST_FALL_PER_BLOCK: i32 = 2;

/// True when a body two blocks tall fits in `c`.
fn standable<W: VoxelQuery + ?Sized>(world: &W, c: Cell) -> bool {
    !world.solid(c[0], c[1], c[2]) && !world.solid(c[0], c[1] + 1, c[2])
}

/// True when the cell has something solid directly beneath it.
fn has_floor<W: VoxelQuery + ?Sized>(world: &W, c: Cell) -> bool {
    world.solid(c[0], c[1] - 1, c[2])
}

/// Drop `c` to the nearest cell with a floor, within [`MAX_FALL`] blocks.
///
/// A buddy knocked into the air has no grounded cell to start from, and an A*
/// whose start has no floor would expand nothing.
pub fn snap_to_ground<W: VoxelQuery + ?Sized>(world: &W, c: Cell) -> Option<Cell> {
    for drop in 0..=MAX_FALL {
        let candidate = [c[0], c[1] - drop, c[2]];
        if standable(world, candidate) && has_floor(world, candidate) {
            return Some(candidate);
        }
    }
    None
}

/// A closed AABB of cells.
#[derive(Clone, Copy, Debug)]
struct SearchBox {
    min: Cell,
    max: Cell,
}

impl SearchBox {
    fn around(start: Cell, goal: Cell, y_margin: i32) -> Option<SearchBox> {
        for axis in [0usize, 2] {
            if (goal[axis] - start[axis]).abs() > MAX_SEARCH_EXTENT {
                return None;
            }
        }
        Some(SearchBox {
            min: [
                start[0].min(goal[0]) - MARGIN,
                start[1].min(goal[1]) - y_margin,
                start[2].min(goal[2]) - MARGIN,
            ],
            max: [
                start[0].max(goal[0]) + MARGIN,
                start[1].max(goal[1]) + y_margin,
                start[2].max(goal[2]) + MARGIN,
            ],
        })
    }

    fn dims(&self) -> [i32; 3] {
        [
            self.max[0] - self.min[0] + 1,
            self.max[1] - self.min[1] + 1,
            self.max[2] - self.min[2] + 1,
        ]
    }

    fn len(&self) -> usize {
        let d = self.dims();
        (d[0].max(0) * d[1].max(0) * d[2].max(0)) as usize
    }

    fn index(&self, c: Cell) -> Option<usize> {
        if c[0] < self.min[0]
            || c[1] < self.min[1]
            || c[2] < self.min[2]
            || c[0] > self.max[0]
            || c[1] > self.max[1]
            || c[2] > self.max[2]
        {
            return None;
        }
        let d = self.dims();
        let local = [c[0] - self.min[0], c[1] - self.min[1], c[2] - self.min[2]];
        Some((local[0] + d[0] * (local[2] + d[2] * local[1])) as usize)
    }

    fn cell_of(&self, index: usize) -> Cell {
        let d = self.dims();
        let x = index as i32 % d[0];
        let z = (index as i32 / d[0]) % d[2];
        let y = index as i32 / (d[0] * d[2]);
        [self.min[0] + x, self.min[1] + y, self.min[2] + z]
    }
}

/// One entry in the open set.
///
/// `Ord` is written so `BinaryHeap` (a max-heap) pops the *lowest* `f`, and
/// breaks ties on the cell itself. Insertion order never affects the result.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Node {
    f: i32,
    cell: Cell,
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // Reversed on `f` so the smallest comes out of a max-heap first.
        other
            .f
            .cmp(&self.f)
            .then_with(|| other.cell.cmp(&self.cell))
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Manhattan distance in the straight-step cost metric.
fn heuristic(a: Cell, b: Cell) -> i32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}

/// Fill `out` with the cells reachable from `c` in one move, returning how many.
///
/// Four horizontal directions, each tried at the current level, one block up
/// (step up), and one to [`PathLimits::max_fall`] blocks down.
fn expand<W: VoxelQuery + ?Sized>(
    world: &W,
    c: Cell,
    max_fall: i32,
    out: &mut [Cell; MAX_NEIGHBOURS],
) -> usize {
    let mut n = 0;
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let x = c[0] + dx;
        let z = c[2] + dz;

        // Level ground.
        let level = [x, c[1], z];
        if standable(world, level) && has_floor(world, level) && n < MAX_NEIGHBOURS {
            out[n] = level;
            n += 1;
        }

        // Step up one block. Needs headroom above the destination.
        let up = [x, c[1] + 1, z];
        if standable(world, up)
            && !world.solid(x, c[1] + 2, z)
            && has_floor(world, up)
            && n < MAX_NEIGHBOURS
        {
            out[n] = up;
            n += 1;
        }

        // Fall one to `max_fall` blocks, taking the first landing only: a
        // deeper landing past a ledge is a different move, not a better one.
        for drop in 1..=max_fall {
            let down = [x, c[1] - drop, z];
            if !standable(world, down) {
                continue;
            }
            if has_floor(world, down) {
                if n < MAX_NEIGHBOURS {
                    out[n] = down;
                    n += 1;
                }
                break;
            }
        }
    }
    n
}

/// Cost of moving from `from` to the adjacent cell `to`.
fn step_cost(from: Cell, to: Cell) -> i32 {
    if to[1] > from[1] {
        COST_STEP_UP
    } else if to[1] < from[1] {
        COST_FALL + COST_FALL_PER_BLOCK * (from[1] - to[1] - 1)
    } else {
        COST_STRAIGHT
    }
}

/// Find a path from `start` to `goal`, or `None`.
///
/// The returned path excludes `start` and ends with `goal` (or the closest
/// standable cell to it), so a caller can walk it as a queue of waypoints.
///
/// Returns `None` when: the budget is exhausted, the goal is too far, the box
/// cannot be allocated, or the goal is genuinely unreachable. Callers treat all
/// four the same way — steer directly — because a buddy that refuses to move is
/// worse than a buddy that walks into a wall.
pub fn find_path<W: VoxelQuery + ?Sized>(
    world: &W,
    start: Cell,
    goal: Cell,
    limits: PathLimits,
) -> Option<Vec<Cell>> {
    let start = snap_to_ground(world, start)?;
    let goal = snap_to_ground(world, goal).unwrap_or(goal);

    if start == goal {
        return Some(Vec::new());
    }

    let bounds = SearchBox::around(start, goal, limits.y_margin)?;
    let len = bounds.len();
    if len == 0 {
        return None;
    }

    let mut g_score = vec![i32::MAX; len];
    let mut came_from = vec![-1i32; len];

    let start_index = bounds.index(start)?;
    // `SearchBox::around` guarantees the goal is inside, but the margin is a
    // number and this is a search that must never index out of bounds.
    bounds.index(goal)?;
    g_score[start_index] = 0;

    let mut open: std::collections::BinaryHeap<Node> = std::collections::BinaryHeap::new();
    open.push(Node {
        f: heuristic(start, goal),
        cell: start,
    });

    let mut expansions = 0u32;
    let mut neighbours = [[0i32; 3]; MAX_NEIGHBOURS];

    while let Some(Node { cell, .. }) = open.pop() {
        if cell == goal {
            return Some(reconstruct(&bounds, &came_from, goal));
        }
        if expansions >= limits.budget {
            return None;
        }
        expansions += 1;

        let Some(here) = bounds.index(cell) else {
            continue;
        };
        let here_g = g_score[here];
        if here_g == i32::MAX {
            continue;
        }

        let count = expand(world, cell, limits.max_fall, &mut neighbours);
        for next in &neighbours[..count] {
            let Some(next_index) = bounds.index(*next) else {
                // A neighbour outside the box is simply not searched. The box
                // already covers start and goal plus a margin, so this only
                // clips the fringe of a detour.
                continue;
            };
            let tentative = here_g.saturating_add(step_cost(cell, *next));
            if tentative >= g_score[next_index] {
                continue;
            }
            g_score[next_index] = tentative;
            came_from[next_index] = here as i32;
            open.push(Node {
                f: tentative.saturating_add(heuristic(*next, goal)),
                cell: *next,
            });
        }
    }

    None
}

/// Walk `came_from` back from `goal` into a start-to-goal cell list.
fn reconstruct(bounds: &SearchBox, came_from: &[i32], goal: Cell) -> Vec<Cell> {
    let Some(goal_index) = bounds.index(goal) else {
        return Vec::new();
    };
    let mut path = Vec::new();
    let mut current = goal_index;
    // The bound is the array length, so a corrupted `came_from` cannot loop.
    for _ in 0..came_from.len() {
        let cell = bounds.cell_of(current);
        if came_from[current] < 0 {
            break;
        }
        path.push(cell);
        current = came_from[current] as usize;
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_query::VoxelGrid;

    /// A 40 x 10 x 40 grid with a solid floor at y = 0.
    fn floor_world() -> VoxelGrid {
        let mut w = VoxelGrid::new([0, 0, 0], [40, 10, 40]);
        w.fill_layer(0, true);
        w
    }

    #[test]
    fn a_straight_line_is_found() {
        let w = floor_world();
        let path = find_path(&w, [2, 1, 2], [2, 1, 12], PathLimits::default())
            .expect("open floor must be walkable");
        assert_eq!(path.last(), Some(&[2, 1, 12]));
        assert_eq!(path.len(), 10, "ten steps, no detours");
    }

    #[test]
    fn a_path_goes_around_a_wall() {
        let mut w = floor_world();
        // A wall from z = 6, x = 0..5. The gap at x >= 5 has to stay inside
        // the search box (start/goal +/- 4), which is the point of the test:
        // a detour the box cannot see is a path the buddy will not take.
        for x in 0..5 {
            for y in 1..4 {
                w.set_solid(x, y, 6, true);
            }
        }
        let path = find_path(&w, [2, 1, 2], [2, 1, 12], PathLimits::default())
            .expect("there is a gap to the east");
        assert_eq!(path.last(), Some(&[2, 1, 12]));
        assert!(
            path.iter().any(|c| c[0] >= 5),
            "the path must go through the gap, got {path:?}"
        );
    }

    #[test]
    fn a_sealed_room_returns_no_path_and_does_not_stall() {
        let mut w = floor_world();
        // Seal the start in a 3 x 3 box of walls two blocks tall.
        for x in 0..6 {
            for z in 0..6 {
                let on_edge = x == 0 || z == 0 || x == 5 || z == 5;
                if on_edge {
                    for y in 1..4 {
                        w.set_solid(x, y, z, true);
                    }
                }
            }
        }
        let started = std::time::Instant::now();
        let path = find_path(&w, [2, 1, 2], [20, 1, 20], PathLimits::default());
        let elapsed_us = started.elapsed().as_micros();
        assert_eq!(path, None, "the start is walled in");
        // The budget must bound the work, not the map size.
        assert!(
            elapsed_us < 5_000,
            "a failed search took {elapsed_us} us; the budget is not being applied"
        );
    }

    #[test]
    fn budget_is_enforced() {
        let w = floor_world();
        // A budget of one expansion cannot reach a goal 20 cells away.
        let tight = PathLimits {
            budget: 1,
            ..PathLimits::default()
        };
        assert_eq!(find_path(&w, [2, 1, 2], [2, 1, 22], tight), None);

        // The same search succeeds with the real budget.
        assert!(find_path(&w, [2, 1, 2], [2, 1, 22], PathLimits::default()).is_some());
    }

    #[test]
    fn a_far_goal_is_refused_without_searching() {
        let w = floor_world();
        let goal = [2, 1, MAX_SEARCH_EXTENT + 40];
        assert_eq!(find_path(&w, [2, 1, 2], goal, PathLimits::default()), None);
    }

    #[test]
    fn step_up_and_fall_are_used() {
        let mut w = floor_world();
        // A one-block step at z = 6, x = 0..40: raise the floor.
        for x in 0..40 {
            w.set_solid(x, 1, 6, true);
            for z in 6..40 {
                w.set_solid(x, 1, z, true);
            }
        }
        // Now y = 2 is the walking surface past z = 6.
        let path = find_path(&w, [2, 1, 2], [2, 2, 12], PathLimits::default())
            .expect("a one-block step is climbable");
        assert_eq!(path.last(), Some(&[2, 2, 12]));
        assert!(
            path.iter().any(|c| c[1] == 2),
            "the path must climb, got {path:?}"
        );
    }

    #[test]
    fn snap_to_ground_finds_the_floor() {
        let w = floor_world();
        assert_eq!(snap_to_ground(&w, [5, 4, 5]), Some([5, 1, 5]));
        assert_eq!(snap_to_ground(&w, [5, 1, 5]), Some([5, 1, 5]));
    }

    #[test]
    fn a_start_equal_to_the_goal_is_an_empty_path() {
        let w = floor_world();
        assert_eq!(
            find_path(&w, [3, 1, 3], [3, 1, 3], PathLimits::default()),
            Some(Vec::new())
        );
    }

    #[test]
    fn paths_are_deterministic() {
        let mut w = floor_world();
        for x in 0..5 {
            for y in 1..3 {
                w.set_solid(x, y, 8, true);
            }
        }
        let first = find_path(&w, [2, 1, 2], [2, 1, 18], PathLimits::default());
        for _ in 0..8 {
            assert_eq!(
                find_path(&w, [2, 1, 2], [2, 1, 18], PathLimits::default()),
                first
            );
        }
    }
}
