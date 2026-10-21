# Bot swarm report

Measured by `cargo run --release -p bot-swarm` on the development machine.

| Metric | Value |
| --- | --- |
| Rooms | 20 |
| Bots per room | 6 |
| Simulated duration | 180 s (3600 ticks/room) |
| Measured ticks | 72000 |
| Tick p50 | 0.050 ms |
| Tick p99 | 0.455 ms |
| Tick max | 3.423 ms |
| Snapshot bytes p50 | 40 B |
| Snapshot bytes p95 | 67 B |
| Snapshot bytes max | 127 B |
| Process RSS | 27376 KB |
| CPU | 100.0% of one core |
| Wall time | 5.35 s |

The T-053 gate is `tick_p99_under_12ms`: 20 rooms times six bots, p99 <= 12 ms.
This run reports 0.455 ms p99. The 20 rooms are in one process and share one
core. At 100.0% of one core for 20 rooms, one core carries about 20 rooms,
or roughly 120 players; two Oracle cores therefore carry about 40 rooms,
inside the plan's 150-300 concurrent-player assumption. Socket framing is
measured separately by T-045.
