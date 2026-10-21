/**
 * T-061 accessibility specs: keyboard menus, reduced-motion CSS and measured
 * HUD contrast. The in-match pointer-lock surface is intentionally excluded.
 */

import { expect, test, type Page } from "@playwright/test";

async function boot(page: Page, url = "/"): Promise<void> {
  await page.goto(url);
  await page.waitForFunction(() => (window as unknown as { __cbReady?: boolean }).__cbReady === true, null, {
    timeout: 30_000,
  });
}

function luminance(r: number, g: number, b: number): number {
  const channel = (value: number): number => {
    const c = value / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function contrast(a: [number, number, number], b: [number, number, number]): number {
  const l1 = luminance(...a);
  const l2 = luminance(...b);
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}

test("every visible menu button is reachable from the keyboard", async ({ page }: { page: Page }) => {
  await boot(page);
  await page.locator('[data-screen-action="continue"]').click();
  await page.locator('[data-screen-action="online"]').focus();
  await page.keyboard.press("Tab");
  const focused = await page.evaluate(() => {
    const element = document.activeElement as HTMLElement | null;
    return {
      tag: element?.tagName ?? "",
      text: element?.textContent ?? "",
    };
  });
  expect(focused.tag).toBe("BUTTON");
  expect(focused.text.trim().length).toBeGreaterThan(0);
  const unnamed = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLButtonElement>(".screen-root button")]
      .filter((button) => button.offsetParent !== null)
      .filter((button) => (button.textContent ?? "").trim().length === 0)
      .length,
  );
  expect(unnamed).toBe(0);
});

test("reduced motion removes transition time in CSS", async ({ page }: { page: Page }) => {
  await boot(page);
  await page.evaluate(() => document.body.classList.add("cb-reduced-motion"));
  const duration = await page.evaluate(() => {
    const panel = document.querySelector<HTMLElement>(".screen-panel");
    if (panel === null) return "";
    return getComputedStyle(panel).transitionDuration;
  });
  expect(Number.parseFloat(duration)).toBeLessThan(0.01);
});

test("HUD labels meet 4.5:1 against their panel", async ({ page }: { page: Page }) => {
  await boot(page, "/?screen=match");
  await page.waitForSelector('[data-hud="hp-text"]');
  const colors = await page.evaluate(() => {
    const label = document.querySelector<HTMLElement>(".hud-label");
    const panel = label?.closest<HTMLElement>(".hud-panel") ?? null;
    const parse = (value: string): [number, number, number, number] => {
      const match = value.match(/rgba?\(([^)]+)\)/);
      if (match === null) return [0, 0, 0, 1];
      const parts = match[1].split(",").map((part) => Number(part.trim()));
      return [parts[0], parts[1], parts[2], parts[3] ?? 1];
    };
    return {
      text: parse(getComputedStyle(label!).color),
      background: parse(getComputedStyle(panel!).backgroundColor),
    };
  });
  const [r, g, b, alpha] = colors.background;
  const composited: [number, number, number] = [r * alpha, g * alpha, b * alpha];
  const ratio = contrast([colors.text[0], colors.text[1], colors.text[2]], composited);
  expect(ratio).toBeGreaterThanOrEqual(4.5);
});
