# CRATEBOUND — Implementation Plan

**A 2–6 player voxel browser game: Minecraft-style block building fused with Team Buddies crate-stacking unit production.**

|                         |                                                                                                                                      |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| Document status         | v1.1 — decisions locked, execution-ready                                                                                                               |
| Audience                | AI coding agents executing task-by-task; human owner approving gates                                                                 |
| Working title           | Cratebound                                                                                                                           |
| Scope                   | Playable online multiplayer vertical slice → launchable free-tier product                                                            |
| Execution model         | Sequential task graph (T-001 … T-064). No time estimates — progress is measured by task completion and gate criteria, not calendars. |
| Owner decisions pending | See §18                                                                                                                              |

---

## Revision log

**v1.1 — owner decisions applied.** Five answers changed the design; two of them changed the architecture's data flow, so the affected sections were rewritten rather than patched.

| # | Decision | Sections rewritten | Tasks changed |
|---|---|---|---|
| D-01 | **Airdrop crates replace mining entirely.** Crates fall from the sky on a timer, land as pickups, are carried to a base stacking pad, and placed as blocks there. No ore, no veins, no resource chain. | §4.2, §4.3, §4.4, §4.5.1, §4.5.4, §4.11, §5.3, §12.4 | T-005, T-006, T-010, **T-025 (new)**, T-054 |
| D-02 | **Instant elimination. Spectator mode dropped.** Core destroyed → every living player on that team dies on the same tick → defeat screen → leave match. | §4.8, §4.10, §6.11, §11, §12.5 | T-013, T-021, T-054 |
| D-03 | **Buddies autonomous, single-button stance toggle** (Follow / Hold / Assault). Confirmed as planned — no change. | — | — |
| D-04 | **No meta-progression.** Confirmed — every match starts equal, server stays stateless. | — | — |
| D-05 | **Bots fill empty slots but cannot win.** When the last human is eliminated the match ends immediately with no winner, even if friendly bots survive. | §4.7, §4.8, §6.10, §12.5 | T-014, T-023 |

**Consequence worth flagging up front:** D-01 *reduces* the netcode budget rather than increasing it. With terrain indestructible (§4.4), the only blocks that ever change are crates inside an 8×8 pad — worst-case chunk diff falls from ~1.2 KB to ~120 bytes, and the 2 KB snapshot ceiling gains an order of magnitude of headroom. A design change that makes the hard part easier is rare enough to call out.

---

## Contents

| §  | Section                      | What it answers                                                                    |
| -- | ---------------------------- | ---------------------------------------------------------------------------------- |
| 1  | How to read this document    | Conventions, task protocol                                                         |
| 2  | Assumptions register         | 12 numbered assumptions, each with its blast radius                                |
| 3  | Verdict                      | The three decisions that make or break this                                        |
| 4  | Game design specification    | Loop, map, crate economy, 14 recipes, 6 buddy roles, caps, combat math, juice      |
| 5  | Technical architecture       | Stack decision, topology, crate layout, the wasm boundary, determinism             |
| 6  | Netcode                      | Transport, tick model, prediction, interest management, world streaming, bandwidth |
| 7  | Protocol contract            | Every message, every byte, validation rules                                        |
| 8  | Client architecture          | Render pipeline, 13 wasm exports, mobile perf, touch, HUD, audio                   |
| 9  | Free-tier verification       | **The capacity arithmetic** — sourced limits, computed ceilings, hosting verdict   |
| 10 | Deployment                   | Vercel, Cloudflare, fallback VM, CI, rollback                                      |
| 11 | Security and anti-cheat      | 15-threat model, rate limits, what is *not* defended                               |
| 12 | Testing strategy             | 8 layers, determinism harness, recipe test matrix, invariant tests                 |
| 13 | Observability and operations | Signals, thresholds, kill switches                                                 |
| 14 | Risk register                | 10 scored risks with detectors                                                     |
| 15 | Task backlog | **60 tasks**, dependencies, files, acceptance criteria |
| 16 | Milestones and gates         | M0–M6 exit criteria                                                                |
| 17 | What you must prepare        | Accounts, secrets, assets, blocking decisions                                      |
| 18 | Decisions, open questions, suggestions | What is locked, what I decided for you, what is still open |

**If you read only three things:** §3 (the verdict), §9.2 (the free-tier arithmetic — it is the constraint everything else bends around), and §16 (the gates).

---

## 1. How to read this document

**Normative language.** `MUST` = a gate. A task is not complete until every `MUST` in its acceptance criteria passes. `SHOULD` = do it unless a documented reason exists. `MAY` = optional.

**Task graph.** §15 is the execution unit. Every task has an ID, dependencies, the exact files it creates, and acceptance criteria that are *testable by a command or an assertion* — never "looks good". An agent picks the lowest-numbered task whose dependencies are all `DONE`, executes it, runs the verification, and marks it complete. If verification fails, the task stays open; do not mark it done and move on.

**Numbers, not adjectives.** Every performance claim in this plan carries a number and a named measurement method. If a task cannot be verified numerically, it carries an assertion instead (state hash equality, byte count, status code).

**Order of truth.** When this document disagrees with reality (a limit changed, a library broke), reality wins — but update the document in the same commit, and record the change in §14 (Risk register) if it affects a budget.

---

## 2. Assumptions register

Every assumption is numbered so it can be challenged in isolation. If an assumption is wrong, the tasks that depend on it are listed.

| ID   | Assumption                                                                                                                                                                           | Impact if wrong                                                               | Depends on it      |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------- | ------------------ |
| A-01 | Players are on desktop or mid-to-high-tier mobile (≥ 4 GB RAM, WebGL2, 2020-era SoC or better). Low-end Android is explicitly out of scope for v1.                                   | Dynamic-resolution floor and entity budget must drop; T-055 gates change.     | T-031, T-055       |
| A-02 | No accounts in v1. Identity = a server-signed guest token bound to a display name. Persistent accounts are a post-launch feature.                                                    | Auth tasks (T-050) expand; DB becomes mandatory.                              | T-050, T-053       |
| A-03 | "Free tier only" means **no payment method may be required**. A provider that demands a card for identity verification is treated as *fallback*, not primary.                        | Primary host choice flips to Oracle Always Free.                              | §9, T-046, T-057   |
| A-04 | Matches are room-code based. No global public matchmaking browser in v1 (a server list is a stretch goal).                                                                           | Room registry needs a directory service; +1 service to operate.               | T-047              |
| A-05 | The world is finite and procedurally generated from a seed. There is no persistent world between matches.                                                                            | Chunk streaming design changes fundamentally.                                 | T-005, T-042       |
| A-06 | Block edits are the only persistent per-match state; the terrain itself is never transferred — the client regenerates it from the seed.                                              | Snapshot budget blows past 2 KB; bandwidth target fails.                      | T-005, T-042, §6.6 |
| A-07 | One authoritative simulation implementation. There is exactly one `sim` crate, compiled to wasm for the client and native for the server. No rules are re-implemented in TypeScript. | Desync becomes structurally unavoidable. Reject any PR that duplicates rules. | T-003…T-014        |
| A-08 | Determinism is achieved with fixed-point integer math in authoritative state. Floating point is permitted only in rendering and audio.                                               | Cross-platform desync on `wasm32` vs `aarch64` from `f32` rounding.           | T-003, T-015       |
| A-09 | A match is 2–6 players, target duration 15–25 minutes, one room per match, rooms are ephemeral.                                                                                      | Buddy caps and map size are tuned to this.                                    | §4, T-013          |
| A-10 | Audio is synthesized/loaded from small OGG/Opus files; no licensed music in v1.                                                                                                      | Asset budget and licensing review grow.                                       | T-030              |
| A-11 | The game ships in English; UI strings are externalised from day one so localisation is a data change, not a refactor.                                                                | String extraction later is a wide, risky diff.                                | T-027              |
| A-12 | "Offline" mode means the same `room` crate compiled to wasm and run in a Web Worker, with bot players filling empty slots. It is not a separate game.                                | A second code path for singleplayer; desync between modes.                    | T-048              |

---

## 3. Verdict

The design is buildable on free infrastructure, but only if one uncomfortable number is accepted up front: **Cloudflare's free Durable Objects tier supports roughly one full 6-player room running ~6 hours per day.** That is a demo ceiling, not a launch ceiling. The plan therefore commits to a **runtime-agnostic room core** with two thin hosts — a Durable Object for zero-ops deployment and an Axum binary for a free VM — so that hitting the ceiling is a configuration change, not a rewrite. Everything else (client on Vercel, Postgres, Redis) fits inside free tiers with room to spare.

The second structural decision: **the client does not download the world.** Terrain is generated locally from a seed by the same `world` crate the server uses; the server only replicates block *edits*. This removes the single largest bandwidth cost from a voxel multiplayer game and is what makes a 2 KB snapshot budget achievable at all. It is only possible because A-07 and A-08 hold — one deterministic rules implementation, integer math. If those two assumptions break, this plan breaks.

The third: **the crate-recipe mechanic is the game, so it gets the most engineering.** Crates arrive by airdrop, get carried to a stacking pad, and are placed as blocks inside it — which means block placement is the input to a production system. The matcher must therefore be deterministic, rotation-aware, abuse-resistant, and legible to players, and it must be bounded to an 8×8 pad rather than the whole world. A voxel builder with a weak recipe system is a worse Minecraft; the recipe matcher (T-010) is the highest-risk task in the plan and is scheduled early for that reason.

---

## 4. Game design specification

> All numbers in this section are **tuning defaults**, not laws. Every one of them lives in a single data file (`crates/sim/src/tuning.rs` generated from `data/tuning.ron`) so a balance pass is a data change, never a code change. The acceptance tests in §12 assert *relationships* (e.g. "bare hands cannot destroy a core within the match limit"), not absolute values, so tuning does not break tests.

### 4.1 Pillars

1. **Building is production.** Every block you place is either terrain, defence, or a factory input. There is no separate "crafting menu".
2. **Buddies are commitments.** They cost a visible structure in the world, they take time to build, and they never come back. Losing one is a real loss, not a respawn timer.
3. **The base is the only score.** Kills are means; base HP is the scoreboard. This is what stops the game from becoming a deathmatch with scenery.

### 4.2 Match shape

| Phase    | Window        | What players are doing                                                                  | Design intent                                       |
| -------- | ------------- | --------------------------------------------------------------------------------------- | --------------------------------------------------- |
| Opening | 0:00 – 1:30 | Race for the first airdrop (lands at 0:10), carry 6 crates to the pad, build the first 4–9 crate recipe | Everyone has a weapon and a plan within 90 s |
| Build-up | 1:30 – 6:00   | First buddy (target: **60–90 s** time-to-first-buddy), contest The Yard, tier-2 recipes | The map's centre becomes worth fighting for         |
| Pressure | 6:00 – 14:00  | Skirmishes, buddy attrition, chip damage on exposed cores, tier-3 farming               | Attrition matters because buddy death is permanent  |
| Siege    | 14:00 – 22:00 | Siege kit deployed, coordinated push, base falls                                        | The match must be *decided* by action, not by timer |
| Overtime | 22:00 +       | All cores take +2% max-HP damage per 30 s (global "decay")                              | Guarantees termination without a hard draw          |

**Target match length: 15–25 minutes.** The overtime decay exists because a stalemate between two turtled teams is the most likely failure mode of a base-destruction game, and "nobody wins" is a worse outcome than "the more aggressive team wins".

### 4.3 World and map

| Property | Value | Rationale |
|---|---|---|
| World size | 256 × 256 blocks (X, Z) × 96 tall (Y) | 65,536 m² of floor. At 4.5 m/s that's 57 s corner-to-corner — big enough that a rush cannot cross the map before the defender has a weapon. |
| Chunk | 16 × 96 × 16 **columns** (one mesh per column, 6 internal 16³ sections for dirty tracking) | 169 columns at render distance 6 → ~80 visible draw calls after frustum culling. Meshing per 16³ cube would be 1,014 meshes — 6× the draw calls for no benefit. |
| Sea level | Y = 40 | Terrain spans Y 28–64; gives 32 blocks of build height above sea level. |
| Base count | 2–6, arranged on a circle of radius 96 around map centre, equal angular spacing | Rotational symmetry = fairness. Every base is equidistant from the centre objective. |
| Base spacing | 2 players: 192 m apart. 3: 166 m. 4: 136 m. 6: 96 m. | At 96 m (6-player), first contact is ~21 s after spawn — enough to reach a drop. At 192 m (duel), ~43 s — enough for a first buddy. |
| Base pad | 24 × 24 flattened platform, core at centre | Gives room for a 4×4 siege structure and a defensive ring. |
| **Stacking pad** | **8 × 8 flat area at the edge of the base pad, floor indestructible, 4 blocks of build height** | The only place in the world where crates may be placed. Bounds the recipe search to 256 cells, makes the matcher trivially fast, and gives every base a visible "factory" that an attacker can target. |
| Centre feature | "The Yard": a 48 × 48 raised plateau at Y 52, **3× airdrop weighting**, no base | The only place worth holding. Creates a mid-game objective without adding a rule. |
| Airdrop sites | Candidate grid of 32 × 32 m cells, flat ground (slope ≤ 2 blocks), excluding base pads | Deterministic site selection (§4.4) so both server and client can predict where a drop *could* land, and neither can be surprised. |
| Terrain | 3 biomes (plains, badlands, forest) by low-frequency noise; caves disabled in v1; **indestructible** | See §4.4 — indestructible terrain is what collapses the netcode's block-edit budget to near zero. |
| Crate veins | **Removed.** There is no ore and no mining in v1. | D-01: crates arrive by airdrop only. |


**Why this size yields 15–25 minutes, not 3 or 60:** the map is now a *delivery* problem rather than a *mining* problem. A drop lands, and the nearest team is 96 m away in a 6-player match (~21 s) or 192 m in a duel (~43 s). A tier-3 siege recipe costs 24 crates, which at the §4.4 rates is ~2 minutes of uncontested collection plus a 43 s carry. A core needs ~90 s of uncontested tier-2 DPS (§4.8). So the minimum viable attack is `21 s to drop + 120 s collecting + 21 s to enemy pad + 90 s siege ≈ 4 min` — but only if the defender does nothing, which they won't. Match length is dominated by how well teams contest drops, which is exactly the tension the airdrop system is for.

### 4.4 Airdrop economy

There is no mining. Crates fall from the sky on a global timer, land as physical pickups, and are carried to a base's stacking pad where they become buildable blocks. This is the Team Buddies loop, and it removes the entire resource-chain layer that would otherwise sit between the player and the fun part.

#### 4.4.1 Drop schedule

| Parameter | Value | Rationale |
|---|---|---|
| First drop | **0:10** | A player must have something to do within ten seconds of spawning. |
| Interval, 0:00 – 5:00 | **25 s** | Slow enough that the opening is about positioning, not vacuuming. |
| Interval, 5:00 – end | **15 s** | Escalation: the mid-game needs a higher crate throughput to fund tier-3 recipes and replace dead buddies. |
| Crates per drop | **3 × player_count** | **This is the key scaling rule.** A shared drop in a 6-player match must not starve six players. 2p → 6 crates; 6p → 18 crates. Without this, a 6-player match delivers 4 crates/min/player and the game stalls. |
| Heavy drop | Every **5th** drop, ×**1.75** crates | A periodic contested prize. Also the natural moment for a team fight. |
| Telegraph | **5 s** before impact | A descending shadow, a smoke flare, a HUD marker, and an audio cue. A drop nobody can see coming is not a contest, it is a lottery. |
| Fall speed | 12 m/s from Y 120 | ~6.7 s of visible descent. |
| Total over 20 min | ~59 drops ≈ **354 crates (2p) … 1,062 crates (6p)** | Compare: the v1.0 mining economy produced ~430 crates for any player count. The 6-player case needed the scaling rule to stay equivalent per-head. |

#### 4.4.2 Site selection

Deterministic and shared by server and client:

```
candidates = flat 32×32 cells outside base pads
weight(c)  = 1 + 2 × (1 − dist(c, centre) / max_dist)
site       = weighted_random(rng_airdrop, candidates, exclude = last_site)
```

Centre drops are ~3× as likely, so The Yard is contested by geometry rather than by a rule. `exclude = last_site` prevents two consecutive drops in the same place, which would let one team camp a single cell.

#### 4.4.3 Pickup, carry, deposit

| Rule | Value |
|---|---|
| Pickup | Automatic on walking within **1.2 m** of a landed crate, up to the carry cap |
| Carry cap | **12 crates** — one drop's worth in a duel, two-thirds of a drop in a 6-player match, so a full drop always requires either two trips or two players |
| Carried crates are visible | Rendered as a stack in the player's arms (one instanced mesh, count = carried) — opponents can see who is loaded and worth killing |
| Deposit | Hold the place button inside your own stacking pad: **6 crates/s**, each becoming a block |
| Placement constraint | Crates may be placed **only inside your own 8×8 stacking pad**, up to 4 blocks high. Not on terrain, not in enemy pads |
| Crates on death | Carried crates drop as pickups, **50% destroyed**. The main anti-snowball lever — a loaded player who dies feeds the killer |
| Pickup lifetime | **180 s**, then despawn |
| Pickup entity cap | **120**; oldest despawned first | Bounds the entity count so a 20-minute match cannot leak. |

#### 4.4.4 The terrain rule

**Terrain is indestructible. Crates are the only placeable and breakable block.** Consequences, all of them good:

1. No resource chain, no tool tiering, no "should I mine or fight" — the answer is always *go get the drop*.
2. No tunnelling under a base, no griefing a map you don't own, no wall-breaching exploit.
3. **The block-edit budget collapses.** The only cells that ever change are inside a 256-cell pad volume. Worst-case chunk diff drops from ~1.2 KB to ~120 bytes, and the snapshot ceiling in §6.3 gains an order of magnitude of headroom.
4. Base walls built from crates are meaningful, because nothing else can breach them except a ram or a catapult.

The tradeoff is real and worth stating: you lose the "dig" half of Minecraft. The voxel interaction that remains is *building*, which is the half this game actually needs. If playtest says otherwise, `tuning.terrain.breakable = true` restores digging with no architectural change — edits are already general — at the cost of the diff budget and a tunnelling exploit class.

### 4.5 The recipe system

This is the core mechanic and the highest-risk system in the plan. It must be unambiguous enough that two independent implementations would agree, and cheap enough to run on every block placement.

#### 4.5.1 What counts as a candidate

A **cluster** is a 6-connected component of `Crate` blocks, computed by flood fill from the placed block, capped at **32 blocks** (recipes max out at 24; anything larger is rejected as "not a recipe" without scanning further).

A cluster is **eligible** when:

1. **Every member block is inside one team's 8×8 stacking pad** (D-01). This is checked first and rejects almost everything, which is why the matcher is cheap: the search space is 256 cells, not the world.
2. Size is between 4 and 32 blocks.
3. Every crate in the cluster's **lowest Y layer** has a solid (pad floor or another crate) block directly beneath it. Floating crate structures do not craft. *(Rationale: recipes are physical objects in Team Buddies; also stops mid-air placement exploits.)*
4. The cluster has been byte-identical for **10 consecutive ticks (500 ms)**. This debounces the crafting trigger while a player is still depositing.
5. No member block overlaps the core's protected volume.

#### 4.5.2 Canonical form

```
canonicalize(cluster) -> [u8; N]:
  origin = (min_x, min_y, min_z) over cluster
  offsets = sorted( (x-origin.x, y-origin.y, z-origin.z) for each block )
  // y is NOT rotated (gravity is vertical), only yaw is
  best = None
  for k in 0..4:
      rot = rotate_y(offsets, k * 90°)
      key = pack_varint_triples(rot)      // deterministic byte string
      best = min(best, key)               // lexicographic minimum = rotation-invariant
  return best
```

**Matching is exact equality of the canonical byte string.** No fuzzy matching, no "contains", no subset matching. A 3×3 slab of 9 crates does not match a 3×3-plus-3 recipe, and it does not match a 3×2 sword either — it matches nothing, and the UI says so (see 4.5.5).

Why lexicographic-minimum over rotations: it makes the key rotation-invariant without storing 4 keys per recipe, and it makes the matcher a single hash lookup — `O(n log n)` to canonicalize, `O(1)` to match.

#### 4.5.3 Recipe table

Coordinates are `(x, z)` in plan view with `y` as layer. `#` = crate.

**Tier 1 — available from match start**

| ID              | Shape                | Cost | Output                                     | Cooldown |
| --------------- | -------------------- | ---- | ------------------------------------------ | -------- |
| `R01_SWORD`     | `###/###` (3×2 slab) | 6    | Sword pickup: 25 dmg / 0.45 s, reach 2.6 m | 20 s     |
| `R02_BOW`       | `#../#../###` (L)    | 5    | Bow pickup: 30 dmg projectile @ 40 m/s     | 20 s     |
| `R03_BANDAGE`   | `##/##` (2×2)        | 4    | Heal kit: +40 HP, consumed                 | 15 s     |
| `R04_SWORDSMAN` | `###/###/###` (3×3)  | 9    | Buddy: Swordsman                           | 40 s     |

**Tier 2 — unlocks at match minute 3:00**

| ID           | Shape                                     | Cost | Output                                           | Cooldown |
| ------------ | ----------------------------------------- | ---- | ------------------------------------------------ | -------- |
| `R05_ARCHER` | 2×2×2 cube (8)                            | 8    | Buddy: Archer                                    | 45 s     |
| `R06_MAGE`   | `###/###/###` + `.#.` on top (12)         | 12   | Buddy: Mage                                      | 60 s     |
| `R07_HEALER` | plus-sign (5) + plus-sign on top (5) = 10 | 10   | Buddy: Healer                                    | 60 s     |
| `R08_SPEAR`  | 1×7 line                                  | 7    | Spear: 20 dmg / 0.55 s, reach 3.6 m              | 30 s     |
| `R09_TOWER`  | 2×2 footprint, 4 layers = 16              | 16   | Static tower: 12 dmg / 0.8 s, range 18 m, 200 HP | 90 s     |

**Tier 3 — unlocks at match minute 8:00**

| ID              | Shape                            | Cost | Output                                                         | Cooldown |
| --------------- | -------------------------------- | ---- | -------------------------------------------------------------- | -------- |
| `R10_RAM`       | 4×2 footprint, 3 layers = 24     | 24   | Siege ram: 250 structure dmg / 2.0 s, 400 HP, moves at 1.2 m/s | 120 s    |
| `R11_CATAPULT`  | 4×4 base + 2×2 top = 20          | 20   | Catapult: 180 structure dmg projectile, range 30 m             | 120 s    |
| `R12_BERSERKER` | 3×3 base + 3×3 second layer = 18 | 18   | Buddy: Berserker                                               | 90 s     |
| `R13_WARD`      | 3×3 ring (8) + 6 on top = 14     | 14   | Base ward: core takes −50% damage for 60 s                     | 120 s    |
| `R14_SNIPER`    | 3×5 slab = 15                    | 15   | Buddy: Sniper                                                  | 90 s     |

#### 4.5.4 Anti-abuse rules

| Threat | Rule |
|---|---|
| Spam tiny patterns for free units | Minimum cost 4, and every output has a cooldown ≥ 15 s |
| Deposit-craft-deposit flicker | 10-tick stability window (§4.5.1 rule 4) |
| Craft inside an enemy base | Crates can only be placed inside your **own** pad; enemy pad cells reject placement |
| Duplicate the same buddy past the cap | A craft attempt at the cap **consumes nothing** and returns a "cap reached" event — the player keeps their crates |
| Wall-mounted cheese | Ground-support requirement (§4.5.1 rule 3) |
| Two players racing the same cluster | Server processes deposits serially per tick; the first canonical match consumes the blocks, the second gets "already consumed" |
| Building a 33-block wall and lagging the matcher | Cluster cap 32; flood fill aborts immediately on exceeding it |
| Farming crates faster than intended | Drops are the only source, on a global timer — the rate is a server constant, not a player action |

#### 4.5.5 Player legibility (this is a design requirement, not polish)

A player must be able to answer "what will this build?" without a wiki. Three mechanisms, all cheap:

1. **Recipe book** (HUD panel): every unlocked recipe as a top-down ASCII-style grid, with cost and output.
2. **Ghost overlay**: while holding a crate block, the HUD projects the nearest *matching-so-far* recipe onto the ground as a translucent overlay, aligned to the block the player is aiming at. This is a client-side-only visual — the server still does the real match.
3. **Near-miss feedback**: if the current cluster's canonical form is not a recipe but is within Hamming distance 2 of one, the HUD shows "SWORD — needs 1 more crate". This is computed from the same canonical form, so it can never lie about what will happen.

### 4.6 Buddy roster

| Role      | HP  | Damage         | Interval | Range | Speed   | Aggro | Cost | Per-team cap | Role                 |
| --------- | --- | -------------- | -------- | ----- | ------- | ----- | ---- | ------------ | -------------------- |
| Swordsman | 120 | 14             | 0.60 s   | 1.8 m | 3.4 m/s | 12 m  | 9    | 4            | Line holder, soaks   |
| Archer    | 80  | 12             | 1.10 s   | 14 m  | 3.2 m/s | 16 m  | 8    | 4            | Sustained ranged     |
| Mage      | 70  | 22 (AoE r=2.5) | 1.80 s   | 10 m  | 3.0 m/s | 14 m  | 12   | 3            | Anti-clump           |
| Healer    | 75  | +15 heal       | 1.50 s   | 8 m   | 3.2 m/s | 12 m  | 10   | 2            | Force multiplier     |
| Berserker | 200 | 20             | 0.50 s   | 1.8 m | 3.8 m/s | 14 m  | 18   | 2            | Breaks a line        |
| Sniper    | 60  | 40             | 2.50 s   | 28 m  | 2.8 m/s | 32 m  | 15   | 2            | Punishes towers/rams |

**Counter relationships (intended, not enforced):** Sniper > Tower/Ram · Mage > clustered Swordsmen · Berserker > Healer · Archer > Berserker (kiting) · Swordsman > Archer (closes distance).

**AI model — a 4-state machine, deterministic:**

```
IDLE     → no target, owner in range       → FOLLOW
FOLLOW   → enemy within aggro radius        → ENGAGE
ENGAGE   → target dead / out of leash       → FOLLOW
ENGAGE   → role == Healer && ally < 60% HP  → HEAL
any      → HP == 0                          → DEAD (permanent)
```

- **Target selection:** score = `distance_weight × dist − role_weight[target_role] − (target.hp < 30% ? 15 : 0)`. Highest score wins, ties broken by lowest `EntityId` (determinism).
- **Pathfinding:** A* on the voxel grid, **budgeted at 512 node expansions per buddy per tick**, repath every 30 ticks or when a block changes within 2 m of the path. Step up 1 block, fall up to 3. If no path within budget, steer directly and accept getting stuck — a stuck buddy is a legible failure, a server hang is not.
- **Leash:** 24 m from owner. Beyond it, path back and ignore combat.
- **Owner dies:** buddy keeps its current target, then returns to the owner's spawn point and holds. It does not despawn — that would make player death a cheap way to reset.
- **Command model (A-Q3):** one stance toggle per buddy — **FOLLOW / HOLD / ASSAULT** — cycled by a single key or HUD tap. No selection cursor, no RTS layer. This keeps the game an FPS on mobile.

### 4.7 Team and buddy caps

Buddies are capped **per team**, not per player, because the fantasy is a squad, not a personal pet.

| Mode  | Players | Teams | Players/team | **Buddies/team** | Total buddies | Total entities (players + buddies) |
| ----- | ------- | ----- | ------------ | ---------------- | ------------- | ---------------------------------- |
| FFA   | 2       | 2     | 1            | 3                | 6             | 8                                  |
| FFA   | 3       | 3     | 1            | 3                | 9             | 12                                 |
| FFA   | 4       | 4     | 1            | 2                | 8             | 12                                 |
| FFA   | 5       | 5     | 1            | 2                | 10            | 15                                 |
| FFA   | 6       | 6     | 1            | 2                | 12            | 18                                 |
| Teams | 2       | 2     | 1v1          | 4                | 8             | 10                                 |
| Teams | 4       | 2     | 2v2          | 5                | 10            | 14                                 |
| Teams | 6       | 2     | 3v3          | 5                | 10            | 16                                 |
| Teams | 6       | 3     | 2v2v2        | 4                | 12            | 18                                 |

**Worst case the server ticks: 18 entities + ~24 projectiles + up to 4 siege structures = ~46 simulated actors at 20 Hz.** Client renders the same. This is the number the entity budget in §8 is built around.

**Why these numbers:** with 2 buddies each in a 6-player FFA, losing one costs you half your army — permanent death has weight. In a 3v3 with 5 per team, a team can field a real composition (2 frontline + 2 ranged + 1 support) and still feel a loss. The cap rises as team count falls because fewer teams means fewer simultaneous fights, so more entities per fight is affordable *and* more interesting.

**Bots (D-05).** Bots fill empty slots so a 2-player room still feels alive, but they are **filler, not contenders**. Two rules follow:

1. Bots never appear on a team that has no human on it *if* a human is still playing — they only top up existing teams to the mode's player count.
2. **The moment the last human is eliminated, the match ends immediately with `winner = None`.** Friendly bots surviving does not extend the match, and a bot team destroying the final human core does not produce a winner. The results screen says "no human survivor", which is honest about what happened rather than awarding a victory to an AI.

This is the right call for a 2–6 player game: it stops a bot from "winning" a match a human was watching, and it stops a losing human from waiting three minutes for bots to finish a fight they no longer care about.

### 4.8 Combat and base math

| Stat                      | Value                                                                        |
| ------------------------- | ---------------------------------------------------------------------------- |
| Player HP                 | 100                                                                          |
| Respawn                   | 8 s at own base, 2 s spawn protection                                        |
| Fists                     | 8 dmg / 0.5 s = 16 DPS, reach 2.2 m                                          |
| Sword                     | 25 dmg / 0.45 s = 55 DPS                                                     |
| Spear                     | 20 dmg / 0.55 s = 36 DPS, reach 3.6 m                                        |
| Bow                       | 30 dmg projectile, 40 m/s, gravity 0 (v1: straight-line, simpler to predict) |
| Staff                     | 40 dmg AoE r=2.5                                                             |
| **Player TTK with sword** | 100 / 55 = **1.8 s**                                                         |
| **Player TTK with fists** | 100 / 16 = **6.3 s**                                                         |
| Core HP                   | **10,000**                                                                   |
| Core damage multiplier    | **0.35** (all sources)                                                       |
| Core regen                | **0** (deliberate: chip damage accumulates and creates late-game pressure)   |


**Time-to-destroy the core:**

| Attacker                           | Raw DPS   | vs core (×0.35) | Time to 10,000              |
| ---------------------------------- | --------- | --------------- | --------------------------- |
| 1 player, fists                    | 16        | 5.6             | **29.8 min** — not viable ✔ |
| 1 player, sword                    | 55        | 19.3            | 8.7 min — not viable solo ✔ |
| 4 players, swords                  | 220       | 77              | 130 s                       |
| 4 players + ram                    | 220 + 125 | 121             | 83 s                        |
| 4 players + ram + 4 tier-2 buddies | ~430      | ~150            | **67 s**                    |

So: **a coordinated push needs ~70–90 seconds of uncontested presence at the core.** With any defence at all, that becomes 3–5 minutes. This is the single most important number in the game's pacing — it must be re-measured in playtest (T-060) and adjusted only via `core_hp` and `core_damage_multiplier`.

**Elimination (D-02, instant):** core HP ≤ 0 → the team's pad and all its crates are destroyed → **every living player on that team dies on the same tick**, no exceptions and no grace period → 2 s defeat freeze → defeat screen → leave match. **There is no spectator mode.** Eliminated players cannot reconnect into the match, and their buddy slots are freed. The winning teams continue until one remains.

### 4.9 Feedback without destruction ("not flat")

No debris, no block shatter physics, no ragdolls. Everything below is ≤ 2 ms of frame budget.

| Effect | Trigger | Implementation | Perf risk |
|---|---|---|---|
| Hit flash | Any damage event | Vertex-colour pulse on the instanced mesh, 80 ms, one uniform write | none |
| Hitstop | Melee connect | Freeze *presentation* (not authority) for 60 ms — camera and animation only | none |
| Knockback | Melee connect | 2.5 m/s impulse away from attacker, decayed in sim | none |
| Camera shake | Damage taken, core hit | 120 ms, amplitude `min(dmg/50, 1.0) × 0.06 m`, Perlin offset on camera only | none |
| Damage numbers | Any damage | Canvas sprite batch, **cap 32 concurrent**, oldest evicted | low (1 draw call) |
| **Airdrop telegraph** | 5 s before impact | Descending shadow + smoke flare + HUD marker + rising audio sweep. **This is the game's main attention hook** and must be unmissable at any camera angle | low |
| **Airdrop landing** | Impact | Dust ring (16 particles), ground thump with ±5% pitch jitter, brief screen-edge flash if within 15 m | low |
| **Crate pickup** | Pickup tick | Small "clack" per crate, carried-stack model grows visibly | none |
| **Pad deposit** | Each crate placed | Rising pitch per consecutive deposit (satisfying "filling" feedback) | none |
| Craft complete | Recipe matched | Vertical light beam 4 m, 12 rising particles, 3-note chord | low |
| Base under attack | Core damaged | Screen-edge red vignette pulse + 55 Hz sub-bass swell (AudioWorklet) | none (GPU composite) |
| Buddy death | HP → 0 | Alpha dissolve 300 ms + role-specific death cry + kill feed entry | none |
| Low HP | HP < 30% | Desaturated edges + heartbeat loop at 90 BPM | none |
| Victory / defeat | Match end | Slow-motion 0.35× for 1.5 s, then results. **Defeat is abrupt by design (D-02)** — the slow-motion is what stops it feeling like a crash | none |

**Reduced-motion mode** (accessibility, T-032) disables shake, vignette pulse, and slow-motion. It must not disable hit flash, damage numbers, or the airdrop telegraph — those carry gameplay information.

### 4.10 Lobby and session flow

```
Boot
 ├─ Offline  →  mode select (FFA / Teams) → team size → map seed → local room (wasm, bots fill)
 └─ Online
      ├─ Create Room → room code (6 chars, Crockford base32) → lobby (share code)
      └─ Join Room   → enter code → lobby
                          ↓
                    Lobby: players list, team assign (auto-balance, manual swap),
                           mode display, buddy cap display, ready toggle
                          ↓ all ready & ≥ 2 players
                    3 s countdown → Live → (core destroyed) → Defeat/Victory → Leave
```

**Offline is not a stub.** It runs the same `room` crate in a Web Worker with `crates/room::bot` filling empty slots. It exists from Milestone 2 (T-048) because it is the only way to test whether the game is fun before the netcode exists, and because it is what makes the project demonstrable at zero hosting cost.

### 4.11 Tuning table (single source of truth)

Everything above that a designer would want to change lives in `data/tuning.ron`, loaded by `sim` at room creation and by the client at boot:

```ron
(
  match: ( target_minutes: 20, overtime_start: 22, decay_per_30s: 0.02 ),
  player: ( hp: 100, respawn_s: 8, spawn_protect_s: 2, speed: 4.5 ),
  core: ( hp: 10000, damage_mult: 0.35, regen: 0 ),
  airdrop: ( first_drop_s: 10, interval_early_s: 25, interval_late_s: 15,
             late_after_s: 300, crates_per_player: 3, heavy_every: 5,
             heavy_mult: 1.75, telegraph_s: 5, fall_speed: 12.0,
             pickup_radius: 1.2, carry_cap: 12, drop_loss: 0.5,
             pickup_ttl_s: 180, max_pickups: 120 ),
  pad: ( size: 8, max_height: 4, deposit_rate: 6 ),
  terrain: ( breakable: false ),
  caps: { /* §4.7 matrix */ },
  buddies: [ /* §4.6 table */ ],
  recipes: [ /* §4.5.3 table */ ],
)
```

A CI test asserts `tuning.ron` parses and that every recipe in it has a shape whose canonical form is unique — a duplicate shape would make one recipe unreachable, which is a silent content bug.

---

## 5. Technical architecture

### 5.1 Stack decision

| Layer           | Choice                                               | Why this, and what was rejected                                                                                                                                                                                                                                                                                                                                        |
| --------------- | ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Simulation      | Rust, `crates/sim`, `no_std`-compatible, fixed-point | Must run identically in wasm (client prediction) and native (server authority). Nothing else gives one implementation with no FFI tax.                                                                                                                                                                                                                                 |
| World / meshing | Rust, `crates/world` → wasm                          | Greedy meshing over a 16³ chunk is 10–50× faster in Rust than JS, and it runs off the main thread budget.                                                                                                                                                                                                                                                              |
| Rendering       | **TypeScript + three.js on WebGL2**                  | **Rejected: Bevy.** A minimal Bevy 3D wasm build lands ~2.0–3.0 MB brotli against a 1.2 MB initial-wasm budget, and its ECS/renderer is dead weight when the world is already a flat voxel array we mesh ourselves. three.js core is ~170 KB gzipped, gives instancing, frustum culling and an atlas pipeline for free, and keeps the wasm payload to simulation only. |
| Shell / HUD     | TypeScript + DOM + CSS                               | Text in the DOM, pixels on the canvas. No text rendering in Rust that a `<div>` could do.                                                                                                                                                                                                                                                                              |
| Transport       | WebSocket, binary frames, one message per frame      | **Rejected for v1: WebTransport.** Chromium-only in practice, needs HTTP/3, and Durable Object support is not something to bet a launch on. Migration path in §6.1.                                                                                                                                                                                                    |
| Room authority  | Rust `crates/room` — pure state machine, no I/O      | The whole portability story. Testable without a network, runnable in a DO, in Axum, and in a Web Worker for offline mode.                                                                                                                                                                                                                                              |
| Production host | Cloudflare Durable Object (`workers-rs`)             | Free, no card, global, one object = one room actor = exactly the concurrency model the game needs. Ceiling documented in §9.                                                                                                                                                                                                                                           |
| Fallback host   | Axum + tokio on a free VM                            | Same `room` crate, different adapter. Activated by config, not by refactor.                                                                                                                                                                                                                                                                                            |
| Client hosting  | Vercel Hobby, static output                          | 100 GB/month transfer; a 2.1 MB cold load budget allows ~47k cold loads/month.                                                                                                                                                                                                                                                                                         |
| Persistence     | Durable Object SQLite (match history, guest tokens)  | Nothing else is needed in v1. Postgres is a post-launch concern (§6.9).                                                                                                                                                                                                                                                                                                |

**The one rule that keeps this coherent:** the boundary between Rust and TypeScript is a *data* boundary, never a *logic* boundary. JS never decides whether a move is legal, whether a recipe matches, or whether a hit lands. It draws what Rust hands it and forwards what the player pressed.

### 5.2 Topology

```
┌──────────────────────────── Browser ────────────────────────────┐
│  main thread                          Web Worker (optional)     │
│  ┌──────────────┐  ┌──────────────┐   ┌──────────────────────┐  │
│  │ DOM shell    │  │ three.js     │   │ meshing + worldgen   │  │
│  │ HUD / lobby  │  │ WebGL2       │   │ (crates/world → wasm)│  │
│  └──────┬───────┘  └──────▲───────┘   └──────────▲───────────┘  │
│         │                 │ typed-array views    │              │
│         │        ┌────────┴──────────────────────┴──────────┐   │
│         └───────►│  wasm core (crates/sim + client-wasm)     │   │
│                  │  prediction · input pack · render buffer  │   │
│                  └────────┬──────────────────────────────────┘   │
│                           │ Uint8Array (postcard)                │
└───────────────────────────┼──────────────────────────────────────┘
                            │ WSS (binary)
        ┌───────────────────▼────────────────────┐
        │  Room host (one per match)             │
        │  crates/room  — pure state machine     │
        │  ├─ host-worker: Durable Object        │
        │  └─ host-axum: tokio + axum (fallback) │
        └───────────────────┬────────────────────┘
                            │ SQLite (DO storage)
                    ┌───────▼────────┐
                    │ match history  │
                    └────────────────┘

Static: Vercel Hobby (client bundle, wasm, assets)
```

### 5.3 Crate layout

```
cratebound/
├── Cargo.toml                     # [workspace] members = crates/*
├── rust-toolchain.toml            # pinned channel, wasm32-unknown-unknown target
├── crates/
│   ├── sim/                       # THE RULES. no I/O, no std-only deps, no floats in state
│   │   ├── src/lib.rs
│   │   ├── src/fixed.rs           # Q16.16 wrapper, saturating ops, no f32 anywhere
│   │   ├── src/ids.rs             # EntityId(u32), PlayerId(u8), TeamId(u8) newtypes
│   │   ├── src/rng.rs             # xoshiro256**, seedable, no OS entropy
│   │   ├── src/input.rs           # InputFrame, bitpacked
│   │   ├── src/movement.rs        # AABB-vs-voxel sweep, gravity, step-up
│   │   ├── src/combat.rs          # damage events, cooldowns, projectiles
│   │   ├── src/recipe.rs          # crate cluster -> canonical form -> match  <- HIGHEST RISK
│   │   ├── src/airdrop.rs         # drop schedule, site selection, pickup/carry/deposit
│   │   ├── src/buddy.rs           # role stats, AI state machine
│   │   ├── src/pathfind.rs        # A* on the voxel grid, budgeted per tick
│   │   ├── src/base.rs            # core HP, elimination transition
│   │   ├── src/match_rules.rs     # modes, team assignment, buddy caps, win check
│   │   └── src/state.rs           # WorldState, the single authoritative struct
│   ├── world/
│   │   ├── src/chunk.rs           # paletted 16³ storage + RLE
│   │   ├── src/gen.rs             # seeded terrain, biomes, base pads, airdrop site grid
│   │   ├── src/mesh.rs            # greedy meshing + per-vertex AO
│   │   └── src/diff.rs            # per-chunk edit log, compact encoding
│   ├── protocol/
│   │   ├── src/lib.rs             # message enums, postcard, PROTOCOL_VERSION
│   │   └── src/codec.rs           # encode/decode + size guards
│   ├── room/                      # pure: apply(input) -> advance(tick) -> snapshots
│   │   ├── src/lib.rs
│   │   ├── src/lifecycle.rs       # Lobby→Ready→Countdown→Live→Ended→Cleanup
│   │   ├── src/snapshot.rs        # per-client delta builder, AOI filtering
│   │   └── src/bot.rs             # deterministic bot brains (used offline + load tests)
│   ├── client-wasm/               # wasm-bindgen surface. THIN. ≤15 exports.
│   │   ├── src/lib.rs
│   │   ├── src/render_buffer.rs   # flat structs written to linear memory
│   │   └── src/predict.rs         # input ring buffer, reconciliation
│   ├── host-worker/               # workers-rs Durable Object adapter
│   │   └── src/lib.rs
│   └── host-axum/                 # tokio adapter, local dev + fallback
│       ├── src/main.rs
│       └── src/ws.rs
├── apps/web/
│   ├── index.html
│   ├── src/main.ts                # boot, canvas, wasm init
│   ├── src/render/                # chunk manager, entity renderer, sky, fog
│   ├── src/game/                  # input, net client, prediction glue
│   ├── src/ui/                    # HUD, lobby, recipe book, settings
│   ├── src/audio/
│   └── public/assets/             # atlas.ktx2 / atlas.png, sfx/*.ogg, fonts
├── tools/
│   ├── recipe-editor/             # local-only Vite app to author + validate recipes
│   └── bot-swarm/                 # N headless clients for load + desync testing
├── tests/
│   ├── golden/                    # state-hash fixtures, per seed
│   └── e2e/                       # Playwright specs
├── .github/workflows/ci.yml
└── vercel.json
```


### 5.4 The wasm ↔ JS boundary

This is the part that decides whether the game runs at 60 fps or 24. Rules:

1. **No `JsValue` in the hot path.** Nothing per-entity crosses as an object. Rust writes flat structs into linear memory; JS reads them through typed-array views created once and reused.
2. **Crossing budget: ≤ 50 calls per frame, target < 20.** One call in (input), one call out (tick), one call to fetch the dirty-chunk list, one call to fetch the render buffer descriptor. Everything else is memory reads.
3. **The buffer never moves under JS.** All hot buffers are allocated once at boot at a fixed capacity with `Vec::with_capacity` + `set_len`, and the wasm memory is grown *before* views are taken (`memory.grow` detaches every existing `ArrayBuffer` — the single most common way this class of project breaks). A guard task asserts the growth policy.
4. **Ownership of chunk meshes is explicit.** Rust owns the bytes until it hands over a `(ptr, len, generation)` triple; JS copies into a `BufferAttribute` and immediately calls `release_mesh(generation)`. Rust may then reuse the allocation. No aliasing, no dangling views.

Exact signatures and struct layouts: §8.4.

### 5.5 Determinism model

| Concern                        | Rule                                                                                                                                   |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| Position, velocity, HP, timers | `i32` fixed-point, Q16.16 (`1.0 == 65536`). Never `f32`.                                                                               |
| Trig / sqrt / atan2            | Table-driven integer approximations in `fixed.rs`, same tables both targets.                                                           |
| Randomness                     | `xoshiro256**` seeded per room; every consumer draws from a per-subsystem stream so ordering changes don't cascade.                    |
| Iteration order                | `BTreeMap`/`Vec` only. No `HashMap` in any code path that affects state.                                                               |
| Time                           | Integer tick counter. Wall clock never enters `sim`.                                                                                   |
| Verification                   | `state_hash()` — FNV-1a over a canonical byte serialisation — asserted equal between native and wasm for identical input logs (T-015). |

Float is allowed in exactly three places: camera, audio, and interpolation for *display only*. If a float ever feeds back into state, that's a bug with a name: `desync`.

---

## 6. Netcode

### 6.1 Transport

**WebSocket, binary, one protocol message per frame.** Rationale: universal browser support, native support in Durable Objects, works through every corporate proxy and mobile carrier NAT, and needs no COOP/COEP headers.

Rejected for v1: **WebTransport**. Chromium-only in practice (no Safari, no Firefox stable), requires HTTP/3 end-to-end, and Durable Object support is not something to depend on. The migration path exists and costs one adapter: the `Transport` trait in `apps/web/src/game/net/transport.ts` has two implementations (`WsTransport`, `WebTransportTransport`), and the protocol is transport-agnostic because it is just length-delimited bytes. If WebTransport ever becomes universal, it is a feature flag, not a refactor.

**Framing:** each WebSocket frame carries exactly one message. First byte is the message tag, remaining bytes are postcard-encoded payload. No length prefix (the WebSocket frame already delimits), no compression at the protocol layer (`permessage-deflate` is disabled — it costs CPU per message and the payloads are already compact; the transport is TLS-compressed at the record layer).

### 6.2 Tick model

| Parameter            | Value                    | Reasoning                                                                                                                                                                                                                  |
| -------------------- | ------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Server tick          | **20 Hz** (50 ms)        | Below this, melee combat feels unresponsive; above it, cost rises linearly for no perceptible gain in a game with 1.8 s TTK.                                                                                               |
| Client input rate    | **15 Hz**                | Each packet carries the last **3** input frames, so a dropped packet costs nothing (the next packet re-sends them). 15 Hz cuts free-tier message cost by 25% versus 20 Hz for a 1-tick latency difference nobody can feel. |
| Snapshot rate        | **20 Hz** per client     | Matches the tick.                                                                                                                                                                                                          |
| Snapshot keyframe    | Every **60 ticks** (3 s) | Bounds recovery time from packet loss.                                                                                                                                                                                     |
| Max players per room | 6                        | §4.7                                                                                                                                                                                                                       |

**Input application rule:** each player has a queue of received input frames keyed by `(seq, tick)`. Each tick, the server consumes the *newest* frame whose `tick` ≤ current tick, and discards older ones. If no input arrives for 200 ms (4 ticks), the player is treated as holding no keys (not as "last input forever") — this prevents a disconnected player from running into a wall at full speed, and prevents a laggy player from getting free movement.

### 6.3 Snapshot content and interest management

A client is told about:

| Category                                              | Scope                                                                    |
| ----------------------------------------------------- | ------------------------------------------------------------------------ |
| Own player                                            | Always, full fidelity                                                    |
| Other players                                         | Within **48 m** (3 chunk columns) horizontally, any Y                    |
| Buddies                                               | Same 48 m                                                                |
| Projectiles                                           | Same 48 m                                                                |
| Siege structures                                      | Same 48 m                                                                |
| Block edits                                           | Only chunks inside the client's loaded set (96 m radius)                 |
| Global events (recipe crafted, base hit, elimination) | Always — these are the game's information layer and must never be culled |

48 m is the render distance (96 m diameter), so the client is never told about something it cannot see, and never sees something it hasn't been told about. Entities crossing the boundary are handled by a 4 m hysteresis band to stop pop-in/out flicker at the edge.

**Delta encoding:** each snapshot is encoded against the last snapshot the client **acknowledged** (`baseline_seq`). Fields that did not change are omitted via a presence bitmask per entity. A keyframe (full state) is sent every 60 ticks, or immediately if the client NACKs a missing baseline.

### 6.4 Client prediction and reconciliation

```
Client frame:
  1. sample input → InputFrame { buttons, yaw, pitch, move_x, move_z }
  2. append to ring buffer (capacity 128) with seq++
  3. sim::step_local(state, input)          ← same code the server runs
  4. render from predicted state

On snapshot(server_tick, acked_seq, server_state):
  5. drop ring buffer entries with seq <= acked_seq
  6. error = |predicted_pos_at(acked_seq) - server_pos|
  7. if error > 0.05 m:
        state = server_state
        for each remaining buffered input: sim::step_local(state, input)
        visual_offset += (old_predicted_pos - state.pos)   ← smoothing residual
  8. decay visual_offset by exp(-dt / 0.15) each frame
```

**Threshold rationale:** 5 cm is below the voxel grid's visual granularity at typical camera distance, so corrections under 5 cm are invisible and not worth a replay. The 150 ms exponential smoothing constant means a correction decays to 2% in ~0.6 s — visible as a slight glide, never a snap.

**300 ms RTT spike:** the client keeps predicting; the server keeps simulating with stale input (up to 200 ms, then treats as no-input). On recovery, the replay window is bounded at 128 inputs (8.5 s at 15 Hz) — beyond that the client hard-resyncs to the server state and accepts a visible correction. This is the correct tradeoff: a visible correction after a 9-second freeze is better than replaying 9 seconds of input on a mobile CPU.

### 6.5 Entity interpolation

Remote players and buddies are rendered **100 ms in the past** (2 snapshots at 20 Hz). The interpolation buffer holds the last 4 snapshots; render time `t = now - 100 ms` is interpolated between the two bracketing snapshots. If `t` is past the newest snapshot (packet loss), extrapolate for at most **100 ms**, then freeze in place. Never extrapolate further — a buddy sliding through a wall is worse than a buddy standing still.

Local player is never interpolated; it is predicted (§6.4).

### 6.6 Block edits — authority and conflict

```
Client: player aims at block, presses break
  → optimistic: remove block locally, play puff, decrement nothing (inventory is server-owned)
  → send C_EDIT { seq, x, y, z, action }
  → store pending edit (seq → previous block) for rollback

Server: on C_EDIT
  → validate: distance ≤ 6.0 m from authoritative player pos
  → validate: block exists / is placeable, not in protected volume
  → validate: rate ≤ 8 edits/s (token bucket)
  → apply to sim state, append to chunk diff log
  → broadcast S_EDIT { x, y, z, block, server_seq } to all clients with that chunk loaded
  → reply S_EDIT_ACK { seq, accepted }

Client: on S_EDIT_ACK(reject) → revert local change from the pending map
Client: on S_EDIT → if server_seq newer than last seen for that chunk, apply
```

**Conflict (two players edit the same block in the same tick):** edits are applied in server arrival order; the second edit sees the post-first-edit state and is validated against it (e.g. placing into a now-occupied cell is rejected). Last-write-wins is *not* used — it produces ghost blocks on clients that missed one message. Every accepted edit is an absolute assignment with a monotonically increasing `server_seq` per chunk, and clients apply only if `server_seq` is newer than what they have for that chunk.

### 6.7 World streaming without downloading the world

The client generates terrain from the seed using `crates/world::gen` — byte-identical to the server's generation, verified by a golden-hash test (T-015). The server therefore only ever sends **diffs**.

```
S_CHUNK_DIFF {
  cx: i16, cz: i16,           // chunk column
  version: u32,               // server's edit count for this column
  count: u16,
  entries: [(index: u16, block: u8); count]   // index = local x,y,z packed
}
```

**Because terrain is indestructible (§4.4.4) and crates exist only inside 8×8×4 pads, the diff budget is now trivial:**

| Case | Cells that can change in one column | Encoded size |
|---|---|---|
| Typical base column | ~60 crate placements | ~180 B |
| Worst case (column fully occupied by a pad) | 8 × 8 × 4 = 256 | **768 B** |
| Theoretical max (before v1.1, destructible terrain) | 24,576 | 72 KB |

- **Budget rule:** a chunk diff is split across frames so no snapshot exceeds **1.5 KB of diff payload**. At the new worst case that ceiling is never approached — it is retained because the decoder should still be defensive, not because it will trigger.
- Diffs are prioritised by distance to the player, so the pad you are standing next to always arrives first.
- The keyframe-column fallback (`> 8 KB of diffs`) is now effectively dead code. Keep it, test it, and treat it as insurance against a future `terrain.breakable = true` change rather than as a live path.

**Consequence:** joining a match mid-way transfers a few hundred bytes of edits instead of a 65,536 m² world. This is the difference between a 2-second join and a 40-second one — and after D-01 it is not even close.

### 6.8 Bandwidth budget

Per client, 6-player match, steady state:

| Category                       | Rate   | Size       | Bytes/s down    |
| ------------------------------ | ------ | ---------- | --------------- |
| Snapshots (delta)              | 20/s   | ~550 B avg | 11,000          |
| Block edits (broadcast)        | ~2/s   | 14 B       | 28              |
| Chunk diffs (streaming)        | bursty | —          | ~1,500 avg      |
| Events (craft, kill, base hit) | ~0.5/s | 40 B       | 20              |
| **Total down**                 |        |            | **≈ 12.5 KB/s** |
| Inputs up                      | 15/s   | 30 B       | 450 B/s         |


12.5 KB/s × 6 players = 75 KB/s per room = **270 MB/hour per room** at the transport layer. This is why Vercel is the right host for the client and the wrong host for the game: 270 MB/hour would consume Vercel's 100 GB monthly transfer in ~370 room-hours.

The 2 KB snapshot ceiling (§9 budget) is respected with ~3.6× headroom in the average case; the *worst* case (all 6 players + 12 buddies + 24 projectiles visible and all moving) is measured in T-053 and must stay under 2 KB.

### 6.9 Persistence

**In v1: almost nothing.** Rooms are ephemeral; there is no world persistence, no accounts (A-02), no inventory between matches.

Persisted, in Durable Object SQLite:

```sql
CREATE TABLE matches (
  id TEXT PRIMARY KEY, mode TEXT, player_count INTEGER,
  started_at INTEGER, ended_at INTEGER, winner_team INTEGER, duration_s INTEGER
);
CREATE TABLE match_players (
  match_id TEXT, name TEXT, team INTEGER, kills INTEGER, deaths INTEGER,
  buddies_spawned INTEGER, base_damage INTEGER,
  PRIMARY KEY (match_id, name)
);
CREATE TABLE guest_tokens (
  token_hash TEXT PRIMARY KEY, name TEXT, created_at INTEGER, last_seen INTEGER
);
```

Rows written per match: 1 + 6 + 6 = 13. At ~18 matches/day that is 234 rows/day against a 100,000 rows/day budget — three orders of magnitude of headroom. Storage: ~2 KB per match, so 5 GB holds ~2.5 million matches.

**Not** using Postgres, Redis, or any external store in v1. Upstash's 500k commands/month is roughly 3 player-hours of tick traffic — it would be the first thing to break, and adding it for a room registry that a single SQLite table can serve is pure operational risk. If a public room list is ever needed (A-04), it goes in a `ROOMS` registry Durable Object, not in Redis.

### 6.10 Room lifecycle

```
        create
          │
          ▼
      ┌────────┐  all ready, ≥2 players   ┌───────────┐
      │ LOBBY  │ ───────────────────────► │ COUNTDOWN │ 3 s
      └────────┘ ◄─────────────────────── └───────────┘
          │        someone unreadies           │
          │                                     ▼
          │                              ┌───────────┐
          │                              │   LIVE    │ 20 Hz tick
          │                              └───────────┘
          │                                     │ core HP ≤ 0, or overtime end
          │                                     ▼
          │                              ┌───────────┐
          │                              │   ENDED   │ 10 s results
          │                              └───────────┘
          │                                     │
          └──────────── empty 60 s ─────────────┴──► CLEANUP (persist, delete DO storage)
```

Owned by `crates/room::lifecycle`, driven by the host adapter. The host only forwards bytes and schedules ticks; it makes **zero** game decisions.

### 6.11 Reconnect, elimination, and joins

- **Reconnect:** the client stores `{room_code, player_token}` in `sessionStorage`. On disconnect the server holds the player slot for **30 s**: the entity stays in the world and still takes damage, but no input is applied. Reconnecting within 30 s resumes control and sends a keyframe. After 30 s the slot is freed and the player is marked `ABANDONED` (their team continues short-handed).
- **Elimination (D-02):** when a team's core is destroyed, every living player on that team dies on the same tick. Their tokens are invalidated immediately, their snapshots stop, and they are shown a defeat screen with a leave button. **There is no spectator mode and no spectator snapshot stream** — an eliminated client is simply disconnected from the room's fan-out. This removes the spectator camera, the spectator HUD, and the "spectator receives positions" information-leak question entirely.
- **Join in progress:** not supported in v1. A match is sealed at countdown. With 6 players and 20-minute matches, a late joiner adds an entire class of state-sync bugs for a feature nobody needs.
- **Bot-only teams** continue to exist after their humans are gone, but see §4.8 — the match does not outlive the last human.

---

## 7. Protocol contract

`PROTOCOL_VERSION: u16 = 1`. A client whose version differs is rejected at `C_HELLO` with `S_ERROR { code: VERSION_MISMATCH, min: u16, max: u16 }` and a readable message, never a silent disconnect.

Encoding: **postcard** (serde). Fixed-size structs use `#[repr(C)]` + manual `Serialize` where byte layout matters; everything else uses derived encoding. All `i16`/`i32` fields are fixed-point (§5.5) where they represent game state.

### 7.1 Client → Server

| Tag    | Message          | Payload                                                                   | Size   | Rate      | Notes                                                                                                              |
| ------ | ---------------- | ------------------------------------------------------------------------- | ------ | --------- | ------------------------------------------------------------------------------------------------------------------ |
| `0x01` | `C_HELLO`        | `protocol: u16, name: [u8;16], room_code: [u8;6], token: Option<[u8;16]>` | ~44 B  | once      | `token` present = reconnect                                                                                        |
| `0x02` | `C_READY`        | `ready: bool`                                                             | 2 B    | on toggle |                                                                                                                    |
| `0x03` | `C_TEAM`         | `team: u8`                                                                | 2 B    | on toggle | lobby only, validated for balance                                                                                  |
| `0x04` | `C_INPUT`        | `seq: u16, first_tick: u32, frames: [InputFrame; 3]`                      | 30 B   | 15 Hz     | `InputFrame` = `buttons: u8, yaw: u8, pitch: u8, move: u8` (quantised: yaw 256 steps, move packed as 2×4-bit axis) |
| `0x05` | `C_EDIT`         | `seq: u16, x: i16, y: i16, z: i16, block: u8, action: u8`                 | 14 B   | ≤ 8 Hz    | action: 0=break, 1=place                                                                                           |
| `0x06` | `C_CRAFT_INTENT` | `x: i16, y: i16, z: i16`                                                  | 7 B    | on demand | optional UX hint only; the server matches on its own, this just lets it reply faster with a reason                 |
| `0x07` | `C_CHAT`         | `len: u8, text: [u8;120]`                                                 | ≤122 B | 0.5 Hz    | sanitised server-side                                                                                              |
| `0x08` | `C_PING`         | `t: u32`                                                                  | 5 B    | 1 Hz      | RTT measurement                                                                                                    |
| `0x09` | `C_REPORT`       | `kind: u8, tick: u32, hash: u32`                                          | 7 B    | on desync | telemetry only                                                                                                     |

### 7.2 Server → Client

| Tag    | Message        | Payload                                                                                                     | Size     | Rate         |
| ------ | -------------- | ----------------------------------------------------------------------------------------------------------- | -------- | ------------ |
| `0x81` | `S_WELCOME`    | `player_id: u8, team: u8, seed: u32, config: RoomConfig, tick: u32, your_token: [u8;16]`                    | ~120 B   | once         |
| `0x82` | `S_LOBBY`      | `players: [(u8, [u8;16], u8, bool)], settings`                                                              | ≤ 140 B  | on change    |
| `0x83` | `S_START`      | `tick0: u32, bases: [(u8, i16, i16, i16)]`                                                                  | ~40 B    | once         |
| `0x84` | `S_SNAPSHOT`   | `tick: u32, baseline_seq: u16, keyframe: bool, players[], buddies[], projectiles[], structures[], events[]` | ≤ 2048 B | 20 Hz        |
| `0x85` | `S_EDIT_ACK`   | `seq: u16, accepted: bool, reason: u8`                                                                      | 5 B      | per edit     |
| `0x86` | `S_EDIT`       | `x: i16, y: i16, z: i16, block: u8, server_seq: u32`                                                        | 14 B     | per edit     |
| `0x87` | `S_CHUNK_DIFF` | `cx: i16, cz: i16, version: u32, entries: [(u16, u8)]`                                                      | ≤ 1536 B | streaming    |
| `0x88` | `S_EVENT`      | `kind: u8, a: i32, b: i32, c: i32`                                                                          | 14 B     | event-driven |
| `0x89` | `S_END` | `winner_team: i8, reason: u8, stats: [...]` | ≤ 200 B | once | `winner_team = -1` means **no human winner** (D-05); `reason` distinguishes `CORE_DESTROYED` / `NO_HUMANS` / `OVERTIME` |
| `0x8A` | `S_ERROR`      | `code: u8, detail: [u8;64]`                                                                                 | ≤ 66 B   | on error     |
| `0x8B` | `S_PONG`       | `t: u32`                                                                                                    | 5 B      | 1 Hz         |

### 7.3 Snapshot wire layout

```
S_SNAPSHOT
├─ tick          u32
├─ baseline_seq  u16        (0 = keyframe)
├─ keyframe      u8
├─ player_count  u8   ─┐
├─ players[..]        │  7 B each delta:
│                     │  id u8 · presence u16 (bitmask) · [pos_x i32][pos_y i32][pos_z i32]
│                     │  [yaw u8][hp u8][flags u8][anim u8]
├─ buddy_count   u8   ─┤
├─ buddies[..]        │  6 B each delta: id u8 · presence u16 · pos deltas
├─ proj_count    u8   ─┤
├─ projectiles[..]    │  6 B each: id u16 · pos i16×3 · vel i8×3
├─ struct_count  u8   ─┤
├─ structures[..]     │  5 B each: id u8 · hp u8 (percent) · pos
└─ event_count   u8
   events[..]          4 B each: kind u8 · a u8 · b u8 · c u8
```

Position deltas are `i32` fixed-point **relative to the previous snapshot's value for that entity**, varint-encoded. A stationary entity costs 3 bytes total; a running one ~9. This is what makes 18 entities fit in ~550 bytes.

### 7.4 Validation rules (server-side, non-negotiable)

Every inbound message passes through `protocol::codec::decode_checked`:

1. Frame length ≤ **4096 B** (client→server). Larger = close connection, code 1009.
2. Unknown tag = close, code 1008.
3. Any `decode` error = close, not "ignore and continue" (a malformed stream is a bug or an attack; both deserve termination).
4. `C_EDIT` coordinates outside world bounds = reject with `reason=OUT_OF_BOUNDS`, do not close (players will legitimately aim at the sky).
5. `C_CHAT` text: strip to `[A-Za-z0-9 _.,!?-]{0,120}`, reject empty after strip.
6. Per-connection token bucket: **30 messages/s, burst 60**. Exceeded = close, code 1008.
7. `C_HELLO` name: `[A-Za-z0-9_-]{3,16}`, uniqueness enforced per room.

---

---

## 8. Client architecture

### 8.1 Threading model

```
┌─ Main thread ────────────────────────────────────────────────┐
│  three.js render loop (rAF)                                  │
│  DOM HUD (event-driven, 10 Hz max)                            │
│  wasm core: prediction, input pack, snapshot decode           │
│  net: WebSocket, postcard decode                              │
└──────────────────────────────────────────────────────────────┘
┌─ Worker (optional, feature-flagged) ─────────────────────────┐
│  worldgen + greedy meshing for chunks outside the near ring   │
└──────────────────────────────────────────────────────────────┘
```

**No `SharedArrayBuffer`.** It would require COOP/COEP headers on Vercel, which breaks third-party embeds and complicates deployment for zero benefit at this entity count. The worker communicates via `postMessage` with **transferable** `ArrayBuffer`s (zero-copy handoff). Chunk meshing on the main thread costs ~1.2 ms per column at 16×96×16 — acceptable for one column per frame, which is the streaming budget anyway. The worker is therefore an optimisation for the *initial* 169-column burst at load, not a steady-state requirement. Ship main-thread-only first; enable the worker if the first-playable budget is missed.

### 8.2 Render pipeline

| Decision        | Choice                                                                                                                          | Reasoning                                                                                                                                                                      |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Backend         | **WebGL2 only**                                                                                                                 | WebGPU is not on Safari iOS and still needs a WebGL2 fallback. Maintaining two renderers doubles the surface for a voxel game that is fill-rate bound, not feature bound.      |
| Chunk mesh unit | 16 × 96 × 16 column                                                                                                             | 169 meshes max, ~80 after frustum culling. Per-16³-cube meshing would be 1,014 meshes for identical pixels.                                                                    |
| Vertex format   | Interleaved, **12 bytes**: `pos u16×3` (chunk-local, scaled by 16), `normal u8` (6 values + 2 bits AO), `uv u16×2`, `tint u8×3` | 12 B/vertex at 300k tris ≈ 3.6 MB of VRAM per full view. Non-interleaved would be 4 buffers per chunk = 4× the draw-call state changes.                                        |
| Index buffer    | `Uint16` per column (max 65,535 verts; a column that exceeds it is split into two draws)                                        | `Uint32` doubles index bandwidth for no gain at this size.                                                                                                                     |
| Meshing         | Greedy, per face direction, with per-vertex AO                                                                                  | Greedy meshing cuts a typical terrain column from ~14k quads to ~1.8k.                                                                                                         |
| Lighting        | **Baked**: skylight + blocklight in the vertex `tint`, AO in the normal byte's spare bits                                       | A real lightmap needs a second UV set and a second texture fetch per fragment. At 1080p on a mobile GPU that is measurable; baked vertex light is free.                        |
| Shadows         | **Off**                                                                                                                         | A single 2048² cascade over 96 m is ~2.1 ms on a mid-range mobile GPU — 12% of the 16.6 ms budget for a look that a voxel game does not need. Contact darkening comes from AO. |
| Fog             | Linear, 64 m → 96 m, colour matched to sky                                                                                      | Hides the streaming boundary and lets render distance be 6 without looking like a wall.                                                                                        |
| Transparency    | Separate pass, back-to-front by chunk distance, no per-quad sort                                                                | Water and glass only. Per-quad sorting is O(n log n) per frame for a visual difference nobody notices in a voxel game.                                                         |
| Text            | **Never in the canvas.** DOM only.                                                                                              | Crisp at any DPR, free accessibility, free localisation.                                                                                                                       |


### 8.3 Entity rendering

Buddies and players are **instanced boxes with a 3-part hierarchy** (body, head, weapon) — no skeletal animation. This is an art-direction decision as much as a performance one: voxel characters that squash, tilt, and bob read better than low-poly rigs at 16 px, and cost ~0.

| Entity type           | Technique                                                       | Instances | Cost          |
| --------------------- | --------------------------------------------------------------- | --------- | ------------- |
| Players (6)           | `InstancedMesh`, 3 sub-meshes                                   | 18        | 3 draw calls  |
| Buddies (≤12)         | `InstancedMesh` per role, 3 sub-meshes each                     | 36        | 18 draw calls |
| Projectiles (≤24)     | One `InstancedMesh` of a quad, billboarded in the vertex shader | 24        | 1 draw call   |
| Particles (≤256)      | One `InstancedMesh`, GPU-side lifetime in the vertex shader     | 256       | 1 draw call   |
| Siege structures (≤4) | Regular meshes                                                  | 4         | 4 draw calls  |

**Total entity draw calls: ~27.** Plus ~80 chunk draws + 1 skybox + 1 HUD canvas = **~110 draw calls**, under the 120 gate.

Animation is driven by a per-instance `anim_phase` float written by Rust into the render buffer: `y_offset = sin(phase) * 0.06`, `pitch = clamp(velocity_z * -0.1, -0.25, 0.25)`. Attack = a 120 ms scale pulse on the weapon sub-mesh. Death = alpha dissolve, which needs a per-instance alpha attribute — one extra `float` in the instance buffer.

### 8.4 The wasm boundary — exact contract

**Exported functions (13, deliberately):**

```rust
#[wasm_bindgen] pub fn cb_protocol_version() -> u32;
#[wasm_bindgen] pub fn cb_init(seed: u32, tuning_ptr: u32, tuning_len: u32, mode: u8) -> u32;
#[wasm_bindgen] pub fn cb_tick(dt_ms: u32, input_ptr: u32) -> u32;      // → render desc ptr
#[wasm_bindgen] pub fn cb_on_snapshot(ptr: u32, len: u32) -> u32;
#[wasm_bindgen] pub fn cb_local_edit(x: i16, y: i16, z: i16, block: u8, action: u8) -> u32;
#[wasm_bindgen] pub fn cb_ack_edit(seq: u16, accepted: bool) -> u32;
#[wasm_bindgen] pub fn cb_dirty_chunks() -> u32;                        // → [u32; 4] per entry
#[wasm_bindgen] pub fn cb_mesh_chunk(cx: i16, cz: i16, out_ptr: u32, out_cap: u32) -> u32;
#[wasm_bindgen] pub fn cb_release_mesh(generation: u32) -> u32;
#[wasm_bindgen] pub fn cb_render_desc() -> u32;                          // → RenderDesc*
#[wasm_bindgen] pub fn cb_set_quality(level: u8) -> u32;
#[wasm_bindgen] pub fn cb_state_hash() -> u32;
#[wasm_bindgen] pub fn cb_mem_alloc(size: u32) -> u32;
```

Every return value is a `u32` pointer-or-length, never a `JsValue`. Zero allocations cross the boundary per frame.

**Render buffer layout** (`cb_render_desc()` returns a pointer to this header; arrays follow immediately):

```rust
#[repr(C)] struct RenderDesc {
  magic: u32,            // 0xCBA7E001 — guards against reading a stale buffer
  tick: u32,
  player_count: u32, buddy_count: u32, proj_count: u32,
  particle_count: u32, event_count: u32, dirty_count: u32,
  player_ptr: u32, buddy_ptr: u32, proj_ptr: u32,
  particle_ptr: u32, event_ptr: u32,
}

#[repr(C)] struct GpuPlayer {   // 32 B, stride fixed
  id: u16, team: u8, flags: u8,
  x: f32, y: f32, z: f32,
  yaw: f32, pitch: f32, anim: f32,
  hp: u8, role: u8, pad: u16,
}
```

`f32` here is **display only** — Rust converts fixed-point to float at the boundary, once per entity per frame, and nothing reads it back into state.

**The `memory.grow` trap.** Growing wasm memory detaches every existing `ArrayBuffer` view, silently turning every cached `Float32Array` into a zero-length array. The rule:

1. All hot buffers are pre-allocated at `cb_init` at their maximum size (18 players, 64 buddies, 128 projectiles, 512 particles).
2. `cb_mem_alloc` may grow memory, but is **only** called during `cb_init` and during chunk-mesh staging — never mid-frame.
3. After any `cb_mem_alloc`, the JS side calls `rebuildViews()` which recreates every typed-array view from `memory.buffer`. A dev-mode assertion checks `view.buffer.byteLength !== 0` at the start of every frame and throws a loud error naming this exact bug.

**Chunk mesh handoff (ownership is explicit):**

```
JS: ptr = wasm.cb_mem_alloc(cap)
    written = wasm.cb_mesh_chunk(cx, cz, ptr, cap)
    // Rust wrote `written` bytes into wasm memory at ptr
    copy = new Uint8Array(memory.buffer, ptr, written).slice()   // JS now owns a copy
    geom.setAttribute('position', new BufferAttribute(copy, ...))
    wasm.cb_release_mesh(generation)
```

The `.slice()` is one memcpy per chunk (~40 KB) at a rate of ~1 chunk/frame. It is the price of never holding a view into wasm memory that three.js still references — the alternative is a dangling buffer after the next `grow`, which manifests as corrupted geometry in one chunk out of two hundred and is essentially undebuggable.

### 8.5 Mobile performance plan

| Control               | Rule                                                                                                                                                                                                                            |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| DPR                   | `dpr = min(devicePixelRatio, 2.0)`. Never render above 2× — a 3× phone at 1080p logical would render 9× the pixels for no visible gain.                                                                                         |
| Dynamic resolution    | Track a 60-frame rolling median frame time. If `median > 14 ms` for 1 s → scale ×0.85. If `median < 10 ms` for 2 s → scale ×1.05. Clamp `scale ∈ [0.55, 1.0]`. Asymmetric hysteresis (fast down, slow up) prevents oscillation. |
| Quality tiers         | `LOW / MED / HIGH` set: render distance (4/5/6 chunks), particle cap (64/128/256), entity animation (off/on/on), fog start (40/52/64 m). Auto-selected by a 2-second boot benchmark; user-overridable.                          |
| Thermal response      | If frame time degrades >25% over a 60 s window *without* a scene change, drop one quality tier permanently for the session. Mobile throttling is not recoverable by waiting.                                                    |
| JS main-thread budget | ≤ 4 ms/frame for everything outside the GPU: input, prediction tick, snapshot decode, HUD. Measured with `performance.measure('frame-work')`.                                                                                   |
| First playable        | See §9.4 — target 3 s, ceiling 5 s.                                                                                                                                                                                             |

**Frame budget breakdown at 60 fps (16.6 ms):**

| Stage                   | Budget | Notes                                                                     |
| ----------------------- | ------ | ------------------------------------------------------------------------- |
| wasm tick + prediction  | 1.5 ms | 18 entities, no I/O                                                       |
| Snapshot decode         | 0.8 ms | postcard, ~550 B                                                          |
| Chunk streaming (1 col) | 1.2 ms | amortised, only when moving                                               |
| three.js scene update   | 2.0 ms | matrix updates, culling                                                   |
| GPU                     | 8.0 ms | measured indirectly via `EXT_disjoint_timer_query_webgl2` where available |
| Headroom                | 3.1 ms | for GC and OS jitter                                                      |

### 8.6 Input

**Desktop:** pointer lock on click, WASD, mouse look, `1–9` hotbar, scroll to cycle, LMB break, RMB place, `Q` drop, `Tab` scoreboard, `E` recipe book, `F` buddy stance, `Esc` menu.

**Mobile — the hard problem is break-vs-place disambiguation.** Solving it with tap-vs-hold is ambiguous and produces rage. The solution is an **explicit mode**:

- The hotbar is a two-part control: a **tool slot** (pickaxe) and **block slots**. The selected slot *is* the mode.
- Tap anywhere on the world → acts according to the selected slot. Pickaxe selected = break. Block selected = place.
- A single tap on the hotbar's pickaxe icon switches to break mode; tapping a block switches to place mode. No modal dialog, no long-press.
- Crosshair shows the current action icon, and the targeted block gets a wireframe highlight, so the mode is always visible before you commit.

| Control       | Placement                                                                      |
| ------------- | ------------------------------------------------------------------------------ |
| Move          | Virtual stick, left half, appears where the thumb lands (not a fixed position) |
| Look          | Drag on right half, 0.35°/px sensitivity, independent of move stick            |
| Break / place | Tap (mode-dependent, above)                                                    |
| Jump          | Dedicated button, bottom-right, 64 px                                          |
| Hotbar        | Horizontal strip, bottom-centre, 9 slots, 44 px each                           |
| Recipe book   | Slide-up sheet, opens from a button above the hotbar                           |
| Buddy stance  | Long-press on the buddy list entry → 3-option radial                           |

**Accessibility:** full key/button remapping; UI scale 80–150%; colourblind-safe team colours (blue/orange instead of red/green for team identity — the red/green convention is reserved for damage direction, which uses shape as well as colour); **reduced motion** disables shake, vignette pulse, and slow-motion; damage direction indicators use distinct shapes per quadrant so they survive both colourblindness and reduced motion.

### 8.7 HUD and shell

**Framework: none. Vanilla TypeScript + a 60-line reactive store.** React would add ~45 KB gzipped and, more importantly, invite per-frame re-renders of a HUD that updates 20 times a second. The HUD is ~8 components with text and bars; a hand-written store that batches DOM writes into one `requestAnimationFrame` callback is both smaller and structurally incapable of the re-render storm.

| Rule                                                                               | Enforcement                                                                     |
| ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| Numeric HUD (HP, crates, timers) updates at **10 Hz max**                          | Store coalesces writes; a value that changes 20×/s is sampled on a 100 ms timer |
| Bars use CSS `transform: scaleX()` only                                            | Never `width` — layout thrash on every hit                                      |
| Event HUD (kill feed, craft toast) appends a node and removes it on `animationend` | Max 5 nodes, oldest evicted                                                     |
| All player-supplied text goes through `textContent`                                | Never `innerHTML` — this is the XSS boundary (§11)                              |

**Screens:** Boot → Main menu (Online / Offline / Settings) → Lobby (create/join, code display, player list, team swap, ready) → Match HUD → Results. One screen manager, one `show(screenId)` function, CSS transitions only.

**Recipe book UX** (§4.5.5): a grid of recipe cards, each drawn as a top-down ASCII pattern from the same data the matcher uses. Locked tiers are visible but greyed with the unlock condition ("Tier 2 at 3:00"). This is deliberately *not* a discovery mechanic — hiding the recipes would make the core mechanic unlearnable.

### 8.8 Audio

```
AudioContext
 └─ master GainNode
     └─ DynamicsCompressor (threshold -12 dB, ratio 4:1)   ← stops combat from clipping
         ├─ sfxBus   → PannerNode[] (24 voices max, distance-culled)
         ├─ uiBus    → stereo, no panning
         └─ musicBus → not used in v1
```

| Use                                         | Technique                                                                    | Why                                                                                                                                                                                      |
| ------------------------------------------- | ---------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| One-shots (break, place, hit, craft, death) | `AudioBufferSourceNode` from decoded OGG/Opus                                | Cheapest, sample-accurate start                                                                                                                                                          |
| Continuous "base under attack" rumble       | **`AudioWorklet`** — a 55 Hz oscillator with LFO-modulated gain              | This is the one legitimate worklet use: a signal that must run continuously and respond to a parameter within one audio quantum. A `BufferSource` loop would click on parameter changes. |
| Spatialisation                              | `PannerNode`, `HRTF`, `refDistance 4`, `maxDistance 40`, `rolloffFactor 1.2` | Buddy combat needs directional cues; HRTF on 24 voices is affordable (measured cost ~0.3 ms/frame on desktop, higher on mobile → fall back to `equalpower` on `LOW` tier).               |
| Music                                       | **None in v1**                                                               | Licensed music is a legal and bandwidth problem; ambient wind loop only.                                                                                                                 |


**iOS autoplay:** `AudioContext` starts `suspended`. The boot screen has a single "Tap to play" gate; on the first `pointerdown` we `await ctx.resume()`. Never attempt to resume without a user gesture — Safari will silently refuse and the game will appear mute with no error.

**SFX list (30 files, 22.05 kHz mono Opus, ~6 KB each ≈ 180 KB total):** block break (3 variants), block place, footstep (4), jump, land, sword swing (3), sword hit flesh, sword hit crate, bow draw, bow release, arrow hit, staff cast, staff impact, heal, buddy spawn (per role family, 3), buddy death (3), tower fire, ram impact, catapult launch, catapult impact, core hit, core destroyed, recipe complete, tier unlock, victory, defeat, UI click, UI error.

**Audio carries the juice** that visual destruction would: the core-hit sound is layered (impact + 55 Hz sub + a 3-second metallic ring), so hitting a base *feels* like hitting a structure even though no blocks fly off.

### 8.9 Asset pipeline

| Asset         | Format                                                        | Size         | Notes                                                                                                                                                                                                         |
| ------------- | ------------------------------------------------------------- | ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Block atlas   | PNG, 256×256, 16×16 tiles, 24 used                            | ~28 KB       | **Not** KTX2/Basis. Basis is designed for large photographic textures with mipmaps; a 256² pixel-art atlas with nearest filtering transcodes to ~40 KB with a decoder that costs 90 KB of JS. Strictly worse. |
| Entity meshes | glTF → three.js `BufferGeometry`, no textures, vertex colours | ~35 KB       | 6 role meshes + 3 player meshes, all < 400 tris                                                                                                                                                               |
| SFX           | OGG/Opus                                                      | 180 KB       | 22.05 kHz mono                                                                                                                                                                                                |
| Fonts         | woff2, subset to Latin + digits                               | 55 KB        | 1 display, 1 body                                                                                                                                                                                             |
| **Total**     |                                                               | **≈ 300 KB** | vs the 700 KB budgeted in §9.4 — the client is not asset-bound                                                                                                                                                |

**Caching:** every asset under `/assets/[hash].[ext]` with `Cache-Control: public, max-age=31536000, immutable`. The wasm binary is hashed too. `index.html` is `no-cache`. This means a repeat visit loads ~15 KB.

### 8.10 Client-side perf gates (enforced in CI, T-055)

| Gate            | Threshold                          | Measurement                                                             |
| --------------- | ---------------------------------- | ----------------------------------------------------------------------- |
| wasm (brotli)   | **≤ 700 KB target, fail > 1.2 MB** | `wasm-opt -Oz` output size, asserted in CI                              |
| JS (gzipped)    | **≤ 450 KB target, fail > 550 KB** | `vite build` report                                                     |
| Blocking assets | ≤ 90 KB                            | atlas + fonts; SFX and entity meshes are deferred                       |
| First playable  | ≤ 3 s @ 8 Mbps / 150 ms RTT        | Playwright + CDP network throttle, `performance.mark('first-playable')` |
| Frame p95       | ≤ 20 ms                            | 60 s scripted replay, `performance.now()` deltas, 95th percentile       |
| Draw calls      | ≤ 120                              | `renderer.info.render.calls` sampled at frame 300                       |
| Triangles       | ≤ 350k                             | `renderer.info.render.triangles`                                        |
| wasm tick       | ≤ 3 ms p99                         | `performance.measure` around `cb_tick`                                  |

> **Caveat on headless GPU:** headless Chrome runs on SwiftShader by default, so frame-time numbers are *relative*, not absolute — they catch regressions, they do not prove a phone will hit 60 fps. Absolute mobile numbers come from the manual device matrix (T-062), which is a documented checklist, not an automated gate. Do not pretend the CI gate proves mobile performance.

---

---

## 9. Free-tier verification and capacity

All figures fetched from provider documentation on 2026-10-20. Re-verify before launch; limits move.

### 9.1 What the free tiers actually give you

| Provider                                 | Resource                    | Free allowance                                                                                                  | Source              |
| ---------------------------------------- | --------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------------- |
| **Cloudflare Workers / Durable Objects** | Requests                    | **100,000 / day** (hard fail above)                                                                             | DO Pricing          |
|                                          | Incoming WebSocket messages | **metered at 20:1** — 100 msgs = 5 billable requests. Outgoing msgs and protocol pings: not charged.            | DO Pricing          |
|                                          | Duration                    | **13,000 GB-s / day**, billed at a flat **128 MB** allocation regardless of actual use                          | DO Pricing          |
|                                          | DO storage                  | 5 GB/account, 10 GB/object; 5M rows read/day, 100k rows written/day                                             | DO Limits           |
|                                          | DO CPU per invocation       | 30 s default (configurable to 5 min) — *not* the 10 ms Workers HTTP limit                                       | DO Limits           |
|                                          | Worker bundle               | 64 MiB uncompressed, no compressed cap                                                                          | Workers Limits      |
|                                          | DO concurrency              | one object is single-threaded, ~1,000 req/s soft limit, unlimited objects                                       | DO Limits           |
| **Vercel Hobby**                         | Fast Data Transfer          | **100 GB / month**                                                                                              | Fair Use Guidelines |
|                                          | Function invocations        | 1,000,000 / month                                                                                               | Fair Use Guidelines |
|                                          | Builds                      | 1 concurrent, 45 min cap, 100 deployments/day, 100 MB CLI upload                                                | Limits              |
|                                          | **Licence**                 | **Non-commercial personal use only**                                                                            | Fair Use Guidelines |
| **Upstash Redis**                        | Commands                    | 500,000 / month, 250 MB, 10 GB bandwidth                                                                        | Pricing             |
| **Oracle Always Free**                   | Compute                     | 1,500 OCPU-hours + 9,000 GB-hours / month → **~2 OCPU + 12 GB RAM running 24/7** (or 4 OCPU for half the month) | Oracle Free Tier    |
|                                          | Egress                      | 10 TB / month                                                                                                   | Oracle Free Tier    |
|                                          | Block storage               | 200 GB                                                                                                          | Oracle Free Tier    |

> **Correction worth stating loudly:** the widely-repeated "4 OCPU / 24 GB free forever" figure is stale. Oracle now expresses the allowance in *hours*: 1,500 OCPU-h ÷ 744 h ≈ **2.0 OCPU continuous**, 9,000 GB-h ÷ 744 h ≈ **12.1 GB continuous**. Plan for 2 cores, not 4.

> **One unresolved ambiguity, flagged deliberately:** the Workers limits page states a **10 ms CPU limit per HTTP request on the Free plan**, while the Durable Objects limits page states **30 s default per DO invocation**. These are probably describing different code paths (Workers HTTP vs. DO invocations), but the documentation does not say so explicitly. The plan does not depend on the answer — a per-message handler that decodes a 30-byte input and advances one tick is ~50 µs — but **T-046 must measure p99 CPU per WebSocket message and record it**, because if the 10 ms figure does apply to DO message handlers, the entire free-tier plan is invalid.

### 9.2 Capacity arithmetic — the number that matters

**Assumptions for the calculation:** server tick 20 Hz; client input packets 15 Hz (each packet carries up to 3 pending input frames, so loss recovery doesn't need retransmits); 6-player room; 20-minute match.

```
Incoming WS messages per hour, 6 players @ 15 Hz:
  6 × 15 × 3600 = 324,000 msgs/h

Billable requests (20:1 ratio):
  324,000 / 20 = 16,200 requests/h

Daily request budget:
  100,000 / 16,200 = 6.17 hours of 6-player play per day
                  = ~18 matches of 20 minutes, per day, total
```

**Duration check (does it bind first?):**

```
Object active time per match: 1,200 s
Flat memory allocation:       128 MB = 0.125 GB
GB-s per match:               0.125 × 1,200 = 150 GB-s
Daily duration budget:        13,000 GB-s
Matches per day by duration:  13,000 / 150 ≈ 86
```

Duration is **not** the binding constraint; **requests are, by a factor of ~4.8**. A 20 Hz tick loop with `setInterval` keeps the object active for the whole match, so hibernation does not help here — hibernation saves *duration*, and duration was never the problem. This is the counter-intuitive part: the free tier punishes *message volume*, not *time*.

**Scaling the same math to other room sizes:**

| Room size | Billable req/h | Hours/day | 20-min matches/day |
| --------- | -------------- | --------- | ------------------ |
| 2 players | 5,400          | 18.5      | ~55                |
| 3 players | 8,100          | 12.3      | ~37                |
| 4 players | 10,800         | 9.3       | ~28                |
| 6 players | 16,200         | 6.2       | ~18                |

### 9.3 Verdict on hosting

| Option                      | Concurrent players                                                                                              | Match-hours/day           | Card required        | Ops burden                               | Verdict                                                                                |
| --------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------------------- | -------------------- | ---------------------------------------- | -------------------------------------------------------------------------------------- |
| **Cloudflare DO (free)**    | 6 (one 6-player room)                                                                                           | ~6                        | No                   | None                                     | **Primary.** Perfect for development, demos, small communities. Not a launch platform. |
| **Oracle Always Free VM**   | ~150–300 [ASSUMPTION: 1 room ≈ 1.5% of one Ampere core at 20 Hz with 6 players + 24 buddies — measure in T-053] | ~unbounded (10 TB egress) | Yes (identity check) | Low-medium (VM patching, Caddy, systemd) | **Fallback / scale path.** Same binary.                                                |
| Vercel functions            | 0                                                                                                               | 0                         | No                   | —                                        | Cannot hold a WebSocket. Static hosting only.                                          |
| Upstash Redis as room state | 0                                                                                                               | 0                         | No                   | —                                        | 500k commands/month ≈ 3 player-hours. Unusable for tick state; fine for room registry. |

**Decision:** build `host-worker` first because it costs nothing and needs no card. Build `host-axum` in the same milestone, not later — it is also the local development server, so it is not extra work, it is the *only* way to develop without deploying on every change. When the 6-hour/day ceiling becomes the problem, the switch is a DNS record and a config value.

**Client-side capacity:**

```
Blocking payload on first visit:
  index.html + CSS        8 KB
  JS bundle (gz)        450 KB
  wasm (brotli)         700 KB
  atlas + fonts          90 KB
  ─────────────────────────────
  total blocking      ≈ 1.25 MB
  deferred (lazy)     ≈ 220 KB   (SFX, entity meshes, recipe-book art)

100 GB / 1.25 MB ≈ 80,000 cold loads per month ≈ 2,650 per day
```

Cached repeat visits cost ~8 KB (only `index.html`, which is `no-cache`; everything else is immutable and hashed). **82k cold loads/month is ~2,650/day — an order of magnitude above what a 6-hours-of-6-player-rooms-per-day game server can absorb. The client host will never be the bottleneck; the game host will.**

### 9.4 First-playable budget

| Stage                                                       | Cost   | Cumulative        |
| ----------------------------------------------------------- | ------ | ----------------- |
| HTML + CSS                                                  | 8 KB   | 0.01 s            |
| JS bundle download (450 KB @ 8 Mbps)                        | —      | 0.45 s            |
| wasm download (700 KB @ 8 Mbps, parallel with JS)           | —      | 0.70 s            |
| wasm compile (streaming `WebAssembly.instantiateStreaming`) | 180 ms | 0.88 s            |
| atlas + fonts (90 KB)                                       | —      | 0.79 s (parallel) |
| `cb_init` (tuning parse, world seed)                        | 15 ms  | 0.90 s            |
| First column worldgen + mesh                                | 110 ms | 1.01 s            |
| First paint + 3 warm-up frames                              | 90 ms  | **1.10 s**        |
| **First playable (spawn ring meshed, controls live)**       |        | **≈ 1.6 s**       |

Headroom to the 3 s target is ~2×. At 4 Mbps it lands at ~2.6 s. **At throttled "Slow 4G" (1.6 Mbps) it is ~6.5 s and the target is missed** — this is stated plainly rather than hidden, because the fix (halving the wasm) costs more than the benefit for a target audience on mid-to-high devices (A-01). The CI gate therefore runs at 8 Mbps and reports the Slow-4G number informationally.

---

## 10. Deployment

### 10.1 Topology

| Component            | Host                                 | Artifact                                | Trigger           |
| -------------------- | ------------------------------------ | --------------------------------------- | ----------------- |
| Client               | Vercel Hobby                         | `apps/web/dist` (static)                | push to `main`    |
| Room host (primary)  | Cloudflare Workers + Durable Objects | `crates/host-worker` → wasm             | tag `v*`          |
| Room host (fallback) | Oracle Always Free VM                | `crates/host-axum` (musl static binary) | tag `v*` + manual |
| Local dev            | your machine                         | `cargo run -p host-axum` + `pnpm dev`   | —                 |


### 10.2 Vercel

```json
// vercel.json
{
  "buildCommand": "pnpm --filter web build",
  "outputDirectory": "apps/web/dist",
  "headers": [
    { "source": "/assets/(.*)", "headers": [
      { "key": "Cache-Control", "value": "public, max-age=31536000, immutable" }]},
    { "source": "/(.*).wasm", "headers": [
      { "key": "Content-Type", "value": "application/wasm" },
      { "key": "Cache-Control", "value": "public, max-age=31536000, immutable" }]},
    { "source": "/(.*)", "headers": [
      { "key": "X-Content-Type-Options", "value": "nosniff" },
      { "key": "Referrer-Policy", "value": "strict-origin-when-cross-origin" },
      { "key": "Permissions-Policy", "value": "microphone=(), camera=(), geolocation=()" },
      { "key": "Content-Security-Policy", "value": "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self' wss://game.example.com; img-src 'self' data:; style-src 'self' 'unsafe-inline'; font-src 'self'" }]}
  ]
}
```

Notes that matter:

- `script-src 'wasm-unsafe-eval'` is **required** for `WebAssembly.instantiate` under CSP. Without it the game fails to boot on any browser that enforces CSP, with a console error most developers never see locally.
- **No COOP/COEP headers.** They would only be needed for `SharedArrayBuffer`, which §8.1 deliberately avoids. Adding them breaks third-party embeds and iframes for no gain.
- `style-src 'unsafe-inline'` is required because the HUD sets `element.style` transforms directly. If that is unacceptable later, move to a pre-built stylesheet with CSS custom properties — but do not block launch on it.

### 10.3 Cloudflare Worker

```jsonc
// wrangler.jsonc
{
  "name": "cratebound-rooms",
  "main": "build/worker/shim.mjs",
  "compatibility_date": "2026-01-01",
  "durable_objects": { "bindings": [{ "name": "ROOMS", "class_name": "Room" }] },
  "migrations": [{ "tag": "v1", "new_sqlite_classes": ["Room"] }],
  "observability": { "enabled": true }
}
```

- `new_sqlite_classes` is mandatory — the free plan only allows SQLite-backed Durable Objects (§9.1).
- `observability.enabled` gives Workers Logs without a paid plan; the room's structured JSON logs land there.
- Deploy: `wrangler deploy` in CI on tag, then `wrangler rollback` is the one-command revert.
- **Do not enable WebSocket Hibernation.** A 20 Hz ticking room never idles, so hibernation saves nothing (§9.2) and its API constraints (no `setInterval` across hibernation, state must be rehydrated per message) would force a redesign of the tick loop for zero benefit. Use a plain `setInterval(50ms)` and `ws.accept()`.

### 10.4 Fallback VM (Oracle Always Free)

```bash
# one-time
sudo apt install -y caddy
sudo systemctl enable --now cratebound   # systemd unit, EnvironmentFile=/etc/cratebound/env
```

- Caddy handles TLS automatically (`game.example.com { reverse_proxy 127.0.0.1:8080 }`) — no certbot cron.
- Binary: `cargo build --release --target aarch64-unknown-linux-musl` (Ampere is ARM).
- 2 OCPU / 12 GB (§9.1) runs ~130–260 concurrent rooms [ASSUMPTION, measured in T-053].
- **Switching hosts is a DNS change plus one client env var** (`VITE_GAME_WS_URL`). That is the entire point of the `room` crate abstraction.

### 10.5 CI pipeline (`.github/workflows/ci.yml`)

```
jobs:
  rust-native      cargo test --workspace --exclude host-worker
  rust-wasm        wasm-pack test --headless --chrome crates/client-wasm
  determinism      golden-hash suite on BOTH x86_64 and ubuntu-24.04-arm   ← R-07
  size-gate        wasm-opt -Oz → assert ≤ 1.2 MB brotli
  protocol-fuzz    cargo fuzz run decode -- -max_total_time=60
  web-build        pnpm build → assert JS ≤ 550 KB gz
  e2e              playwright test (chromium, firefox, webkit, mobile-chrome)
  perf             playwright perf spec → assert frame p95 ≤ 20 ms, draw calls ≤ 120
  deploy-web       vercel --prod        (main only)
  deploy-worker    wrangler deploy      (tag only)
```

The ARM determinism job is not optional. `f32` behaviour and `usize` width differ between `x86_64` and `aarch64`; a desync that only appears on phones is exactly the bug that ships to production because CI was green.

### 10.6 Rollback

| Failure                           | Action                                                                                          | Time to recover       |
| --------------------------------- | ----------------------------------------------------------------------------------------------- | --------------------- |
| Bad client build                  | Vercel dashboard → previous deployment → Promote                                                | < 30 s                |
| Bad worker build                  | `wrangler rollback`                                                                             | < 60 s                |
| Protocol change broke old clients | Bump `PROTOCOL_MIN_VERSION`, ship a client that shows "please refresh"                          | one deploy            |
| Free-tier quota exhausted         | Set `ROOMS_ENABLED=false` (existing matches finish, new rooms rejected with a readable message) | instant flag flip     |
| Total host outage                 | Repoint DNS to the Axum VM                                                                      | DNS TTL (set to 60 s) |

---

---

## 11. Security and anti-cheat

### 11.1 The one invariant

**The client is an input device and a display.** It sends intents (`I pressed W`, `I aimed at x,y,z`, `I want to place block B`) and receives state. It never sends a position, a damage value, a health value, an inventory count, or a crafting result. Every one of those is computed server-side by `crates/sim`. If a feature cannot be expressed as an intent, it cannot be cheated by forging a message — only by forging an *input*, which the server validates against the same physics it applies to everyone.

This single rule removes the majority of the threat model below. What remains is rate limiting, input validation, and resource exhaustion.

### 11.2 Threat model

| #   | Threat                                                            | Vector                                                           | Mitigation                                                                                                                                                                                                 | Enforced in                  |
| --- | ----------------------------------------------------------------- | ---------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------- |
| T1  | **Speed / teleport hack**                                         | Modified client sends a position or an impossible input          | Position is never received, only computed. `sim::movement` clamps velocity to the role's max; teleport is structurally impossible.                                                                         | `sim::movement`              |
| T2  | **Reach hack** (break/place at 40 m)                              | Forged `C_EDIT` coordinates                                      | Server validates `distance(player_pos, block) ≤ 6.0 m` using its own player position                                                                                                                       | `room::apply_edit`           |
| T3  | **Damage spoofing**                                               | Client claims a hit                                              | Damage is computed in `sim::combat` from server-side positions and cooldowns; no client damage message exists in the protocol                                                                              | protocol has no such message |
| T4  | **Recipe spam / free units**                                      | Rapid crate placement to trigger many crafts                     | 10-tick stability window, per-recipe cooldown, per-team craft cooldown 3 s, crate cost actually consumed                                                                                                   | `sim::recipe`                |
| T5  | **Crate duplication via edit rollback**                           | Place crate → craft consumes → disconnect before ack → reconnect | Inventory and block state are server-authoritative and persisted per tick; a client rollback is *visual only* and never feeds back. Crates are derived from blocks, not a separate counter that can drift. | `room` state                 |
| T6  | **Block-edit race duping**                                        | Two clients break the same crate block in one tick               | Edits applied serially per tick; second edit validates against post-first state and is rejected                                                                                                            | `room::apply_edit`           |
| T7  | **Snapshot flooding** (amplification)                             | Client sends tiny messages that trigger large broadcasts         | `S_EDIT` broadcasts only to clients with the chunk loaded; `C_EDIT` rate-limited to 8/s; message size capped                                                                                               | §7.4                         |
| T8  | **Message-size DoS**                                              | 10 MB frame                                                      | Frame > 4096 B → close 1009 before decode                                                                                                                                                                  | `codec::decode_checked`      |
| T9  | **Connection flooding** — *the most dangerous one on a free tier* | Open 100k sockets; the 100k/day request budget dies in minutes   | Per-IP connection cap (3) enforced in a lightweight `REGISTRY` DO; `C_HELLO` required within 5 s or the socket closes; room creation rate-limited to 3/min/IP                                              | `REGISTRY` DO                |
| T10 | **Room-code brute force**                                         | Enumerate 6-char codes                                           | 32^6 = 1.07 B combinations; 5 attempts/min/IP, then 15-minute lockout                                                                                                                                      | `REGISTRY` DO                |
| T11 | **XSS via player name / chat**                                    | `<img onerror=...>` as a name                                    | Server strips to `[A-Za-z0-9 _.,!?-]{0,120}`; client renders with `textContent` only, never `innerHTML`; CSP has no `unsafe-eval` for scripts                                                              | `protocol::sanitize` + HUD   |
| T12 | **Secret leakage into bundle**                                    | A token imported in `apps/web`                                   | CI step greps the built bundle for known secret patterns; `VITE_` prefix is documented as public-by-definition (§17)                                                                                       | CI                           |
| T13 | **Prototype pollution / deserialisation**                         | Crafted postcard payload                                         | `postcard` is not JSON; unknown enum variants fail decode; no dynamic property assignment from wire data                                                                                                   | `protocol`                   |
| T14 | **Eliminated player rejoins** | Reuses a pre-elimination token to re-enter a match | Tokens are invalidated on elimination (§6.11); reconnect window is 30 s and closes permanently at elimination | `room::lifecycle` |
| T15 | **Replay / token reuse**                                          | Stolen `player_token`                                            | Token is room-scoped, 16 bytes random, valid only for that room's lifetime, and invalidated at `CLEANUP`                                                                                                   | `room::lifecycle`            |

### 11.3 Rate limits (all server-side, all per connection)

| Channel       | Limit            | Burst | On exceed                               |
| ------------- | ---------------- | ----- | --------------------------------------- |
| Any message   | 30/s             | 60    | close 1008                              |
| `C_EDIT`      | 8/s              | 12    | reject with `reason=RATE`, do not close |
| `C_CHAT`      | 0.5/s            | 2     | drop silently                           |
| `C_HELLO`     | 1 per connection | —     | close                                   |
| Room creation | 3/min/IP         | 5     | 429 from the registry                   |
| Join attempts | 5/min/IP         | 5     | 15-minute lockout                       |

### 11.4 What is deliberately *not* defended

Honest limits, so nobody assumes protection that does not exist:

- **Aimbot.** Server-side hit validation stops impossible hits, but a human-accuracy aimbot that stays within legal angles is undetectable without statistical analysis over many matches. Out of scope for v1; the counter is that TTK is 1.8 s and buddies are the real army.
- **Map/wallhack via memory inspection.** The client must know about entities in its interest radius to render them, so a modified client can read them. Mitigated by interest management (you cannot see what is 100 m away) but not eliminated.
- **Bot automation.** No CAPTCHA, no behavioural analysis. Accepted risk for a free hobby game.
- **DDoS at the network layer.** Delegated entirely to Cloudflare's edge. This is a genuine benefit of the DO choice.

---

## 12. Testing strategy

### 12.1 Layers

| Layer          | Scope                                               | Tool                                              | Gate                        |
| -------------- | --------------------------------------------------- | ------------------------------------------------- | --------------------------- |
| L1 Unit        | `sim` rules, recipe matcher, movement, combat, caps | `cargo test`                                      | every PR, must be 100% pass |
| L2 Determinism | native vs wasm, x86_64 vs aarch64                   | `cargo test -p sim --test golden` on both runners | **blocking**                |
| L3 Integration | `room` crate driven by synthetic inputs, no network | `cargo test -p room --test match_sim`             | every PR                    |
| L4 Protocol    | decode robustness                                   | `cargo fuzz` 60 s in CI                           | every PR                    |
| L5 E2E         | real browser, real wasm, real WebSocket             | Playwright (chromium/firefox/webkit/mobile)       | every PR                    |
| L6 Load        | 20 rooms × 6 bots                                   | `tools/bot-swarm` against `host-axum`             | pre-release                 |
| L7 Perf        | frame time, draw calls, bundle size                 | Playwright + build assertions                     | every PR                    |
| L8 Device      | real phones                                         | manual checklist (T-062)                          | pre-release                 |


### 12.2 Determinism harness (the most important test in the project)

```
fixture: (seed, [InputFrame; N]) for N = 20_000 ticks
  → run native, hash state every 100 ticks
  → run wasm, hash state every 100 ticks
  → assert identical hash sequences
  → assert final hash matches a committed golden file
```

Goldens are committed per seed. **A golden hash change is a reviewable event** — it means game rules changed, and the diff must be explained in the PR. This is what stops "harmless refactors" from silently changing physics.

Failure triage: when hashes diverge, the harness bisects the tick index, then dumps the first differing entity field. That narrows a 20,000-tick divergence to a single field in one command.

### 12.3 Recipe matcher test matrix (the highest-risk system)

Table-driven, and the **negative cases matter more than the positive ones**:

| Case                                 | Input                                              | Expected                                                           |
| ------------------------------------ | -------------------------------------------------- | ------------------------------------------------------------------ |
| Exact match                          | 3×2 slab of 6 crates                               | `R01_SWORD`                                                        |
| Rotated match                        | 2×3 slab (the same shape rotated 90°)              | `R01_SWORD` — rotation must not matter                             |
| One extra crate                      | 3×2 slab + 1 adjacent                              | `None` — must not match by subset                                  |
| One missing crate                    | 5 of 6 in the sword shape                          | `None`                                                             |
| Correct shape, floating | 3×2 slab with no ground support | `None` |
| Correct shape, one crate unsupported | lowest layer partially floating                    | `None`                                                             |
| Too large                            | 33-crate wall                                      | `None`, and flood fill aborts within budget                        |
| Two clusters adjacent                | sword next to a 2×2                                | sword matches, 2×2 matches, independently                          |
| Cluster outside any pad | valid 3×2 sword shape built on open terrain | `None` — pad containment is checked first (§4.5.1 rule 1) |
| Cluster in an enemy pad | valid sword shape inside an opponent pad | `None` — crates cannot be placed there at all |
| Ambiguous shapes                     | two recipes whose canonical forms collide          | **assert at load time that no two recipes share a canonical form** |
| Stability                            | shape held 9 ticks then broken                     | `None` (needs 10)                                                  |
| Stability                            | shape held 10 ticks                                | matches exactly once, then blocks are consumed                     |
| Double trigger                       | two players place the final block in the same tick | exactly one craft, one `S_EVENT`                                   |

### 12.4 Gameplay invariant tests (L3)

Run a full 20-minute match in ~2 seconds of wall time (headless, no rendering), asserting invariants every tick:

- HP never < 0 and never > max.
- Entity count never grows unbounded (no leak): `entities ≤ 18 + 24 + 4` at all times.
- A buddy, once `DEAD`, never reappears.
- Core HP is monotonically non-increasing.
- A team's buddy count never exceeds its cap.
- Crates are conserved: `dropped − consumed − destroyed == carried + on ground + placed on pads`.
- Match always terminates within 25 minutes of simulated time (overtime decay guarantees it).
- Elimination is idempotent: eliminating a team twice is a no-op.

### 12.5 E2E specs (L5)

| Spec                      | Asserts                                                                                   |
| ------------------------- | ----------------------------------------------------------------------------------------- |
| `boot.spec.ts`            | Page loads, wasm initialises, first playable < 3 s on throttled 8 Mbps                    |
| `offline-match.spec.ts` | Start offline FFA → collect a landed airdrop → carry crates to the pad → place 3×2 → sword spawns |
| `recipe-negative.spec.ts` | Place 7 crates (near-miss) → no sword spawns, HUD shows "needs 6"                         |
| `buddy-cap.spec.ts`       | Craft to the team cap → 6th craft is rejected with a visible message, crates not consumed |
| `base-destroy.spec.ts` | Damage core to 0 → every living enemy player dies on the same tick → victory screen for the winner, defeat screen for the loser, **no spectator camera** |
| `touch-controls.spec.ts`  | Mobile viewport: stick moves player, tap with pickaxe breaks, tap with block places       |
| `reconnect.spec.ts`       | Kill the socket, reconnect within 30 s, control resumes, no duplicate entity              |
| `two-client.spec.ts`      | Two browser contexts join the same room code, see each other move, edits replicate        |

### 12.6 Perf gates (L7)

Exactly the table in §8.10. The gate fails the build; it is not advisory.

### 12.7 What CI cannot prove

Stated so it is not mistaken for coverage:

- **Real mobile GPU performance** — headless Chrome is SwiftShader (§8.10). Only T-062's device matrix is real.
- **Fun.** No test asserts that the game is enjoyable. That is M2's playtest gate (§16), and it is the only gate where a human's judgment is the pass criterion.
- **Network conditions beyond simulated throttling** — packet loss and reordering are simulated in `room` tests, but real carrier networks do things simulations do not.

---

---

## 13. Observability and operations

| Signal                         | Where                                   | Alert threshold        | Why it matters                                                                             |
| ------------------------------ | --------------------------------------- | ---------------------- | ------------------------------------------------------------------------------------------ |
| Room tick duration p99         | host logs (structured JSON)             | > 12 ms sustained 30 s | The tick budget is 50 ms; at 12 ms you have <4× headroom before input lag becomes visible. |
| Snapshot bytes/client/tick p95 | host                                    | > 2 KB                 | Blows the bandwidth budget and the free-tier message cost simultaneously.                  |
| Disconnect rate                | host                                    | > 8% of joins          | Distinguishes "netcode is broken" from "the game is boring".                               |
| Desync reports                 | client → host telemetry event           | any                    | A single confirmed desync invalidates the whole architecture claim. Log the input log.     |
| DO request count               | Cloudflare dashboard                    | > 70% of 100k/day      | At 85% you must shed load (reject new rooms) before the hard fail at 100%.                 |
| Frame time p95 (client)        | client telemetry, sampled 1/20 sessions | > 20 ms                | Detects a regression on real devices that CI's headless Chrome misses.                     |
| Crash / wasm trap              | client telemetry                        | any spike              | A wasm trap is an unrecoverable tab death; treat as P0.                                    |

**Kill switches (must exist before launch):** `ROOMS_ENABLED=false` (reject new room creation, let existing matches finish), `PROTOCOL_MIN_VERSION` (refuse stale clients with a readable message), and `CLIENT_BUILD_OVERRIDE` (pin Vercel to a previous deployment in one click).

**Logging discipline:** structured JSON only, one line per event, no PII beyond display name, and never log full snapshots (they're large and they leak positions). Log the *input log hash* instead — it's enough to reproduce a desync offline.

---

## 14. Risk register

Scored P (probability) × I (impact), 1–5 each. Anything ≥ 12 needs a mitigation task that exists in §15, not just good intentions.

| ID   | Risk                                                                                                                                                                        | P | I | Score  | Mitigation                                                                                                                                         | Detector                                                                                      |
| ---- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | - | - | ------ | -------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| R-01 | **Desync between client prediction and server authority.** The single most likely way this project fails. Fixed-point drift, iteration-order differences, or a stray `f32`. | 4 | 5 | **20** | A-08 enforced by a CI lint that greps `sim/` for `f32`/`f64` and fails. T-015 replay harness compares native vs wasm state hashes on every commit. | Hash mismatch in CI; player-visible rubber-banding.                                           |
| R-02 | **Recipe matcher is ambiguous or exploitable.** Near-miss shapes match, or spam produces free units.                                                                        | 4 | 4 | **16** | Table-driven matcher tests with explicit negative cases (T-010). Server-side cooldown + crate cost floor.                                          | Negative-case tests; economy telemetry (units/minute per player).                             |
| R-03 | **Free-tier request ceiling hit mid-session.** 100k/day is exhausted and *every* room dies at once, not gracefully.                                                         | 4 | 4 | **16** | Track spend in the room registry; shed at 85%. Document the Axum switch.                                                                           | Dashboard alert at 70%.                                                                       |
| R-04 | **Mobile frame budget blown** by entity count (24 buddies + 6 players + projectiles).                                                                                       | 3 | 4 | **12** | Instanced buddy rendering, animation LOD, hard entity cap from §4, dynamic resolution.                                                             | T-055 perf gate; field p95 telemetry.                                                         |
| R-05 | **wasm bundle exceeds budget**, first playable slips past 5 s on 4G.                                                                                                        | 3 | 3 | 9      | Size gate in CI (fail > 1.6 MB brotli). `wasm-opt -Oz`, no `std::fmt` in release paths.                                                            | CI size report on every PR.                                                                   |
| R-06 | **`workers-rs` Durable Object + WebSocket API friction** burns the schedule (hibernation semantics, no `setInterval` guarantees).                                           | 3 | 3 | 9      | The `room` crate is host-agnostic, so the fallback is a config change. Build `host-axum` first; treat `host-worker` as a port, not a design.       | T-046 longer than T-045 by >2×.                                                               |
| R-07 | **Determinism breaks only on ARM** (mobile wasm vs x86 native), invisible in CI on x86 runners.                                                                             | 3 | 4 | 12     | Run the golden-hash suite on an ARM runner (GitHub `macos-14` or `ubuntu-24.04-arm`) in CI.                                                        | CI matrix job.                                                                                |
| R-08 | **Vercel Hobby non-commercial clause** violated if the game is ever monetised.                                                                                              | 2 | 3 | 6      | Documented in §17; migration to Cloudflare Pages (unlimited static, free, commercial OK) is a one-file change.                                     | Owner decision at monetisation.                                                               |
| R-09 | **Cheating ruins public matches** (speed hack, reach hack, crate duping).                                                                                                   | 3 | 3 | 9      | §11 threat model; server validates every intent; no client authority anywhere.                                                                     | Anomaly telemetry: distance-per-tick outliers, edits beyond reach.                            |
| R-10 | **Scope: the game is not fun.** Voxel + RTS-lite is a real design risk, not a technical one.                                                                                | 3 | 5 | 15     | Playable gate at M2 with bots before any netcode polish. Recipe table is data, tunable without code.                                               | Playtest session at M2; if the loop isn't fun against bots, stop and fix design before T-040. |

---

## 15. Task backlog

**Execution protocol for the agent.** Pick the lowest-numbered task whose `Depends` are all `DONE`. Read the task's files list — those are the files you create or modify, and *only* those. Run the acceptance verification. If any criterion fails, the task is not done: fix it, do not proceed. Commit one task per commit, message `T-0NN: <title>`. Never start a task whose dependency is open, even if it "looks independent" — the dependency graph encodes real interface contracts.

**Legend:** `AC` = acceptance criterion (must be mechanically verifiable). Every AC names its verification.

**Numbering:** IDs are grouped by phase — A `001–016`, B `017–024`, C `030–039`, D `040–044`, E `045–048`, F `049–052`, G `053–058`, H `059–064`. The gaps between groups are intentional phase boundaries, not missing work.

### 15.0 Dependency graph (critical path)

```
T-001 → T-002
T-001 → T-003 → T-004 → T-005 → T-006 → T-007 → T-008
                     └→ T-009 → T-010 → T-011 → T-012 → T-013 → T-014
T-005, T-009 → T-015
T-016 (tuning) feeds T-005..T-014, must land before T-010
T-014 → T-017 → T-018 → T-019 → T-020 → T-021 → T-022
T-020..T-022 → T-023 (bots) → T-024 (match_sim integration)
T-024 → T-045 (axum) → T-046 (worker) → T-047 (registry)
T-030 (wasm bridge) needs T-009,T-010,T-021
T-030 → T-031 → T-032 → T-033 → T-034 → T-035/T-036
T-030, T-040 → T-041 → T-043
T-024, T-040 → T-048 (offline)
T-024 → T-050, T-053
T-055, T-056, T-057 → T-063 (launch)
```


### Phase A — Rust core (T-001 … T-016)

---

**T-001 — Monorepo scaffold**

- **Depends:** —
- **Files:** `Cargo.toml`, `rust-toolchain.toml`, `package.json`, `pnpm-workspace.yaml`, `.gitignore`, `.editorconfig`, `crates/{sim,world,protocol,room,client-wasm,host-worker,host-axum}/Cargo.toml` + `src/lib.rs` stubs, `apps/web/package.json`, `README.md`
- **AC:**
  - `cargo build --workspace` succeeds with empty crates.
  - `cargo build --target wasm32-unknown-unknown -p sim` succeeds.
  - `pnpm install` succeeds; `pnpm -r build` is a no-op success.
  - `rust-toolchain.toml` pins a channel and includes the `wasm32-unknown-unknown` target.

---

**T-002 — CI skeleton**

- **Depends:** T-001
- **Files:** `.github/workflows/ci.yml`
- **AC:**
  - Pipeline runs on PR and fails if `cargo test` fails.
  - Pipeline has a job on `ubuntu-24.04-arm` (or `macos-14`) that runs `cargo test` — this is the ARM determinism guard (R-07) and must exist from day one, not added later.
  - Pipeline caches `~/.cargo` and `node_modules`; a warm run completes without recompiling the world.

---

**T-003 — `sim`: fixed-point math**

- **Depends:** T-001
- **Files:** `crates/sim/src/fixed.rs`, `crates/sim/src/lib.rs`
- **AC:**
  - `Fixed(i32)` Q16.16 with `add/sub/mul/div/neg/abs/min/max/clamp`, saturating on overflow, documented rounding mode.
  - `sqrt`, `sin`, `cos`, `atan2` implemented via 1024-entry tables; `#[test] fn table_accuracy()` asserts error ≤ 1e-3 vs `f64` reference.
  - `#[test] fn no_float_in_sim()` — a CI grep (`rg 'f32|f64' crates/sim/src --glob '!fixed.rs'`) returns empty; this is the R-01 tripwire and it must fail the build.
  - Cross-target test: identical results on `x86_64` and `wasm32` for 10,000 random inputs.

---

**T-004 — `sim`: ids, RNG, entity allocator, tick clock**

- **Depends:** T-003
- **Files:** `crates/sim/src/ids.rs`, `crates/sim/src/rng.rs`, `crates/sim/src/time.rs`
- **AC:**
  - `EntityId(u32)` with generation bits so a recycled id cannot be confused with a dead entity; `#[test] fn recycled_id_is_not_equal()`.
  - `xoshiro256**` with `#[test] fn known_vector()` matching published output for seed 0.
  - Separate streams: `rng_world`, `rng_combat`, `rng_loot` — `#[test] fn streams_are_independent()` (drawing from one does not change another's next value).
  - `Tick(u32)` newtype; `#[test] fn tick_wraps_at_u32_max()` documented behaviour.

---

**T-005 — `world`: column storage and block registry**

- **Depends:** T-004
- **Files:** `crates/world/src/chunk.rs`, `crates/world/src/block.rs`, `data/blocks.ron`
- **AC:**
  - Paletted 16×96×16 column: `get/set` in O(1), `#[test] fn roundtrip_all_blocks()` for 20 block types.
  - Memory: a column with ≤ 8 distinct blocks allocates ≤ 16 KB (assert via `size_of` + a counting allocator test).
  - `BlockId` table in `data/blocks.ron` with `solid`, `opaque`, `hardness`, `placeable`, `breakable`, `crate` flags. **There is no `CrateOre` block** (D-01) and `terrain.breakable = false` means every terrain block has `breakable: false`.
  - `#[test] fn only_crates_are_placeable()` — exactly one block id has `placeable: true`.
  - `#[test] fn pad_floor_is_indestructible()`.
  - Serialisation roundtrip is byte-identical (`proptest`).

---

**T-006 — `world`: deterministic worldgen**

- **Depends:** T-005
- **Files:** `crates/world/src/gen.rs`, `crates/world/src/noise.rs`
- **AC:**
  - `gen(seed, cx, cz) -> Column` is pure and deterministic; `#[test] fn same_seed_same_column()` across 1,000 random coords.
  - Terrain height stays within Y 28–64 for 10,000 sampled columns.
  - Base pads: `#[test] fn bases_are_flat_and_symmetric()` — for N ∈ {2,3,4,6}, all pads have identical height profile and equal angular spacing; each pad carries an 8×8 indestructible stacking-pad floor at its edge.
  - **Airdrop site grid** (D-01): `#[test] fn drop_sites_are_flat_and_legal()` — every candidate site has slope ≤ 2 blocks, lies outside all base pads, and the set is identical across runs for the same seed.
  - `#[test] fn no_crate_ore_exists()` — grep-level assertion that no ore block is emitted anywhere in generation.
  - Generation of one column ≤ 1.5 ms native, ≤ 4 ms wasm (bench in `crates/world/benches/`).

---

**T-007 — `world`: greedy mesher with AO**

- **Depends:** T-005, T-006
- **Files:** `crates/world/src/mesh.rs`, `crates/world/benches/mesh.rs`
- **AC:**
  - Output vertex stride is exactly 12 bytes (asserted in a test).
  - `#[test] fn greedy_reduces_quads()` — a flat 16×16 plane produces 1 quad, not 256.
  - AO: `#[test] fn ao_corners()` — a block in a concave corner produces darker AO than an exposed one; values in 0..=3.
  - Meshing a full terrain column ≤ 1.2 ms native (bench asserted, not printed).
  - `#[test] fn mesh_is_deterministic()` — same column hashed twice yields identical bytes.

---

**T-008 — `world`: chunk diff encoding**

- **Depends:** T-005
- **Files:** `crates/world/src/diff.rs`
- **AC:**
  - `ChunkDiff { version: u32, entries: Vec<(u16, u8)> }` with postcard encode/decode roundtrip.
  - `#[test] fn worst_case_400_edits()` encodes to ≤ 1,400 bytes.
  - `#[test] fn keyframe_fallback()` — a diff with > 2,000 entries serialises as an RLE keyframe column and decodes to identical state.
  - `#[test] fn apply_is_idempotent()` — applying the same diff twice yields the same column.

---

**T-009 — `sim`: movement and collision**

- **Depends:** T-004, T-005
- **Files:** `crates/sim/src/movement.rs`
- **AC:**
  - AABB (0.6 × 1.8 × 0.6) vs voxel sweep, no tunnelling at 20 m/s (tested at 1/20 s steps).
  - Step-up of 1 block automatic; 2 blocks blocked.
  - `#[test] fn max_speed_is_enforced()` — no input sequence produces horizontal speed > 4.5 m/s.
  - Gravity 24 m/s², terminal velocity 50 m/s, jump apex 1.25 blocks.
  - Swimming: reduced gravity, upward swim at 2.0 m/s, no drowning damage in v1.
  - `#[test] fn cannot_enter_solid()` over 10,000 random inputs against a random 16³ voxel field.

---

**T-010 — `sim`: recipe matcher** ⚠️ *highest-risk task in the plan*

- **Depends:** T-005, T-009, T-016
- **Files:** `crates/sim/src/recipe.rs`, `data/recipes.ron`, `crates/sim/tests/recipe_matrix.rs`
- **AC:**
  - `canonicalize(&[BlockPos]) -> Vec<u8>` per §4.5.2, rotation-invariant across all 4 Y-rotations.
  - `match_cluster(state, pos) -> Option<RecipeId>` implementing §4.5.1 exactly: **pad containment first**, then 6-connected flood fill, cap 32, ground-support rule, 10-tick stability.
  - **All 14 rows of the §12.3 matrix pass**, including every negative case. Negative cases are mandatory; a matcher that passes only positives is not done.
  - `#[test] fn rejects_cluster_outside_pad()` — a valid sword shape placed on open terrain returns `None`.
  - `#[test] fn no_duplicate_canonical_forms()` — loading `data/recipes.ron` fails loudly if two recipes share a canonical form.
  - `#[test] fn cluster_cap_aborts_fast()` — a 64-block wall returns `None` in ≤ 20 µs.
  - Consumption is atomic: blocks removed and output spawned in the same tick, exactly once (§12.3 "double trigger").
  - Deposit path: `#[test] fn deposit_then_craft()` — 6 crates deposited one per tick at 6/s produce a sword on the tick after the 10-tick stability window.

---

**T-011 — `sim`: combat**

- **Depends:** T-009, T-016
- **Files:** `crates/sim/src/combat.rs`
- **AC:**
  - Melee: cone check (45°) + reach per weapon table; `#[test] fn sword_ttk_is_1_8s()` (100 HP, 25 dmg, 0.45 s).
  - Projectiles: fixed-step integration at 20 Hz, 40 m/s, hit test against player/buddy AABBs; `#[test] fn arrow_hits_at_14m()`.
  - AoE: `#[test] fn mage_hits_3_clustered()`.
  - Damage events are emitted into a per-tick `Vec<DamageEvent>` (the renderer consumes them for hit flash / numbers), never applied to presentation directly.
  - `#[test] fn no_damage_to_dead_entities()`.

---

**T-012 — `sim`: buddy AI and pathfinding**

- **Depends:** T-011
- **Files:** `crates/sim/src/buddy.rs`, `crates/sim/src/pathfind.rs`, `data/buddies.ron`
- **AC:**
  - 4-state machine per §4.6; `#[test] fn state_transitions()` covers all 7 edges in the diagram.
  - Target selection is deterministic: `#[test] fn tie_breaks_by_entity_id()`.
  - A* budgeted at 512 expansions; `#[test] fn budget_is_enforced()` — a sealed room returns no path and the buddy does not stall the tick.
  - `#[test] fn leash_returns_home()` at 24 m.
  - `#[test] fn buddy_never_respawns()` — after `DEAD`, entity is absent from state for 10,000 ticks.
  - 12 buddies + 6 players simulate in ≤ 2.0 ms per tick native.

---

**T-013 — `sim`: base core and instant elimination**

- **Depends:** T-011
- **Files:** `crates/sim/src/base.rs`
- **AC:**
  - Core 10,000 HP, 0.35 damage multiplier, no regen.
  - `#[test] fn bare_hands_cannot_win()` — 1 player, fists, 25 simulated minutes → core HP > 0.
  - `#[test] fn squad_push_takes_70_to_90s()` — 4 players + ram + 4 buddies, uncontested, destroys in the 70–90 s window.
  - **D-02 instant elimination:** `#[test] fn elimination_kills_all_living_players_same_tick()` — with 3 players alive on the team and 2 mid-swing, all 3 are `DEAD` on the tick the core reaches 0, with no intermediate state.
  - `#[test] fn no_spectator_state_exists()` — the player state enum has no `SPECTATOR` variant (compile-time assertion via exhaustive match).
  - `#[test] fn elimination_is_idempotent()`.
  - Pad and crates are destroyed on elimination; the pad volume becomes unplaceable.
  - Protected volume around cores rejects placement (`#[test] fn cannot_build_on_core()`).

---

**T-014 — `sim`: match rules, modes, caps, bot-win rule**

- **Depends:** T-013
- **Files:** `crates/sim/src/match_rules.rs`, `crates/sim/src/state.rs`
- **AC:**
  - `#[test] fn cap_matrix()` — all 9 rows of §4.7 enforced exactly; a craft attempt at cap consumes nothing.
  - FFA and Teams assignment; `#[test] fn team_balance()` for 2/4/6 players.
  - Win check: last team standing, or all cores destroyed simultaneously (draw) → `Ended`.
  - **D-05 bot rule:** `#[test] fn match_ends_when_last_human_dies()` — with 1 human + 5 bots, killing the human ends the match immediately with `winner = None` and `reason = NO_HUMANS`, **even though friendly bots are alive**. `#[test] fn bots_cannot_win_alone()` asserts a bot team reaching the last-core state still produces `winner = None`.
  - Overtime decay: `#[test] fn match_always_terminates()` — 100 random bot matches all end ≤ 25 simulated minutes.
  - `state_hash()` covers every field that affects gameplay; `#[test] fn hash_changes_on_every_mutation()`.

---

**T-015 — Determinism harness and golden fixtures**

- **Depends:** T-009 … T-014
- **Files:** `crates/sim/tests/golden.rs`, `tests/golden/*.hash`, `tools/replay/`
- **AC:**
  - 3 fixtures: `(seed, [InputFrame; 20_000])` → state hash every 100 ticks.
  - Native and wasm produce **identical hash sequences** for all fixtures.
  - Goldens committed; a changed hash fails CI with a diff pointing at the first diverging tick and field.
  - `cargo test -p sim --test golden` runs on both x86_64 and ARM CI runners.
  - A deliberate `f32` mutation in `movement.rs` makes the suite fail (verified once, manually, then reverted — record it in the PR).

---

**T-016 — Tuning data pipeline**

- **Depends:** T-003
- **Files:** `data/tuning.ron`, `crates/sim/src/tuning.rs`, `build.rs`
- **AC:**
  - `data/tuning.ron` parses into `Tuning` at compile time; invalid data fails the build with the field name.
  - Every constant used by `sim` comes from `Tuning` — verified by a grep test that no numeric literal > 1 appears in `movement.rs`/`combat.rs`/`base.rs` outside of unit conversions.
  - `#[test] fn tuning_matches_docs()` — the values in §4.11 are asserted equal to the parsed struct (catches doc/code drift).

---

### Phase B — Room core (T-017 … T-024)

---

**T-017 — `protocol`: message types**

- **Depends:** T-014
- **Files:** `crates/protocol/src/lib.rs`
- **AC:**
  - Every message in §7.1/§7.2 exists with the specified fields and tags.
  - `PROTOCOL_VERSION` exported.
  - postcard roundtrip for every message (`#[test] fn roundtrip_all()` via a macro over the enum).
  - `#[test] fn sizes_match_spec()` — each message's encoded size is within ±10% of the §7.1/§7.2 table.

---

**T-018 — `protocol`: codec, validation, fuzz target**

- **Depends:** T-017
- **Files:** `crates/protocol/src/codec.rs`, `fuzz/fuzz_targets/decode.rs`
- **AC:**
  - `decode_checked` enforces every rule in §7.4 (size cap, unknown tag, bounds, name/chat sanitisation).
  - `#[test] fn rejects_oversize()` at 4,097 bytes.
  - `cargo fuzz run decode -- -max_total_time=60` finds no panic and no OOM.
  - Sanitiser: `#[test] fn strips_xss()` — `<img src=x onerror=alert(1)>` becomes an empty/allowed-chars string.

---

**T-019 — `room`: lifecycle state machine**

- **Depends:** T-017
- **Files:** `crates/room/src/lifecycle.rs`
- **AC:**
  - States and transitions exactly as §6.10, including un-ready during countdown.
  - `#[test] fn illegal_transitions_are_rejected()` — every disallowed pair returns an error, not a panic.
  - Empty room for 60 s → `CLEANUP` and the persistence callback fires exactly once.
  - No I/O, no `async`, no clock — time is passed in as a tick count (`#[test] fn no_std_clock()`: crate has no `std::time` import).

---

**T-020 — `room`: tick loop and input queue**

- **Depends:** T-019, T-014
- **Files:** `crates/room/src/lib.rs`
- **AC:**
  - `Room::tick() -> TickOutput` advances exactly 50 ms of simulated time, pure function of (state, queued inputs).
  - Input queue consumes the newest frame with `tick ≤ now`, discards older; `#[test] fn stale_input_is_dropped()`.
  - 200 ms with no input → treated as no keys held (`#[test] fn afk_player_stops_moving()`).
  - `#[test] fn tick_is_reentrant_safe()` — two rooms ticked in the same process do not share state.

---

**T-021 — `room`: snapshot encoder and interest management**

- **Depends:** T-020
- **Files:** `crates/room/src/snapshot.rs`
- **AC:**
  - Per-client AOI at 48 m with 4 m hysteresis (§6.3).
  - Delta against `baseline_seq`; keyframe every 60 ticks; `#[test] fn keyframe_equals_full_state()`.
  - `#[test] fn snapshot_under_2kb()` — worst case (6 players + 12 buddies + 24 projectiles, all moving) encodes to ≤ 2,048 bytes.
  - `#[test] fn stationary_entity_costs_3_bytes()`.
  - Events (craft, kill, base hit) are never culled.

---

**T-022 — `room`: block edit pipeline**

- **Depends:** T-020, T-008
- **Files:** `crates/room/src/edit.rs`
- **AC:**
  - Validation order per §6.6 (reach, existence, protection, rate).
  - `#[test] fn reach_hack_rejected()` at 7 m.
  - `#[test] fn same_tick_race()` — two edits on one block: first accepted, second rejected, one `S_EDIT` broadcast.
  - `#[test] fn diff_version_monotonic()`.
  - `#[test] fn edit_rate_limited()` at 9 edits in one tick.

---

**T-023 — `room`: bots**

- **Depends:** T-021, T-025
- **Files:** `crates/room/src/bot.rs`
- **AC:**
  - Deterministic bots that **collect airdrops, carry crates to their pad, build a valid recipe**, fight, and push a core — driven only through the same input API as a human (`#[test] fn bot_uses_only_input_frames()`).
  - A full 6-bot match completes in ≤ 25 simulated minutes and produces a winner among bots (`#[test] fn bots_can_win_against_bots()`), while still respecting D-05 when a human is present.
  - Bots are reusable by `tools/bot-swarm` and by offline mode (single implementation, no second AI).

---

**T-024 — Integration harness: `match_sim`**

- **Depends:** T-021, T-022, T-023
- **Files:** `crates/room/tests/match_sim.rs`
- **AC:**
  - Runs a full match headless in ≤ 2 s wall time.
  - All 8 invariants of §12.4 asserted every tick.
  - `#[test] fn no_entity_leak()` over 3 consecutive matches in one process.
  - `#[test] fn reconnect_mid_match()` — player drops at tick 400, reconnects at tick 1000, state is consistent.

---

**T-025 — `sim`: airdrop system** *(new in v1.1 — D-01)*

- **Depends:** T-009, T-016
- **Files:** `crates/sim/src/airdrop.rs`, `crates/sim/tests/airdrop.rs`
- **AC:**
  - Schedule: first drop at 0:10, every 25 s to 5:00, every 15 s after; `#[test] fn drop_schedule_over_20_min()` produces 59 ± 1 drops.
  - Drop size = `3 × player_count`, ×1.75 on every 5th drop; `#[test] fn drop_size_scales_with_players()` for 2/4/6 players.
  - Site selection per §4.4.2 is deterministic and **identical on native and wasm** (`#[test] fn site_selection_is_portable()`), never repeats the previous site, and never selects a base pad.
  - Falling entity: 5 s telegraph then descent at 12 m/s from Y 120; `#[test] fn crates_land_on_ground()` and `#[test] fn landing_scatter_stays_in_3m()`.
  - Pickup within 1.2 m, capped at 12 carried; `#[test] fn carry_cap_enforced()`.
  - Deposit at 6/s inside own pad only; `#[test] fn cannot_deposit_in_enemy_pad()`.
  - Conservation: `#[test] fn crates_are_conserved()` — dropped − consumed − destroyed == carried + on ground + placed.
  - Death drops 50% of carried crates; pickups despawn at 180 s; `#[test] fn pickup_cap_never_exceeded()` at 120.
  - 60 drops + 120 pickups simulate in ≤ 0.4 ms per tick.

---

### Phase C — Client foundation (T-030 … T-039)

---

**T-030 — wasm bridge**

- **Depends:** T-009, T-010, T-017
- **Files:** `crates/client-wasm/src/lib.rs`, `crates/client-wasm/src/render_buffer.rs`, `apps/web/src/wasm/loader.ts`
- **AC:**
  - Exactly the 13 exports of §8.4 exist, with those signatures.
  - `RenderDesc` and `GpuPlayer` layouts match the spec byte-for-byte (`#[test] fn layout_is_stable()` asserting `size_of` and `offset_of`).
  - `cb_tick` performs **zero** allocations after the first 10 frames (measured with a counting allocator).
  - `#[test] fn render_desc_magic()` — a stale or zeroed buffer is detected via the `0xCBA7E001` guard rather than read as garbage.
  - `loader.ts` calls `rebuildViews()` after any `cb_mem_alloc`, and asserts `view.buffer.byteLength > 0` every frame in dev builds.

---

**T-031 — three.js bootstrap, quality tiers, dynamic resolution**

- **Depends:** T-030
- **Files:** `apps/web/src/main.ts`, `apps/web/src/render/renderer.ts`, `apps/web/src/render/quality.ts`
- **AC:**
  - WebGL2 context created; a clear-colour frame renders at 60 fps on desktop.
  - DPR clamp `min(devicePixelRatio, 2)` — asserted by reading `renderer.getPixelRatio()` under an emulated DPR of 3.
  - Dynamic resolution: forced 25 ms frames drop `scale` within 1 s and never below 0.55; forced 8 ms frames restore it within 2 s (Playwright spec with a synthetic frame-time injector).
  - The three quality tiers apply exactly the render distance / particle cap / fog values in §8.5.
  - Boot benchmark selects a tier and persists it to `localStorage`.

---

**T-032 — Chunk manager**

- **Depends:** T-031
- **Files:** `apps/web/src/render/chunkManager.ts`
- **AC:**
  - Streams columns within 6 chunks of the player; unloads beyond 7 (hysteresis).
  - Mesh handoff follows the ownership protocol of §8.4 (`.slice()` copy, `cb_release_mesh` called).
  - `renderer.info.render.calls ≤ 120` with the player at map centre at render distance 6.
  - `#[test] fn no_wasm_view_leak()` — after 200 load/unload cycles, JS heap growth < 5 MB and no `ArrayBuffer` is detached.
  - Visible geometry is still correct after a forced `memory.grow` (test hook allocates 64 MB).

---

**T-033 — Entity renderer**

- **Depends:** T-032
- **Files:** `apps/web/src/render/entities.ts`, `apps/web/src/render/particles.ts`
- **AC:**
  - Players and buddies render as instanced 3-part meshes; 18 entities cost ≤ 27 draw calls (asserted).
  - Per-instance animation (bob, tilt, attack pulse, death dissolve) driven only from `RenderDesc` floats.
  - 256 particles in a single instanced draw with GPU-side lifetime.
  - `#[test] fn entity_count_scales()` — 18 entities at 60 fps with ≤ 3 ms CPU in `entities.update()`.

---

**T-034 — Block interaction: raycast, reach, ghost preview**

- **Depends:** T-032
- **Files:** `apps/web/src/game/interaction.ts`
- **AC:**
  - DDA voxel raycast returns the first solid block within 6.0 m; `#[test] fn raycast_accuracy()` over 20 known cases.
  - Targeted block shows a wireframe highlight; the hit face normal determines the placement cell.
  - Ghost preview renders the selected block translucently before commit.
  - Break/place call `cb_local_edit` and record the pending edit for rollback.
  - `#[test] fn cannot_target_through_wall()`.

---

**T-035 — Desktop input**

- **Depends:** T-034
- **Files:** `apps/web/src/game/inputDesktop.ts`
- **AC:**
  - Pointer lock on canvas click; WASD, mouse look, hotbar `1–9`, scroll cycle, LMB/RMB, `Q/Tab/E/F/Esc`.
  - Input packs into the 4-byte `InputFrame` of §7.1 and reaches `cb_tick` exactly once per frame.
  - `#[test] fn no_input_when_unfocused()` — window blur releases all keys (prevents the classic "player runs forever" bug).
  - Sensitivity and invert-Y persist across reloads.

---

**T-036 — Touch input**

- **Depends:** T-034
- **Files:** `apps/web/src/game/inputTouch.ts`
- **AC:**
  - Virtual stick spawns where the thumb lands on the left half; look-drag on the right half; both simultaneously (multi-touch, asserted in a Playwright mobile test).
  - **Mode-based tap** (§8.6): pickaxe selected → tap breaks; block selected → tap places. No long-press ambiguity.
  - **Deposit control:** holding the place button inside the pad deposits continuously at the §4.4.3 rate.
  - **Stance toggle (D-03):** one button, cycles Follow / Hold / Assault, applies to all of the player's buddies; icon shows the current stance.
  - Crosshair icon always reflects the active mode; jump, stance, and hotbar targets ≥ 44 px.
  - `#[test] fn stick_and_look_are_independent()`.

---

**T-037 — HUD core and reactive store**

- **Depends:** T-031
- **Files:** `apps/web/src/ui/store.ts`, `apps/web/src/ui/hud.ts`, `apps/web/src/ui/hud.css`
- **AC:**
  - Vanilla store; numeric HUD writes coalesced to **10 Hz max** (count DOM mutations over 1 s of 20 Hz updates → ≤ 10 per field).
  - Bars animate via `transform: scaleX()` only (asserted: zero `width` style writes).
  - All text via `textContent`; CI grep asserts no `innerHTML` under `apps/web/src/ui`.
  - HUD frame cost ≤ 1 ms (`performance.measure`).

---

**T-038 — Recipe book, ghost overlay, near-miss**

- **Depends:** T-037, T-010
- **Files:** `apps/web/src/ui/recipeBook.ts`, `apps/web/src/render/ghostOverlay.ts`
- **AC:**
  - Every unlocked recipe renders as a top-down grid generated from the same canonical data the matcher uses — no duplicated shape data anywhere.
  - Locked tiers are visible with their unlock time.
  - Near-miss: a cluster within Hamming distance 2 shows "needs N more", computed from the canonical form.
  - `#[test] fn near_miss_never_lies()` — over 500 random clusters, the UI prediction matches the server matcher's result.

---

**T-039 — Lobby, settings, screen manager**

- **Depends:** T-037
- **Files:** `apps/web/src/ui/screens.ts`, `apps/web/src/ui/lobby.ts`, `apps/web/src/ui/settings.ts`
- **AC:**
  - Boot → Main (Online / Offline / Settings) → Lobby → Match → Results; no unreachable screen, no dead end.
  - Create Room shows a 6-char code with a copy button; Join Room validates format before connecting.
  - Settings: quality tier, sensitivity, invert Y, UI scale, reduced motion, remap — all persisted.
  - `#[test] fn every_screen_is_reachable()` (Playwright traversal).

---

### Phase D — Client networking (T-040 … T-044)

---

**T-040 — Net client**

- **Depends:** T-017, T-030
- **Files:** `apps/web/src/game/net/transport.ts`, `apps/web/src/game/net/client.ts`
- **AC:**
  - `Transport` interface with `WsTransport`; one protocol message per WebSocket frame.
  - Reconnect with exponential backoff (0.5 s → 8 s, ±20% jitter), max 6 attempts, token reused from `sessionStorage`.
  - `#[test] fn protocol_mismatch_shows_message()` — `S_ERROR(VERSION_MISMATCH)` renders a readable screen, never a silent disconnect.
  - RTT measured via `C_PING`/`S_PONG` at 1 Hz and shown in a debug overlay.

---

**T-041 — Prediction and reconciliation**

- **Depends:** T-040, T-030
- **Files:** `apps/web/src/game/predict.ts`, `crates/client-wasm/src/predict.rs`
- **AC:**
  - 128-entry input ring buffer; `#[test] fn replay_matches_server()` — over 1,000 snapshots with injected error, post-reconciliation position is within 1 cm of the server's.
  - Corrections below 5 cm are ignored; above, replay from the acked state through unacked inputs.
  - Visual residual decays as `exp(-dt/0.15)`; `#[test] fn no_visible_snap()` — max per-frame delta after a 1 m correction is ≤ 0.05 m.
  - At 300 ms simulated RTT the player stays controllable with no input loss.

---

**T-042 — Chunk diff streaming and apply**

- **Depends:** T-040, T-032, T-008
- **Files:** `apps/web/src/game/net/chunkSync.ts`
- **AC:**
  - `S_CHUNK_DIFF` applied per column only when `version` is newer; keyframe columns replace the whole column.
  - Diffs prioritised by distance; the column under the player always lands first.
  - `#[test] fn edit_then_join()` — a client joining after 400 edits produces a terrain `state_hash` identical to the server's.
  - No decoded snapshot carries more than 1.5 KB of diff payload (asserted in the decoder).

---

**T-043 — Snapshot decode and entity interpolation**

- **Depends:** T-040, T-033
- **Files:** `apps/web/src/game/net/snapshot.ts`, `apps/web/src/game/interp.ts`
- **AC:**
  - Decode into `RenderDesc` with zero per-entity object allocation (pooled; `#[test] fn no_gc_in_hot_path()` — zero allocations over 600 frames).
  - 100 ms interpolation buffer; extrapolate at most 100 ms, then freeze.
  - `#[test] fn interpolation_is_smooth()` — a constant-velocity target produces monotonic rendered positions with no reversal.
  - Losing 3 consecutive snapshots produces no visible teleport (asserted via max per-frame delta).

---

**T-044 — Juice layer**

- **Depends:** T-033, T-037
- **Files:** `apps/web/src/render/juice.ts`, `apps/web/src/render/shake.ts`
- **AC:**
  - Every effect in §4.9 exists and is driven by events from `RenderDesc`, not by polling state.
  - Hitstop freezes presentation only — `#[test] fn hitstop_does_not_stop_simulation()`.
  - Damage numbers capped at 32 concurrent, oldest evicted.
  - Reduced-motion disables shake, vignette pulse and slow-motion but **not** hit flash or damage numbers.
  - Total juice cost ≤ 1.5 ms at 18 entities.

---

### Phase E — Hosts (T-045 … T-048)

---

**T-045 — `host-axum`: WebSocket server**

- **Depends:** T-024
- **Files:** `crates/host-axum/src/main.rs`, `crates/host-axum/src/ws.rs`, `crates/host-axum/src/rooms.rs`
- **AC:**
  - `GET /ws?room=<code>&token=<opt>` upgrades; non-matching `Origin` is rejected.
  - One task per room driving the 20 Hz tick; `#[test] fn tick_interval_p99()` ≤ 55 ms with 6 bots.
  - Graceful shutdown drains rooms and persists summaries.
  - `GET /healthz` returns 200 with room count and uptime.
  - **This is also the local dev server** — `cargo run -p host-axum` plus `pnpm dev` must be a complete development loop with no deployment required.

---

**T-046 — `host-worker`: Durable Object adapter**

- **Depends:** T-045
- **Files:** `crates/host-worker/src/lib.rs`, `wrangler.jsonc`
- **AC:**
  - `Room` DO implements the same `RoomHost` trait as `host-axum`; **zero game logic** in the adapter (grep test: no `sim::` reference outside `room::`).
  - `setInterval(50ms)` tick loop with `ws.accept()`; hibernation deliberately not used (§10.3).
  - A 6-bot match runs end-to-end under `wrangler dev`.
  - `wrangler deploy` succeeds with `new_sqlite_classes`.
  - **Measured:** p99 CPU per WebSocket message ≤ 3 ms, recorded in the commit message — this is the number that decides whether the free tier's per-invocation CPU limit is a real constraint.

---

**T-047 — Room registry and create/join**

- **Depends:** T-046
- **Files:** `crates/host-worker/src/registry.rs`, `apps/web/src/ui/roomCode.ts`
- **AC:**
  - Codes are 6 chars of Crockford base32, collision-checked on create.
  - `POST /rooms` creates and returns a code; `GET /rooms/:code` returns existence and player count. **No enumeration endpoint exists.**
  - Per-IP limits from §11.3 enforced in the registry DO.
  - `#[test] fn brute_force_lockout()` — 6 failed joins from one IP trigger a 15-minute lockout.
  - Quota guard: new rooms rejected at 85% of the daily request budget with a readable message (§9.3).

---

**T-048 — Offline mode**

- **Depends:** T-024, T-030, T-037
- **Files:** `crates/client-wasm/src/offline.rs`, `apps/web/src/game/offline.ts`, `apps/web/src/workers/room.worker.ts`
- **AC:**
  - The `room` crate runs in a Web Worker; the main thread exchanges the same message bytes as the network path (no second protocol).
  - Bots fill empty slots per §4.10; both FFA and Teams playable.
  - A full offline match is completable: mine → craft → buddy → destroy a core → results.
  - `#[test] fn offline_and_online_share_rules()` — an identical input log yields an identical state hash in both paths.
  - Works with the network fully disabled (Playwright `offline: true`).

---

### Phase F — Feel, content, security (T-049 … T-052)

---

**T-049 — Audio engine**

- **Depends:** T-031
- **Files:** `apps/web/src/audio/engine.ts`, `apps/web/src/audio/worklets/rumble.ts`, `apps/web/public/assets/sfx/*`
- **AC:**
  - Graph per §8.8 (master → compressor → buses); 24-voice cap with distance culling.
  - `AudioWorklet` rumble responds to the `base_under_attack` parameter within one audio quantum.
  - iOS unlock: context resumes only after a real `pointerdown`; `#[test] fn ios_unlock_flow()` on WebKit.
  - Spatial panning is audibly correct for a buddy to the player's left (asserted via `PannerNode` position in a test hook).
  - All 30 SFX of §8.8 present; total ≤ 200 KB.

---

**T-050 — Anti-cheat validation layer** *(§11)*

- **Depends:** T-024
- **Files:** `crates/room/src/validate.rs`, `crates/room/tests/cheat_matrix.rs`, `tools/cheat-client/`
- **AC:**
  - Every threat T1–T15 of §11.2 has an executable test named after its ID (`fn t2_reach_hack_rejected()`).
  - `#[test] fn no_client_position_acceptance()` — a grep test proves no protocol message carries a position or a damage value.
  - Rate limits of §11.3 enforced and tested per channel.
  - `tools/cheat-client` attempts all 15 attacks; every attempt is rejected or clamped, and its output is committed as a fixture.

---

**T-051 — Art and asset pipeline**

- **Depends:** T-033
- **Files:** `apps/web/public/assets/atlas.png`, `apps/web/public/assets/models/*.glb`, `tools/asset-pipeline/`
- **AC:**
  - 256×256 atlas, 24 tiles, nearest filtering, no mipmaps, ≤ 30 KB.
  - 6 buddy meshes + 3 player meshes, each < 400 tris, vertex colours, no textures, total ≤ 40 KB.
  - Every block id in `data/blocks.ron` has an atlas tile (`#[test] fn every_block_has_a_tile()`).
  - Total blocking asset payload ≤ 90 KB (§9.4).

---

**T-052 — Content pass: recipes, buddies, map**

- **Depends:** T-038, T-051
- **Files:** `data/recipes.ron`, `data/buddies.ron`, `data/tuning.ron`, `tools/recipe-editor/`
- **AC:**
  - All 14 recipes of §4.5.3 present, unique canonical forms, authored shapes.
  - All 6 buddy roles of §4.6 present with stats and caps.
  - `tools/recipe-editor` renders every recipe as a grid and refuses to save a duplicate or unsupported shape.
  - `#[test] fn content_is_reachable()` — every recipe can be built from a legal ground-supported placement in a flat test world.

---

### Phase G — Hardening (T-053 … T-058)

---

**T-053 — Bot swarm load test**

- **Depends:** T-045, T-023
- **Files:** `tools/bot-swarm/src/main.rs`, `tools/bot-swarm/report.md`
- **AC:**
  - 20 rooms × 6 bots for 3 simulated minutes against `host-axum`.
  - Records tick p50/p99, snapshot bytes p95, RSS, CPU%.
  - `#[test] fn tick_p99_under_12ms()` at 20 rooms.
  - Report committed with measured numbers; §9.3's Oracle capacity assumption is either confirmed or corrected in the same commit.

---

**T-054 — Playwright E2E suite**

- **Depends:** T-048, T-045
- **Files:** `tests/e2e/*.spec.ts`, `playwright.config.ts`
- **AC:**
  - All 8 specs of §12.5 pass on chromium, firefox, webkit, and a mobile viewport.
  - `two-client.spec.ts` runs two browser contexts against one `host-axum` instance.
  - `airdrop.spec.ts` asserts the full loop: telegraph appears → crates land → pickup → carry count rises → deposit → recipe completes.
  - `bot-win.spec.ts` asserts D-05: eliminating the only human ends the match with no winner even with friendly bots alive.
  - Suite completes in CI in ≤ 6 minutes (asserted — a suite that is skipped is a suite that does not exist).

---

**T-055 — Performance gates in CI** *(§8.10)*

- **Depends:** T-054
- **Files:** `.github/workflows/perf.yml`, `tests/e2e/perf.spec.ts`, `perf.budget.json`
- **AC:**
  - Every gate in §8.10 asserted, and violating one fails the build.
  - wasm size gate prints the brotli size in the PR comment.
  - Frame gate runs a 60 s scripted replay and asserts p95 ≤ 20 ms.
  - All thresholds live in `perf.budget.json`, not scattered through the workflow.

---

**T-056 — Observability and kill switches** *(§13)*

- **Depends:** T-046
- **Files:** `crates/host-worker/src/telemetry.rs`, `crates/host-axum/src/telemetry.rs`, `apps/web/src/telemetry.ts`, `docs/RUNBOOK.md`
- **AC:**
  - Structured JSON logs, one line per event, for every signal in §13.
  - Client crash and wasm trap reporting wired to Sentry/GlitchTip.
  - `ROOMS_ENABLED`, `PROTOCOL_MIN_VERSION`, `CLIENT_BUILD_OVERRIDE` implemented and tested.
  - `docs/RUNBOOK.md` documents each alert, its likely cause, and its one-line mitigation.

---

**T-057 — Deployment pipelines**

- **Depends:** T-055, T-056
- **Files:** `vercel.json`, `.github/workflows/deploy.yml`, `deploy/Caddyfile`, `deploy/cratebound.service`
- **AC:**
  - Push to `main` deploys the client to Vercel; a tag deploys the worker.
  - `vercel.json` headers match §10.2 exactly, including `application/wasm` and the CSP with `wasm-unsafe-eval`.
  - Cold load from the deployed URL reaches first playable in ≤ 3 s on an 8 Mbps profile.
  - `deploy/` contains a working systemd unit and Caddyfile for the fallback VM.
  - The §10.6 rollback procedure is executed once and documented.

---

**T-058 — Documentation**

- **Depends:** T-057
- **Files:** `docs/PROTOCOL.md`, `docs/ARCHITECTURE.md`, `docs/CONTRIBUTING.md`, `README.md`
- **AC:**
  - `PROTOCOL.md` is generated from the `protocol` crate's types — no hand-maintained duplicate that can drift.
  - `ARCHITECTURE.md` states the boundary rule and determinism rules verbatim from §5.
  - `README.md` takes a developer from `git clone` to a running local match in ≤ 4 commands, verified on a clean machine.

---

### Phase H — Release (T-059 … T-064)

---

**T-059 — Security headers, CSP, secret scan**

- **Depends:** T-057
- **Files:** `.github/workflows/security.yml`, `apps/web/src/security.ts`
- **AC:**
  - CSP from §10.2 applied and verified by a Playwright test asserting zero `securitypolicyviolation` events.
  - CI greps the built bundle for `SESSION_SIGNING_KEY`, `ROOM_REGISTRY_TOKEN`, and any 32+ char base64 blob → fails on match.
  - `#[test] fn no_innerhtml()` across `apps/web/src`.
  - `cargo audit` and `pnpm audit` run and fail on high severity.

---

**T-060 — Balance pass with instrumentation** *(M5 gate)*

- **Depends:** T-054, T-052
- **Files:** `apps/web/src/telemetry/economy.ts`, `docs/BALANCE.md`
- **AC:**
  - The four economy metrics of §18 (suggestion 3) are recorded per match: crates/min, recipes completed, buddies spawned, **time-to-first-buddy**.
  - 10 bot-vs-bot matches produce a report; time-to-first-buddy lands in 60–90 s (§4.2) or tuning is adjusted and the report re-run.
  - `BALANCE.md` records every changed constant with the before/after number that justified it.

---

**T-061 — Accessibility audit**

- **Depends:** T-039
- **Files:** `apps/web/src/ui/a11y.css`, `docs/A11Y.md`
- **AC:**
  - Reduced-motion, UI scale, remap, and colourblind-safe team colours all functional.
  - Every menu element reachable by keyboard (in-match is pointer-locked by design).
  - All HUD text ≥ 4.5:1 contrast against its background (measured, not eyeballed).
  - `docs/A11Y.md` lists what is supported and what is explicitly not.

---

**T-062 — Device matrix** *(manual — cannot be automated, §12.7)*

- **Depends:** T-055
- **Files:** `docs/DEVICE-MATRIX.md`
- **AC:**
  - Tested on ≥ 1 iOS Safari device, ≥ 2 Android devices (one mid-tier), and desktop Chrome/Firefox/Safari.
  - Each row records first playable (s), frame p95 (ms), thermal behaviour over 10 minutes, and touch usability.
  - Any device failing the 20 ms p95 gate is documented with its tier setting, not silently dropped.

---

**T-063 — Launch checklist**

- **Depends:** T-057 … T-062
- **Files:** `docs/LAUNCH.md`
- **AC:**
  - Every account and secret from §17 provisioned and verified present in the target environment.
  - Quota guard, kill switches, and rollback each exercised once.
  - A full 6-player match completed by six humans on the production URL.
  - Known limitations published (§11.4) — no silent overclaiming of anti-cheat.

---

**T-064 — Post-launch triage**

- **Depends:** T-063
- **Files:** `docs/BACKLOG-v1.1.md`
- **AC:**
  - First week's telemetry reviewed against the §13 thresholds.
  - Every desync report triaged using its input-log hash.
  - v1.1 backlog ordered by measured impact, not enthusiasm.

---

## 16. Milestones and gates

Milestones are **exit criteria**, not dates. A milestone is complete when its gate passes; nothing downstream starts before it does.

| Milestone                 | Tasks                               | Exit criteria (the gate)                                                                                                                                                                                |
| ------------------------- | ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **M0 — Skeleton**         | T-001 … T-008                       | `cargo test` green on x86_64 **and** ARM. A terrain column generates and meshes in a native test binary. No game yet — just the world.                                                                  |
| **M1 — Rules exist**      | T-009 … T-016                       | T-015's golden hashes pass native vs wasm. **The recipe matcher's 13-case matrix is green, including every negative case.** This gate decides whether the core mechanic is real.                        |
| **M2 — Playable offline** | T-017 … T-025, T-030 … T-038, T-048 | A human completes an offline match against bots: catch an airdrop → carry → stack on the pad → buddy spawns → destroy a core → results. **Human judgment gate:** if this is not fun, stop and fix the design before writing any network code. |
| **M3 — Online**           | T-040 … T-047                       | Two browser windows join a room code and play together with prediction and no visible desync. Tick p99 ≤ 12 ms with 6 bots.                                                                             |
| **M4 — Ship-shape**       | T-049 … T-058                       | All CI gates green. Deployed to Vercel + Cloudflare. `RUNBOOK.md` complete. Rollback executed once.                                                                                                     |
| **M5 — Tuned**            | T-059 … T-062                       | Time-to-first-buddy in 60–90 s. Accessibility audit passed. Device matrix recorded with real numbers.                                                                                                   |
| **M6 — Live**             | T-063 … T-064                       | Six humans complete a match on the production URL. Quota guard, kill switch, and rollback each exercised.                                                                                               |

**M2 is the most important gate in this plan.** Everything after it is engineering that can be estimated. Before it, you are still guessing whether the game is worth the engineering — and a voxel builder with a weak production mechanic is a worse Minecraft, which is a game that already exists.

---

## 17. What you must prepare

### Accounts (create in this order — each unblocks a task)

| # | Account                    | Why                                                                                                                      | Card?               | Unblocks              |
| - | -------------------------- | ------------------------------------------------------------------------------------------------------------------------ | ------------------- | --------------------- |
| 1 | GitHub                     | Repo + Actions CI                                                                                                        | No                  | T-001                 |
| 2 | Vercel (Hobby)             | Client hosting                                                                                                           | No                  | T-057                 |
| 3 | Cloudflare (Free)          | Workers + Durable Objects                                                                                                | No                  | T-046, T-057          |
| 4 | Oracle Cloud (Always Free) | Fallback host — *only if* you accept card verification                                                                   | **Yes**             | T-057 (fallback path) |
| 5 | ~~Upstash (Free)~~         | **Not used in v1.** The room registry is a Durable Object (§6.9); 500k commands/month would be the first thing to break. | —                   | —                     |
| 6 | Sentry / GlitchTip (Free)  | Client crash + wasm trap reporting                                                                                       | No                  | T-056                 |
| 7 | Domain registrar           | `cratebound.gg` or similar; needed for a stable WS origin and TLS                                                        | Yes (paid, ~$12/yr) | T-057                 |

> Item 7 is the only thing in this plan that costs money. It is not strictly required — `*.vercel.app` + `*.workers.dev` work — but a game URL on a shared domain looks untrustworthy and makes CSP/origin rules messier.

### Secrets and configuration

| Name                                    | Scope                          | Where it lives                                    | Never                        |
| --------------------------------------- | ------------------------------ | ------------------------------------------------- | ---------------------------- |
| `SESSION_SIGNING_KEY`                   | Ed25519 private key, base64    | Cloudflare secret / systemd `EnvironmentFile`     | client bundle, repo, CI logs |
| `ROOM_REGISTRY_TOKEN`                   | shared secret, host ↔ registry | Cloudflare secret                                 | client                       |
| `SENTRY_DSN` (server)                   | server-side ingest             | host env                                          | client                       |
| `VITE_SENTRY_DSN` (client)              | public ingest key              | Vercel env, `VITE_` prefix = **public by design** | —                            |
| `VITE_GAME_WS_URL`                      | `wss://…`                      | Vercel env, public                                | —                            |
| `CLOUDFLARE_API_TOKEN`                  | deploy scope only              | GitHub Actions secret                             | repo                         |
| `VERCEL_TOKEN`                          | deploy scope only              | GitHub Actions secret                             | repo                         |
| `ROOMS_ENABLED`, `PROTOCOL_MIN_VERSION` | runtime flags                  | host env                                          | —                            |
|                                         |                                |                                                   |                              |
|                                         |                                |                                                   |                              |

**Rule:** anything prefixed `VITE_` is compiled into the client and is public. There is no such thing as a secret in the frontend. If a value must stay secret, it belongs on the host and the client asks the host for a decision.

### Assets you must supply or approve

| Asset                | Spec                                                                                 | Notes                                                                                     |
| -------------------- | ------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------- |
| Block texture atlas  | 16×16 px tiles, 256×256 or 512×512 sheet, power-of-two                               | ~24 block types in v1 (§4). Pixel-art, nearest-neighbour filtering.                       |
| Buddy sprites/models | Low-poly voxel-ish, ≤ 400 tris each, 6 roles × 3 animation states (idle/walk/attack) | Keep to one shared skeleton per role family.                                              |
| UI font              | 1 display + 1 body, woff2, subset to Latin                                           | Budget 60 KB total.                                                                       |
| SFX                  | ~30 one-shots, mono, 22.05 kHz OGG/Opus                                              | Impact, block break/place, craft complete, buddy spawn/death, base hit, victory/defeat.   |
| Name / branding      | Final game name, logo, favicon                                                       | Working title is Cratebound; rename is a find-replace if strings are externalised (A-11). |

### Decisions you owe me (blocking)

1. **FFA or Teams as the default mode?** The plan supports both, but tuning effort should concentrate on one. My recommendation: Teams, because buddy caps and base defence only become interesting with coordination.
2. **Guest-only, or accounts in v1?** Plan assumes guest-only (A-02). Accounts add a database and a password-reset flow for near-zero gameplay value at this stage.
3. **Is this ever going to be commercial?** If yes, Vercel Hobby is disqualified (R-08) and the client moves to Cloudflare Pages from day one.

---

## 18. Decisions, open questions, and suggestions

### 18.1 Locked decisions (v1.1)

| ID | Decision | What it changed |
|---|---|---|
| D-01 | **Airdrop crates, no mining.** Crates fall on a global timer, are carried to an 8×8 stacking pad, and placed there. | Removed the entire resource chain; collapsed the per-column block-edit budget from 1.2 KB realistic / 72 KB theoretical to **768 B worst case**; added T-025. |
| D-02 | **Instant elimination, no spectator.** Core destroyed → every living player on that team dies on the same tick → defeat screen → leave. | Deleted the spectator camera, spectator HUD, and spectator snapshot fan-out; simplified §6.11. |
| D-03 | **Autonomous buddies with a one-button stance toggle** (Follow / Hold / Assault). | Confirmed as designed; no RTS layer, no selection cursor, mobile stays playable. |
| D-04 | **No meta-progression.** Every match starts equal. | Server stays stateless; no accounts, no inventory persistence (§6.9). |
| D-05 | **Bots fill empty slots but cannot win.** Last human eliminated → match ends with no winner. | New rule in T-014; `S_END.winner_team = -1` with `reason = NO_HUMANS`. |

### 18.2 What I decided on your behalf — please confirm or overrule

These are inferences from your answers, not things you said. Each is cheap to reverse now and expensive later.

1. **Terrain is indestructible.** This is the largest inference. "No traditional mining" removes the *resource* reason to dig, but not necessarily the *traversal* reason. I made terrain solid because it kills the tunnelling exploit, makes crate walls meaningful, and cuts the netcode's diff budget by ~100×. **If you wanted players to still carve tunnels through hills, say so** — `tuning.terrain.breakable = true` restores it with no architectural change, at the cost of that budget and a new exploit class.
2. **Crates can only be placed inside your own 8×8 stacking pad.** This follows from "carry them to their stacking pad", and it is what makes the matcher cheap and the base attackable. The alternative — place crates anywhere — would make every chunk a potential edit target and re-open the whole diff budget.
3. **Drop size scales with player count (`3 × player_count`).** A fixed drop size starves a 6-player match to 4 crates/min/player, which stalls the game. This is the single most important balance constant in the new economy and the first thing to tune at M5.
4. **Carry cap 12.** One drop in a duel, two-thirds of a drop at 6 players — so a full drop always needs either two trips or two players. This is deliberate friction that creates the mid-map fights.
5. **Eliminated players leave immediately** rather than watching. If you later want a "watch your friends finish" option, it is a spectator camera plus a snapshot subscription — the protocol already has what it needs, so it is additive, not a rewrite.

### 18.3 Open questions

1. **Stance scope:** the plan applies the toggle to *all* of the player's buddies at once. Team-wide (affecting allies' buddies too) would make coordination stronger but lets one player override another. I chose per-player.
2. **Mid-match disconnect:** the slot is held 30 s, then marked `ABANDONED` — no bot takeover. A bot takeover would keep the match fair but makes "is this a human?" unknowable. I chose the simpler, honest option.
3. **Should drops pause when every human is dead/AFK?** Currently no — the timer is wall-clock. This matters only for abandoned rooms, which the 60 s empty-room cleanup already handles.
4. **Default mode:** FFA or Teams? Still open from §17. My recommendation remains Teams, because buddy caps and base defence only become interesting with coordination.
5. **Commercial intent:** if this is ever monetised, Vercel Hobby is disqualified (R-08) and the client moves to Cloudflare Pages on day one.

### 18.4 Suggestions, in order of value

1. **The drop timer is your entire difficulty dial.** One constant — `interval_late_s` — controls how fast the game accelerates. If matches feel slow, drop it to 12 s before you touch anything else. If they feel like a loot vacuum, raise it. Almost every pacing complaint will be solved here, not in the combat numbers.
2. **Build offline mode first and make it the demo.** It costs nothing to host, exercises ~90% of the engine (worldgen, meshing, airdrops, recipes, buddies, combat, elimination), and gives you something to play before the netcode exists. It is why T-048 sits in M2, not M4.
3. **Instrument four numbers from the first playable:** crates/min per player, recipes completed per match, buddies spawned per team, and **time-to-first-buddy**. If time-to-first-buddy is over 90 s the opening feels dead; under 20 s the recipe system is decoration. Those four numbers will tell you more about fun than any survey.
4. **Make the stacking pad visually loud.** It is now the only place anything can be built and the thing an attacker must reach. If a player cannot find their own pad in the first five seconds, the whole loop fails at step one. A coloured floor, a beam, and a minimap icon are not polish — they are load-bearing.
5. **Do not add a second transport, renderer, or rules implementation "for flexibility".** Each one is a desync or a doubled maintenance cost. The architecture's value is that there is exactly one of each.
6. **Treat the free-tier ceiling as a design constraint.** ~6 hours of 6-player play per day is plenty for a launch week with 30 people. By the time it isn't, you will know whether the game deserves the Oracle VM.
