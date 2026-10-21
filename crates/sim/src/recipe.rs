//! Recipe matching: turning a pile of crates into a thing.
//!
//! This is the highest-risk system in the plan (§4.5, R-02) for one reason:
//! it is the only place where the game can be *wrong* in a way that is not
//! merely unbalanced. A matcher that fires on near-misses hands out free units;
//! one that fires twice hands out two. Both are economy-breaking, and neither
//! is caught by "does it feel right" testing.
//!
//! So the design is deliberately boring:
//!
//! * A cluster is a 6-connected component of crates, capped at
//!   [`MAX_CLUSTER`]. The cap is checked *during* the fill, not after, so a
//!   64-crate wall costs 33 steps and then stops (§4.5.4).
//! * Eligibility is four independent predicates — size, ground support,
//!   stability, and (structurally, via [`Pad`]) pad containment.
//! * A shape's identity is a canonical byte string: normalise to the origin,
//!   rotate through the four Y-rotations, re-normalise, sort, pack, and take
//!   the lexicographic minimum (§4.5.2). Matching is `==` on that string.
//!
//! There is no fuzzy matching, no subset matching, and no "close enough". A
//! shape that is not exactly a recipe matches nothing, which is what makes the
//! near-miss feedback in §4.5.5 honest: the client runs the same canonicaliser,
//! so it cannot promise a craft the server will refuse.
//!
//! # Two deviations from the plan, both recorded rather than hidden
//!
//! 1. §4.5.2's pseudocode rotates the *sorted* offset list and then packs it
//!    without re-sorting. Rotating a sorted list unsorts it, and rotating about
//!    the origin also pushes coordinates negative. This implementation
//!    re-normalises and re-sorts after each rotation, which is what makes the
//!    four keys comparable at all.
//! 2. §12.3 rows 5 and 6 ("floating" and "one crate unsupported") are the same
//!    predicate once you assume 6-connectivity and a uniformly solid pad floor:
//!    a crate directly under a cluster member is 6-adjacent, so it is *in* the
//!    cluster, so the cluster's lowest layer is always its minimum. Rule 3
//!    therefore reduces to "the lowest layer sits on `y = 0`". The general form
//!    is still implemented, because it is what the spec says and because a
//!    future "crates fall" rule would need it.

use serde::Deserialize;

use crate::buddy::BuddyKind;
use crate::pad::{Pad, PAD_CELLS};
use crate::time::Tick;

/// A pad-local block coordinate.
pub type BlockPos = [i32; 3];

/// Largest cluster the matcher will consider (§4.5.1).
///
/// The largest recipe is 24 blocks, so 32 is headroom rather than a magic
/// number; anything bigger is rejected without scanning the rest of it.
pub const MAX_CLUSTER: usize = 32;

/// Smallest cluster the matcher will consider (§4.5.1 rule 2, §4.5.4).
pub const MIN_CLUSTER: usize = 4;

/// Rotations about the Y axis that a shape is invariant under.
pub const YAW_STEPS: u32 = 4;

/// The six face neighbours, in a fixed order.
///
/// The order does not affect the result (the canonical form sorts), but a fixed
/// order keeps the flood fill's allocation pattern identical on every target,
/// which keeps timing tests honest.
const NEIGHBOURS: [BlockPos; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

// ---------------------------------------------------------------------------
// Recipes
// ---------------------------------------------------------------------------

/// Identifies a recipe within a [`RecipeBook`].
///
/// An index rather than a string: it is `Copy`, it is two bytes on the wire,
/// and comparing it cannot accidentally depend on locale or case.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RecipeId(pub u16);

/// What a recipe produces.
///
/// The weapon and structure *stats* deliberately live elsewhere (T-011's combat
/// table): a recipe says what you get, not how hard it hits. Keeping them apart
/// means retuning a sword does not invalidate a shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
pub enum RecipeOutput {
    /// Melee pickup.
    Sword,
    /// Projectile pickup.
    Bow,
    /// Consumable heal.
    Bandage,
    /// Long-reach melee pickup.
    Spear,
    /// Static defensive structure.
    Tower,
    /// Mobile siege structure.
    Ram,
    /// Ranged siege structure.
    Catapult,
    /// Base buff.
    Ward,
    /// A new buddy.
    Buddy(BuddyKind),
}

impl RecipeOutput {
    /// The buddy this output spawns, if it spawns one.
    pub fn buddy(self) -> Option<BuddyKind> {
        match self {
            RecipeOutput::Buddy(k) => Some(k),
            _ => None,
        }
    }
}

/// One row of `data/recipes.ron`.
#[derive(Clone, Debug)]
pub struct Recipe {
    /// Index within the book.
    pub id: RecipeId,
    /// Stable authoring name, e.g. `R01_SWORD`.
    pub name: String,
    /// What a match produces.
    pub output: RecipeOutput,
    /// Match time in seconds at which this recipe becomes craftable.
    pub unlock_s: u32,
    /// Per-team cooldown in seconds.
    pub cooldown_s: u32,
    /// Pad-local shape, as authored.
    pub blocks: Vec<BlockPos>,
    key: Vec<u8>,
}

impl Recipe {
    /// Block count. The cost of a recipe *is* its block count (§4.5.3).
    pub fn cost(&self) -> u32 {
        self.blocks.len() as u32
    }

    /// True when the recipe is available at `elapsed_s` into the match.
    pub fn unlocked_at(&self, elapsed_s: u32) -> bool {
        elapsed_s >= self.unlock_s
    }

    /// The canonical form used for matching (§4.5.2).
    pub fn canonical_key(&self) -> &[u8] {
        &self.key
    }
}

/// A recipe table that failed to load.
///
/// Carries the offending recipe's name so the message is actionable without a
/// debugger, which is the whole point of validating at load.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RecipeError {
    /// Authoring name of the offending recipe, or `<document>`.
    pub recipe: String,
    /// What is wrong with it.
    pub reason: String,
}

impl core::fmt::Display for RecipeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid recipe `{}`: {}", self.recipe, self.reason)
    }
}

impl std::error::Error for RecipeError {}

/// The deserialised shape of one row.
///
/// `blocks` is `Vec<Vec<i32>>` rather than `Vec<[i32; 3]>` on purpose: RON
/// writes fixed-size arrays as tuples (`(0, 0, 0)`), and the authoring file is
/// the one place a human has to read a grid, so it keeps `[0, 0, 0]`. The
/// conversion below is where a malformed triple is caught.
#[derive(Debug, Deserialize)]
struct RecipeFile {
    id: String,
    output: RecipeOutput,
    unlock_s: u32,
    cooldown_s: u32,
    blocks: Vec<Vec<i32>>,
}

/// Convert an authored triple, naming the recipe and the offending row.
fn to_positions(recipe: &str, rows: &[Vec<i32>]) -> Result<Vec<BlockPos>, RecipeError> {
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let [x, y, z] = match row.as_slice() {
            [x, y, z] => [*x, *y, *z],
            other => {
                return Err(RecipeError {
                    recipe: recipe.to_string(),
                    reason: format!("block {i} is {other:?}, which is not an [x, y, z] triple"),
                })
            }
        };
        out.push([x, y, z]);
    }
    Ok(out)
}

/// Every recipe, in file order.
#[derive(Clone, Debug)]
pub struct RecipeBook {
    recipes: Vec<Recipe>,
}

impl RecipeBook {
    /// Parse and validate a RON document.
    ///
    /// Validation is not optional politeness: §12.3 requires that a colliding
    /// canonical form is a *load* error, because two recipes that share a form
    /// make one of them unreachable and the symptom is "that recipe does not
    /// work" three weeks later.
    pub fn parse(src: &str) -> Result<RecipeBook, RecipeError> {
        let rows: Vec<RecipeFile> = ron::from_str(src).map_err(|e| RecipeError {
            recipe: "<document>".to_string(),
            reason: format!("not valid RON for the recipe schema: {e}"),
        })?;
        RecipeBook::from_rows(rows)
    }

    fn from_rows(rows: Vec<RecipeFile>) -> Result<RecipeBook, RecipeError> {
        if rows.is_empty() {
            return Err(RecipeError {
                recipe: "<document>".to_string(),
                reason: "the recipe table is empty".to_string(),
            });
        }

        let mut recipes: Vec<Recipe> = Vec::with_capacity(rows.len());

        for (index, row) in rows.into_iter().enumerate() {
            let blocks = to_positions(&row.id, &row.blocks)?;
            let fail = |reason: String| RecipeError {
                recipe: row.id.clone(),
                reason,
            };

            if row.id.is_empty() {
                return Err(fail("id must not be empty".to_string()));
            }
            if recipes.iter().any(|r| r.name == row.id) {
                return Err(fail("duplicate id".to_string()));
            }
            if row.cooldown_s == 0 {
                // §4.5.4: every output has a cooldown >= 15 s precisely so
                // that spam cannot be free. Zero would re-open that hole.
                return Err(fail("cooldown_s must be > 0".to_string()));
            }
            if blocks.len() < MIN_CLUSTER {
                return Err(fail(format!(
                    "{} blocks is below the {} block minimum",
                    blocks.len(),
                    MIN_CLUSTER
                )));
            }
            if blocks.len() > MAX_CLUSTER {
                return Err(fail(format!(
                    "{} blocks exceeds the {} block cap",
                    blocks.len(),
                    MAX_CLUSTER
                )));
            }
            for b in &blocks {
                if !Pad::in_bounds(*b) {
                    return Err(fail(format!("block {b:?} is outside the pad volume")));
                }
            }
            if !blocks.iter().enumerate().all(|(i, a)| {
                blocks
                    .iter()
                    .skip(i + 1)
                    .all(|b| a[0] != b[0] || a[1] != b[1] || a[2] != b[2])
            }) {
                return Err(fail("contains a duplicate coordinate".to_string()));
            }
            // §4.5.1 rule 3: the lowest layer must rest on the pad floor.
            // A recipe authored above y = 0 could never satisfy it.
            let min_y = blocks.iter().map(|b| b[1]).min().unwrap_or(0);
            if min_y != 0 {
                return Err(fail(format!(
                    "lowest layer is y = {min_y}; it must be y = 0 to rest on the pad floor"
                )));
            }
            if !is_connected(&blocks) {
                return Err(fail(
                    "shape is not a single 6-connected component, so it can never be \
                     built as one cluster"
                        .to_string(),
                ));
            }

            let key = canonicalize(&blocks);
            if let Some(other) = recipes.iter().find(|r| r.key == key) {
                return Err(fail(format!(
                    "canonical form collides with `{}`; one of the two would be unreachable",
                    other.name
                )));
            }

            recipes.push(Recipe {
                id: RecipeId(index as u16),
                name: row.id,
                output: row.output,
                unlock_s: row.unlock_s,
                cooldown_s: row.cooldown_s,
                blocks,
                key,
            });
        }

        Ok(RecipeBook { recipes })
    }

    /// Every recipe, in file order.
    pub fn recipes(&self) -> &[Recipe] {
        &self.recipes
    }

    /// Look up a recipe by id.
    pub fn get(&self, id: RecipeId) -> Option<&Recipe> {
        self.recipes.get(id.0 as usize)
    }

    /// Look up a recipe by authoring name.
    pub fn by_name(&self, name: &str) -> Option<&Recipe> {
        self.recipes.iter().find(|r| r.name == name)
    }

    /// Match a canonical form against the whole table.
    ///
    /// A linear scan of 14 short byte strings. A hash map would be faster and
    /// would also introduce an iteration-order dependency the moment anyone
    /// iterated it, so the scan stays.
    pub fn match_key(&self, key: &[u8]) -> Option<RecipeId> {
        self.recipes.iter().find(|r| r.key == key).map(|r| r.id)
    }

    /// Number of recipes.
    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    /// True when the book has no recipes.
    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }

    /// The recipe table embedded in the binary at compile time.
    pub fn embedded() -> &'static RecipeBook {
        static CELL: std::sync::OnceLock<&'static RecipeBook> = std::sync::OnceLock::new();
        CELL.get_or_init(|| {
            let book = RecipeBook::parse(EMBEDDED_RON)
                .expect("embedded data/recipes.ron is invalid; see the load-time rules there");
            Box::leak(Box::new(book))
        })
    }
}

/// A recipe a placed cluster is close to.
///
/// The missing blocks are what the player still has to add; the count is the
/// "needs N more" number in the HUD. Keeping the blocks (not just the count)
/// lets the test complete the shape and ask the real matcher whether the
/// prediction was true.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NearMiss {
    /// The recipe that would match once the missing blocks are placed.
    pub recipe: RecipeId,
    /// The blocks the recipe has and the cluster does not.
    pub missing_blocks: Vec<BlockPos>,
}

impl NearMiss {
    /// How many more crates the completion needs.
    pub fn missing(&self) -> u32 {
        self.missing_blocks.len() as u32
    }
}

/// The nearest unlocked recipe within two missing blocks, or `None`.
///
/// This is the same canonicaliser the matcher uses, run over rotations and
/// translations of the placed cluster. A cluster that already matches returns
/// `None`: exact matches are the matcher's answer, not a near miss.
///
/// The prediction is one-sided on purpose. A cluster with an extra block is not
/// reported as "needs one more", because adding the missing block would still
/// leave the extra in place and would not craft. Only a cluster that is a
/// subset of a recipe can be completed by adding blocks, so only that case is
/// predicted.
pub fn near_miss(blocks: &[BlockPos], book: &RecipeBook, elapsed_s: u32) -> Option<NearMiss> {
    if blocks.is_empty() || blocks.len() > MAX_CLUSTER {
        return None;
    }

    let mut cluster = blocks.to_vec();
    cluster.sort_unstable();
    let before = cluster.len();
    cluster.dedup();
    if cluster.len() != before {
        return None;
    }
    if cluster.iter().map(|b| b[1]).min() != Some(0) || !is_connected(&cluster) {
        return None;
    }
    if book.match_key(&canonicalize(&cluster)).is_some() {
        return None;
    }

    let mut best: Option<(u32, RecipeId, Vec<BlockPos>)> = None;
    for recipe in book.recipes() {
        if !recipe.unlocked_at(elapsed_s) {
            continue;
        }
        let cost = recipe.cost() as usize;
        if cluster.len() >= cost || cost - cluster.len() > 2 {
            continue;
        }
        let mut recipe_sorted = recipe.blocks.clone();
        recipe_sorted.sort_unstable();

        for turns in 0..YAW_STEPS {
            let rotated = rotate_y(&cluster, turns);
            for anchor in &recipe_sorted {
                if anchor[1] != 0 {
                    // The cluster's lowest layer is y = 0; a vertical shift
                    // would make the completion float.
                    continue;
                }
                for first in rotated.iter().filter(|b| b[1] == 0) {
                    let dx = anchor[0] - first[0];
                    let dz = anchor[2] - first[2];
                    let mut placed = Vec::with_capacity(rotated.len());
                    let mut inside = true;
                    for b in &rotated {
                        let p = [b[0] + dx, b[1], b[2] + dz];
                        if recipe_sorted.binary_search(&p).is_err() {
                            inside = false;
                            break;
                        }
                        placed.push(p);
                    }
                    if !inside {
                        continue;
                    }
                    placed.sort_unstable();
                    if placed.windows(2).any(|pair| pair[0] == pair[1]) {
                        continue;
                    }
                    let missing: Vec<BlockPos> = recipe_sorted
                        .iter()
                        .copied()
                        .filter(|p| placed.binary_search(p).is_err())
                        .collect();
                    if missing.is_empty() {
                        continue;
                    }
                    // Return the missing blocks in the cluster's own frame, so
                    // adding them to the original placement completes the
                    // shape. The UI only needs the count, but the parity test
                    // completes the cluster and asks the matcher.
                    let undo = (YAW_STEPS - turns) % YAW_STEPS;
                    let missing_local: Vec<BlockPos> = missing
                        .iter()
                        .map(|p| {
                            let unshifted = [p[0] - dx, p[1], p[2] - dz];
                            rotate_y(&[unshifted], undo)[0]
                        })
                        .collect();
                    let better = match &best {
                        None => true,
                        Some((current, id, _)) => {
                            missing_local.len() < *current as usize
                                || (missing_local.len() == *current as usize && recipe.id < *id)
                        }
                    };
                    if better {
                        best = Some((missing_local.len() as u32, recipe.id, missing_local));
                    }
                }
            }
        }
    }

    best.map(|(_, recipe, missing_blocks)| NearMiss {
        recipe,
        missing_blocks,
    })
}

/// Raw embedded RON, exposed so tests can re-parse and doctor it.
pub const EMBEDDED_RON: &str = include_str!("../../../data/recipes.ron");

// ---------------------------------------------------------------------------
// Canonical form (§4.5.2)
// ---------------------------------------------------------------------------

/// Translate a shape so its minimum corner is the origin, then sort.
fn normalise(blocks: &[BlockPos]) -> Vec<BlockPos> {
    if blocks.is_empty() {
        return Vec::new();
    }
    let mut min = blocks[0];
    for b in blocks {
        for (axis, value) in b.iter().enumerate() {
            if *value < min[axis] {
                min[axis] = *value;
            }
        }
    }
    let mut out: Vec<BlockPos> = blocks
        .iter()
        .map(|b| [b[0] - min[0], b[1] - min[1], b[2] - min[2]])
        .collect();
    out.sort_unstable();
    out
}

/// Rotate a shape by `k * 90` degrees about the Y axis.
///
/// `(x, z) -> (-z, x)` is the counter-clockwise quarter turn; four applications
/// are the identity, so `k` is taken modulo 4.
fn rotate_y(blocks: &[BlockPos], k: u32) -> Vec<BlockPos> {
    let turns = k % YAW_STEPS;
    blocks
        .iter()
        .map(|b| {
            let (mut x, mut z) = (b[0], b[2]);
            for _ in 0..turns {
                let next_x = -z;
                let next_z = x;
                x = next_x;
                z = next_z;
            }
            [x, b[1], z]
        })
        .collect()
}

/// Pack sorted offsets into a byte string, 6 bytes per block.
///
/// `i16` little-endian per component rather than a varint: fixed stride keeps
/// two keys of different shapes from ever comparing as a prefix of each other,
/// and `i16` covers the whole pad with room to spare, so no coordinate can be
/// truncated into a collision.
fn pack(blocks: &[BlockPos]) -> Vec<u8> {
    let mut out = Vec::with_capacity(blocks.len() * 6);
    for b in blocks {
        for value in b {
            out.extend_from_slice(&(*value as i16).to_le_bytes());
        }
    }
    out
}

/// The canonical, rotation-invariant byte string for a shape (§4.5.2).
///
/// Two shapes are the same recipe if and only if their canonical forms are
/// byte-identical. Rotation about Y is quotiented out; reflection and rotation
/// about X or Z are **not**, because a bow lying on its side is a different
/// object to a bow standing up, and allowing reflections would double the
/// number of shapes a player has to learn.
pub fn canonicalize(blocks: &[BlockPos]) -> Vec<u8> {
    if blocks.is_empty() {
        return Vec::new();
    }
    let base = normalise(blocks);
    let mut best: Option<Vec<u8>> = None;
    for k in 0..YAW_STEPS {
        // Re-normalise after rotating: a rotation about the origin moves the
        // minimum corner, so the offsets are no longer anchored at zero.
        let key = pack(&normalise(&rotate_y(&base, k)));
        let better = match &best {
            None => true,
            Some(current) => key < *current,
        };
        if better {
            best = Some(key);
        }
    }
    best.expect("at least one rotation is always produced")
}

/// True when `blocks` form one 6-connected component.
///
/// O(n^2) with n <= 32, and only ever called at load. A recipe split into two
/// pieces could never be matched, because a cluster is by definition connected.
fn is_connected(blocks: &[BlockPos]) -> bool {
    if blocks.is_empty() {
        return true;
    }
    let mut seen = vec![false; blocks.len()];
    let mut stack = vec![0usize];
    seen[0] = true;
    let mut visited = 0usize;
    while let Some(i) = stack.pop() {
        visited += 1;
        for d in NEIGHBOURS {
            let want = [
                blocks[i][0] + d[0],
                blocks[i][1] + d[1],
                blocks[i][2] + d[2],
            ];
            for (j, b) in blocks.iter().enumerate() {
                if !seen[j] && *b == want {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
    }
    visited == blocks.len()
}

// ---------------------------------------------------------------------------
// Matching (§4.5.1)
// ---------------------------------------------------------------------------

/// One matched cluster, ready to be consumed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Craft {
    /// Which recipe matched.
    pub recipe: RecipeId,
    /// The pad-local blocks it consumed.
    pub blocks: Vec<BlockPos>,
}

/// The result of one crafting pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CraftOutcome {
    /// A recipe fired. Its blocks are already gone and the output must be
    /// spawned in this same tick.
    Crafted(Craft),
    /// A recipe matched but the caller refused it — the team is at its buddy
    /// cap, say. **Nothing was consumed** (§4.5.4), so the player keeps their
    /// crates and can try again once a slot frees up.
    Blocked(Craft),
}

impl CraftOutcome {
    /// The recipe involved, whichever way the pass went.
    pub fn recipe(&self) -> RecipeId {
        match self {
            CraftOutcome::Crafted(c) | CraftOutcome::Blocked(c) => c.recipe,
        }
    }

    /// The blocks involved, whichever way the pass went.
    pub fn blocks(&self) -> &[BlockPos] {
        match self {
            CraftOutcome::Crafted(c) | CraftOutcome::Blocked(c) => &c.blocks,
        }
    }

    /// True when the pass actually produced something.
    pub fn crafted(&self) -> bool {
        matches!(self, CraftOutcome::Crafted(_))
    }
}

/// A 6-connected component found by the flood fill.
struct Cluster {
    blocks: Vec<BlockPos>,
    /// True when the component exceeded [`MAX_CLUSTER`] and the fill stopped.
    overflowed: bool,
}

/// Flood fill from `seed`, marking `visited` as it goes.
///
/// Aborts the moment the component is known to be too big (§4.5.4), which is
/// what bounds a 64-crate wall to 33 steps instead of 64.
fn fill(pad: &Pad, seed: BlockPos, visited: &mut [bool; PAD_CELLS]) -> Cluster {
    let mut blocks = Vec::with_capacity(8);
    let mut stack = Vec::with_capacity(8);
    if let Some(i) = Pad::index_of(seed) {
        visited[i] = true;
    }
    stack.push(seed);
    while let Some(p) = stack.pop() {
        blocks.push(p);
        if blocks.len() > MAX_CLUSTER {
            return Cluster {
                blocks,
                overflowed: true,
            };
        }
        for d in NEIGHBOURS {
            let n = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
            let Some(i) = Pad::index_of(n) else {
                continue;
            };
            if visited[i] || !pad.get(n) {
                continue;
            }
            visited[i] = true;
            stack.push(n);
        }
    }
    Cluster {
        blocks,
        overflowed: false,
    }
}

/// §4.5.1 rule 3: every crate in the cluster's lowest layer has something
/// solid beneath it.
///
/// The pad floor is the `y == 0` case. Above that, "something solid" means a
/// crate — and because a crate directly underneath would be 6-adjacent, it
/// would already be a member of the cluster, so the layer below the lowest
/// layer is empty by construction. The check is written in its general form
/// anyway: it is what the spec says, and it stays correct if crates ever gain
/// gravity.
fn ground_supported(pad: &Pad, cluster: &Cluster) -> bool {
    let min_y = cluster.blocks.iter().map(|b| b[1]).min().unwrap_or(0);
    for b in &cluster.blocks {
        if b[1] != min_y {
            continue;
        }
        if b[1] == 0 {
            continue; // the pad floor
        }
        if !pad.get([b[0], b[1] - 1, b[2]]) {
            return false;
        }
    }
    true
}

/// Find the first cluster on `pad` that matches an unlocked recipe.
///
/// Clusters are visited in [`Pad::occupied`] order — `y` slowest, then `z`,
/// then `x` — so "which one crafts first" is a property of the pad's contents
/// and not of a hash map's mood. That is the whole determinism story for this
/// function; there is nothing else to get wrong.
///
/// `elapsed_s` gates tier 2 and tier 3 recipes. A locked shape returns `None`
/// rather than consuming the player's crates, which is the only defensible
/// behaviour: the client's recipe book greys the recipe out, so a player who
/// builds it early has made a mistake, not a wager.
pub fn find_match(pad: &Pad, book: &RecipeBook, elapsed_s: u32) -> Option<Craft> {
    if pad.is_empty() {
        return None;
    }
    // Collect first: `occupied()` borrows the pad and the caller may want to
    // mutate it afterwards.
    let seeds: Vec<BlockPos> = pad.occupied().collect();
    let mut visited = [false; PAD_CELLS];

    for seed in seeds {
        let Some(i) = Pad::index_of(seed) else {
            continue;
        };
        if visited[i] {
            continue;
        }
        let cluster = fill(pad, seed, &mut visited);
        if cluster.overflowed || cluster.blocks.len() < MIN_CLUSTER {
            continue;
        }
        if !ground_supported(pad, &cluster) {
            continue;
        }
        let key = canonicalize(&cluster.blocks);
        if let Some(recipe) = book.match_key(&key) {
            if !book
                .get(recipe)
                .map(|r| r.unlocked_at(elapsed_s))
                .unwrap_or(false)
            {
                continue;
            }
            return Some(Craft {
                recipe,
                blocks: cluster.blocks,
            });
        }
    }
    None
}

/// Run one crafting pass, allowing every recipe.
pub fn tick(pad: &mut Pad, book: &RecipeBook, now: Tick, elapsed_s: u32) -> Option<CraftOutcome> {
    tick_with(pad, book, now, elapsed_s, |_| true)
}

/// Run one crafting pass, consuming the match atomically.
///
/// `allowed` is the caller's veto — in a real match it answers "is this team
/// under its buddy cap for this output?". A refused match **consumes nothing**
/// and restarts the stability window, so a player standing at the cap gets one
/// event every 10 ticks rather than twenty per second.
///
/// Returns `None` while the pad is still moving, and `Some` at most once per
/// stability window. Two callers in the same tick cannot both win: the first
/// call empties the pad and resets the clock.
pub fn tick_with(
    pad: &mut Pad,
    book: &RecipeBook,
    now: Tick,
    elapsed_s: u32,
    allowed: impl Fn(RecipeId) -> bool,
) -> Option<CraftOutcome> {
    if !pad.tick_stability(now) {
        return None;
    }
    let craft = find_match(pad, book, elapsed_s)?;
    if !allowed(craft.recipe) {
        pad.reset_stability(now);
        return Some(CraftOutcome::Blocked(craft));
    }
    pad.consume(&craft.blocks);
    pad.reset_stability(now);
    Some(CraftOutcome::Crafted(craft))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sword() -> Vec<BlockPos> {
        vec![
            [0, 0, 0],
            [1, 0, 0],
            [2, 0, 0],
            [0, 0, 1],
            [1, 0, 1],
            [2, 0, 1],
        ]
    }

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
    }

    #[test]
    fn near_miss_never_lies() {
        let book = RecipeBook::embedded();
        let mut rng = Lcg(0x5EED_1234);
        let mut positives = 0;

        for _ in 0..500 {
            let recipe = &book.recipes()[(rng.next() as usize) % book.recipes().len()];
            let turns = (rng.next() % YAW_STEPS as u64) as u32;
            let rotated = rotate_y(&recipe.blocks, turns);
            let min_x = rotated.iter().map(|b| b[0]).min().unwrap();
            let max_x = rotated.iter().map(|b| b[0]).max().unwrap();
            let min_z = rotated.iter().map(|b| b[2]).min().unwrap();
            let max_z = rotated.iter().map(|b| b[2]).max().unwrap();
            let span_x = (max_x - min_x) as u64;
            let span_z = (max_z - min_z) as u64;
            let dx = (rng.next() % (8 - span_x)) as i32 - min_x;
            let dz = (rng.next() % (8 - span_z)) as i32 - min_z;

            let mut placed: Vec<BlockPos> = rotated
                .iter()
                .map(|b| [b[0] + dx, b[1], b[2] + dz])
                .collect();
            let remove = (rng.next() % 4) as usize;
            for _ in 0..remove {
                let index = (rng.next() as usize) % placed.len();
                placed.swap_remove(index);
            }
            if placed.is_empty() {
                continue;
            }

            let exact = book.match_key(&canonicalize(&placed));
            let predicted = near_miss(&placed, book, u32::MAX);

            if let Some(near) = predicted {
                positives += 1;
                assert!(
                    exact.is_none(),
                    "near miss predicted for an exact match: {placed:?}"
                );
                assert!(near.missing() <= 2);
                let mut completed = placed.clone();
                completed.extend(near.missing_blocks.iter().copied());
                assert_eq!(
                    book.match_key(&canonicalize(&completed)),
                    Some(near.recipe),
                    "completing a near miss did not match the predicted recipe: {placed:?}"
                );
            } else if let Some(id) = exact {
                assert!(book.get(id).is_some());
            }
        }

        assert!(
            positives >= 50,
            "the fixture must exercise near misses; got {positives}"
        );
    }

    #[test]
    fn embedded_table_loads_and_is_unambiguous() {
        let book = RecipeBook::embedded();
        assert_eq!(book.len(), 14, "§4.5.3 lists 14 recipes");

        // §12.3 "ambiguous shapes": every canonical form is distinct.
        for (i, a) in book.recipes().iter().enumerate() {
            for b in book.recipes().iter().skip(i + 1) {
                assert_ne!(
                    a.canonical_key(),
                    b.canonical_key(),
                    "{} and {} share a canonical form",
                    a.name,
                    b.name
                );
            }
        }
    }

    #[test]
    fn tier_boundaries_match_the_plan() {
        let book = RecipeBook::embedded();
        let by_name = |n: &str| book.by_name(n).expect("recipe must exist");
        assert_eq!(by_name("R01_SWORD").unlock_s, 0);
        assert_eq!(by_name("R05_ARCHER").unlock_s, 180);
        assert_eq!(by_name("R10_RAM").unlock_s, 480);
        // Cost is the block count and nothing else.
        assert_eq!(by_name("R01_SWORD").cost(), 6);
        assert_eq!(by_name("R10_RAM").cost(), 24);
        assert_eq!(by_name("R03_BANDAGE").cost(), 4);
    }

    #[test]
    fn canonical_form_ignores_the_four_yaw_rotations() {
        let base = sword();
        let key = canonicalize(&base);
        for k in 0..YAW_STEPS {
            let rotated = normalise(&rotate_y(&base, k));
            assert_eq!(canonicalize(&rotated), key, "rotation {k} changed the key");
        }
        // A 90 degree turn of a 3x2 slab is a 2x3 slab, which must be equal.
        let turned: Vec<BlockPos> = base.iter().map(|b| [-b[2], b[1], b[0]]).collect();
        assert_eq!(canonicalize(&turned), key);
    }

    #[test]
    fn canonical_form_ignores_translation() {
        let base = sword();
        let shifted: Vec<BlockPos> = base
            .iter()
            .map(|b| [b[0] + 5, b[1] + 2, b[2] + 3])
            .collect();
        assert_eq!(canonicalize(&shifted), canonicalize(&base));
    }

    #[test]
    fn canonical_form_distinguishes_mirrors() {
        // A mirror is a different object: reflections are deliberately not
        // quotiented out (see `canonicalize`). This uses the L-pentomino, not
        // the V-pentomino — V is mirror-symmetric about its diagonal, so its
        // mirror *is* a rotation of itself and would prove nothing.
        let l: Vec<BlockPos> = vec![[0, 0, 0], [1, 0, 0], [2, 0, 0], [3, 0, 0], [0, 0, 1]];
        let mirrored: Vec<BlockPos> = l.iter().map(|b| [-b[0], b[1], b[2]]).collect();
        assert_ne!(canonicalize(&l), canonicalize(&mirrored));
    }

    #[test]
    fn canonical_form_is_order_independent() {
        let mut shuffled = sword();
        shuffled.reverse();
        shuffled.swap(0, 4);
        assert_eq!(canonicalize(&shuffled), canonicalize(&sword()));
    }

    #[test]
    fn canonical_form_rejects_a_near_miss() {
        let mut extra = sword();
        extra.push([3, 0, 0]);
        assert_ne!(canonicalize(&extra), canonicalize(&sword()));

        let mut missing = sword();
        missing.pop();
        assert_ne!(canonicalize(&missing), canonicalize(&sword()));
    }

    #[test]
    fn empty_shape_has_an_empty_key() {
        assert!(canonicalize(&[]).is_empty());
    }

    #[test]
    fn connectivity_is_required() {
        assert!(is_connected(&sword()));
        // Two blocks apart, not touching.
        assert!(!is_connected(&[[0, 0, 0], [5, 0, 5]]));
        // Touching diagonally is not touching: clusters are 6-connected.
        assert!(!is_connected(&[[0, 0, 0], [1, 0, 1]]));
        // Touching vertically is.
        assert!(is_connected(&[[0, 0, 0], [0, 1, 0]]));
    }

    #[test]
    fn duplicate_canonical_forms_fail_to_load() {
        let ron = r#"
        [
          (id: "A", output: Sword, unlock_s: 0, cooldown_s: 20,
           blocks: [[0,0,0],[1,0,0],[0,0,1],[1,0,1]]),
          (id: "B", output: Bow, unlock_s: 0, cooldown_s: 20,
           blocks: [[0,0,0],[0,0,1],[0,0,2],[1,0,2],[2,0,2]]),
          (id: "C", output: Spear, unlock_s: 0, cooldown_s: 20,
           blocks: [[1,0,1],[2,0,1],[1,0,2],[2,0,2]]),
        ]
        "#;
        let err = RecipeBook::parse(ron).unwrap_err();
        assert_eq!(err.recipe, "C", "the second of the pair is reported");
        assert!(
            err.reason.contains("collides with `A`"),
            "the message must name the recipe it collided with, got: {}",
            err.reason
        );
    }

    #[test]
    fn invalid_recipes_fail_to_load_with_a_named_reason() {
        let one = |blocks: &str, cooldown: u32| {
            format!(
                "[(id: \"X\", output: Sword, unlock_s: 0, cooldown_s: {cooldown}, blocks: {blocks})]"
            )
        };

        let err = RecipeBook::parse(&one("[[0,0,0],[1,0,0],[0,0,1],[1,0,1]]", 0)).unwrap_err();
        assert!(err.reason.contains("cooldown_s"), "{}", err.reason);

        let err = RecipeBook::parse(&one("[[0,0,0],[1,0,0],[2,0,0]]", 20)).unwrap_err();
        assert!(err.reason.contains("minimum"), "{}", err.reason);

        let err = RecipeBook::parse(&one("[[0,0,0],[1,0,0],[0,0,1],[9,0,1]]", 20)).unwrap_err();
        assert!(err.reason.contains("outside the pad"), "{}", err.reason);

        let err = RecipeBook::parse(&one("[[0,0,0],[1,0,0],[0,0,1],[0,0,1]]", 20)).unwrap_err();
        assert!(
            err.reason.contains("duplicate coordinate"),
            "{}",
            err.reason
        );

        let err = RecipeBook::parse(&one("[[0,1,0],[1,1,0],[0,1,1],[1,1,1]]", 20)).unwrap_err();
        assert!(err.reason.contains("y = 1"), "{}", err.reason);

        let err = RecipeBook::parse(&one("[[0,0,0],[1,0,0],[0,0,1],[5,0,5]]", 20)).unwrap_err();
        assert!(err.reason.contains("connected"), "{}", err.reason);

        assert!(RecipeBook::parse("[]").is_err());
        assert!(RecipeBook::parse("not ron (").is_err());
    }

    #[test]
    fn matching_is_exact_not_subset() {
        let book = RecipeBook::embedded();
        let pad = pad_with(&sword());
        let craft = find_match(&pad, book, 0).expect("the sword must match");
        assert_eq!(book.get(craft.recipe).unwrap().name, "R01_SWORD");

        let mut extra = sword();
        extra.push([3, 0, 0]);
        assert!(find_match(&pad_with(&extra), book, 0).is_none());
    }

    #[test]
    fn locked_recipes_do_not_consume() {
        let book = RecipeBook::embedded();
        let archer = book.by_name("R05_ARCHER").unwrap().blocks.clone();
        let mut pad = pad_with(&archer);
        // Unlocked at 180 s.
        assert!(find_match(&pad, book, 179).is_none());
        assert!(tick(&mut pad, book, Tick(20), 179).is_none());
        assert_eq!(pad.count(), 8, "a locked shape must not eat the crates");
        assert!(find_match(&pad, book, 180).is_some());
    }

    /// A pad holding `blocks`, with the stability window primed at tick 0.
    ///
    /// Priming matters: `set` does not know the tick, so a batch of out-of-band
    /// edits leaves the pad looking "changed on the next tick" until something
    /// calls [`Pad::reset_stability`]. In a real match the tick loop does this
    /// by ticking every frame; in a test the fixture has to say when the
    /// crates arrived.
    fn pad_with(blocks: &[BlockPos]) -> Pad {
        let mut pad = Pad::new([0, 0, 0]);
        for b in blocks {
            assert!(pad.set(*b, true), "{b:?} should be placeable");
        }
        pad.reset_stability(Tick::ZERO);
        pad
    }

    #[test]
    fn blocked_match_consumes_nothing() {
        let book = RecipeBook::embedded();
        let mut pad = pad_with(&sword());
        let out = tick_with(&mut pad, book, Tick(20), 0, |_| false)
            .expect("the shape is stable and matches");
        assert!(matches!(out, CraftOutcome::Blocked(_)));
        assert_eq!(pad.count(), 6, "§4.5.4: a cap refusal keeps the crates");
    }

    #[test]
    fn a_craft_resets_the_window() {
        let book = RecipeBook::embedded();
        let mut pad = pad_with(&sword());
        assert!(tick(&mut pad, book, Tick(10), 0).unwrap().crafted());
        assert!(pad.is_empty());
        assert!(
            tick(&mut pad, book, Tick(11), 0).is_none(),
            "an empty pad cannot craft"
        );
    }
}
