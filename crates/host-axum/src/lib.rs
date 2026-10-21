//! `host-axum`: the local development and fallback WebSocket host (T-045).
//!
//! The host owns HTTP concerns only. Room lifecycle and rules stay in `room`;
//! this crate validates an upgrade, starts one task per room, forwards bytes and
//! exposes `/healthz`. The same `room::Match` runs in `host-worker` without this
//! code, which is the point of the runtime-agnostic split.

pub mod rooms;
pub mod ws;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

pub use rooms::{RoomCommand, RoomHub, HUMAN_ID, ROOM_SEED, TICK_MS};

/// Shared state for both routes.
#[derive(Clone)]
pub struct AppState {
    /// Room map and task factory.
    pub hub: RoomHub,
    /// Browser Origins accepted by `/ws`. An empty list rejects every browser.
    pub allowed_origins: Arc<Vec<String>>,
    /// Process start time, for `/healthz`.
    pub started: Instant,
}

impl AppState {
    /// Build state with summaries under `summaries`.
    pub fn new(allowed_origins: Vec<String>, summaries: PathBuf) -> AppState {
        AppState {
            hub: RoomHub::new(summaries),
            allowed_origins: Arc::new(allowed_origins),
            started: Instant::now(),
        }
    }

    /// The health document. Kept as a method so a test can inspect it without
    /// binding a socket.
    pub async fn health(&self) -> Value {
        json!({
            "status": "ok",
            "rooms": self.hub.room_count().await,
            "uptime_s": self.started.elapsed().as_secs(),
        })
    }
}

/// The HTTP application.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/ws", get(ws::ws_handler))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(state.health().await)
}

/// The default browser origins for `pnpm dev` and `vite preview`.
pub fn default_origins() -> Vec<String> {
    [
        "http://127.0.0.1:5173",
        "http://localhost:5173",
        "http://127.0.0.1:4173",
        "http://localhost:4173",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_reports_status_rooms_and_uptime() {
        let state = AppState::new(default_origins(), std::env::temp_dir().join("cb-health"));
        let value = state.health().await;
        assert_eq!(value["status"], "ok");
        assert_eq!(value["rooms"], 0);
        assert!(value["uptime_s"].is_u64());
    }
}
