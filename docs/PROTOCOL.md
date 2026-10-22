# Protocol

Generated from `crates/protocol/src/lib.rs` by `tools/protocol-doc.mjs`.
Source digest: `1c0e3c7f2525`. Do not edit by hand; run the generator.

One WebSocket frame is one message: the first byte is the tag, the rest is a
postcard payload. Frames are capped at 4096 bytes.

## Constants

- `PROTOCOL_VERSION`: `1`
- `MAX_FRAME_BYTES`: `4096`
- `MAX_PLAYERS`: `6`
- `MAX_TEAMS`: `6`
- `MAX_BUDDIES`: `12`
- `MAX_PROJECTILES`: `24`
- `MAX_STRUCTURES`: `4`
- `MAX_EVENTS`: `16`
- `MAX_BASES`: `MAX_TEAMS`
- `MAX_DIFF_ENTRIES`: `500`

## Client to server

| Variant | Tag | Payload type |
| --- | --- | --- |
| Hello | `0x01` | `Hello` |
| Ready | `0x02` | `Ready` |
| TeamChoice | `0x03` | `TeamChoice` |
| Input | `0x04` | `Input` |
| Edit | `0x05` | `Edit` |
| CraftIntent | `0x06` | `CraftIntent` |
| Chat | `0x07` | `Chat` |
| Ping | `0x08` | `Ping` |
| Report | `0x09` | `Report` |

## Server to client

| Variant | Tag | Payload type |
| --- | --- | --- |
| Welcome | `0x81` | `Welcome` |
| Lobby | `0x82` | `Lobby` |
| Start | `0x83` | `Start` |
| Snapshot | `0x84` | `Snapshot` |
| EditAck | `0x85` | `EditAck` |
| EditBroadcast | `0x86` | `EditBroadcast` |
| ChunkDiff | `0x87` | `ChunkDiff` |
| Event | `0x88` | `Event` |
| End | `0x89` | `End` |
| Error | `0x8A` | `ErrorMsg` |
| Pong | `0x8B` | `Pong` |

## Payload types

- `BaseSpawn`
- `Chat`
- `ChunkDiff`
- `CraftIntent`
- `DiffEntry`
- `DiffRun`
- `Edit`
- `EditAck`
- `EditBroadcast`
- `End`
- `ErrorMsg`
- `Event`
- `FixedBytes`
- `Hello`
- `Input`
- `Lobby`
- `LobbyPlayer`
- `Ping`
- `PlayerStats`
- `Pong`
- `Ready`
- `Report`
- `RoomConfig`
- `RoomSettings`
- `Snapshot`
- `SnapshotBuddy`
- `SnapshotEvent`
- `SnapshotPlayer`
- `SnapshotProjectile`
- `SnapshotStructure`
- `Start`
- `TeamChoice`
- `Welcome`
- `WireInput`

The field-level authority is the Rust struct above each tag. Validation rules
and size budgets live in `crates/protocol/src/codec.rs` and in the protocol
tests; this page is an index, not a second implementation.
