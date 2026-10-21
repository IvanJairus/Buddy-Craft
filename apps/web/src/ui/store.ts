/**
 * The HUD store and its 10 Hz sampler (plan §8.7, task T-037).
 *
 * The store is deliberately tiny. Components subscribe, writes mark a dirty
 * flag, and one 100 ms interval flushes the latest snapshot to every
 * subscriber. A value that changes at 20 Hz therefore reaches the DOM at most
 * ten times a second, and a hundred writes in one tick still cost one flush.
 *
 * The interval stops after a flush with no new writes. An idle HUD does not
 * keep a timer alive, and the next update starts it again.
 */

export type HudStance = "follow" | "hold" | "assault";

export interface HudState {
  hp: number;
  maxHp: number;
  crates: number;
  carryCap: number;
  matchSeconds: number;
  dropSeconds: number;
  stance: HudStance;
  buddies: number;
  buddyCap: number;
  coreHp: number;
  coreMax: number;
  enemyCoreHp: number;
  enemyCoreMax: number;
}

export function initialHudState(overrides: Partial<HudState> = {}): HudState {
  return {
    hp: 100,
    maxHp: 100,
    crates: 0,
    carryCap: 12,
    matchSeconds: 0,
    dropSeconds: 10,
    stance: "follow",
    buddies: 0,
    buddyCap: 5,
    coreHp: 10_000,
    coreMax: 10_000,
    enemyCoreHp: 10_000,
    enemyCoreMax: 10_000,
    ...overrides,
  };
}

export type StoreListener<T> = (state: Readonly<T>) => void;

export class Store<T extends object> {
  private readonly state: T;
  private readonly listeners = new Set<StoreListener<T>>();
  private timer: ReturnType<typeof setInterval> | null = null;
  private dirty = false;

  constructor(initial: T) {
    this.state = { ...initial };
  }

  get(): Readonly<T> {
    return this.state;
  }

  set(patch: Partial<T>): void {
    let changed = false;
    for (const key of Object.keys(patch) as Array<keyof T>) {
      const value = patch[key];
      if (value !== undefined && !Object.is(this.state[key], value)) {
        this.state[key] = value as T[keyof T];
        changed = true;
      }
    }
    if (!changed) {
      return;
    }
    this.dirty = true;
    if (this.timer === null) {
      this.timer = setInterval(() => this.tick(), 100);
    }
  }

  subscribe(listener: StoreListener<T>): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  dispose(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
    this.listeners.clear();
    this.dirty = false;
  }

  private tick(): void {
    if (!this.dirty) {
      if (this.timer !== null) {
        clearInterval(this.timer);
        this.timer = null;
      }
      return;
    }
    this.dirty = false;
    const snapshot = this.state;
    for (const listener of this.listeners) {
      listener(snapshot);
    }
  }
}
