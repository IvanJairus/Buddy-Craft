//! Offline match session (task T-048).
//!
//! The room loop runs in the client wasm module: one `room::Match` with the
//! human in seat 0 and bots in the rest. The Worker wrapper feeds the same C2S
//! bytes and receives the same S2C bytes as the network path. No rules are
//! reimplemented in TypeScript.

use std::collections::VecDeque;

use protocol::{
    end_reason, event_kind, FixedBytes, PlayerStats, RoomConfig, S2c, SnapshotEvent, Start,
    Welcome, INPUT_FRAMES,
};
use room::{Match, MatchEvent, SnapshotEncoder};
use sim::{input::InputFrame, Mode, PlayerId, RoomSetup, Tick};

/// The offline session owns the authoritative match for one player.
pub struct OfflineSession {
    game: Match,
    encoder: SnapshotEncoder,
    human: Option<InputFrame>,
    acked: u16,
    queue: VecDeque<Vec<u8>>,
    ended: bool,
    seed: u64,
    teams: u8,
    mode: u8,
}

impl OfflineSession {
    /// Start an FFA or Teams match with five bots.
    pub fn new(seed: u64, mode: u8, teams: u8) -> Option<OfflineSession> {
        let ffa = mode == protocol::room_mode::FFA;
        let setup = RoomSetup {
            mode: if ffa { Mode::Ffa } else { Mode::Teams },
            teams: if ffa { None } else { Some(teams.clamp(2, 3)) },
            humans: 1,
            bots: 5,
        };
        let mut game = Match::new(seed, setup).ok()?;
        for id in 1..=5u8 {
            game.add_bot(PlayerId(id));
        }
        let mut session = OfflineSession {
            game,
            encoder: SnapshotEncoder::new(PlayerId(0)),
            human: None,
            acked: 0,
            queue: VecDeque::new(),
            ended: false,
            seed,
            teams: if ffa { 6 } else { teams.clamp(2, 3) },
            mode: if ffa {
                protocol::room_mode::FFA
            } else {
                protocol::room_mode::TEAMS
            },
        };
        session.queue_start();
        Some(session)
    }

    /// The seed the client regenerated terrain from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Number of bases in this layout (six in FFA).
    pub fn teams(&self) -> u8 {
        self.teams
    }

    /// The worldgen bases for this seed and team count.
    pub fn bases(&self) -> Vec<world::gen::Base> {
        world::gen::bases_for(self.seed, self.teams)
    }

    /// Decode one C2S frame. Bad frames are ignored by the offline host; the
    /// browser already validated its own input.
    pub fn message(&mut self, bytes: &[u8]) {
        if bytes.first() == Some(&0xf0) {
            self.debug_message(bytes);
            return;
        }
        let Ok(message) = protocol::codec::decode_checked(bytes, protocol::codec::Bounds::STANDARD)
        else {
            return;
        };
        match message {
            protocol::C2s::Input(packet) => {
                self.acked = packet.acked_seq;
                let last = packet.frames[INPUT_FRAMES - 1];
                self.human = Some(last.to_sim(Tick(self.game.now().0.wrapping_add(1))));
            }
            protocol::C2s::Edit(edit) => {
                let result = self.game.submit_edit(PlayerId(0), &edit);
                self.queue.push_back(
                    S2c::EditAck(protocol::EditAck {
                        seq: result.seq,
                        accepted: result.accepted,
                        reason: result.reason,
                    })
                    .encode(),
                );
                if result.accepted {
                    for diff in self.game.take_diffs() {
                        self.queue.push_back(S2c::ChunkDiff(diff).encode());
                    }
                }
            }
            protocol::C2s::Ping(ping) => {
                self.queue
                    .push_back(S2c::Pong(protocol::Pong { t: ping.t }).encode());
            }
            _ => {}
        }
    }

    /// Accelerated E2E setup, kept off the normal C2S path: 0xF0 is not a
    /// protocol tag. The commands only grant state or fill a pad; the next
    /// ordinary tick still runs the real crafting and outcome rules.
    fn debug_message(&mut self, bytes: &[u8]) {
        match bytes.get(1) {
            Some(1) if bytes.len() >= 5 => {
                let player = PlayerId(bytes[2]);
                let crates = u32::from(u16::from_le_bytes([bytes[3], bytes[4]]));
                self.game.grant_crates(player, crates);
            }
            Some(2) if bytes.len() >= 7 => {
                let team = sim::TeamId(bytes[2]);
                let amount = i32::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]);
                self.game.damage_core(team, amount);
            }
            Some(3) if bytes.len() >= 5 => {
                let team = sim::TeamId(bytes[2]);
                let recipe = sim::RecipeId(u16::from_le_bytes([bytes[3], bytes[4]]));
                if self.game.debug_place_recipe(team, recipe) {
                    for diff in self.game.take_diffs() {
                        self.queue.push_back(S2c::ChunkDiff(diff).encode());
                    }
                }
            }
            Some(4) if bytes.len() >= 4 => {
                let team = sim::TeamId(bytes[2]);
                let count = bytes[3] as usize;
                let end = 4 + count * 3;
                if bytes.len() >= end {
                    let blocks: Vec<[i32; 3]> = (0..count)
                        .map(|i| {
                            let at = 4 + i * 3;
                            [
                                i32::from(bytes[at]),
                                i32::from(bytes[at + 1]),
                                i32::from(bytes[at + 2]),
                            ]
                        })
                        .collect();
                    if self.game.debug_place_blocks(team, &blocks) {
                        for diff in self.game.take_diffs() {
                            self.queue.push_back(S2c::ChunkDiff(diff).encode());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Advance one 50 ms tick and queue the resulting snapshot.
    pub fn tick(&mut self) {
        if self.ended {
            return;
        }
        let human = self
            .human
            .take()
            .map(|frame| vec![(PlayerId(0), frame)])
            .unwrap_or_default();
        self.game.tick(&human);
        let events = snapshot_events(self.game.events());
        let encoded =
            self.encoder
                .encode(self.game.now(), self.game.state(), &[], &events, self.acked);
        self.queue.push_back(encoded.bytes);
        if self.game.is_over() {
            let outcome = self.game.outcome();
            let winner = outcome
                .winner()
                .map_or(-1, |team| i8::try_from(team.0).unwrap_or(-1));
            let reason = match outcome.reason() {
                Some(sim::match_rules::EndReason::NoHumans) => end_reason::NO_HUMANS,
                Some(sim::match_rules::EndReason::Draw) => end_reason::DRAW,
                _ => end_reason::CORE_DESTROYED,
            };
            let stats = self
                .game
                .state()
                .players()
                .iter()
                .map(|slot| PlayerStats {
                    id: slot.id.0,
                    kills: 0,
                    deaths: 0,
                    damage: 0,
                    crates: 0,
                })
                .collect();
            self.queue.push_back(
                S2c::End(protocol::End {
                    winner_team: winner,
                    reason,
                    stats,
                })
                .encode(),
            );
            self.ended = true;
        }
    }

    /// Pop one S2C frame, or `None` when the queue is empty.
    pub fn poll(&mut self) -> Option<Vec<u8>> {
        self.queue.pop_front()
    }

    /// FNV of the authoritative match state, for the parity test.
    pub fn state_hash(&self) -> u64 {
        self.game.state().state_hash()
    }

    fn queue_start(&mut self) {
        self.queue.push_back(
            S2c::Welcome(Welcome {
                player_id: 0,
                team: 0,
                seed: self.seed as u32,
                config: RoomConfig {
                    mode: self.mode,
                    teams: self.teams,
                    per_team: if self.teams == 6 { 1 } else { 3 },
                    buddy_cap: 5,
                    target_minutes: 20,
                },
                tick: self.game.now(),
                your_token: FixedBytes([0x4f; protocol::TOKEN_LEN]),
            })
            .encode(),
        );
        self.queue.push_back(
            S2c::Start(Start {
                tick0: Tick(0),
                bases: Vec::new(),
            })
            .encode(),
        );
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
            MatchEvent::CraftBlocked { team, recipe } => SnapshotEvent {
                kind: event_kind::CRAFT_BLOCKED,
                a: i32::from(team.0),
                b: i32::from(recipe.0),
                c: 0,
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

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::WireInput;

    fn online_match() -> Match {
        let setup = RoomSetup {
            mode: Mode::Ffa,
            teams: None,
            humans: 1,
            bots: 5,
        };
        let mut game = Match::new(0x00c0_ffee, setup).expect("match");
        for id in 1..=5u8 {
            game.add_bot(PlayerId(id));
        }
        game
    }

    #[test]
    fn offline_and_online_share_rules() {
        let mut offline = OfflineSession::new(0x00c0_ffee, 0, 0).expect("offline");
        let mut online = online_match();
        let frame = InputFrame {
            tick: Tick(1),
            move_x: 0,
            move_z: 100,
            yaw: 32,
            pitch: 0,
            buttons: 0,
        };
        offline.human = Some(frame);
        online.tick(&[(PlayerId(0), frame)]);
        offline.tick();
        assert_eq!(offline.state_hash(), online.state().state_hash());
        assert!(offline.poll().is_some(), "the welcome frame is queued");
    }

    #[test]
    fn offline_messages_produce_wire_frames() {
        let mut offline = OfflineSession::new(7, protocol::room_mode::TEAMS, 2).expect("offline");
        let frame = InputFrame {
            tick: Tick(1),
            move_x: 0,
            move_z: 100,
            yaw: 0,
            pitch: 0,
            buttons: 0,
        };
        let packet = protocol::Input {
            seq: 1,
            acked_seq: 0,
            first_tick: Tick(1),
            frames: [WireInput::from_sim(&frame); INPUT_FRAMES],
        };
        offline.message(&protocol::C2s::Input(packet).encode());
        offline.tick();
        let frames: Vec<Vec<u8>> = std::iter::from_fn(|| offline.poll()).collect();
        assert!(frames.iter().any(|bytes| bytes[0] == 0x81), "welcome");
        let snapshot = frames
            .iter()
            .find(|bytes| bytes[0] == 0x84)
            .expect("snapshot");
        match protocol::decode_s2c(snapshot).expect("decode") {
            S2c::Snapshot(snapshot) => {
                assert!(
                    snapshot.players.len() >= 2,
                    "the viewer and at least one team-mate bot"
                );
            }
            other => panic!("expected snapshot, got {other:?}"),
        }
    }
}
