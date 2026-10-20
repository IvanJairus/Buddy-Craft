# Buddy-Craft

A browser multiplayer voxel arena. Airdropped crates are carried to a stacking
pad and arranged into recipes, recipes spawn weapons, kits and **buddies** —
autonomous units with armed roles. Buddies die permanently, players respawn, and
the match ends when a team's core is destroyed.

> **Status: M1 (simulation rules) in progress.** M0 — the simulation core, voxel
> storage, deterministic worldgen, greedy mesher and chunk-diff codec — is
> complete. M1 has added fixed-point movement (T-009), the crate-recipe matcher
> (T-010), combat (T-011) and buddy AI (T-012); base damage and match rules are
> next, then the network hosts and the web client are M2–M6. See
> [`docs/PLAN.md`](docs/PLAN.md) for the full 60-task backlog.

---

## The one decision everything else follows from

**The rules are written once, in Rust, and compiled twice.**

`crates/sim` is built to `wasm32-unknown-unknown` for client-side prediction and
to native for the authoritative server. There is exactly one implementation of
movement, combat, recipes and match rules, so prediction and authority agree *by
construction* rather than by two teams keeping two codebases in sync. This is
assumption **A-07** in the plan, and it is why the crate has no I/O, no threads,
no wall clock and no floating point.

Determinism is enforced, not hoped for:

| Guard | Where | What it catches |
| --- | --- | --- |
| Q16.16 fixed point (`Fixed`) | `sim::fixed` | cross-target float drift |
| Integer-only noise (no `sin`/`exp`/`pow`) | `world::noise` | libm differences between wasm and aarch64 |
| `no_float` source scan | `sim/tests`, `world/tests` | a stray `f32` reaching authoritative state |
| `aarch64` CI job | `.github/workflows/ci.yml` | "works on x86, drifts on a phone" (risk R-07) |
| Seeded `xoshiro256**`, independent streams | `sim::rng` | adding one arrow changing the terrain |

## The second decision: the client never downloads the world

The map is 256 × 96 × 256. Shipping it would be tens of megabytes and would blow
the free-tier egress budget on its own. Instead the server sends a `u64` seed,
the client regenerates the terrain locally (`world::gen`), and only the cells
that actually change are transmitted.

With the airdrop design that is a very small number: terrain is indestructible,
crates are the only placeable block, and stacking pads are 8 × 8, so the worst
case is 256 edits — roughly 400 bytes, against a 2 KB snapshot ceiling.

```
seed ──► world::gen ──► Column ──► world::mesh ──► 12-byte vertices ──► GPU
                            ▲
                            └── ChunkDiff (a handful of cells) ◄── server
```

## The third decision: the matcher is boring on purpose

The recipe system is the highest-risk part of the plan (risk **R-02**), because
it is the only place the game can be *wrong* rather than merely unbalanced. A
matcher that fires on a near-miss hands out free units; one that fires twice
hands out two. Neither is caught by "does it feel right".

So matching is exact equality on a canonical byte string:

```
cluster ──► normalise to origin ──► 4 Y-rotations ──► re-normalise, sort
        ──► pack (6 bytes/block) ──► lexicographic minimum ──► == recipe key
```

Four properties fall out of that, and each has a test:

| Property | Why it matters |
| --- | --- |
| Rotation-invariant, reflection-sensitive | a bow on its side is still a bow; a mirror is a different object |
| Exact, never subset | 7 crates is not "a sword plus one" |
| Cluster cap checked *during* the flood fill | a 64-crate wall costs 33 steps, not 64 |
| One craft per stability window | consumption and the match are the same tick |

`data/recipes.ron` is validated at load: no duplicate canonical forms (a
collision would silently make one recipe unreachable), lowest layer on `y = 0`,
single 6-connected component, cooldown > 0. Bad data is a startup panic with the
offending recipe named, not a mystery three weeks later.

Measured: **4.8 µs** to reject a 64-crate wall in release (budget 20 µs), and
the matcher runs at most six times per tick — 0.06 % of the 50 ms tick budget.

## Layout

```
crates/
  sim/          rules: fixed point, ids, rng, tuning, movement, pads,
                recipes, combat, buddies, base, match rules    (compiled twice)
  world/        voxel storage, worldgen, mesher, chunk diffs
  protocol/     wire messages                          (M2)
  room/         pure room state machine, runtime-agnostic (M2)
  client-wasm/  wasm bridge for the browser            (M3)
  host-worker/  Cloudflare Durable Object host         (M4)
  host-axum/    tokio/axum host                        (M4)
data/
  tuning.ron    every designer-facing number, single source of truth
  buddies.ron   the six buddy roles, one row each, validated at load
  blocks.ron    the 20 block types; the array index *is* the BlockId
  recipes.ron   the 14 crate shapes, validated at load
docs/
  PLAN.md       the specification this repo implements
```

`room` is deliberately runtime-agnostic. The same state machine runs inside a
Cloudflare Durable Object and inside a `tokio` task, which turns "which host do
we use" from an architectural decision into a configuration change.

## Quick start

```bash
cargo test --workspace --all-targets              # 239 tests
cargo bench -p world --bench gen                  # asserts the worldgen and mesh budgets
cargo build --target wasm32-unknown-unknown -p sim
cargo clippy --workspace --all-targets -- -D warnings

# Look at the world. Property tests say a map is legal; this says it is a place.
cargo run -p world --example ascii_map -- 0xC0FFEE 4
```

Budgets are **asserted, not printed**: worldgen must stay under 1.5 ms per
column and meshing under 1.2 ms, or the benchmark exits non-zero. Set
`WORLD_BENCH_SLACK=3` on a noisy machine.

Current numbers on an M-series laptop (release):

```
gen    0.139 ms/column   budget 1.5 ms
mesh   0.619 ms/column   budget 1.2 ms
ai     0.046 ms/tick     budget 2.0 ms   (12 buddies + 6 players, all pathfinding)
```

## What M0 and M1 cover so far

| Task | Deliverable | Status |
| --- | --- | --- |
| T-001 | workspace scaffold, pinned toolchain | done |
| T-002 | CI with an aarch64 determinism job | done |
| T-003 | Q16.16 fixed point, 512-entry sin table, `atan2` | done |
| T-004 | generational entity ids, `xoshiro256**` streams | done |
| T-005 | paletted nibble-packed 16×96×16 columns, 20-block registry | done |
| T-006 | deterministic worldgen: terrain, bases, drop sites, trees | done |
| T-007 | greedy mesher, 12-byte vertices, per-vertex AO | done |
| T-008 | sparse chunk diffs with an RLE keyframe fallback | done |
| T-009 | quantised input, swept AABB movement, 4 substeps, step-up | done |
| T-010 | crate-recipe matcher, pad placement rules, 14-recipe table | done |
| T-011 | melee cones, swept projectiles, area damage, damage events | done |
| T-012 | buddy AI: 4-state machine, budgeted A*, leash, stance toggle | done |
| T-013 | core HP, damage multiplier, instant elimination (D-02) | done |
| T-016 | `data/tuning.ron` validated against the plan | done |

Four plan acceptance criteria were changed deliberately, all recorded here
rather than quietly skipped:

- **`proptest` was removed.** The version that satisfies the workspace requires
  rustc 1.88; the pinned toolchain is 1.87. The property-style tests use a
  deterministic LCG driver instead, which is faster to build, has zero version
  risk, and is equally reproducible — the seeds are literals, so a failure is
  always replayable.
- **The 1,000-coordinate determinism test compares the height field, not whole
  columns.** Generating 2,000 columns would make one test slower than the rest
  of the suite combined. Full byte-level column equality is still asserted on a
  fixed sample; see `gen::tests::same_seed_same_column`.
- **§12.3 rows 5 and 6 collapse into one predicate.** Row 5 is "correct shape,
  floating"; row 6 is "correct shape, one crate unsupported". Under
  6-connectivity plus a uniformly solid pad floor they are the same test: a
  crate directly beneath a cluster member is face-adjacent, therefore a member,
  therefore the cluster's lowest layer *is* its minimum, so rule 3 reduces to
  "the lowest layer sits on `y = 0`". Both rows are still exercised end-to-end,
  and the general form of the rule is implemented (a future "crates fall" rule
  would need it). `R13_WARD` — whose `(1,1,1)` crate floats over the ring's hole
  — is the witness that the rule is scoped to the lowest layer and not to every
  crate.
- **T-016's "no numeric literal > 1" grep test is not implemented.** The other
  two T-016 criteria are: tuning parses at compile time and `tuning_matches_docs`
  pins it to the plan. The third is deferred because a blanket "no literal above
  1" is the wrong shape for it — `CORE_FOOTPRINT = 2`, a 3v3 team count and a
  6-player cap are *structure*, not tuning, and a lint that flags them would
  train people to add `#[allow]`. What the criterion is reaching for is "no
  designer-facing number is hardcoded", which needs a rule that can tell a named
  constant from an inline magic number. That is a source-scanning tool, and it
  belongs next to T-015's harness rather than bolted onto the rules crate.

## Combat: events, not side effects

Every hit produces a `DamageEvent` in a per-tick vector. Nothing in
`sim::combat` applies damage or decides what happens at zero HP — that is the
match loop's job. Three consumers need the same hit in three forms (a hit flash,
a material-dependent thud, a death), and emitting once and applying once is what
keeps their ordering explicit instead of inferred from a changed HP value.

| Rule | Where it comes from |
| --- | --- |
| 45° melee cone, reach measured to the target's centre | §4.8 |
| Projectiles are swept, not sampled at endpoints | 2 m/tick at 40 m/s would tunnel a wall |
| Terrain is tested before entities | so cover actually works |
| Cooldowns convert to whole ticks at load | 0.45 s → 9 ticks; two clients cannot disagree about a swing |
| No friendly fire | not specified in §4.8; in a 3v3 with AoE it is a griefing tool |

The projectile-vs-terrain test is sampled at 0.2 m rather than DDA-swept. That
cannot miss a 1 m block except by clipping a corner by less than 0.2 m, and the
failure mode is a hit that should have happened. DDA is the upgrade if a
playtester ever reports an arrow passing through a wall; the interface would not
change.

## Buddies: legible, not clever

A buddy is not a player with worse aim. It has to be *explainable*: when it
dies, the player must be able to say what it was doing and whether that was
reasonable. That constraint rules out behaviour trees, utility scoring over
time, and any memory of previous fights. What is left is a four-state machine
with seven transitions (§4.6), a target score you can predict after seeing it
once, and a leash.

```
IDLE ──owner alive──► FOLLOW ──enemy in aggro──► ENGAGE ──ally < 60% HP──► HEAL
  ▲                     ▲                          │                        │
  └─────────────────────┴──────────────────────────┴────────────────────────┘
                              any ──HP == 0──► DEAD (terminal)
```

Three things are worth knowing before reading `sim::buddy`:

| Decision | Why |
| --- | --- |
| The leash is a *filter*, not a branch | A target outside 24 m is removed from consideration before the state machine runs, so "engage → follow" covers both "the target died" and "the target ran" — one behaviour, tested once |
| Target ties break on `EntityId` | Not insertion order, not "first in the slice". Buddy positions feed the state hash, so an arbitrary tie-break is a desync |
| `Dead` has no outgoing edge | Permanent death is the whole point of the crate cost. `buddy_never_respawns` runs 10,000 ticks and asserts the id never comes back |

Pathfinding is bounded three ways — a 512-expansion budget, a search box of the
start/goal bounding box plus 4 cells, and a 56-cell distance cap. When a search
fails the buddy steers straight and gets stuck, which is a deliberate trade: a
stuck buddy is legible to the player, a stalled tick is not. The failed-search
test asserts both halves — no path *and* under 20 µs.

`data/buddies.ron` is a separate file from `tuning.ron` because it is a table
rather than a set of knobs. The cost of the split is that the loader must prove
the file is complete: a missing role would otherwise become a buddy that spawns
with zero HP and dies on its first tick. Missing rows, duplicate rows, zeroed
fields and caps above §4.7's team total are all load errors with the offending
role named.

## The base: one number does all the pacing work

10,000 HP behind a 0.35 multiplier is the whole time budget of a match. A
bare-handed player needs about 28 minutes of uninterrupted hitting; four players
with swords and a ram need 80 seconds. That gap is the game — chip damage is
real but never decisive, so the win condition is *presence at the core*, which
is what makes the airdrop schedule and the delivery trip matter at all.

Measured against §4.8's table, by simulating the damage pipeline rather than
multiplying DPS:

| Composition | Measured | §4.8 |
| --- | --- | --- |
| 1 player, fists | 27.8 min (core survives 25) | 29.8 min |
| 4 players + ram | 80.1 s | 83 s |
| 4 players + ram + 4 tier-2 buddies | 66.7 s | 67 s |

The fists row is 2 minutes faster than the table because the multiplier is
applied **per hit**: 8 damage × 0.35 is 2.8, which rounds to 3, so a fist does
2.9 effective DPS rather than 2.8. Accumulating the fraction instead would make
the core's HP depend on the order hits arrive within a tick, which is precisely
the kind of thing two clients can disagree about. The property the AC actually
needs — "fists cannot win in 25 minutes" — holds with 1,000 HP to spare, so a
rounding change that broke it would fail loudly.

**Elimination (D-02) is one function, not a sequence.** `Base::eliminate` takes
the whole team and kills every living player in a single loop, so "all on the
same tick" is a property of the call graph rather than a promise the match loop
has to keep. `PlayerLife` has exactly two variants and the test asserting that
is written as an exhaustive match — adding `Spectator` breaks the build. A
spectator mode was the alternative: it keeps losers in the room, but it costs a
spectator camera, a spectator HUD, a separate snapshot stream, and an
information-leak question about enemy positions. Dropping it removed all four.

## Content discrepancies, resolved rather than guessed

`R06_MAGE` in §4.5.3 is sketched as `###/###/###` plus `.#.` on top — 10 blocks
— while the same row states a cost of **12**. The count is stated twice (shape
and cost) against once (sketch), so the shipped shape is a 3-long ridge: 9 + 3 =
12. It is a one-line edit in `data/recipes.ron` if the sketch was the intent, and
the note is repeated there so the next reader does not have to rediscover it.

§4.8 lists a "Staff — 40 dmg AoE r=2.5" that no recipe produces and no buddy
owns, while §4.6 gives the Mage 22 damage at the same 2.5 m radius. §4.6 is the
table with a full stat row (HP, interval, range, speed, aggro, cost, cap) and is
therefore the authoritative one; `weapons.mage_bolt` ships at 22. The 40 is
unreachable, so nothing in the game can observe the difference.

The bow has no cadence anywhere in the plan. It ships at 0.70 s, which keeps bow
DPS (43) below sword DPS (56) so the sword remains the damage option and range
is the bow's compensation. It is one number in `data/tuning.ron`.

§4.6 writes the target score as `distance_weight × dist − role_weight[role] −
low_hp_bonus`, maximised. Read literally with positive weights that prefers
*distant* targets and *low-priority* roles — a sniper across the map over a
healer at your feet. The intent is unambiguous from the role ordering that
follows ("Sniper > Tower/Ram, Berserker > Healer"), so both weights are negative
in `sim::buddy` and the formula is written in its intended form:
`priority − distance + low_hp_bonus`. The weights are `ROLE_PRIORITY`, which is
the first thing to tune if buddies feel like they are shooting the wrong thing.

The T-013 acceptance criterion asks for "4 players + ram + 4 buddies,
uncontested, destroys in the 70–90 s window", but §4.8's own table gives 67 s
for exactly that composition — and adding attackers to the ram-only push cannot
make it *slower*. The table has the arithmetic behind it, so both are asserted
at their own values: 80 s for the ram push (the AC's window) and 67 s with four
buddies. If the pacing is wrong in playtest, §4.8 says to move `core.hp` or
`core.damage_mult` and nothing else; both tests recompute from tuning, so they
will follow.

## Non-goals for v1

No meta-progression, no spectator mode, no ore or resource chain, no
destructible terrain. Those are decisions D-01 … D-05 in the plan, each made
because it made the hard part smaller rather than larger — for example, airdrop
crates plus indestructible terrain cut the worst-case chunk diff from ~1.2 KB to
~400 bytes.
