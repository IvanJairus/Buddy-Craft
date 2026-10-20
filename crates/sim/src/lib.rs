//! `sim` — the deterministic rules of Cratebound.
//!
//! This crate is compiled twice: to `wasm32-unknown-unknown` for client-side
//! prediction, and natively for the authoritative server. There is exactly one
//! implementation of every rule (plan A-07), which is what makes prediction and
//! authority agree by construction rather than by discipline.
//!
//! # Invariants
//!
//! * No I/O, no threads, no wall clock. Time is a [`Tick`] counter.
//! * No floating point in authoritative state. Everything is [`Fixed`]
//!   (Q16.16). The one exception is the conversion helpers in [`fixed`], which
//!   are used by tests and by data authoring; `tests/no_float.rs` fails the
//!   build if `f32`/`f64` appear anywhere else in this crate.
//! * No `HashMap` in any code path that affects state — iteration order must be
//!   identical on every target.
//!
//! # Module status
//!
//! M0 (plan §16) shipped `fixed`, `ids`, `rng`, `time` and `tuning`.
//! M1 adds `vec`, `input`, `world_query`, `movement`, `pad`, `recipe` and
//! `combat`; `buddy`, `base` and `match_rules` follow in the same milestone.

pub mod base;
pub mod buddy;
pub mod combat;
pub mod fixed;
pub mod ids;
pub mod input;
pub mod match_rules;
pub mod movement;
pub mod pad;
pub mod pathfind;
pub mod recipe;
pub mod rng;
pub mod sin_table;
pub mod state;
pub mod time;
pub mod tuning;
pub mod vec;
pub mod world_query;

pub use base::{Base, Core, Elimination, PlayerLife, PlayerSlot};
pub use buddy::{
    Buddy, BuddyAction, BuddyContext, BuddyIntent, BuddyKind, BuddyState, BuddyStats, Stance,
    Target,
};
pub use combat::{Combatant, DamageEvent, DamageKind, Projectile, WeaponStats};
pub use fixed::{cos, cos_yaw, sin, sin_yaw, Fixed, ONE, ZERO};
pub use ids::{EntityId, PlayerId, TeamId};
pub use input::InputFrame;
pub use match_rules::{
    cap_row, check_outcome, CapRow, EndReason, Layout, MatchRules, Mode, Outcome, TeamStatus,
    CAP_MATRIX, MAX_ENTITIES,
};
pub use movement::{MoveConfig, MoveFlags, MoveIntent, MoveState};
pub use pad::{Pad, Pads, PlaceError};
pub use recipe::{BlockPos, Craft, CraftOutcome, RecipeBook, RecipeId, RecipeOutput};
pub use rng::{RngStreams, Xoshiro256};
pub use state::{ring_pad_origins, BuddyCapReached, MatchState, RoomSetup, SetupError, TeamState};
pub use time::Tick;
pub use vec::Vec3;
pub use world_query::VoxelQuery;
