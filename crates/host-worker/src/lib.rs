//! `host-worker`: registry policy plus the Cloudflare adapter surface (T-046/T-047).
//!
//! The pure registry in [`registry`] owns code allocation, collision checks,
//! per-IP lockout and the quota guard. It has no Cloudflare types and is tested
//! natively.
//!
//! The Durable Object adapter is intentionally not claimed yet. It needs
//! `worker-build` and a live `wrangler deploy`, which the T-046 commit marks
//! BLOCKED; the room tick loop is already proven in `host-axum` and the shared
//! `room::Match` is the same code.

pub mod registry;
