/**
 * Touch input and the mobile control surface (plan §8.6, task T-036).
 *
 * The hard part of mobile input is not detecting a finger; it is deciding what
 * that finger means. The plan's answer is an explicit mode: slot 0 is the
 * pickaxe, slots 1-8 are blocks, and the selected slot *is* the action. A tap
 * on the world breaks in pickaxe mode and places in block mode. There is no
 * long-press disambiguation and no hidden gesture.
 *
 * The left half owns a stick that spawns under the thumb, not at a fixed
 * position. The right half owns look drag. The zones listen to their own touch
 * events, so two fingers work at once and neither has to wait for the other.
 *
 * A held placement in block mode repeats at the airdrop deposit rate, 6/s
 * (§4.4.3). The first repeat starts after a short hold; a drag past 8 px
 * cancels it and becomes a look drag, so a stationary hold deposits and a
 * moving finger looks.
 *
 * The buttons are at least 44 px (jump 64, stance 56, hotbar slots 44) and the
 * crosshair carries the mode icon. Copy is deliberately plain: the stance
 * button says FOLLOW, HOLD or ASSAULT, and every control has an aria-label.
 */

import { packFrame, type PackedState } from "./inputDesktop";

export type TouchMode = "break" | "place";
export type Stance = "follow" | "hold" | "assault";

export const STANCES: readonly Stance[] = ["follow", "hold", "assault"];
export const DEPOSIT_PER_SECOND = 6;
export const HOLD_DELAY_MS = 150;
const STICK_RADIUS = 60;
const TAP_SLOP = 8;
const LOOK_UNITS_PER_PX = (0.35 / 360) * 256;

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

const CSS = `
.cb-touch-root { position: fixed; inset: 0; z-index: 30; pointer-events: none;
  touch-action: none; user-select: none; -webkit-user-select: none;
  font-family: ui-sans-serif, system-ui, -apple-system, sans-serif; color: #f3f6f9; }
.cb-touch-zone { position: absolute; top: 0; bottom: 0; width: 50%; z-index: 1;
  pointer-events: auto; touch-action: none; }
.cb-touch-zone.left { left: 0; }
.cb-touch-zone.right { left: 50%; }
.cb-touch-stick { position: absolute; width: 96px; height: 96px; border-radius: 50%;
  border: 2px solid rgba(255,255,255,0.35); background: rgba(8,12,18,0.28);
  transform: translate(-50%, -50%); display: none; pointer-events: none; }
.cb-touch-knob { position: absolute; left: 50%; top: 50%; width: 44px; height: 44px;
  border-radius: 50%; background: rgba(255,255,255,0.62);
  box-shadow: 0 2px 10px rgba(0,0,0,0.35); transform: translate(-50%, -50%);
  pointer-events: none; }
.cb-touch-crosshair { position: absolute; left: 50%; top: 50%; width: 44px; height: 44px;
  transform: translate(-50%, -50%); display: flex; align-items: center;
  justify-content: center; pointer-events: none; z-index: 2; }
.cb-touch-crosshair::before { content: ""; position: absolute; width: 30px; height: 30px;
  border: 1px solid rgba(255,255,255,0.65); border-radius: 50%; }
.cb-pick-icon { width: 3px; height: 18px; border-radius: 2px; background: #fff;
  transform: rotate(45deg); position: relative; }
.cb-pick-icon::before { content: ""; position: absolute; top: -3px; left: -5px;
  width: 13px; height: 4px; border-radius: 2px; background: #fff; transform: rotate(-8deg); }
.cb-block-icon { width: 14px; height: 14px; border-radius: 3px;
  background: var(--cb-block-color, #6cc0ff); box-shadow: 0 0 0 2px rgba(0,0,0,0.35); }
.cb-touch-crosshair[data-mode="break"] .cb-block-icon { display: none; }
.cb-touch-crosshair[data-mode="place"] .cb-pick-icon { display: none; }
.cb-touch-hotbar { position: absolute; left: 50%; bottom: calc(env(safe-area-inset-bottom, 0px) + 14px);
  transform: translateX(-50%); display: flex; gap: 2px; padding: 4px; max-width: calc(100vw - 16px);
  overflow-x: auto; background: rgba(8,12,18,0.62); border-radius: 10px; z-index: 3;
  pointer-events: auto; touch-action: none; }
.cb-touch-slot { width: 44px; height: 44px; min-width: 44px; min-height: 44px;
  display: flex; align-items: center; justify-content: center; padding: 0;
  border: 2px solid transparent; border-radius: 8px; background: rgba(255,255,255,0.08);
  touch-action: none; }
.cb-touch-slot[aria-pressed="true"] { border-color: #fff; background: rgba(255,255,255,0.2); }
.cb-slot-pick { width: 3px; height: 16px; border-radius: 2px; background: #fff;
  transform: rotate(45deg); position: relative; }
.cb-slot-pick::before { content: ""; position: absolute; top: -3px; left: -5px;
  width: 13px; height: 4px; border-radius: 2px; background: #fff; }
.cb-slot-block { width: 22px; height: 22px; border-radius: 4px;
  box-shadow: inset 0 0 0 1px rgba(0,0,0,0.3); }
.cb-touch-jump { position: absolute; right: calc(env(safe-area-inset-right, 0px) + 20px);
  bottom: calc(env(safe-area-inset-bottom, 0px) + 82px); width: 64px; height: 64px;
  border-radius: 50%; border: 2px solid rgba(255,255,255,0.45);
  background: rgba(8,12,18,0.55); color: #f3f6f9; font: 600 10px ui-sans-serif, system-ui, sans-serif;
  letter-spacing: 0.06em; z-index: 3; pointer-events: auto; touch-action: none; }
.cb-touch-stance { position: absolute; right: calc(env(safe-area-inset-right, 0px) + 96px);
  bottom: calc(env(safe-area-inset-bottom, 0px) + 82px); width: 56px; height: 56px;
  border-radius: 12px; border: 2px solid rgba(255,255,255,0.45);
  background: rgba(8,12,18,0.55); color: #f3f6f9;
  display: flex; flex-direction: column; align-items: center; justify-content: center;
  gap: 2px; padding: 0; z-index: 3; pointer-events: auto; touch-action: none; }
.cb-stance-glyph { display: block; width: 18px; }
.cb-stance-glyph i { display: block; height: 3px; margin: 2px 0; border-radius: 2px;
  background: rgba(255,255,255,0.28); }
.cb-touch-stance[data-stance="follow"] .cb-stance-glyph i:nth-child(1),
.cb-touch-stance[data-stance="hold"] .cb-stance-glyph i:nth-child(2),
.cb-touch-stance[data-stance="assault"] .cb-stance-glyph i:nth-child(3) { background: #9fe0b0; }
.cb-stance-label { font: 600 8px ui-sans-serif, system-ui, sans-serif; letter-spacing: 0.04em; }
`;

export interface TouchState {
  mode: TouchMode;
  selectedSlot: number;
  selectedBlock: number;
  stance: Stance;
  stickActive: boolean;
  lookActive: boolean;
  yaw: number;
  pitch: number;
  moveX: number;
  moveY: number;
  polls: number;
  breaks: number;
  places: number;
  frame: number[];
}

export interface TouchInputOptions {
  parent?: HTMLElement;
}

export class TouchInput {
  onBreak: (() => void) | null = null;
  onPlace: ((block: number) => void) | null = null;
  onStance: ((stance: Stance) => void) | null = null;
  onSlot: ((slot: number) => void) | null = null;

  private readonly root: HTMLDivElement;
  private readonly style: HTMLStyleElement;
  private readonly stickBase: HTMLDivElement;
  private readonly stickKnob: HTMLDivElement;
  private readonly crosshair: HTMLDivElement;
  private readonly hotbar: HTMLDivElement;
  private readonly jumpButton: HTMLButtonElement;
  private readonly stanceButton: HTMLButtonElement;
  private readonly stanceLabel: HTMLSpanElement;
  private readonly frame = new Uint8Array(4);
  private readonly slots: HTMLButtonElement[] = [];
  private readonly zoneLeft: HTMLDivElement;
  private readonly zoneRight: HTMLDivElement;

  private selected = 0;
  private stanceIndex = 0;
  private yaw = 0;
  private pitch = 0;
  private moveX = 0;
  private moveY = 0;
  private stickOriginX = 0;
  private stickOriginY = 0;
  private stickTouch: number | null = null;
  private lookTouch: number | null = null;
  private lookStartX = 0;
  private lookStartY = 0;
  private lookLastX = 0;
  private lookLastY = 0;
  private lookMoved = false;
  private holdFired = false;
  private useHeld = false;
  private jumpHeld = false;
  private holdTimer: number | null = null;
  private repeatTimer: number | null = null;
  private polls = 0;
  private breaks = 0;
  private places = 0;

  constructor(options: TouchInputOptions = {}) {
    this.style = document.createElement("style");
    this.style.textContent = CSS;
    document.head.append(this.style);

    this.root = document.createElement("div");
    this.root.className = "cb-touch-root";

    this.zoneLeft = document.createElement("div");
    this.zoneLeft.className = "cb-touch-zone left";
    this.zoneRight = document.createElement("div");
    this.zoneRight.className = "cb-touch-zone right";

    this.stickBase = document.createElement("div");
    this.stickBase.className = "cb-touch-stick";
    this.stickKnob = document.createElement("div");
    this.stickKnob.className = "cb-touch-knob";
    this.stickBase.append(this.stickKnob);

    this.crosshair = document.createElement("div");
    this.crosshair.className = "cb-touch-crosshair";
    this.crosshair.dataset.mode = "break";
    const pick = document.createElement("span");
    pick.className = "cb-pick-icon";
    const block = document.createElement("span");
    block.className = "cb-block-icon";
    this.crosshair.append(pick, block);

    this.hotbar = document.createElement("div");
    this.hotbar.className = "cb-touch-hotbar";
    for (let index = 0; index < 9; index += 1) {
      const slot = document.createElement("button");
      slot.type = "button";
      slot.className = "cb-touch-slot";
      slot.dataset.slot = String(index);
      slot.setAttribute("aria-label", index === 0 ? "Pickaxe" : `Block ${index}`);
      slot.setAttribute("aria-pressed", index === 0 ? "true" : "false");
      if (index === 0) {
        const icon = document.createElement("span");
        icon.className = "cb-slot-pick";
        slot.append(icon);
      } else {
        const icon = document.createElement("span");
        icon.className = "cb-slot-block";
        icon.style.background = BLOCK_COLORS[index - 1];
        slot.append(icon);
      }
      slot.addEventListener("touchstart", (event) => {
        event.preventDefault();
        event.stopPropagation();
        this.selectSlot(index);
      }, { passive: false });
      this.slots.push(slot);
      this.hotbar.append(slot);
    }

    this.jumpButton = document.createElement("button");
    this.jumpButton.type = "button";
    this.jumpButton.className = "cb-touch-jump";
    this.jumpButton.textContent = "JUMP";
    this.jumpButton.setAttribute("aria-label", "Jump");
    this.jumpButton.addEventListener("touchstart", (event) => {
      event.preventDefault();
      event.stopPropagation();
      this.jumpHeld = true;
    }, { passive: false });
    for (const type of ["touchend", "touchcancel"] as const) {
      this.jumpButton.addEventListener(type, (event) => {
        event.preventDefault();
        this.jumpHeld = false;
      }, { passive: false });
    }

    this.stanceButton = document.createElement("button");
    this.stanceButton.type = "button";
    this.stanceButton.className = "cb-touch-stance";
    this.stanceButton.dataset.stance = "follow";
    this.stanceButton.setAttribute("aria-label", "Stance: Follow");
    const glyph = document.createElement("span");
    glyph.className = "cb-stance-glyph";
    for (let i = 0; i < 3; i += 1) {
      glyph.append(document.createElement("i"));
    }
    this.stanceLabel = document.createElement("span");
    this.stanceLabel.className = "cb-stance-label";
    this.stanceLabel.textContent = "FOLLOW";
    this.stanceButton.append(glyph, this.stanceLabel);
    this.stanceButton.addEventListener("touchstart", (event) => {
      event.preventDefault();
      event.stopPropagation();
      this.cycleStance();
    }, { passive: false });

    this.zoneLeft.addEventListener("touchstart", (event) => event.preventDefault(), {
      passive: false,
    });
    this.zoneRight.addEventListener("touchstart", (event) => event.preventDefault(), {
      passive: false,
    });
    // The root sees touches from either zone (they bubble) and both touches of
    // a two-finger gesture in one event. Controls stop propagation, so their
    // taps never become world gestures.
    this.root.addEventListener("touchstart", this.onTouchStart, { passive: false });
    this.root.addEventListener("touchmove", this.onTouchMove, { passive: false });
    this.root.addEventListener("touchend", this.onTouchEnd, { passive: false });
    this.root.addEventListener("touchcancel", this.onTouchEnd, { passive: false });

    this.root.append(
      this.zoneLeft,
      this.zoneRight,
      this.stickBase,
      this.crosshair,
      this.hotbar,
      this.jumpButton,
      this.stanceButton,
    );
    (options.parent ?? document.body).append(this.root);
    this.selectSlot(0);
  }

  /** Fill and return the 4-byte frame. One call per tick. */
  poll(): Uint8Array {
    this.polls += 1;
    const state: PackedState = {
      forward: this.moveY,
      strafe: -this.moveX,
      jump: this.jumpHeld,
      fire: false,
      use: this.useHeld,
      stance: false,
      crouch: false,
      yaw: this.yaw,
      pitch: this.pitch,
    };
    return packFrame(state, this.frame);
  }

  frameBytes(): number[] {
    return [this.frame[0], this.frame[1], this.frame[2], this.frame[3]];
  }

  state(): TouchState {
    return {
      mode: this.selected === 0 ? "break" : "place",
      selectedSlot: this.selected,
      selectedBlock: this.selected,
      stance: STANCES[this.stanceIndex],
      stickActive: this.stickTouch !== null,
      lookActive: this.lookTouch !== null,
      yaw: this.yaw,
      pitch: this.pitch,
      moveX: this.moveX,
      moveY: this.moveY,
      polls: this.polls,
      breaks: this.breaks,
      places: this.places,
      frame: this.frameBytes(),
    };
  }

  get pollsValue(): number {
    return this.polls;
  }

  get yawValue(): number {
    return this.yaw;
  }

  get pitchValue(): number {
    return this.pitch;
  }

  get stanceValue(): Stance {
    return STANCES[this.stanceIndex];
  }

  selectSlot(index: number): void {
    this.selected = Math.max(0, Math.min(8, Math.floor(index)));
    for (const slot of this.slots) {
      slot.setAttribute(
        "aria-pressed",
        slot.dataset.slot === String(this.selected) ? "true" : "false",
      );
    }
    const mode = this.selected === 0 ? "break" : "place";
    this.crosshair.dataset.mode = mode;
    this.crosshair.setAttribute(
      "aria-label",
      this.selected === 0 ? "Targeting: pickaxe" : `Targeting: block ${this.selected}`,
    );
    if (this.selected > 0) {
      this.crosshair.style.setProperty("--cb-block-color", BLOCK_COLORS[this.selected - 1]);
    }
    if (mode === "break") {
      this.cancelHold();
    }
    this.onSlot?.(this.selected);
  }

  cycleStance(): void {
    this.stanceIndex = (this.stanceIndex + 1) % STANCES.length;
    const stance = STANCES[this.stanceIndex];
    this.stanceButton.dataset.stance = stance;
    this.stanceLabel.textContent = stance.toUpperCase();
    this.stanceButton.setAttribute("aria-label", `Stance: ${stance[0].toUpperCase()}${stance.slice(1)}`);
    this.onStance?.(stance);
  }

  dispose(): void {
    this.cancelHold();
    this.root.remove();
    this.style.remove();
  }

  private readonly onTouchStart = (event: TouchEvent): void => {
    let handled = false;
    const half = window.innerWidth / 2;
    for (const touch of Array.from(event.changedTouches)) {
      if (touch.clientX < half) {
        if (this.stickTouch === null) {
          this.beginStick(touch);
          handled = true;
        }
      } else if (this.lookTouch === null) {
        this.beginLook(touch);
        handled = true;
      }
    }
    if (handled) {
      event.preventDefault();
    }
  };

  private readonly onTouchMove = (event: TouchEvent): void => {
    let handled = false;
    for (const touch of Array.from(event.changedTouches)) {
      if (touch.identifier === this.stickTouch) {
        this.updateStick(touch);
        handled = true;
      }
      if (touch.identifier === this.lookTouch) {
        this.updateLook(touch);
        handled = true;
      }
    }
    if (handled) {
      event.preventDefault();
    }
  };

  private readonly onTouchEnd = (event: TouchEvent): void => {
    let handled = false;
    for (const touch of Array.from(event.changedTouches)) {
      if (touch.identifier === this.stickTouch) {
        this.endStick();
        handled = true;
      }
      if (touch.identifier === this.lookTouch) {
        this.endLook();
        handled = true;
      }
    }
    if (handled) {
      event.preventDefault();
    }
  };

  private beginStick(touch: Touch): void {
    this.stickTouch = touch.identifier;
    this.stickOriginX = touch.clientX;
    this.stickOriginY = touch.clientY;
    this.stickBase.style.left = `${touch.clientX}px`;
    this.stickBase.style.top = `${touch.clientY}px`;
    this.stickBase.style.display = "block";
    this.stickKnob.style.transform = "translate(-50%, -50%)";
    this.moveX = 0;
    this.moveY = 0;
  }

  private updateStick(touch: Touch): void {
    let dx = touch.clientX - this.stickOriginX;
    let dy = touch.clientY - this.stickOriginY;
    const length = Math.hypot(dx, dy);
    if (length > STICK_RADIUS) {
      dx = (dx / length) * STICK_RADIUS;
      dy = (dy / length) * STICK_RADIUS;
    }
    this.stickKnob.style.transform =
      `translate(-50%, -50%) translate(${dx}px, ${dy}px)`;
    this.moveX = dx / STICK_RADIUS;
    this.moveY = -dy / STICK_RADIUS;
  }

  private endStick(): void {
    this.stickTouch = null;
    this.moveX = 0;
    this.moveY = 0;
    this.stickBase.style.display = "none";
  }

  private beginLook(touch: Touch): void {
    this.lookTouch = touch.identifier;
    this.lookStartX = touch.clientX;
    this.lookStartY = touch.clientY;
    this.lookLastX = touch.clientX;
    this.lookLastY = touch.clientY;
    this.lookMoved = false;
    this.holdFired = false;
    this.useHeld = this.selected > 0;
    if (this.selected > 0) {
      this.startHold();
    }
  }

  private updateLook(touch: Touch): void {
    if (
      !this.lookMoved &&
      Math.hypot(touch.clientX - this.lookStartX, touch.clientY - this.lookStartY) >
        TAP_SLOP
    ) {
      this.lookMoved = true;
      this.cancelHold();
    }
    if (!this.lookMoved) {
      return;
    }
    const dx = touch.clientX - this.lookLastX;
    const dy = touch.clientY - this.lookLastY;
    this.lookLastX = touch.clientX;
    this.lookLastY = touch.clientY;
    this.yaw = (((this.yaw - dx * LOOK_UNITS_PER_PX) % 256) + 256) % 256;
    this.pitch = Math.max(-64, Math.min(64, this.pitch - dy * LOOK_UNITS_PER_PX));
  }

  private endLook(): void {
    if (!this.lookMoved && !this.holdFired) {
      if (this.selected === 0) {
        this.breaks += 1;
        this.onBreak?.();
      } else {
        this.placeNow();
      }
    }
    this.cancelHold();
    this.lookTouch = null;
    this.lookMoved = false;
    this.holdFired = false;
    this.useHeld = false;
  }

  private startHold(): void {
    this.cancelHold();
    this.holdTimer = window.setTimeout(() => {
      this.holdTimer = null;
      if (this.lookTouch === null || this.lookMoved || this.selected === 0) {
        return;
      }
      this.holdFired = true;
      this.placeNow();
      this.repeatTimer = window.setInterval(
        () => this.placeNow(),
        1000 / DEPOSIT_PER_SECOND,
      );
    }, HOLD_DELAY_MS);
  }

  private cancelHold(): void {
    if (this.holdTimer !== null) {
      window.clearTimeout(this.holdTimer);
      this.holdTimer = null;
    }
    if (this.repeatTimer !== null) {
      window.clearInterval(this.repeatTimer);
      this.repeatTimer = null;
    }
  }

  private placeNow(): void {
    if (this.selected === 0) {
      return;
    }
    this.places += 1;
    this.onPlace?.(this.selected);
  }
}
