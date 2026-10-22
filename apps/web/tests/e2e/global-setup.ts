/**
 * E2E global setup (task T-054).
 *
 * Starts one host-axum process for the whole suite. The host is the normal
 * binary with `CB_E2E=1`, which only adds the debug command endpoint and a
 * shared spawn point for two-client movement tests.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HOST_URL = "http://127.0.0.1:18080";
const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../../..");

async function healthy(): Promise<boolean> {
  try {
    const response = await fetch(`${HOST_URL}/healthz`, {
      signal: AbortSignal.timeout(1_000),
    });
    return response.ok;
  } catch {
    return false;
  }
}

async function waitHealthy(): Promise<void> {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    if (await healthy()) return;
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
  }
  throw new Error("host-axum did not become healthy on 127.0.0.1:18080");
}

export default async function globalSetup(): Promise<(() => Promise<void>) | void> {
  if (process.env.E2E !== "1") return;
  if (await healthy()) return;

  execFileSync("cargo", ["build", "-p", "host-axum"], {
    cwd: repoRoot,
    stdio: "inherit",
  });
  const binary = join(repoRoot, "target", "debug", "host-axum");
  const child: ChildProcess = spawn(binary, [], {
    cwd: repoRoot,
    stdio: "ignore",
    env: {
      ...process.env,
      CB_ADDR: "127.0.0.1:18080",
      CB_E2E: "1",
      CB_ALLOWED_ORIGINS: "http://127.0.0.1:4173",
      CB_SUMMARY_DIR: join(repoRoot, "target", "e2e-summaries"),
    },
  });
  await waitHealthy();
  return async () => {
    child.kill("SIGTERM");
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 250));
    if (!child.killed) child.kill("SIGKILL");
  };
}
