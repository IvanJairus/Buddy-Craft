# Architecture

## One rule set, compiled twice

The rules are written once in Rust. `crates/sim` compiles to native for the
server and to `wasm32-unknown-unknown` for prediction and offline play. There is
no second movement, combat, recipe or match implementation in TypeScript.

Determinism is enforced, not hoped for:

- Simulation state is fixed point Q16.16. No `f32` or `f64` appears in `sim`
  outside the math table builder; `no_float` tests fail the build on a violation.
- No I/O, threads, wall clock, OS entropy or `HashMap` iteration in a state
  path. Time is a `Tick`, RNG streams are explicit, and iteration is `Vec` or
  `BTreeMap`.
- `MatchState::state_hash()` is the canonical observable state. Golden fixtures
  compare the same input log on native and wasm.
- Floats are allowed only for camera, audio and display interpolation. A float
  that feeds back into state is a desync bug.

## The wasm boundary

The browser loads the raw wasm module, not wasm-bindgen glue. Every export takes
and returns a `u32`; entity data is written into one `RenderDesc` allocation and
read through typed-array views. The current 20 exports are listed in
`tools/wasm-exports.mjs`.

Rules at the boundary:

1. All hot buffers are allocated at boot. A frame path allocates nothing.
2. `memory.grow` detaches every view. The loader rebuilds views after every
   allocation or mesh handoff.
3. Per-entity data is a flat fixed-stride record. No `JsValue` crosses per frame.
4. Chunk meshes are copied out before their staging buffer is reused.

## Authority and transport

The client never sends a player position or a damage value. It sends quantised
input, edit intent, chat, ready and ping. The server computes movement, reach,
damage and outcomes from its own state.

`room` is runtime-agnostic. `Room` owns lifecycle, input queues and movement;
`room::play::Match` owns airdrops, edits, combat, crafting, buddies and
elimination. `host-axum` drives a `tokio` task per room; `host-worker` is the
Cloudflare adapter. Offline puts the same `Match` in a Web Worker and swaps only
the transport, so C2S/S2C bytes are identical in both paths.

## Data flow

```
input bytes -> Room/Match -> SnapshotEncoder -> S2C bytes -> NetClient
                                                   -> SnapshotDecoder
                                                   -> Interpolator
                                                   -> EntityRenderer
chunk diffs -> ChunkSync -> wasm column cache -> ChunkManager remesh
```

The local player is predicted in Rust in the online path. Remote entities are
interpolated 100 ms in the past. Offline currently renders the local player from
the same interpolated snapshot stream.

## Failure boundaries

- Protocol decode rejects bad frames before parsing payloads and has a 4 KB cap.
- Chunk diffs are versioned and byte-budgeted at 1.5 KB.
- Room tasks are single writers; clients communicate by bounded channels.
- `ROOMS_ENABLED`, `PROTOCOL_MIN_VERSION` and `CLIENT_BUILD_OVERRIDE` are the
  runtime kill switches.

See `PROTOCOL.md` for the generated wire index, `RUNBOOK.md` for operations and
`A11Y.md` for accessibility scope.
