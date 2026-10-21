//! Client-side prediction and reconciliation (plan §6.4, task T-041).
//!
//! The client applies its own inputs immediately, keeps the last 128 of them
//! with the state each produced, and reconciles against the server's state at
//! an acknowledged sequence. A correction smaller than 5 cm is ignored; a
//! larger one resets the body to the server state and replays every unacked
//! input in order, so the next frame is still the player's current intent.
//!
//! # The visual residual
//!
//! Resetting the authoritative state would snap the camera if the render read
//! it directly. The predictor keeps a visual offset equal to the old rendered
//! position minus the corrected one, and decays that offset with
//! `exp(-dt / 0.15)`. The first exponential step after a 1 m correction is about
//! 10 cm, so the step is also clamped to 5 cm per frame: the residual decays
//! smoothly instead of turning one correction into a visible jump.
//!
//! # What this module does not do
//!
//! It does not decode snapshots. [`Snapshot`] is the state a decoder hands it,
//! and T-043 owns the wire form. The bridge does not expose this predictor yet;
//! T-043/T-048 wire it to `cb_tick` and the render buffer.

use sim::fixed::Fixed;
use sim::input::InputFrame;
use sim::movement::{MoveConfig, MoveState};
use sim::player::{self, PlayerBody};
use sim::time::Tick;
use sim::vec::Vec3;
use sim::world_query::VoxelQuery;

/// The client keeps this many unacked inputs (§6.4).
pub const RING_CAPACITY: usize = 128;
/// Corrections at or below this distance are not worth a replay.
pub const CORRECTION_THRESHOLD: Fixed = Fixed::from_raw(3_277); // 0.05 m
/// The visual residual's exponential time constant.
pub const SMOOTHING_SECONDS: f32 = 0.15;
/// The largest movement of the rendered position in one frame.
pub const MAX_VISUAL_STEP: Fixed = Fixed::from_raw(3_277); // 0.05 m

/// The server state at an acknowledged input sequence.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    /// Server tick the state belongs to.
    pub tick: Tick,
    /// Last input sequence the server processed.
    pub acked_seq: u32,
    /// Full movement state; a decoder that only has a position can build one
    /// with zero velocity and let the replay restore it.
    pub state: MoveState,
    /// Facing.
    pub yaw: u8,
    /// Facing.
    pub pitch: u8,
    /// Buttons held at the acknowledged tick, for the jump edge on resync.
    pub buttons: u8,
    /// Health.
    pub hp: i32,
}

#[derive(Clone, Copy)]
struct Record {
    seq: u32,
    frame: InputFrame,
    state_after: MoveState,
    yaw: u8,
    pitch: u8,
}

/// The result of one reconciliation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reconcile {
    /// The prediction was within the 5 cm threshold; nothing changed.
    Ignored,
    /// The body was reset and the unacked inputs were replayed.
    Corrected {
        /// Distance between the prediction and the server, before correction.
        error: Fixed,
    },
    /// The acknowledged sequence had fallen out of the ring; the body was
    /// reset to the snapshot and the stale ring was discarded.
    Resynced,
}

pub struct Predictor {
    body: PlayerBody,
    cfg: MoveConfig,
    ring: [Option<Record>; RING_CAPACITY],
    head: usize,
    len: usize,
    visual: Vec3,
    dropped: u32,
    corrections: u32,
    resyncs: u32,
}

impl Predictor {
    /// A predictor starting from the same body the server spawned.
    pub fn new(body: PlayerBody, cfg: MoveConfig) -> Predictor {
        Predictor {
            body,
            cfg,
            ring: [None; RING_CAPACITY],
            head: 0,
            len: 0,
            visual: Vec3::ZERO,
            dropped: 0,
            corrections: 0,
            resyncs: 0,
        }
    }

    /// Apply one local input and remember it. Returns the rendered position.
    pub fn push<W: VoxelQuery + ?Sized>(&mut self, seq: u32, frame: InputFrame, world: &W) -> Vec3 {
        if self.len == RING_CAPACITY {
            self.ring[self.head] = None;
            self.head = (self.head + 1) % RING_CAPACITY;
            self.len -= 1;
            self.dropped += 1;
        }
        let previous = self.body.prev_buttons;
        player::apply(&mut self.body, &frame, previous, world, &self.cfg);
        let record = Record {
            seq,
            frame,
            state_after: self.body.state,
            yaw: self.body.yaw,
            pitch: self.body.pitch,
        };
        let index = (self.head + self.len) % RING_CAPACITY;
        self.ring[index] = Some(record);
        self.len += 1;
        self.render_pos()
    }

    /// Reconcile against the server. See the module docs for the order.
    pub fn reconcile<W: VoxelQuery + ?Sized>(
        &mut self,
        snapshot: Snapshot,
        world: &W,
    ) -> Reconcile {
        let mut acked_index = None;
        for offset in 0..self.len {
            let index = (self.head + offset) % RING_CAPACITY;
            if self.ring[index].is_some_and(|record| record.seq == snapshot.acked_seq) {
                acked_index = Some(index);
                break;
            }
        }

        let Some(acked_index) = acked_index else {
            return self.resync(snapshot);
        };
        let record = self.ring[acked_index].expect("index points at a record");
        let error = record.state_after.pos.distance(snapshot.state.pos);
        if error <= CORRECTION_THRESHOLD {
            self.drop_through(snapshot.acked_seq);
            return Reconcile::Ignored;
        }

        let rendered = self.render_pos();
        self.body.state = snapshot.state;
        self.body.yaw = snapshot.yaw;
        self.body.pitch = snapshot.pitch;
        self.body.hp = snapshot.hp;
        self.body.prev_buttons = record.frame.buttons;

        let mut offset = 1;
        while offset < self.len {
            let index = (acked_index + offset) % RING_CAPACITY;
            if let Some(mut later) = self.ring[index] {
                let previous = self.body.prev_buttons;
                player::apply(&mut self.body, &later.frame, previous, world, &self.cfg);
                later.state_after = self.body.state;
                later.yaw = self.body.yaw;
                later.pitch = self.body.pitch;
                self.ring[index] = Some(later);
            }
            offset += 1;
        }

        self.visual = rendered - self.body.state.pos;
        self.drop_through(snapshot.acked_seq);
        self.corrections += 1;
        Reconcile::Corrected { error }
    }

    /// Decay the visual residual by one frame. Returns the rendered position.
    pub fn decay_visual(&mut self, dt_seconds: f32) -> Vec3 {
        let length = self.visual.length();
        if length <= Fixed::from_f32(0.0001) {
            self.visual = Vec3::ZERO;
            return self.render_pos();
        }
        let factor = Fixed::from_f32((-dt_seconds / SMOOTHING_SECONDS).exp());
        let reduction = length * (Fixed::from_int(1) - factor);
        if reduction > MAX_VISUAL_STEP {
            let keep = Fixed::from_int(1) - MAX_VISUAL_STEP / length;
            self.visual = self.visual * keep;
        } else {
            self.visual = self.visual * factor;
        }
        self.render_pos()
    }

    /// The position the renderer should draw: authoritative state + residual.
    pub fn render_pos(&self) -> Vec3 {
        self.body.state.pos + self.visual
    }

    /// The body the predictor is currently simulating.
    pub fn body(&self) -> &PlayerBody {
        &self.body
    }

    /// The current visual residual.
    pub fn visual_offset(&self) -> Vec3 {
        self.visual
    }

    /// Inputs currently waiting for an acknowledgement.
    pub fn pending(&self) -> usize {
        self.len
    }

    /// Inputs evicted because the ring was full.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Corrections that reset and replayed.
    pub fn corrections(&self) -> u32 {
        self.corrections
    }

    /// Hard resyncs.
    pub fn resyncs(&self) -> u32 {
        self.resyncs
    }

    /// The predicted state after a sequence, if it is still in the ring.
    pub fn state_after(&self, seq: u32) -> Option<MoveState> {
        for offset in 0..self.len {
            let index = (self.head + offset) % RING_CAPACITY;
            if self.ring[index].is_some_and(|record| record.seq == seq) {
                return self.ring[index].map(|record| record.state_after);
            }
        }
        None
    }

    fn resync(&mut self, snapshot: Snapshot) -> Reconcile {
        let rendered = self.render_pos();
        self.body.state = snapshot.state;
        self.body.yaw = snapshot.yaw;
        self.body.pitch = snapshot.pitch;
        self.body.hp = snapshot.hp;
        self.body.prev_buttons = snapshot.buttons;
        // A gap means some inputs between the snapshot and the oldest record
        // were evicted. Replaying the survivors from this state would skip
        // those inputs, which is worse than accepting the server's position.
        self.ring = [None; RING_CAPACITY];
        self.head = 0;
        self.len = 0;
        self.visual = rendered - self.body.state.pos;
        self.resyncs += 1;
        Reconcile::Resynced
    }

    fn drop_through(&mut self, acked_seq: u32) {
        while self.len > 0 {
            let Some(record) = self.ring[self.head] else {
                break;
            };
            if record.seq > acked_seq {
                break;
            }
            self.ring[self.head] = None;
            self.head = (self.head + 1) % RING_CAPACITY;
            self.len -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flat;

    impl VoxelQuery for Flat {
        fn solid(&self, _x: i32, y: i32, _z: i32) -> bool {
            y < 0
        }
    }

    fn config() -> MoveConfig {
        MoveConfig::from_tuning(sim::tuning::embedded())
    }

    fn body() -> PlayerBody {
        PlayerBody::spawned(Vec3::from_ints(0, 0, 0))
    }

    fn frame(index: u32) -> InputFrame {
        InputFrame {
            tick: Tick(index),
            move_x: if index % 5 == 0 { -100 } else { 0 },
            move_z: if index % 3 == 0 { 0 } else { 100 },
            yaw: (index % 256) as u8,
            pitch: 0,
            buttons: if index % 47 == 0 { 1 } else { 0 },
        }
    }

    fn snapshot(body: &PlayerBody, acked_seq: u32) -> Snapshot {
        Snapshot {
            tick: Tick(acked_seq),
            acked_seq,
            state: body.state,
            yaw: body.yaw,
            pitch: body.pitch,
            buttons: body.prev_buttons,
            hp: body.hp,
        }
    }

    #[test]
    fn replay_matches_server() {
        let cfg = config();
        let mut server = body();
        let mut expected = body();
        let mut client = Predictor::new(body(), cfg);
        let mut queue: std::collections::VecDeque<(u32, InputFrame)> =
            std::collections::VecDeque::new();
        let mut corrected = 0;

        for index in 0..1_000u32 {
            let input = frame(index);
            let previous = expected.prev_buttons;
            player::apply(&mut expected, &input, previous, &Flat, &cfg);
            queue.push_back((index, input));

            // Drift the current prediction. The error belongs to `index`, so
            // it is not visible until a snapshot acknowledges that input; the
            // older snapshots below correctly leave the current state alone.
            client.body.state.pos +=
                Vec3::new(Fixed::from_f32(0.3), Fixed::from_int(0), Fixed::from_int(0));
            client.push(index, input, &Flat);

            // Six inputs are in flight: the snapshot acks `index - 6` and the
            // predictor has to replay the six later ones.
            if queue.len() > 6 {
                let (seq, old) = queue.pop_front().expect("one input is due");
                let previous = server.prev_buttons;
                player::apply(&mut server, &old, previous, &Flat, &cfg);
                let result = client.reconcile(snapshot(&server, seq), &Flat);
                if matches!(result, Reconcile::Corrected { .. }) {
                    corrected += 1;
                    assert!(
                        client.body.state.pos.distance(expected.state.pos) <= Fixed::from_f32(0.01),
                        "iteration {index}: client {:?} expected {:?}",
                        client.body.state.pos,
                        expected.state.pos
                    );
                }
                assert_eq!(client.pending(), 6);
            }
        }

        assert!(
            corrected >= 100,
            "only {corrected} corrections were exercised"
        );
        assert_eq!(client.pending(), 6);
        assert_eq!(client.dropped(), 0);

        while let Some((seq, old)) = queue.pop_front() {
            let previous = server.prev_buttons;
            player::apply(&mut server, &old, previous, &Flat, &cfg);
            client.reconcile(snapshot(&server, seq), &Flat);
        }
        assert!(
            client.body.state.pos.distance(server.state.pos) <= Fixed::from_f32(0.01),
            "client {:?} server {:?}",
            client.body.state.pos,
            server.state.pos
        );
        assert_eq!(client.pending(), 0);
    }

    #[test]
    fn corrections_under_five_centimetres_are_ignored() {
        let cfg = config();
        let mut server = body();
        let mut client = Predictor::new(body(), cfg);
        let input = frame(1);
        let previous = server.prev_buttons;
        player::apply(&mut server, &input, previous, &Flat, &cfg);

        client.body.state.pos += Vec3::new(
            Fixed::from_f32(0.03),
            Fixed::from_int(0),
            Fixed::from_int(0),
        );
        client.push(1, input, &Flat);
        assert_eq!(
            client.reconcile(snapshot(&server, 1), &Flat),
            Reconcile::Ignored
        );
        assert_eq!(client.corrections(), 0);

        let mut client = Predictor::new(body(), cfg);
        let mut server = body();
        let previous = server.prev_buttons;
        player::apply(&mut server, &input, previous, &Flat, &cfg);
        client.body.state.pos += Vec3::new(
            Fixed::from_f32(0.06),
            Fixed::from_int(0),
            Fixed::from_int(0),
        );
        client.push(1, input, &Flat);
        assert!(matches!(
            client.reconcile(snapshot(&server, 1), &Flat),
            Reconcile::Corrected { .. }
        ));
        assert_eq!(client.corrections(), 1);
    }

    #[test]
    fn no_visible_snap() {
        let cfg = config();
        let mut client = Predictor::new(body(), cfg);
        client.push(1, frame(1), &Flat);
        let before = client.render_pos();
        let mut server = client.body.state;
        server.pos += Vec3::new(Fixed::from_int(1), Fixed::from_int(0), Fixed::from_int(0));
        let result = client.reconcile(
            Snapshot {
                tick: Tick(1),
                acked_seq: 1,
                state: server,
                yaw: client.body.yaw,
                pitch: client.body.pitch,
                buttons: client.body.prev_buttons,
                hp: client.body.hp,
            },
            &Flat,
        );
        assert!(matches!(result, Reconcile::Corrected { .. }));
        assert!(
            client.render_pos().distance(before) <= Fixed::from_f32(0.001),
            "the correction must not move the rendered position on its first frame"
        );

        let mut previous = client.render_pos();
        let mut max_step = Fixed::from_int(0);
        for _ in 0..60 {
            let next = client.decay_visual(1.0 / 60.0);
            let step = next.distance(previous);
            if step > max_step {
                max_step = step;
            }
            previous = next;
        }
        assert!(
            max_step <= Fixed::from_f32(0.0501),
            "a frame moved {max_step:?}, over the 5 cm visibility bound"
        );
        assert!(client.visual_offset().length() <= Fixed::from_f32(0.01));
    }

    #[test]
    fn three_hundred_ms_rtt_keeps_control() {
        let cfg = config();
        let mut server = body();
        let mut client = Predictor::new(body(), cfg);
        let mut queue: std::collections::VecDeque<(u32, InputFrame)> =
            std::collections::VecDeque::new();

        for index in 0..1_000u32 {
            let input = frame(index);
            client.push(index, input, &Flat);
            queue.push_back((index, input));
            if queue.len() > 6 {
                let (seq, old) = queue.pop_front().expect("one input is due");
                let previous = server.prev_buttons;
                player::apply(&mut server, &old, previous, &Flat, &cfg);
                client.reconcile(snapshot(&server, seq), &Flat);
            }
        }
        assert_eq!(client.pending(), 6, "the six unacked inputs must survive");
        assert_eq!(client.dropped(), 0);
        while let Some((_seq, input)) = queue.pop_front() {
            let previous = server.prev_buttons;
            player::apply(&mut server, &input, previous, &Flat, &cfg);
        }
        assert!(
            client.render_pos().distance(server.state.pos) <= Fixed::from_f32(0.01),
            "client and server diverged after the 300 ms lag"
        );
    }

    #[test]
    fn the_ring_keeps_the_last_128_inputs() {
        let cfg = config();
        let mut client = Predictor::new(body(), cfg);
        for index in 0..200u32 {
            client.push(index, frame(index), &Flat);
        }
        assert_eq!(client.pending(), RING_CAPACITY);
        assert_eq!(client.dropped(), 72);
        assert!(client.state_after(72).is_some());
        assert!(client.state_after(71).is_none());
    }

    #[test]
    fn an_ack_outside_the_ring_hard_resyncs_without_replaying_a_gap() {
        let cfg = config();
        let mut client = Predictor::new(body(), cfg);
        for index in 0..200u32 {
            client.push(index, frame(index), &Flat);
        }
        let rendered = client.render_pos();
        let mut server = body();
        server.state.pos += Vec3::new(Fixed::from_int(1), Fixed::from_int(0), Fixed::from_int(0));

        assert_eq!(
            client.reconcile(snapshot(&server, 0), &Flat),
            Reconcile::Resynced
        );
        assert_eq!(client.pending(), 0, "the evicted history must be discarded");
        assert_eq!(client.resyncs(), 1);
        assert!(client.body.state.pos.distance(server.state.pos) <= Fixed::from_f32(0.001));
        assert!(
            client.render_pos().distance(rendered) <= Fixed::from_f32(0.001),
            "the residual still hides the resync from the camera"
        );
    }
}
