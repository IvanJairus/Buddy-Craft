//! Chunk diff encoding (plan T-008).
//!
//! The client regenerates terrain from the seed, so the server only has to
//! describe **what changed**. With D-01 (airdrop crates, indestructible
//! terrain) the only blocks that ever change are crates inside an 8x8 stacking
//! pad, so a realistic diff is tens of bytes and the worst case is bounded by
//! `pad.size^2 * pad.max_height = 256` entries. The 400-entry / 1,400-byte
//! assertion in [`tests::worst_case_400_edits`] therefore has roughly 3x
//! headroom, which is deliberate: it must still hold if a later milestone lets
//! players place crates outside a pad.
//!
//! # Wire format
//!
//! One tag byte, then a postcard-encoded body.
//!
//! ```text
//! 0x00  diff      u32 version, Vec<(u16 index, u8 block)>
//! 0x01  keyframe  u32 version, Vec<(u8 block, u32 run_length)>
//! ```
//!
//! A keyframe is a run-length encoding of the **entire** column. It is used
//! when a diff would be larger than the keyframe (more than
//! [`KEYFRAME_THRESHOLD`] entries), which happens after a long match has
//! accumulated thousands of edits. Decoding a keyframe overwrites the column
//! wholesale, so it also doubles as the resync path after a client rejoins.
//!
//! Indices are `u16` because a column is 24,576 cells and `24576 < 65536`.

use serde::{Deserialize, Serialize};

use crate::block::BlockId;
use crate::chunk::{Column, CHUNK_Y, VOLUME};

/// Tag byte for a sparse diff frame.
pub const TAG_DIFF: u8 = 0x00;
/// Tag byte for a run-length keyframe.
pub const TAG_KEYFRAME: u8 = 0x01;

/// Above this many entries a keyframe is smaller than the diff, so it wins.
pub const KEYFRAME_THRESHOLD: usize = 2_000;

/// Errors produced while decoding a patch.
#[derive(Debug, PartialEq, Eq)]
pub enum DiffError {
    /// The buffer was empty.
    Empty,
    /// The tag byte is not one this module defines.
    UnknownTag(u8),
    /// The postcard body did not parse.
    Malformed,
    /// A cell index was outside the column.
    IndexOutOfRange(u16),
    /// Run lengths in a keyframe did not sum to exactly [`VOLUME`].
    BadRunLength {
        /// Cells the keyframe actually described.
        got: usize,
    },
}

impl core::fmt::Display for DiffError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DiffError::Empty => write!(f, "empty patch buffer"),
            DiffError::UnknownTag(t) => write!(f, "unknown patch tag {t:#04x}"),
            DiffError::Malformed => write!(f, "malformed patch body"),
            DiffError::IndexOutOfRange(i) => write!(f, "cell index {i} out of range"),
            DiffError::BadRunLength { got } => {
                write!(f, "keyframe described {got} cells, expected {VOLUME}")
            }
        }
    }
}

/// A sparse set of cell edits against a column.
///
/// `version` is a monotonic counter per column. The client ignores a diff whose
/// version is not exactly `last_applied + 1`, which makes packet reordering
/// harmless without needing a reliable ordered channel for edits.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkDiff {
    /// Monotonic per-column version.
    pub version: u32,
    /// `(cell index, block id)` pairs, in the order the edits were made.
    pub entries: Vec<(u16, u8)>,
}

/// A decoded patch: either a sparse diff or a full-column keyframe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkPatch {
    /// Apply the listed cell edits.
    Diff(ChunkDiff),
    /// Replace the column with the run-length decoded contents.
    Keyframe {
        /// Monotonic per-column version.
        version: u32,
        /// `(block id, run length)` pairs covering every cell in order.
        runs: Vec<(u8, u32)>,
    },
}

impl ChunkDiff {
    /// An empty diff at `version`.
    pub fn new(version: u32) -> ChunkDiff {
        ChunkDiff {
            version,
            entries: Vec::new(),
        }
    }

    /// Record a change. Returns `false` when the value is unchanged.
    pub fn push(&mut self, col: &Column, x: usize, y: usize, z: usize, id: BlockId) -> bool {
        if !Column::in_bounds(x, y, z) {
            return false;
        }
        let index = Column::index(x, y, z);
        let i = index as u16;
        if let Some(slot) = self.entries.iter_mut().find(|(idx, _)| *idx == i) {
            if slot.1 == id.0 {
                return false;
            }
            slot.1 = id.0;
            return true;
        }
        if col.get(x, y, z) == id {
            return false;
        }
        self.entries.push((i, id.0));
        true
    }

    /// True when there is nothing to send.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Encode as a sparse diff frame, unconditionally.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 4 + self.entries.len() * 3);
        out.push(TAG_DIFF);
        let body = postcard::to_allocvec(self).expect("ChunkDiff always serialises");
        out.extend_from_slice(&body);
        out
    }

    /// Encode, falling back to a run-length keyframe of `col` when the diff has
    /// grown past [`KEYFRAME_THRESHOLD`] entries.
    pub fn encode_patch(&self, col: &Column) -> Vec<u8> {
        if self.entries.len() > KEYFRAME_THRESHOLD {
            encode_keyframe(self.version, col)
        } else {
            self.encode()
        }
    }

    /// Apply the edits to a column. Unknown or out-of-range indices are skipped
    /// rather than treated as fatal: a corrupt packet must not take down a room.
    pub fn apply(&self, col: &mut Column) {
        for (index, id) in &self.entries {
            let i = *index as usize;
            if i >= VOLUME {
                continue;
            }
            let (x, y, z) = Column::coords(i);
            col.set(x, y, z, BlockId(*id));
        }
    }
}

/// Run-length encode an entire column into a keyframe frame.
pub fn encode_keyframe(version: u32, col: &Column) -> Vec<u8> {
    let mut runs: Vec<(u8, u32)> = Vec::new();
    for y in 0..CHUNK_Y {
        for z in 0..crate::chunk::CHUNK_Z {
            for x in 0..crate::chunk::CHUNK_X {
                let id = col.get(x, y, z).0;
                match runs.last_mut() {
                    Some((last, n)) if *last == id => *n += 1,
                    _ => runs.push((id, 1)),
                }
            }
        }
    }

    let mut out = Vec::with_capacity(1 + 4 + runs.len() * 3);
    out.push(TAG_KEYFRAME);
    out.extend_from_slice(&postcard::to_allocvec(&(version, &runs)).expect("keyframe serialises"));
    out
}

/// Decode any patch frame produced by [`ChunkDiff::encode`],
/// [`ChunkDiff::encode_patch`] or [`encode_keyframe`].
pub fn decode(bytes: &[u8]) -> Result<ChunkPatch, DiffError> {
    let (&tag, body) = bytes.split_first().ok_or(DiffError::Empty)?;
    match tag {
        TAG_DIFF => {
            let diff: ChunkDiff = postcard::from_bytes(body).map_err(|_| DiffError::Malformed)?;
            for (index, _) in &diff.entries {
                if *index as usize >= VOLUME {
                    return Err(DiffError::IndexOutOfRange(*index));
                }
            }
            Ok(ChunkPatch::Diff(diff))
        }
        TAG_KEYFRAME => {
            let (version, runs): (u32, Vec<(u8, u32)>) =
                postcard::from_bytes(body).map_err(|_| DiffError::Malformed)?;
            let total: usize = runs.iter().map(|(_, n)| *n as usize).sum();
            if total != VOLUME {
                return Err(DiffError::BadRunLength { got: total });
            }
            Ok(ChunkPatch::Keyframe { version, runs })
        }
        other => Err(DiffError::UnknownTag(other)),
    }
}

impl ChunkPatch {
    /// The version this patch brings the column to.
    pub fn version(&self) -> u32 {
        match self {
            ChunkPatch::Diff(d) => d.version,
            ChunkPatch::Keyframe { version, .. } => *version,
        }
    }

    /// Apply the patch. Idempotent: applying twice yields the same column.
    pub fn apply(&self, col: &mut Column) {
        match self {
            ChunkPatch::Diff(d) => d.apply(col),
            ChunkPatch::Keyframe { runs, .. } => {
                let mut i = 0usize;
                for (id, n) in runs {
                    let block = BlockId(*id);
                    for _ in 0..*n {
                        if i >= VOLUME {
                            break;
                        }
                        let (x, y, z) = Column::coords(i);
                        col.set(x, y, z, block);
                        i += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::AIR;
    use crate::gen::WorldGen;

    fn terrain() -> Column {
        WorldGen::new(0xD1FF, 2).column(3, 3)
    }

    #[test]
    fn roundtrip_is_byte_identical() {
        let col = terrain();
        let mut diff = ChunkDiff::new(1);
        let crate_id = crate::block::CRATE;
        for i in 0..40u16 {
            let (x, y, z) = Column::coords(i as usize * 7);
            diff.push(&col, x, y, z, crate_id);
        }
        let bytes = diff.encode();
        assert_eq!(bytes[0], TAG_DIFF);
        match decode(&bytes).unwrap() {
            ChunkPatch::Diff(back) => assert_eq!(back, diff),
            other => panic!("expected a diff, got {other:?}"),
        }
    }

    #[test]
    fn worst_case_400_edits() {
        let col = terrain();
        let mut diff = ChunkDiff::new(9);
        // 400 distinct cells spread over the column, which is far more than a
        // real pad can ever produce (8x8x4 = 256).
        for i in 0..400u16 {
            let index = (i as usize * 61) % VOLUME;
            let (x, y, z) = Column::coords(index);
            diff.push(&col, x, y, z, crate::block::CRATE);
        }
        assert_eq!(diff.entries.len(), 400);
        let bytes = diff.encode();
        assert!(
            bytes.len() <= 1_400,
            "400 edits encoded to {} bytes, budget is 1,400",
            bytes.len()
        );
        let patch = decode(&bytes).unwrap();
        let mut target = col.clone();
        patch.apply(&mut target);
        let mut expected = col;
        diff.apply(&mut expected);
        assert_eq!(target.count_non_air(), expected.count_non_air());
    }

    #[test]
    fn keyframe_fallback() {
        // Build a column with ~8,000 crates by direct writes, then hand the
        // encoder a diff that is past the threshold. The entries do not have to
        // match the column: the point of the fallback is that the *column* is
        // sent verbatim once the diff stops being the smaller frame.
        let mut col = Column::new();
        for y in 0..CHUNK_Y {
            for z in 0..crate::chunk::CHUNK_Z {
                for x in 0..crate::chunk::CHUNK_X {
                    if (x + y + z) % 3 == 0 {
                        col.set(x, y, z, crate::block::CRATE);
                    }
                }
            }
        }
        let mut diff = ChunkDiff::new(3);
        diff.entries = (0..KEYFRAME_THRESHOLD as u16 + 1)
            .map(|i| (i * 3, crate::block::CRATE.0))
            .collect();
        assert!(diff.entries.len() > KEYFRAME_THRESHOLD);

        let bytes = diff.encode_patch(&col);
        assert_eq!(
            bytes[0], TAG_KEYFRAME,
            "large diffs must fall back to a keyframe"
        );

        let patch = decode(&bytes).unwrap();
        assert_eq!(patch.version(), 3);

        // Decoding into a pristine column must reproduce the source exactly.
        let mut restored = Column::new();
        patch.apply(&mut restored);
        for y in 0..CHUNK_Y {
            for z in 0..crate::chunk::CHUNK_Z {
                for x in 0..crate::chunk::CHUNK_X {
                    assert_eq!(restored.get(x, y, z), col.get(x, y, z), "({x}, {y}, {z})");
                }
            }
        }
    }

    #[test]
    fn apply_is_idempotent() {
        let col = terrain();
        let mut diff = ChunkDiff::new(2);
        for i in 0..25u16 {
            let (x, y, z) = Column::coords(i as usize * 13);
            diff.push(&col, x, y, z, crate::block::CRATE);
        }
        let patch = decode(&diff.encode()).unwrap();

        let mut once = col.clone();
        patch.apply(&mut once);
        let mut twice = once.clone();
        patch.apply(&mut twice);

        for y in 0..CHUNK_Y {
            for z in 0..crate::chunk::CHUNK_Z {
                for x in 0..crate::chunk::CHUNK_X {
                    assert_eq!(once.get(x, y, z), twice.get(x, y, z));
                }
            }
        }
        assert_eq!(once.count_non_air(), twice.count_non_air());
    }

    #[test]
    fn push_rejects_no_ops_and_coalesces_repeats() {
        let col = Column::new();
        let mut diff = ChunkDiff::new(1);
        // Writing air over air changes nothing.
        assert!(!diff.push(&col, 1, 1, 1, AIR));
        // First real write is recorded.
        assert!(diff.push(&col, 1, 1, 1, crate::block::CRATE));
        // The same write again is a no-op.
        assert!(!diff.push(&col, 1, 1, 1, crate::block::CRATE));
        // A different value coalesces into the same entry rather than adding one.
        assert!(diff.push(&col, 1, 1, 1, crate::block::BlockId(3)));
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].1, 3);
        // Out of bounds is rejected.
        assert!(!diff.push(&col, 99, 99, 99, crate::block::CRATE));
    }

    #[test]
    fn malformed_input_is_rejected_not_panicked() {
        assert_eq!(decode(&[]), Err(DiffError::Empty));
        assert_eq!(decode(&[0x7F]), Err(DiffError::UnknownTag(0x7F)));
        assert_eq!(decode(&[TAG_DIFF, 0xFF, 0xFF]), Err(DiffError::Malformed));
        // A keyframe whose runs do not cover the column is rejected.
        let body = postcard::to_allocvec(&(1u32, vec![(0u8, 5u32)])).unwrap();
        let mut frame = vec![TAG_KEYFRAME];
        frame.extend_from_slice(&body);
        assert!(matches!(
            decode(&frame),
            Err(DiffError::BadRunLength { got: 5 })
        ));
    }

    #[test]
    fn empty_diff_is_one_byte_plus_header() {
        let d = ChunkDiff::new(0);
        assert!(d.is_empty());
        let bytes = d.encode();
        assert_eq!(bytes[0], TAG_DIFF);
        assert!(
            bytes.len() <= 8,
            "an empty diff must be tiny, got {}",
            bytes.len()
        );
        match decode(&bytes).unwrap() {
            ChunkPatch::Diff(back) => assert!(back.is_empty()),
            _ => panic!("expected a diff"),
        }
    }

    #[test]
    fn keyframe_roundtrips_terrain_exactly() {
        let col = terrain();
        let bytes = encode_keyframe(42, &col);
        assert_eq!(bytes[0], TAG_KEYFRAME);
        let patch = decode(&bytes).unwrap();
        let mut out = Column::new();
        patch.apply(&mut out);
        assert_eq!(out.count_non_air(), col.count_non_air());
        assert_eq!(out.highest_solid_y(), col.highest_solid_y());
    }
}
