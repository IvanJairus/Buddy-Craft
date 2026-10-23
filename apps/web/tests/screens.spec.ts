/**
 * T-039 acceptance specs: every screen is reachable, the lobby validates a
 * room code before anything connects, and settings survive a reload including
 * the key remap that the input frame actually uses.
 */

import { expect, test, type Page } from "@playwright/test";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    info(): { tier: string };
    input(): { frame: number[]; rawInput: boolean; slot: number };
    viewModel(): {
      active: boolean;
      selected: number;
      pickVisible: boolean;
      crateVisible: boolean;
      frames: number;
    };
  };
}

async function boot(page: Page, url = "/?quality=high&scene=clear"): Promise<void> {
  await page.goto(url);
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
}

async function screen(page: Page): Promise<string | undefined> {
  return page.evaluate(() => document.body.dataset.screen);
}

async function openSettings(page: Page): Promise<void> {
  await page.locator('[data-screen-action="settings"]').click();
  await expect(page.locator('[data-screen-action="settings-back"]')).toBeVisible();
}

test("every_screen_is_reachable", async ({ page }) => {
  await boot(page);
  expect(await screen(page)).toBe("boot");
  await page.locator('[data-screen-action="continue"]').click();
  expect(await screen(page)).toBe("main");

  await page.locator('[data-screen-action="online"]').click();
  expect(await screen(page)).toBe("lobby");
  await page.locator('[data-screen-action="create-room"]').click();
  const code = await page.locator('[data-lobby="code"]').textContent();
  expect(code).toMatch(/^[A-Z0-9]{6}$/);
  expect(await page.locator('[data-screen-action="copy-code"]').isEnabled()).toBe(true);

  await page.locator('[data-lobby="join"]').fill("abc");
  await page.locator('[data-screen-action="join-room"]').click();
  expect(await page.locator('[data-lobby="error"]').textContent()).toContain("6-character");

  await page.locator('[data-screen-action="start-room"]').click();
  expect(await screen(page)).toBe("match");
  expect(await page.locator("#game").isVisible()).toBe(true);
  await expect(page.locator(".hotbar")).toBeVisible();
  await expect(page.locator(".hotbar-slot")).toHaveCount(9);
  await page.locator('[data-slot="4"]').click();
  expect(await page.locator('[data-slot="4"]').getAttribute("aria-pressed")).toBe("true");
  expect(await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.input().slot)).toBe(4);
  const hand = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.viewModel());
  expect(hand.active).toBe(true);
  expect(hand.selected).toBe(4);
  expect(hand.crateVisible).toBe(true);
  expect(hand.pickVisible).toBe(false);
  expect(hand.frames).toBeGreaterThan(0);
  await page.keyboard.press("2");
  expect(await page.locator('[data-slot="1"]').getAttribute("aria-pressed")).toBe("true");
  expect(
    await page.evaluate(
      () => (window as unknown as TestWindow).__cbTest!.viewModel().crateVisible,
    ),
  ).toBe(true);

  await page.locator(".match-menu").click();
  expect(await screen(page)).toBe("pause");
  await expect(page.locator(".hotbar")).toBeHidden();
  await page.locator('[data-screen-action="pause-leave"]').click();
  expect(await screen(page)).toBe("results");
  expect(
    await page.evaluate(
      () => (window as unknown as TestWindow).__cbTest!.viewModel().active,
    ),
  ).toBe(false);
  await page.locator('[data-screen-action="rematch"]').click();
  expect(await screen(page)).toBe("match");
  await page.locator(".match-menu").click();
  await page.locator('[data-screen-action="pause-leave"]').click();
  await page.locator('[data-screen-action="results-main"]').click();
  expect(await screen(page)).toBe("main");

  await openSettings(page);
  expect(await screen(page)).toBe("settings");
  await page.locator('[data-screen-action="settings-back"]').click();
  expect(await screen(page)).toBe("main");

  await page.locator('[data-screen-action="offline"]').click();
  expect(await screen(page)).toBe("match");
  await expect(page.locator(".match-menu")).toBeVisible();
  await page.locator(".match-menu").click();
  await page.locator('[data-screen-action="pause-settings"]').click();
  expect(await screen(page)).toBe("settings");
  await page.locator('[data-screen-action="settings-back"]').click();
  expect(await screen(page)).toBe("pause");
  await page.locator('[data-screen-action="pause-resume"]').click();
  expect(await screen(page)).toBe("match");
});

test("pause freezes the offline match until resume", async ({ page }) => {
  await boot(page, "/?scene=clear");
  await page.locator('[data-screen-action="continue"]').click();
  await page.locator('[data-screen-action="offline"]').click();
  await page.waitForFunction(
    () =>
      (
        window as unknown as {
          __cbTest: { offlineStats(): { state: string; tick: number } };
        }
      ).__cbTest.offlineStats().state === "open",
    null,
    { timeout: 15_000 },
  );

  await page.keyboard.press("Escape");
  expect(await screen(page)).toBe("pause");
  await page.waitForTimeout(150);
  const paused = await page.evaluate(
    () =>
      (
        window as unknown as {
          __cbTest: { offlineStats(): { tick: number } };
        }
      ).__cbTest.offlineStats().tick,
  );
  await page.waitForTimeout(350);
  const stillPaused = await page.evaluate(
    () =>
      (
        window as unknown as {
          __cbTest: { offlineStats(): { tick: number } };
        }
      ).__cbTest.offlineStats().tick,
  );
  expect(Math.abs(stillPaused - paused)).toBeLessThanOrEqual(1);

  await page.locator('[data-screen-action="pause-resume"]').click();
  expect(await screen(page)).toBe("match");
  await page.waitForTimeout(350);
  const resumed = await page.evaluate(
    () =>
      (
        window as unknown as {
          __cbTest: { offlineStats(): { tick: number } };
        }
      ).__cbTest.offlineStats().tick,
  );
  expect(resumed).toBeGreaterThan(stillPaused);
});

test("settings persist and the remap reaches the input frame", async ({ page }) => {
  // No `quality` query: the stored tier must be what a reload sees.
  await boot(page, "/?scene=clear");
  await page.locator('[data-screen-action="continue"]').click();
  await openSettings(page);

  await page.locator('[data-setting="quality"]').selectOption("LOW");
  await page.locator('[data-sensitivity-preset="high"]').click();
  expect(await page.locator('[data-setting="sensitivity"]').inputValue()).toBe("0.5");
  expect(await page.locator('[data-setting="sensitivity"] + output').textContent()).toBe(
    "70.3°/100px",
  );
  await page.locator('[data-setting="dpi"]').evaluate((element) => {
    const input = element as HTMLInputElement;
    input.value = "1600";
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect(await page.locator('[data-setting="cm360"]').textContent()).toBe("0.8 cm/360");
  await page.locator('[data-setting="invert"]').check();
  await page.locator('[data-setting="raw-input"]').uncheck();
  await page.locator('[data-setting="ui-scale"]').evaluate((element) => {
    const input = element as HTMLInputElement;
    input.value = "120";
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await page.locator('[data-setting="reduced-motion"]').check();
  await page.locator('[data-remap="forward"]').click();
  await page.keyboard.press("i");
  expect(await page.locator('[data-remap="forward"]').textContent()).toBe("I");

  const stored = await page.evaluate(() => {
    const test = (window as unknown as TestWindow).__cbTest!;
    return {
      quality: localStorage.getItem("buddycraft.quality"),
      sensitivity: localStorage.getItem("buddycraft.sensitivity"),
      invert: localStorage.getItem("buddycraft.invertY"),
      rawInput: localStorage.getItem("buddycraft.rawInput"),
      dpi: localStorage.getItem("buddycraft.mouseDpi"),
      inputRaw: test.input().rawInput,
      scale: localStorage.getItem("buddycraft.uiScale"),
      motion: localStorage.getItem("buddycraft.reducedMotion"),
      keys: JSON.parse(localStorage.getItem("buddycraft.keys") ?? "{}") as Record<string, string>,
      tier: test.info().tier,
      reduced: document.body.classList.contains("cb-reduced-motion"),
      uiScale: getComputedStyle(document.documentElement).getPropertyValue("--cb-ui-scale").trim(),
    };
  });
  expect(stored.quality).toBe("LOW");
  expect(stored.sensitivity).toBe("0.5");
  expect(stored.invert).toBe("true");
  expect(stored.rawInput).toBe("false");
  expect(stored.dpi).toBe("1600");
  expect(stored.inputRaw).toBe(false);
  expect(stored.scale).toBe("120");
  expect(stored.motion).toBe("true");
  expect(stored.keys.forward).toBe("KeyI");
  expect(stored.tier).toBe("LOW");
  expect(stored.reduced).toBe(true);
  expect(stored.uiScale).toBe("1.2");

  await page.locator('[data-screen-action="settings-back"]').click();
  await page.locator('[data-screen-action="offline"]').click();
  expect(await screen(page)).toBe("match");
  await page.keyboard.down("i");
  await page.waitForTimeout(100);
  const frame = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.input().frame);
  expect(frame[3] >> 4).toBeGreaterThan(0);
  await page.keyboard.up("i");

  await page.reload();
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  expect(
    await page.evaluate(() => document.body.classList.contains("cb-reduced-motion")),
  ).toBe(true);
  await page.locator('[data-screen-action="continue"]').click();
  await openSettings(page);
  expect(await page.locator('[data-setting="quality"]').inputValue()).toBe("LOW");
  expect(await page.locator('[data-setting="sensitivity"]').inputValue()).toBe("0.5");
  expect(await page.locator('[data-setting="sensitivity"] + output').textContent()).toBe(
    "70.3°/100px",
  );
  expect(await page.locator('[data-setting="dpi"]').inputValue()).toBe("1600");
  expect(await page.locator('[data-setting="cm360"]').textContent()).toBe("0.8 cm/360");
  expect(await page.locator('[data-setting="invert"]').isChecked()).toBe(true);
  expect(await page.locator('[data-setting="raw-input"]').isChecked()).toBe(false);
  expect(await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.input().rawInput)).toBe(false);
  expect(await page.locator('[data-setting="ui-scale"]').inputValue()).toBe("120");
  expect(await page.locator('[data-setting="reduced-motion"]').isChecked()).toBe(true);
  expect(await page.locator('[data-remap="forward"]').textContent()).toBe("I");
});
