//! The §12.3 recipe matcher matrix, one test per row.
//!
//! This file exists separately from the unit tests in `src/recipe.rs` because
//! it is the *contract*: the plan calls the matcher the highest-risk system in
//! the project (R-02), and §12.3 says the negative cases matter more than the
//! positive ones. A matcher that passes only the positive rows is not done.
//!
//! Rows are numbered to match the table so a failure names the row.

use sim::buddy::BuddyKind;
use sim::pad::{Pad, Pads, PlaceError, PAD_HEIGHT, PAD_SIZE, STABILITY_TICKS};
use sim::recipe::{
    find_match, tick, tick_with, BlockPos, CraftOutcome, RecipeBook, RecipeId, RecipeOutput,
    MIN_CLUSTER,
};
use sim::time::Tick;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn book() -> &'static RecipeBook {
    RecipeBook::embedded()
}

/// Authoring name of a recipe id, for readable assertions.
fn name_of(id: RecipeId) -> &'static str {
    &book().get(id).expect("recipe id from this book").name
}

/// A pad holding `blocks`, with the stability window primed at tick 0.
///
/// Priming is explicit because `Pad::set` does not know the tick: without it a
/// fixture would look like it had been edited on the first tick, and every
/// stability test would be measuring the fixture rather than the rule.
fn pad_with(blocks: &[BlockPos]) -> Pad {
    let mut pad = Pad::new([0, 0, 0]);
    for b in blocks {
        assert!(pad.set(*b, true), "{b:?} should be placeable");
    }
    pad.reset_stability(Tick::ZERO);
    pad
}

/// §4.5.3 `R01_SWORD`: a 3 x 2 slab, 6 blocks.
fn sword() -> Vec<BlockPos> {
    vec![
        [0, 0, 0],
        [1, 0, 0],
        [2, 0, 0],
        [0, 0, 1],
        [1, 0, 1],
        [2, 0, 1],
    ]
}

/// The same slab turned 90 degrees: 2 x 3.
fn sword_turned() -> Vec<BlockPos> {
    vec![
        [0, 0, 0],
        [1, 0, 0],
        [0, 0, 1],
        [1, 0, 1],
        [0, 0, 2],
        [1, 0, 2],
    ]
}

/// §4.5.3 `R03_BANDAGE`: a 2 x 2 square, 4 blocks, offset by `at`.
fn square_2x2(at: [i32; 3]) -> Vec<BlockPos> {
    vec![
        [at[0], at[1], at[2]],
        [at[0] + 1, at[1], at[2]],
        [at[0], at[1], at[2] + 1],
        [at[0] + 1, at[1], at[2] + 1],
    ]
}

fn expect_recipe(pad: &Pad, elapsed_s: u32, expected: &str) {
    let craft = find_match(pad, book(), elapsed_s)
        .unwrap_or_else(|| panic!("expected {expected}, matcher found nothing"));
    assert_eq!(name_of(craft.recipe), expected);
}

fn expect_nothing(pad: &Pad, elapsed_s: u32) {
    if let Some(craft) = find_match(pad, book(), elapsed_s) {
        panic!(
            "expected no match, got {} ({} blocks)",
            name_of(craft.recipe),
            craft.blocks.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Row 1 — exact match
// ---------------------------------------------------------------------------

#[test]
fn row01_exact_match() {
    let pad = pad_with(&sword());
    assert_eq!(pad.count(), 6);
    let craft = find_match(&pad, book(), 0).expect("an exact 3x2 slab must match");
    assert_eq!(name_of(craft.recipe), "R01_SWORD");
    assert_eq!(
        craft.blocks.len(),
        6,
        "the match consumes exactly the shape"
    );
}

// ---------------------------------------------------------------------------
// Row 2 — rotated match
// ---------------------------------------------------------------------------

#[test]
fn row02_rotated_match() {
    let pad = pad_with(&sword_turned());
    assert_eq!(pad.count(), 6, "same shape, same cost");
    expect_recipe(&pad, 0, "R01_SWORD");
}

#[test]
fn row02b_all_four_rotations_match() {
    // Every quarter turn of the slab must be the same recipe, not just the one
    // the fixture happens to use.
    let base = sword();
    for turn in 0..4u32 {
        let rotated: Vec<BlockPos> = base
            .iter()
            .map(|b| {
                let (mut x, mut z) = (b[0], b[2]);
                for _ in 0..turn {
                    let (nx, nz) = (-z, x);
                    x = nx;
                    z = nz;
                }
                // Re-anchor into the pad so the shape stays placeable.
                [x + 2, b[1], z + 2]
            })
            .collect();
        let pad = pad_with(&rotated);
        expect_recipe(&pad, 0, "R01_SWORD");
    }
}

// ---------------------------------------------------------------------------
// Row 3 — one extra crate
// ---------------------------------------------------------------------------

#[test]
fn row03_one_extra_crate_does_not_match() {
    let mut blocks = sword();
    blocks.push([3, 0, 0]);
    let pad = pad_with(&blocks);
    assert_eq!(pad.count(), 7);
    // 7 blocks is `R08_SPEAR`'s cost, so this row also proves the matcher is
    // not matching on size alone.
    expect_nothing(&pad, 0);
}

// ---------------------------------------------------------------------------
// Row 4 — one missing crate
// ---------------------------------------------------------------------------

#[test]
fn row04_one_missing_crate_does_not_match() {
    let mut blocks = sword();
    blocks.pop(); // drop (2,0,1)
    let pad = pad_with(&blocks);
    assert_eq!(pad.count(), 5);
    // 5 blocks is `R02_BOW`'s cost: again, cost is not identity.
    expect_nothing(&pad, 0);
}

// ---------------------------------------------------------------------------
// Row 5 — correct shape, floating
// ---------------------------------------------------------------------------

#[test]
fn row05_floating_shape_does_not_match() {
    // §4.5.1 rule 3: nothing at y = 0, so the lowest layer has no floor.
    let lifted: Vec<BlockPos> = sword().iter().map(|b| [b[0], b[1] + 1, b[2]]).collect();
    let pad = pad_with(&lifted);
    assert_eq!(pad.count(), 6, "the shape is complete, only unsupported");
    expect_nothing(&pad, 0);
}

// ---------------------------------------------------------------------------
// Row 6 — correct shape, one crate unsupported
// ---------------------------------------------------------------------------

#[test]
fn row06_unsupported_crate_is_scoped_to_the_lowest_layer() {
    // §12.3 rows 5 and 6 are the same predicate under 6-connectivity plus a
    // uniformly solid pad floor: a crate directly beneath a cluster member is
    // face-adjacent, therefore a member, therefore the cluster's lowest layer
    // is its minimum. So "partially floating" cannot be expressed as a shape
    // whose lowest layer is at y = 0.
    //
    // What *can* be tested — and what a naive implementation gets wrong — is
    // that rule 3 is scoped to the lowest layer and not to every crate.
    // `R13_WARD` is the witness: its `(1,1,1)` crate has nothing beneath it,
    // and the recipe must still match.
    let ward = book()
        .by_name("R13_WARD")
        .expect("R13_WARD exists")
        .blocks
        .clone();
    let pad = pad_with(&ward);
    assert!(
        !pad.get([1, 0, 1]),
        "the ward's top-centre crate floats over the ring's hole"
    );
    expect_recipe(&pad, 480, "R13_WARD");
}

#[test]
fn row06b_the_same_shape_lifted_loses_its_floor() {
    let ward = book().by_name("R13_WARD").unwrap().blocks.clone();
    let lifted: Vec<BlockPos> = ward.iter().map(|b| [b[0], b[1] + 1, b[2]]).collect();
    let pad = pad_with(&lifted);
    assert_eq!(pad.count(), ward.len());
    expect_nothing(&pad, 480);
}

// ---------------------------------------------------------------------------
// Row 7 — too large
// ---------------------------------------------------------------------------

/// A 64-crate wall: a full 8 x 4 face, two blocks deep.
fn wall_64() -> Pad {
    let mut pad = Pad::new([0, 0, 0]);
    for y in 0..PAD_HEIGHT {
        for x in 0..PAD_SIZE {
            for z in 0..2 {
                pad.set([x, y, z], true);
            }
        }
    }
    pad.reset_stability(Tick::ZERO);
    pad
}

#[test]
fn row07_too_large_does_not_match() {
    let pad = wall_64();
    assert_eq!(pad.count(), 64);
    expect_nothing(&pad, 0);
}

#[test]
fn cluster_cap_aborts_fast() {
    // AC: "a 64-block wall returns None in <= 20 us". The meaningful bound is
    // the release one; a debug build does the same work with bounds checks and
    // no inlining, so it gets a looser bound rather than a flaky test.
    let pad = wall_64();
    assert!(find_match(&pad, book(), 0).is_none());

    let runs = 200u32;
    let start = std::time::Instant::now();
    for _ in 0..runs {
        assert!(find_match(&pad, book(), 0).is_none());
    }
    let per_call_us = start.elapsed().as_micros() as f64 / f64::from(runs);

    let budget_us = if cfg!(debug_assertions) { 500.0 } else { 20.0 };
    println!("matcher on a 64-crate wall: {per_call_us:.2} us/call (budget {budget_us} us)");
    assert!(
        per_call_us < budget_us,
        "matcher took {per_call_us:.2} us per call on a 64-crate wall (budget {budget_us} us); \
         the flood fill must abort at {} members, not walk the whole pad",
        MIN_CLUSTER
    );
}

// ---------------------------------------------------------------------------
// Row 8 — two clusters adjacent
// ---------------------------------------------------------------------------

#[test]
fn row08_two_adjacent_clusters_craft_independently() {
    // The sword occupies x 0..2; the square starts at x 4, leaving a one-cell
    // gap so they are genuinely two clusters.
    let mut blocks = sword();
    blocks.extend(square_2x2([4, 0, 0]));
    let mut pad = pad_with(&blocks);
    assert_eq!(pad.count(), 10);

    let first = tick(&mut pad, book(), Tick(10), 0).expect("the first cluster must craft");
    assert_eq!(name_of(first.recipe()), "R01_SWORD");
    assert!(first.crafted());
    assert_eq!(pad.count(), 4, "only the sword's blocks are consumed");

    let second = tick(&mut pad, book(), Tick(20), 0).expect("the second cluster must craft");
    assert_eq!(name_of(second.recipe()), "R03_BANDAGE");
    assert!(second.crafted());
    assert!(pad.is_empty());
}

#[test]
fn row08b_crafting_order_follows_the_pad_scan_order() {
    // Same contents, but the square is now *first* in the fixed y/z/x scan, so
    // it must craft first. The order is a property of the contents, never of a
    // hash map.
    let mut blocks = square_2x2([0, 0, 0]);
    blocks.extend(sword().iter().map(|b| [b[0] + 4, b[1], b[2]]));
    let mut pad = pad_with(&blocks);

    let first = tick(&mut pad, book(), Tick(10), 0).expect("something must craft");
    assert_eq!(name_of(first.recipe()), "R03_BANDAGE");
    let second = tick(&mut pad, book(), Tick(20), 0).expect("the rest must craft");
    assert_eq!(name_of(second.recipe()), "R01_SWORD");
}

// ---------------------------------------------------------------------------
// Row 9 — cluster outside any pad
// ---------------------------------------------------------------------------

#[test]
fn rejects_cluster_outside_pad() {
    // AC, §4.5.1 rule 1. A perfect sword built on open terrain must not exist
    // as far as the matcher is concerned — and it cannot exist, because
    // placement itself refuses.
    let mut pads = Pads::new();
    let team = pads.push(Pad::new([0, 0, 0]));

    for b in sword() {
        let world = [200 + b[0], 40 + b[1], 200 + b[2]];
        assert_eq!(
            pads.place(team, world, true),
            Err(PlaceError::Outside),
            "terrain is not buildable (§4.4.3)"
        );
    }
    let pad = pads.get(team).expect("pad exists");
    assert!(pad.is_empty(), "nothing reached the pad");
    expect_nothing(pad, 0);
}

// ---------------------------------------------------------------------------
// Row 10 — cluster in an enemy pad
// ---------------------------------------------------------------------------

#[test]
fn row10_enemy_pad_rejects_placement() {
    let mut pads = Pads::new();
    let mine = pads.push(Pad::new([0, 0, 0]));
    let theirs = pads.push(Pad::new([100, 0, 0]));

    for b in sword() {
        let world = [100 + b[0], b[1], b[2]];
        assert_eq!(
            pads.place(mine, world, true),
            Err(PlaceError::NotYours),
            "§4.5.4: enemy pad cells reject placement"
        );
    }
    assert!(pads.get(theirs).expect("pad exists").is_empty());
    expect_nothing(pads.get(theirs).expect("pad exists"), 0);

    // The owner can build the identical shape, and then it does match — so the
    // rejection above is about ownership, not about the shape.
    for b in sword() {
        assert_eq!(pads.place(theirs, [100 + b[0], b[1], b[2]], true), Ok(true));
    }
    let pad = pads.get(theirs).expect("pad exists");
    pad_has_sword(pad);
}

fn pad_has_sword(pad: &Pad) {
    expect_recipe(pad, 0, "R01_SWORD");
}

// ---------------------------------------------------------------------------
// Row 11 — ambiguous shapes
// ---------------------------------------------------------------------------

#[test]
fn no_duplicate_canonical_forms() {
    // AC: loading the shipped table must succeed, and every form must be
    // distinct, or one of the 14 recipes is unreachable.
    let book = book();
    assert_eq!(book.len(), 14, "§4.5.3 lists 14 recipes");
    let mut keys: Vec<&[u8]> = book.recipes().iter().map(|r| r.canonical_key()).collect();
    keys.sort_unstable();
    for pair in keys.windows(2) {
        assert_ne!(
            pair[0], pair[1],
            "two recipes share a canonical form; matching is exact equality, so one is dead"
        );
    }

    // And a table that *does* collide must fail at load, naming both sides.
    let doctored = r#"
    [
      (id: "R01_SWORD", output: Sword, unlock_s: 0, cooldown_s: 20,
       blocks: [[0,0,0],[1,0,0],[2,0,0],[0,0,1],[1,0,1],[2,0,1]]),
      (id: "R01_SWORD_AGAIN", output: Sword, unlock_s: 0, cooldown_s: 20,
       blocks: [[0,0,0],[0,0,1],[0,0,2],[1,0,0],[1,0,1],[1,0,2]]),
    ]
    "#;
    let err = RecipeBook::parse(doctored).expect_err("a collision must be a load error");
    assert_eq!(err.recipe, "R01_SWORD_AGAIN");
    assert!(
        err.reason.contains("R01_SWORD"),
        "the error must name the recipe it collided with, got: {}",
        err.reason
    );
}

// ---------------------------------------------------------------------------
// Row 12 — stability, broken before the window closes
// ---------------------------------------------------------------------------

#[test]
fn row12_shape_broken_before_ten_ticks_does_not_craft() {
    let mut pad = Pad::new([0, 0, 0]);
    let blocks = sword();

    // One crate per tick, last one at tick 5.
    for (t, b) in blocks.iter().enumerate() {
        pad.set(*b, true);
        assert!(
            tick(&mut pad, book(), Tick(t as u32), 0).is_none(),
            "must not craft mid-deposit at tick {t}"
        );
    }

    // Ticks 6..=13 are quiet: eight of the ten the rule needs.
    for t in 6..=13u32 {
        assert!(tick(&mut pad, book(), Tick(t), 0).is_none(), "fired at {t}");
    }

    // Break the shape on the ninth quiet tick.
    pad.set([7, 0, 7], true);
    assert!(tick(&mut pad, book(), Tick(14), 0).is_none());
    pad.set([7, 0, 7], false);
    // The window restarted, so the shape that was one tick from firing is now
    // ten ticks away again.
    assert!(tick(&mut pad, book(), Tick(15), 0).is_none());
}

// ---------------------------------------------------------------------------
// Row 13 — stability, ten ticks
// ---------------------------------------------------------------------------

#[test]
fn deposit_then_craft() {
    // AC: "6 crates deposited one per tick at 6/s produce a sword on the tick
    // after the 10-tick stability window". Last deposit is tick 5, so the
    // window closes at tick 15 — not 14, and not 16.
    let mut pad = Pad::new([0, 0, 0]);
    let blocks = sword();

    for (t, b) in blocks.iter().enumerate() {
        pad.set(*b, true);
        assert!(tick(&mut pad, book(), Tick(t as u32), 0).is_none());
    }
    for t in 6..=14u32 {
        assert!(
            tick(&mut pad, book(), Tick(t), 0).is_none(),
            "fired early at {t}"
        );
    }

    let out = tick(&mut pad, book(), Tick(15), 0).expect("tick 15 is ten ticks after tick 5");
    assert!(out.crafted(), "the craft must actually produce something");
    assert_eq!(name_of(out.recipe()), "R01_SWORD");
    assert_eq!(
        out.blocks().len(),
        6,
        "the outcome reports exactly the blocks it consumed"
    );
    assert_eq!(pad.count(), 0, "consumption is atomic with the match");

    // Consumption is also final: nothing fires again from the same shape.
    for t in 16..40u32 {
        assert!(tick(&mut pad, book(), Tick(t), 0).is_none());
    }
}

// ---------------------------------------------------------------------------
// Row 14 — double trigger
// ---------------------------------------------------------------------------

#[test]
fn row14_two_players_completing_in_the_same_tick_craft_once() {
    let mut pad = Pad::new([0, 0, 0]);
    let blocks = sword();

    // Four crates over four ticks.
    for (t, b) in blocks[..4].iter().enumerate() {
        pad.set(*b, true);
        assert!(tick(&mut pad, book(), Tick(t as u32), 0).is_none());
    }

    // Two players deposit the last two in the same tick.
    pad.set(blocks[4], true);
    pad.set(blocks[5], true);

    let mut crafts = 0;
    for t in 4..=14u32 {
        if tick(&mut pad, book(), Tick(t), 0).is_some() {
            crafts += 1;
        }
    }
    assert_eq!(crafts, 1, "exactly one craft, exactly one event");
    assert!(pad.is_empty());

    // A second caller in the same tick finds nothing to consume. This is the
    // structural half of the double-trigger defence: the first match already
    // emptied the pad.
    assert!(
        tick(&mut pad, book(), Tick(14), 0).is_none(),
        "the same tick must not craft twice"
    );
}

// ---------------------------------------------------------------------------
// Economy rules that the matrix implies but does not spell out
// ---------------------------------------------------------------------------

#[test]
fn buddy_cap_refusal_consumes_nothing() {
    // §4.5.4: "A craft attempt at the cap consumes nothing and returns a
    // 'cap reached' event — the player keeps their crates."
    let mut pad = pad_with(&sword());
    let out = tick_with(&mut pad, book(), Tick(10), 0, |_| false)
        .expect("the shape is stable and matches");
    match out {
        CraftOutcome::Blocked(craft) => assert_eq!(name_of(craft.recipe), "R01_SWORD"),
        CraftOutcome::Crafted(_) => panic!("a refused craft must not be reported as crafted"),
    }
    assert_eq!(pad.count(), 6, "the crates are still on the pad");

    // And it retries once per stability window rather than once per tick.
    assert!(tick_with(&mut pad, book(), Tick(11), 0, |_| false).is_none());
    assert!(tick_with(&mut pad, book(), Tick(20), 0, |_| true).is_some());
}

#[test]
fn locked_recipes_do_not_eat_crates() {
    // A tier-3 shape built at 1:00 is a player mistake, not a wager: it must
    // sit there untouched until the tier unlocks.
    let ram = book()
        .by_name("R10_RAM")
        .expect("R10_RAM exists")
        .blocks
        .clone();
    let mut pad = pad_with(&ram);
    assert!(tick(&mut pad, book(), Tick(10), 60).is_none());
    assert_eq!(pad.count(), ram.len(), "nothing consumed while locked");
    let out = tick(&mut pad, book(), Tick(20), 480).expect("unlocked at 8:00");
    assert_eq!(name_of(out.recipe()), "R10_RAM");
}

#[test]
fn content_matches_the_plan_table() {
    let expected = [
        ("R01_SWORD", RecipeOutput::Sword, 6, 0, 20),
        ("R02_BOW", RecipeOutput::Bow, 5, 0, 20),
        ("R03_BANDAGE", RecipeOutput::Bandage, 4, 0, 15),
        (
            "R04_SWORDSMAN",
            RecipeOutput::Buddy(BuddyKind::Swordsman),
            9,
            0,
            40,
        ),
        (
            "R05_ARCHER",
            RecipeOutput::Buddy(BuddyKind::Archer),
            8,
            180,
            45,
        ),
        (
            "R06_MAGE",
            RecipeOutput::Buddy(BuddyKind::Mage),
            12,
            180,
            60,
        ),
        (
            "R07_HEALER",
            RecipeOutput::Buddy(BuddyKind::Healer),
            10,
            180,
            60,
        ),
        ("R08_SPEAR", RecipeOutput::Spear, 7, 180, 30),
        ("R09_TOWER", RecipeOutput::Tower, 16, 180, 90),
        ("R10_RAM", RecipeOutput::Ram, 24, 480, 120),
        ("R11_CATAPULT", RecipeOutput::Catapult, 20, 480, 120),
        (
            "R12_BERSERKER",
            RecipeOutput::Buddy(BuddyKind::Berserker),
            18,
            480,
            90,
        ),
        ("R13_WARD", RecipeOutput::Ward, 14, 480, 120),
        (
            "R14_SNIPER",
            RecipeOutput::Buddy(BuddyKind::Sniper),
            15,
            480,
            90,
        ),
    ];
    let book = book();
    assert_eq!(book.len(), expected.len(), "§4.5.3 has 14 recipes");
    for (recipe, (name, output, cost, unlock, cooldown)) in book.recipes().iter().zip(expected) {
        assert_eq!(recipe.name, name);
        assert_eq!(recipe.output, output, "{name} output");
        assert_eq!(recipe.cost(), cost, "{name} cost");
        assert_eq!(recipe.unlock_s, unlock, "{name} unlock");
        assert_eq!(recipe.cooldown_s, cooldown, "{name} cooldown");
    }
}

#[test]
fn content_is_reachable() {
    // Load-time validation already checks bounds, ground support and
    // connectivity; this proves every shipped shape is also reachable through
    // a real crafting pass after being placed one cell in from the pad edge.
    // A recipe that validates but never matches is the worst content bug.
    for recipe in book().recipes() {
        let placed: Vec<BlockPos> = recipe
            .blocks
            .iter()
            .map(|b| [b[0] + 1, b[1], b[2] + 1])
            .collect();
        let mut pad = pad_with(&placed);
        let outcome = tick(&mut pad, book(), Tick(STABILITY_TICKS), u32::MAX)
            .unwrap_or_else(|| panic!("{} is in the table but cannot be crafted", recipe.name));
        assert!(outcome.crafted(), "{} was blocked", recipe.name);
        assert_eq!(
            outcome.recipe(),
            recipe.id,
            "{} matched the wrong recipe",
            recipe.name
        );
        assert_eq!(
            outcome.blocks().len(),
            recipe.cost() as usize,
            "{} consumed a different number of crates",
            recipe.name
        );
        assert!(pad.is_empty(), "{} left crates behind", recipe.name);
    }
}
