# Runbook

## Signals

The host writes one JSON object per line to stdout. Every line has `ts_ms` and
`event`. The client posts crash reports to `/telemetry` when
`VITE_TELEMETRY_URL` points at an ingest endpoint.

| Event | Meaning | Alert |
| --- | --- | --- |
| `room_started` / `room_stopped` | Room task lifecycle | unexpected stop with no shutdown |
| `room_metrics` | 20-tick average and max tick time, average snapshot bytes, client count | avg tick > 12 ms for 30 s, snapshot > 2 KB |
| `ws_connected` / `ws_disconnected` | Connection lifecycle | disconnect rate > 8% of joins |
| `client` | Crash or wasm trap report | any spike; wasm trap is P0 |

The bot-swarm tool reports the same room cost without sockets:
`cargo run --release -p bot-swarm`.

## Alerts and one-line mitigations

| Alert | Likely cause | Mitigation |
| --- | --- | --- |
| Room tick p99 > 12 ms sustained | pathfinding or a pathological room | lower bot count for new rooms, restart the affected room |
| Snapshot bytes p95 > 2 KB | a client missed baselines and is getting keyframes | inspect disconnect/ack logs, ask the client to reload |
| Disconnect rate > 8% | bad carrier path or a deploy | roll back `CLIENT_BUILD_OVERRIDE`, check Origin and WSS |
| Desync report | client and host hashes diverged | keep the input log, replay it under `sim` before changing code |
| DO requests > 70% daily | room volume | set `ROOMS_ENABLED=false` for new rooms |
| Client frame p95 > 20 ms | device or asset regression | drop quality tier, compare against the perf gate |
| Crash or wasm trap spike | bad deploy or asset | pin the previous Vercel deployment, inspect `client` reports |

## Kill switches

- `ROOMS_ENABLED=false` rejects new upgrades with 503. Existing rooms finish.
- `PROTOCOL_MIN_VERSION=<n>` rejects an old `C_HELLO` protocol.
- `CLIENT_BUILD_OVERRIDE=<id>` records the pinned build in health and startup;
  the actual Vercel pin is a deployment action.

`GET /healthz` reports `rooms`, `uptime_s`, the three switch values and the
current room count. The default development address is `127.0.0.1:8080`.

## Escalation

1. Stop new rooms.
2. Preserve the structured log and any client input log.
3. Replay the input log in `room` tests.
4. Roll back the client or worker deployment.
5. Only then change a rule or threshold.
