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
    input(): { frame: number[] };
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

  await page.locator(".match-leave").click();
  expect(await screen(page)).toBe("results");
  await page.locator('[data-screen-action="rematch"]').click();
  expect(await screen(page)).toBe("match");
  await page.locator(".match-leave").click();
  await page.locator('[data-screen-action="results-main"]').click();
  expect(await screen(page)).toBe("main");

  await openSettings(page);
  expect(await screen(page)).toBe("settings");
  await page.locator('[data-screen-action="settings-back"]').click();
  expect(await screen(page)).toBe("main");

  await page.locator('[data-screen-action="offline"]').click();
  expect(await screen(page)).toBe("match");
  await expect(page.locator(".match-leave")).toBeVisible();
});

test("settings persist and the remap reaches the input frame", async ({ page }) => {
  // No `quality` query: the stored tier must be what a reload sees.
  await boot(page, "/?scene=clear");
  await page.locator('[data-screen-action="continue"]').click();
  await openSettings(page);

  await page.locator('[data-setting="quality"]').selectOption("LOW");
  await page.locator('[data-setting="sensitivity"]').evaluate((element) => {
    const input = element as HTMLInputElement;
    input.value = "0.5";
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await page.locator('[data-setting="invert"]').check();
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
  expect(await page.locator('[data-setting="invert"]').isChecked()).toBe(true);
  expect(await page.locator('[data-setting="ui-scale"]').inputValue()).toBe("120");
  expect(await page.locator('[data-setting="reduced-motion"]').isChecked()).toBe(true);
  expect(await page.locator('[data-remap="forward"]').textContent()).toBe("I");
});
