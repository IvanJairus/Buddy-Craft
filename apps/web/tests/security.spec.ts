/**
 * T-059 security specs: the meta CSP is active, a normal load produces no
 * violations, an injected inline script is blocked, and the source never uses
 * innerHTML.
 */

import { expect, test, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

async function boot(page: Page): Promise<string[]> {
  const violations: string[] = [];
  page.on("console", (message) => {
    const text = message.text();
    if (text.includes("Content Security Policy") || text.includes("Refused to")) {
      violations.push(text);
    }
  });
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as { __cbReady?: boolean }).__cbReady === true, null, {
    timeout: 30_000,
  });
  return violations;
}

function sourceFiles(path: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(path)) {
    const full = join(path, entry);
    if (statSync(full).isDirectory()) out.push(...sourceFiles(full));
    else if (full.endsWith(".ts")) out.push(full);
  }
  return out;
}

test("the shipped page has the CSP and no violations on load", async ({ page }: { page: Page }) => {
  const violations = await boot(page);
  expect(violations).toEqual([]);
  const policy = await page.evaluate(
    () =>
      document.querySelector<HTMLMetaElement>('meta[http-equiv="Content-Security-Policy"]')
        ?.content ?? "",
  );
  expect(policy).toContain("default-src 'self'");
  expect(policy).toContain("'wasm-unsafe-eval'");
  expect(policy).toContain("object-src 'none'");
  expect(policy).toContain("base-uri 'none'");
});

test("an inline script is blocked by the policy", async ({ page }: { page: Page }) => {
  await boot(page);
  const result = await page.evaluate(async () => {
    let fired = 0;
    document.addEventListener("securitypolicyviolation", () => {
      fired += 1;
    });
    const script = document.createElement("script");
    script.textContent = "window.__cbPwned = 1";
    document.head.append(script);
    await new Promise((resolve) => setTimeout(resolve, 50));
    return {
      pwned: (window as unknown as { __cbPwned?: number }).__cbPwned,
      fired,
    };
  });
  expect(result.pwned).toBeUndefined();
  expect(result.fired).toBeGreaterThan(0);
});

test("t12_secret_scan_rejects_a_planted_key", () => {
  const dir = mkdtempSync(join(tmpdir(), "cb-secret-"));
  const tool = join(process.cwd(), "../../tools/secret-scan.mjs");
  try {
    writeFileSync(join(dir, "bundle.js"), "const SESSION_SIGNING_KEY = 'leak';");
    expect(() =>
      execFileSync(process.execPath, [tool, dir], { stdio: "pipe" }),
    ).toThrow();
    writeFileSync(join(dir, "bundle.js"), "const answer = 42;");
    expect(() =>
      execFileSync(process.execPath, [tool, dir], { stdio: "pipe" }),
    ).not.toThrow();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("t11_no_innerhtml_or_executable_html_sinks", () => {
  const sources = sourceFiles("src");
  const offenders = sources.filter((file) => {
    const text = readFileSync(file, "utf8");
    return text.includes(".innerHTML") || text.includes("insertAdjacentHTML") || text.includes("outerHTML");
  });
  expect(offenders).toEqual([]);
});
