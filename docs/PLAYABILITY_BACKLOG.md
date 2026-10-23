# Playability backlog

Living plan for the remaining game-feel work after the movement-latency pass
and the Settings Mouse panel. Update the checkboxes and the log as work lands.
Do not mark an item done without a test and a measured result.

Baseline, 2026-10-23:

- Local camera: 31.3 ms from W to motion in headed Opera at 1512x982 DPR 2.
- Render: 120.3 fps, 71 draw calls, 73k triangles in the same Opera run.
- Gates: 156 Playwright passed, 4 perf-gated skipped; 501 Rust tests.
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
- [ ] Confirm the HUD marker is readable when the drop is off screen; add an
      edge marker if the world marker is not enough.

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
- [ ] Replace the white 1 m wireframe with a thin dark block outline.
- [ ] Desktop hotbar: nine slots, selected state, number keys and wheel,
      clickable, hidden for coarse pointers because touch has its own hotbar.
- [ ] First-person hand and held block: simple model, idle bob, swing on use.
- [ ] Break progress: cracks that advance while holding the button.
- [ ] Break feedback: particles, a short sound, and a crate that returns to
      carry instead of disappearing.
- [ ] Material-specific break and place sounds.
- [ ] Keep the frame, draw-call and bundle budgets green.

## 4. World art

- [ ] Trees: 1x1 trunks, branches, uneven canopies, more than one silhouette.
- [ ] Animated water surface and a shoreline that reads as water.
- [ ] Sky gradient, clouds and a light direction; stronger distance fog.
- [ ] Keep nearest filtering and the atlas and model byte budgets.
- [ ] Re-run the browser playtest for readability in a forest.

## 5. Sprint

- [ ] Add a sprint bit to the input frame and server movement, target 5.6 m/s.
- [ ] Left Shift on desktop; a touch control for mobile.
- [ ] FOV kick while sprinting, respecting reduced motion.
- [ ] Unit tests for the frame bit and authoritative speed; bot inputs unchanged.

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
