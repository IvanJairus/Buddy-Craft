/**
 * Settings screen (task T-039).
 *
 * Every control writes its value before applying it, so a reload sees the same
 * choice the current session is using. The remap buttons capture one key on the
 * window in the capture phase, which keeps the key from also reaching the game
 * input while the player is assigning it.
 */

import {
  INVERT_Y_KEY,
  MAX_SENSITIVITY,
  MIN_SENSITIVITY,
  SENSITIVITY_KEY,
  loadKeyMap,
  saveKeyMap,
  type KeyMap,
} from "../game/inputDesktop";
import {
  parseTier,
  readStoredTier,
  writeStoredTier,
  type QualityTier,
} from "../render/quality";

export const UI_SCALE_KEY = "buddycraft.uiScale";
export const REDUCED_MOTION_KEY = "buddycraft.reducedMotion";

const KEY_LABELS: Record<keyof KeyMap, string> = {
  forward: "Forward",
  back: "Back",
  left: "Left",
  right: "Right",
  jump: "Jump",
  stance: "Stance",
  recipes: "Recipe book",
  scoreboard: "Scoreboard",
  drop: "Drop",
  menu: "Menu",
};

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

function sensitivityDegrees(value: number): string {
  return `${((value * 360 * 100) / 256).toFixed(1)}°/100px`;
}

function keyLabel(code: string): string {
  if (code.startsWith("Key")) return code.slice(3);
  if (code.startsWith("Digit")) return code.slice(5);
  if (code === "Space") return "SPACE";
  if (code === "Escape") return "ESC";
  if (code === "Tab") return "TAB";
  return code;
}

export interface SettingsCallbacks {
  onQuality?: (tier: QualityTier) => void;
  onSensitivity?: (value: number) => void;
  onInvertY?: (value: boolean) => void;
  onKeyMap?: (map: KeyMap) => void;
  onBack: () => void;
}

export class SettingsScreen {
  readonly element: HTMLDivElement;

  private readonly storage: Pick<Storage, "getItem" | "setItem">;
  private readonly callbacks: SettingsCallbacks;
  private readonly sensitivityValue: HTMLOutputElement;
  private readonly scaleValue: HTMLOutputElement;
  private readonly remapButtons = new Map<keyof KeyMap, HTMLButtonElement>();
  private keyMap: KeyMap;
  private listening: keyof KeyMap | null = null;

  constructor(callbacks: SettingsCallbacks, storage: Pick<Storage, "getItem" | "setItem"> = window.localStorage) {
    this.callbacks = callbacks;
    this.storage = storage;
    this.keyMap = loadKeyMap(storage);
    this.element = make("div", "screen-panel settings-panel");
    this.element.dataset.screen = "settings";
    this.element.setAttribute("aria-label", "Settings");

    const title = make("h2", "screen-title");
    title.textContent = "Settings";
    const form = make("div", "settings-form");

    const quality = make("label", "settings-row");
    const qualityLabel = make("span", "settings-label");
    qualityLabel.textContent = "Quality";
    const qualitySelect = make("select", "settings-control");
    qualitySelect.dataset.setting = "quality";
    for (const tier of ["LOW", "MED", "HIGH"] as QualityTier[]) {
      const option = document.createElement("option");
      option.value = tier;
      option.textContent = tier;
      qualitySelect.append(option);
    }
    qualitySelect.value = readStoredTier(storage) ?? "MED";
    qualitySelect.addEventListener("change", () => {
      const tier = parseTier(qualitySelect.value) ?? "MED";
      writeStoredTier(storage, tier);
      this.callbacks.onQuality?.(tier);
    });
    quality.append(qualityLabel, qualitySelect);

    const sensitivity = make("label", "settings-row");
    const sensitivityLabel = make("span", "settings-label");
    sensitivityLabel.textContent = "Sensitivity";
    const sensitivityInput = make("input", "settings-control");
    sensitivityInput.type = "range";
    sensitivityInput.min = String(MIN_SENSITIVITY);
    sensitivityInput.max = String(MAX_SENSITIVITY);
    sensitivityInput.step = "0.01";
    sensitivityInput.dataset.setting = "sensitivity";
    const storedSensitivity = Number.parseFloat(storage.getItem(SENSITIVITY_KEY) ?? "");
    sensitivityInput.value = String(
      Number.isFinite(storedSensitivity)
        ? Math.max(MIN_SENSITIVITY, Math.min(MAX_SENSITIVITY, storedSensitivity))
        : 0.25,
    );
    this.sensitivityValue = make("output", "settings-value");
    this.sensitivityValue.value = sensitivityInput.value;
    this.sensitivityValue.textContent = sensitivityDegrees(
      Number.parseFloat(sensitivityInput.value),
    );
    this.sensitivityValue.title =
      "Degrees of camera rotation per 100 pixels of mouse movement";
    sensitivityInput.addEventListener("input", () => {
      const value = Number.parseFloat(sensitivityInput.value);
      this.sensitivityValue.value = String(value);
      this.sensitivityValue.textContent = sensitivityDegrees(value);
      this.storage.setItem(SENSITIVITY_KEY, String(value));
      this.callbacks.onSensitivity?.(value);
    });
    sensitivity.append(sensitivityLabel, sensitivityInput, this.sensitivityValue);

    const invert = make("label", "settings-row");
    const invertLabel = make("span", "settings-label");
    invertLabel.textContent = "Invert Y";
    const invertInput = make("input", "settings-control");
    invertInput.type = "checkbox";
    invertInput.dataset.setting = "invert";
    invertInput.checked = storage.getItem(INVERT_Y_KEY) === "true";
    invertInput.addEventListener("change", () => {
      this.storage.setItem(INVERT_Y_KEY, invertInput.checked ? "true" : "false");
      this.callbacks.onInvertY?.(invertInput.checked);
    });
    invert.append(invertLabel, invertInput);

    const scale = make("label", "settings-row");
    const scaleLabel = make("span", "settings-label");
    scaleLabel.textContent = "UI scale";
    const scaleInput = make("input", "settings-control");
    scaleInput.type = "range";
    scaleInput.min = "80";
    scaleInput.max = "150";
    scaleInput.step = "5";
    scaleInput.dataset.setting = "ui-scale";
    const storedScale = Number.parseInt(storage.getItem(UI_SCALE_KEY) ?? "", 10);
    scaleInput.value = String(
      Number.isFinite(storedScale) ? Math.max(80, Math.min(150, storedScale)) : 100,
    );
    this.scaleValue = make("output", "settings-value");
    this.scaleValue.value = `${scaleInput.value}%`;
    this.scaleValue.textContent = `${scaleInput.value}%`;
    scaleInput.addEventListener("input", () => {
      const percent = Number.parseInt(scaleInput.value, 10);
      this.scaleValue.value = `${percent}%`;
      this.scaleValue.textContent = `${percent}%`;
      this.storage.setItem(UI_SCALE_KEY, String(percent));
      this.applyUiScale(percent);
    });
    scale.append(scaleLabel, scaleInput, this.scaleValue);

    const motion = make("label", "settings-row");
    const motionLabel = make("span", "settings-label");
    motionLabel.textContent = "Reduced motion";
    const motionInput = make("input", "settings-control");
    motionInput.type = "checkbox";
    motionInput.dataset.setting = "reduced-motion";
    motionInput.checked = storage.getItem(REDUCED_MOTION_KEY) === "true";
    motionInput.addEventListener("change", () => {
      this.storage.setItem(REDUCED_MOTION_KEY, motionInput.checked ? "true" : "false");
      this.applyReducedMotion(motionInput.checked);
    });
    motion.append(motionLabel, motionInput);

    const controls = make("div", "settings-controls");
    const controlsLabel = make("div", "settings-section");
    controlsLabel.textContent = "Controls";
    for (const action of Object.keys(KEY_LABELS) as Array<keyof KeyMap>) {
      const row = make("div", "settings-row");
      const label = make("span", "settings-label");
      label.textContent = KEY_LABELS[action];
      const button = make("button", "settings-remap");
      button.type = "button";
      button.dataset.remap = action;
      button.textContent = keyLabel(this.keyMap[action]);
      button.addEventListener("click", () => this.beginRemap(action));
      this.remapButtons.set(action, button);
      row.append(label, button);
      controls.append(row);
    }

    const back = make("button", "screen-button screen-back");
    back.type = "button";
    back.dataset.screenAction = "settings-back";
    back.textContent = "BACK";
    back.addEventListener("click", () => this.callbacks.onBack());
    form.append(quality, sensitivity, invert, scale, motion, controlsLabel, controls, back);
    this.element.append(title, form);
    this.applyUiScale(Number.parseInt(scaleInput.value, 10));
    this.applyReducedMotion(motionInput.checked);
    window.addEventListener("keydown", this.onCaptureKey, true);
  }

  dispose(): void {
    window.removeEventListener("keydown", this.onCaptureKey, true);
    this.element.remove();
  }

  private readonly onCaptureKey = (event: KeyboardEvent): void => {
    if (this.listening === null) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (event.code === "Escape") {
      this.listening = null;
      this.refreshRemapButtons();
      return;
    }
    const action = this.listening;
    this.keyMap = { ...this.keyMap, [action]: event.code };
    saveKeyMap(this.storage, this.keyMap);
    this.callbacks.onKeyMap?.(this.keyMap);
    this.listening = null;
    this.refreshRemapButtons();
  };

  private beginRemap(action: keyof KeyMap): void {
    this.listening = action;
    this.refreshRemapButtons();
  }

  private refreshRemapButtons(): void {
    for (const [action, button] of this.remapButtons) {
      button.textContent =
        this.listening === action ? "PRESS A KEY" : keyLabel(this.keyMap[action]);
      button.classList.toggle("listening", this.listening === action);
    }
  }

  private applyUiScale(percent: number): void {
    document.documentElement.style.setProperty("--cb-ui-scale", String(percent / 100));
  }

  private applyReducedMotion(enabled: boolean): void {
    document.body.classList.toggle("cb-reduced-motion", enabled);
  }
}
