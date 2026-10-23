/**
 * Settings screen (task T-039).
 *
 * Every control writes its value before applying it, so a reload sees the same
 * choice the current session is using. The remap buttons capture one key on the
 * window in the capture phase, which keeps the key from also reaching the game
 * input while the player is assigning it.
 */

import {
  DEFAULT_SENSITIVITY,
  INVERT_Y_KEY,
  MAX_SENSITIVITY,
  MIN_SENSITIVITY,
  RAW_INPUT_KEY,
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
export const DPI_KEY = "buddycraft.mouseDpi";
export const DEFAULT_DPI = 800;
const MIN_DPI = 100;
const MAX_DPI = 32_000;

const KEY_LABELS: Record<keyof KeyMap, string> = {
  forward: "Forward",
  back: "Backward",
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

function clampDpi(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_DPI;
  return Math.round(Math.max(MIN_DPI, Math.min(MAX_DPI, value)));
}

function cmPer360(sensitivity: number, dpi: number): number {
  return 650.24 / (Math.max(MIN_SENSITIVITY, sensitivity) * Math.max(1, dpi));
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
  onRawInput?: (value: boolean) => void;
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

    const mouseSection = make("div", "settings-section");
    mouseSection.textContent = "Mouse";

    const sensitivity = make("label", "settings-row");
    const sensitivityLabel = make("span", "settings-label");
    sensitivityLabel.textContent = "Look sensitivity";
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
        : DEFAULT_SENSITIVITY,
    );
    this.sensitivityValue = make("output", "settings-value");
    this.sensitivityValue.title =
      "Degrees of camera rotation per 100 pixels of mouse movement";
    sensitivity.append(sensitivityLabel, sensitivityInput, this.sensitivityValue);

    const presets = make("div", "settings-row");
    const presetsLabel = make("span", "settings-label");
    presetsLabel.textContent = "Presets";
    const presetBox = make("div", "settings-presets");
    presets.append(presetsLabel, presetBox);

    const dpi = make("label", "settings-row");
    const dpiLabel = make("span", "settings-label");
    dpiLabel.textContent = "Mouse DPI";
    const dpiInput = make("input", "settings-control settings-number");
    dpiInput.type = "number";
    dpiInput.min = String(MIN_DPI);
    dpiInput.max = String(MAX_DPI);
    dpiInput.step = "50";
    dpiInput.inputMode = "numeric";
    dpiInput.dataset.setting = "dpi";
    const storedDpi = Number.parseInt(storage.getItem(DPI_KEY) ?? "", 10);
    dpiInput.value = String(Number.isFinite(storedDpi) ? clampDpi(storedDpi) : DEFAULT_DPI);
    const cmValue = make("output", "settings-value");
    cmValue.dataset.setting = "cm360";
    cmValue.title = "Centimetres of mouse movement for a full 360 degree turn";
    dpi.append(dpiLabel, dpiInput, cmValue);

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

    const raw = make("label", "settings-row");
    const rawLabel = make("span", "settings-label");
    rawLabel.textContent = "Raw input";
    const rawInput = make("input", "settings-control");
    rawInput.type = "checkbox";
    rawInput.dataset.setting = "raw-input";
    rawInput.checked = storage.getItem(RAW_INPUT_KEY) !== "false";
    rawInput.addEventListener("change", () => {
      this.storage.setItem(RAW_INPUT_KEY, rawInput.checked ? "true" : "false");
      this.callbacks.onRawInput?.(rawInput.checked);
    });
    raw.append(rawLabel, rawInput);

    const mouseHint = make("div", "settings-hint");
    mouseHint.textContent =
      "Raw input applies on the next pointer-lock click. DPI only feeds the cm/360 calculator; the browser cannot read it.";

    const updateMouseOutputs = (): void => {
      const value = Number.parseFloat(sensitivityInput.value);
      this.sensitivityValue.value = String(value);
      this.sensitivityValue.textContent = sensitivityDegrees(value);
      const dpiValue = clampDpi(Number.parseFloat(dpiInput.value));
      const cm = cmPer360(value, dpiValue);
      cmValue.value = `${cm.toFixed(1)} cm/360`;
      cmValue.textContent = cmValue.value;
    };
    const applySensitivity = (value: number): void => {
      const clamped = Math.max(MIN_SENSITIVITY, Math.min(MAX_SENSITIVITY, value));
      sensitivityInput.value = String(clamped);
      this.storage.setItem(SENSITIVITY_KEY, String(clamped));
      this.callbacks.onSensitivity?.(clamped);
      updateMouseOutputs();
    };
    for (const [label, value] of [
      ["LOW", 0.1],
      ["DEFAULT", DEFAULT_SENSITIVITY],
      ["HIGH", 0.5],
    ] as const) {
      const button = make("button", "settings-preset");
      button.type = "button";
      button.dataset.sensitivityPreset = label.toLowerCase();
      button.textContent = label;
      button.title = sensitivityDegrees(value);
      button.addEventListener("click", () => applySensitivity(value));
      presetBox.append(button);
    }
    sensitivityInput.addEventListener("input", () => {
      applySensitivity(Number.parseFloat(sensitivityInput.value));
    });
    dpiInput.addEventListener("input", () => {
      const parsed = Number.parseFloat(dpiInput.value);
      if (Number.isFinite(parsed)) {
        this.storage.setItem(DPI_KEY, String(clampDpi(parsed)));
      }
      updateMouseOutputs();
    });
    updateMouseOutputs();

    const displaySection = make("div", "settings-section");
    displaySection.textContent = "Display";

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
    form.append(
      mouseSection,
      sensitivity,
      presets,
      dpi,
      invert,
      raw,
      mouseHint,
      displaySection,
      quality,
      scale,
      motion,
      controlsLabel,
      controls,
    );
    this.element.append(title, form, back);
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
