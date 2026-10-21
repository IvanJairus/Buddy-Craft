//! The per-player input queue (plan §6.2, T-020).
//!
//! A client sends **three consecutive frames per packet, 15 times a second**
//! against a 20 Hz simulation. Those numbers are chosen so that one lost packet
//! costs nothing: the next packet re-sends the frames that were in the lost one.
//! The queue's job is to turn that stream into exactly one [`InputFrame`] per
//! player per tick, which means answering three questions:
//!
//! * **Which frame applies to this tick?** The newest one whose tick is not in
//!   the future — not the newest received, because a packet can arrive early.
//! * **What if nothing arrived this tick?** Reuse the last frame. At 15 Hz there
//!   are ticks with no new packet, and holding the stick for one tick is what
//!   the player did.
//! * **What if nothing has arrived for 200 ms?** Hold *nothing*. §6.2 is explicit
//!   that a silent client is not "still holding forward" — otherwise a player who
//!   unplugs runs into a wall at full speed, and a laggy player gets free
//!   movement.
//!
//! # Why a fixed array
//!
//! Six players, twelve queued frames each, no allocation and no `HashMap`. The
//! whole queue is 6 × 12 × 8 bytes and iterating it is a linear scan of twelve
//! `Option`s. A `VecDeque` per player would be three allocations per room for no
//! measurable benefit, and this is a code path that runs 20 times a second
//! forever.

use protocol::WireInput;
use sim::{InputFrame, PlayerId, Tick};

/// How long a silent client may go before its input is treated as "no keys
/// held" — 200 ms (§6.2).
pub const INPUT_TIMEOUT_TICKS: u32 = 4;

/// Frames queued per player. A packet carries three and covers about 1.3 ticks,
/// so twelve is roughly nine packets of lookahead — far more than the ~200 ms a
/// client can legitimately run ahead.
pub const QUEUE_CAPACITY: usize = 12;

/// Players the queue has room for (§6.2).
pub const MAX_PLAYERS: usize = 6;

/// One frame waiting to be consumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Queued {
    tick: Tick,
    seq: u16,
    input: WireInput,
}

/// True when `a` is a newer sequence number than `b`.
///
/// Wrapping comparison: `seq` is a `u16` counter and a long session will wrap it.
/// The half-range rule is the standard one — two sequence numbers more than
/// 32,767 apart are assumed to be the far side of a wrap rather than a
/// 32,768-message reordering.
#[inline]
fn is_newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

/// One player's queue.
#[derive(Clone, Debug)]
struct PlayerQueue {
    entries: [Option<Queued>; QUEUE_CAPACITY],
    len: usize,
    /// The last frame resolved, held for ticks with no new packet.
    last: Option<WireInput>,
    /// The simulation tick the last frame was resolved at, for the timeout.
    resolved_at: Option<Tick>,
}

impl PlayerQueue {
    const fn new() -> PlayerQueue {
        PlayerQueue {
            entries: [None; QUEUE_CAPACITY],
            len: 0,
            last: None,
            resolved_at: None,
        }
    }

    /// Insert one frame, replacing a same-tick frame from an older packet.
    fn insert(&mut self, frame: Queued) {
        // Replace an entry for the same tick if this packet is newer.
        for slot in self.entries.iter_mut().flatten() {
            if slot.tick == frame.tick {
                if is_newer(frame.seq, slot.seq) {
                    *slot = frame;
                }
                return;
            }
        }
        if self.len == QUEUE_CAPACITY {
            // Full: drop the oldest queued frame rather than the new one. The
            // newest frame is the one the player most wants applied.
            let mut oldest = 0;
            let mut oldest_tick = Tick(u32::MAX);
            for (i, slot) in self.entries.iter().enumerate() {
                if let Some(e) = slot {
                    if e.tick < oldest_tick {
                        oldest_tick = e.tick;
                        oldest = i;
                    }
                }
            }
            self.entries[oldest] = Some(frame);
            return;
        }
        if let Some(slot) = self.entries.iter_mut().find(|s| s.is_none()) {
            *slot = Some(frame);
            self.len += 1;
        }
    }

    /// Drop every frame at or before `now`.
    fn discard_through(&mut self, now: Tick) {
        for slot in self.entries.iter_mut() {
            if slot.map(|e| e.tick <= now).unwrap_or(false) {
                *slot = None;
                self.len -= 1;
            }
        }
    }

    /// The newest frame whose tick is at or before `now`.
    fn newest_at_or_before(&self, now: Tick) -> Option<Queued> {
        let mut best: Option<Queued> = None;
        for entry in self.entries.iter().flatten() {
            if entry.tick <= now && best.map(|b| entry.tick > b.tick).unwrap_or(true) {
                best = Some(*entry);
            }
        }
        best
    }
}

/// The queue for every player in a room.
#[derive(Clone, Debug)]
pub struct InputQueue {
    slots: [PlayerQueue; MAX_PLAYERS],
}

impl Default for InputQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl InputQueue {
    /// An empty queue.
    pub const fn new() -> InputQueue {
        InputQueue {
            slots: [const { PlayerQueue::new() }; MAX_PLAYERS],
        }
    }

    fn slot(&mut self, player: PlayerId) -> Option<&mut PlayerQueue> {
        self.slots.get_mut(player.0 as usize)
    }

    /// Queue the frames from one `C_INPUT` packet.
    ///
    /// Frames for ticks the simulation has already passed are dropped: applying
    /// them would be rewinding, and a client that sends them is either late or
    /// lying. This is also what stops a resent packet from resurrecting input
    /// that has already been consumed.
    pub fn push(&mut self, player: PlayerId, packet: &protocol::Input) {
        let Some(queue) = self.slot(player) else {
            return;
        };
        for (i, input) in packet.frames.iter().enumerate() {
            let tick = packet.first_tick.advance(i as u32);
            if queue.resolved_at.map(|at| tick <= at).unwrap_or(false) {
                continue;
            }
            queue.insert(Queued {
                tick,
                seq: packet.seq,
                input: *input,
            });
        }
    }

    /// The frame to apply for `player` at `now`.
    ///
    /// Always returns a frame — a player who has never sent input is holding
    /// nothing, which is the same answer as one who stopped.
    pub fn resolve(&mut self, player: PlayerId, now: Tick) -> InputFrame {
        let Some(queue) = self.slot(player) else {
            return InputFrame {
                tick: now,
                ..InputFrame::default()
            };
        };

        if let Some(newest) = queue.newest_at_or_before(now) {
            queue.last = Some(newest.input);
            queue.resolved_at = Some(now);
            queue.discard_through(now);
            return newest.input.to_sim(now);
        }

        // Nothing new. Hold the last frame for up to the timeout, then let go.
        let expired = queue
            .resolved_at
            .map(|at| now.since(at) > INPUT_TIMEOUT_TICKS)
            .unwrap_or(true);
        if expired {
            queue.last = None;
            return InputFrame {
                tick: now,
                ..InputFrame::default()
            };
        }

        queue
            .last
            .map(|input| input.to_sim(now))
            .unwrap_or(InputFrame {
                tick: now,
                ..InputFrame::default()
            })
    }

    /// Frames currently queued for `player`, for tests and diagnostics.
    pub fn queued(&self, player: PlayerId) -> usize {
        self.slots
            .get(player.0 as usize)
            .map(|q| q.len)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::input::buttons;

    fn packet(seq: u16, first_tick: u32, buttons_held: u8) -> protocol::Input {
        protocol::Input {
            seq,
            acked_seq: 0,
            first_tick: Tick(first_tick),
            frames: [WireInput {
                axes: WireInput::pack_axes(0, 1),
                yaw: 0,
                pitch: 0,
                buttons: buttons_held,
            }; protocol::INPUT_FRAMES],
        }
    }

    fn p0() -> PlayerId {
        PlayerId(0)
    }

    #[test]
    fn the_newest_frame_at_or_before_now_is_the_one_applied() {
        let mut q = InputQueue::new();
        // A packet covering ticks 10, 11, 12.
        q.push(p0(), &packet(1, 10, buttons::JUMP));
        assert_eq!(q.queued(p0()), 3);

        // At tick 11, the frame for tick 11 applies — not the newest received.
        let frame = q.resolve(p0(), Tick(11));
        assert_eq!(frame.tick, Tick(11));
        assert!(frame.held(buttons::JUMP));
        // 10 and 11 are consumed, 12 is still ahead.
        assert_eq!(q.queued(p0()), 1);
    }

    #[test]
    fn stale_input_is_dropped() {
        let mut q = InputQueue::new();
        q.push(p0(), &packet(1, 10, buttons::JUMP));
        // Run the clock past the whole packet.
        q.resolve(p0(), Tick(12));
        assert_eq!(q.queued(p0()), 0);

        // A late packet for ticks that have already been simulated must not
        // rewind anything: the queue drops all three frames and the resolved
        // input is unchanged.
        let before = q.resolve(p0(), Tick(12));
        q.push(p0(), &packet(2, 9, buttons::FIRE));
        assert_eq!(q.queued(p0()), 0, "late frames must not queue");
        let after = q.resolve(p0(), Tick(12));
        assert_eq!(before, after);
        assert!(!after.held(buttons::FIRE), "stale input was applied");
    }

    #[test]
    fn an_early_packet_waits_its_turn() {
        let mut q = InputQueue::new();
        // A packet for future ticks: nothing applies yet.
        q.push(p0(), &packet(1, 100, buttons::FIRE));
        let now = q.resolve(p0(), Tick(50));
        assert!(!now.held(buttons::FIRE));
        assert_eq!(q.queued(p0()), 3, "future frames must stay queued");
        // Once the clock reaches them, they apply.
        assert!(q.resolve(p0(), Tick(100)).held(buttons::FIRE));
    }

    #[test]
    fn afk_player_stops_moving() {
        let mut q = InputQueue::new();
        // A packet covering ticks 10, 11 and 12.
        q.push(p0(), &packet(1, 10, buttons::JUMP));
        for tick in 10..=12 {
            assert!(
                q.resolve(p0(), Tick(tick)).held(buttons::JUMP),
                "tick {tick} is covered by the packet"
            );
        }

        // The client goes silent. For four ticks the last frame is held — at
        // 15 Hz there are legitimately ticks with no new packet, so letting go
        // immediately would make a 15 Hz client stutter at 20 Hz.
        for tick in 13..=16 {
            assert!(
                q.resolve(p0(), Tick(tick)).held(buttons::JUMP),
                "tick {tick} is within the 200 ms window"
            );
        }

        // On the fifth silent tick, the player is holding nothing.
        let frame = q.resolve(p0(), Tick(17));
        assert!(
            frame.is_idle(),
            "after 200 ms of silence the player must not still be moving: {frame:?}"
        );
        assert_eq!(INPUT_TIMEOUT_TICKS, 4);
    }

    #[test]
    fn input_resumes_immediately_after_a_gap() {
        let mut q = InputQueue::new();
        q.push(p0(), &packet(1, 10, buttons::JUMP));
        for tick in 10..=12 {
            q.resolve(p0(), Tick(tick));
        }
        // A long silence: the player is idle by tick 20.
        assert!(q.resolve(p0(), Tick(20)).is_idle());

        // A returning client should not have to wait for a grace period.
        q.push(p0(), &packet(2, 20, buttons::FIRE));
        assert!(
            q.resolve(p0(), Tick(20)).held(buttons::FIRE),
            "a returning client should not have to wait"
        );
    }

    #[test]
    fn a_resent_packet_does_not_duplicate_frames() {
        let mut q = InputQueue::new();
        q.push(p0(), &packet(1, 10, buttons::JUMP));
        q.push(p0(), &packet(1, 10, buttons::JUMP));
        assert_eq!(q.queued(p0()), 3, "the same packet twice is still 3 frames");
    }

    #[test]
    fn a_newer_packet_wins_the_overlap() {
        // Two packets overlap at tick 11: the newer one's frame must win.
        let mut q = InputQueue::new();
        q.push(p0(), &packet(1, 10, buttons::JUMP));
        q.push(p0(), &packet(2, 11, buttons::FIRE));
        let frame = q.resolve(p0(), Tick(11));
        assert!(frame.held(buttons::FIRE), "{frame:?}");
        assert!(!frame.held(buttons::JUMP));
    }

    #[test]
    fn an_older_packet_cannot_overwrite_a_newer_one() {
        // Out-of-order delivery: seq 2 arrives before seq 1 for the same tick.
        let mut q = InputQueue::new();
        q.push(p0(), &packet(2, 10, buttons::FIRE));
        q.push(p0(), &packet(1, 10, buttons::JUMP));
        let frame = q.resolve(p0(), Tick(10));
        assert!(frame.held(buttons::FIRE), "older packet won: {frame:?}");
    }

    #[test]
    fn a_wrapping_sequence_number_is_still_ordered() {
        // 65535 -> 0 is a step forward, not a 65,535-message step back.
        assert!(is_newer(0, 65_535));
        assert!(!is_newer(65_535, 0));
        assert!(is_newer(2, 1));
        assert!(!is_newer(1, 2));
        assert!(!is_newer(7, 7));

        let mut q = InputQueue::new();
        q.push(p0(), &packet(65_535, 10, buttons::JUMP));
        q.push(p0(), &packet(0, 10, buttons::FIRE));
        assert!(q.resolve(p0(), Tick(10)).held(buttons::FIRE));
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut q = InputQueue::new();
        // Sixty packets covering disjoint future ticks: far more than the queue
        // can hold. The newest frames must survive.
        for i in 0..60u32 {
            q.push(p0(), &packet(i as u16, 100 + i * 3, buttons::JUMP));
        }
        assert_eq!(q.queued(p0()), QUEUE_CAPACITY);
        // The newest packet survived the overflow: its first frame applies.
        let last = 100 + 59 * 3;
        assert!(q.resolve(p0(), Tick(last)).held(buttons::JUMP));
        // Resolving at that tick discards everything at or before it, leaving
        // only the two frames still ahead of the clock.
        assert_eq!(q.queued(p0()), 2);
    }

    #[test]
    fn players_do_not_share_a_queue() {
        let mut q = InputQueue::new();
        q.push(PlayerId(0), &packet(1, 10, buttons::JUMP));
        q.push(PlayerId(1), &packet(1, 10, buttons::FIRE));
        assert!(q.resolve(PlayerId(0), Tick(10)).held(buttons::JUMP));
        assert!(q.resolve(PlayerId(1), Tick(10)).held(buttons::FIRE));
    }

    #[test]
    fn an_unknown_player_is_ignored_rather_than_panicking() {
        let mut q = InputQueue::new();
        q.push(PlayerId(200), &packet(1, 10, buttons::JUMP));
        assert_eq!(q.queued(PlayerId(200)), 0);
        assert!(q.resolve(PlayerId(200), Tick(10)).is_idle());
    }

    #[test]
    fn a_player_who_never_sends_input_is_idle() {
        let mut q = InputQueue::new();
        for tick in 0..20 {
            let frame = q.resolve(p0(), Tick(tick));
            assert!(frame.is_idle());
            assert_eq!(frame.tick, Tick(tick), "the frame must be stamped");
        }
    }
}
