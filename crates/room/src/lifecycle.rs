//! The room's lifecycle state machine (plan §6.10, T-019).
//!
//! ```text
//!         create
//!           │
//!           ▼
//!       ┌────────┐  all ready, ≥2 players   ┌───────────┐
//!       │ LOBBY  │ ───────────────────────► │ COUNTDOWN │ 3 s
//!       └────────┘ ◄─────────────────────── └───────────┘
//!           │        someone unreadies           │
//!           │                                     ▼
//!           │                              ┌───────────┐
//!           │                              │   LIVE    │ 20 Hz tick
//!           │                              └───────────┘
//!           │                                     │ core HP ≤ 0, or overtime end
//!           │                                     ▼
//!           │                              ┌───────────┐
//!           │                              │   ENDED   │ 10 s results
//!           │                              └───────────┘
//!           │                                     │
//!           └──────────── empty 60 s ─────────────┴──► CLEANUP (persist, delete storage)
//! ```
//!
//! # The host makes zero decisions
//!
//! This module has no I/O, no `async`, no clock and no knowledge of sockets. The
//! host adapter forwards bytes, schedules ticks and calls [`Lifecycle::update`]
//! once per tick; everything the host is allowed to *do* comes back as an
//! [`Effect`]. That is what makes the same state machine run inside a Cloudflare
//! Durable Object and inside a `tokio` task, and it is why the whole thing is
//! testable in microseconds — `tests/no_clock.rs` fails the build if `std::time`
//! or `async` appears in this crate.
//!
//! # Timers are tick counts, not durations
//!
//! [`COUNTDOWN_TICKS`] is 60 because 3 s at 20 Hz is 60 ticks. Storing seconds
//! and multiplying later would be a second place for the tick rate to live, and
//! the room's tick rate is not negotiable (§6.2).

use sim::time::TICK_HZ;
use sim::Tick;

/// The countdown before a match starts: 3 s (§6.10).
pub const COUNTDOWN_TICKS: u32 = 3 * TICK_HZ;

/// How long the results screen stays up before the room is cleaned up: 10 s.
pub const ENDED_TICKS: u32 = 10 * TICK_HZ;

/// How long a room with nobody in it survives before cleanup: 60 s.
pub const EMPTY_TICKS: u32 = 60 * TICK_HZ;

/// Fewest players a match can start with (§6.10: "all ready, ≥2 players").
pub const MIN_PLAYERS: u8 = 2;

/// The results window is shorter than the empty-room window, which is why
/// §6.10's "empty 60 s" edge out of [`Phase::Ended`] can never fire. Checked at
/// compile time rather than in a test: if a future change inverts these, the
/// diagram in §6.10 needs revisiting, not a failing assertion.
const _: () = assert!(ENDED_TICKS < EMPTY_TICKS);

/// Where a room is in its life.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Waiting for players to arrive and ready up.
    Lobby,
    /// Everyone is ready; the clock is running and anyone can still unready.
    Countdown,
    /// The match is being simulated at 20 Hz.
    Live,
    /// The match is over and the results are on screen.
    Ended,
    /// Persisted and finished. Terminal.
    Cleanup,
}

impl Phase {
    /// Every phase, for the transition matrix test.
    pub const ALL: [Phase; 5] = [
        Phase::Lobby,
        Phase::Countdown,
        Phase::Live,
        Phase::Ended,
        Phase::Cleanup,
    ];

    /// The phases this one may move to (§6.10).
    ///
    /// Written as an explicit list rather than as conditions, because the *set*
    /// is the specification. `Countdown → Ended` is not a shorter path to the
    /// same place: a countdown that never went live has no match to end, and a
    /// state machine that allows it would let a host "end" a match that does not
    /// exist.
    pub const fn can_enter(self, to: Phase) -> bool {
        matches!(
            (self, to),
            (Phase::Lobby, Phase::Countdown)
                | (Phase::Lobby, Phase::Cleanup)
                | (Phase::Countdown, Phase::Lobby)
                | (Phase::Countdown, Phase::Live)
                | (Phase::Live, Phase::Ended)
                | (Phase::Ended, Phase::Cleanup)
        )
    }

    /// The phase's name, for logs and error messages.
    pub const fn name(self) -> &'static str {
        match self {
            Phase::Lobby => "lobby",
            Phase::Countdown => "countdown",
            Phase::Live => "live",
            Phase::Ended => "ended",
            Phase::Cleanup => "cleanup",
        }
    }

    /// True once the room is finished and nothing more can happen.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Phase::Cleanup)
    }
}

/// What the host should do after an [`Lifecycle::update`].
///
/// One effect per transition, and every effect is a *thing the host does* rather
/// than a thing the room did. `BeginMatch` means "start stepping the simulation";
/// it does not mean the simulation has been started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// The countdown started. The host should show it.
    BeginCountdown,
    /// The countdown was cancelled. The host should stop showing it.
    CancelCountdown,
    /// The match starts this tick. The host should emit `S_START` and begin
    /// stepping `MatchState`.
    BeginMatch,
    /// The match is over. The host should emit `S_END`.
    EndMatch,
    /// Persist the room and release its storage. Emitted **exactly once**.
    Cleanup,
}

/// A transition the state machine refuses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IllegalTransition {
    /// Where the room was.
    pub from: Phase,
    /// Where the caller asked it to go.
    pub to: Phase,
}

impl core::fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} -> {} is not a §6.10 transition",
            self.from.name(),
            self.to.name()
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The room's observable state, supplied by the host each tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Conditions {
    /// How many players are connected. Bots do not count: a room of bots is not
    /// a room (§4.8 — the match does not outlive the last human).
    pub players: u8,
    /// Whether every connected player is ready.
    pub all_ready: bool,
    /// Whether the match rules report an outcome.
    pub match_over: bool,
}

/// The lifecycle state machine.
#[derive(Clone, Debug)]
pub struct Lifecycle {
    phase: Phase,
    entered: Tick,
    /// When the room became empty, if it is.
    empty_since: Option<Tick>,
}

impl Lifecycle {
    /// A fresh room in [`Phase::Lobby`].
    pub fn new(now: Tick) -> Lifecycle {
        Lifecycle {
            phase: Phase::Lobby,
            entered: now,
            empty_since: None,
        }
    }

    /// A room resuming in `phase` at `now`.
    ///
    /// Used by a host that restarted and rehydrated its state from storage. The
    /// timers restart from `now` rather than resuming, which is deliberate: a
    /// Durable Object that was evicted mid-countdown should show a fresh three
    /// seconds, not a countdown that jumps straight to live because the wall
    /// clock moved while nothing was simulating.
    pub fn at(phase: Phase, now: Tick) -> Lifecycle {
        Lifecycle {
            phase,
            entered: now,
            empty_since: None,
        }
    }

    /// The current phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Ticks spent in the current phase.
    pub fn elapsed(&self, now: Tick) -> u32 {
        now.since(self.entered)
    }

    /// How many ticks are left in the current phase's timer, if it has one.
    pub fn remaining(&self, now: Tick) -> Option<u32> {
        let total = match self.phase {
            Phase::Countdown => COUNTDOWN_TICKS,
            Phase::Ended => ENDED_TICKS,
            Phase::Lobby => {
                // The lobby's timer only runs while the room is empty.
                return self
                    .empty_since
                    .map(|since| EMPTY_TICKS.saturating_sub(now.since(since)));
            }
            Phase::Live | Phase::Cleanup => return None,
        };
        Some(total.saturating_sub(self.elapsed(now)))
    }

    /// Move to `to`, or refuse.
    ///
    /// The single place a phase changes, so "is this transition legal" has one
    /// answer. A caller that assigns `phase` directly does not compile, because
    /// the field is private.
    pub fn transition(&mut self, to: Phase, now: Tick) -> Result<Effect, IllegalTransition> {
        if !self.phase.can_enter(to) {
            return Err(IllegalTransition {
                from: self.phase,
                to,
            });
        }
        let effect = match (self.phase, to) {
            (Phase::Lobby, Phase::Countdown) => Effect::BeginCountdown,
            (Phase::Countdown, Phase::Lobby) => Effect::CancelCountdown,
            (Phase::Countdown, Phase::Live) => Effect::BeginMatch,
            (Phase::Live, Phase::Ended) => Effect::EndMatch,
            (_, Phase::Cleanup) => Effect::Cleanup,
            // Unreachable: `can_enter` is the same table. Written as a panic so
            // that adding an edge to `can_enter` without adding it here fails
            // loudly in the transition matrix test rather than silently
            // returning the wrong effect.
            (from, to) => panic!(
                "can_enter allows {} -> {} but transition has no effect for it",
                from.name(),
                to.name()
            ),
        };
        self.phase = to;
        self.entered = now;
        Ok(effect)
    }

    /// Advance the machine one tick.
    ///
    /// Returns the effect if this tick caused a transition. Timers, readiness and
    /// emptiness are all evaluated here, in one place, so the host never has to
    /// decide whether "everyone is ready" means "start" — that is a rule, and
    /// rules live in the room.
    pub fn update(&mut self, now: Tick, c: Conditions) -> Option<Effect> {
        // Track emptiness for the phases where an empty room is cleaned up.
        if c.players == 0 {
            self.empty_since.get_or_insert(now);
        } else {
            self.empty_since = None;
        }

        match self.phase {
            Phase::Lobby => {
                if let Some(since) = self.empty_since {
                    if now.since(since) >= EMPTY_TICKS {
                        return self.transition(Phase::Cleanup, now).ok();
                    }
                }
                if c.players >= MIN_PLAYERS && c.all_ready {
                    return self.transition(Phase::Countdown, now).ok();
                }
                None
            }
            Phase::Countdown => {
                // Un-readying during the countdown is a first-class transition,
                // not a "cancel the timer" special case: the lobby is where a
                // player who changed their mind belongs, and it is reachable
                // right up to the last tick.
                if c.players < MIN_PLAYERS || !c.all_ready {
                    return self.transition(Phase::Lobby, now).ok();
                }
                if self.elapsed(now) >= COUNTDOWN_TICKS {
                    return self.transition(Phase::Live, now).ok();
                }
                None
            }
            Phase::Live => {
                if c.match_over {
                    return self.transition(Phase::Ended, now).ok();
                }
                None
            }
            Phase::Ended => {
                if self.elapsed(now) >= ENDED_TICKS {
                    return self.transition(Phase::Cleanup, now).ok();
                }
                None
            }
            // Terminal. Returning `None` forever is what makes `Cleanup` fire
            // exactly once.
            Phase::Cleanup => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(n: u32) -> Tick {
        Tick(n)
    }

    /// Run `update` for `n` ticks with fixed conditions, collecting effects.
    fn run(lc: &mut Lifecycle, from: u32, n: u32, c: Conditions) -> Vec<(u32, Effect)> {
        let mut out = Vec::new();
        for i in 0..n {
            let now = tick(from + i);
            if let Some(effect) = lc.update(now, c) {
                out.push((from + i, effect));
            }
        }
        out
    }

    fn ready(players: u8) -> Conditions {
        Conditions {
            players,
            all_ready: true,
            match_over: false,
        }
    }

    // -- the transition matrix ----------------------------------------------

    #[test]
    fn illegal_transitions_are_rejected() {
        // Every ordered pair, including self-transitions. The only ones that may
        // succeed are the six edges in §6.10.
        let mut allowed = Vec::new();
        for from in Phase::ALL {
            for to in Phase::ALL {
                let mut lc = Lifecycle::at(from, tick(0));
                let result = lc.transition(to, tick(1));
                if result.is_ok() {
                    allowed.push((from, to));
                    assert_eq!(lc.phase(), to, "{from:?} -> {to:?} should have moved");
                } else {
                    assert_eq!(
                        lc.phase(),
                        from,
                        "a refused transition must not move the room"
                    );
                    let err = result.unwrap_err();
                    assert_eq!(err.from, from);
                    assert_eq!(err.to, to);
                }
            }
        }
        assert_eq!(
            allowed.len(),
            6,
            "expected six legal edges, got {allowed:?}"
        );
    }

    #[test]
    fn the_allowed_edges_are_exactly_the_six_in_the_diagram() {
        // Spelled out separately from `can_enter` so that adding an edge to the
        // implementation without deciding to is a test failure rather than a
        // quiet behaviour change.
        let expected = [
            (Phase::Lobby, Phase::Countdown),
            (Phase::Lobby, Phase::Cleanup),
            (Phase::Countdown, Phase::Lobby),
            (Phase::Countdown, Phase::Live),
            (Phase::Live, Phase::Ended),
            (Phase::Ended, Phase::Cleanup),
        ];
        for from in Phase::ALL {
            for to in Phase::ALL {
                let want = expected.contains(&(from, to));
                assert_eq!(
                    from.can_enter(to),
                    want,
                    "{:?} -> {:?} should be {}",
                    from,
                    to,
                    if want { "legal" } else { "illegal" }
                );
            }
        }
        // No self-loops: a phase that "transitions" to itself resets its timer,
        // which would make the countdown and the results screen unendable.
        for phase in Phase::ALL {
            assert!(!phase.can_enter(phase), "{phase:?} has a self-loop");
        }
        // Cleanup is terminal.
        for to in Phase::ALL {
            assert!(!Phase::Cleanup.can_enter(to), "Cleanup -> {to:?} exists");
        }
    }

    #[test]
    fn every_legal_edge_has_an_effect_and_no_illegal_one_does() {
        // Guards the `panic!` arm in `transition`: if an edge is added to
        // `can_enter` and not to the effect match, this walks into it.
        for from in Phase::ALL {
            for to in Phase::ALL {
                if !from.can_enter(to) {
                    continue;
                }
                let mut lc = Lifecycle::at(from, tick(0));
                assert!(
                    lc.transition(to, tick(1)).is_ok(),
                    "{from:?} -> {to:?} panicked or failed"
                );
            }
        }
    }

    // -- the happy path -----------------------------------------------------

    #[test]
    fn lobby_needs_two_players_and_unanimous_readiness() {
        let mut lc = Lifecycle::new(tick(0));

        // One player, ready: not enough.
        assert_eq!(run(&mut lc, 0, 10, ready(1)), vec![]);
        assert_eq!(lc.phase(), Phase::Lobby);

        // Two players, not all ready: not enough.
        let mixed = Conditions {
            players: 2,
            all_ready: false,
            match_over: false,
        };
        assert_eq!(run(&mut lc, 10, 10, mixed), vec![]);
        assert_eq!(lc.phase(), Phase::Lobby);

        // Two players, all ready: countdown.
        assert_eq!(
            run(&mut lc, 20, 1, ready(2)),
            vec![(20, Effect::BeginCountdown)]
        );
        assert_eq!(lc.phase(), Phase::Countdown);
    }

    #[test]
    fn countdown_runs_three_seconds_then_goes_live() {
        let mut lc = Lifecycle::at(Phase::Countdown, tick(100));
        // The countdown starts at tick 100, occupies ticks 100..159, and the
        // match begins at 160 — exactly three seconds later.
        let effects = run(&mut lc, 100, COUNTDOWN_TICKS + 1, ready(4));
        assert_eq!(effects, vec![(100 + COUNTDOWN_TICKS, Effect::BeginMatch)]);
        assert_eq!(lc.phase(), Phase::Live);
        assert_eq!(COUNTDOWN_TICKS, 60);
    }

    #[test]
    fn unreadying_during_the_countdown_returns_to_the_lobby() {
        // The AC calls this out, and it is the transition most likely to be
        // missed: the countdown is a commitment the player can still withdraw
        // from, right up to the last tick.
        let mut lc = Lifecycle::at(Phase::Countdown, tick(0));
        run(&mut lc, 0, 30, ready(3));
        assert_eq!(lc.phase(), Phase::Countdown);

        let one_unready = Conditions {
            players: 3,
            all_ready: false,
            match_over: false,
        };
        assert_eq!(
            run(&mut lc, 30, 1, one_unready),
            vec![(30, Effect::CancelCountdown)]
        );
        assert_eq!(lc.phase(), Phase::Lobby);
    }

    #[test]
    fn a_player_leaving_during_the_countdown_also_returns_to_the_lobby() {
        // Dropping below the minimum is the same rule as unreadying: there is
        // nothing left to start.
        let mut lc = Lifecycle::at(Phase::Countdown, tick(0));
        let alone = Conditions {
            players: 1,
            all_ready: true,
            match_over: false,
        };
        assert_eq!(
            run(&mut lc, 0, 1, alone),
            vec![(0, Effect::CancelCountdown)]
        );
        assert_eq!(lc.phase(), Phase::Lobby);
    }

    #[test]
    fn an_aborted_countdown_starts_over_from_zero() {
        let mut lc = Lifecycle::at(Phase::Countdown, tick(0));
        run(&mut lc, 0, 59, ready(2));
        // Abort one tick before the match would have started.
        let unready = Conditions {
            players: 2,
            all_ready: false,
            match_over: false,
        };
        lc.update(tick(59), unready);
        assert_eq!(lc.phase(), Phase::Lobby);

        // Re-ready: a fresh 60 ticks, not the one that was left.
        assert_eq!(
            run(&mut lc, 60, 1, ready(2)),
            vec![(60, Effect::BeginCountdown)]
        );
        assert_eq!(lc.remaining(tick(60)), Some(COUNTDOWN_TICKS));
    }

    #[test]
    fn the_match_ends_and_the_results_hold_for_ten_seconds() {
        let mut lc = Lifecycle::at(Phase::Live, tick(0));
        run(&mut lc, 0, 100, ready(4));
        assert_eq!(lc.phase(), Phase::Live);

        let over = Conditions {
            players: 4,
            all_ready: true,
            match_over: true,
        };
        assert_eq!(run(&mut lc, 100, 1, over), vec![(100, Effect::EndMatch)]);
        assert_eq!(lc.phase(), Phase::Ended);

        let effects = run(&mut lc, 101, ENDED_TICKS, over);
        assert_eq!(
            effects,
            vec![(101 + ENDED_TICKS - 1, Effect::Cleanup)],
            "the results screen should last exactly ENDED_TICKS"
        );
        assert_eq!(lc.phase(), Phase::Cleanup);
    }

    // -- cleanup ------------------------------------------------------------

    #[test]
    fn an_empty_lobby_cleans_up_after_sixty_seconds() {
        let mut lc = Lifecycle::new(tick(0));
        let empty = Conditions::default();
        let effects = run(&mut lc, 0, EMPTY_TICKS + 1, empty);
        assert_eq!(
            effects,
            vec![(EMPTY_TICKS, Effect::Cleanup)],
            "expected exactly one cleanup at the 60-second mark"
        );
        assert_eq!(lc.phase(), Phase::Cleanup);
        assert_eq!(EMPTY_TICKS, 1200);
    }

    #[test]
    fn cleanup_fires_exactly_once() {
        // The persistence callback is the one effect that must not repeat: a
        // Durable Object that persists on every tick after cleanup would write
        // the same row 20 times a second forever.
        let mut lc = Lifecycle::new(tick(0));
        let effects = run(&mut lc, 0, EMPTY_TICKS * 3, Conditions::default());
        assert_eq!(effects, vec![(EMPTY_TICKS, Effect::Cleanup)]);
        assert_eq!(
            effects
                .iter()
                .filter(|(_, e)| *e == Effect::Cleanup)
                .count(),
            1
        );
        // And it stays terminal.
        assert!(lc.phase().is_terminal());
    }

    #[test]
    fn a_player_arriving_resets_the_empty_timer() {
        let mut lc = Lifecycle::new(tick(0));
        run(&mut lc, 0, 1000, Conditions::default());
        assert_eq!(lc.phase(), Phase::Lobby, "1000 < 1200, still alive");

        // Someone joins for one tick, then leaves again.
        lc.update(tick(1000), ready(1));
        assert_eq!(lc.phase(), Phase::Lobby);
        assert_eq!(lc.remaining(tick(1000)), None, "not empty, no timer");

        // The clock starts again from the second departure.
        let effects = run(&mut lc, 1001, EMPTY_TICKS + 1, Conditions::default());
        assert_eq!(effects, vec![(1001 + EMPTY_TICKS, Effect::Cleanup)]);
    }

    #[test]
    fn an_empty_ended_room_never_uses_the_sixty_second_rule() {
        // §6.10 draws the "empty 60 s" edge out of the Ended column too, but it
        // cannot fire there: the 10-second results timer is always shorter. The
        // empty timer is still tracked (it costs nothing), and this test records
        // why the arrow is redundant rather than silently ignoring it.
        let mut lc = Lifecycle::at(Phase::Ended, tick(0));
        let empty_over = Conditions {
            players: 0,
            all_ready: false,
            match_over: true,
        };
        let effects = run(&mut lc, 0, EMPTY_TICKS * 2, empty_over);
        assert_eq!(effects, vec![(ENDED_TICKS, Effect::Cleanup)]);
    }

    #[test]
    fn a_live_room_is_never_cleaned_up_directly() {
        // There is no Live -> Cleanup edge, and that is correct rather than an
        // omission: a live room with nobody in it still has a match, and the
        // match ends it (D-05's NoHumans) before the empty-room rule could
        // apply. If this ever stops being true, the match rules have a bug that
        // a cleanup timeout would paper over.
        let mut lc = Lifecycle::at(Phase::Live, tick(0));
        let effects = run(&mut lc, 0, EMPTY_TICKS * 2, Conditions::default());
        assert_eq!(effects, vec![]);
        assert_eq!(lc.phase(), Phase::Live);
        assert!(!Phase::Live.can_enter(Phase::Cleanup));
    }

    // -- timers -------------------------------------------------------------

    #[test]
    fn remaining_counts_down_and_never_wraps() {
        let mut lc = Lifecycle::at(Phase::Countdown, tick(0));
        assert_eq!(lc.remaining(tick(0)), Some(COUNTDOWN_TICKS));
        assert_eq!(lc.remaining(tick(30)), Some(30));
        assert_eq!(lc.remaining(tick(59)), Some(1));
        // Past the end it clamps rather than underflowing; the transition would
        // already have happened, but `remaining` must be safe to call anyway.
        assert_eq!(lc.remaining(tick(100)), Some(0));

        // Live has no timer.
        lc.transition(Phase::Live, tick(100)).unwrap();
        assert_eq!(lc.remaining(tick(100)), None);
    }

    #[test]
    fn rehydrating_a_phase_restarts_its_timer() {
        // A host that restarted mid-countdown shows a fresh three seconds. It
        // must not resume a countdown that "should" have finished while nothing
        // was simulating — that would start a match the moment the room came
        // back, with no countdown and possibly no players.
        let lc = Lifecycle::at(Phase::Countdown, tick(9_000));
        assert_eq!(lc.remaining(tick(9_000)), Some(COUNTDOWN_TICKS));
        assert_eq!(lc.elapsed(tick(9_000)), 0);
    }
}
