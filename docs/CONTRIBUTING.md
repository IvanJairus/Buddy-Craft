# Contributing

## Local loop

From a clean clone:

```bash
git clone https://github.com/IvanJairus/Buddy-Craft.git
cd Buddy-Craft/apps/web
npm ci
npm run dev
```

Open the printed Vite URL and choose OFFLINE for a bot-filled local match. For
the network host, run `cargo run -p host-axum` in another terminal.

## Checks before a commit

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --target wasm32-unknown-unknown -p client-wasm
node tools/wasm-exports.mjs target/wasm32-unknown-unknown/debug/client_wasm.wasm
cd apps/web && npm ci && npm run test:e2e
```

Performance gates are separate:

```bash
cargo test --release -p host-axum tick_interval_p99
cargo test --release -p bot-swarm
cd apps/web && PERF_GATE=1 npx playwright test tests/perf.spec.ts --project=headless
```

## Rules

- One commit per plan task. The README is the journal: record measured numbers,
  deviations and known gaps there, not in an untracked notes file.
- No floating point or wall clock in `sim`. If a rule needs time, pass a `Tick`.
- No client-authoritative position or damage. Add a server-side validation and a
  threat test named after its §11.2 ID.
- Do not weaken a budget, timeout or assertion to make a test pass. Fix code or
  document why the threshold was wrong.
- New UI controls need a visible action, an accessible name and a page test.
  Copy follows the project's plain-language rule: no internal task IDs in
  player-facing text.
- Protocol changes regenerate `docs/PROTOCOL.md` with
  `node tools/protocol-doc.mjs`; do not edit the generated page by hand.

## Where to start

`docs/PLAN.md` is the task backlog. `docs/ARCHITECTURE.md` states the boundary
rules. `docs/RUNBOOK.md` covers operations and kill switches.
