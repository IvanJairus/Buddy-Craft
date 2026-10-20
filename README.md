# Buddy-Craft

A browser multiplayer voxel arena. Airdropped crates are carried to a stacking
pad and arranged into recipes, recipes spawn weapons, kits and **buddies** —
autonomous units with armed roles. Buddies die permanently, players respawn, and
the match ends when a team's core is destroyed.

> **Status: M2 (playable offline) in progress.** M0 — the simulation core, voxel
> storage, deterministic worldgen, greedy mesher and chunk-diff codec — and M1 —
> fixed-point movement (T-009), the crate-recipe matcher (T-010), combat (T-011),
> buddy AI (T-012), base damage and elimination (T-013), match rules (T-014) and
> the determinism harness (T-015) — are done. M2 has the wire contract (T-017);
> the room state machine, snapshots and bots are next, then the web client.
> See [`docs/PLAN.md`](docs/PLAN.md) for the full 60-task backlog.

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
| Golden hash fixtures, native **and** wasm | `sim/tests/golden` | a rules change nobody meant to make |
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
                recipes, combat, buddies, base, match rules, scenario
                                                             (compiled twice)
  world/        voxel storage, worldgen, mesher, chunk diffs
  protocol/     wire messages, tags, size budgets        (M2)
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
tools/
  wasm-golden.mjs  runs the wasm build of the golden test under node:wasi
```

`room` is deliberately runtime-agnostic. The same state machine runs inside a
Cloudflare Durable Object and inside a `tokio` task, which turns "which host do
we use" from an architectural decision into a configuration change.

## Quick start

```bash
cargo test --workspace --all-targets              # 284 tests
cargo bench -p world --bench gen                  # asserts the worldgen and mesh budgets
cargo build --target wasm32-unknown-unknown -p sim
cargo clippy --workspace --all-targets -- -D warnings

# Look at the world. Property tests say a map is legal; this says it is a place.
cargo run -p world --example ascii_map -- 0xC0FFEE 4

# Print the determinism sequence for a fixture; diff it against tests/golden/.
cargo run -p sim --example hash_trace -- --mode duel --seed 0x00C0FFEE
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
| T-014 | cap matrix, FFA/Teams seating, win conditions, overtime decay | done |
| T-015 | scenario driver, golden hash fixtures, native + wasm parity | done |
| T-016 | `data/tuning.ron` validated against the plan | done |
| T-017 | wire contract: 20 messages, tag byte + postcard, size budgets | done |

Five plan acceptance criteria were changed deliberately, all recorded here
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
- **T-015's fixtures commit a seed and a script, not 20,000 input frames.** The
  criterion says `(seed, [InputFrame; 20_000])` → a hash every 100 ticks, which
  implies the input array is part of the fixture. Storing 20,000 frames per
  fixture is ~1.4 MB of committed binary per file for inputs that are, by
  construction, derived from the seed. The fixture is the *seed plus the script
  that turns it into frames*, so the same replay is possible, the file is 15 KB
  of text, and a diff is readable. If a fixture ever needs hand-authored inputs
  — a specific player doing a specific thing — that is when an input array earns
  its place, and `ScenarioConfig` already takes the seed that would feed it.
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

## Match rules: three conditions that can be true at once

The win check is ordered, and the order is the design. When every core falls on
the same tick all three conditions hold, so which one is evaluated first decides
what the results screen says:

```
no cores standing        → Draw,        winner None   (mutual destruction)
no humans alive          → NoHumans,    winner None   (D-05)
exactly one core standing → LastTeamStanding, that team
```

Draw comes first because it is reachable in normal play, not just in theory:
overtime decay hits every core at once, so two turtle teams die together.
`NoHumans` comes before "last team standing" because otherwise a bot team that
outlives the humans would be declared the winner — which is the exact thing D-05
forbids. The consequence is that `winner` is `Some(_)` only when a team with a
living human is the last one holding a core.

The cap matrix is a table, not a formula. A formula would reproduce all nine rows
of §4.7 — FFA caps at 3 up to three players and 2 beyond, Teams at 5 unless there
are three teams — but it would also silently accept a tenth combination nobody
designed. An unlisted `(mode, players, teams)` is an error, and `cap_matrix()`
walks all nine rows asserting the per-team size, the cap, the total buddies and
the total entities, because those four numbers are what the server was sized
against.

`state_hash()` covers every field that can affect play, and the test walks each
one rather than sampling: elapsed, decay applied, layout, outcome, core HP,
destruction and elimination ticks, pad contents, pad seal, player life, bot
flags, and each buddy's id, role, HP, state, stance and position. A field that
affects play but not the hash is a desync no other test would catch.

## Determinism: five fields, not one

`state_hash()` answers "did the match change". It does not answer "which part
of the match changed", and at 20,000 ticks the difference matters. So a
checkpoint is five fields on one line, four of them hashes:

```
tick : state : movement : buddies : combat
```

`state` is the full match hash; the other three cover only entities that moved,
only buddy decisions, and only damage events this tick. When a fixture goes red
the reporter prints the first diverging tick, the field that diverged, and three
lines of context around it — which is the difference between a five-minute
bisect and an afternoon.

| Fixture | Room | Checkpoints | Covers |
| --- | --- | --- | --- |
| `duel` | 1v1, `0x00C0FFEE` | 201 (to tick 20,000) | no elimination ever happens |
| `teams` | 2v2, `0x00005EED` | 201 (to tick 20,000) | chip damage that never finishes a core |
| `brawl` | 3-way FFA, `0x0BADC0DE` | 143 (ends at 14,200) | elimination and an outcome mid-run |

The brawl fixture stops at 143 checkpoints because the match *ended*: once
`Outcome` is decided the scenario stops stepping. A golden file that kept
recording ticks after the match was over would be asserting the behaviour of a
dead loop.

Three things about the harness are worth recording.

**The seed was decorative, and a test caught it.** The first version of the
scenario mixed the seed into the RNG but not into the scripted inputs, so two
different seeds produced byte-identical sequences — and
`a_different_seed_produces_a_different_sequence` failed on exactly that. The
fix is that `script` is a method on the scenario rather than a free function: it
now derives direction phase, yaw and jump cadence from `self.seed`. Without the
guard test the harness would have looked fine and proved less than it claimed —
a golden that is the same for every seed is testing the script, not the seed.

**Wasm parity runs under Node, not a new runtime.** The wasm target is
`wasm32-wasip1` and `tools/wasm-golden.mjs` runs the test binary through
`node:wasi` with `/` preopened, so the same binary reads the same committed
`.hash` files. That is a CI job (`wasm-golden`) with no extra dependency beyond
the Node that `actions/setup-node` already installs — `wasm-bindgen`,
`wasm-pack` and a headless browser were all avoidable because the test needs a
filesystem and nothing else. Release run: 7 tests, 1.80 s, byte-identical to
native.

**The float mutation was verified twice, because the first attempt proved
nothing.** T-015's criterion asks that a deliberate `f32` mutation in
`movement.rs` makes the suite fail. Inserting `let _ = speed;` into the velocity
computation left the golden green — correctly, since the value was discarded and
nothing observable changed. The real experiment replaced the velocity with
`Fixed::from_f32(cfg.speed.to_f32() * 0.999)`, which made **both** detectors
fire: `no_float` rejected the source scan and the golden diverged at tick 100 in
`movement` for all three fixtures. Reverted immediately; the lesson is that a
mutation test has to change a value that reaches the hash, not just reference
one.

`UPDATE_GOLDENS=1 cargo test -p sim --test golden` rewrites the fixtures. It is
for a rule change that was made on purpose, never for making a red test green —
the diff *is* the record of what changed, and reviewing it is the point.

## The wire: one tag byte and no schema

Twenty messages, nine up and eleven down. One message per WebSocket frame, first
byte is the tag (`0x01`–`0x09` client→server, `0x81`–`0x8B` server→client), the
rest is postcard. No length prefix, no compression, no field names.

The high bit separates the directions, which is worth one assertion: a frame's
direction is readable from its first byte alone, in a log, a proxy or a fuzz
corpus, without knowing which side sent it.

| Decision | Why |
| --- | --- |
| A tag byte in front of postcard, not a postcard enum | postcard encodes an enum variant as its *index*, so the tags would be 0, 1, 2 … and adding a variant in the middle would silently renumber the wire |
| Derived encoding everywhere, no `#[repr(C)]` | §7 asks for hand-written `Serialize` where byte layout matters. Pinning fields to fixed widths is exactly what the snapshot budget cannot afford — see below |
| Enumerated values are `u8` fields, not Rust enums | an unknown value from a newer peer must be *representable* so the server can answer with a reason instead of failing to decode the whole message |
| `FixedBytes<N>` for name, room code, token, chat, error detail | serde's array impls stop at 32 elements; and a fixed array is a bound the *type* enforces, so the decoder cannot be asked for a 4 KB chat buffer before validation runs |

`FixedBytes` serialises as a tuple, so there is no length prefix: 120 bytes of
chat costs 120 bytes. Its `Deserialize` reads exactly `N` bytes and fails on a
short frame, which is why a truncated payload is a `DecodeError::Payload` rather
than a partially-filled struct.

### The input frame is 4 bytes, not 8

§7.1 budgets 30 bytes for `C_INPUT { seq, first_tick, frames: [InputFrame; 3] }`,
which works out to 8 bytes per frame: a `u32` tick plus four `u8`s. The shipped
frame has no tick. `first_tick` already implies every frame's tick (`frames[i]`
applies to `first_tick + i`), so shipping both would give each frame **two
sources of truth for the tick it applies to** — a client that sends
`first_tick = 100` and `frames[1].tick = 105` would be describing two different
matches, and the server would have to pick one. The measured message is 19 bytes
at `seq = 65535`, and inputs are the only steady-state upstream traffic, so the
saving is 270 B/s per player instead of 450.

### The size column is a ceiling, not a target

`sizes_match_spec` asserts every message encodes **at or below** its §7.1/§7.2
size, measured at the largest values the protocol can carry — a roundtrip at
`seq = 65535` proves more than one at `seq = 77`, so the same list feeds both the
roundtrip and the size test. A ceiling rather than a ±10% band, because failing
the build for encoding *smaller* than the table would be backwards on a metered
free tier.

Three rows cannot meet their number, and each correction comes with the
arithmetic rather than an apology:

| Tag | §7 says | Actual | Why the spec number is unreachable |
| --- | --- | --- | --- |
| `C_REPORT` | 7 B | 10 B | `kind u8, tick u32, hash u32` is 1 + 1 + 4 + 4 = 10 even at fixed width; 7 matches a `u16` hash. The hash stays `u32` — a 16-bit desync hash collides once per 65,536 reports, which is not enough resolution to bisect a desync with |
| `S_START` | ~40 B | 47 B | Six teams is §4.7's maximum (FFA, six players): 1 + 3 + 1 + 6 × 7 = 47 |
| `S_EDIT_ACK` | 5 B | 6 B | 5 B holds while `seq < 16,384`. §6.6 allows 8 edits/s and a match can run 47 minutes, so `seq` reaches 22,560 and needs a three-byte varint |

`overridden_sizes_are_provably_impossible` asserts each of those three payloads
is *larger than the spec's number by construction*. If a later change brings one
back inside its budget — a `u16` hash, fewer teams — the test fails and tells you
to delete the override, so the exceptions cannot outlive their reasons.

Everything else has room: `S_SNAPSHOT`'s worst case (6 players, 12 buddies, 24
projectiles, 4 structures, 16 events, all fields present, all moving) is **615 B**
against a 2,048 B budget and a 550 B average, which is what makes a keyframe every
three seconds cheap. `S_LOBBY` at six players is 119 B against 140.

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

**Overtime decay cannot terminate a stalemate inside 25 minutes.** §12.4 asks
for "100 random bot matches all end ≤ 25 simulated minutes" and §4.2 calls decay
the thing that "guarantees termination". The shipped rate is 2% of max HP per
30 s starting at 22:00, which needs 50 steps — 25 minutes of overtime — so a
full-health stalemate ends at **47:00**, not 25:00. Getting a stalemate to end
at 25:00 would need either ~16.7% per 30 s or overtime from the first tick.

The tests assert what the rules actually guarantee rather than the number in the
criterion. `match_always_terminates` runs 100 seeded matches with random room
shapes and damage rates and asserts two things: every match ends within the
decay bound, and every match that *can* be decided by combat ends within 25
minutes (measured: 23.0 min worst case). Matches with two or more unkillable
teams are counted separately — 14 of the 100 — and asserted to end as a draw on
the 47-minute bound. If the 25-minute ceiling is meant to be hard, the knob is
`match.decay_per_30s`, and both tests recompute from it.

## Non-goals for v1

No meta-progression, no spectator mode, no ore or resource chain, no
destructible terrain. Those are decisions D-01 … D-05 in the plan, each made
because it made the hard part smaller rather than larger — for example, airdrop
crates plus indestructible terrain cut the worst-case chunk diff from ~1.2 KB to
~400 bytes.
