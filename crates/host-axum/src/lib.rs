//! `host-axum`: the local development and fallback WebSocket host (T-045).
//!
//! The host owns HTTP concerns only. Room lifecycle and rules stay in `room`;
//! this crate validates an upgrade, starts one task per room, forwards bytes and
//! exposes `/healthz`. The same `room::Match` runs in `host-worker` without this
//! code, which is the point of the runtime-agnostic split.

pub mod rooms;
pub mod telemetry;
pub mod ws;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
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
    /// Runtime kill switches.
    pub switches: Arc<telemetry::Switches>,
    /// Accelerated E2E commands and spawn overrides.
    pub e2e: bool,
    /// Per-IP socket budget (T-050).
    pub connections: Arc<Mutex<room::ConnectionBudget>>,
}

impl AppState {
    /// Build state with summaries under `summaries`.
    pub fn new(allowed_origins: Vec<String>, summaries: PathBuf) -> AppState {
        AppState {
            hub: RoomHub::new(summaries),
            allowed_origins: Arc::new(allowed_origins),
            started: Instant::now(),
            switches: Arc::new(telemetry::Switches::from_env()),
            e2e: std::env::var("CB_E2E").ok().as_deref() == Some("1"),
            connections: Arc::new(Mutex::new(room::ConnectionBudget::default())),
        }
    }

    /// Enable the debug command endpoint (tests only).
    pub fn with_e2e(mut self, enabled: bool) -> AppState {
        self.e2e = enabled;
        self
    }

    /// Replace the environment-derived switches (tests and embedders).
    pub fn with_switches(mut self, switches: telemetry::Switches) -> AppState {
        self.switches = Arc::new(switches);
        self
    }

    /// The health document. Kept as a method so a test can inspect it without
    /// binding a socket.
    pub async fn health(&self) -> Value {
        json!({
            "status": "ok",
            "rooms": self.hub.room_count().await,
            "uptime_s": self.started.elapsed().as_secs(),
            "rooms_enabled": self.switches.rooms_enabled,
            "protocol_min_version": self.switches.protocol_min_version,
            "client_build_override": self.switches.client_build_override,
        })
    }
}

/// The HTTP application.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/ws", get(ws::ws_handler))
        .route("/telemetry", post(client_telemetry))
        .route("/debug/command", post(debug_command))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(state.health().await)
}

#[derive(serde::Deserialize)]
struct DebugCommand {
    room: String,
    action: String,
    player: Option<u8>,
    team: Option<u8>,
    crates: Option<u32>,
    amount: Option<i32>,
}

async fn debug_command(
    State(state): State<AppState>,
    Json(command): Json<DebugCommand>,
) -> StatusCode {
    use sim::{PlayerId, TeamId};
    if !state.e2e {
        return StatusCode::NOT_FOUND;
    }
    let tx = state.hub.sender(&command.room).await;
    let sent = match command.action.as_str() {
        "grant" => tx
            .send(RoomCommand::DebugGrant {
                player: PlayerId(command.player.unwrap_or(0)),
                crates: command.crates.unwrap_or(0),
            })
            .await
            .is_ok(),
        "damage-core" => tx
            .send(RoomCommand::DebugDamage {
                team: TeamId(command.team.unwrap_or(0)),
                amount: command.amount.unwrap_or(0),
            })
            .await
            .is_ok(),
        "place-crate" => tx
            .send(RoomCommand::DebugPlace {
                team: TeamId(command.team.unwrap_or(0)),
            })
            .await
            .is_ok(),
        _ => false,
    };
    if sent {
        StatusCode::ACCEPTED
    } else {
        StatusCode::BAD_REQUEST
    }
}

async fn client_telemetry(State(_state): State<AppState>, body: Bytes) -> StatusCode {
    if body.len() > 4096 {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(value) => {
            telemetry::emit("client", value);
            StatusCode::NO_CONTENT
        }
        Err(_) => StatusCode::BAD_REQUEST,
    }
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
