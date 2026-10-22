/**
 * The match HUD (plan §8.7, task T-037).
 *
 * Eight components, one store subscription, one batch per 100 ms. Text is
 * written through `textContent`; the only inline style the numeric path sets is
 * a bar's `transform`, so a hit cannot trigger layout. Event toasts append a
 * node and remove it on the animation's end, with a five-node ceiling.
 */

import "./hud.css";
import type { HudState, Store } from "./store";

function make<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className !== undefined) {
    node.className = className;
  }
  return node;
}

function panel(label: string): { root: HTMLDivElement; value: HTMLDivElement } {
  const root = make("div", "hud-panel");
  const heading = make("div", "hud-label");
  heading.textContent = label;
  const value = make("div", "hud-value");
  root.append(heading, value);
  return { root, value };
}

function bar(hud: string): { track: HTMLDivElement; fill: HTMLDivElement } {
  const track = make("div", "hud-bar");
  const fill = make("div", "hud-bar-fill");
  fill.dataset.hud = hud;
  track.append(fill);
  return { track, fill };
}

function clock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

function clamp01(value: number): number {
  return Math.max(0, Math.min(1, value));
}

export interface HudStats {
  flushes: number;
  lastFrameMs: number;
  maxFrameMs: number;
}

export class Hud {
  private readonly root: HTMLDivElement;
  private readonly hpText: HTMLDivElement;
  private readonly hpBar: HTMLDivElement;
  private readonly cratesText: HTMLDivElement;
  private readonly matchTimer: HTMLDivElement;
  private readonly dropTimer: HTMLDivElement;
  private readonly stanceText: HTMLDivElement;
  private readonly buddiesText: HTMLDivElement;
  private readonly coreBar: HTMLDivElement;
  private readonly enemyCoreBar: HTMLDivElement;
  private readonly feed: HTMLDivElement;
  private readonly lastText = new Map<string, string>();
  private readonly lastBar = new Map<string, number>();
  private readonly unsubscribe: () => void;
  private flushes = 0;
  private lastFrameMs = 0;
  private maxFrameMs = 0;

  constructor(parent: HTMLElement, store: Store<HudState>) {
    this.root = make("div", "hud");
    this.root.setAttribute("aria-label", "Match HUD");
    const reticle = make("div", "hud-reticle");
    reticle.dataset.hud = "reticle";
    this.root.append(reticle);

    const top = make("div", "hud-top");
    const left = make("div", "hud-stack");
    const centre = make("div", "hud-stack hud-stack-center");
    const right = make("div", "hud-stack");

    const health = panel("Health");
    this.hpText = health.value;
    this.hpText.dataset.hud = "hp-text";
    const healthBar = bar("hp-bar");
    this.hpBar = healthBar.fill;
    health.root.append(healthBar.track);
    left.append(health.root);

    const crates = panel("Crates");
    this.cratesText = crates.value;
    this.cratesText.dataset.hud = "crates-text";
    left.append(crates.root);

    const clockPanel = make("div", "hud-panel hud-clock");
    this.matchTimer = make("div", "hud-timer");
    this.matchTimer.dataset.hud = "match-timer";
    this.dropTimer = make("div", "hud-sub");
    this.dropTimer.dataset.hud = "drop-timer";
    clockPanel.append(this.matchTimer, this.dropTimer);
    centre.append(clockPanel);

    const stance = panel("Stance");
    this.stanceText = stance.value;
    this.stanceText.dataset.hud = "stance";
    right.append(stance.root);

    const buddies = panel("Buddies");
    this.buddiesText = buddies.value;
    this.buddiesText.dataset.hud = "buddies";
    right.append(buddies.root);

    const cores = make("div", "hud-panel hud-cores");
    const friendlyLabel = make("div", "hud-label");
    friendlyLabel.textContent = "Your core";
    const friendlyBar = bar("core-bar");
    this.coreBar = friendlyBar.fill;
    const enemyLabel = make("div", "hud-label");
    enemyLabel.textContent = "Enemy core";
    const enemyBar = bar("enemy-core-bar");
    this.enemyCoreBar = enemyBar.fill;
    this.enemyCoreBar.classList.add("enemy");
    cores.append(friendlyLabel, friendlyBar.track, enemyLabel, enemyBar.track);
    right.append(cores);

    this.feed = make("div", "hud-feed");
    this.feed.setAttribute("aria-live", "polite");
    this.feed.dataset.hud = "feed";

    top.append(left, centre, right);
    this.root.append(top, this.feed);
    parent.append(this.root);

    this.unsubscribe = store.subscribe((state) => this.render(state));
    // The first paint builds every text node and is a boot cost, not a HUD
    // frame; measuring it in the same budget would charge layout to the wrong
    // place. Subsequent store flushes are measured.
    this.render(store.get(), false);
  }

  stats(): HudStats {
    return {
      flushes: this.flushes,
      lastFrameMs: this.lastFrameMs,
      maxFrameMs: this.maxFrameMs,
    };
  }

  /** Current feed lines, oldest first. Used by the E2E specs. */
  feedText(): string[] {
    return Array.from(this.feed.children, (node) => node.textContent ?? "");
  }

  /** Append an event line. Plain text only; the feed is player-facing. */
  toast(text: string): void {
    const node = make("div", "hud-toast");
    node.textContent = text;
    this.feed.append(node);
    while (this.feed.children.length > 5) {
      this.feed.firstElementChild?.remove();
    }
    node.addEventListener("animationend", () => node.remove(), { once: true });
    window.setTimeout(() => node.remove(), 3000);
  }

  dispose(): void {
    this.unsubscribe();
    this.root.remove();
  }

  private render(state: Readonly<HudState>, record = true): void {
    const start = performance.now();
    this.writeText(this.hpText, "hp", `${Math.round(state.hp)} / ${Math.round(state.maxHp)}`);
    this.writeBar(this.hpBar, "hp", state.hp, state.maxHp);
    this.writeText(
      this.cratesText,
      "crates",
      `${Math.round(state.crates)} / ${Math.round(state.carryCap)}`,
    );
    this.writeText(this.matchTimer, "timer", clock(state.matchSeconds));
    this.writeText(this.dropTimer, "drop", `Next drop ${clock(state.dropSeconds)}`);
    this.writeText(this.stanceText, "stance", state.stance.toUpperCase());
    this.writeText(
      this.buddiesText,
      "buddies",
      `${Math.round(state.buddies)} / ${Math.round(state.buddyCap)}`,
    );
    this.writeBar(this.coreBar, "core", state.coreHp, state.coreMax);
    this.writeBar(this.enemyCoreBar, "enemy", state.enemyCoreHp, state.enemyCoreMax);

    if (!record) {
      return;
    }
    const end = performance.now();
    this.lastFrameMs = end - start;
    this.maxFrameMs = Math.max(this.maxFrameMs, this.lastFrameMs);
    this.flushes += 1;
    performance.measure("hud", { start, end });
  }

  private writeText(node: HTMLElement, key: string, value: string): void {
    if (this.lastText.get(key) === value) {
      return;
    }
    this.lastText.set(key, value);
    node.textContent = value;
  }

  private writeBar(node: HTMLElement, key: string, value: number, max: number): void {
    const fraction = Math.round(clamp01(max > 0 ? value / max : 0) * 1000) / 1000;
    if (this.lastBar.get(key) === fraction) {
      return;
    }
    this.lastBar.set(key, fraction);
    node.style.transform = `scaleX(${fraction})`;
  }
}
