/**
 * Build the raw wasm bridge and copy it next to the loader.
 *
 * No wasm-bindgen CLI: the bridge's exports are numbers, so the browser can
 * instantiate the `.wasm` directly (see `loadClientWasm`). The target file is
 * generated and ignored by git; `npm run build` always refreshes it.
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const appRoot = join(here, "..");
const repoRoot = join(appRoot, "..", "..");
const target = process.env.WASM_TARGET ?? "wasm32-unknown-unknown";
// The client ships the release artifact: debug wasm is 9.3 MB and would blow
// the §8.10 budget before a single line of game code runs.
const profile = process.env.WASM_PROFILE ?? "release";

const args = ["build", "--target", target, "-p", "client-wasm"];
if (profile === "release") {
  args.push("--release");
}
execFileSync("cargo", args, { cwd: repoRoot, stdio: "inherit" });

const built = join(repoRoot, "target", target, profile, "client_wasm.wasm");
const destination = join(appRoot, "src", "wasm", "client_wasm.wasm");
mkdirSync(dirname(destination), { recursive: true });
copyFileSync(built, destination);
console.log(`wasm bridge: ${built} -> ${destination}`);
