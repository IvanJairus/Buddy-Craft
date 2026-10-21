# Buddy-Craft

A browser multiplayer voxel arena. Airdropped crates are carried to a stacking
pad and arranged into recipes, recipes spawn weapons, kits and **buddies** —
autonomous units with armed roles. Buddies die permanently, players respawn, and
the match ends when a team's core is destroyed.

> **Status: M2 (playable offline) in progress.** M0 — the simulation core, voxel
> storage, deterministic worldgen, greedy mesher and chunk-diff codec — and M1 —
> fixed-point movement (T-009), the crate-recipe matcher (T-010), combat (T-011),
> buddy AI (T-012), base damage and elimination (T-013), match rules (T-014) and
> the determinism harness (T-015) — are done. M2 has the wire contract and its
> validation (T-017, T-018), the room lifecycle and tick loop (T-019, T-020),
> player bodies (P-01), the per-client snapshot encoder (T-021), the block edit
> pipeline (T-022), the airdrop economy (T-025), the headless match loop (P-02),
> the bots that play it (T-023), the whole-match invariant harness (T-024), the
> wasm boundary with its render buffer (T-030), the three.js renderer with its
> quality tiers and adaptive resolution (T-031) and chunk streaming with real
> wasm meshing (T-032), instanced entities with GPU-lifetime particles
> (T-033), the block interaction raycast with its previews (T-034), desktop
> input with pointer lock (T-035), touch input with explicit modes (T-036), the
> HUD with its 10 Hz store (T-037), the recipe book with Rust near-miss (T-038)
> and the lobby/settings shell (T-039). Client networking and the offline
> match loop follow.
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
                recipes, combat, buddies, base, match rules, airdrops,
                scenario                                        (compiled twice)
  world/        voxel storage, worldgen, mesher, chunk diffs
  protocol/     wire messages, tags, size budgets        (M2)
  room/         pure room state machine, match loop and bots (M2)
  client-wasm/  wasm boundary: state exports, recipe helpers, buffer (M2)
  host-worker/  Cloudflare Durable Object host         (M4)
  host-axum/    tokio/axum host                        (M4)
apps/
  web/src/main.ts          boot benchmark, wasm init, chunk loop (M2)
  web/src/render/          renderer, tiers, chunks, entities, particles (M2)
  web/src/game/            interaction, desktop and touch input (M2)
  web/src/ui/              store, HUD, recipe book, screens, settings (M2)
  web/src/wasm/loader.ts   raw wasm loader and render views (M2)
  web/scripts/             wasm bridge build step (M2)
  web/tests/               Playwright specs for the browser contract (M2)
data/
  tuning.ron    every designer-facing number, single source of truth
  buddies.ron   the six buddy roles, one row each, validated at load
  blocks.ron    the 20 block types; the array index *is* the BlockId
  recipes.ron   the 14 crate shapes, validated at load
docs/
  PLAN.md       the specification this repo implements
tools/
  wasm-golden.mjs  runs the wasm build of the golden test under node:wasi
  wasm-exports.mjs checks the bridge's 13-function export contract
fuzz/
  fuzz_targets/decode.rs  libFuzzer harness for the decoder (not a workspace member)
```

`room` is deliberately runtime-agnostic. The same state machine runs inside a
Cloudflare Durable Object and inside a `tokio` task, which turns "which host do
we use" from an architectural decision into a configuration change. Transport
and gameplay are separate: `Room` owns lifecycle, input queues and movement,
while `room::play::Match` owns airdrops, edits, combat, crafting, buddies and
elimination, and takes input frames as a value. The host composes the two, and
tests use the second without a runtime.

## Quick start

```bash
cargo test --workspace --all-targets              # 457 Rust tests
cargo bench -p world --bench gen                  # asserts the worldgen and mesh budgets
cargo build --target wasm32-unknown-unknown -p sim
cargo clippy --workspace --all-targets -- -D warnings

# Client (first run also needs `npx playwright install chromium`)
cd apps/web && npm ci && npm run test:e2e          # 41 specs, 1 headed (pointer lock)

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

## What is done so far

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
| T-018 | `decode_checked`, name/chat sanitisers, token buckets, fuzz target | done |
| T-019 | room lifecycle: six edges, one terminal state, no clock | done |
| T-020 | tick loop, per-player input queue, 200 ms AFK timeout | done |
| P-01 | player bodies: spawn search, input application, death, respawn | done |
| T-021 | per-client snapshots: 48 m AOI, acked-baseline deltas, 2 KB budget | done |
| T-022 | edit pipeline: reach/existence/protection/rate, diff log, pad+voxel in sync | done |
| T-025 | airdrops: schedule, integer site pick, carry, deposit, conservation | done |
| P-02 | match loop: joins airdrops, edits, combat, crafting, buddies and elimination | done |
| T-023 | bots: collect, build, fight, push a core, driven only by inputs and edits | done |
| T-024 | whole-match harness: eight invariants per tick, reconnect, three-match leak probe | done |
| T-030 | wasm boundary: state exports, byte-stable render buffer, zero-allocation tick | done |
| T-031 | three.js WebGL2 bootstrap, exact quality tiers, adaptive resolution, boot benchmark | done |
| T-032 | chunk streaming: 6/7 hysteresis, neighbour meshing, 88 chunk draws, 34 KB leak | done |
| T-033 | instanced 3-part entities, float-driven animation, 256 GPU-lifetime particles | done |
| T-034 | DDA voxel raycast, 6 m reach, wireframe highlight, ghost preview, pending edits | done |
| T-035 | desktop input: 4-byte frame, pointer lock, hotbar, blur release, persisted settings | done |
| T-036 | touch input: stick/look multi-touch, mode taps, 6/s deposit hold, stance, 44 px controls | done |
| T-037 | HUD core: 10 Hz store, transform-only bars, textContent, 5-node feed, measured cost | done |
| T-038 | recipe book from the wasm table, locked tiers, Rust near-miss, ghost overlay | done |
| T-039 | shell: Boot/Main/Lobby/Match/Results, room-code validation, persisted settings and remap | done |

The plan is followed except where it is wrong or silent, and every such case is
recorded here rather than quietly skipped:

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
- **T-018's `cargo fuzz` run could not be executed in the environment this was
  built in.** The harness is committed, builds clean, and is compiled by CI on
  stable so it cannot rot — but the fuzzing binary itself is killed by this
  machine before it completes a single run, so "no panic and no OOM in 60
  seconds" is unverified here. What *is* verified is the same property without
  libFuzzer: `codec::tests` sweeps 20,000 random frames, every single-byte
  substitution and bit flip of five valid frames, and every truncation of them,
  asserting the decoder returns rather than panics. That sweep is deterministic,
  runs in CI on the pinned toolchain, and is a subset of what the fuzzer would
  explore — a fuzzer's advantage is coverage guidance, not the property. Run
  `cargo +nightly fuzz run decode -- -max_total_time=60` before trusting the
  decoder with a public endpoint.
- **T-020's `TickOutput.outcome` was `None` on the tick the match ended**, and a
  three-frame packet covers two future ticks rather than three. Both are
  described in "The tick loop" above; they are test expectations that were
  wrong, not rules that were changed.
- **P-01 is not a plan task.** The plan calls `sim::step_local(state, input)` in
  §6.4 but no task creates it, so T-021's snapshots, T-022's reach checks, T-023's
  bots and T-024's full match all depended on something the task list never
  scheduled. It is numbered `P-01`, prerequisite one, so the gap is visible in
  the table instead of being smuggled into T-021.
- **P-02 is not a plan task either.** The list goes from edits and airdrops
  straight to T-023 (bots) and T-024 (match harness), with no task that joins
  airdrops, movement, edits, combat, crafting, buddies and elimination into one
  tick. `room::play::Match` is that join, and it is numbered P-02 in the table.
  It lives beside `Room` rather than inside it because transport and gameplay
  have different tests: a socket can drop a player, while a match only sees the
  frames that arrived.
- **Worldgen trees got taller in T-023.** Terrain is indestructible, and the
  canopy used to start one block below the trunk top, which could put leaves
  within a player's height of sloping ground. A bot that pathed into such a
  pocket could not jump out and stayed there for the match. Trunks are now 7 to
  9 blocks and the canopy starts at the top. Trees are scenery, so this is a
  clearance fix, not balance.
- **`DropSite::y` is the surface block, not the crate's cell.** Worldgen reports
  the same height `height_at` reports, while a crate and a player's feet occupy
  the air cell above it. P-02 adds one when it builds the site list. Without
  that, the first airdrop landed inside the top terrain block and no pickup
  could ever reach it; no `sim` test could see this because `sim` receives sites
  that are already positioned.
- **The T-015 fixture *script* changed, and that is a real rule change.** The
  fixtures are regenerated in this commit because bodies are now hashed and
  because the script aims at its target instead of walking in fixed compass
  directions. The old files recorded 20,000 ticks in which no player ever hit
  another; keeping them byte-identical would have meant keeping the blind spot.
  The diff is the record: `state`, `movement` and `combat` all move at tick 0, and
  the checkpoint counts (201/201/143) are unchanged, so no match got shorter.
- **The goldens were regenerated a second time in T-021**, because `PlayerBody`
  gained `yaw` and `pitch` and both are hashed: facing is state, and a field that
  is not hashed is a desync no fixture would catch. Every checkpoint in all three
  fixtures changes, which is the honest signature of "a field was added to the
  hash" rather than a rules change — the movement, buddies and combat fields are
  untouched at tick 0 and the wasm build produces the same new sequence.
- **T-016's "no numeric literal > 1" grep test is not implemented.** The other
  two T-016 criteria are: tuning parses at compile time and `tuning_matches_docs`
  pins it to the plan. The third is deferred because a blanket "no literal above
  1" is the wrong shape for it — `CORE_FOOTPRINT = 2`, a 3v3 team count and a
  6-player cap are *structure*, not tuning, and a lint that flags them would
  train people to add `#[allow]`. What the criterion is reaching for is "no
  designer-facing number is hardcoded", which needs a rule that can tell a named
  constant from an inline magic number. That is a source-scanning tool, and it
  belongs next to T-015's harness rather than bolted onto the rules crate.
- **§6.3's "own player: always, full fidelity" is read as scope, not as
  encoding.** The table's column header is *Scope*, and the row means "your own
  body is never culled". Sending it whole on every delta frame — the literal
  reading — makes the own-player entry absolute while every other entry is a
  delta, which is a decoder trap: a client that adds deltas doubles its own
  position. The gate test (`delta_chain_reconstructs_the_state`) failed on
  exactly that, so the encoder gives every entity one rule. The cost is a few
  bytes per frame; the benefit is that the client has no special case for its own
  body, which is the entity prediction depends on.
- **The edit token bucket holds 8, not §11.3's burst of 12.** T-022's criterion
  says the ninth edit in one tick is refused, which only holds with a capacity of
  8. The stricter number wins: a burst of 12 in a 50 ms tick is an instantaneous
  240/s, nothing in the game needs it, and the only thing a wider burst buys is a
  wider hole in the flood defence. The refill rate is 8/s either way.
- **The airdrop schedule produces 73 drops in 20 minutes, not ~59.** §4.4.1's
  two intervals (25 s until 5:00, then 15 s) give 12 early drops and 61 late
  ones, and no pair of intervals produces 59 while still matching the rows above
  it. The per-drop rate is what players experience and what the crate totals are
  derived from, so 73 ships; the summary's 354/1,062 crates become 508/1,510.
  Both the schedule and the throughput tests pin the computed numbers.
- **"First drop 0:10" means it spawns at 0:10, not lands at 0:10.** §4.2 says the
  first airdrop "lands at 0:10" while §4.4.1 and T-025's criterion say the first
  drop is at 0:10. A drop that lands at 0:10 would have to spawn at 3.3 s to
  cover the 6.7 s descent, which contradicts the telegraph and the criterion.
  Spawn wins; touchdown is at 0:16.7, still inside the 0:00–1:30 opening.
- **The 5 s telegraph is a floor, not the descent length.** 80 m at 12 m/s is
  6.7 s, so a marker that appeared 5 s before impact would have to appear after
  the drop is already falling. The drop is announced at spawn and the descent is
  the warning; the constant remains the floor for any shallower site.
- **Landing scatter is a ±2 m square.** A disc of 3 m needs a square root or a
  rejection loop per crate; the square's 2.83 m diagonal satisfies "within 3 m"
  and is reached in one fixed-point draw per axis.
- **T-025's "never selects a base pad" is a property of the candidate set, not
  the pick.** `Airdrops::new` takes the sites worldgen produced and trusts them
  the same way movement trusts a `VoxelQuery`; `world::gen`'s
  `drop_sites_are_flat_and_legal` is the test that keeps pads out. Duplicating
  the pad geometry inside the airdrop module would give two definitions of
  "legal site".
- **§12.4's conservation equation double-counts a placed crate.** "dropped −
  consumed − destroyed == carried + on ground + placed" subtracts consumption
  and then adds placement, which is the same crate twice. The shipped ledger
  puts each crate in exactly one bucket: `dropped == destroyed + deposited +
  carried + on_ground + falling`. T-024 adds a separate reconciliation that
  does check the plan's intent, after crafting and pad sweeps.
- **T-024's entity ceiling is read as actors, not as every prop.**
  `18 + 24 + 4` maps to the networked actor budget, the buddy ceiling and the
  drops in the air. The 120 floor pickups have their own cap in §4.4.3 and are
  counted separately; folding them into 46 would make the two rules
  contradict each other for no benefit.
- **T-030's file list assumed two things this repo did not have.** It names
  `client-wasm/src/lib.rs`, `render_buffer.rs` and `loader.ts`, but the crate had
  no `wasm-bindgen` dependency or cdylib target, and `apps/web` was never
  created (T-001's web skeleton was skipped). The bridge adds the dependency and
  `crate-type = ["cdylib", "rlib"]`, creates only the wasm loader, and leaves
  the rest of the app to T-031. Raw wasm export names carry wasm-bindgen's hash
  suffix, so `tools/wasm-exports.mjs` checks the 15 base names and CI runs it.
  The wasm-bindgen CLI is not part of the build: the raw loader instantiates the
  module directly and stubs its four unused imports. A future export that takes
  a string or an object would need the glue back; none does today.
- **T-031 had to build the web scaffold the plan assumed.** T-001's file list
  includes `apps/web/package.json`, but this repo never created it. T-031 adds
  the package manifest, tsconfig, `index.html`, Playwright config and specs,
  because the acceptance criteria cannot run without them. It uses npm rather
  than the plan's pnpm (pnpm is not installed here); the CI `web` job is now an
  npm build plus Chromium Playwright run instead of a no-op. The T-031 file list
  names three source files; everything else is the minimum needed to execute
  the specs.
- **T-032's file list omitted the meshing bridge and its build pipeline.** The
  task names `chunkManager.ts`, but `cb_mesh_chunk` was a T-030 stub returning
  zero, and the app had no way to build or load the wasm. T-032 adds the
  worldgen and mesher path in `client-wasm`, a raw wasm loader, a build script
  that copies the release artifact into `apps/web/src/wasm`, and the streaming
  specs. The wasm file itself is generated and ignored; the web CI job installs
  Rust, the wasm target and Node, then runs `npm run build` before Playwright.
- **The raw loader does not accept a tuning override yet.** `cb_init` still
  validates the tuning blob when one is supplied, but before init there is no
  bridge and no arena to copy the blob into, so the JS loader passes 0/0 and
  uses the embedded table. The app ships the same `data/tuning.ron`, so this is
  not a behavioural gap today; a pre-init allocation export is the clean fix if
  overrides become a feature.
- **T-033's file list omitted the bridge flag and the app wiring.** The task
  names `entities.ts` and `particles.ts`, but the attack window needed a bit in
  `GpuPlayer` (bit 2, no layout change), and the renderer needed construction
  and test hooks in `main.ts`. Death dissolve also reuses `anim` rather than
  adding the extra float §8.3 mentions: the 32-byte stride is pinned by offset
  tests, and adding a seventh float would move every field after it. The
  convention is stated in both the section above and the module docs.
- **T-034 needed a voxel source the bridge does not provide.** The task's file
  list names only `interaction.ts`, but none of the bridge exports answers
  "what block is at (x, y, z)?". The mesh payload grew a
  256-byte height map per column, and the chunk manager implements
  `VoxelSource` over it. That is a surface, not a full column mirror: caves and
  the air under a canopy are approximate. T-042 owns the real voxel state; the
  interaction does not need to change when it lands.
- **T-035's pointer-lock spec cannot run in the headless shell.** Chromium's
  headless build refuses `requestPointerLock` even from a trusted click, so the
  spec is a headed Playwright project and CI runs the suite under Xvfb. The
  other specs stay headless. The task's file list also names only
  `inputDesktop.ts`; the loop wiring and the poll/tick counters live in
  `main.ts`.
- **T-036's "place button" is the block-mode world tap.** The plan names a
  place button, but §8.6's control table has no separate tap-to-place button:
  the selected slot is the mode and a tap on the world acts. Holding that same
  touch repeats at the deposit rate, so one gesture means one thing. The task's
  file list also names only `inputTouch.ts`; the control surface lives there
  because T-037's HUD is later, and `main.ts` gained the touch/desktop switch
  and the touch-to-interaction callbacks. The stance is local until the network
  layer can apply it to every buddy.
- **T-037 uses a 100 ms timer, not a frame callback.** §8.7 describes the store
  batching writes into a `requestAnimationFrame`; the acceptance criterion
  names a 100 ms sample for 20 Hz values and a hard 10 per second ceiling. A
  frame callback at 60 Hz is a second throttle around the same work, so the
  timer is the simpler exact bound. The initial paint is excluded from the 1 ms
  measurement for the reason given in the section above; boot has its own
  budget.
- **The HUD is fed demo values until the match stream exists.** HP, crates and
  core bars initialize to their documented starting values; the clock and drop
  countdown are local; stance is wired to both input paths. This is the store's
  contract, not a hidden match implementation, and the test hooks drive the
  same store a network loop will.
- **T-038 grew the wasm bridge to 15 exports.** T-030's criterion pinned the
  bridge at exactly 13, and that was true when T-030 shipped. The recipe book
  needs the same table and the same near-miss logic the matcher uses, and §5.1
  forbids reimplementing recipe matching in JS, so `cb_recipes` and
  `cb_near_miss` are two read-only helpers. The state API is unchanged;
  `tools/wasm-exports.mjs`, CI and this README now pin 15.
- **Near-miss is "missing blocks ≤ 2", not byte Hamming distance.** A byte
  comparison of canonical keys does not yield a "needs N" count, and a cluster
  with an extra block cannot be fixed by adding a missing one. The implemented
  metric is the useful subset: the placed cluster is a subset of a recipe and
  is 1 or 2 blocks short. `near_miss_never_lies` completes the prediction and
  runs the real matcher over 500 random clusters, so a false "needs N" cannot
  pass.
- **The recipe book's CSS is a third file.** The task names two TypeScript
  files; the panel needs layout styles, so `recipeBook.css` is added. The
  prediction and the ghost are not yet wired to pad state; their APIs are
  exercised by the page specs and T-042/T-048 will feed them.
- **The lobby is local until T-040.** The room code, copy button and join
  validation are real and tested; Start enters the demo match because there is
  no WebSocket client yet. The screen manager is the part T-040 plugs into, not
  a second networking layer.
- **The UI scale applies to menu panels, not the in-match touch controls.** A
  0.8 zoom on a 44 px target would make it 35 px, which contradicts T-036's
  target gate. Menus scale through `--cb-ui-scale`; the in-match controls keep
  their physical size until the accessibility pass can scale targets and layout
  together.
- **The match and results are not connected to a real outcome.** LEAVE MATCH is
  a deliberate route so the Results screen is reachable and the shell has no
  dead end. T-048 will replace the leave action with the match-end event.
- **The key map is one code per action, not a chord model.** The ten controls
  each get one keyboard code; modifier combinations would need an array or a
  chord type. The JSON format is versioned by its own key, so a later chord
  model can migrate without guessing.
- **`window.__cbTest` ships in the production bundle.** The synthetic
  frame-time injector has to reach the same adaptive code the player runs, so
  gating it to a dev build would test a different path. It only calls renderer
  setters and reads state; it cannot mutate game state or open a socket. If the
  client ever runs against untrusted peers, revisit whether the hook should be
  query-gated.
- **`no_entity_leak` runs the same seed three times, not three seeds.** A
  growing entity count would show up in any run, but a leaked global shows up
  only as a *divergence* between two runs of the same input. Three identical
  hash sequences are the stronger proof, so that is what the test asserts.
- **`protocol::DiffEntry.index`'s doc comment was wrong and is fixed.**
  It said `x | y << 4 | z << 12`; `world::Column::index` — the only producer of
  these entries — packs `x + 16 * (z + 16 * y)`. Both are permutations of the
  same three fields, so the wire format never changed, but a client written
  against the old comment would have decoded every crate to the wrong cell. No
  test could catch that until T-030 exists, which is exactly why the comment now
  points at the encoder as the authority.

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

## Validation: four outcomes, not two

`C2s::decode` answers "can these bytes be a message". `decode_checked` answers
"will this server act on it", and the difference is a second axis: *what happens
to the connection*.

| Failure | Response | Why |
| --- | --- | --- |
| Frame over 4 KB | close, code 1009 | checked before the payload is touched, so a 4 MB frame is never parsed |
| Unknown tag, or a payload that does not match it | close, code 1008 | a malformed stream is a bug or an attack; both deserve termination, and "ignore and continue" turns one desynced byte into a permanently desynced connection |
| Token bucket empty | close, code 1008 | §7.4 rule 6 |
| Bad name, empty chat, out-of-bounds edit | reject, connection stays open | aiming at the sky is something players do |

The last row is the one worth the extra type. §7.4 rule 4 says an out-of-bounds
edit is answered with a reason rather than a disconnect, so `Error` has a
`close_code() -> Option<u16>` and the caller closes only when it is `Some`.

**Sanitisation returns a message, not a verdict.** `decode_checked` hands back a
`C2s` whose name and chat are already canonical. A function that only *reported*
"this is dirty" would leave the caller holding the dirty copy, and the next
person to add a caller would have to remember to clean it. Returning the clean
value makes the sanitised form the only one that escapes the codec — and makes
the property testable as "decode twice, get the same thing".

`<img src=x onerror=alert(1)>` becomes `img srcx onerroralert1`: every byte that
could open a tag, close a tag or break out of an attribute is dropped, and what
is left renders as text in any client. Disallowed bytes are *dropped* rather than
replaced with spaces, because a message of 120 punctuation marks becoming 120
spaces is neither what the sender typed nor obviously nothing.

**The token bucket is integer.** Millitokens, refilled per tick from the tick
delta, because a float refill would let two connections that arrived in a
different order get different verdicts on the same frame — which is how "the game
kicks me on my phone" becomes a week of debugging. A tick that goes backwards
grants nothing: `saturating_sub`, not a wrapping difference, because ticks wrap
by design and a wrapped tick is not 6.8 years of credit.

`RateLimiter::messages()` is §7.4's 30/s with a burst of 60; `RateLimiter::edits()`
is §6.6's 8/s with a burst of 8, so a player cannot bank a minute of edits and
spend them in one tick. The tests assert the bucket buys *exactly* its configured
rate over a second, and that 15 messages/s for ten seconds drops nothing.

## The room: six edges and one terminal state

§6.10 is a diagram of five phases and six arrows. It is implemented as exactly
that — a `can_enter` table, not a set of `if`s:

```
Lobby     → Countdown      everyone ready, ≥2 players
Lobby     → Cleanup        empty for 60 s
Countdown → Lobby          someone unreadied, or dropped below 2
Countdown → Live           after 60 ticks
Live      → Ended          the match rules report an outcome
Ended     → Cleanup        after 200 ticks
```

`illegal_transitions_are_rejected` walks all 25 ordered pairs, including
self-transitions, and asserts that exactly six succeed — and a second test spells
the six out independently, so adding an edge to the implementation without
deciding to is a test failure rather than a quiet behaviour change. Every
transition is funnelled through one method, so "is this legal" has one answer,
and a caller that assigns the phase directly does not compile.

**The host makes zero decisions.** `update(now, conditions)` returns an `Effect`:
`BeginCountdown`, `CancelCountdown`, `BeginMatch`, `EndMatch`, `Cleanup`. The
host shows a timer, emits `S_START`, steps the simulation — it never decides
*when*. That is what lets the same state machine run inside a Durable Object and
inside a `tokio` task, and it is why the whole lifecycle is tested in
microseconds with no mocking: time is a `sim::Tick` parameter.

Three things are worth recording.

**`Cleanup` fires exactly once, and the test proves it by running three times
long.** A Durable Object that persisted on every tick after cleanup would write
the same row twenty times a second forever. `Cleanup` is emitted once because
`Cleanup` has no outgoing edges and `update` returns `None` in a terminal phase —
the property is structural, not a flag someone has to remember to check.

**§6.10 draws an "empty 60 s" arrow out of `Ended` that can never fire.** The
results window is 10 s and the empty-room window is 60 s, so the results timer
always wins. The empty timer is still tracked (it costs one `Option<Tick>`), but
the redundant edge is recorded as a `const _: () = assert!(ENDED_TICKS <
EMPTY_TICKS)` rather than as a test that would fail if someone *fixed* the
diagram.

**There is no `Live → Cleanup` edge, and that is correct.** A live room with
nobody in it still has a match, and the match ends it — `check_outcome` returns
`NoHumans` (D-05) — before the empty-room rule could apply. If that ever stops
being true, the match rules have a bug that a cleanup timeout would paper over,
so the missing edge is a guard rather than an omission. `a_live_room_is_never_cleaned_up_directly`
runs 2,400 ticks of an empty live room and asserts nothing happens.

`tests/no_clock.rs` reads this crate's own source and fails the build if
`std::time`, `Instant`, `async fn`, `.await` or `tokio` appears in it — comments
and the test module stripped first, because a doc link is not a dependency. The
guard was verified by adding `std::time::Instant::now()` to `lib.rs`, watching it
fail on both tokens, and reverting.

## The tick loop: 15 Hz in, 20 Hz out

A client sends **three consecutive frames, fifteen times a second**, against a
**20 Hz** simulation (§6.2). Those numbers are chosen so that one lost packet
costs nothing — the next packet re-sends the frames that were in the lost one —
and the queue's job is to turn that stream into exactly one frame per player per
tick. Three questions, three answers:

| Question | Answer | Why not the obvious alternative |
| --- | --- | --- |
| Which frame applies to this tick? | the newest one whose tick is **not in the future** | "newest received" would let an early packet skip a tick |
| Nothing arrived this tick? | reuse the last frame | 15 Hz into 20 Hz leaves ticks with no packet; letting go every one of them makes a 15 Hz client stutter |
| Nothing for 200 ms? | hold **nothing** | "still holding forward" runs a disconnected player into a wall and gives a lagging one free movement |

A frame for a tick the simulation has already passed is **dropped on arrival**,
not applied. That is what stops a resent packet from rewinding, and it is why
`stale_input_is_dropped` pushes a packet for ticks 9–11 after the clock has
reached 12 and asserts the resolved input is unchanged.

The queue is a fixed 6 × 12 array of `Option<Queued>`: no allocation, no
`HashMap`, no iteration-order dependency. Twelve frames per player is about nine
packets of lookahead, far more than the ~200 ms a client can legitimately run
ahead, and when it overflows the *oldest* frame is dropped rather than the newest.

### Two things this found

**`TickOutput.outcome` was `None` on the tick the match ended.** The first version
reported the outcome only while `Phase::Live`, so the tick that transitioned to
`Ended` — the one where the host builds `S_END` — carried nothing. A host that
stops ticking on `EndMatch` would never have seen the winner. It now reports the
outcome in both `Live` and `Ended`.

**A three-frame packet covers two future ticks, not three.** The frame for the
current tick is dropped as stale, because the room has already simulated it. The
AFK test asserted "three frames then four ticks of grace" and failed on the sixth
tick; the correct answer is two frames plus four. The test now asserts the
property rather than the arithmetic — the player stops within one packet plus
200 ms — and pins the exact count separately.

### The gap this closed

Until this commit `Room::tick` resolved input, reported it, and stepped
`MatchState` — but **nothing applied the frame to a player's position**, because
`MatchState` had no player entities. Players were `PlayerSlot { id, team, life,
bot }`: a seat, not a body. Buddies had positions; players did not.

That was a hole in the plan rather than a shortcut here. §6.4 calls
`sim::step_local(state, input)` but no task creates it, and T-021 (snapshots),
T-022 (reach checks "using its own player position"), T-023 (bots that fight and
push a core) and T-024 (a full match) all need it. It is recorded in the table
below as **P-01** — a prerequisite, not a plan task — and the section after this
one is what it turned out to involve.

It ships as `MatchState::apply_input`, not `step_local`: the plan's name is for
the client's prediction entry point, which wraps this and adds the replay of
unacknowledged frames (§6.4 steps 5–8). The one function both sides run is the
input application itself, so that is the thing that got a name here.

## Bodies: two kinds of dead

`PlayerSlot` is what *elimination* sees, and D-02 requires that elimination look at
nothing but `team` and `life`: a player mid-swing dies on the same tick as one
standing still. A body is the opposite — it is everything elimination must
ignore. Keeping them in two types is what makes "elimination does not depend on
the body" checkable by reading one struct instead of auditing a function.

The consequence is that "dead" is two different things, and conflating them is
the bug the module is shaped to prevent:

| | `hp` | `PlayerLife` | Comes back? |
| --- | --- | --- | --- |
| Shot to zero | `0` | `Alive` | Yes, after 8 s (§4.8) |
| Team eliminated (D-02) | `0` | `Dead` | Never |

`PlayerLife` is one-way — there is no spectator state — so a respawn checks it
first. `respawn_due` skips an eliminated seat even if a timer somehow fires on
its body, because a respawn that trusted the timer would resurrect a team D-02
had removed from the match.

The order inside a tick is **respawn, then input**. A player whose 8 s expires on
tick N can act on tick N rather than N+1; the other order costs one tick of dead
time per death, which is invisible in a test and very visible in a fight.

### The spawn point is searched, not stored

"Cannot respawn inside a block" is a rule, not an assertion, so the point is
found rather than remembered. Three obstacles, and only one of them is visible to
the voxel query:

1. **The core.** Not a voxel — it is a structure with a protected volume stamped
   into the pad's blocked mask. `collides` cannot see it, so the search consults
   the pad directly. A spawn point chosen by "first cell where the AABB fits"
   would put players inside their own core.
2. **Crates.** Also not voxels in the current model; the pad owns them.
3. **Terrain.** This one the world query does see.

`MatchState::new` has no world, so it produces the *nominal* point — first pad
column outside the core with no crate in it — and the room calls `place_spawns`
with real terrain on the tick the match goes live. `respawn_due` re-checks the
stored point, because a pad can be built over during the eight seconds a player
is dead.

The search walks rings outward from the pad's centre, and within a ring it takes
the **four cardinals before the corners**. `(r, 0)` is `r` away; `(r, r)` is
`r√2` away. A plain `(dz, dx)` walk of a ring picks a corner when a nearer
cardinal was free — a respawn 41% further from where the player died, for no
reason anyone could see.

### Three things this found

**The golden fixtures had never landed a single hit.** T-015's fixtures hash five
fields, one of which is `combat`. That field was hashing an *empty event list*
for 20,000 ticks. The scripted input walked in four fixed compass directions, so
two duelists starting 6.7 m apart drifted further apart and never came within a
2.6 m sword reach — `combat` was stable, reproducible, and measuring nothing.
The script now faces the nearest other body and walks toward it. A guard test
(`the_scenario_reaches_movement_damage_and_respawn`) fails if a run ever produces
zero downs again, in the same spirit as the "decay branch was never exercised"
assertion in `match_always_terminates`. The duel fixture now records 34 downs,
34 revives and 1,326 protected body-ticks in 200 s.

**A point-blank swing always missed.** The melee cone test was
`if distance > ZERO`, so it applied at every distance — and at 5 cm the direction
between two bodies is dominated by whichever centimetre they happen to be offset
by, which is noise. Since `sim` has no player-versus-player collision, two
players *do* end up overlapping, which makes "standing on top of each other" the
most common melee situation in the game and the one situation where nobody could
be hit. The cone now applies only beyond the target's own half-width: inside its
silhouette, aim cannot be wrong.

**Elimination left the bodies at full health.** D-02 sets `PlayerLife::Dead` for
the whole team on one tick, but a `PlayerSlot` has no HP — so an eliminated team
left six bodies at 100 HP standing in the world. Nothing simulated them, but the
state hash and a future snapshot would both have reported a team that was out of
the match as fully healthy. `settle` now downs the bodies of the players
`Base::eliminate` reports killing, with `respawn_at` left `None` so nothing can
bring them back.

### A gap recorded, not invented

**There is no player-versus-player collision.** Two bodies walk through each
other. §4.8 does not mention it, no task asks for it, and adding it means
changing the movement sweep that T-009 owns and that every golden fixture runs
through — a design decision, not a bug fix. What it costs today: bodies can
occupy the same space, and the point-blank exemption above exists partly because
of it. It should be decided before the client renders two players in one place.

## Snapshots: the baseline is the frame the client acknowledged

§6.3 says each snapshot is delta-encoded against "the last snapshot the client
**acknowledged**", and that one word decides the whole module. Encoding against
the last frame the server *sent* is cheaper and wrong: if that frame is dropped,
the client applies a delta to a state it never had, and the error survives until
the next keyframe — three seconds of a player standing in the wrong place. The
server therefore keeps the frames it sent, keyed by sequence, and encodes against
whichever one the client names in `C_INPUT.acked_seq`.

That makes packet loss cost exactly one frame, and it makes the recovery path
free: a baseline that has fallen out of the 64-frame history produces a keyframe
instead of a guess. There is no NACK message on the wire — an ack that cannot be
resolved *is* the NACK.

The ack rides on `C_INPUT` rather than becoming its own message because §7.4's
token bucket is 30 messages/s: 20 Hz of acks plus 15 Hz of input is 35/s, so a
separate ack message would rate-limit the client into a disconnect.

Measured on the worst case §7.3 budgets for — 6 players, 12 buddies, 24
projectiles, 4 structures, 16 events, every field changing, all inside 48 m:

```
keyframe   529 B   budget 2048 B
delta      383 B   budget 2048 B   (§6.8 average: ~550 B)
stationary entity   2 B   ceiling 3 B
```

Three decisions are worth knowing about, because each one is a trap that the
gate test caught or that the next task would have hit:

- **Every entity obeys the same rule, including your own body.** §6.3's "own
  player: always, full fidelity" is a *scope* statement — your body is never
  culled — not an instruction to re-send it whole every frame. The first
  implementation did re-send it whole, and the reconstruction test failed
  immediately: a client that adds deltas doubled its own position. One rule for
  every entity costs a few bytes and removes an exception from the decoder.
- **A new entity needs no wire bit.** Its baseline row does not exist, so its
  delta is taken against zero — which is its absolute position. The client's
  "absent from the baseline" case and the "new entity" case are the same case.
- **The hysteresis band is a flag, not a second radius.** An entity between 48 m
  and 52 m stays in the snapshot and carries `STALE`, so the client knows the
  position is last-known. Past 52 m it simply disappears, and the client removes
  what it is not told about — there is no despawn message, and adding one would
  give two ways to say the same thing.

Events are never culled: the snapshot carries all of them regardless of distance,
and an overflow past 16 in one tick is reported to the host rather than
`truncate`d. A player learning their core is under attack must not depend on
being within render distance of it.

## Edits: validate everything, then write one line

The mutable world is tiny — terrain is indestructible, crates are the only
placeable block, so each team's 8 × 4 × 8 pad is the whole editable surface. What
is not tiny is the attack surface: `C_EDIT` is the one message a client can use
to change authoritative state, and §6.6's four checks exist to make that safe.

The order is **bounds → reach → existence → protection → rate → apply**, and the
ordering is load-bearing rather than stylistic:

- **Bounds first.** A coordinate outside the map has no block to be near, so
  "reach" is meaningless there. §7.4 rule 4 also names this case: reject, never
  close, because players legitimately aim at the sky.
- **Rate last, but still before anything is written.** If the pad were written
  first and the token bucket refused afterwards, a flood of edits would still
  change the world every tick it was refused — the pad's 10-tick stability timer
  would never complete, so no recipe would ever fire, and the cheapest attack in
  the game would be "spam `C_EDIT`". `Pads::check` is the read-only half of
  `Pads::place`; the rule lives in one place, and the pipeline can ask without
  mutating.
- **The pad mask and the voxel move together.** The pad is what the recipe
  matcher reads; the voxel is what movement collides with. An edit that updated
  one and not the other produces a crate you can walk through, or a recipe that
  fires on nothing.

Two rules are the plan's, and worth stating because they are the difference
between a tolerant server and an exploitable one:

- **Last-write-wins is not used.** Two edits on one cell in the same tick are
  resolved in arrival order: the second is validated against the post-first
  state and refused with `OCCUPIED`. Last-write-wins produces ghost blocks on
  clients that missed one message.
- **Diff versions increment on flush, not on edit.** A client applies a diff only
  when its version is exactly `last_applied + 1`, so two edits in one tick must
  not consume two versions — the second would be a version the client waits for
  forever.

`EditWorld` regenerates terrain from the seed and keeps the columns it touches:
a cache, not a second source of truth. Nothing is persisted, and seed + diff log
reconstructs the same world on any machine.

## Airdrops: the schedule is the economy

There is no mining. Crates arrive on a global timer, land as physical pickups,
and become blocks on a team's pad, so the drop schedule is not pacing flavour,
it is the resource chain. Four numbers define it:

| Rule | Shipped value | Why |
| --- | --- | --- |
| First drop | 0:10 | Every player has something to run at within ten seconds |
| Interval | 25 s to 5:00, then 15 s | Early positioning, then enough throughput to fund tier-3 |
| Crates per drop | 3 × player count | A shared drop must not starve six players |
| Every fifth drop | × 1.75 | A scheduled contested prize, not a random one |

**Site selection is integer arithmetic, not a float ratio.** A candidate at
distance `d` from the map centre, with a maximum distance `D`, gets weight
`3D − 2d`, algebraically `D × (1 + 2(1 − d/D))`, so centre drops are three
times as likely, but every operation is a whole number. The pick is
`next_below(total)` over that weight; the previous site is excluded. A committed
golden vector is asserted in the unit test and was re-run under `wasm32-wasip1`
in this commit, because a client that picks a different site draws its telegraph
in the wrong place for every player at once.

The drop falls 80 m at 12 m/s: 6.7 s from spawn to touchdown, 134 ticks. It is
announced when it spawns, not five seconds before impact, so the marker is up for
the whole descent. That is the telegraph row as a *floor* rather than a timer.
Landing scatters each crate into a ±2 m square (2.83 m diagonal, inside the 3 m
bound) with no rejection loop.

**Carry and deposit.** Pickup is automatic inside 1.2 m, capped at 12. Holding
the place action inside your own pad deposits at 6/s; the rate is an elapsed-tick
accumulator capped just under two crates, so time spent walking is not banked and
a long gap cannot become a burst. Death destroys `floor(load × 0.5)` and spills
the rest as pickups; that is the anti-snowball lever, and the reason a loaded
player is worth killing. Pickups expire at 180 s and the world holds at most 120.

The module keeps a ledger: `dropped == destroyed + deposited + carried +
on_ground + falling`. The `falling` bucket was added in T-024: a drop is booked
as dropped when it spawns, six or seven seconds before it lands, and without
that bucket every descent made the books fail to balance. Only a test that
samples every tick could see it.

Pickup distance is compared squared. The inner loop runs pickups × bodies on
every tick, and the square root alone measured 17% of a full match's wall time.

**What this module does not own:** pad ownership. `deposit` takes
`inside_own_pad` from the caller; the candidate set comes from worldgen, which
already excludes base pads. Both boundaries exist so the economy has no voxel
storage and no team rules of its own. The room loop is what joins them; that
join is P-02, and the bots that drive it are T-023.

## Bots: a player made of inputs

A bot is a pure function from an immutable `BotView` to a `BotOutput`: one
`InputFrame` plus, while building, one `C_EDIT` message. It cannot write state
because it is never handed a mutable one. `bot_uses_only_input_frames` proves
that rather than asserting it: one match attaches the brain to a seat, a second
match runs the same brain in the test and submits its frame as a human input,
and the two state hashes match for 800 ticks. A side channel would show up as
the first divergent tick.

The policy is a fixed priority ladder:

| Priority | Behaviour | Why here |
| --- | --- | --- |
| 1 | Finish the build already on the pad | A half-built shape matches nothing, and switching plans would abandon it |
| 2 | Self-defence inside 2 m | A player punching the bot is more urgent than any errand |
| 3 | Collect a pickup, else camp a falling drop | Walking to the marker is how a bot wins the race to a drop |
| 4 | Build the best shape the load affords: 9 crates for a buddy, 6 for a sword, 4 for a bandage | The recipe ladder is the only way to convert crates into power |
| 5 | Punch the enemy core | It is the only win condition that matters |

Two rules stop six bots from destroying a recipe through teamwork. Only the
primary builder, the teammate carrying the most crates with ties to the lowest
seat, may place: two bots laying different shapes in one pad produce a cluster
that matches nothing, and the stability timer resets on every block. And while
the pad is still empty a bot re-plans to its full load before the first crate
lands; once a cell is down the plan is committed.

Navigation is A* in 20 m legs, recomputed every 40 ticks. The goal is shifted
to a standable cell at terrain height, every cell target is a cell centre, and
`PathLimits::y_margin` widens the search band so a leg over a hill is not
rejected because both endpoints sit in a valley. Without those details bots
clip walls, path into tree trunks, or stand at the edge of their own collision
box, which looks like a stuck bot rather than a pathfinding bug.

D-05 is a separate assertion, not a branch in the brain: a match with one human
and five bots ends with `winner: None` and `EndReason::NoHumans` on the tick the
human's core falls, before last-team-standing can hand the win to a bot.
`d05_ends_a_bot_match_when_the_last_human_dies` is that test.

The full 3v3, six bot-driven bodies, finishes in **41.1 s debug and 1.77 s
release**, inside the 30,000-tick (25 simulated minute) bound. Wall time is
dominated by pathfinding and terrain queries, which is why the same match costs
twenty times more in an unoptimised build.

## A full match: eight promises, checked every tick

T-024 runs the real `room::play::Match` loop with six bots driving every seat,
for **28,490 ticks (23.7 simulated minutes)**, and checks all eight §12.4
invariants after every one:

| Invariant | How it is checked |
| --- | --- |
| HP stays in bounds | player, buddy and core HP compared with its own maximum every tick |
| Entity count is bounded | players + buddies + falling drops ≤ 46, pickups ≤ 120 |
| A dead buddy never returns | every buddy id that leaves the roster is retired; a return is a failure |
| Core HP never rises | compared with the previous tick's value |
| Buddy caps hold | per team and per role, against the cap matrix and the roster table |
| Crates are conserved | the airdrop ledger balances, and deposits reconcile with pad blocks after crafting and elimination sweeps |
| The match always ends | the loop is bounded at 30,000 ticks and asserts `is_over` |
| Elimination is idempotent | a second hit on a fallen core removes zero HP and leaves `state_hash` unchanged |

Two of those checks found real bugs that the unit tests could not see:

- **The ledger had no bucket for a falling drop.** `dropped` was incremented at
  spawn, but `destroyed + deposited + carried + on_ground` only described
  crates on the ground, so the books failed to balance for 6.7 seconds of every
  drop. `Audit::falling` is that bucket now, and `balances` includes it.
- **The core's elimination stamp was one tick behind the event.** The match
  clock advanced after actions ran, so damage used the state's previous tick
  while the `Eliminated` event was reported on the match's current tick.
  `MatchState::damage_core_at` takes the tick explicitly; the match loop passes
  its own, and a host no longer sees two tick numbers for one hit.

The conservation check needs care because §12.4's equation double-counts a
placed crate (see the discrepancy list). The harness proves the ledger the game
actually keeps: `audit.deposited` equals the blocks still on pads plus the
blocks recipes consumed plus the blocks a sealed pad swept. It is checked every
tick, not just at the end, so a phantom block cannot hide between checkpoints.

`no_entity_leak` runs three matches in one process from the same seed and
requires all three hash sequences to match. That turns "no global state" into a
test: a leaked cache or RNG would make the second run diverge. `reconnect_mid_match`
drops a human for 600 ticks, asserts the seat is still exactly one entity with
the same body and no drift, then proves the next jump moves it.

**The two-second gate is a release number.** The same match takes about 41
seconds unoptimised, because pathfinding and terrain queries are the hot paths.
Debug still checks all eight invariants; CI runs this file in both profiles and
only the optimised run enforces the wall clock.

The gate uses a 3v3. A 2v2 and a three-way FFA were tried first, and in 25
simulated minutes neither pushed a core to zero: a lone bot spends its time
collecting and dying rather than assembling a push. That is a bot-quality
limit, not a harness limitation, and it is why T-023's acceptance test is a
full six-bot room.

## The wasm boundary: one buffer, fifteen exports

The client does not parse a scene graph out of wasm. It reads one `#[repr(C)]`
buffer: a 52-byte header followed by the arrays it points at. Fifteen exports
are the whole API, each returning a `u32` pointer or status, so no `JsValue` is
created on the frame path. T-030 shipped 13; T-038 added two read-only recipe
helpers, described below.

| Piece | Size or rule | Why |
| --- | --- | --- |
| `RenderDesc` | 52 bytes | 13 `u32` fields; one pointer finds the whole frame |
| `GpuPlayer` | 32 bytes | u16/u8/u8, six f32, u8/u8/u16, with no padding |
| Arrays | 18 players, 64 buddies, 128 projectiles, 512 particles | allocated once in `cb_init` |
| `cb_tick` | zero allocations after 10 frames | 600 ticks under a counting allocator |
| Magic | `0xCBA7E001` on every tick | a zeroed buffer is a dropped frame, not an empty one |

The tests assert every `size_of` and `offset_of`, not just the totals. A field
reorder that keeps `GpuPlayer` at 32 bytes still moves `hp` away from offset 28,
and a JS `DataView` written against the old layout would read a position byte as
hit points. `RenderBuffer` puts the arrays immediately after the header, which
is what lets the loader size every view from one `cb_render_desc()` pointer.

`apps/web/src/wasm/loader.ts` owns the other half of the contract. Its `alloc`
wrapper calls `cb_mem_alloc` and then `rebuildViews()` unconditionally, because
a grow detaches every cached `ArrayBuffer`. In dev builds every frame checks
that each view still has a non-zero buffer and that the magic is present; the
error names `memory.grow`, the actual cause.

The browser does not load wasm-bindgen glue. All 15 exports take and return
numbers, so `loadClientWasm` compiles the raw `.wasm`, stubs the four internal
imports (`describe`, a throw shim, two externref-table hooks), and maps the
hash-suffixed export names back to the contract. The wasm artifact is the single
source of truth; `tools/wasm-exports.mjs` checks the 15 base names in CI.

`cb_tick` still steps one local body against a flat floor. That is enough to
prove wire input decoding, the 50 ms accumulator (at most four catch-up steps
per frame) and the render write without pretending to be a second match loop.
T-032 fills `cb_mesh_chunk` with real neighbour-aware terrain; `cb_on_snapshot`
records the payload size for the 4 KB boundary check until T-043 decodes it.

## The renderer: a budget that moves, a ceiling that does not

T-031 boots a three.js WebGL2 renderer and makes two different promises about
pixels. The resolution scale is allowed to move; the device pixel ratio is not.

| Guard | Value | Measured |
| --- | --- | --- |
| DPR clamp | `min(devicePixelRatio, 2)` | emulated DPR 3 gives `getPixelRatio()` 2 |
| Scale floor | 0.55 | 200 ms frames settle at 0.553 |
| Slow response | 25 ms frames drop the scale | 0.670 after 1 s |
| Fast response | 8 ms frames restore it | 0.997 after 2 s |
| Clear frame | 60 rAF frames/s | 60 frames in 1 s |

The scale is an exponential average of frame time with a target of
`16.7 / average`, smoothed at 15% per frame and clamped to `[0.55, 1]`. It
changes `renderer.setPixelRatio`, not the CSS size: the canvas stays full-window
and the drawing buffer gets cheaper. The DPR clamp happens once, at
construction, so a phone reporting a ratio of 3 or 4 never allocates a 4x
buffer even for one frame.

The three tiers are the §8.5 numbers, no more and no less:

| Tier | Render distance | Particle cap | Animation | Fog start | Fog end |
| --- | --- | --- | --- | --- | --- |
| LOW | 4 chunks | 64 | off | 40 m | 64 m |
| MED | 5 chunks | 128 | on | 52 m | 80 m |
| HIGH | 6 chunks | 256 | on | 64 m | 96 m |

Fog end is the one number §8.5 does not state. It is set to the render distance
in metres (distance x 16), which is also the camera far plane, so the fog hides
the streaming edge exactly where the world stops.

The boot benchmark renders 256 instanced boxes for two seconds and classifies
the average frame time: HIGH at 18 ms or better, MED up to 25 ms, LOW beyond
that. Those thresholds are not in the plan. The first version used 12 ms for
HIGH, which no 60 Hz display can reach; a measured 16.67 ms benchmark was
classified MED, and that measurement is what caught it. The thresholds now make
"kept up with 60 Hz" HIGH, and `the benchmark thresholds make 60 Hz HIGH` pins
the policy. The chosen tier is stored in localStorage, and
`?quality=low|med|high` overrides and persists it.

The specs run against `vite preview`, not the dev server: the benchmark and the
adaptive loop are the things under test, and a dev-only path would prove
nothing about the shipped bundle. They drive the loop through
`window.__cbTest.setFrameTime`, which is how a 60 Hz runner tests a 25 ms
device deterministically. The JS bundle is 598.6 KB raw and 152.8 KB gzipped,
under §8.10's 450 KB gzip target; the wasm asset is measured in the chunk
section below.

## Chunks: one mesh per column, a ring that lags

T-032 streams the 16 x 16 column map around the camera. The policy is a
Chebyshev radius 6 to load and 7 to keep: a loaded column survives until it is
eight away, so walking back and forth across a boundary does not re-mesh the
same terrain. Loading is one column per client frame; tests ask for a larger
budget when they want a steady state without waiting.

| Check | Measured |
| --- | --- |
| Loaded at the map centre, radius 6 | 169 columns |
| Draw calls at the map centre | 92 total: 88 chunks + 3 player + 1 particle (gate 120) |
| Triangles | 104,862 (gate 350k) |
| 200 load/unload cycles | 34 KB heap growth after GC (gate 5 MB) |
| 64 MB `memory.grow` | views rebuilt, geometry unchanged, new meshes still work |
| wasm asset | 376.0 KB raw, 121.6 KB gzipped (target 700 KB) |

The mesher is neighbour-aware: each column is meshed with the four columns
around it, so interior chunk borders do not draw walls. The payload is a
12-byte header, 12-byte vertices, `u32` indices and a 256-byte height map. The
header exists because the mesher chooses the AO-safe diagonal per quad;
rebuilding indices from vertex order would lose that choice. The height map is
what T-034's raycast targets; it comes from the column, not from the merged
vertices, so a greedy quad cannot leave a cell without a height.

Ownership is explicit twice. `cb_mesh_chunk` writes into staging memory, the
loader copies with `.slice()`, and only then releases. And because the Rust
meshing allocations can grow wasm memory on their own, the loader rebuilds its
views after the handoff, not only after `cb_mem_alloc`. A test caught this: the
first frame meshed, the heap grew, the desc view detached, and the loop stopped.
The extra rebuild is the fix, and it is why the grow spec passes now.

The chunk manager itself has no wasm dependency; it talks to a `MeshSource`.
The page uses the real bridge. The streaming unit tests use a fake source: one
runs the default 6/7 policy and checks a kept column at distance 7 and an
unloaded one at distance 8, the other runs 2/3 and asserts the whole set,
including the ring.

## Entities: three boxes and a float contract

Players and buddies are three instanced boxes: body, head and weapon. Six
players cost 3 draw calls; each buddy role costs 3 more. The 18-entity mix
used by the test, 6 players and 12 buddies, costs 21 draw calls, and the whole
scene at the map centre with 169 chunks is 92 draw calls and 105,410 triangles.

| Behaviour | Source | Rule |
| --- | --- | --- |
| Bob | `anim` | `sin(anim * 2π) * 0.06`, off on the LOW tier |
| Tilt | `pitch` | clamped to ±0.25 rad |
| Attack pulse | `flags` bit 2 | weapon scales up to 1.35 |
| Death dissolve | `hp`, `flags` bit 0, `anim` | alpha is `1 - anim` while dead |

The 32-byte `GpuPlayer` has exactly six floats and no spare one, so `anim`
carries the bob phase while alive and the death progress while dead, and the
attack window is a flag bit. Both are conventions, not free interpretation:
the bridge and T-041 must keep them, and this is the paragraph a future reader
should find before adding a seventh float.

The alpha is a per-instance attribute injected into one shared
`MeshBasicMaterial`; a single shader edit multiplies each fragment by the
instance alpha. That is why death can dissolve instead of popping without a
material per role. Projectiles and siege structures are later tasks; the plan's
27-draw-call line has room for them.

18 entities update in **0.025 ms on average** over 120 frames (gate 3 ms); the
first measured update was 0.1 ms. Pools and scratch objects are allocated at
construction, so the frame path only writes matrices, colors and alpha in
place.

Particles are one `InstancedMesh` of 256 billboarded quads, one draw call. The
CPU writes origin, velocity, spawn time and life when a particle is born; the
vertex shader computes age, position and alpha from `uTime`. `update()` writes
one uniform, and the test asserts the attribute-write counter stays at 256
after 120 frames. The tier's particle cap changes `instanceCount` without
reallocating the pool.

## Interaction: the same grid the server validates

The block you highlight and the block you send are the same integer cell. The
raycast is Amanatides-Woo DDA over the voxel grid, not a triangle ray: a mesh
hit gives a point on a greedy quad, and converting it back to a cell is the
off-by-one this avoids.

| Check | Assertion |
| --- | --- |
| 20-case ray table | six faces, diagonals, fractional origins, unnormalised and zero directions, out of reach, origin inside a block |
| through-wall | a wall at x = 2 wins over a target at x = 5 |
| visuals | wireframe at cell + 0.5; ghost at cell + normal + 0.5 |
| edits | place and break call the sink once and leave a pending record; ack retires it |

`VoxelSource` is the seam. The 13-export bridge has no block query, so the mesh
payload now carries a 256-byte height map per column, computed in Rust from the
column itself rather than from merged vertices. The chunk manager turns that into
`solid(x, y, z) = y < top(x, z)`. It is a surface approximation: the air under a
canopy and a cave are not represented. T-042's column diff stream replaces the
source without touching the interaction.

Reach is 6 m. The local edit sink is `cb_local_edit`; the pending map keeps the
returned sequence until the ack, which is the record a predicted-edit layer
needs. A rejected ack removes it. The local world is not mutated before the ack,
so rollback means forgetting the prediction, not undoing a write.

The page spec streams the real map, reads the top boundary of column (128,128),
casts down from two metres above it, and asserts the hit cell, normal, placement
cell and both previews before placing, breaking and acking both edits.

## Desktop input: one frame, one tick

The browser state is packed into the same 4 bytes §7.1 defines: buttons, yaw,
pitch and two signed 4-bit axes. The loop calls `poll()` once and hands the
returned array to `cb_tick` once; the test asserts the poll and tick counters
advance by the same amount over 300 ms.

| Concern | Behaviour |
| --- | --- |
| Pointer lock | the first canvas click requests it; mouse look and mouse buttons only apply while locked |
| Blur | every key and button is released; the packed axes and buttons go to zero |
| Hotbar | 1-9 select a slot, the wheel cycles, both emit a local `slot` action |
| Actions | Q drop, Tab scoreboard, E recipes, F stance, Esc menu; Tab never moves focus |
| Settings | sensitivity and invert-Y in localStorage; 0.5/true survives a reload |

Pointer lock is the one part of the contract the headless browser refuses. That
spec is a separate Playwright project with `headless: false`; CI installs Xvfb
and runs the suite under a virtual display. The other 40 specs stay headless,
and a local full run opens a window for the one headed test.

Sensitivity defaults to 0.25 yaw units per pixel; the plan does not pin it. LMB
sets FIRE and asks the interaction to break, RMB sets USE and asks it to place,
both only while locked. The wire frame has no slot or edit fields, so hotbar
actions are local callbacks and edits travel through `cb_local_edit`, not
through the 4-byte frame.

## Touch: the mode is the gesture

The mobile controls solve break-vs-place by making the selected hotbar slot the
mode. Slot 0 is the pickaxe; slots 1-8 are blocks. A tap on the world breaks in
pickaxe mode and places in block mode. There is no long-press to disambiguate.

| Control | Behaviour |
| --- | --- |
| Move | a stick spawns where the left thumb lands; drag distance maps to the two signed axes |
| Look | drag on the right half; 0.35 degrees per pixel, independent of the stick |
| Break and place | a tap, selected by the hotbar mode |
| Deposit | holding a block-mode touch repeats placements at 6/s |
| Jump | 64 px button, held jump bit |
| Stance | 56 px button; cycles Follow, Hold and Assault and shows the current one |
| Hotbar | nine 44 px slots, pickaxe plus eight block colours |

The two-finger spec sends both touches through CDP `Input.dispatchTouchEvent`,
because Playwright's `touchscreen.tap` is single-touch. It asserts both flags
active, nonzero stick axes, a changed yaw and a nonzero packed move nibble, then
both flags cleared on release. A separate hold test leaves one finger stationary
for 750 ms and requires 3-6 placement requests; a drag past 8 px cancels the
hold and becomes look, so the two gestures cannot both fire.

The stance button cycles local state and emits the new stance; applying it to
every buddy is the server's job (D-03) and arrives with the network layer. The
deposit repeat is also a request rate: the room's edit pipeline enforces 6/s and
pad ownership, so a hold outside the pad sends the same requests and the server
refuses them.

The touch surface lives in `inputTouch.ts` because T-037's HUD does not exist
yet. It creates its own style element and uses only `textContent`; every control
has an aria-label and the required 44 px minimum.

## HUD: a sampler, not a render loop

The HUD is eight components behind one store. Writes mark a dirty flag; a
100 ms interval flushes the latest snapshot to subscribers; the interval stops
when a flush finds nothing new. A value that changes at 20 Hz is written nine
times in a second in the test, never eleven.

| Rule | Enforcement | Measured |
| --- | --- | --- |
| 10 Hz numeric writes | 100 ms store interval | 9 mutations per field over 1 s of 20 Hz updates |
| Bars use transform only | fill width is CSS; the script writes `scaleX` | inline style is only `transform: scaleX(...)`, no width |
| Text goes through `textContent` | typed node writes; CI grep on `src/ui` | an image tag in a toast renders as text and creates no element |
| HUD cost ≤ 1 ms | `performance.measure("hud")` around each flush | max 0.2 ms over 60 flushes, last 0.1 ms |
| Event feed ≤ 5 | append, evict oldest | six toasts leave five |

The eight components are health, crates, match clock, next drop, stance,
buddies, your core and the enemy core; the event feed is the separate append-only
part. The initial paint is a boot cost and is deliberately outside the frame
measurement, because charging first layout to a per-frame budget would make the
number meaningless. CSS containment on the panels keeps a text change from
invalidating the whole screen.

The app's values are demo numbers until the match stream lands: the clock and
drop countdown are local, HP and crates start at their full and empty values,
and the stance is wired to F and the touch button. The store is the seam that
T-045 and T-048 will feed.

Plan §8.7 says the store batches DOM writes into a `requestAnimationFrame`; the
acceptance criterion says a 20 Hz value is sampled on a 100 ms timer. The timer
wins: a 60 Hz frame callback would need a second throttle anyway, and 100 ms is
the exact bound the criterion names.

## Recipes: the table and the prediction come from Rust

The client does not carry a second recipe list. At boot it calls `cb_recipes`,
which serialises `RecipeBook::embedded()`; the book decodes 14 rows and draws
each top-down grid from that row's blocks. Locked rows are visible with their
unlock clock.

| Check | Result |
| --- | --- |
| Cards built from the wasm table | 14 |
| Locked cards | visible, each with `Unlocks m:ss` |
| Near-miss parity | 500 random clusters; every predicted completion matched the matcher |
| Banner | `SWORD: needs 1 more crate` |
| Ghost | one `InstancedMesh`, count = recipe cost |
| wasm asset | 376.0 KB raw, 121.6 KB gzipped |

`cb_near_miss` runs the Rust canonicaliser over rotations and translations and
returns `recipe_id << 8 | missing`. Rust maps the missing blocks back into the
cluster's frame, so `near_miss_never_lies` can add them to the original cluster
and ask the real matcher; every positive prediction matched. The UI only needs
the count. The prediction is one-sided: a cluster with an extra block is not
reported as "needs one more", because adding the missing block would not craft.
That is the Hamming-distance-2 intent without a false promise on a shape that
also has an extra crate.

The ghost is one translucent instanced copy of a recipe row. The book opens from
E on desktop and from its own 44 px button on touch; the near-miss banner is
driven by the same Rust call. The pad state that will feed an anchor and a live
cluster is T-042/T-048's job; the prediction and overlay APIs are already
tested.

This task is why the wasm boundary grew from 13 to 15 exports. §5.1 says JS
never decides whether a recipe matches, so porting the canonicaliser into
TypeScript was not an option. The two new exports are read-only data and
prediction helpers; the state exports are unchanged and the export check now
pins 15.

## Shell: one manager and no dead ends

Boot to Main to Lobby to Match to Results, with Settings branching from Main. A
single `ScreenManager` writes `body[data-screen]`; CSS hides the canvas, HUD,
recipe book and touch surface whenever the attribute is not `match`. Every
screen has a button that leaves it, and the match carries a LEAVE MATCH button,
so Results is reachable without waiting for a core.

| Screen | Exits |
| --- | --- |
| Boot | Continue to Main |
| Main | Online, Offline, Settings |
| Lobby | Create or Join, Start to Match, Back to Main |
| Settings | Back to Main |
| Match | Leave to Results |
| Results | Rematch, Main |

The lobby generates a six-character code from an unambiguous alphabet, shows a
copy button, and validates a joined code against `[A-Z0-9]{6}` before doing
anything else. Networking is T-040; Start currently enters the local demo match,
which is the only part of the lobby that is a placeholder.

Settings persist quality, sensitivity, invert-Y, UI scale (80-150%), reduced
motion and a key map for ten actions. The remap buttons capture one key on the
window in the capture phase, so assigning a key cannot also fire the game
action. The persistence test changes all six, reloads, then presses the
remapped forward key and reads the packed frame's forward nibble. The game
inputs never parse settings; they read the same storage at construction.

The UI scale is applied to menu panels through a CSS variable; the in-match
touch controls keep their 44 px targets. `?screen=match` is a test entry point
that bypasses the shell; production starts at Boot. The traversal spec does not
use it and visits every screen through the buttons.

The match screen is still the demo simulation: Results is reached by the leave
button, not by a core falling, because the host and offline match arrive later.

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
