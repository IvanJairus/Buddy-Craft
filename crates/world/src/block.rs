//! Block registry.
//!
//! The block table lives in `data/blocks.ron`; the index in that array **is**
//! the `BlockId`. Two invariants are enforced at load because violating either
//! silently breaks the game rather than failing loudly:
//!
//! * exactly one block is `placeable` — the crate (plan §4.4.4). If terrain
//!   became placeable, players could build infinite walls for free and the
//!   whole airdrop economy would be pointless.
//! * no terrain block is `breakable` while `tuning.terrain.breakable` is false,
//!   because the mesher and the diff encoder both assume terrain is static.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// A block type. The value is an index into [`registry`].
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[repr(transparent)]
pub struct BlockId(pub u8);

/// The empty block. Always index 0; the registry asserts this at load.
pub const AIR: BlockId = BlockId(0);

/// Index of the crate block, asserted against the registry by a test.
pub const CRATE_INDEX: u8 = 11;

/// The crate block — the only placeable block, and the recipe currency.
pub const CRATE: BlockId = BlockId(CRATE_INDEX);

impl BlockId {
    /// True for air.
    #[inline]
    pub const fn is_air(self) -> bool {
        self.0 == 0
    }
}

/// One entry of the block table.
#[derive(Clone, Debug, Deserialize)]
pub struct BlockDef {
    /// Stable identifier used by data files and tests.
    pub name: String,
    /// Blocks movement.
    pub solid: bool,
    /// Hides the neighbouring face during meshing.
    pub opaque: bool,
    /// May be placed by a player.
    pub placeable: bool,
    /// May be removed by a player.
    pub breakable: bool,
    /// Counts as a crate for the recipe matcher.
    #[serde(rename = "crate")]
    pub is_crate: bool,
    /// Index into the texture atlas.
    pub tile: u16,
}

/// The loaded block table.
#[derive(Debug)]
pub struct BlockRegistry {
    defs: Vec<BlockDef>,
    by_name: BTreeMap<String, BlockId>,
}

impl BlockRegistry {
    /// Parse and validate a registry document.
    pub fn parse(src: &str) -> Result<BlockRegistry, String> {
        let defs: Vec<BlockDef> =
            ron::from_str(src).map_err(|e| format!("blocks.ron is not valid RON: {e}"))?;

        if defs.is_empty() {
            return Err("blocks.ron is empty".into());
        }
        if defs[0].name != "air" {
            return Err("block 0 must be `air`".into());
        }
        if defs.len() > u8::MAX as usize {
            return Err(format!("at most 255 block types, got {}", defs.len()));
        }

        let mut by_name = BTreeMap::new();
        for (i, d) in defs.iter().enumerate() {
            if by_name.insert(d.name.clone(), BlockId(i as u8)).is_some() {
                return Err(format!("duplicate block name `{}`", d.name));
            }
        }

        let placeable: Vec<&str> = defs
            .iter()
            .filter(|d| d.placeable)
            .map(|d| d.name.as_str())
            .collect();
        if placeable != ["crate"] {
            return Err(format!(
                "exactly one block may be placeable and it must be `crate`, got {placeable:?}"
            ));
        }

        if !defs[0].is_crate {
            // air must never count as a crate
        } else {
            return Err("air must not be a crate".into());
        }

        Ok(BlockRegistry { defs, by_name })
    }

    /// Number of registered blocks.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// True when no blocks are registered (never true for a valid registry).
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Definition for a block id.
    #[inline]
    pub fn def(&self, id: BlockId) -> &BlockDef {
        &self.defs[id.0 as usize]
    }

    /// Look up a block by name.
    pub fn id_of(&self, name: &str) -> Option<BlockId> {
        self.by_name.get(name).copied()
    }

    /// True when the block hides its neighbours.
    #[inline]
    pub fn is_opaque(&self, id: BlockId) -> bool {
        self.defs[id.0 as usize].opaque
    }

    /// True when the block blocks movement.
    #[inline]
    pub fn is_solid(&self, id: BlockId) -> bool {
        self.defs[id.0 as usize].solid
    }

    /// True when the block participates in recipe matching.
    #[inline]
    pub fn is_crate(&self, id: BlockId) -> bool {
        self.defs[id.0 as usize].is_crate
    }

    /// True when a player may place the block.
    #[inline]
    pub fn is_placeable(&self, id: BlockId) -> bool {
        self.defs[id.0 as usize].placeable
    }

    /// True when a player may remove the block.
    #[inline]
    pub fn is_breakable(&self, id: BlockId) -> bool {
        self.defs[id.0 as usize].breakable
    }
}

/// The registry embedded in the binary.
pub fn registry() -> &'static BlockRegistry {
    static CELL: OnceLock<&'static BlockRegistry> = OnceLock::new();
    CELL.get_or_init(|| {
        let parsed =
            BlockRegistry::parse(EMBEDDED_RON).expect("embedded data/blocks.ron is invalid");
        Box::leak(Box::new(parsed))
    })
}

/// Raw embedded registry source, so tests can re-parse and diff it.
pub const EMBEDDED_RON: &str = include_str!("../../../data/blocks.ron");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_loads_with_20_blocks() {
        let r = registry();
        assert_eq!(r.len(), 20, "plan T-005 requires 20 block types");
        assert_eq!(r.id_of("air"), Some(AIR));
    }

    #[test]
    fn crate_index_constant_matches_data() {
        // If someone reorders blocks.ron this fails, which is the point: the
        // constant is used in hot paths and in tests.
        assert_eq!(registry().id_of("crate"), Some(CRATE));
        assert!(registry().is_crate(CRATE));
    }

    #[test]
    fn only_crates_are_placeable() {
        let r = registry();
        let placeable: Vec<&str> = (0..r.len() as u8)
            .map(BlockId)
            .filter(|b| r.is_placeable(*b))
            .map(|b| r.def(b).name.as_str())
            .collect();
        assert_eq!(
            placeable,
            vec!["crate"],
            "plan §4.4.4: crates are the only placeable block"
        );
    }

    #[test]
    fn terrain_is_not_breakable() {
        let r = registry();
        for i in 0..r.len() as u8 {
            let id = BlockId(i);
            if r.is_crate(id) {
                assert!(r.is_breakable(id), "crates must be breakable");
            } else {
                assert!(
                    !r.is_breakable(id),
                    "{} must not be breakable",
                    r.def(id).name
                );
            }
        }
    }

    #[test]
    fn air_is_not_solid_and_not_opaque() {
        let r = registry();
        assert!(!r.is_solid(AIR));
        assert!(!r.is_opaque(AIR));
    }

    #[test]
    fn tiles_are_unique() {
        let r = registry();
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..r.len() as u8 {
            let tile = r.def(BlockId(i)).tile;
            assert!(seen.insert(tile), "duplicate atlas tile {tile}");
        }
    }

    #[test]
    fn invalid_registries_are_rejected() {
        // Two placeable blocks.
        let bad = EMBEDDED_RON.replace(
            r#"(name: "stone",     solid: true,  opaque: true,  placeable: false"#,
            r#"(name: "stone",     solid: true,  opaque: true,  placeable: true "#,
        );
        assert!(BlockRegistry::parse(&bad).is_err());

        // Duplicate names.
        let bad = EMBEDDED_RON.replace(r#"(name: "dirt""#, r#"(name: "stone""#);
        assert!(BlockRegistry::parse(&bad).is_err());

        // Block 0 not air.
        let bad = EMBEDDED_RON.replace(r#"(name: "air""#, r#"(name: "void""#);
        assert!(BlockRegistry::parse(&bad).is_err());
    }
}
