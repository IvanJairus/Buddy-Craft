//! `room` — the room state machine, runtime-agnostic (plan §6, M2).
//!
//! The crate exists so that "which host do we use" is a configuration change
//! rather than an architectural one. Nothing here knows what a socket is, what a
//! wall clock is, or whether it is running inside a Cloudflare Durable Object or
//! a `tokio` task: the host forwards bytes and calls [`lifecycle::Lifecycle::update`]
//! once per tick, and every decision comes back as a value.
//!
//! # Invariants
//!
//! * No I/O, no threads, no `async`, no `std::time`. Time is a [`sim::Tick`].
//!   `tests/no_clock.rs` fails the build if that stops being true.
//! * All game rules live in `sim`, which is compiled for both the server and the
//!   client. This crate decides *when* to run them, never *what* they do.
//! * Every state change is a function of (state, inputs). Two rooms ticked in the
//!   same process share nothing.

pub mod lifecycle;

pub use lifecycle::{Conditions, Effect, IllegalTransition, Lifecycle, Phase};
