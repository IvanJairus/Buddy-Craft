/**
 * T-056 client telemetry specs: structured crash payloads and the endpoint
 * transport. The host-side JSON and kill switches are tested in host-axum.
 */

import { expect, test, type Page } from "@playwright/test";
import { CrashReporter, buildCrashReport } from "../src/telemetry";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    telemetryReport(message: string): {
      kind: string;
      message: string;
      build: string;
      atMs: number;
    };
    telemetryLast(): { kind: string; message: string } | null;
  };
}

test("crash reports keep kind, message, stack and build", () => {
  const report = buildCrashReport("error", new Error("boom"), 123, "abc");
  expect(report.kind).toBe("error");
  expect(report.message).toBe("boom");
  expect(report.stack).toContain("boom");
  expect(report.atMs).toBe(123);
  expect(report.build).toBe("abc");
  expect(buildCrashReport("unhandledrejection", "nope", 5, "x").stack).toBe("");
});

test("the reporter sends once to an endpoint and remembers the last report", () => {
  const sent: string[] = [];
  const reporter = new CrashReporter("https://telemetry.example/ingest", "test", (_, body) => {
    sent.push(body);
  });
  const report = reporter.capture("wasm", new Error("trap"));
  expect(report.kind).toBe("wasm");
  expect(sent).toHaveLength(1);
  expect(JSON.parse(sent[0]).message).toBe("trap");
  expect(reporter.lastReport).toEqual(report);
});

test("the page hook captures a wasm trap without network", async ({ page }: { page: Page }) => {
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  const report = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.telemetryReport("wasm trap"),
  );
  expect(report.kind).toBe("wasm");
  expect(report.message).toBe("wasm trap");
  const last = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.telemetryLast(),
  );
  expect(last?.message).toBe("wasm trap");
});
