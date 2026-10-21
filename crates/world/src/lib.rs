//! `world` — voxel storage, deterministic worldgen, meshing and chunk diffs.
//!
//! This crate is the *data* half of the voxel problem; `sim` is the *rules*
//! half. It is compiled into both the wasm client (which regenerates terrain
//! from the seed and meshes it locally) and the native server (which owns the
//! handful of blocks a match actually changes), so everything here has to be
//! bit-identical on `wasm32`, `x86_64` and `aarch64`.
//!
//! # Module map
//!
//! | Module | Plan task | Responsibility |
//! | ------ | --------- | -------------- |
//! | [`block`] | T-005 | the 20-entry block table from `data/blocks.ron` |
//! | [`chunk`] | T-005 | paletted, nibble-packed 16x96x16 column storage |
//! | [`noise`] | T-006 | integer value noise and fBm, no libm |
//! | [`gen`] | T-006 | pure `(seed, teams, cx, cz) -> Column` worldgen |
//! | [`mesh`] | T-007 | greedy mesher with baked per-vertex AO |
//! | [`diff`] | T-008 | sparse cell diffs plus an RLE keyframe fallback |
//!
//! # The invariant that matters
//!
//! The client never downloads the world. It gets a `u64` seed and regenerates
//! the terrain, and the server only sends the cells that changed. If worldgen
//! is not bit-identical between the two, players see different maps — and
//! because there is no packet to blame, the bug presents as "the crate I placed
//! is invisible", which is the single hardest class of bug to chase. That is
//! why [`gen`] contains no floats, no `HashMap` iteration and no cross-column
//! state.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod block;
pub mod chunk;
pub mod diff;
pub mod gen;
pub mod mesh;
pub mod noise;

pub use block::{BlockDef, BlockId, BlockRegistry, AIR, CRATE, CRATE_INDEX};
pub use chunk::{column_hash, Column, CHUNK_X, CHUNK_Y, CHUNK_Z, VOLUME};
pub use diff::{decode, ChunkDiff, ChunkPatch, DiffError};
pub use gen::{Base, DropSite, WorldGen};
pub use mesh::{Face, Mesh, Vertex, VERTEX_STRIDE};

/// Version of the world data format.
///
/// Bumped whenever `data/blocks.ron` is reordered, the column layout changes,
/// or worldgen output changes. The client refuses to join a room whose version
/// differs, because a mismatch would silently desync rather than error.
pub const WORLD_FORMAT_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_index_matches_registry() {
        assert_eq!(block::registry().id_of("crate"), Some(CRATE));
        assert_eq!(CRATE.0, CRATE_INDEX);
    }

    #[test]
    fn end_to_end_place_a_crate_and_ship_the_diff() {
        // The smallest honest integration test of the whole crate: generate a
        // column, place a crate on the pad, encode the change, decode it into a
        // freshly regenerated column, and confirm both sides agree.
        let g = WorldGen::new(0x5EED_1234, 2);
        let base = g.bases()[0];
        let cx = base.pad_x.div_euclid(CHUNK_X as i32);
        let cz = base.pad_z.div_euclid(CHUNK_Z as i32);

        let mut server = g.column(cx, cz);
        let mut client = g.column(cx, cz);

        let lx = (base.pad_x - cx * CHUNK_X as i32) as usize;
        let lz = (base.pad_z - cz * CHUNK_Z as i32) as usize;
        let ly = base.y as usize + 1;

        let mut diff = ChunkDiff::new(1);
        assert!(diff.push(&server, lx, ly, lz, CRATE));
        server.set(lx, ly, lz, CRATE);

        let bytes = diff.encode_patch(&server);
        assert!(bytes.len() < 64, "one crate must cost a handful of bytes");

        let patch = decode(&bytes).expect("patch decodes");
        patch.apply(&mut client);

        assert_eq!(client.get(lx, ly, lz), CRATE);
        assert_eq!(client.count_non_air(), server.count_non_air());
    }

    #[test]
    fn meshing_a_column_stays_within_the_quad_budget() {
        // A terrain column must not produce anything close to the naive
        // 60,000-quad worst case; greedy meshing has to be doing real work.
        let g = WorldGen::new(0x2024, 2);
        let mesh = mesh::mesh_column(&g.column(6, 6));
        assert!(!mesh.is_empty());
        assert!(
            mesh.quad_count() < 6_000,
            "greedy meshing produced {} quads",
            mesh.quad_count()
        );
    }
}
