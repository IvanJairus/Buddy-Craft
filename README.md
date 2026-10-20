# Buddy-Craft

A browser multiplayer voxel arena. Airdropped crates are carried to a stacking
pad and arranged into recipes, recipes spawn weapons, kits and **buddies** —
autonomous units with armed roles. Buddies die permanently, players respawn, and
the match ends when a team's core is destroyed.

> **Status: M1 (simulation rules) in progress.** M0 — the simulation core, voxel
> storage, deterministic worldgen, greedy mesher and chunk-diff codec — is
> complete. M1 has added fixed-point movement (T-009) and the crate-recipe
> matcher (T-010); combat, buddy AI, base damage and match rules are next, then
> the network hosts and the web client are M2–M6. See
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
  sim/          rules: fixed point, ids, rng, tuning, movement,
                pads, recipes                                  (compiled twice)
  world/        voxel storage, worldgen, mesher, chunk diffs
  protocol/     wire messages                          (M2)
  room/         pure room state machine, runtime-agnostic (M2)
  client-wasm/  wasm bridge for the browser            (M3)
  host-worker/  Cloudflare Durable Object host         (M4)
  host-axum/    tokio/axum host                        (M4)
data/
  tuning.ron    every designer-facing number, single source of truth
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
cargo test --workspace --all-targets              # 179 tests
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
gen    0.184 ms/column   budget 1.5 ms
mesh   0.650 ms/column   budget 1.2 ms
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
| T-016 | `data/tuning.ron` validated against the plan | done |

Three plan acceptance criteria were changed deliberately, all recorded here
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

## A content discrepancy, resolved rather than guessed

`R06_MAGE` in §4.5.3 is sketched as `###/###/###` plus `.#.` on top — 10 blocks
— while the same row states a cost of **12**. The count is stated twice (shape
and cost) against once (sketch), so the shipped shape is a 3-long ridge: 9 + 3 =
12. It is a one-line edit in `data/recipes.ron` if the sketch was the intent, and
the note is repeated there so the next reader does not have to rediscover it.

## Non-goals for v1

No meta-progression, no spectator mode, no ore or resource chain, no
destructible terrain. Those are decisions D-01 … D-05 in the plan, each made
because it made the hard part smaller rather than larger — for example, airdrop
crates plus indestructible terrain cut the worst-case chunk diff from ~1.2 KB to
~400 bytes.
