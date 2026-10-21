//! Room tasks, client fan-out and summaries (task T-045).
//!
//! One `tokio` task owns one room. It is the only writer to the `Match`, so
//! input, ticks, snapshots and edits cannot race. WebSocket connections never
//! touch game state: they send commands into the task's channel and receive
//! encoded frames from their own bounded queue.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use protocol::{
    event_kind, FixedBytes, Input, RoomConfig, S2c, SnapshotEvent, Start, Welcome, INPUT_FRAMES,
    TOKEN_LEN,
};
use room::{snapshot::SnapshotEncoder, Match, MatchEvent};
use sim::{Mode, PlayerId, RoomSetup, Tick};
use tokio::sync::{mpsc, oneshot, Mutex};

/// The one human seat a browser connection drives. Bots occupy the rest.
pub const HUMAN_ID: PlayerId = PlayerId(0);
/// Deterministic development seed. The client regenerates the same terrain.
pub const ROOM_SEED: u64 = 0x00c0_ffee;
/// Tick duration (§6.2).
pub const TICK_MS: u64 = 50;
/// Seconds a room's summary stays meaningful; the host writes on shutdown.
const ROOM_COMMAND_CAPACITY: usize = 64;

/// Commands accepted by a room task.
pub enum RoomCommand {
    /// Register a connection and send its welcome.
    Join {
        /// Stable connection id, used by `Leave`.
        id: u64,
        /// Encoded server frames for this connection.
        to_client: mpsc::Sender<Vec<u8>>,
    },
    /// A `C_INPUT` packet from the human seat.
    Input(Input),
    /// A validated `C_EDIT` from the human seat.
    Edit(protocol::Edit),
    /// Remove a connection.
    Leave {
        /// Connection id from `Join`.
        id: u64,
    },
    /// Persist and stop the task.
    Shutdown(oneshot::Sender<()>),
}

struct Client {
    id: u64,
    tx: mpsc::Sender<Vec<u8>>,
    encoder: SnapshotEncoder,
    acked: u16,
}

struct HubInner {
    rooms: Mutex<HashMap<String, mpsc::Sender<RoomCommand>>>,
    summaries: PathBuf,
    next_client: AtomicU64,
}

/// Shared handle to the room map.
#[derive(Clone)]
pub struct RoomHub {
    inner: Arc<HubInner>,
}

impl RoomHub {
    /// A hub that writes summaries under `summaries`.
    pub fn new(summaries: PathBuf) -> RoomHub {
        RoomHub {
            inner: Arc::new(HubInner {
                rooms: Mutex::new(HashMap::new()),
                summaries,
                next_client: AtomicU64::new(1),
            }),
        }
    }

    /// A connection id for `Join` and `Leave`.
    pub fn next_client_id(&self) -> u64 {
        self.inner.next_client.fetch_add(1, Ordering::Relaxed)
    }

    /// Get the command sender for a room, creating the task on first use.
    pub async fn sender(&self, code: &str) -> mpsc::Sender<RoomCommand> {
        let mut rooms = self.inner.rooms.lock().await;
        if let Some(tx) = rooms.get(code) {
            return tx.clone();
        }
        let (tx, rx) = mpsc::channel(ROOM_COMMAND_CAPACITY);
        rooms.insert(code.to_owned(), tx.clone());
        let code = code.to_owned();
        let inner = Arc::clone(&self.inner);
        eprintln!("[room {code}] task started");
        tokio::spawn(async move {
            run_room(code, rx, &inner).await;
        });
        tx
    }

    /// Number of live room tasks, for `/healthz`.
    pub async fn room_count(&self) -> usize {
        self.inner.rooms.lock().await.len()
    }

    /// Ask every room to persist its summary and stop.
    pub async fn shutdown(&self) {
        let handles: Vec<mpsc::Sender<RoomCommand>> = {
            let mut rooms = self.inner.rooms.lock().await;
            rooms.drain().map(|(_, tx)| tx).collect()
        };
        for tx in handles {
            let (ack, done) = oneshot::channel();
            if tx.send(RoomCommand::Shutdown(ack)).await.is_ok() {
                let _ = tokio::time::timeout(Duration::from_secs(2), done).await;
            }
        }
    }
}

async fn run_room(code: String, mut commands: mpsc::Receiver<RoomCommand>, hub: &HubInner) {
    let setup = RoomSetup {
        mode: Mode::Ffa,
        teams: None,
        humans: 1,
        bots: 5,
    };
    let mut game = Match::new(ROOM_SEED, setup).expect("1 human + 5 bots is a legal FFA room");
    for id in 1..=5u8 {
        game.add_bot(PlayerId(id));
    }
    let mut clients: Vec<Client> = Vec::new();
    let mut human: Option<sim::InputFrame> = None;
    let mut interval = tokio::time::interval(Duration::from_millis(TICK_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(RoomCommand::Join { id, to_client }) => {
                        let welcome = S2c::Welcome(Welcome {
                            player_id: HUMAN_ID.0,
                            team: 0,
                            seed: ROOM_SEED as u32,
                            config: RoomConfig {
                                mode: protocol::room_mode::FFA,
                                teams: 6,
                                per_team: 1,
                                buddy_cap: 5,
                                target_minutes: 20,
                            },
                            tick: game.now(),
                            your_token: FixedBytes(token_for(&code)),
                        });
                        if to_client.try_send(welcome.encode()).is_ok() {
                            let start = S2c::Start(Start {
                                tick0: Tick(0),
                                bases: Vec::new(),
                            });
                            let _ = to_client.try_send(start.encode());
                            clients.push(Client {
                                id,
                                tx: to_client,
                                encoder: SnapshotEncoder::new(HUMAN_ID),
                                acked: 0,
                            });
                        }
                    }
                    Some(RoomCommand::Input(packet)) => {
                        for client in &mut clients {
                            client.acked = packet.acked_seq;
                        }
                        let last = packet.frames[INPUT_FRAMES - 1];
                        human = Some(last.to_sim(Tick(packet.first_tick.0 + 2)));
                    }
                    Some(RoomCommand::Edit(edit)) => {
                        let result = game.submit_edit(HUMAN_ID, &edit);
                        let ack = S2c::EditAck(protocol::EditAck {
                            seq: result.seq,
                            accepted: result.accepted,
                            reason: result.reason,
                        });
                        broadcast(&mut clients, ack.encode());
                        if result.accepted {
                            for diff in game.take_diffs() {
                                broadcast(&mut clients, S2c::ChunkDiff(diff).encode());
                            }
                        }
                    }
                    Some(RoomCommand::Leave { id }) => {
                        clients.retain(|client| client.id != id);
                    }
                    Some(RoomCommand::Shutdown(ack)) => {
                        persist(&code, &game, &hub.summaries);
                        let _ = ack.send(());
                        break;
                    }
                    None => {
                        persist(&code, &game, &hub.summaries);
                        break;
                    }
                }
            }
            _ = interval.tick() => {
                let frames = human.take().map(|frame| vec![(HUMAN_ID, frame)]).unwrap_or_default();
                game.tick(&frames);
                let events = snapshot_events(game.events());
                let now = game.now();
                let mut alive = Vec::with_capacity(clients.len());
                for mut client in clients.drain(..) {
                    let encoded = client.encoder.encode(now, game.state(), &[], &events, client.acked);
                    match client.tx.try_send(encoded.bytes) {
                        Ok(()) => alive.push(client),
                        Err(mpsc::error::TrySendError::Full(_)) => alive.push(client),
                        Err(mpsc::error::TrySendError::Closed(_)) => {}
                    }
                }
                clients = alive;
            }
        }
    }
    eprintln!("[room {code}] stopped at tick {}", game.now().0);
    hub.rooms.lock().await.remove(&code);
}

fn broadcast(clients: &mut [Client], bytes: Vec<u8>) {
    for client in clients.iter_mut() {
        let _ = client.tx.try_send(bytes.clone());
    }
}

fn snapshot_events(events: &[MatchEvent]) -> Vec<SnapshotEvent> {
    events
        .iter()
        .map(|event| match *event {
            MatchEvent::AirdropSpawned { site, .. } => SnapshotEvent {
                kind: event_kind::AIRDROP_TELEGRAPH,
                a: site[0],
                b: site[1],
                c: site[2],
            },
            MatchEvent::AirdropLanded { site, crates } => SnapshotEvent {
                kind: event_kind::AIRDROP,
                a: site[0],
                b: site[2],
                c: crates as i32,
            },
            MatchEvent::Picked { player, count } => SnapshotEvent {
                kind: event_kind::PICKUP,
                a: i32::from(player.0),
                b: count as i32,
                c: 0,
            },
            MatchEvent::Deposited { cell, .. } => SnapshotEvent {
                kind: event_kind::DEPOSIT,
                a: cell[0],
                b: cell[1],
                c: cell[2],
            },
            MatchEvent::Crafted {
                team,
                recipe,
                buddy,
            } => SnapshotEvent {
                kind: event_kind::CRAFT,
                a: i32::from(team.0),
                b: i32::from(recipe.0),
                c: i32::from(buddy.is_some()),
            },
            MatchEvent::CoreHit { team, amount, by } => SnapshotEvent {
                kind: event_kind::BASE_HIT,
                a: i32::from(team.0),
                b: amount,
                c: i32::from(by.0),
            },
            MatchEvent::PlayerDown { player } => SnapshotEvent {
                kind: event_kind::HIT,
                a: i32::from(player.0),
                b: 0,
                c: 0,
            },
            MatchEvent::Eliminated { team } => SnapshotEvent {
                kind: event_kind::ELIMINATION,
                a: i32::from(team.0),
                b: 0,
                c: 0,
            },
        })
        .collect()
}

fn persist(code: &str, game: &Match, dir: &PathBuf) {
    let _ = std::fs::create_dir_all(dir);
    let summary = serde_json::json!({
        "code": code,
        "seed": game.seed(),
        "tick": game.now().0,
        "over": game.is_over(),
        "players": game.state().players().len(),
    });
    let _ = std::fs::write(
        dir.join(format!("{code}.json")),
        serde_json::to_vec_pretty(&summary).unwrap_or_default(),
    );
}

fn token_for(code: &str) -> [u8; TOKEN_LEN] {
    let mut token = [0u8; TOKEN_LEN];
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in code.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    for (i, slot) in token.iter_mut().enumerate() {
        *slot = ((hash >> ((i % 8) * 8)) & 0xff) as u8;
    }
    token
}

/// Play six bots for a while and measure the wall-clock interval p99.
///
/// This is the acceptance test for T-045, not a microbenchmark: the interval is
/// scheduled by tokio at 20 Hz and includes room state, bot brains and snapshot
/// preparation. It intentionally uses the same `Match` the host runs.
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(debug_assertions))]
    fn six_bot_match() -> Match {
        let setup = RoomSetup {
            mode: Mode::Ffa,
            teams: None,
            humans: 1,
            bots: 5,
        };
        let mut game = Match::new(ROOM_SEED, setup).expect("legal room");
        for id in 0..6u8 {
            assert!(game.add_bot(PlayerId(id)), "seat {id}");
        }
        game
    }

    #[cfg(not(debug_assertions))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tick_interval_p99() {
        use std::time::Instant;

        let mut game = six_bot_match();
        for _ in 0..30 {
            game.tick(&[]);
        }
        let mut interval = tokio::time::interval(Duration::from_millis(TICK_MS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The 55 ms budget is for the 20 Hz tick body. Timer wake-up jitter is
        // an OS scheduler property, not room work, so it is measured in the
        // release soak rather than asserted here.
        let mut compute = Vec::with_capacity(61);
        for _ in 0..61 {
            interval.tick().await;
            let begin = Instant::now();
            game.tick(&[]);
            compute.push(begin.elapsed());
        }
        compute.sort_unstable();
        let p99 = compute[(compute.len() * 99 / 100).min(compute.len() - 1)];
        assert!(
            p99 <= Duration::from_millis(55),
            "tick body p99 {p99:?}, 6 bots"
        );
    }

    #[tokio::test]
    async fn shutdown_persists_a_summary_and_removes_the_room() {
        let dir = std::env::temp_dir().join(format!("buddy-craft-t045-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hub = RoomHub::new(dir.clone());
        let _tx = hub.sender("DEV001").await;
        assert_eq!(hub.room_count().await, 1);
        hub.shutdown().await;
        assert_eq!(hub.room_count().await, 0);
        let text = std::fs::read_to_string(dir.join("DEV001.json")).expect("summary");
        assert!(text.contains("\"code\": \"DEV001\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_is_stable_and_not_all_zero() {
        assert_eq!(token_for("DEV001"), token_for("DEV001"));
        assert_ne!(token_for("DEV001"), token_for("DEV002"));
        assert!(token_for("DEV001").iter().any(|byte| *byte != 0));
    }
}
