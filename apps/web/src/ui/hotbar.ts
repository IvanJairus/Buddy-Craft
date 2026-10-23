/**
 * Desktop hotbar. The touch surface has its own nine-slot control; this is the
 * always-visible keyboard and wheel readout for a pointer-locked desktop.
 */

import "./hotbar.css";

const BLOCK_COLORS = [
  "#8a6b4d",
  "#9b4f45",
  "#6c8f56",
  "#a89352",
  "#657f96",
  "#7b5f8f",
  "#8f7a5a",
  "#4f8f86",
];

function make<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = className;
  return node;
}

export class Hotbar {
  readonly element: HTMLDivElement;
  private readonly slots: HTMLButtonElement[] = [];
  private selected = 0;

  constructor(parent: HTMLElement, onSelect: (slot: number) => void, initial = 0) {
    this.element = make("div", "hotbar");
    this.element.setAttribute("aria-label", "Hotbar");

    for (let index = 0; index < 9; index += 1) {
      const slot = make("button", "hotbar-slot");
      slot.type = "button";
      slot.dataset.slot = String(index);
      slot.setAttribute("aria-label", index === 0 ? "Pickaxe" : `Block ${index}`);
      slot.setAttribute("aria-pressed", "false");

      const number = make("span", "hotbar-number");
      number.textContent = String(index + 1);
      slot.append(number);

      if (index === 0) {
        const icon = make("span", "hotbar-pick");
        slot.append(icon);
      } else {
        const icon = make("span", "hotbar-block");
        icon.style.background = BLOCK_COLORS[index - 1];
        slot.append(icon);
      }

      slot.addEventListener("click", () => {
        this.setSelected(index);
        onSelect(index);
      });
      this.slots.push(slot);
      this.element.append(slot);
    }

    parent.append(this.element);
    this.setSelected(initial);
  }

  setSelected(index: number): void {
    this.selected = Math.max(0, Math.min(8, Math.floor(index)));
    for (let i = 0; i < this.slots.length; i += 1) {
      this.slots[i].setAttribute("aria-pressed", i === this.selected ? "true" : "false");
    }
  }

  get selectedSlot(): number {
    return this.selected;
  }

  dispose(): void {
    this.element.remove();
  }
}
