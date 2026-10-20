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
//! M0 (plan §16) ships `fixed`, `ids`, `rng`, `time` and `tuning`.
//! `movement`, `combat`, `recipe`, `airdrop`, `buddy`, `base` and `match_rules`
//! land in M1 (T-009 … T-016).

pub mod fixed;
pub mod ids;
pub mod rng;
pub mod sin_table;
pub mod time;
pub mod tuning;

pub use fixed::{cos, cos_yaw, sin, sin_yaw, Fixed, ONE, ZERO};
pub use ids::{EntityId, PlayerId, TeamId};
pub use rng::{RngStreams, Xoshiro256};
pub use time::Tick;
