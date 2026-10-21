//! The block edit pipeline: validation, application and the per-column diff log
//! (plan §6.6, §6.7; task T-022).
//!
//! # What can change
//!
//! Almost nothing. Terrain is indestructible (§4.4.4) and crates are the only
//! placeable block, so the entire mutable world is the 8 × 4 × 8 stacking pad of
//! each team. That is what makes this module small: the authoritative world is
//! `world::gen` output plus a few hundred cells, and the diff log for a whole
//! match is a few hundred bytes.
//!
//! # Why validation happens before mutation
//!
//! §6.6 lists four checks — reach, existence, protection, rate — and only then
//! "apply to sim state". The order is not cosmetic. If the pad were written
//! first and the rate check refused afterwards, a flood of edits would still
//! change the world on every tick it was refused: the pad's stability timer
//! would reset, no recipe would ever fire, and the cheapest attack in the game
//! would be "spam C_EDIT". So every check is a read, [`Pads::check`] exists to
//! keep the protection rule in one place, and exactly one line mutates.
//!
//! # Why the world is a cache and not a copy
//!
//! The client regenerates terrain from the seed and the server only stores the
//! cells that changed (§6.7). The server still has to *answer* collision queries
//! for movement, and generating a column per query is 0.14 ms against a 50 ms
//! tick. So columns are generated on first touch and kept — a cache, not a
//! second source of truth: nothing here is ever persisted, and the seed plus the
//! diff log reconstructs the same world on any machine.
//!
//! [`Pads::check`]: sim::Pads::check

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use protocol::{edit_action, edit_reject, ChunkDiff, DiffEntry, DiffRun, MAX_DIFF_ENTRIES};
use sim::{Fixed, Pads, PlayerId, TeamId, Tick, Vec3, VoxelQuery};
use world::block::{self, BlockId, AIR};
use world::{Column, WorldGen, CHUNK_X, CHUNK_Y, CHUNK_Z};

/// How far a player may reach to edit a block, in metres (§6.6).
pub const REACH: Fixed = Fixed::from_int(6);

/// Edits per second per player (§6.6, §11.3).
pub const EDITS_PER_SECOND: u32 = 8;

/// The §6.7 ceiling for one `S_CHUNK_DIFF` frame, including its tag.
pub const MAX_DIFF_BYTES: usize = 1_536;

/// The bucket's capacity, and therefore the largest burst in one tick.
///
/// §11.3 says burst 12; T-022's acceptance criterion says the ninth edit in one
/// tick is refused, which implies 8. The stricter number wins: 12 edits in one
/// 50 ms tick is an instantaneous 240/s, nothing in the game needs it, and the
/// only thing a larger burst buys is a wider hole in the flood defence.
pub const EDIT_BURST: u32 = 8;

/// Columns along one axis of the 256 × 96 × 256 map.
pub const WORLD_CHUNKS: i32 = 16;

/// Tokens added per tick, in thousandths, so 8/s at 20 Hz is exact.
const TOKENS_PER_TICK: u32 = EDITS_PER_SECOND * 1_000 / 20;

/// One column's storage, its diff version and the edits not yet sent.
struct Chunk {
    column: Column,
    /// Bumped once per *emitted* diff, because a client accepts a diff only when
    /// its version is newer than what it holds.
    version: u32,
    pending: Vec<DiffEntry>,
    /// Every cell ever written, including cells restored to generated terrain.
    /// A joining client needs the current value of each, not the edit history.
    changed: BTreeSet<u16>,
}

/// The server's view of the mutable world: generated terrain plus edits.
pub struct EditWorld {
    gen: WorldGen,
    /// Interior mutability because [`VoxelQuery`] takes `&self` and movement
    /// asks thousands of questions per tick. A room is single-threaded and
    /// worldgen is pure, so a `RefCell` here cannot observably differ from a
    /// `&mut self` API — it only lets the collision path read a column it has
    /// not touched yet.
    columns: RefCell<BTreeMap<(i32, i32), Chunk>>,
}

impl EditWorld {
    /// A world for `seed`, with `teams` bases, holding no columns yet.
    pub fn new(seed: u64, teams: u8) -> EditWorld {
        EditWorld {
            gen: WorldGen::new(seed, teams),
            columns: RefCell::new(BTreeMap::new()),
        }
    }

    /// The seed this world was generated from.
    pub fn seed(&self) -> u64 {
        self.gen.seed()
    }

    /// Terrain height at a world column, ignoring trees and placed crates.
    ///
    /// Bots use this to aim a path leg at the ground rather than at their own
    /// current altitude. A leg whose endpoint is in mid-air is unreachable to
    /// the pathfinder, which requires a floor under every cell it expands.
    pub fn surface_height(&self, x: i32, z: i32) -> i32 {
        self.gen.height_at(x, z)
    }

    /// The block at a world position, or air outside the map.
    pub fn block(&self, x: i32, y: i32, z: i32) -> BlockId {
        let Some((cx, cz, lx, ly, lz)) = split(x, y, z) else {
            return AIR;
        };
        let mut columns = self.columns.borrow_mut();
        let chunk = columns
            .entry((cx, cz))
            .or_insert_with(|| Chunk::new(self.gen.column(cx, cz)));
        chunk.column.get(lx, ly, lz)
    }

    /// Write a block without validation.
    ///
    /// Not public to the pipeline — [`EditPipeline`] is the only caller that
    /// should reach the world, and it validates first. This exists for the
    /// host's setup path (applying a persisted diff) and for tests.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, id: BlockId) -> bool {
        let Some((cx, cz, lx, ly, lz)) = split(x, y, z) else {
            return false;
        };
        let columns = self.columns.get_mut();
        let chunk = columns
            .entry((cx, cz))
            .or_insert_with(|| Chunk::new(self.gen.column(cx, cz)));
        if chunk.column.get(lx, ly, lz) == id {
            return false;
        }
        chunk.column.set(lx, ly, lz, id);
        chunk.pending.push(DiffEntry {
            index: Column::index(lx, ly, lz) as u16,
            block: id.0,
        });
        chunk.changed.insert(Column::index(lx, ly, lz) as u16);
        true
    }

    /// Take the next pending edits for one column, if any, as a wire diff.
    ///
    /// The version increments here rather than on each edit, and the batch is
    /// cut by encoded bytes so the client's 1.5 KB decoder limit always holds.
    pub fn take_diff(&mut self, cx: i32, cz: i32) -> Option<ChunkDiff> {
        let columns = self.columns.get_mut();
        let chunk = columns.get_mut(&(cx, cz))?;
        if chunk.pending.is_empty() {
            return None;
        }
        let next_version = chunk.version + 1;
        let header = diff_header_len(cx, cz, next_version);
        let mut used = header;
        let mut count = 0usize;
        for entry in &chunk.pending {
            if count > 0
                && (count == MAX_DIFF_ENTRIES
                    || used + entry_wire_len(entry.index) > MAX_DIFF_BYTES)
            {
                break;
            }
            used += entry_wire_len(entry.index);
            count += 1;
        }
        if count == 0 {
            return None;
        }
        chunk.version = next_version;
        Some(ChunkDiff {
            cx: cx as i16,
            cz: cz as i16,
            version: next_version,
            keyframe: false,
            entries: chunk.pending.drain(..count).collect(),
            runs: Vec::new(),
        })
    }

    /// Columns with edits not yet sent, nearest first is the caller's job; this
    /// is sorted by `(cz, cx)` because `BTreeMap` iteration is deterministic and
    /// a `HashMap` here would make the send order differ between two servers
    /// running the same match.
    pub fn pending_columns(&self) -> Vec<(i32, i32)> {
        self.columns
            .borrow()
            .iter()
            .filter(|(_, chunk)| !chunk.pending.is_empty())
            .map(|(key, _)| *key)
            .collect()
    }

    /// Columns that hold at least one cell differing from generated terrain.
    ///
    /// A joining client needs these even after the pending diffs were drained:
    /// the latest value of a cell is not in the pending queue any more.
    pub fn edited_columns(&self) -> Vec<(i32, i32)> {
        self.columns
            .borrow()
            .iter()
            .filter(|(_, chunk)| !chunk.changed.is_empty())
            .map(|(key, _)| *key)
            .collect()
    }

    /// Everything a client joining now needs to reproduce one column.
    ///
    /// One or more frames, each under the §6.7 1.5 KB budget. A run-length
    /// keyframe is used when the changed-cell list is larger than one sparse
    /// message and the keyframe itself fits. Repeated joins get monotonic
    /// versions, and a column at version 0 is bumped so the client accepts it.
    pub fn full_diffs(&mut self, cx: i32, cz: i32) -> Vec<ChunkDiff> {
        let generated = self.gen.column(cx, cz);
        let columns = self.columns.get_mut();
        let Some(chunk) = columns.get_mut(&(cx, cz)) else {
            return Vec::new();
        };

        let mut entries = Vec::new();
        for index in &chunk.changed {
            let (x, y, z) = Column::coords(*index as usize);
            let block = chunk.column.get(x, y, z);
            if block != generated.get(x, y, z) {
                entries.push(DiffEntry {
                    index: *index,
                    block: block.0,
                });
            }
        }
        if entries.is_empty() {
            return Vec::new();
        }
        if chunk.version == 0 {
            chunk.version = 1;
        }
        let mut version = chunk.version;

        if entries.len() > MAX_DIFF_ENTRIES {
            let keyframe = ChunkDiff {
                cx: cx as i16,
                cz: cz as i16,
                version,
                keyframe: true,
                entries: Vec::new(),
                runs: column_runs(&chunk.column),
            };
            let frame = protocol::encode_s2c(&protocol::S2c::ChunkDiff(keyframe.clone()));
            if frame.len() <= MAX_DIFF_BYTES {
                return vec![keyframe];
            }
        }

        let mut out = Vec::new();
        let mut batch: Vec<DiffEntry> = Vec::new();
        let mut used = diff_header_len(cx, cz, version);
        for entry in entries {
            let entry_len = entry_wire_len(entry.index);
            if !batch.is_empty()
                && (batch.len() == MAX_DIFF_ENTRIES || used + entry_len > MAX_DIFF_BYTES)
            {
                out.push(ChunkDiff {
                    cx: cx as i16,
                    cz: cz as i16,
                    version,
                    keyframe: false,
                    entries: std::mem::take(&mut batch),
                    runs: Vec::new(),
                });
                version += 1;
                used = diff_header_len(cx, cz, version);
            }
            used += entry_len;
            batch.push(entry);
        }
        out.push(ChunkDiff {
            cx: cx as i16,
            cz: cz as i16,
            version,
            keyframe: false,
            entries: batch,
            runs: Vec::new(),
        });
        chunk.version = version;
        out
    }

    /// Hash every cell in one column, generating it if it was never touched.
    pub fn column_hash(&self, cx: i32, cz: i32) -> u64 {
        let columns = self.columns.borrow();
        match columns.get(&(cx, cz)) {
            Some(chunk) => world::column_hash(&chunk.column),
            None => world::column_hash(&self.gen.column(cx, cz)),
        }
    }

    /// The version of one column's diff stream.
    pub fn version(&self, cx: i32, cz: i32) -> u32 {
        self.columns
            .borrow()
            .get(&(cx, cz))
            .map_or(0, |chunk| chunk.version)
    }
}

impl Chunk {
    fn new(column: Column) -> Chunk {
        Chunk {
            column,
            version: 0,
            pending: Vec::new(),
            changed: BTreeSet::new(),
        }
    }
}

impl VoxelQuery for EditWorld {
    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        let id = self.block(x, y, z);
        block::registry().is_solid(id)
    }

    fn water(&self, x: i32, y: i32, z: i32) -> bool {
        self.block(x, y, z) == block::registry().id_of("water").unwrap_or(AIR)
    }
}

/// Bytes an index takes inside a postcard `u16`, plus the one-byte block id.
fn entry_wire_len(index: u16) -> usize {
    let value = u32::from(index);
    let index_bytes = if value < 128 {
        1
    } else if value < 16_384 {
        2
    } else {
        3
    };
    index_bytes + 1
}

/// Size of an empty diff with this header, tag included.
fn diff_header_len(cx: i32, cz: i32, version: u32) -> usize {
    protocol::encode_s2c(&protocol::S2c::ChunkDiff(ChunkDiff {
        cx: cx as i16,
        cz: cz as i16,
        version,
        keyframe: false,
        entries: Vec::new(),
        runs: Vec::new(),
    }))
    .len()
}

/// Split a column into runs in the same order as generated storage expects.
fn column_runs(col: &Column) -> Vec<DiffRun> {
    let mut runs: Vec<DiffRun> = Vec::new();
    for y in 0..CHUNK_Y {
        for z in 0..CHUNK_Z {
            for x in 0..CHUNK_X {
                let block = col.get(x, y, z).0;
                match runs.last_mut() {
                    Some(last) if last.block == block => last.length += 1,
                    _ => runs.push(DiffRun { block, length: 1 }),
                }
            }
        }
    }
    runs
}

/// Split a world position into a column key and local coordinates, or `None`
/// outside the map.
fn split(x: i32, y: i32, z: i32) -> Option<(i32, i32, usize, usize, usize)> {
    let limit = WORLD_CHUNKS * CHUNK_X as i32;
    if !(0..limit).contains(&x) || !(0..limit).contains(&z) || !(0..CHUNK_Y as i32).contains(&y) {
        return None;
    }
    let cx = x / CHUNK_X as i32;
    let cz = z / CHUNK_Z as i32;
    Some((
        cx,
        cz,
        (x % CHUNK_X as i32) as usize,
        y as usize,
        (z % CHUNK_Z as i32) as usize,
    ))
}

/// One edit request, as the host turns a `C_EDIT` into one.
pub struct EditRequest<'a> {
    /// Who sent it.
    pub player: PlayerId,
    /// Their team, which owns the only pad they may build on.
    pub team: TeamId,
    /// Their authoritative position — never the position the client claims.
    pub eye: Vec3,
    /// The message.
    pub message: &'a protocol::Edit,
}

/// What the pipeline decided.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EditResult {
    /// The client's sequence number, echoed in `S_EDIT_ACK`.
    pub seq: u16,
    /// Whether the world changed.
    pub accepted: bool,
    /// See [`protocol::edit_reject`]; `NONE` when accepted.
    pub reason: u8,
    /// The broadcast to send to everyone who has the chunk loaded (§6.6).
    /// `None` when the edit was refused.
    pub broadcast: Option<protocol::EditBroadcast>,
    /// The column that changed, for the diff stream.
    pub column: Option<(i32, i32)>,
}

impl EditResult {
    fn refused(seq: u16, reason: u8) -> EditResult {
        EditResult {
            seq,
            accepted: false,
            reason,
            broadcast: None,
            column: None,
        }
    }
}

/// One player's edit token bucket.
#[derive(Clone, Copy, Debug)]
struct Bucket {
    /// Tokens in thousandths, so 0.4 tokens per tick is exact.
    milli: u32,
    last: Tick,
}

impl Bucket {
    fn full() -> Bucket {
        Bucket {
            milli: EDIT_BURST * 1_000,
            last: Tick::ZERO,
        }
    }

    fn take(&mut self, now: Tick) -> bool {
        let elapsed = now.0.saturating_sub(self.last.0);
        self.milli = (self.milli + elapsed * TOKENS_PER_TICK).min(EDIT_BURST * 1_000);
        self.last = now;
        if self.milli >= 1_000 {
            self.milli -= 1_000;
            true
        } else {
            false
        }
    }
}

/// The edit pipeline: rate limits, validation and application.
#[derive(Clone, Debug)]
pub struct EditPipeline {
    buckets: Vec<Bucket>,
    /// Monotonic per room. Clients apply a broadcast only if it is newer than
    /// what they hold for that chunk (§6.6).
    server_seq: u32,
}

impl Default for EditPipeline {
    fn default() -> EditPipeline {
        EditPipeline::new()
    }
}

impl EditPipeline {
    /// A pipeline with no rate history and a zero sequence.
    pub fn new() -> EditPipeline {
        EditPipeline {
            buckets: Vec::new(),
            server_seq: 0,
        }
    }

    /// The last `server_seq` handed out.
    pub fn server_seq(&self) -> u32 {
        self.server_seq
    }

    /// Validate and, if everything passes, apply one edit.
    pub fn submit(
        &mut self,
        now: Tick,
        request: &EditRequest<'_>,
        world: &mut EditWorld,
        pads: &mut Pads,
    ) -> EditResult {
        let msg = request.message;
        let seq = msg.seq;
        let pos = [msg.x as i32, msg.y as i32, msg.z as i32];

        // 1. Bounds first, and it is not one of §6.6's four. A coordinate
        // outside the map has no block to be near, so "reach" is meaningless
        // there; §7.4 rule 4 also names this case explicitly and says to reject
        // rather than close, because players legitimately aim at the sky.
        if split(pos[0], pos[1], pos[2]).is_none() {
            return EditResult::refused(seq, edit_reject::OUT_OF_BOUNDS);
        }

        // 2. Reach (§6.6), against the authoritative position and the block's
        // centre. Centre, not corner: a corner test would make reach depend on
        // which side of a block you stand, by up to 1.7 m on a diagonal.
        let centre = block_centre(pos);
        if request.eye.distance(centre) > REACH {
            return EditResult::refused(seq, edit_reject::TOO_FAR);
        }

        // 3. Existence and placeability. Both are reads, and both have to pass
        // before the protection check can mean anything: there is no point
        // asking whether you may build where something already is.
        let current = world.block(pos[0], pos[1], pos[2]);
        let placing = match msg.action {
            edit_action::PLACE => true,
            edit_action::BREAK => false,
            // Unreachable: `codec::decode_checked` refuses an unknown action at
            // decode. Kept as a refusal rather than a `debug_assert!` because a
            // pipeline that trusts its input is one refactor away from applying
            // an action nobody defined.
            _ => return EditResult::refused(seq, edit_reject::UNKNOWN_BLOCK),
        };

        let id = if placing {
            let id = BlockId(msg.block);
            let registry = block::registry();
            if (msg.block as usize) >= registry.len() {
                return EditResult::refused(seq, edit_reject::UNKNOWN_BLOCK);
            }
            if !registry.is_placeable(id) {
                // Only crates are placeable (§4.4.3). The wire vocabulary has no
                // "you may not place that", and from the server's side a stone
                // block is exactly as unplaceable as an id that does not exist,
                // so both answer with the same code.
                return EditResult::refused(seq, edit_reject::UNKNOWN_BLOCK);
            }
            if !current.is_air() {
                return EditResult::refused(seq, edit_reject::OCCUPIED);
            }
            id
        } else {
            if current.is_air() {
                return EditResult::refused(seq, edit_reject::OCCUPIED);
            }
            if !block::registry().is_breakable(current) {
                // Terrain is indestructible (§4.4.4), so this is the common
                // case for a stray click, not an attack. "There is nothing to
                // break" is the closest the wire vocabulary gets.
                return EditResult::refused(seq, edit_reject::OCCUPIED);
            }
            AIR
        };

        // 4. Protection (§4.5.4, §6.6). `Pads::check` is the read-only half of
        // `Pads::place`, so the rule lives in one place and this stage cannot
        // accidentally mutate.
        if pads.check(request.team, pos, placing).is_err() {
            return EditResult::refused(seq, edit_reject::PROTECTED);
        }

        // 5. Rate last, per §6.6. Nothing has been written yet, so a refusal
        // here is invisible to the world — which is the whole point of
        // validating before applying.
        if !self.bucket(request.player).take(now) {
            return EditResult::refused(seq, edit_reject::RATE_LIMITED);
        }

        // 6. Apply. The pad mask and the voxel must move together: the pad is
        // what the recipe matcher reads, the voxel is what movement collides
        // with, and an edit that updated one but not the other would produce a
        // crate you can walk through or a recipe that fires on nothing.
        //
        // A re-sent edit does not land here: placing onto a crate fails the
        // existence check above with `OCCUPIED`, which is what `same_tick_race`
        // requires of the *second* edit on one cell. A client that lost an ack
        // therefore rolls its optimistic placement back and sends the edit
        // again; that is one visible flicker on a lost packet, and it is the
        // price of never letting two edits both win.
        pads.place(request.team, pos, placing)
            .expect("`Pads::check` passed a moment ago");
        world.set_block(pos[0], pos[1], pos[2], id);

        self.server_seq += 1;
        let (cx, cz, _, _, _) = split(pos[0], pos[1], pos[2]).expect("bounds checked above");
        EditResult {
            seq,
            accepted: true,
            reason: edit_reject::NONE,
            broadcast: Some(protocol::EditBroadcast {
                x: msg.x,
                y: msg.y,
                z: msg.z,
                block: id.0,
                server_seq: self.server_seq,
            }),
            column: Some((cx, cz)),
        }
    }

    fn bucket(&mut self, player: PlayerId) -> &mut Bucket {
        let index = player.0 as usize;
        while self.buckets.len() <= index {
            self.buckets.push(Bucket::full());
        }
        &mut self.buckets[index]
    }
}

/// The centre of a block, in fixed point.
fn block_centre(pos: [i32; 3]) -> Vec3 {
    let half = Fixed::from_raw(1 << 15);
    Vec3::new(
        Fixed::from_int(pos[0]) + half,
        Fixed::from_int(pos[1]) + half,
        Fixed::from_int(pos[2]) + half,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Edit;
    use sim::pad::{PAD_HEIGHT, PAD_SIZE};
    use sim::Pad;

    const SEED: u64 = 0x00C0_FFEE;

    /// A world and a pad at the first base worldgen produces.
    ///
    /// Using worldgen's own base rather than a made-up origin matters: the pad
    /// floor is stamped by `world::gen`, so a hand-picked origin would either
    /// float above the terrain or be buried in it, and the test would be
    /// checking the pipeline against geometry the game never produces.
    fn fixture() -> (EditWorld, Pads, [i32; 3]) {
        let world = EditWorld::new(SEED, 2);
        let base = world.gen.bases()[0];
        // `base.y` is the *floor* block's height: worldgen stamps the pad floor
        // there and `Base::spawn` puts a player at `y + 1`. Pad-local y = 0 is
        // the first cell above the floor, so the origin is one block up.
        let origin = [
            base.pad_x - PAD_SIZE / 2,
            base.y + 1,
            base.pad_z - PAD_SIZE / 2,
        ];
        let mut pads = Pads::new();
        pads.push(Pad::new(origin));
        (world, pads, origin)
    }

    fn eye_at(origin: [i32; 3]) -> Vec3 {
        // Standing on the pad floor, at its centre.
        Vec3::from_ints(origin[0] + 4, origin[1], origin[2] + 4)
    }

    fn edit(seq: u16, pos: [i32; 3], block: u8, action: u8) -> Edit {
        Edit {
            seq,
            x: pos[0] as i16,
            y: pos[1] as i16,
            z: pos[2] as i16,
            block,
            action,
        }
    }

    fn request<'a>(player: u8, eye: Vec3, message: &'a Edit) -> EditRequest<'a> {
        EditRequest {
            player: PlayerId(player),
            team: TeamId(0),
            eye,
            message,
        }
    }

    #[test]
    fn a_crate_inside_your_pad_is_placed() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        let cell = [origin[0], origin[1], origin[2]];
        let msg = edit(1, cell, block::CRATE.0, edit_action::PLACE);

        let result = pipeline.submit(
            Tick(1),
            &request(0, eye_at(origin), &msg),
            &mut world,
            &mut pads,
        );
        assert!(result.accepted, "reason {}", result.reason);
        assert_eq!(result.reason, edit_reject::NONE);
        assert_eq!(world.block(cell[0], cell[1], cell[2]), block::CRATE);
        assert!(world.solid(cell[0], cell[1], cell[2]), "a crate collides");
        assert_eq!(pipeline.server_seq(), 1);
        let broadcast = result.broadcast.expect("a broadcast");
        assert_eq!(broadcast.server_seq, 1);
        assert_eq!(broadcast.block, block::CRATE.0);
    }

    #[test]
    fn t2_reach_hack_rejected() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        // A legal cell exactly seven metres away: the pad's opposite corner,
        // with the player standing at the near one. Everything else about the
        // edit is valid, so only the reach rule can refuse it.
        let cell = [origin[0] + 7, origin[1], origin[2]];
        let eye = Vec3::from_ints(origin[0], origin[1], origin[2]);
        let msg = edit(1, cell, block::CRATE.0, edit_action::PLACE);
        let result = pipeline.submit(Tick(1), &request(0, eye, &msg), &mut world, &mut pads);
        assert!(!result.accepted);
        assert_eq!(result.reason, edit_reject::TOO_FAR);
        assert_eq!(world.block(cell[0], cell[1], cell[2]), AIR);
    }

    #[test]
    fn t6_block_edit_race_duping() {
        // Two edits on one cell in the same tick: the first wins, the second is
        // validated against the post-first state and refused. Last-write-wins is
        // deliberately not used — it produces ghost blocks on clients that
        // missed one message (§6.6).
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        let cell = [origin[0], origin[1], origin[2]];
        let eye = eye_at(origin);
        let first = edit(1, cell, block::CRATE.0, edit_action::PLACE);
        let second = edit(2, cell, block::CRATE.0, edit_action::PLACE);

        let a = pipeline.submit(Tick(1), &request(0, eye, &first), &mut world, &mut pads);
        let b = pipeline.submit(Tick(1), &request(1, eye, &second), &mut world, &mut pads);

        assert!(a.accepted);
        assert!(!b.accepted);
        assert_eq!(b.reason, edit_reject::OCCUPIED);
        assert!(b.broadcast.is_none(), "a refused edit is never broadcast");
        assert_eq!(
            a.broadcast.is_some() as u8 + b.broadcast.is_some() as u8,
            1,
            "exactly one S_EDIT for one block"
        );
        assert_eq!(pipeline.server_seq(), 1);
    }

    #[test]
    fn t4_edit_spam_rate_limited() {
        // Nine placements in one tick: eight fit in the bucket, the ninth does
        // not, and the ninth must not have touched the world.
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        let eye = eye_at(origin);

        let mut accepted = 0;
        let mut limited = 0;
        for i in 0..9 {
            // Distinct cells across the pad's floor layer, all within reach.
            let cell = [
                origin[0] + (i % PAD_SIZE),
                origin[1],
                origin[2] + (i / PAD_SIZE),
            ];
            let msg = edit(i as u16, cell, block::CRATE.0, edit_action::PLACE);
            let result = pipeline.submit(Tick(1), &request(0, eye, &msg), &mut world, &mut pads);
            if result.accepted {
                accepted += 1;
            } else {
                assert_eq!(result.reason, edit_reject::RATE_LIMITED);
                limited += 1;
                assert_eq!(
                    world.block(cell[0], cell[1], cell[2]),
                    AIR,
                    "a rate-limited edit must not be applied"
                );
            }
        }
        assert_eq!(accepted, 8);
        assert_eq!(limited, 1);
    }

    #[test]
    fn the_bucket_refills_at_eight_per_second() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        let eye = eye_at(origin);
        let mut seq = 0u16;

        // Drain the bucket at tick 0.
        for _ in 0..EDIT_BURST {
            let cell = [
                origin[0] + (seq as i32 % PAD_SIZE),
                origin[1] + 1,
                origin[2] + (seq as i32 / PAD_SIZE),
            ];
            let msg = edit(seq, cell, block::CRATE.0, edit_action::PLACE);
            assert!(
                pipeline
                    .submit(Tick(0), &request(0, eye, &msg), &mut world, &mut pads)
                    .accepted
            );
            seq += 1;
        }

        // 20 ticks later (1 s) exactly eight more are allowed, no more.
        let mut accepted = 0;
        for _ in 0..10 {
            let cell = [
                origin[0] + (seq as i32 % PAD_SIZE),
                origin[1] + 1 + (seq as i32 / PAD_SIZE),
                origin[2],
            ];
            let msg = edit(seq, cell, block::CRATE.0, edit_action::PLACE);
            if pipeline
                .submit(Tick(20), &request(0, eye, &msg), &mut world, &mut pads)
                .accepted
            {
                accepted += 1;
            }
            seq += 1;
        }
        assert_eq!(accepted, EDITS_PER_SECOND);
    }

    #[test]
    fn diff_version_monotonic() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        let eye = eye_at(origin);

        let mut versions = Vec::new();
        for i in 0..4 {
            let cell = [origin[0] + i, origin[1], origin[2]];
            let msg = edit(i as u16, cell, block::CRATE.0, edit_action::PLACE);
            let result = pipeline.submit(
                Tick(i as u32),
                &request(0, eye, &msg),
                &mut world,
                &mut pads,
            );
            assert!(result.accepted);
            let (cx, cz) = result.column.expect("a changed column");
            let diff = world.take_diff(cx, cz).expect("a diff");
            assert_eq!(diff.entries.len(), 1, "one edit, one entry");
            versions.push(diff.version);
        }
        assert_eq!(versions, vec![1, 2, 3, 4], "versions must be 1, 2, 3, ...");
        assert!(world.pending_columns().is_empty(), "take_diff drains");
    }

    #[test]
    fn take_diff_splits_by_wire_bytes_not_just_count() {
        let (mut world, _pads, _origin) = fixture();
        let cx = 8i32;
        let cz = 8i32;
        for i in 0..501usize {
            let index = 20_000 + i % 4_000;
            let (x, y, z) = Column::coords(index);
            world.set_block(
                cx * CHUNK_X as i32 + x as i32,
                y as i32,
                cz * CHUNK_Z as i32 + z as i32,
                block::CRATE,
            );
        }

        let mut versions = Vec::new();
        let mut total = 0;
        while let Some(diff) = world.take_diff(cx, cz) {
            assert!(!diff.keyframe);
            assert!(diff.entries.len() <= MAX_DIFF_ENTRIES);
            let frame = protocol::encode_s2c(&protocol::S2c::ChunkDiff(diff.clone()));
            assert!(
                frame.len() <= MAX_DIFF_BYTES,
                "one split frame is {} bytes",
                frame.len()
            );
            total += diff.entries.len();
            versions.push(diff.version);
        }
        assert_eq!(total, 501, "splitting must not lose an edit");
        assert_eq!(versions, vec![1, 2]);
    }

    #[test]
    fn breaking_terrain_is_refused() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        // The pad floor itself: solid, and `breakable: false` by design.
        let floor = [origin[0], origin[1] - 1, origin[2]];
        assert!(!world.block(floor[0], floor[1], floor[2]).is_air());
        let msg = edit(1, floor, 0, edit_action::BREAK);
        let result = pipeline.submit(
            Tick(1),
            &request(0, eye_at(origin), &msg),
            &mut world,
            &mut pads,
        );
        assert!(!result.accepted);
        assert_eq!(result.reason, edit_reject::OCCUPIED);
        assert!(!world.block(floor[0], floor[1], floor[2]).is_air());
    }

    #[test]
    fn breaking_your_own_crate_clears_both_masks() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        let eye = eye_at(origin);
        let cell = [origin[0], origin[1], origin[2]];

        let place = edit(1, cell, block::CRATE.0, edit_action::PLACE);
        assert!(
            pipeline
                .submit(Tick(1), &request(0, eye, &place), &mut world, &mut pads)
                .accepted
        );

        let local = [0, 0, 0];
        assert!(pads.get(TeamId(0)).unwrap().get(local));

        let remove = edit(2, cell, 0, edit_action::BREAK);
        let result = pipeline.submit(Tick(2), &request(0, eye, &remove), &mut world, &mut pads);
        assert!(result.accepted, "reason {}", result.reason);
        assert_eq!(world.block(cell[0], cell[1], cell[2]), AIR);
        assert!(
            !pads.get(TeamId(0)).unwrap().get(local),
            "the pad mask must follow the voxel"
        );
        assert_eq!(result.broadcast.expect("broadcast").block, AIR.0);
    }

    #[test]
    fn placing_outside_a_pad_is_refused() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        // Just off the pad edge: inside reach, outside the volume that may be
        // built on. Terrain is not buildable (§4.4.3).
        let cell = [origin[0] + PAD_SIZE, origin[1], origin[2] + 4];
        let msg = edit(1, cell, block::CRATE.0, edit_action::PLACE);
        let result = pipeline.submit(
            Tick(1),
            &request(0, eye_at(origin), &msg),
            &mut world,
            &mut pads,
        );
        assert!(!result.accepted);
        assert_eq!(result.reason, edit_reject::PROTECTED);
        assert_eq!(world.block(cell[0], cell[1], cell[2]), AIR);
    }

    #[test]
    fn placing_above_the_pad_height_is_refused() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        // Straight up from the pad centre, one layer above the build height.
        let cell = [origin[0] + 4, origin[1] + PAD_HEIGHT, origin[2] + 4];
        let msg = edit(1, cell, block::CRATE.0, edit_action::PLACE);
        let result = pipeline.submit(
            Tick(1),
            &request(0, eye_at(origin), &msg),
            &mut world,
            &mut pads,
        );
        assert!(!result.accepted);
        assert_eq!(result.reason, edit_reject::PROTECTED);
    }

    #[test]
    fn out_of_bounds_is_rejected_without_closing_anything() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        let msg = edit(9, [-1, 40, 300], block::CRATE.0, edit_action::PLACE);
        let result = pipeline.submit(
            Tick(1),
            &request(0, eye_at(origin), &msg),
            &mut world,
            &mut pads,
        );
        assert!(!result.accepted);
        assert_eq!(result.reason, edit_reject::OUT_OF_BOUNDS);
    }

    #[test]
    fn a_non_placeable_block_is_refused() {
        let (mut world, mut pads, origin) = fixture();
        let mut pipeline = EditPipeline::new();
        let stone = block::registry().id_of("stone").expect("stone");
        let cell = [origin[0], origin[1], origin[2]];
        let msg = edit(1, cell, stone.0, edit_action::PLACE);
        let result = pipeline.submit(
            Tick(1),
            &request(0, eye_at(origin), &msg),
            &mut world,
            &mut pads,
        );
        assert!(!result.accepted);
        assert_eq!(result.reason, edit_reject::UNKNOWN_BLOCK);

        let msg = edit(2, cell, 250, edit_action::PLACE);
        let result = pipeline.submit(
            Tick(2),
            &request(0, eye_at(origin), &msg),
            &mut world,
            &mut pads,
        );
        assert_eq!(
            result.reason,
            edit_reject::UNKNOWN_BLOCK,
            "no such block id"
        );
    }

    #[test]
    fn the_world_is_the_generated_column_plus_edits() {
        // The invariant that makes the diff stream correct: an untouched world
        // is byte-identical to worldgen, and a touched one differs by exactly
        // the cells the diff names.
        let (mut world, _pads, origin) = fixture();
        let gen = WorldGen::new(SEED, 2);
        let cx = origin[0] / CHUNK_X as i32;
        let cz = origin[2] / CHUNK_Z as i32;
        let reference = gen.column(cx, cz);

        for (x, y, z) in [(0usize, 0usize, 0usize), (5, 3, 7), (15, 95, 15)] {
            assert_eq!(
                world.block(
                    cx * CHUNK_X as i32 + x as i32,
                    y as i32,
                    cz * CHUNK_Z as i32 + z as i32
                ),
                reference.get(x, y, z)
            );
        }

        let cell = [origin[0], origin[1], origin[2]];
        assert!(world.set_block(cell[0], cell[1], cell[2], block::CRATE));
        assert_eq!(world.block(cell[0], cell[1], cell[2]), block::CRATE);
        let diff = world.take_diff(cx, cz).expect("a diff");
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].block, block::CRATE.0);
    }

    fn apply_join_frame(frame: &[u8], column: &mut Column) {
        let protocol::S2c::ChunkDiff(diff) =
            protocol::decode_s2c(frame).expect("a join diff decodes")
        else {
            panic!("expected a chunk diff");
        };
        if diff.keyframe {
            *column = Column::new();
            let mut index = 0usize;
            for run in &diff.runs {
                for _ in 0..run.length {
                    let (x, y, z) = Column::coords(index);
                    column.set(x, y, z, block::BlockId(run.block));
                    index += 1;
                }
            }
            assert_eq!(index, world::VOLUME, "a keyframe covers the column");
        } else {
            for entry in &diff.entries {
                let (x, y, z) = Column::coords(entry.index as usize);
                column.set(x, y, z, block::BlockId(entry.block));
            }
        }
    }

    #[test]
    fn edit_then_join() {
        let (mut world, _pads, _origin) = fixture();
        let cx = 8i32;
        let cz = 8i32;
        for i in 0..400usize {
            // High cells force three-byte varint indices, the case where the
            // per-entry budget is 4 bytes rather than the plan's estimated 3.
            let index = 20_000 + (i * 11) % 4_000;
            let (x, y, z) = Column::coords(index);
            assert!(world.set_block(
                cx * CHUNK_X as i32 + x as i32,
                y as i32,
                cz * CHUNK_Z as i32 + z as i32,
                block::CRATE,
            ));
        }

        let joins = world.full_diffs(cx, cz);
        assert_eq!(joins.len(), 2, "400 high indices need two frames");
        let frames: Vec<Vec<u8>> = joins
            .iter()
            .map(|diff| protocol::encode_s2c(&protocol::S2c::ChunkDiff(diff.clone())))
            .collect();
        for frame in &frames {
            assert!(
                frame.len() <= MAX_DIFF_BYTES,
                "a join frame is {} bytes, budget is {MAX_DIFF_BYTES}",
                frame.len()
            );
        }

        let mut client = WorldGen::new(SEED, 2).column(cx, cz);
        for frame in &frames {
            apply_join_frame(frame, &mut client);
        }
        assert_eq!(
            world::column_hash(&client),
            world.column_hash(cx, cz),
            "the joining client must reconstruct the server's column"
        );
    }

    #[test]
    fn keyframe_replaces_the_whole_column() {
        let (mut world, _pads, _origin) = fixture();
        let cx = 9i32;
        let cz = 9i32;
        for y in 0..CHUNK_Y {
            for z in 0..CHUNK_Z {
                for x in 0..CHUNK_X {
                    world.set_block(
                        cx * CHUNK_X as i32 + x as i32,
                        y as i32,
                        cz * CHUNK_Z as i32 + z as i32,
                        block::CRATE,
                    );
                }
            }
        }

        let joins = world.full_diffs(cx, cz);
        assert_eq!(joins.len(), 1, "the keyframe fits one message");
        let join = joins.into_iter().next().expect("a keyframe diff");
        assert!(
            join.keyframe,
            "more than 500 changed cells needs a keyframe"
        );
        assert!(join.entries.is_empty());
        let frame = protocol::encode_s2c(&protocol::S2c::ChunkDiff(join));
        assert!(
            frame.len() <= MAX_DIFF_BYTES,
            "the run-length keyframe is {} bytes",
            frame.len()
        );

        let mut client = WorldGen::new(SEED, 2).column(cx, cz);
        apply_join_frame(&frame, &mut client);
        assert_eq!(world::column_hash(&client), world.column_hash(cx, cz));
    }
}
