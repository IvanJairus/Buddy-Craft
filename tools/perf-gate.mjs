#!/usr/bin/env node
/**
 * T-055 size gates. Thresholds live only in perf.budget.json.
 * Usage: node tools/perf-gate.mjs [dist-dir]
 */
import { readdirSync, readFileSync, statSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { brotliCompressSync, gzipSync, constants } from "node:zlib";

const dist = process.argv[2] ?? "apps/web/dist/assets";
const budget = JSON.parse(readFileSync("perf.budget.json", "utf8"));
const files = readdirSync(dist).map((name) => join(dist, name));
const wasm = files.find((file) => file.endsWith(".wasm"));
const js = files
  .filter((file) => file.endsWith(".js"))
  .sort((a, b) => statSync(b).size - statSync(a).size)[0];
if (!wasm || !js) {
  console.error(`perf-gate: no wasm/js asset under ${dist}`);
  process.exit(2);
}
const wasmBytes = readFileSync(wasm);
const jsBytes = readFileSync(js);
const wasmBrotli = brotliCompressSync(wasmBytes, {
  params: { [constants.BROTLI_PARAM_QUALITY]: 11 },
}).byteLength;
const jsGzip = gzipSync(jsBytes, { level: 9 }).byteLength;

function directoryBytes(path) {
  if (!existsSync(path)) return 0;
  let total = 0;
  for (const name of readdirSync(path)) {
    const full = join(path, name);
    total += statSync(full).isDirectory() ? directoryBytes(full) : statSync(full).size;
  }
  return total;
}

// Atlas and fonts are blocking; SFX live under public/audio and are deferred.
const blockingAssets = directoryBytes("apps/web/public/assets");
const rows = [
  ["wasm raw", wasmBytes.byteLength, budget.wasm_raw_bytes],
  ["wasm brotli", wasmBrotli, budget.wasm_brotli_bytes],
  ["js gzip", jsGzip, budget.js_gzip_bytes],
  ["blocking assets", blockingAssets, budget.blocking_assets_bytes],
];
let failed = false;
for (const [name, value, limit] of rows) {
  const ok = value <= limit;
  failed ||= !ok;
  console.log(`${ok ? "ok" : "FAIL"} ${name}: ${value} / ${limit}`);
}
const summary = {
  wasm_raw_bytes: wasmBytes.byteLength,
  wasm_brotli_bytes: wasmBrotli,
  js_gzip_bytes: jsGzip,
  blocking_assets_bytes: blockingAssets,
  failed,
};
writeFileSync("perf-summary.json", `${JSON.stringify(summary, null, 2)}\n`);
console.log(
  `perf-gate: wasm brotli ${(wasmBrotli / 1024).toFixed(1)} KiB, js gzip ${(jsGzip / 1024).toFixed(1)} KiB`,
);
process.exit(failed ? 1 : 0);
