# Playability backlog

Living plan for the remaining game-feel work after the movement-latency pass
and the Settings Mouse panel. Update the checkboxes and the log as work lands.
Do not mark an item done without a test and a measured result.

Baseline, 2026-10-23:

- Local camera: 31.3 ms from W to motion in headed Opera at 1512x982 DPR 2.
- Render: 120.3 fps, 71 draw calls, 73k triangles in the same Opera run.
- Gates: 163 Playwright passed, 4 perf-gated skipped; 505 Rust tests.
- Live client: https://buddy-craft.vercel.app. ONLINE is blocked by T-046.

## 1. Match feedback and audio

Juice already consumes snapshot events through the wasm bridge, but the audio
event mapping still uses the old demo numbering. The airdrop marker exists in
Juice and needs an explicit match-path test.

- [x] Map protocol event kinds in `AudioEngine.handleEvent`:
      pickup 6, airdrop telegraph 8, low HP 9, craft blocked 10.
- [x] Play the `buddy_spawn` cue when a crafted recipe has a buddy output.
- [x] Add `lastCue` or an equivalent test hook and assert the mapping.
- [x] Assert a snapshot `AIRDROP_TELEGRAPH` makes the Juice marker visible in
      an offline match.
- [x] Confirm the HUD marker is readable when the drop is off screen; the
      label now clamps into a HUD-safe edge frame and flips when the drop is
      behind the camera. `juice.spec` covers both cases.

## 2. Pause shell and LEAVE MATCH

The fixed `LEAVE MATCH` bar covers the top center of the view. Replace it with
a small MENU control and a pause screen.

- [x] Pause screen with RESUME, SETTINGS, LEAVE MATCH.
- [x] Escape toggles match and pause; the HUD MENU button opens pause on touch.
- [x] Pause the offline Worker while the menu is open; online may keep running.
- [x] Settings opened from pause returns to pause; from main returns to main.
- [x] Remove the fixed `.match-leave` bar.
- [x] Update `screens.spec` to enter results through pause, and test resume.

## 3. Block feel

- [x] Hide a remote body within 1 m of the first-person camera so a shared
      spawn cell cannot fill the view; bodies that overlap at 2 m are still an
      onboarding/bot-separation risk.
- [x] Replace the white 1 m wireframe with a thin dark block outline.
- [x] Desktop hotbar: nine slots, selected state, number keys and wheel,
      clickable, hidden for coarse pointers because touch has its own hotbar.
- [x] First-person hand and held block: pickaxe on slot 0, crate on slots 1-8,
      own overlay pass with a cleared depth buffer, idle bob and 180 ms swing.
- [x] Break progress: five crack stages over a 400 ms hold. The placement
      ghost hides while mining so it cannot tint the hit face, and the edit is
      sent only when the hold completes. Quick clicks no longer break.
- [x] Break feedback: 10-particle burst, short `sword_crate` / `block_place`
      sound and a hand swing.
- [ ] Crate return is not applicable to the current item model: slots are an
      infinite creative palette and placing a block does not spend a carried
      crate. Carried crates are the recipe currency, not placeable items.
      Revisit only if block items get a finite inventory.
- [ ] Material-specific break and place sounds. Blocked on a block-at query:
      `VoxelSource` only answers `solid()`, and the wasm bridge has no
      block-at export, so the client cannot tell grass from stone. Add the
      query to the bridge first; do not guess the material from height.
- [ ] Keep the frame, draw-call and bundle budgets green.

## 4. World art

- [ ] Trees: 1x1 trunks, branches, uneven canopies, more than one silhouette.
      The current worldgen has single 1x1 trunks and corner-nibbled canopies.
- [ ] Animated water surface and a shoreline that reads as water. The arena
      map has no water in the visible valley, so this needs a water pass in
      worldgen before it can be reviewed.
- [x] Sky gradient, 18 instanced cloud banks and a sun direction. The fog
      colour now matches the horizon band; fog distances are unchanged to keep
      the arena readable. `sky.spec` samples three white cloud pixels and the
      top-to-horizon blue gradient.
- [x] Keep nearest filtering and the atlas and model byte budgets; `assets.spec`
      still passes with the extra sky and cloud draws.
- [ ] Re-run the browser playtest for readability in a forest; the sky pass was
      checked from above the map, not inside the canopy.

## 5. Sprint

- [x] Add a sprint bit to the input frame and server movement, target 5.6 m/s.
      Bit 5; the sim uses it only for forward movement on land.
- [x] Left Shift on desktop; a touch RUN button for mobile.
- [x] FOV kick from 70 to 76 while sprinting, respecting reduced motion.
- [x] Tests for the frame bit and authoritative speed; bot inputs unchanged
      (they never set the bit). Offline measurement: 4.498 m walk vs 5.598 m
      sprint in one second, median of three runs.

## 6. Local prediction

- [ ] Wire `PredictionGlue` into the online local camera instead of only
      following the newest snapshot.
- [ ] Apply the 5 cm correction threshold with the existing visual residual.
- [ ] Soak with latency and packet loss; no rubber-banding or desync.
- [ ] Keep the offline camera path snapshot-based unless prediction is proven
      better there too.

## Order

1. Match feedback and audio.
2. Pause shell.
3. Block feel: outline, hotbar, hand, break feedback.
4. World art.
5. Sprint.
6. Local prediction.

## Execution log

- 2026-10-23: backlog created.
- 2026-10-23: audio event mapping, buddy spawn cue, last-cue test, airdrop
  marker assertion, pause shell, offline pause, settings return and near-camera
  culling are done. The world-marker edge case and the rest of block feel remain
  open.
- 2026-10-23: thin dark outline, desktop hotbar, first-person pickaxe/crate and
  break/place swing + particles are done. The hand first rendered inside the
  camera graph but was clipped by the world pass, so it now draws in a separate
  scene with `clearDepth()`; `screens.spec` asserts the selected hand slot and
  `pointerLock.spec` asserts place/break swings and the particle burst. Cracks,
  crate return and per-material sounds remain.
- 2026-10-23: off-screen airdrop telegraph. The label clamps to an 80 px side,
  200 px top and 140 px bottom safe frame so it clears the HUD, RECIPES and
  hotbar; behind-camera projections are flipped instead of hidden. `juice.spec`
  asserts the on-screen and off-screen boxes and the `data-edge` style.
- 2026-10-23: sky pass. A camera-centred sky dome mixes a horizon and zenith
  colour, adds a sun glow and draws 18 deterministic instanced cloud banks at
  y 78-86. The fog colour matches the horizon. `sky.spec` asserts three cloud
  pixels are white and that the sky pales from the top toward the horizon.
  Trees and water remain open.
- 2026-10-23: sprint. Bit 5 through the input frame, 5.6 m/s in the sim only
  while moving forward on land, Shift/RUN controls and a reduced-motion-gated
  70-to-76 FOV kick. Four new sim tests; `input.spec` and `touch.spec` assert
  the wire bit. Measured 4.498 m walk vs 5.598 m sprint per second in the
  offline worker (n=3 each).
- 2026-10-23: hold-to-break cracks. A five-stage canvas decal follows the hit
  face, updates every frame and completes after 400 ms; `pointerLock.spec`
  checks the decal mid-hold, the no-swing-before-completion guard and the burst
  after release. The ghost is hidden while mining. Crate return was closed as
  not applicable to the creative hotbar; material sounds need a block-at bridge
  query.
