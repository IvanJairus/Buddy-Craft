//! Determinism harness and golden fixtures (plan T-015, §12.2).
//!
//! # What this proves
//!
//! A seeded scenario is driven for 20,000 ticks and a five-field hash is taken
//! every 100. The sequence is compared against a committed file. Two things
//! follow:
//!
//! * **The rules are a pure function of their inputs.** Run it twice on the
//!   same machine and you get the same bytes; that is already true of every
//!   unit test, but here it is true of the *composition* of movement, recipes,
//!   buddy AI, combat and match rules in one tick order.
//! * **The rules are a pure function on any architecture.** CI runs this test
//!   on x86_64 and aarch64 (risk R-07). A fixed-point bug that only shows up on
//!   a phone shows up here instead.
//!
//! # Why five fields and not one
//!
//! A single hash tells you *that* something diverged. Five tells you which
//! subsystem: `state`, `movement`, `buddies`, `combat` or the tick itself. The
//! first diverging line names both the tick and the field, which is the
//! difference between a five-minute bisect and an afternoon.
//!
//! # Regenerating
//!
//! `UPDATE_GOLDENS=1 cargo test -p sim --test golden` rewrites the files. Do it
//! when a *rule* changed on purpose, never to make a red test green: the diff
//! is the record of what changed, and reviewing it is the point.

use std::fmt::Write as _;
use std::path::PathBuf;

use sim::scenario::{HashFrame, Scenario, ScenarioConfig};

/// Ticks per fixture (§12.2: 20,000 input frames).
const TICKS: u32 = 20_000;

/// Checkpoint interval (§12.2: every 100 ticks).
const HASH_EVERY: u32 = 100;

/// One committed fixture.
struct Fixture {
    /// File stem under `tests/golden/`.
    name: &'static str,
    /// How to build it.
    make: fn() -> ScenarioConfig,
}

/// The three fixtures.
///
/// They differ in room shape and in whether the match can end, so between them
/// they cover: no elimination (duel), chip damage that never finishes a core
/// (teams), and elimination plus outcome mid-run (brawl).
const FIXTURES: [Fixture; 3] = [
    Fixture {
        name: "duel",
        make: || ScenarioConfig::duel(0x00C0_FFEE, TICKS),
    },
    Fixture {
        name: "teams",
        make: || ScenarioConfig::teams(0x0000_5EED, TICKS),
    },
    Fixture {
        name: "brawl",
        make: || ScenarioConfig::brawl(0x0BAD_C0DE, TICKS),
    },
];

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn golden_path(name: &str) -> PathBuf {
    golden_dir().join(format!("{name}.hash"))
}

/// Run a fixture and return every checkpoint.
fn sequence(make: fn() -> ScenarioConfig) -> Vec<HashFrame> {
    let mut scenario = Scenario::new(make());
    let mut frames = Vec::with_capacity((TICKS / HASH_EVERY) as usize + 2);
    scenario.run(HASH_EVERY, |frame| frames.push(frame));
    frames
}

fn render(frames: &[HashFrame]) -> String {
    let mut out = String::with_capacity(frames.len() * 80);
    for frame in frames {
        out.push_str(&frame.to_line());
        out.push('\n');
    }
    out
}

fn parse(text: &str) -> Vec<HashFrame> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| HashFrame::parse(line).unwrap_or_else(|| panic!("bad golden line: {line}")))
        .collect()
}

/// The field name of a differing component, for the failure message.
fn differing_field(expected: &HashFrame, actual: &HashFrame) -> &'static str {
    if expected.tick != actual.tick {
        "tick"
    } else if expected.state != actual.state {
        "state"
    } else if expected.movement != actual.movement {
        "movement"
    } else if expected.buddies != actual.buddies {
        "buddies"
    } else {
        "combat"
    }
}

/// Compare a fixture against its committed file, or write it.
///
/// On mismatch the failure names the first diverging tick and field, which is
/// the whole reason the frame is split into components.
fn check(fixture: &Fixture) {
    let actual = sequence(fixture.make);
    let path = golden_path(fixture.name);
    let rendered = render(&actual);

    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(golden_dir()).expect("create tests/golden");
        std::fs::write(&path, &rendered).expect("write golden");
        println!("wrote {} ({} checkpoints)", path.display(), actual.len());
        return;
    }

    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}\nrun: UPDATE_GOLDENS=1 cargo test -p sim --test golden",
            path.display()
        )
    });
    let expected = parse(&text);

    let shared = expected.len().min(actual.len());
    for i in 0..shared {
        if expected[i] != actual[i] {
            let field = differing_field(&expected[i], &actual[i]);
            let mut msg = String::new();
            let _ = write!(
                msg,
                "{} diverged at tick {} in `{}`\n  expected {}\n  actual   {}",
                fixture.name,
                expected[i].tick,
                field,
                expected[i].to_line(),
                actual[i].to_line()
            );
            // Three lines of context either side, so the reader can see whether
            // the divergence was abrupt or drifting.
            for j in i.saturating_sub(3)..(i + 4).min(shared) {
                let _ = write!(
                    msg,
                    "\n  {} tick {:>6}: {}",
                    if j == i { ">>" } else { "  " },
                    expected[j].tick,
                    if expected[j] == actual[j] {
                        "same".to_string()
                    } else {
                        format!("{} differs", differing_field(&expected[j], &actual[j]))
                    }
                );
            }
            panic!("{msg}");
        }
    }
    assert_eq!(
        expected.len(),
        actual.len(),
        "{} has {} checkpoints, expected {} — the run ended early or late",
        fixture.name,
        actual.len(),
        expected.len()
    );
}

#[test]
fn golden_duel() {
    check(&FIXTURES[0]);
}

#[test]
fn golden_teams() {
    check(&FIXTURES[1]);
}

#[test]
fn golden_brawl() {
    check(&FIXTURES[2]);
}

#[test]
fn the_same_seed_produces_the_same_sequence() {
    // The harness's own determinism, independent of the committed files: two
    // independent runs in the same process must agree byte for byte.
    let a = render(&sequence(FIXTURES[0].make));
    let b = render(&sequence(FIXTURES[0].make));
    assert_eq!(a, b, "the harness is not deterministic within one process");
}

#[test]
fn a_different_seed_produces_a_different_sequence() {
    // Guards against the harness ignoring its seed, which would make every
    // golden test pass while testing nothing.
    let a = render(&sequence(|| ScenarioConfig::duel(1, 2_000)));
    let b = render(&sequence(|| ScenarioConfig::duel(2, 2_000)));
    assert_ne!(a, b);
}

#[test]
fn checkpoints_bracket_the_run() {
    let frames = sequence(FIXTURES[0].make);
    assert_eq!(frames[0].tick, 0, "the first checkpoint is tick 0");
    assert!(
        frames.len() >= (TICKS / HASH_EVERY) as usize,
        "expected a checkpoint per {HASH_EVERY} ticks, got {}",
        frames.len()
    );
    // A duel deals no core damage, so it must reach the end of the run.
    assert_eq!(frames.last().unwrap().tick, TICKS);
}

#[test]
fn hash_frames_round_trip_through_their_wire_format() {
    let frames = sequence(|| ScenarioConfig::duel(7, 500));
    let text = render(&frames);
    assert_eq!(parse(&text), frames);
}
