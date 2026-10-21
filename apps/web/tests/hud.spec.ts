/**
 * T-037 acceptance specs: the store's 10 Hz sampler, DOM mutation counts, the
 * transform-only bars, the toast ceiling and the HUD frame cost.
 *
 * The page test drives 20 Hz writes for one second and counts mutations with a
 * MutationObserver on the HP text and bar. The HUD cost is the measure the HUD
 * itself records around each flush.
 */

import { expect, test, type Page } from "@playwright/test";
import { Store } from "../src/ui/store";

interface HudStats {
  flushes: number;
  lastFrameMs: number;
  maxFrameMs: number;
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    hudUpdate(patch: Record<string, number | string>): void;
    hudState(): Record<string, number | string>;
    hudStats(): HudStats;
    hudToast(text: string): void;
  };
}

async function boot(page: Page): Promise<void> {
  await page.goto("/?quality=high&scene=clear&screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
}

test("the store samples 20 Hz writes at 10 Hz", async () => {
  const store = new Store({ value: 0 });
  let flushes = 0;
  const unsubscribe = store.subscribe(() => {
    flushes += 1;
  });
  for (let i = 0; i < 20; i += 1) {
    store.set({ value: i });
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  await new Promise((resolve) => setTimeout(resolve, 150));
  expect(flushes).toBeLessThanOrEqual(10);
  expect(flushes).toBeGreaterThanOrEqual(8);
  unsubscribe();
  store.dispose();
});

test("the HUD coalesces writes, keeps bars on transform and stays under 1 ms", async ({ page }) => {
  await boot(page);
  const result = await page.evaluate(async () => {
    const api = (window as unknown as TestWindow).__cbTest!;
    const hpText = document.querySelector('[data-hud="hp-text"]')!;
    const hpBar = document.querySelector('[data-hud="hp-bar"]') as HTMLElement;
    const cratesText = document.querySelector('[data-hud="crates-text"]')!;
    let hpMutations = 0;
    let cratesMutations = 0;
    let barMutations = 0;
    let widthSeen = false;

    const hpObserver = new MutationObserver((records) => {
      hpMutations += records.length;
    });
    hpObserver.observe(hpText, { childList: true, characterData: true, subtree: true });
    const cratesObserver = new MutationObserver((records) => {
      cratesMutations += records.length;
    });
    cratesObserver.observe(cratesText, { childList: true, characterData: true, subtree: true });
    const barObserver = new MutationObserver((records) => {
      barMutations += records.length;
      const style = hpBar.getAttribute("style") ?? "";
      if (style.includes("width")) {
        widthSeen = true;
      }
    });
    barObserver.observe(hpBar, { attributes: true, attributeFilter: ["style"] });

    const before = api.hudStats();
    for (let i = 0; i < 20; i += 1) {
      api.hudUpdate({ hp: 100 - ((i * 3) % 100), crates: i % 12 });
      await new Promise((resolve) => setTimeout(resolve, 50));
    }

    hpObserver.disconnect();
    cratesObserver.disconnect();
    barObserver.disconnect();
    const after = api.hudStats();
    return {
      hpMutations,
      cratesMutations,
      barMutations,
      widthSeen,
      inlineWidth: hpBar.style.width,
      inlineStyle: hpBar.getAttribute("style") ?? "",
      stats: after,
      flushDelta: after.flushes - before.flushes,
      measures: performance.getEntriesByName("hud").length,
      state: api.hudState(),
    };
  });

  expect(result.hpMutations).toBeLessThanOrEqual(10);
  expect(result.cratesMutations).toBeLessThanOrEqual(10);
  expect(result.barMutations).toBeLessThanOrEqual(10);
  expect(result.hpMutations).toBeGreaterThanOrEqual(8);
  expect(result.flushDelta).toBeLessThanOrEqual(10);
  expect(result.widthSeen).toBe(false);
  expect(result.inlineWidth).toBe("");
  expect(result.inlineStyle).not.toContain("width");
  expect(result.inlineStyle).toContain("scaleX");
  expect(result.stats.maxFrameMs).toBeLessThanOrEqual(1);
  expect(result.stats.lastFrameMs).toBeLessThanOrEqual(1);
  expect(result.measures).toBeGreaterThan(0);
  expect(result.state.hp).toBeDefined();
});

test("the event feed caps at five nodes and never parses the text", async ({ page }) => {
  await boot(page);
  await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    for (let i = 0; i < 6; i += 1) {
      api.hudToast(`<img src=x onerror="alert(1)"> ${i}`);
    }
  });
  const feed = await page.evaluate(() => {
    const root = document.querySelector('[data-hud="feed"]')!;
    return {
      count: root.children.length,
      images: root.querySelectorAll("img").length,
      text: root.firstElementChild?.textContent ?? "",
    };
  });
  expect(feed.count).toBe(5);
  expect(feed.images).toBe(0);
  expect(feed.text).toContain("<img");
});
