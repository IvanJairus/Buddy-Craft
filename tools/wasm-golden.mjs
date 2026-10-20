// Run a `wasm32-wasip1` test binary under Node's WASI implementation.
//
// Plan T-015 asks for native and wasm to produce identical hash sequences. The
// golden test already does the comparison; this script is what lets CI run that
// same test *as wasm* without adding a native wasm runtime dependency —
// `node:wasi` ships with Node, which every GitHub runner already has.
//
// Usage:
//   node tools/wasm-golden.mjs target/wasm32-wasip1/release/deps/golden-*.wasm
//
// The preopen maps `/` to `/` so the absolute `CARGO_MANIFEST_DIR` path baked
// into the test at compile time still resolves inside the sandbox.

import { WASI } from "node:wasi";
import { readFile } from "node:fs/promises";
import { basename } from "node:path";

const file = process.argv[2];
if (!file) {
  console.error("usage: node tools/wasm-golden.mjs <test-binary.wasm> [test args...]");
  process.exit(2);
}

const wasi = new WASI({
  version: "preview1",
  args: [basename(file), ...process.argv.slice(3)],
  env: process.env,
  preopens: { "/": "/" },
  returnOnExit: true,
});

const module = await WebAssembly.compile(await readFile(file));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
const code = wasi.start(instance);

// The Rust test harness exits non-zero on failure, so this propagates.
process.exit(code);
