/**
 * Per-match economy metrics (task T-060).
 *
 * The four §18 numbers are computed from the same snapshot events the HUD
 * consumes: crates per minute, recipes completed, buddies spawned and
 * time-to-first-buddy. The tracker owns no game state; it is a pure fold over
 * events, so the client and a future server report cannot disagree about what a
 * metric means.
 */

export const EVENT_CRAFT = 0;
export const EVENT_ELIMINATION = 3;
export const EVENT_PICKUP = 6;

export interface EconomySummary {
  /** Total crates picked up by every player. */
  cratesPicked: number;
  /** Crates per minute across the match. */
  cratesPerMinute: number;
  /** Crates per minute divided by the seat count. */
  cratesPerMinutePerPlayer: number;
  /** Recipes completed, including buddy recipes. */
  recipesCompleted: number;
  /** Buddy recipes completed. */
  buddiesSpawned: number;
  /** Tick of the first buddy craft, or null when none was made. */
  firstBuddyTick: number | null;
  /** `firstBuddyTick / 20`, or null. */
  timeToFirstBuddySeconds: number | null;
  /** Match time represented by the last observed event tick. */
  matchSeconds: number;
  playerCount: number;
}

export class EconomyTracker {
  private cratesPicked = 0;
  private recipesCompleted = 0;
  private buddiesSpawned = 0;
  private firstBuddyTick: number | null = null;
  private lastTick = 0;

  constructor(private readonly playerCount = 6) {}

  /** Fold one snapshot event. Unknown kinds are ignored. */
  observe(kind: number, _a: number, b: number, c: number, tick: number): void {
    this.lastTick = Math.max(this.lastTick, tick);
    if (kind === EVENT_PICKUP) {
      this.cratesPicked += Math.max(0, b);
      return;
    }
    if (kind === EVENT_CRAFT) {
      this.recipesCompleted += 1;
      if (c > 0) {
        this.buddiesSpawned += 1;
        this.firstBuddyTick =
          this.firstBuddyTick === null ? tick : Math.min(this.firstBuddyTick, tick);
      }
    }
  }

  get firstBuddySeconds(): number | null {
    return this.firstBuddyTick === null ? null : this.firstBuddyTick / 20;
  }

  summary(tick = this.lastTick): EconomySummary {
    const matchSeconds = Math.max(0, tick) / 20;
    const minutes = Math.max(matchSeconds / 60, 1 / 60);
    return {
      cratesPicked: this.cratesPicked,
      cratesPerMinute: this.cratesPicked / minutes,
      cratesPerMinutePerPlayer: this.cratesPicked / minutes / Math.max(1, this.playerCount),
      recipesCompleted: this.recipesCompleted,
      buddiesSpawned: this.buddiesSpawned,
      firstBuddyTick: this.firstBuddyTick,
      timeToFirstBuddySeconds: this.firstBuddySeconds,
      matchSeconds,
      playerCount: this.playerCount,
    };
  }
}

/** Send one summary to the configured telemetry endpoint, if any. */
export function reportEconomy(endpoint: string | null, summary: EconomySummary): void {
  if (endpoint === null || endpoint === "") return;
  const body = JSON.stringify({ kind: "economy", ...summary });
  if (typeof navigator !== "undefined" && typeof navigator.sendBeacon === "function") {
    const blob = new Blob([body], { type: "application/json" });
    if (navigator.sendBeacon(endpoint, blob)) return;
  }
  void fetch(endpoint, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
    keepalive: true,
  }).catch(() => {
    // Metrics must never break the match.
  });
}
