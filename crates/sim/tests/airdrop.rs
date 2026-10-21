//! Acceptance gate for T-025, driven from outside the crate.
//!
//! Everything here goes through `sim`'s public surface, and that is the point:
//! `room` (T-023, T-024) wires its airdrop loop against exactly this API. A
//! helper that only works because a unit test can reach a private field is not
//! an API, and the failure would surface two tasks later as "the room cannot
//! build its own match".
//!
//! The unit tests inside `airdrop.rs` still own the internals — the RNG draw
//! order, the accumulator, the audit arithmetic. This file owns the contract.

use sim::airdrop::{Airdrops, DROP_HEIGHT, SCATTER_RADIUS};
use sim::time::TICK_HZ;
use sim::{PlayerId, Tick, Vec3};

/// A deterministic candidate set: a 7×7 grid of 32 m cells, all at sea level.
///
/// Worldgen's real set excludes base pads; the airdrop module trusts the caller
/// for that, so the fixture does not need to model a map.
fn sites() -> Vec<[i32; 3]> {
    let mut out = Vec::new();
    for x in (32..=224).step_by(32) {
        for z in (32..=224).step_by(32) {
            out.push([x, 40, z]);
        }
    }
    out
}

fn air(players: u32) -> Airdrops {
    Airdrops::new(0xC0FF_EE00, players, sites())
}

/// Tick until the drop in the air lands; returns the landing tick.
fn land(airdrops: &mut Airdrops, from: Tick) -> Tick {
    let mut now = from;
    for _ in 0..(12 * TICK_HZ) {
        if airdrops.falling().is_empty() {
            return now;
        }
        now = Tick(now.0 + 1);
        airdrops.tick(now, &[]);
    }
    panic!("the drop never landed");
}

/// Spawn and land one drop; returns its site and landing tick.
fn drop_and_land(airdrops: &mut Airdrops) -> ([i32; 3], Tick) {
    let at = airdrops.next_drop_at();
    airdrops.tick(at, &[]);
    let site = airdrops.falling().last().expect("a drop").site;
    (site, land(airdrops, at))
}

/// Walk to every crate on the ground, one per tick.
fn collect_everything(airdrops: &mut Airdrops, player: PlayerId, now: &mut Tick) -> u32 {
    let mut got = 0;
    for _ in 0..(airdrops.pickups().len() + 4) {
        let Some(pickup) = airdrops.pickups().first().copied() else {
            break;
        };
        *now = Tick(now.0 + 1);
        let events = airdrops.tick(*now, &[(player, pickup.pos)]);
        got += events.picked.iter().map(|(_, n)| n).sum::<u32>();
    }
    got
}

#[test]
fn drop_schedule_over_20_min() {
    // The acceptance criterion says "59 ± 1 drops". It is not met, and the
    // schedule is right anyway: §4.4.1's own intervals give 12 drops in the
    // first five minutes (0:10, 0:35, … 4:45), a drop at 5:00, then 60 more
    // every 15 s to 20:00 — 73. There is no set of intervals that produces 59
    // and still matches the two rows above it, because 59 − 12 = 47 late drops
    // is 19.1 s apart, not 15 s. The summary line is the arithmetic that is
    // wrong, so the intervals ship and this number is recorded as a plan
    // discrepancy in the README rather than reverse-engineered into the code.
    let mut airdrops = air(2);
    let end = 20 * 60 * TICK_HZ;
    let mut drops = 0;
    for tick in 1..=end {
        drops += airdrops.tick(Tick(tick), &[]).spawned.len();
    }
    assert_eq!(drops, 73, "drops in 20 minutes");
}

#[test]
fn drop_size_scales_with_players() {
    for players in [2u32, 4, 6] {
        let mut airdrops = air(players);
        airdrops.tick(Tick(200), &[]);
        let drop = airdrops.falling()[0];
        assert_eq!(drop.crates, 3 * players, "3 crates per player");
        assert!(!drop.heavy, "the first drop is not the heavy one");
    }
}

#[test]
fn site_selection_never_repeats_and_is_stable() {
    let mut airdrops = air(2);
    let mut seen = Vec::new();
    for tick in 1..=(6 * 60 * TICK_HZ) {
        for spawned in airdrops.tick(Tick(tick), &[]).spawned {
            if let Some(previous) = seen.last() {
                assert_ne!(
                    *previous, spawned.site,
                    "two consecutive drops in one place"
                );
            }
            seen.push(spawned.site);
        }
    }
    assert!(
        seen.len() > 10,
        "the schedule produced {} drops",
        seen.len()
    );

    // Same seed, same sequence, with nothing shared between the runs.
    let mut again = air(2);
    let mut second = Vec::new();
    for tick in 1..=(6 * 60 * TICK_HZ) {
        for spawned in again.tick(Tick(tick), &[]).spawned {
            second.push(spawned.site);
        }
    }
    assert_eq!(seen, second);
}

/// The site sequence is a *shared* number: the client draws the telegraph from
/// the same seed the server picks from, without being told where the drop goes.
/// That only works if the pick is bit-identical on both targets, so this pins
/// the whole sequence to one committed hash and CI runs the test under
/// `wasm32-wasip1` as well as natively (plan T-025 AC "site_selection_is_portable").
///
/// A literal is the right shape here for the same reason T-015 commits golden
/// hashes: "the two runs agree" is a claim about a *third* value, and a test that
/// only compared native to wasm in one process would pass just as happily with
/// both sides wrong in the same way.
#[test]
fn site_selection_is_portable() {
    let mut airdrops = air(2);
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut drops = 0u32;
    let byte = |value: u8, hash: &mut u64| {
        *hash ^= u64::from(value);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for tick in 1..=(6 * 60 * TICK_HZ) {
        for spawned in airdrops.tick(Tick(tick), &[]).spawned {
            drops += 1;
            for axis in spawned.site {
                for b in (axis as u32).to_le_bytes() {
                    byte(b, &mut hash);
                }
            }
            byte(spawned.crates as u8, &mut hash);
            byte(u8::from(spawned.heavy), &mut hash);
        }
    }

    assert_eq!(drops, 17, "six minutes of drops");
    assert_eq!(drops, airdrops.drops_made());
    assert_eq!(
        hash, 0xc5ab_e12a_2d0e_4091,
        "site sequence changed: native and wasm must produce this exact hash"
    );
}

#[test]
fn crates_land_on_ground() {
    let mut airdrops = air(2);
    let at = airdrops.next_drop_at();
    airdrops.tick(at, &[]);
    assert_eq!(airdrops.falling().len(), 1);
    assert_eq!(
        airdrops.falling()[0].y,
        sim::Fixed::from_int(DROP_HEIGHT),
        "a drop spawns at full height, not already falling"
    );

    let landed_at = land(&mut airdrops, at);
    // 80 m from Y 120 to Y 40 at 12 m/s is 6.7 s. The telegraph row asks for
    // 5 s "before impact", which a 6.7 s descent cannot give — the descent is
    // the longer of the two, so the marker is simply up longer.
    assert_eq!(landed_at.0 - at.0, 134, "80 m at 0.6 m/tick");
    assert_eq!(airdrops.pickups().len(), 6, "2 players × 3 crates");
    assert_eq!(airdrops.audit().dropped, 6);
}

#[test]
fn landing_scatter_stays_in_3m() {
    let mut airdrops = air(6);
    for _ in 0..20 {
        let (site, landed_at) = drop_and_land(&mut airdrops);
        let site_pos = Vec3::from_ints(site[0], site[1], site[2]);
        let mut checked = 0;
        for pickup in airdrops.pickups().iter().filter(|p| p.born == landed_at) {
            checked += 1;
            assert!(pickup.pos.horizontal_distance(site_pos) <= SCATTER_RADIUS);
        }
        assert!(checked > 0, "the drop produced no crates");
    }
}

#[test]
fn carry_cap_enforced() {
    let mut airdrops = air(6);
    // Three drops of 18 = 54 crates available, and the cap is 12.
    for _ in 0..3 {
        let (_, mut now) = drop_and_land(&mut airdrops);
        let site = airdrops.pickups().first().expect("crates").pos;
        for _ in 0..3 {
            now = Tick(now.0 + 1);
            airdrops.tick(now, &[(PlayerId(0), site)]);
        }
    }
    assert_eq!(airdrops.carried(PlayerId(0)), 12);
}

#[test]
fn cannot_deposit_in_enemy_pad() {
    let mut airdrops = air(2);
    let (_, mut now) = drop_and_land(&mut airdrops);
    assert_eq!(collect_everything(&mut airdrops, PlayerId(0), &mut now), 6);

    // A full second outside your own pad: nothing leaves the hands, and nothing
    // is banked for later either.
    for tick in 0..20 {
        assert_eq!(airdrops.deposit(PlayerId(0), Tick(now.0 + tick), false), 0);
    }
    assert_eq!(airdrops.carried(PlayerId(0)), 6);
    assert_eq!(airdrops.audit().deposited, 0);

    // One second inside it: six crates, the stated rate, no burst for the
    // second spent walking.
    let mut placed = 0;
    for tick in 20..40 {
        placed += airdrops.deposit(PlayerId(0), Tick(now.0 + tick), true);
    }
    assert_eq!(placed, 6, "6 crates per second, not seven");
    assert_eq!(airdrops.carried(PlayerId(0)), 0);
}

#[test]
fn crates_are_conserved() {
    // The plan writes this as "dropped − consumed − destroyed == carried + on
    // ground + placed", which double-counts a placed crate: placing *is* the
    // consumption. The audit asserts the form that actually balances, with each
    // crate in exactly one bucket: dropped == destroyed + deposited + carried +
    // on ground.
    let mut airdrops = air(4);
    for round in 0..8 {
        let (_, mut now) = drop_and_land(&mut airdrops);
        let site = airdrops.pickups().first().expect("crates").pos;
        // Walking the drop is what puts crates in a hand; standing still only
        // reaches the crates that scattered inside the pickup radius, and a
        // round that collects nothing never has a load for the death path to
        // spill. The unit test learned this the same way.
        assert!(
            collect_everything(&mut airdrops, PlayerId(0), &mut now) > 0,
            "round {round} collected nothing"
        );
        for tick in 0..20 {
            airdrops.deposit(PlayerId(0), Tick(now.0 + tick), true);
        }
        now = Tick(now.0 + 20);
        // Kill the carrier, not the bystander: the pickup loop hands every
        // crate to the first body in the list, so player 1 never holds one and
        // killing them would exercise nothing.
        if round % 2 == 0 {
            airdrops.on_death(PlayerId(0), site, now);
        }
        let audit = airdrops.audit();
        assert!(audit.balances(), "round {round}: {audit:?}");
    }
    let audit = airdrops.audit();
    assert!(audit.deposited > 0, "the loop must exercise deposit");
    assert!(audit.destroyed > 0, "the loop must exercise death");
    assert!(audit.on_ground > 0, "the loop must exercise the spill");
}

#[test]
fn pickup_cap_never_exceeded() {
    let mut airdrops = air(6);
    // Seven drops land inside one 180 s pickup lifetime, so nothing has expired
    // when the ceiling is reached: 6 × 18 plus the heavy drop's 32 is 140, and
    // 120 is what survives.
    for _ in 0..7 {
        drop_and_land(&mut airdrops);
    }
    assert_eq!(airdrops.pickups().len(), 120);
    assert!(airdrops.audit().balances());
}

#[test]
fn sixty_drops_and_a_hundred_and_twenty_pickups_fit_the_tick_budget() {
    let mut airdrops = air(6);
    let mut now = Tick(0);
    for _ in 0..60 {
        let (_, landed_at) = drop_and_land(&mut airdrops);
        now = landed_at;
    }
    assert_eq!(airdrops.pickups().len(), 120);

    // Six players standing in the busiest possible spot: the worst case for the
    // pickup loop is every crate tested against every body.
    let bodies: Vec<(PlayerId, Vec3)> = (0..6u8)
        .map(|id| (PlayerId(id), airdrops.pickups()[id as usize].pos))
        .collect();

    let start = std::time::Instant::now();
    let ticks = 2_000;
    for _ in 0..ticks {
        now = Tick(now.0 + 1);
        airdrops.tick(now, &bodies);
    }
    let micros = start.elapsed().as_micros() as u64 / ticks as u64;
    assert!(
        micros <= 400,
        "airdrop tick is {micros} µs with 120 pickups, budget 400 µs"
    );
}
