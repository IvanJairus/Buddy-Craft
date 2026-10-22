/**
 * §12.5 boot.spec: page loads, wasm initialises, first playable under 3 s.
 *
 * Chromium is also throttled to 8 Mbps / 150 ms before navigation. Firefox and
 * WebKit have no CDP throttler, so their assertion is the same time bound on a
 * normal connection; the throttled number is the one the plan names.
 */

import { expect, test } from "@playwright/test";
import { boot, screen } from "./helpers";

test("boots wasm and reaches the main menu under 3 s", async ({ page }, testInfo) => {
  if (testInfo.project.name === "chromium-e2e") {
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Network.enable");
    await cdp.send("Network.emulateNetworkConditions", {
      offline: false,
      latency: 150,
      downloadThroughput: 1_000_000,
      uploadThroughput: 250_000,
      connectionType: "cellular3g",
    });
    testInfo.annotations.push({
      type: "network",
      description: "8 Mbps down, 150 ms RTT (Chromium CDP)",
    });
  }
  const started = Date.now();
  await boot(page, "/?quality=low");
  const elapsed = Date.now() - started;
  expect(elapsed).toBeLessThan(3_000);
  expect(await screen(page)).toBe("boot");
  await page.getByRole("button", { name: "CONTINUE" }).click();
  expect(await screen(page)).toBe("main");
  await expect(page.getByRole("button", { name: "OFFLINE" })).toBeVisible();
});
