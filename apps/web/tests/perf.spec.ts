/**
 * T-055 performance gates. These are skipped in the normal suite and run with
 * `PERF_GATE=1` from `.github/workflows/perf.yml`.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";

const budget = JSON.parse(
  readFileSync(join(process.cwd(), "../../perf.budget.json"), "utf8"),
) as {
  frame_p95_ms: number;
  juice_ms: number;
  draw_calls: number;
  triangles: number;
  wasm_tick_p99_ms: number;
  first_playable_ms: number;
};

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    info(): { drawCalls: number; triangles: number };
    stream(x: number, z: number, budget?: number): unknown;
    juiceBenchmark(frames?: number): { averageMs: number };
    wasmTickBenchmark(frames?: number): number;
  };
}

test("frame p95 and draw budgets over 60 seconds", async ({ page }: { page: Page }) => {
  test.skip(process.env.PERF_GATE !== "1", "run with PERF_GATE=1");
  test.setTimeout(90_000);
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.stream(128, 128, 1024));
  const result = await page.evaluate(async () => {
    const frames: number[] = [];
    const started = performance.now();
    let last = started;
    await new Promise<void>((resolve) => {
      const step = (now: number): void => {
        frames.push(now - last);
        last = now;
        if (now - started < 60_000) requestAnimationFrame(step);
        else resolve();
      };
      requestAnimationFrame(step);
    });
    frames.sort((a, b) => a - b);
    const info = (window as unknown as TestWindow).__cbTest!.info();
    return {
      p95: frames[Math.floor(frames.length * 0.95)],
      info,
    };
  });
  expect(result.p95).toBeLessThanOrEqual(budget.frame_p95_ms);
  expect(result.info.drawCalls).toBeLessThanOrEqual(budget.draw_calls);
  expect(result.info.triangles).toBeLessThanOrEqual(budget.triangles);
});

test("juice stays inside its budget", async ({ page }: { page: Page }) => {
  test.skip(process.env.PERF_GATE !== "1", "run with PERF_GATE=1");
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  const result = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.juiceBenchmark(60),
  );
  expect(result.averageMs).toBeLessThanOrEqual(budget.juice_ms);
});

test("wasm tick p99 stays under three milliseconds", async ({ page }: { page: Page }) => {
  test.skip(process.env.PERF_GATE !== "1", "run with PERF_GATE=1");
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  const p99 = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.wasmTickBenchmark(600),
  );
  expect(p99).toBeLessThanOrEqual(budget.wasm_tick_p99_ms);
});

test("first playable at 8 Mbps and 150 ms RTT", async ({ page, context }: { page: Page; context: import("@playwright/test").BrowserContext }) => {
  test.skip(process.env.PERF_GATE !== "1", "run with PERF_GATE=1");
  test.setTimeout(30_000);
  const cdp = await context.newCDPSession(page);
  await cdp.send("Network.enable");
  await cdp.send("Network.emulateNetworkConditions", {
    offline: false,
    latency: 150,
    downloadThroughput: 1_000_000,
    uploadThroughput: 250_000,
  });
  await cdp.send("Network.setCacheDisabled", { cacheDisabled: true });
  const started = Date.now();
  // `quality` skips the two-second boot benchmark, which is a deliberate
  // first-run cost and not part of the first-paint measurement.
  await page.goto("/?quality=low");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 25_000,
  });
  const elapsed = Date.now() - started;
  expect(elapsed).toBeLessThanOrEqual(budget.first_playable_ms);
});
