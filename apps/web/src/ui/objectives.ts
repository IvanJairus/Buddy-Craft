/**
 * Always-on objective strip and combat target feedback. These are DOM readouts
 * over the world; they never own simulation state.
 */

import "./map.css";
import type { Bearing } from "../game/objectives";

export interface ObjectiveRowData {
  key: "pad" | "enemy" | "drop";
  label: string;
  bearing: Bearing;
}

interface RowNodes {
  root: HTMLDivElement;
  arrow: HTMLSpanElement;
  distance: HTMLSpanElement;
}

export class ObjectiveHud {
  readonly element: HTMLDivElement;
  private readonly rows = new Map<string, RowNodes>();
  private readonly hint: HTMLDivElement;
  private current: ObjectiveRowData[] = [];

  constructor(parent: HTMLElement) {
    this.element = document.createElement("div");
    this.element.className = "cb-objectives";
    this.element.setAttribute("aria-label", "Objectives");
    for (const [key, tone] of [
      ["pad", "pad"],
      ["enemy", "enemy"],
      ["drop", "drop"],
    ] as const) {
      const row = document.createElement("div");
      row.className = "cb-objective-row";
      row.dataset.key = key;
      row.dataset.tone = tone;
      const arrow = document.createElement("span");
      arrow.className = "cb-objective-arrow";
      arrow.textContent = "▲";
      const label = document.createElement("span");
      label.className = "cb-objective-label";
      const distance = document.createElement("span");
      distance.className = "cb-objective-distance";
      row.append(arrow, label, distance);
      row.hidden = true;
      this.element.append(row);
      this.rows.set(key, { root: row, arrow, distance });
    }
    this.hint = document.createElement("div");
    this.hint.className = "cb-objective-hint";
    this.hint.textContent = "No crates · follow the DROP marker\nPick up crates, then place them\nM map";
    this.element.append(this.hint);
    parent.append(this.element);
  }

  setHint(text: string): void {
    if (this.hint.textContent !== text) this.hint.textContent = text;
  }

  update(rows: ObjectiveRowData[]): void {
    this.current = rows.map((row) => ({ ...row }));
    const seen = new Set<string>();
    for (const data of rows) {
      const nodes = this.rows.get(data.key);
      if (nodes === undefined) continue;
      seen.add(data.key);
      nodes.root.hidden = false;
      const label = nodes.root.querySelector<HTMLSpanElement>(".cb-objective-label");
      if (label !== null) label.textContent = data.label;
      nodes.distance.textContent = `${Math.round(data.bearing.distance)}m`;
      nodes.arrow.style.transform = `rotate(${data.bearing.angle}rad)`;
    }
    for (const [key, nodes] of this.rows) {
      if (!seen.has(key)) nodes.root.hidden = true;
    }
  }

  hide(): void {
    this.current = [];
    for (const nodes of this.rows.values()) nodes.root.hidden = true;
  }

  info(): Array<{ key: string; label: string; distance: number; angle: number }> {
    return this.current.map((row) => ({
      key: row.key,
      label: row.label,
      distance: row.bearing.distance,
      angle: row.bearing.angle,
    }));
  }
}

export interface TargetInfo {
  visible: boolean;
  hp: number;
  hits: number;
}

export class CombatOverlay {
  readonly element: HTMLDivElement;
  private readonly chip: HTMLDivElement;
  private readonly chipLabel: HTMLSpanElement;
  private readonly chipFill: HTMLElement;
  private readonly hitMarker: HTMLDivElement;
  private hits = 0;
  private visible = false;
  private hp = 0;
  private hideAt = 0;

  constructor(parent: HTMLElement) {
    this.chip = document.createElement("div");
    this.chip.className = "cb-target";
    this.chip.dataset.visible = "false";
    this.chipLabel = document.createElement("span");
    this.chipLabel.textContent = "ENEMY";
    const bar = document.createElement("div");
    bar.className = "cb-target-bar";
    this.chipFill = document.createElement("i");
    bar.append(this.chipFill);
    const hint = document.createElement("span");
    hint.className = "cb-target-hint";
    hint.textContent = "LMB ATTACK";
    this.chip.append(this.chipLabel, bar, hint);

    this.hitMarker = document.createElement("div");
    this.hitMarker.className = "cb-hit-marker";
    this.hitMarker.dataset.hit = "false";

    this.element = this.chip;
    parent.append(this.chip, this.hitMarker);
  }

  setTarget(
    hp: number | null,
    maxHp = 100,
    now = performance.now(),
    label = "ENEMY",
  ): void {
    if (hp === null) {
      this.hideAt = now + 120;
      return;
    }
    this.visible = true;
    this.hp = hp;
    this.hideAt = 0;
    this.chip.dataset.visible = "true";
    this.chipLabel.textContent = `${label} ${hp}`;
    this.chipFill.style.width = `${Math.max(0, Math.min(100, (hp / maxHp) * 100))}%`;
  }

  tick(now: number): void {
    if (this.hideAt !== 0 && now >= this.hideAt) {
      this.hideAt = 0;
      this.visible = false;
      this.chip.dataset.visible = "false";
    }
  }

  hit(): void {
    this.hits += 1;
    this.hitMarker.dataset.hit = "false";
    // Force a reflow so the CSS animation restarts on rapid hits.
    void this.hitMarker.offsetWidth;
    this.hitMarker.dataset.hit = "true";
  }

  info(): TargetInfo {
    return { visible: this.visible, hp: this.hp, hits: this.hits };
  }
}
