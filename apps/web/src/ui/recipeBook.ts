/**
 * Recipe book and near-miss banner (plan §4.5.5, task T-038).
 *
 * Every card is built from a `RecipeRow` decoded from the wasm recipe table,
 * which is the same table the matcher uses. The top-down grid is a projection
 * of that row's blocks; there is no second shape list in TypeScript.
 *
 * Locked recipes are shown, not hidden: the player needs to know what is
 * coming. Near-miss text is filled by the Rust `near_miss`, so the banner
 * cannot disagree with what the matcher will do.
 */

import "./recipeBook.css";
import type { RecipeRow } from "../wasm/loader";

const OUTPUT_LABELS = [
  "Sword",
  "Bow",
  "Bandage",
  "Spear",
  "Tower",
  "Ram",
  "Catapult",
  "Ward",
];
const BUDDY_LABELS = [
  "Swordsman",
  "Archer",
  "Mage",
  "Healer",
  "Berserker",
  "Sniper",
];

export function displayName(row: RecipeRow): string {
  return row.name.replace(/^R\d+_/, "").replace(/_/g, " ");
}

export function outputLabel(row: RecipeRow): string {
  if (row.output === 8) {
    return BUDDY_LABELS[row.buddy] ?? "Buddy";
  }
  return OUTPUT_LABELS[row.output] ?? "Unknown";
}

export function formatClock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

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

interface Card {
  row: RecipeRow;
  root: HTMLDivElement;
  lock: HTMLDivElement;
}

export interface RecipeBookInfo {
  open: boolean;
  cards: number;
  locked: number;
  unlocked: number;
  nearMiss: string | null;
}

export class RecipeBookUi {
  private readonly rows: RecipeRow[];
  private readonly root: HTMLDivElement;
  private readonly panel: HTMLDivElement;
  private readonly toggleButton: HTMLButtonElement;
  private readonly nearBanner: HTMLDivElement;
  private readonly cards: Card[] = [];
  private elapsed = 0;
  private openState = false;
  private nearText: string | null = null;

  constructor(parent: HTMLElement, rows: readonly RecipeRow[]) {
    this.rows = [...rows].sort((a, b) => a.id - b.id);
    this.root = make("div", "recipe-ui");

    this.toggleButton = make("button", "recipe-toggle");
    this.toggleButton.type = "button";
    this.toggleButton.textContent = "RECIPES";
    this.toggleButton.setAttribute("aria-controls", "recipe-book-panel");
    this.toggleButton.setAttribute("aria-expanded", "false");
    this.toggleButton.addEventListener("click", () => this.toggle(this.elapsed));

    this.panel = make("div", "recipe-book");
    this.panel.id = "recipe-book-panel";
    this.panel.setAttribute("aria-label", "Recipe book");
    const head = make("div", "recipe-head");
    const title = make("h2", "recipe-title");
    title.textContent = "Recipe book";
    const close = make("button", "recipe-close");
    close.type = "button";
    close.textContent = "CLOSE";
    close.addEventListener("click", () => this.close());
    head.append(title, close);

    const cards = make("div", "recipe-cards");
    for (const row of this.rows) {
      const card = make("div", "recipe-card");
      card.dataset.recipe = String(row.id);
      const name = make("div", "recipe-name");
      name.textContent = displayName(row);
      const meta = make("div", "recipe-meta");
      meta.textContent = `${row.cost} crates · ${outputLabel(row)}`;
      const grid = make("div", "recipe-grid");
      let maxX = 0;
      let maxZ = 0;
      for (const [x, , z] of row.blocks) {
        maxX = Math.max(maxX, x);
        maxZ = Math.max(maxZ, z);
      }
      grid.style.setProperty("--cols", String(maxX + 1));
      for (let z = 0; z <= maxZ; z += 1) {
        for (let x = 0; x <= maxX; x += 1) {
          const cell = make("span", "recipe-cell");
          const heights = row.blocks
            .filter((block) => block[0] === x && block[2] === z)
            .map((block) => block[1]);
          if (heights.length > 0) {
            cell.dataset.filled = "true";
            cell.dataset.height = String(Math.max(...heights) + 1);
          }
          grid.append(cell);
        }
      }
      const lock = make("div", "recipe-lock");
      card.append(name, grid, meta, lock);
      this.cards.push({ row, root: card, lock });
      cards.append(card);
    }

    this.panel.append(head, cards);
    this.nearBanner = make("div", "recipe-near");
    this.nearBanner.setAttribute("role", "status");

    this.root.append(this.toggleButton, this.panel, this.nearBanner);
    parent.append(this.root);
    this.render(this.elapsed);
  }

  toggle(elapsed = this.elapsed): void {
    if (this.openState) {
      this.close();
    } else {
      this.open(elapsed);
    }
  }

  open(elapsed = this.elapsed): void {
    this.openState = true;
    this.elapsed = Math.max(0, elapsed);
    this.panel.classList.add("open");
    this.toggleButton.setAttribute("aria-expanded", "true");
    this.render(this.elapsed);
  }

  close(): void {
    this.openState = false;
    this.panel.classList.remove("open");
    this.toggleButton.setAttribute("aria-expanded", "false");
  }

  setElapsed(elapsed: number): void {
    const next = Math.max(0, Math.floor(elapsed));
    if (next === this.elapsed) {
      return;
    }
    this.elapsed = next;
    if (this.openState) {
      this.render(this.elapsed);
    }
  }

  showNearMiss(row: RecipeRow, missing: number): void {
    const noun = missing === 1 ? "crate" : "crates";
    this.nearText = `${displayName(row)}: needs ${missing} more ${noun}`;
    this.nearBanner.textContent = this.nearText;
    this.nearBanner.dataset.recipe = String(row.id);
    this.nearBanner.classList.add("visible");
  }

  clearNearMiss(): void {
    this.nearText = null;
    this.nearBanner.textContent = "";
    this.nearBanner.classList.remove("visible");
    delete this.nearBanner.dataset.recipe;
  }

  info(): RecipeBookInfo {
    const locked = this.cards.filter((card) => card.row.unlockSeconds > this.elapsed).length;
    return {
      open: this.openState,
      cards: this.cards.length,
      locked,
      unlocked: this.cards.length - locked,
      nearMiss: this.nearText,
    };
  }

  row(id: number): RecipeRow | undefined {
    return this.rows.find((row) => row.id === id);
  }

  dispose(): void {
    this.root.remove();
  }

  private render(elapsed: number): void {
    this.elapsed = elapsed;
    for (const card of this.cards) {
      const locked = card.row.unlockSeconds > elapsed;
      card.root.dataset.locked = locked ? "true" : "false";
      card.root.classList.toggle("locked", locked);
      card.lock.textContent = locked
        ? `Unlocks ${formatClock(card.row.unlockSeconds)}`
        : "";
    }
  }
}
