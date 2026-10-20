//! `room` — the room state machine, runtime-agnostic (plan §6, M2).
//!
//! The crate exists so that "which host do we use" is a configuration change
//! rather than an architectural one. Nothing here knows what a socket is, what a
//! wall clock is, or whether it is running inside a Cloudflare Durable Object or
//! a `tokio` task: the host forwards bytes and calls [`Room::tick`] once per
//! tick, and every decision comes back as a value.
//!
//! # Invariants
//!
//! * No I/O, no threads, no `async`, no `std::time`. Time is a [`sim::Tick`].
//!   `tests/no_clock.rs` fails the build if that stops being true.
//! * All game rules live in `sim`, which is compiled for both the server and the
//!   client. This crate decides *when* to run them, never *what* they do.
//! * Every state change is a function of (state, inputs). Two rooms ticked in the
//!   same process share nothing — there is no global, no cache, no thread-local.

pub mod input;
pub mod lifecycle;

pub use input::{InputQueue, INPUT_TIMEOUT_TICKS, QUEUE_CAPACITY};
pub use lifecycle::{Conditions, Effect, IllegalTransition, Lifecycle, Phase};

use sim::{
    ring_pad_origins, InputFrame, Layout, MatchState, Outcome, PlayerId, RoomSetup, SetupError,
    Tick, VoxelQuery,
};

/// What the host knows about the lobby that the room does not.
///
/// `match_over` is deliberately absent: whether the match is over is the *room's*
/// knowledge, read from [`MatchState::is_over`]. Letting the host supply it would
/// give two answers to one question, and the wrong one would win.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Roster {
    /// Connected humans. Bots do not count — a room of bots is not a room.
    pub players: u8,
    /// Whether every connected player is ready.
    pub all_ready: bool,
}

/// What one tick produced.
///
/// The host does not inspect the room's internals to decide what to send; it
/// looks at this.
#[derive(Clone, Debug, Default)]
pub struct TickOutput {
    /// The tick that was just simulated.
    pub tick: Tick,
    /// Set when this tick changed the lifecycle phase.
    pub effect: Option<Effect>,
    /// The frame each player's input resolved to.
    ///
    /// Resolved every tick so the queue drains, but only *applied* to the
    /// simulation while the room is [`Phase::Live`]. A host that is showing a
    /// "waiting for players" screen can ignore this.
    pub inputs: Vec<(PlayerId, InputFrame)>,
    /// The match outcome, once the match is running.
    pub outcome: Option<Outcome>,
}

/// A room: one match, its lifecycle and its input queues.
///
/// A `Room` owns everything it needs and nothing it does not. There is no
/// reference to a transport, a clock, a world or a database.
#[derive(Clone, Debug)]
pub struct Room {
    now: Tick,
    lifecycle: Lifecycle,
    state: MatchState,
    queue: InputQueue,
}

impl Room {
    /// Build a room with the simulation already seated.
    ///
    /// The match state exists from the start, in the lobby, because building it
    /// later would make `tick` fallible — and a tick loop that can fail is one
    /// the host will get wrong. Nothing is simulated until the lifecycle reaches
    /// [`Phase::Live`].
    pub fn new(now: Tick, setup: RoomSetup, pad_origins: &[[i32; 3]]) -> Result<Room, SetupError> {
        Ok(Room {
            now,
            lifecycle: Lifecycle::new(now),
            state: MatchState::new(setup, pad_origins)?,
            queue: InputQueue::new(),
        })
    }

    /// Build a room whose bases are laid out on a ring.
    ///
    /// Convenience for tests and for hosts that have no world yet. Worldgen owns
    /// the real base sites (§4.4); this is the deterministic stand-in.
    pub fn on_ring(
        now: Tick,
        setup: RoomSetup,
        radius: i32,
        ground_y: i32,
    ) -> Result<Room, SetupError> {
        // `MatchState::new` resolves the layout again from the same inputs, so
        // the origins handed in here are guaranteed to match the seats.
        let layout = Layout::resolve(setup.mode, setup.players(), setup.teams).ok_or(
            SetupError::NotALegalShape {
                mode: setup.mode,
                players: setup.players(),
                teams: setup.teams.unwrap_or(0),
            },
        )?;
        let origins = ring_pad_origins(&layout, radius, ground_y);
        Room::new(now, setup, &origins)
    }

    /// The tick the room is at.
    pub fn now(&self) -> Tick {
        self.now
    }

    /// The current lifecycle phase.
    pub fn phase(&self) -> Phase {
        self.lifecycle.phase()
    }

    /// The lifecycle machine, for phase timers.
    pub fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    /// The simulation.
    pub fn state(&self) -> &MatchState {
        &self.state
    }

    /// The input queues.
    pub fn queue(&self) -> &InputQueue {
        &self.queue
    }

    /// Queue one `C_INPUT` packet.
    ///
    /// Returns false when the packet came from a player the room does not have,
    /// so the host can close the connection instead of accumulating frames for
    /// nobody.
    pub fn queue_input(&mut self, player: PlayerId, packet: &protocol::Input) -> bool {
        if !self.state.players().iter().any(|slot| slot.id == player) {
            return false;
        }
        self.queue.push(player, packet);
        true
    }

    /// Advance exactly one tick — 50 ms of simulated time (§6.2).
    ///
    /// Pure in the sense that matters: the result is a function of the room's
    /// state, the packets queued since the last call, and the world. No clock is
    /// read, so a test can run 20,000 ticks in milliseconds and get the same
    /// answer as a server that took 17 minutes to do it.
    ///
    /// # Why the world is a parameter
    ///
    /// The room does not own the terrain. It is generated from a seed on the
    /// client and stored as diffs on the server (§6.7), and either way it is far
    /// too large to copy into a room. Passing `&dyn VoxelQuery` keeps the room
    /// agnostic about where blocks live while still letting movement collide
    /// against them. `dyn` rather than `impl` because a room is long-lived and
    /// its tick loop should not be monomorphised once per world type.
    pub fn tick(&mut self, roster: Roster, world: &dyn VoxelQuery) -> TickOutput {
        self.now = self.now.next();

        let effect = self.lifecycle.update(
            self.now,
            Conditions {
                players: roster.players,
                all_ready: roster.all_ready,
                match_over: self.state.is_over(),
            },
        );

        // The tick the match starts is the only one where spawn points can be
        // resolved against real terrain: `MatchState::new` has no world, so it
        // produces nominal points and leaves the collision check to here.
        if effect == Some(Effect::BeginMatch) {
            self.state.place_spawns(world);
        }

        // Resolve input every tick, in every phase. In the lobby this only
        // drains the queue, but skipping it there would let a client that held
        // the stick during the countdown keep a backlog of frames to replay the
        // instant the match starts.
        let mut inputs = Vec::with_capacity(self.state.players().len());
        for slot in self.state.players() {
            inputs.push((slot.id, self.queue.resolve(slot.id, self.now)));
        }

        // The simulation only runs while live. On the tick the countdown expires
        // the phase has already changed, so the match's first step happens on the
        // same tick the host emits `S_START` — one tick, not two.
        let outcome = match self.lifecycle.phase() {
            Phase::Live => {
                // Respawn before input, so a player whose 8 s expires on this
                // tick can act on this tick. The other order costs one tick of
                // dead time per death and is invisible in a test but not in a
                // fight.
                self.state.respawn_due(self.now, world);
                for (id, frame) in &inputs {
                    // A down or eliminated player is refused, which is not an
                    // error: they are still connected and still sending.
                    let _ = self.state.apply_input(*id, frame, world);
                }
                Some(self.state.advance())
            }
            // The match just ended on this tick, and the host needs the outcome
            // *now* to build `S_END` — reporting it only from the next tick
            // would mean a host that stops ticking on `EndMatch` never sees it.
            Phase::Ended => Some(self.state.outcome()),
            _ => None,
        };

        TickOutput {
            tick: self.now,
            effect,
            inputs,
            outcome,
        }
    }

    /// Mutable access to the simulation.
    ///
    /// Exists for the systems that sit on top of the tick loop — the edit
    /// pipeline, bots, damage from combat — which need to write to the match
    /// state between ticks. It is deliberately not `pub` in the sense of "anyone
    /// can restructure the match": every caller is inside this crate or is the
    /// host applying a validated message.
    pub fn state_mut(&mut self) -> &mut MatchState {
        &mut self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::WireInput;
    use sim::{input::buttons, Mode, TeamId};

    fn duel() -> RoomSetup {
        RoomSetup::humans(Mode::Ffa, None, 2)
    }

    fn room() -> Room {
        Room::on_ring(Tick(0), duel(), 20, 40).expect("room")
    }

    /// The floor the room's pads stand on: `ring` at radius 20 and height 40, so
    /// the walking surface is `y = 40` and the block under it is `y = 39`.
    fn ground() -> sim::world_query::VoxelGrid {
        let mut w = sim::world_query::VoxelGrid::new([-48, 36, -48], [96, 8, 96]);
        w.fill_layer(39, true);
        w
    }

    fn ready() -> Roster {
        Roster {
            players: 2,
            all_ready: true,
        }
    }

    fn idle() -> Roster {
        Roster::default()
    }

    fn packet(seq: u16, first_tick: u32, held: u8) -> protocol::Input {
        protocol::Input {
            seq,
            first_tick: Tick(first_tick),
            frames: [WireInput {
                axes: WireInput::pack_axes(0, 1),
                yaw: 0,
                pitch: 0,
                buttons: held,
            }; protocol::INPUT_FRAMES],
        }
    }

    /// Tick until the room is live, returning the tick the match started on.
    fn start(room: &mut Room, world: &dyn VoxelQuery) -> Tick {
        for _ in 0..200 {
            if room.tick(ready(), world).effect == Some(Effect::BeginMatch) {
                return room.now();
            }
        }
        panic!("room never went live");
    }

    #[test]
    fn a_tick_advances_exactly_one_tick() {
        let world = ground();
        let mut room = room();
        assert_eq!(room.now(), Tick(0));
        let out = room.tick(idle(), &world);
        assert_eq!(out.tick, Tick(1));
        assert_eq!(room.now(), Tick(1));
        assert_eq!(sim::time::TICK_MS, 50);
    }

    #[test]
    fn the_match_clock_does_not_run_in_the_lobby() {
        let world = ground();
        let mut room = room();
        for _ in 0..100 {
            room.tick(idle(), &world);
        }
        assert_eq!(room.phase(), Phase::Lobby);
        assert_eq!(
            room.state().elapsed(),
            Tick(0),
            "the simulation must not advance before the match starts"
        );
    }

    #[test]
    fn the_match_starts_on_the_tick_the_countdown_expires() {
        let world = ground();
        let mut room = room();
        let started = start(&mut room, &world);
        assert_eq!(room.phase(), Phase::Live);
        // One step has already happened on that tick, so the match clock is at 1.
        assert_eq!(room.state().elapsed(), Tick(1));
        assert_eq!(started, Tick(lifecycle::COUNTDOWN_TICKS + 1));
    }

    #[test]
    fn input_is_resolved_but_not_applied_before_the_match() {
        let world = ground();
        let mut room = room();
        let spawn = room.state().body(PlayerId(0)).expect("body").state.pos;
        room.queue_input(PlayerId(0), &packet(1, 1, buttons::JUMP));
        let out = room.tick(idle(), &world);
        assert_eq!(out.inputs.len(), 2, "every player resolves every tick");
        let (_, frame) = out.inputs[0];
        assert!(
            frame.held(buttons::JUMP),
            "the queue still resolves in the lobby"
        );
        assert!(out.outcome.is_none(), "nothing is simulated yet");
        assert_eq!(
            room.state().body(PlayerId(0)).expect("body").state.pos,
            spawn,
            "a lobby frame must not move anybody"
        );
    }

    #[test]
    fn afk_player_stops_moving_in_the_room() {
        let world = ground();
        let mut room = room();
        start(&mut room, &world);
        let queued_at = room.now().0;
        room.queue_input(PlayerId(0), &packet(1, queued_at, buttons::JUMP));

        // The packet covers three ticks, but the first is the tick that has
        // already been simulated, so the queue drops it: two frames are ahead of
        // the clock. After those, four ticks of grace — 200 ms — during which
        // the last frame is held, because at 15 Hz there are legitimately ticks
        // with no new packet. Then the player lets go.
        let mut held = 0u32;
        let mut stopped_after = None;
        for i in 0..20 {
            if room.tick(ready(), &world).inputs[0].1.held(buttons::JUMP) {
                held += 1;
            } else {
                stopped_after = Some(i);
                break;
            }
        }

        assert!(
            stopped_after.is_some(),
            "the player never stopped: {held} ticks of held input"
        );
        assert_eq!(
            held,
            2 + INPUT_TIMEOUT_TICKS,
            "two frames ahead of the clock plus a four-tick grace window"
        );
        // The property the AC is about, stated without the off-by-one: the
        // player stops within one packet's worth of ticks plus 200 ms.
        assert!(held <= protocol::INPUT_FRAMES as u32 + INPUT_TIMEOUT_TICKS);
        assert!(
            held >= INPUT_TIMEOUT_TICKS,
            "the grace window was cut short"
        );
    }

    #[test]
    fn an_input_packet_from_an_unknown_player_is_refused() {
        let mut room = room();
        assert!(room.queue_input(PlayerId(0), &packet(1, 0, 0)));
        assert!(
            !room.queue_input(PlayerId(5), &packet(1, 0, 0)),
            "a two-player room has no player 5"
        );
    }

    #[test]
    fn the_outcome_appears_when_the_match_ends() {
        let world = ground();
        let mut room = room();
        start(&mut room, &world);
        // Kill every core. `damage_core` takes raw damage and applies the 0.35
        // multiplier, so this is deliberately far past 10,000.
        for team in 0..2u8 {
            room.state_mut().damage_core(TeamId(team), 100_000);
        }
        let out = room.tick(ready(), &world);
        assert_eq!(out.effect, Some(Effect::EndMatch));
        assert!(out.outcome.is_some_and(|o| o.is_over()));
        assert_eq!(room.phase(), Phase::Ended);
    }

    #[test]
    fn two_rooms_in_one_process_do_not_share_state() {
        // The reentrancy property. If anything in the room were a global — a
        // static counter, a lazy cache — the two rooms would contaminate each
        // other, and the contamination would depend on tick order.
        let world = ground();
        let mut a = room();
        let mut b = room();
        start(&mut a, &world);
        start(&mut b, &world);

        // Give each room a different history, using nothing but input: `a`'s
        // player 0 walks forward, `b`'s stands still. This is the divergence the
        // body layer made possible — before it, `MatchState::advance` could not
        // move anybody and a test like this had to damage a core to get two
        // different hashes.
        let now = a.now().0;
        a.queue_input(PlayerId(0), &packet(1, now, buttons::JUMP));

        // Tick them alternately, so any shared state would be written by one and
        // read by the other.
        for _ in 0..40 {
            a.tick(ready(), &world);
            let out_b = b.tick(ready(), &world);
            assert!(
                !out_b.inputs[0].1.held(buttons::JUMP),
                "room b saw room a's input"
            );
        }
        assert_ne!(
            a.state().body(PlayerId(0)).unwrap().state.pos,
            b.state().body(PlayerId(0)).unwrap().state.pos,
            "input must actually move the body"
        );
        assert_ne!(
            a.state().state_hash(),
            b.state().state_hash(),
            "rooms with different input must diverge"
        );

        // And a third room ticked alone matches `b` exactly, so `b`'s result did
        // not depend on `a` existing.
        let mut c = room();
        start(&mut c, &world);
        for _ in 0..40 {
            c.tick(ready(), &world);
        }
        assert_eq!(b.state().state_hash(), c.state().state_hash());
    }

    #[test]
    fn a_tick_is_reproducible_from_the_same_inputs() {
        let world = ground();
        let run = || {
            let mut room = room();
            start(&mut room, &world);
            let now = room.now().0;
            for (i, player) in [PlayerId(0), PlayerId(1)].into_iter().enumerate() {
                room.queue_input(player, &packet(i as u16, now, buttons::FIRE));
            }
            for _ in 0..50 {
                room.tick(ready(), &world);
            }
            room.state().state_hash()
        };
        assert_eq!(run(), run(), "the same inputs must produce the same room");
    }

    #[test]
    fn an_empty_room_cleans_up_and_stops_ticking() {
        let world = ground();
        let mut room = room();
        let mut cleanups = 0;
        for _ in 0..(lifecycle::EMPTY_TICKS * 2) {
            if room.tick(idle(), &world).effect == Some(Effect::Cleanup) {
                cleanups += 1;
            }
        }
        assert_eq!(cleanups, 1);
        assert_eq!(room.phase(), Phase::Cleanup);
    }
}
